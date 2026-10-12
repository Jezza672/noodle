//! An editor window on macOS: an `NSWindow` whose content view is the
//! plugin's parent. Made and used on the main thread.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventMask, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString,
};

use super::{EditorWindow, ParentWindow};

pub(super) struct MacWindow {
    window: Retained<NSWindow>,
    view: Retained<NSView>,
    mtm: MainThreadMarker,
    /// Shown by us: until then, not being on screen isn't the user closing it.
    shown: bool,
}

/// Sets the application up the first time (a plugin host has no `NSApp`
/// run loop, and no Dock icon to show), then hands every waiting event to
/// AppKit. Main thread only; does nothing elsewhere.
pub(super) fn pump_events() {
    use std::sync::Once;
    static SETUP: Once = Once::new();
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    SETUP.call_once(|| {
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        // SAFETY: once, on the main thread, before the first event.
        unsafe { app.finishLaunching() };
    });
    // A burst is bounded so a flood of events can't starve the control loop.
    for _ in 0..256 {
        // SAFETY: the main thread, the default mode, and an expiry already
        // past so it never blocks.
        let event = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&NSDate::distantPast()),
                NSDefaultRunLoopMode,
                true,
            )
        };
        let Some(event) = event else { break };
        // SAFETY: an event AppKit just gave us.
        unsafe { app.sendEvent(&event) };
    }
}

pub(super) fn make(title: &str) -> Option<Box<dyn EditorWindow>> {
    let mtm = MainThreadMarker::new()?;
    let frame = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(400.0, 300.0));
    let style =
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
    // SAFETY: a plain window made on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            frame,
            style,
            NSBackingStoreType::NSBackingStoreBuffered,
            false,
        )
    };
    // We keep it (and drop it ourselves), so closing it mustn't free it.
    // SAFETY: the window is ours alone.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    let view = window.contentView()?;
    Some(Box::new(MacWindow {
        window,
        view,
        mtm,
        shown: false,
    }))
}

impl EditorWindow for MacWindow {
    fn parent(&self) -> ParentWindow {
        ParentWindow::Cocoa(Retained::as_ptr(&self.view) as *mut c_void)
    }

    fn set_size(&mut self, width: u32, height: u32, resizable: bool) {
        let mut style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable;
        if resizable {
            style |= NSWindowStyleMask::Resizable;
        }
        self.window.setStyleMask(style);
        self.window
            .setContentSize(NSSize::new(f64::from(width), f64::from(height)));
    }

    fn size(&self) -> (u32, u32) {
        let size = self.view.frame().size;
        (size.width.round() as u32, size.height.round() as u32)
    }

    fn show(&mut self) {
        // A plugin host runs as a background application, so ask for focus,
        // or the editor shows but takes no keys.
        // `activate` needs macOS 14; this works on all we support.
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.mtm).activateIgnoringOtherApps(true);
        self.window.makeKeyAndOrderFront(None);
        self.shown = true;
    }

    fn hide(&mut self) {
        self.window.orderOut(None);
        self.shown = false;
    }

    fn is_closed(&self) -> bool {
        // The close button takes the window off the screen. Minimizing and
        // hiding the whole application do too, and aren't closing.
        // SAFETY: a plain query, on the main thread.
        let app_hidden = unsafe { NSApplication::sharedApplication(self.mtm).isHidden() };
        self.shown && !self.window.isVisible() && !self.window.isMiniaturized() && !app_hidden
    }
}

impl Drop for MacWindow {
    fn drop(&mut self) {
        // The plugin's view has been removed by then; close the window.
        self.window.close();
    }
}
