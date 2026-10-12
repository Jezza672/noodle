//! Windows the host makes for plugin editors.
//!
//! A plugin editor either floats (the plugin makes its own top-level window)
//! or is *embedded*: the host makes a native window and hands the plugin a
//! handle to draw into. Embedding is what most plugins support best, and on
//! Windows and macOS it is often all they support. The application supplies
//! the windows through a [`set_window_factory`]; macOS and Windows have one
//! built in (installed by [`adopt_current_thread`](crate::adopt_current_thread),
//! since the window belongs to the thread that pumps its messages). Where
//! there is no factory (Linux), editors float.

use std::ffi::c_void;
use std::sync::OnceLock;

/// The native handle a plugin is given to embed its editor in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentWindow {
    /// An `NSView*`.
    Cocoa(*mut c_void),
    /// An `HWND`.
    Win32(*mut c_void),
    /// An X11 window ID.
    X11(std::ffi::c_ulong),
}

/// A native window an editor is embedded in. It is made, used and dropped on
/// the plugin thread; sizes are the content area, in the units the plugin
/// API for the window system uses (points on macOS, pixels elsewhere).
pub trait EditorWindow {
    /// What to give the plugin as its parent.
    fn parent(&self) -> ParentWindow;
    /// Sets the content size, and whether the user may resize the window.
    fn set_size(&mut self, width: u32, height: u32, resizable: bool);
    /// The content size now (the user can drag it).
    fn size(&self) -> (u32, u32);
    fn show(&mut self);
    fn hide(&mut self);
    /// Whether the user closed the window.
    fn is_closed(&self) -> bool;
}

type Factory = Box<dyn Fn(&str) -> Option<Box<dyn EditorWindow>> + Send + Sync>;

static FACTORY: OnceLock<Factory> = OnceLock::new();

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// Installs the factory for this system's native windows, where there is one
/// (macOS and Windows). Does nothing if a factory is already set.
pub(crate) fn install_native() {
    #[cfg(target_os = "macos")]
    let _ = set_window_factory(macos::make);
    #[cfg(windows)]
    let _ = set_window_factory(windows::make);
}

/// Sets how editor windows are made: given a title, a hidden window to embed
/// an editor in (or `None` if it can't be made, and the editor floats
/// instead). Called on the plugin thread. Returns false if one is already set.
pub fn set_window_factory(
    factory: impl Fn(&str) -> Option<Box<dyn EditorWindow>> + Send + Sync + 'static,
) -> bool {
    FACTORY.set(Box::new(factory)).is_ok()
}

pub(crate) fn make(title: &str) -> Option<Box<dyn EditorWindow>> {
    FACTORY.get().and_then(|factory| factory(title))
}

pub(crate) fn available() -> bool {
    FACTORY.get().is_some()
}

/// Handles the window-system events waiting for this thread, for a program
/// with no event loop of its own (the plugin host process, whose main thread
/// is the plugin thread). Without it an editor window would never redraw or
/// take a click. Does nothing where editors float on Linux.
pub(crate) fn pump_events() {
    #[cfg(target_os = "macos")]
    macos::pump_events();
    #[cfg(windows)]
    windows::pump_events();
}
