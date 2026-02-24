//! Native macOS reminder dialog using AppKit NSAlert (supports many buttons; one click = done).
//! Used when the daemon spawns `ts --reminder-dialog choice1 choice2 ...` via launchctl asuser.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSImage,
    NSModalResponse,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSString};
use std::cell::RefCell;
use std::path::PathBuf;

// NSAlert button return codes (first button = 1000, second = 1001, ...)
const NSALERT_FIRST_BUTTON_RETURN: NSModalResponse = 1000;

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
            // Force to front (deprecated on macOS 14+ but still helps on earlier versions).
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);

            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str("What are you working on?"));
            // One button per choice (NSAlert supports any number of buttons).
            for choice in choices.iter() {
                alert.addButtonWithTitle(&NSString::from_str(choice));
            }

            // Force the alert window on top of other apps.
            let alert_window = alert.window();
            alert_window.orderFrontRegardless();

            let response: NSModalResponse = unsafe { msg_send![&alert, runModal] };
            let idx = response as isize - NSALERT_FIRST_BUTTON_RETURN;
            if idx >= 0 && (idx as usize) < choices.len() {
                let selected = choices[idx as usize].clone();
                DIALOG_RESULT.with(|r| *r.borrow_mut() = Some(selected));
            }

            // Prohibited: hide from dock and Cmd-Tab again before exiting.
            app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
            let _: () = unsafe { msg_send![&app, stop: None::<&AnyObject>] };
        }
    }
);
