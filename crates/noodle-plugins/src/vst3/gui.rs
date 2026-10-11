//! VST3 editors: a plugin's `IPlugView`, embedded in a window the app makes.
//!
//! VST3 editors are always embedded (the standard has no floating windows),
//! so they need the window factory the application sets (see
//! [`crate::set_window_factory`]; macOS and Windows have one built in). Where
//! there is none, opening an editor says so.
//!
//! Everything here belongs to the plugin thread: the views live in a
//! thread-local table keyed by an ID the node holds, and the editor
//! commands are jobs run on that thread. A plugin's calls back into the host
//! (the component handler, `IPlugFrame`) only set flags, which the thread
//! acts on the next time it looks.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

use crate::editor::Captured;
use crate::host::{Flags, HostError};
use crate::thread::{self, COMMAND_TIMEOUT};
use crate::window::{self, EditorWindow, ParentWindow};

use super::classes::{Frame, raw};
use super::module::Module;
use super::plugin::{Control, Plugin};

/// Set in the IDs of VST3 editors, so they can't be mistaken for a CLAP
/// instance's key.
const KEY_BIT: u64 = 1 << 63;

static NEXT_KEY: AtomicU64 = AtomicU64::new(0);

/// Whether `key` is a VST3 editor's.
pub(crate) fn is_key(key: u64) -> bool {
    key & KEY_BIT != 0
}

/// What a plugin is given when it has no size of its own to offer.
const DEFAULT_SIZE: (u32, u32) = (800, 600);

/// One plugin's editor.
struct Gui {
    control: Control,
    flags: Arc<Flags>,
    frame: ComWrapper<Frame>,
    view: Option<ComPtr<IPlugView>>,
    /// The host window the view is in, and the size it was last given, to
    /// tell when the user has resized it.
    window: Option<Box<dyn EditorWindow>>,
    size: (u32, u32),
    // Last: the plugin's code must stay loaded while the above are released.
    _module: Arc<Module>,
}

impl Drop for Gui {
    fn drop(&mut self) {
        close_view(self);
    }
}

thread_local! {
    static GUIS: RefCell<HashMap<u64, Gui>> = RefCell::new(HashMap::new());
}

/// What moves a plugin's COM pointers to the plugin thread. They are only
/// used there.
struct OnPluginThread<T>(T);
// SAFETY: the value is only touched on the plugin thread.
unsafe impl<T> Send for OnPluginThread<T> {}

/// Makes the plugin's editor available under a new key, on the plugin
/// thread. `flags` is shared with whoever asks whether the editor is open.
pub(crate) fn register(plugin: &Plugin, flags: &Arc<Flags>) -> u64 {
    let key = KEY_BIT | NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    let gui = OnPluginThread(Gui {
        control: plugin.control(),
        flags: Arc::clone(flags),
        frame: ComWrapper::new(Frame {
            flags: Arc::clone(flags),
        }),
        view: None,
        window: None,
        size: (0, 0),
        _module: Arc::clone(plugin.module()),
    });
    thread::run_job_detached(move || {
        let gui = gui;
        GUIS.with(|guis| guis.borrow_mut().insert(key, gui.0));
    });
    key
}

/// Closes the editor and forgets it, on the plugin thread, before the plugin
/// itself is dropped.
pub(crate) fn unregister(key: u64) {
    let gui = GUIS.with(|guis| guis.borrow_mut().remove(&key));
    // Dropped outside the table's borrow: closing the view calls the plugin.
    drop(gui);
}

/// Whether any plugin has an editor to look after.
pub(crate) fn any() -> bool {
    GUIS.with(|guis| guis.try_borrow().map_or(true, |g| !g.is_empty()))
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

/// Reads the plugin's state and parameter values.
pub(crate) fn capture(key: u64) -> Result<Captured, HostError> {
    thread::run_job(COMMAND_TIMEOUT, move || {
        with_gui(key, |gui| {
            Ok(Captured {
                state: gui.control.save_state()?,
                params: gui.control.values(),
            })
        })
    })?
}

/// Closed windows and resize requests, for every editor. Called by the
/// plugin thread each time round.
pub(crate) fn housekeeping() {
    GUIS.with(|guis| {
        let Ok(mut guis) = guis.try_borrow_mut() else {
            return;
        };
        for gui in guis.values_mut().filter(|g| g.view.is_some()) {
            if gui.window.as_ref().is_some_and(|w| w.is_closed()) {
                close_view(gui);
            } else {
                resize(gui);
            }
        }
    });
}

fn rect_of((width, height): (u32, u32)) -> ViewRect {
    ViewRect {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    }
}

fn size_of(rect: &ViewRect) -> (u32, u32) {
    (
        (rect.right - rect.left).max(1) as u32,
        (rect.bottom - rect.top).max(1) as u32,
    )
}

fn open_view(gui: &mut Gui) -> Result<(), HostError> {
    if gui.view.is_some() {
        if let Some(window) = gui.window.as_mut() {
            window.show();
        }
        return Ok(());
    }
    let mut window = window::make("Plugin editor").ok_or_else(|| {
        HostError(
            "VST3 editors need a window made by the app, which this system doesn't have".into(),
        )
    })?;
    let controller = gui
        .control
        .controller
        .as_ref()
        .ok_or_else(|| HostError("this plugin has no editor".into()))?;
    // SAFETY: a plain call into the plugin; "editor" is the name the
    // standard gives the main view.
    let view = unsafe { controller.createView(c"editor".as_ptr()) };
    // SAFETY: on success the plugin returns a view with a reference for us.
    let view = unsafe { ComPtr::from_raw(view) }
        .ok_or_else(|| HostError("this plugin has no editor".into()))?;

    let (parent, platform) = match window.parent() {
        ParentWindow::Cocoa(v) => (v, kPlatformTypeNSView),
        ParentWindow::Win32(h) => (h, kPlatformTypeHWND),
        ParentWindow::X11(id) => (id as usize as *mut c_void, kPlatformTypeX11EmbedWindowID),
    };
    // SAFETY: plain calls into the plugin's view, in the order the standard
    // gives: its size, the frame, then attaching it to the parent window.
    unsafe {
        if view.isPlatformTypeSupported(platform) != kResultTrue {
            return Err(HostError(
                "this plugin's editor can't be embedded on this system".into(),
            ));
        }
        let mut rect = rect_of((0, 0));
        let size = if view.getSize(&mut rect) == kResultOk {
            size_of(&rect)
        } else {
            DEFAULT_SIZE
        };
        let resizable = view.canResize() == kResultTrue;
        window.set_size(size.0, size.1, resizable);
        gui.flags.resize.store(0, Ordering::Release);
        view.setFrame(raw::<IPlugFrame, _>(&gui.frame));
        if view.attached(parent, platform) != kResultOk {
            view.setFrame(std::ptr::null_mut());
            return Err(HostError("the plugin wouldn't attach its editor".into()));
        }
    }
    gui.size = window.size();
    window.show();
    gui.view = Some(view);
    gui.window = Some(window);
    gui.flags.editor_closed.store(false, Ordering::Release);
    gui.flags.editor_open.store(true, Ordering::Release);
    Ok(())
}

/// Takes the view down before the window it is in.
fn close_view(gui: &mut Gui) {
    if let Some(view) = gui.view.take() {
        // SAFETY: plain calls into the plugin's view.
        unsafe {
            view.removed();
            view.setFrame(std::ptr::null_mut());
        }
    }
    if let Some(mut window) = gui.window.take() {
        window.hide();
    }
    gui.flags.resize.store(0, Ordering::Release);
    gui.flags.editor_open.store(false, Ordering::Release);
}

/// Carries out a resize the plugin asked for, or the user made by dragging
/// the window's edge, and tells the plugin the size it now has.
fn resize(gui: &mut Gui) {
    let (Some(view), Some(window)) = (gui.view.as_ref(), gui.window.as_mut()) else {
        return;
    };
    // SAFETY: plain calls into the plugin's view.
    unsafe {
        let resizable = view.canResize() == kResultTrue;
        let asked = gui.flags.resize.swap(0, Ordering::AcqRel);
        if asked != 0 {
            window.set_size(
                (asked >> 32) as u32,
                (asked & 0xFFFF_FFFF) as u32,
                resizable,
            );
        } else if resizable && window.size() != gui.size && window.size() != (0, 0) {
            // The user dragged the edge: the plugin may want another size.
            let wanted = window.size();
            let mut rect = rect_of(wanted);
            view.checkSizeConstraint(&mut rect);
            let fitted = size_of(&rect);
            if fitted != wanted {
                window.set_size(fitted.0, fitted.1, true);
            }
        } else {
            return;
        }
        gui.size = window.size();
        let mut rect = rect_of(gui.size);
        view.onSize(&mut rect);
    }
}
