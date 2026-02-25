//! Native macOS reminder dialog using a custom NSPanel with vertical NSStackView of buttons.
//! Used when the daemon spawns `ts --reminder-dialog choice1 choice2 ...` via launchctl asuser.
//! Custom panel guarantees vertical layout regardless of choice count (NSAlert switches to horizontal).

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSButton, NSImage, NSPanel, NSScrollView, NSStackView, NSStackViewDistribution,
    NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSString, NSSize};
use std::cell::RefCell;
use std::path::PathBuf;

// NSUserInterfaceLayoutOrientationVertical = 1
const NS_USER_INTERFACE_LAYOUT_ORIENTATION_VERTICAL: NSUserInterfaceLayoutOrientation =
    NSUserInterfaceLayoutOrientation(1);

thread_local! {
    static DIALOG_RESULT: RefCell<Option<String>> = const { RefCell::new(None) };
}


static CHOICES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
/// Icon path for dock (ts-icon.svg/png next to exe, or assets/icon.svg when running from repo).
static ICON_PATH: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Run the native reminder dialog. Must be called from the main thread (e.g. when invoked as `ts --reminder-dialog ...`).
/// Returns the selected choice string, or None if cancelled/error.
pub fn run_native_reminder_dialog(choices: Vec<String>) -> Option<String> {
    let mtm = MainThreadMarker::new()?;
    CHOICES.set(choices).ok()?;
    DIALOG_RESULT.with(|r| *r.borrow_mut() = None);
    // Resolve icon path once: next to exe (ts-icon.svg / ts-icon.png) or repo assets/icon.svg.
    let _ = ICON_PATH.set(
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
            .and_then(|dir| {
                let next_to = [dir.join("ts-icon.svg"), dir.join("ts-icon.png")];
                let dev = dir.join("..").join("assets").join("icon.svg");
                next_to
                    .into_iter()
                    .find(|p| p.exists())
                    .or_else(|| if dev.exists() { Some(dev) } else { None })
            }),
    );

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    app.activate();

    let allocated = ReminderDialogDelegate::alloc(mtm);
    let delegate: Retained<ReminderDialogDelegate> = unsafe { msg_send![allocated, init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    app.run();

    DIALOG_RESULT.with(|r| r.borrow_mut().take())
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TSReminderButtonHandler"]
    struct TSReminderButtonHandler;

    impl TSReminderButtonHandler {
        #[unsafe(method(choiceClicked:))]
        fn choice_clicked(&self, sender: Option<&NSButton>) {
            if let Some(btn) = sender {
                let title = btn.title().to_string();
                DIALOG_RESULT.with(|r| *r.borrow_mut() = Some(title));
                let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
                app.stopModal();
            }
        }
    }

    unsafe impl NSObjectProtocol for TSReminderButtonHandler {}
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TSReminderPanelDelegate"]
    struct TSReminderPanelDelegate;

    impl TSReminderPanelDelegate {
        /// Only allow close when user chose a button (DIALOG_RESULT set). Block Escape and close button.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            DIALOG_RESULT.with(|r| r.borrow().is_some())
        }
        /// If window closes despite windowShouldClose (e.g. app quit), end modal so we can re-show.
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            if DIALOG_RESULT.with(|r| r.borrow().is_some()) {
                NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).stopModal();
            }
        }
    }

    unsafe impl NSObjectProtocol for TSReminderPanelDelegate {}
    unsafe impl NSWindowDelegate for TSReminderPanelDelegate {}
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ReminderDialogDelegate"]
    struct ReminderDialogDelegate;

    unsafe impl NSObjectProtocol for ReminderDialogDelegate {}

    unsafe impl NSApplicationDelegate for ReminderDialogDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _notification: Option<&NSNotification>) {
            let mtm = MainThreadMarker::new().expect("main thread");
            let choices = match CHOICES.get() {
                Some(c) => c,
                None => return,
            };
            if choices.is_empty() {
                return;
            }

            // Regular: visible in dock and Cmd-Tab so the user can reach the dialog.
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
            // Use timesheet icon in dock when available.
            if let Some(Some(path)) = ICON_PATH.get() {
                if path.exists() {
                    if let Some(s) = path.to_str() {
                        let ns_path = NSString::from_str(s);
                        if let Some(image) =
                            NSImage::initWithContentsOfFile(NSImage::alloc(), &ns_path)
                        {
                            unsafe { app.setApplicationIconImage(Some(&image)) };
                        }
                    }
                }
            }
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);

            // Create handler for button clicks.
            let handler_alloc = TSReminderButtonHandler::alloc(mtm);
            let handler: Retained<TSReminderButtonHandler> = unsafe { msg_send![handler_alloc, init] };
            let sel_choice_clicked = objc2::sel!(choiceClicked:);

            // Panel: 320x400 content, titled, closable.
            let content_rect = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(320.0, 400.0),
            );
            let style = NSWindowStyleMask::Titled; // No Closable: only button-clicks dismiss
            let panel_alloc = NSPanel::alloc(mtm);
            let panel: Retained<NSPanel> =
                NSPanel::initWithContentRect_styleMask_backing_defer(
                    panel_alloc,
                    content_rect,
                    style,
                    NSBackingStoreType::Buffered,
                    false,
                );
            panel.setTitle(&NSString::from_str("What are you working on?"));
            unsafe { panel.setReleasedWhenClosed(false) };
            let panel_delegate_alloc = TSReminderPanelDelegate::alloc(mtm);
            let panel_delegate: Retained<TSReminderPanelDelegate> =
                unsafe { msg_send![panel_delegate_alloc, init] };
            panel.setDelegate(Some(ProtocolObject::from_ref(&*panel_delegate)));

            // Content view.
            let content_frame = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(320.0, 400.0),
            );
            let content_alloc = NSView::alloc(mtm);
            let content: Retained<NSView> =
                unsafe { msg_send![content_alloc, initWithFrame: content_frame] };
            panel.setContentView(Some(&content));

            // Vertical stack for buttons. Height = ~32pt per button (24pt + 8pt spacing).
            let button_height: f64 = 32.0;
            let stack_height = (choices.len() as f64 * button_height).max(160.0);
            let stack_frame = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(280.0, stack_height),
            );
            let stack_alloc = NSStackView::alloc(mtm);
            let stack: Retained<NSStackView> =
                unsafe { msg_send![stack_alloc, initWithFrame: stack_frame] };
            stack.setOrientation(NS_USER_INTERFACE_LAYOUT_ORIENTATION_VERTICAL);
            stack.setSpacing(8.0);
            stack.setDistribution(NSStackViewDistribution::FillEqually);

            for choice in choices.iter() {
                let btn = unsafe {
                    NSButton::buttonWithTitle_target_action(
                        &NSString::from_str(choice),
                        Some(handler.as_ref() as &AnyObject),
                        Some(sel_choice_clicked),
                        mtm,
                    )
                };
                stack.addArrangedSubview(&btn);
            }

            let scroll_frame = NSRect::new(
                NSPoint::new(20.0, 20.0),
                NSSize::new(280.0, 360.0),
            );
            let scroll_alloc = NSScrollView::alloc(mtm);
            let scroll: Retained<NSScrollView> =
                unsafe { msg_send![scroll_alloc, initWithFrame: scroll_frame] };
            scroll.setDocumentView(Some(&stack));
            scroll.setHasVerticalScroller(true);
            scroll.setHasHorizontalScroller(false);
            scroll.setAutohidesScrollers(true);
            content.addSubview(&scroll);
            panel.setContentSize(NSSize::new(320.0, 400.0));
            panel.center();
            panel.orderFrontRegardless();

            // Re-show if dismissed without a button choice (e.g. process killed).
            loop {
                DIALOG_RESULT.with(|r| *r.borrow_mut() = None);
                let _ = app.runModalForWindow(&panel);
                if DIALOG_RESULT.with(|r| r.borrow().is_some()) {
                    break;
                }
                panel.orderFrontRegardless();
            }

            app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
            let _: () = unsafe { msg_send![&app, stop: None::<&AnyObject>] };
        }
    }
);
