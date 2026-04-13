//! Native macOS reminder dialog using a custom NSPanel with vertical NSStackView of buttons.
//! Used when the daemon spawns `ts --reminder-dialog choice1 choice2 ...` via launchctl asuser.
//! Custom panel guarantees vertical layout regardless of choice count (NSAlert switches to horizontal).

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSButton, NSEvent, NSImage, NSPanel, NSScreen, NSScrollView, NSStackView,
    NSStackViewDistribution, NSTextField, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use std::cell::RefCell;
use std::path::PathBuf;

// NSUserInterfaceLayoutOrientationVertical = 1
const NS_USER_INTERFACE_LAYOUT_ORIENTATION_VERTICAL: NSUserInterfaceLayoutOrientation =
    NSUserInterfaceLayoutOrientation(1);

thread_local! {
    static DIALOG_RESULT: RefCell<Option<String>> = const { RefCell::new(None) };
    static INPUT_CONFIRMED: RefCell<bool> = const { RefCell::new(false) };
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
                next_to.into_iter().find(|p| p.exists()).or_else(|| {
                    if dev.exists() {
                        Some(dev)
                    } else {
                        None
                    }
                })
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

// Content view that swallows all keystrokes; only mouse clicks and scrolling work.
define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "TSReminderContentView"]
    struct TSReminderContentView;

    impl TSReminderContentView {
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, _event: &NSEvent) -> bool {
            true
        }
        #[unsafe(method(keyDown:))]
        fn key_down(&self, _event: &NSEvent) {}
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }
        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            false
        }
    }

    unsafe impl NSObjectProtocol for TSReminderContentView {}
);

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
    #[name = "TSReminderInputButtonHandler"]
    struct TSReminderInputButtonHandler;

    impl TSReminderInputButtonHandler {
        #[unsafe(method(confirmInput:))]
        fn confirm_input(&self, _sender: Option<&NSButton>) {
            INPUT_CONFIRMED.with(|confirmed| *confirmed.borrow_mut() = true);
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            app.stopModal();
        }

        #[unsafe(method(cancelInput:))]
        fn cancel_input(&self, _sender: Option<&NSButton>) {
            INPUT_CONFIRMED.with(|confirmed| *confirmed.borrow_mut() = false);
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            app.stopModal();
        }
    }

    unsafe impl NSObjectProtocol for TSReminderInputButtonHandler {}
);

fn centered_rect(screen_frame: NSRect, width: f64, height: f64) -> NSRect {
    let x = screen_frame.origin.x + (screen_frame.size.width - width) / 2.0;
    let y = screen_frame.origin.y + (screen_frame.size.height - height) / 2.0;
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

fn run_native_enter_activity_dialog(mtm: MainThreadMarker, app: &NSApplication) -> Option<String> {
    INPUT_CONFIRMED.with(|confirmed| *confirmed.borrow_mut() = false);

    let screen_frame = NSScreen::mainScreen(mtm)
        .map(|s| s.frame())
        .unwrap_or_else(|| NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0)));
    let panel_alloc = NSPanel::alloc(mtm);
    let panel: Retained<NSPanel> = NSPanel::initWithContentRect_styleMask_backing_defer(
        panel_alloc,
        centered_rect(screen_frame, 520.0, 160.0),
        NSWindowStyleMask::Titled,
        NSBackingStoreType::Buffered,
        false,
    );
    panel.setTitle(&NSString::from_str("Enter activity"));
    unsafe { panel.setReleasedWhenClosed(false) };

    let content_rect = panel.contentRectForFrameRect(panel.frame());
    let content_alloc = NSView::alloc(mtm);
    let content: Retained<NSView> =
        unsafe { msg_send![content_alloc, initWithFrame: content_rect] };
    panel.setContentView(Some(&content));

    let label_alloc = NSTextField::alloc(mtm);
    let label: Retained<NSTextField> = unsafe {
        msg_send![
            label_alloc,
            initWithFrame: NSRect::new(NSPoint::new(20.0, 108.0), NSSize::new(480.0, 22.0))
        ]
    };
    let prompt = NSString::from_str("Enter activity:");
    let _: () = unsafe { msg_send![&*label, setStringValue: &*prompt] };
    label.setEditable(false);
    label.setSelectable(false);
    label.setBezeled(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    content.addSubview(&label);

    let input_alloc = NSTextField::alloc(mtm);
    let input: Retained<NSTextField> = unsafe {
        msg_send![
            input_alloc,
            initWithFrame: NSRect::new(NSPoint::new(20.0, 64.0), NSSize::new(480.0, 28.0))
        ]
    };
    input.setEditable(true);
    input.setSelectable(true);
    let placeholder = NSString::from_str("Paste or type the activity name");
    input.setPlaceholderString(Some(&placeholder));
    content.addSubview(&input);

    let handler_alloc = TSReminderInputButtonHandler::alloc(mtm);
    let handler: Retained<TSReminderInputButtonHandler> = unsafe { msg_send![handler_alloc, init] };

    let cancel = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Cancel"),
            Some(handler.as_ref() as &AnyObject),
            Some(objc2::sel!(cancelInput:)),
            mtm,
        )
    };
    cancel.setFrame(NSRect::new(
        NSPoint::new(330.0, 20.0),
        NSSize::new(80.0, 30.0),
    ));
    content.addSubview(&cancel);

    let ok = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("OK"),
            Some(handler.as_ref() as &AnyObject),
            Some(objc2::sel!(confirmInput:)),
            mtm,
        )
    };
    ok.setFrame(NSRect::new(
        NSPoint::new(420.0, 20.0),
        NSSize::new(80.0, 30.0),
    ));
    content.addSubview(&ok);

    panel.setInitialFirstResponder(Some(input.as_ref() as &NSView));
    let _: () = unsafe { msg_send![&panel, makeKeyAndOrderFront: None::<&AnyObject>] };
    unsafe { input.selectText(None) };
    let _ = app.runModalForWindow(&panel);
    let _: () = unsafe { msg_send![&panel, orderOut: None::<&AnyObject>] };

    if !INPUT_CONFIRMED.with(|confirmed| *confirmed.borrow()) {
        return None;
    }

    let value: Retained<NSString> = unsafe { msg_send![&*input, stringValue] };
    let activity = value.to_string();
    let activity = activity.trim().to_string();
    if activity.is_empty() {
        None
    } else {
        Some(activity)
    }
}

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
            let handler: Retained<TSReminderButtonHandler> =
                unsafe { msg_send![handler_alloc, init] };
            let sel_choice_clicked = objc2::sel!(choiceClicked:);

            // Panel: full screen.
            let screen_frame = NSScreen::mainScreen(mtm)
                .map(|s| s.frame())
                .unwrap_or_else(|| NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0)));
            let style = NSWindowStyleMask::Titled; // No Closable: only button-clicks dismiss
            let panel_alloc = NSPanel::alloc(mtm);
            let panel: Retained<NSPanel> = NSPanel::initWithContentRect_styleMask_backing_defer(
                panel_alloc,
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 400.0)),
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            panel.setFrame_display(screen_frame, true);
            panel.setTitle(&NSString::from_str("What are you working on?"));
            unsafe { panel.setReleasedWhenClosed(false) };
            let panel_delegate_alloc = TSReminderPanelDelegate::alloc(mtm);
            let panel_delegate: Retained<TSReminderPanelDelegate> =
                unsafe { msg_send![panel_delegate_alloc, init] };
            panel.setDelegate(Some(ProtocolObject::from_ref(&*panel_delegate)));

            // Content view: fill panel content area (resize with window).
            // TSReminderContentView swallows keystrokes; only mouse and scroll work.
            let content_rect = panel.contentRectForFrameRect(screen_frame);
            let content_alloc = TSReminderContentView::alloc(mtm);
            let content: Retained<TSReminderContentView> =
                unsafe { msg_send![content_alloc, initWithFrame: content_rect] };
            content.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable
                    | NSAutoresizingMaskOptions::ViewMinXMargin
                    | NSAutoresizingMaskOptions::ViewMaxXMargin
                    | NSAutoresizingMaskOptions::ViewMinYMargin
                    | NSAutoresizingMaskOptions::ViewMaxYMargin,
            );
            panel.setContentView(Some(&content));
            panel.setInitialFirstResponder(Some(content.as_ref() as &NSView));

            // Vertical stack for buttons. Height = ~32pt per button (24pt + 8pt spacing).
            let button_width: f64 = 280.0;
            let button_height: f64 = 32.0;
            let stack_height = (choices.len() as f64 * button_height).max(160.0);
            let stack_frame = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(button_width, stack_height),
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

            // Container to center the stack horizontally within the scroll area.
            let scroll_width = content_rect.size.width - 40.0;
            let scroll_height = content_rect.size.height - 40.0;
            let doc_height = scroll_height.max(stack_height);
            let stack_center_x = (scroll_width - button_width) / 2.0;
            let container_frame = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(scroll_width, doc_height),
            );
            let container_alloc = NSView::alloc(mtm);
            let container: Retained<NSView> =
                unsafe { msg_send![container_alloc, initWithFrame: container_frame] };
            stack.setFrame(NSRect::new(
                NSPoint::new(stack_center_x, doc_height - stack_height),
                NSSize::new(button_width, stack_height),
            ));
            container.addSubview(&stack);

            // Scroll view: fill content (with insets for padding).
            let scroll_frame = NSRect::new(
                NSPoint::new(20.0, 20.0),
                NSSize::new(scroll_width, scroll_height),
            );
            let scroll_alloc = NSScrollView::alloc(mtm);
            let scroll: Retained<NSScrollView> =
                unsafe { msg_send![scroll_alloc, initWithFrame: scroll_frame] };
            scroll.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable
                    | NSAutoresizingMaskOptions::ViewMinXMargin
                    | NSAutoresizingMaskOptions::ViewMaxXMargin
                    | NSAutoresizingMaskOptions::ViewMinYMargin
                    | NSAutoresizingMaskOptions::ViewMaxYMargin,
            );
            scroll.setDocumentView(Some(&container));
            scroll.setHasVerticalScroller(true);
            scroll.setHasHorizontalScroller(false);
            scroll.setAutohidesScrollers(true);
            content.addSubview(&scroll);
            panel.orderFrontRegardless();

            // Re-show if dismissed without a button choice (e.g. process killed).
            loop {
                DIALOG_RESULT.with(|r| *r.borrow_mut() = None);
                let _ = app.runModalForWindow(&panel);
                match DIALOG_RESULT.with(|r| r.borrow().clone()) {
                    Some(selected) if selected == "Enter new activity..." => {
                        let _: () = unsafe { msg_send![&panel, orderOut: None::<&AnyObject>] };
                        if let Some(activity) = run_native_enter_activity_dialog(mtm, &app) {
                            DIALOG_RESULT.with(|r| *r.borrow_mut() = Some(activity));
                            break;
                        }
                        panel.orderFrontRegardless();
                    }
                    Some(_) => break,
                    None => panel.orderFrontRegardless(),
                }
            }

            app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
            let _: () = unsafe { msg_send![&app, stop: None::<&AnyObject>] };
        }
    }
);
