//! Audio Unit editors: the unit's own Cocoa view, or Apple's generic
//! parameter view when it has none, embedded in a window the app makes.
//!
//! A unit publishes its view as a bundle and a factory class
//! (`kAudioUnitProperty_CocoaUI`, the `AUCocoaUIBase` protocol). A unit with
//! no view of its own gets `AUGenericView` from CoreAudioKit, so every unit
//! has an editor. (A view-controller editor, `AUViewController`, comes with
//! AUv3 instantiation, which is not done yet: it needs the `AUAudioUnit`.)
//!
//! Everything here belongs to the plugin thread, which is the main thread on
//! macOS (see [`crate::adopt_current_thread`]): the views live in a
//! thread-local table keyed by an ID the node holds, and the editor commands
//! are jobs run on that thread. The unit itself is only borrowed: the node
//! unregisters the editor on the plugin thread before it lets the unit go.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{msg_send, msg_send_id, sel};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView};
use objc2_audio_toolbox::AudioUnit;
use objc2_foundation::{NSPoint, NSSize, NSString};

use crate::editor::Captured;
use crate::host::{Flags, HostError};
use crate::thread::{self, COMMAND_TIMEOUT};
use crate::window::{self, EditorWindow, ParentWindow};

use super::plugin::{cocoa_view_of, parameter_of, save_state_of};

// The generic view's class lives here; linking it registers the class.
#[link(name = "CoreAudioKit", kind = "framework")]
unsafe extern "C" {}

/// Set in the IDs of AU editors, so they can't be mistaken for another
/// format's key (CLAP keys have no high bits; VST3 uses 63, sandboxed
/// instances 62).
const KEY_BIT: u64 = 1 << 61;

static NEXT_KEY: AtomicU64 = AtomicU64::new(0);

/// Whether `key` is an AU editor's.
pub(crate) fn is_key(key: u64) -> bool {
    key & KEY_BIT != 0 && key & (3 << 62) == 0
}

/// What a view that doesn't say how big it is is given.
const DEFAULT_SIZE: (u32, u32) = (480, 320);

/// One unit's editor.
struct Gui {
    /// Borrowed from the node, which unregisters this before the unit goes.
    unit: AudioUnit,
    /// The parameters a capture reads.
    params: Vec<u32>,
    flags: Arc<Flags>,
    view: Option<Retained<NSView>>,
    /// The host window the view is in.
    window: Option<Box<dyn EditorWindow>>,
}

impl Drop for Gui {
    fn drop(&mut self) {
        close_view(self);
    }
}

thread_local! {
    static GUIS: RefCell<HashMap<u64, Gui>> = RefCell::new(HashMap::new());
}

/// What moves a unit's pointers to the plugin thread. They are only used
/// there.
struct OnPluginThread<T>(T);
// SAFETY: the value is only touched on the plugin thread.
unsafe impl<T> Send for OnPluginThread<T> {}

/// Makes the unit's editor available under a new key, on the plugin thread.
/// `flags` is shared with whoever asks whether the editor is open. The unit
/// must stay alive until [`unregister`] has run.
pub(crate) fn register(unit: AudioUnit, params: Vec<u32>, flags: &Arc<Flags>) -> u64 {
    let key = KEY_BIT | NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    let gui = OnPluginThread(Gui {
        unit,
        params,
        flags: Arc::clone(flags),
        view: None,
        window: None,
    });
    // Waited for, so the editor can be opened as soon as the node is
    // findable. (If this fails the editor reports the plugin isn't running.)
    let _ = thread::run_job(COMMAND_TIMEOUT, move || {
        let gui = gui;
        GUIS.with(|guis| guis.borrow_mut().insert(key, gui.0));
    });
    key
}

/// Closes the editor and forgets it, on the plugin thread, before the unit
/// itself is dropped.
pub(crate) fn unregister(key: u64) {
    let gui = GUIS.with(|guis| guis.borrow_mut().remove(&key));
    // Dropped outside the table's borrow: closing the view calls the unit.
    drop(gui);
}

/// Whether any unit has an editor to look after.
pub(crate) fn any() -> bool {
    GUIS.with(|guis| {
        guis.try_borrow()
            .map_or(true, |g| g.values().any(|g| g.view.is_some()))
    })
}

fn with_gui<T>(key: u64, f: impl FnOnce(&mut Gui) -> Result<T, HostError>) -> Result<T, HostError> {
    GUIS.with(|guis| {
        let mut guis = guis
            .try_borrow_mut()
            .map_err(|_| HostError("the plugin thread is busy".into()))?;
        let gui = guis
            .get_mut(&key)
            .ok_or_else(|| HostError("the plugin is no longer running".into()))?;
        f(gui)
    })
}

/// Opens the editor (or shows it again if it was hidden).
pub(crate) fn open(key: u64) -> Result<(), HostError> {
    thread::run_job(COMMAND_TIMEOUT, move || with_gui(key, open_view))?
}

/// Closes the editor, if it is open.
pub(crate) fn close(key: u64) {
    thread::run_job_detached(move || {
        let _ = with_gui(key, |gui| {
            close_view(gui);
            Ok(())
        });
    });
}

/// Reads the unit's state and parameter values.
pub(crate) fn capture(key: u64) -> Result<Captured, HostError> {
    thread::run_job(COMMAND_TIMEOUT, move || {
        with_gui(key, |gui| {
            Ok(Captured {
                state: save_state_of(gui.unit)?,
                params: gui
                    .params
                    .iter()
                    .filter_map(|&id| parameter_of(gui.unit, id).map(|v| (id, f64::from(v))))
                    .collect(),
            })
        })
    })?
}

/// Editors the user closed. Called by the plugin thread each time round.
pub(crate) fn housekeeping() {
    GUIS.with(|guis| {
        let Ok(mut guis) = guis.try_borrow_mut() else {
            return;
        };
        for gui in guis.values_mut().filter(|g| g.view.is_some()) {
            if gui.window.as_ref().is_some_and(|w| w.is_closed()) {
                close_view(gui);
            }
        }
    });
}

/// The unit's own view, from the factory it publishes.
fn custom_view(unit: AudioUnit) -> Option<Retained<NSView>> {
    let (url, class_name) = cocoa_view_of(unit)?;
    let bundle_class = AnyClass::get("NSBundle")?;
    let url: *const c_void = std::ptr::from_ref(&*url).cast();
    // SAFETY: plain Objective-C calls on the main thread: the url is a
    // CFURL, which is an NSURL; the bundle is asked for its class by name;
    // the factory protocol is `AUCocoaUIBase`.
    unsafe {
        let bundle: Option<Retained<AnyObject>> =
            msg_send_id![bundle_class, bundleWithURL: url.cast::<AnyObject>()];
        let bundle = bundle?;
        let loaded: bool = msg_send![&bundle, load];
        if !loaded {
            return None;
        }
        let name = NSString::from_str(&class_name);
        let class: Option<&AnyClass> = msg_send![&bundle, classNamed: &*name];
        let alloc: Allocated<AnyObject> = msg_send_id![class?, alloc];
        let factory: Option<Retained<AnyObject>> = msg_send_id![alloc, init];
        let factory = factory?;
        // Sent by hand: the runtime would check `AudioUnit`'s encoding
        // against the factory's own spelling of the type, and refuse it.
        let send: unsafe extern "C" fn(*mut AnyObject, Sel, AudioUnit, NSSize) -> *mut AnyObject =
            std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C" fn());
        let view = send(
            Retained::as_ptr(&factory).cast_mut(),
            sel!(uiViewForAudioUnit:withSize:),
            unit,
            NSSize::new(0.0, 0.0),
        );
        // Handed over autoreleased.
        Retained::retain(view.cast::<NSView>())
    }
}

/// Apple's parameter view, for a unit with none of its own.
fn generic_view(unit: AudioUnit) -> Option<Retained<NSView>> {
    let class = AnyClass::get("AUGenericView")?;
    // SAFETY: plain Objective-C calls on the main thread. The init message
    // is sent by hand because the class doesn't say what type `AudioUnit` is
    // (the runtime's check of the argument's encoding would refuse any).
    unsafe {
        let object: *mut AnyObject = msg_send![class, alloc];
        let send: unsafe extern "C" fn(*mut AnyObject, Sel, AudioUnit) -> *mut AnyObject =
            std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C" fn());
        let view = send(object, sel!(initWithAudioUnit:), unit);
        Retained::from_raw(view.cast::<NSView>())
    }
}

/// Opens the editor (or shows it again if it was hidden).
fn open_view(gui: &mut Gui) -> Result<(), HostError> {
    if gui.view.is_some() {
        if let Some(window) = gui.window.as_mut() {
            window.show();
        }
        return Ok(());
    }
    let mut window = window::make("Audio Unit editor").ok_or_else(|| {
        HostError(
            "Audio Unit editors need a window made by the app, which this system doesn't have"
                .into(),
        )
    })?;
    let ParentWindow::Cocoa(parent) = window.parent() else {
        return Err(HostError(
            "this window can't hold an Audio Unit view".into(),
        ));
    };
    let view = custom_view(gui.unit)
        .or_else(|| generic_view(gui.unit))
        .ok_or_else(|| HostError("this Audio Unit has no editor".into()))?;
    let frame = view.frame();
    let size = if frame.size.width >= 1.0 && frame.size.height >= 1.0 {
        (
            frame.size.width.round() as u32,
            frame.size.height.round() as u32,
        )
    } else {
        DEFAULT_SIZE
    };
    // SAFETY: plain view calls on the main thread, with a view we own and
    // the content view of the window.
    let mask = unsafe { view.autoresizingMask() };
    let resizable = mask.contains(NSAutoresizingMaskOptions::NSViewWidthSizable)
        || mask.contains(NSAutoresizingMaskOptions::NSViewHeightSizable);
    window.set_size(size.0, size.1, resizable);
    // SAFETY: the window's content view, alive as long as the window.
    let parent = unsafe { &*parent.cast::<NSView>() };
    unsafe {
        view.setFrameOrigin(NSPoint::new(0.0, 0.0));
        parent.addSubview(&view);
    }
    window.show();
    gui.view = Some(view);
    gui.window = Some(window);
    gui.flags.editor_closed.store(false, Ordering::Release);
    gui.flags.editor_open.store(true, Ordering::Release);
    Ok(())
}

/// Takes the view out of its window and closes the window.
fn close_view(gui: &mut Gui) {
    if let Some(view) = gui.view.take() {
        // SAFETY: a plain call on the main thread.
        unsafe { view.removeFromSuperview() };
    }
    gui.window = None;
    gui.flags.editor_open.store(false, Ordering::Release);
}
