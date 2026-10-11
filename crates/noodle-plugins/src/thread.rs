//! The shared plugin thread.
//!
//! CLAP wants a plugin's "main thread" calls (creating it, saving its state,
//! its editor, its timers) all made from one thread, and JUCE-based plugins
//! and editors expect it to be the same thread for every plugin in the
//! process. So there is one thread, started on first use, which owns every
//! live instance. Nodes (which must be `Send`) hold a key and the audio
//! processor, which CLAP lets the audio thread use, and talk to the instance
//! by sending this thread a command.
//!
//! That thread is the host's CLAP main thread but not the process's, which
//! is what a macOS editor wants (and a Windows editor needs a thread that
//! pumps messages). An application whose UI thread should be the plugin
//! thread calls [`adopt_current_thread`] and then [`pump`] each frame: the
//! same code then runs on that thread instead of one of its own.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGui, Window};
use clack_extensions::params::PluginParams;
use clack_extensions::timer::PluginTimer;
use clack_host::prelude::*;

use crate::editor::Captured;
use crate::host::{
    Flags, Host, HostError, LOAD_TIMEOUT, host_info, load_entry, load_state, new_instance,
    save_state_of, visible_params,
};
use crate::window::{self, EditorWindow, ParentWindow};

/// How long the thread waits for a command when no timer is due sooner.
const IDLE: Duration = Duration::from_millis(50);

/// How long an editor command (open, capture) may take.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// An instance the thread holds.
struct Live {
    // The instance must go before the entry whose code it runs.
    instance: PluginInstance<Host>,
    _entry: PluginEntry,
    flags: Arc<Flags>,
    /// The plugin's editor exists (shown or hidden), so `destroy` is owed.
    gui: bool,
    /// The host window an embedded editor is in, and the size it was last
    /// given, to tell when the user has resized it.
    window: Option<Box<dyn EditorWindow>>,
    size: (u32, u32),
}

impl Drop for Live {
    fn drop(&mut self) {
        // However it goes, the plugin's view goes before its window and the
        // plugin itself.
        destroy_editor(self);
    }
}

/// An instance, activated and ready.
pub(crate) struct Loaded {
    pub key: u64,
    processor: Option<StoppedPluginAudioProcessor<Host>>,
    pub flags: Arc<Flags>,
}

impl Loaded {
    /// The processor, which the node then owns (and gives back with `close`).
    pub fn take_processor(&mut self) -> StoppedPluginAudioProcessor<Host> {
        self.processor.take().expect("taken once")
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        // Dropped without the node having taken it: the caller timed out just
        // as the load finished. Let the thread deactivate the instance.
        if let Some(processor) = self.processor.take() {
            close(self.key, processor);
        }
    }
}

enum Command {
    Load {
        path: PathBuf,
        plugin_id: CString,
        state: Option<Vec<u8>>,
        sample_rate: f32,
        max_frames: usize,
        reply: Sender<Result<Loaded, HostError>>,
    },
    Close {
        key: u64,
        processor: StoppedPluginAudioProcessor<Host>,
    },
    Capture {
        key: u64,
        reply: Sender<Result<Captured, HostError>>,
    },
    /// Loads a plugin without activating it, only to see the state it saves.
    CaptureFresh {
        path: PathBuf,
        plugin_id: CString,
        state: Option<Vec<u8>>,
        reply: Sender<Result<Vec<u8>, HostError>>,
    },
    OpenEditor {
        key: u64,
        reply: Sender<Result<(), HostError>>,
    },
    CloseEditor {
        key: u64,
    },
}

/// The instances, and what the thread keeps for them.
#[derive(Default)]
struct Executor {
    live: HashMap<u64, Live>,
    next_key: u64,
}

impl Executor {
    fn handle(&mut self, command: Command) {
        handle(command, &mut self.live, &mut self.next_key);
    }

    /// Timers, callbacks and closed editors, for every instance.
    fn housekeeping(&mut self) {
        let now = Instant::now();
        for l in self.live.values_mut() {
            housekeeping(l, now);
        }
    }

    /// How long until the next timer is due (at most [`IDLE`]).
    fn next_wake(&self) -> Duration {
        let now = Instant::now();
        self.live
            .values()
            .filter_map(|l| l.flags.next_due())
            .min()
            .map_or(IDLE, |due| due.saturating_duration_since(now).min(IDLE))
    }
}

/// The thread that was adopted as the plugin thread, if any: its queue and
/// instances.
struct Adopted {
    receiver: Receiver<Command>,
    executor: Executor,
}

thread_local! {
    static ADOPTED: RefCell<Option<Adopted>> = const { RefCell::new(None) };
}

struct Queue {
    sender: Sender<Command>,
    /// Called after a command is queued for an adopted thread, to wake it.
    wake: Option<Box<dyn Fn() + Send + Sync>>,
}

static QUEUE: OnceLock<Queue> = OnceLock::new();

/// Makes the calling thread the plugin thread instead of starting one.
///
/// A plugin's editor wants the application's main thread on macOS, and a
/// window made on a thread that doesn't pump messages is dead on Windows. An
/// application whose UI thread is that thread calls this once at startup
/// (before any plugin is used) and then calls [`pump`] regularly from it.
/// Commands from other threads wait in a queue for `pump`, and `wake` is
/// called after each is queued (to request a repaint, say). Commands from
/// the adopted thread itself run on the spot. Returns false if the plugin
/// thread has already started or another thread was adopted.
pub fn adopt_current_thread(wake: impl Fn() + Send + Sync + 'static) -> bool {
    let (sender, receiver) = mpsc::channel();
    let queue = Queue {
        sender,
        wake: Some(Box::new(wake)),
    };
    if QUEUE.set(queue).is_err() {
        return false;
    }
    // Editor windows belong to this thread too.
    window::install_native();
    ADOPTED.with(|a| {
        *a.borrow_mut() = Some(Adopted {
            receiver,
            executor: Executor::default(),
        });
    });
    true
}

/// Runs what other threads have asked of the adopted plugin thread, and its
/// instances' timers. Returns how long until it should be called again, or
/// `None` if nothing is running that needs it (a queued command calls the
/// `wake` given to [`adopt_current_thread`]), or this isn't the adopted
/// thread.
pub fn pump() -> Option<Duration> {
    ADOPTED.with(|a| {
        let mut adopted = a.try_borrow_mut().ok()?;
        let adopted = adopted.as_mut()?;
        while let Ok(command) = adopted.receiver.try_recv() {
            adopted.executor.handle(command);
        }
        adopted.executor.housekeeping();
        (!adopted.executor.live.is_empty()).then(|| adopted.executor.next_wake())
    })
}

fn queue() -> &'static Queue {
    QUEUE.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        // If the thread can't start, every command fails with "ended".
        let _ = thread::Builder::new()
            .name("noodle-plugin-main".into())
            .spawn(move || run(receiver));
        Queue { sender, wake: None }
    })
}

impl Command {
    /// Whether the sender waits for an answer.
    fn waits(&self) -> bool {
        !matches!(self, Command::Close { .. } | Command::CloseEditor { .. })
    }
}

fn gone() -> HostError {
    HostError("the plugin thread ended".into())
}

fn send(command: Command) -> Result<(), HostError> {
    // On the adopted thread, run the command now (waiting on it would wait
    // for ourselves), unless a command is already running.
    let command = ADOPTED.with(|a| match a.try_borrow_mut() {
        Ok(mut adopted) => match adopted.as_mut() {
            Some(adopted) => {
                adopted.executor.handle(command);
                Ok(None)
            }
            None => Ok(Some(command)),
        },
        // A command is running on this (the adopted) thread and a plugin has
        // called back into us. A command that waits would wait for itself.
        Err(_) if command.waits() => Err(HostError("the plugin thread is busy".into())),
        Err(_) => Ok(Some(command)),
    })?;
    let Some(command) = command else {
        return Ok(());
    };
    let queue = queue();
    queue.sender.send(command).map_err(|_| gone())?;
    if let Some(wake) = &queue.wake {
        wake();
    }
    Ok(())
}

fn wait<T>(receiver: &Receiver<Result<T, HostError>>, timeout: Duration) -> Result<T, HostError> {
    receiver
        .recv_timeout(timeout)
        .map_err(|error| match error {
            RecvTimeoutError::Timeout => HostError(format!(
                "the plugin didn't answer within {timeout:?} (it may be busy or stuck)"
            )),
            RecvTimeoutError::Disconnected => gone(),
        })?
}

fn cstring(id: &str) -> Result<CString, HostError> {
    CString::new(id).map_err(|e| HostError(e.to_string()))
}

/// Loads the plugin, restores `state` into it if given, and activates it.
pub(crate) fn start(
    path: &Path,
    plugin_id: &str,
    state: Option<Vec<u8>>,
    sample_rate: f32,
    max_frames: usize,
) -> Result<Loaded, HostError> {
    let (reply, receiver) = mpsc::channel();
    send(Command::Load {
        path: path.to_owned(),
        plugin_id: cstring(plugin_id)?,
        state,
        sample_rate,
        max_frames,
        reply,
    })?;
    wait(&receiver, LOAD_TIMEOUT)
}

/// Gives the processor back so the plugin is deactivated and destroyed on
/// the plugin thread.
pub(crate) fn close(key: u64, processor: StoppedPluginAudioProcessor<Host>) {
    let _ = send(Command::Close { key, processor });
}

pub(crate) fn capture(key: u64) -> Result<Captured, HostError> {
    let (reply, receiver) = mpsc::channel();
    send(Command::Capture { key, reply })?;
    wait(&receiver, COMMAND_TIMEOUT)
}

pub(crate) fn capture_fresh(
    path: &Path,
    plugin_id: &str,
    state: Option<&[u8]>,
) -> Result<Vec<u8>, HostError> {
    let (reply, receiver) = mpsc::channel();
    send(Command::CaptureFresh {
        path: path.to_owned(),
        plugin_id: cstring(plugin_id)?,
        state: state.map(<[u8]>::to_vec),
        reply,
    })?;
    wait(&receiver, LOAD_TIMEOUT)
}

pub(crate) fn open_editor(key: u64) -> Result<(), HostError> {
    let (reply, receiver) = mpsc::channel();
    send(Command::OpenEditor { key, reply })?;
    wait(&receiver, COMMAND_TIMEOUT)
}

pub(crate) fn close_editor(key: u64) {
    let _ = send(Command::CloseEditor { key });
}

fn run(receiver: Receiver<Command>) {
    let mut executor = Executor::default();
    loop {
        match receiver.recv_timeout(executor.next_wake()) {
            Ok(command) => executor.handle(command),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        executor.housekeeping();
    }
}

/// Timers, callbacks the plugin asked for, and an editor the plugin closed.
fn housekeeping(live: &mut Live, now: Instant) {
    if live.flags.callback.swap(false, Ordering::AcqRel) {
        live.instance.call_on_main_thread_callback();
    }
    let due = live.flags.take_due(now);
    if !due.is_empty() {
        let handle = live.instance.plugin_handle();
        if let Some(timer) = handle.get_extension::<PluginTimer>() {
            for id in due {
                timer.on_timer(&handle, id);
            }
        }
    }
    if live.flags.editor_closed.swap(false, Ordering::AcqRel)
        || live.window.as_ref().is_some_and(|w| w.is_closed())
    {
        destroy_editor(live);
    }
    resize_window(live);
}

/// Carries out a resize the plugin asked for, or the user made by dragging
/// the window's edge.
fn resize_window(live: &mut Live) {
    let Some(window) = live.window.as_mut() else {
        return;
    };
    let asked = live.flags.resize.swap(0, Ordering::AcqRel);
    let handle = live.instance.plugin_handle();
    let Some(gui) = handle.get_extension::<PluginGui>() else {
        return;
    };
    if asked != 0 {
        let size = ((asked >> 32) as u32, asked as u32);
        window.set_size(size.0, size.1, gui.can_resize(&handle));
        live.size = window.size();
        let _ = gui.set_size(
            &handle,
            GuiSize {
                width: live.size.0,
                height: live.size.1,
            },
        );
    } else if window.size() != live.size && window.size() != (0, 0) && gui.can_resize(&handle) {
        // The user dragged it: the plugin may want a different size.
        let wanted = window.size();
        let GuiSize { width, height } = gui
            .adjust_size(
                &handle,
                GuiSize {
                    width: wanted.0,
                    height: wanted.1,
                },
            )
            .unwrap_or(GuiSize {
                width: wanted.0,
                height: wanted.1,
            });
        if (width, height) != wanted {
            window.set_size(width, height, true);
        }
        live.size = window.size();
        let _ = gui.set_size(
            &handle,
            GuiSize {
                width: live.size.0,
                height: live.size.1,
            },
        );
    }
}

fn destroy_editor(live: &mut Live) {
    if !live.gui {
        return;
    }
    live.gui = false;
    live.flags.editor_open.store(false, Ordering::Release);
    live.flags.embedded.store(false, Ordering::Release);
    live.flags.resize.store(0, Ordering::Release);
    let handle = live.instance.plugin_handle();
    if let Some(gui) = handle.get_extension::<PluginGui>() {
        gui.destroy(&handle);
    }
    // The plugin's view goes before the window it was in.
    if let Some(mut window) = live.window.take() {
        window.hide();
    }
}

fn handle(command: Command, live: &mut HashMap<u64, Live>, next_key: &mut u64) {
    match command {
        Command::Load {
            path,
            plugin_id,
            state,
            sample_rate,
            max_frames,
            reply,
        } => {
            let result = load(&path, &plugin_id, state.as_deref(), sample_rate, max_frames);
            match result {
                Ok((instance_live, processor)) => {
                    let key = *next_key;
                    *next_key += 1;
                    let flags = Arc::clone(&instance_live.flags);
                    let loaded = Loaded {
                        key,
                        processor: Some(processor),
                        flags,
                    };
                    // Registered first: if the caller is gone, dropping the
                    // `Loaded` sends a Close for this key.
                    live.insert(key, instance_live);
                    let _ = reply.send(Ok(loaded));
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
            }
        }
        Command::Close { key, processor } => {
            if let Some(mut l) = live.remove(&key) {
                destroy_editor(&mut l);
                l.instance.deactivate(processor);
            }
        }
        Command::Capture { key, reply } => {
            let result = match live.get_mut(&key) {
                Some(l) => capture_live(l),
                None => Err(HostError("the plugin is no longer running".into())),
            };
            let _ = reply.send(result);
        }
        Command::CaptureFresh {
            path,
            plugin_id,
            state,
            reply,
        } => {
            let result = (|| {
                let entry = load_entry(&path)?;
                let info = host_info()?;
                let mut instance = new_instance(&entry, &plugin_id, &info, Arc::default())?;
                if let Some(state) = &state {
                    load_state(&mut instance, state)?;
                }
                save_state_of(&mut instance)
            })();
            let _ = reply.send(result);
        }
        Command::OpenEditor { key, reply } => {
            let result = match live.get_mut(&key) {
                Some(l) => open_editor_of(l),
                None => Err(HostError("the plugin is no longer running".into())),
            };
            let _ = reply.send(result);
        }
        Command::CloseEditor { key } => {
            if let Some(l) = live.get_mut(&key) {
                if l.gui {
                    let handle = l.instance.plugin_handle();
                    if let Some(gui) = handle.get_extension::<PluginGui>() {
                        let _ = gui.hide(&handle);
                    }
                }
                destroy_editor(l);
            }
        }
    }
}

fn load(
    path: &Path,
    plugin_id: &std::ffi::CStr,
    state: Option<&[u8]>,
    sample_rate: f32,
    max_frames: usize,
) -> Result<(Live, StoppedPluginAudioProcessor<Host>), HostError> {
    let entry = load_entry(path)?;
    let info = host_info()?;
    let flags = Arc::new(Flags::default());
    let mut instance = new_instance(&entry, plugin_id, &info, Arc::clone(&flags))?;
    if let Some(state) = state {
        load_state(&mut instance, state)?;
    }
    let processor = instance
        .activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate: f64::from(sample_rate),
                min_frames_count: 1,
                max_frames_count: max_frames as u32,
            },
        )
        .map_err(|e| HostError(e.to_string()))?;
    Ok((
        Live {
            instance,
            _entry: entry,
            flags,
            gui: false,
            window: None,
            size: (0, 0),
        },
        processor,
    ))
}

/// The state the plugin saves now, and every parameter's value now, so edits
/// made in its editor can be kept in the project.
fn capture_live(live: &mut Live) -> Result<Captured, HostError> {
    let state = save_state_of(&mut live.instance)?;
    let mut handle = live.instance.plugin_handle();
    let mut params = Vec::new();
    if let Some(ext) = handle.get_extension::<PluginParams>() {
        for (id, desc) in visible_params(&mut handle) {
            if let Some(value) = ext.get_value(&handle, id) {
                params.push((desc.id, value));
            }
        }
    }
    Ok(Captured { state, params })
}

/// The windowing APIs to try, best first, for a floating window.
fn apis() -> &'static [GuiApiType<'static>] {
    if cfg!(windows) {
        &[GuiApiType::WIN32]
    } else if cfg!(target_os = "macos") {
        &[GuiApiType::COCOA]
    } else {
        &[GuiApiType::X11, GuiApiType::WAYLAND]
    }
}

fn open_editor_of(live: &mut Live) -> Result<(), HostError> {
    if live.gui {
        // Already up: bring it back if it was hidden.
        let handle = live.instance.plugin_handle();
        let gui = handle
            .get_extension::<PluginGui>()
            .ok_or_else(|| HostError("this plugin has no editor".into()))?;
        gui.show(&handle)
            .map_err(|e| HostError(format!("can't show the editor: {e}")))?;
        if let Some(window) = live.window.as_mut() {
            window.show();
        }
        return Ok(());
    }
    // Embedded in a window of ours if the application makes those and the
    // plugin can; otherwise the plugin's own floating window.
    let embedded_error = if window::available() {
        match open_embedded(live) {
            Ok(()) => return Ok(()),
            Err(error) => Some(error),
        }
    } else {
        None
    };
    let handle = live.instance.plugin_handle();
    let Some(gui) = handle.get_extension::<PluginGui>() else {
        return Err(HostError("this plugin has no editor".into()));
    };
    let configuration = apis()
        .iter()
        .map(|api| GuiConfiguration {
            api_type: *api,
            is_floating: true,
        })
        .find(|c| gui.is_api_supported(&handle, *c))
        .ok_or_else(|| {
            embedded_error.unwrap_or_else(|| {
                HostError("this plugin has no editor that floats on this system".into())
            })
        })?;
    gui.create(&handle, configuration)
        .map_err(|e| HostError(format!("can't create the editor: {e}")))?;
    live.gui = true;
    live.flags.editor_closed.store(false, Ordering::Release);
    if let Err(error) = gui.show(&handle) {
        gui.destroy(&handle);
        live.gui = false;
        return Err(HostError(format!("can't show the editor: {error}")));
    }
    live.flags.editor_open.store(true, Ordering::Release);
    Ok(())
}

/// What a plugin is given when it has no size of its own to offer.
const DEFAULT_SIZE: (u32, u32) = (800, 600);

/// Opens the editor inside a window the host makes.
fn open_embedded(live: &mut Live) -> Result<(), HostError> {
    let Live {
        instance,
        flags,
        gui: gui_exists,
        window: live_window,
        size: live_size,
        ..
    } = live;
    let handle = &instance.plugin_handle();
    let gui = &handle
        .get_extension::<PluginGui>()
        .ok_or_else(|| HostError("this plugin has no editor".into()))?;
    let configuration = apis()
        .iter()
        .map(|api| GuiConfiguration {
            api_type: *api,
            is_floating: false,
        })
        .find(|c| gui.is_api_supported(handle, *c))
        .ok_or_else(|| HostError("this plugin has no editor that embeds on this system".into()))?;
    let mut window = window::make("Plugin editor")
        .ok_or_else(|| HostError("can't make a window for the editor".into()))?;
    gui.create(handle, configuration)
        .map_err(|e| HostError(format!("can't create the editor: {e}")))?;
    let result = (|| {
        let size = gui
            .get_size(handle)
            .map_or(DEFAULT_SIZE, |s| (s.width, s.height));
        window.set_size(size.0, size.1, gui.can_resize(handle));
        let parent = match window.parent() {
            // SAFETY: the window made the handle.
            ParentWindow::Cocoa(view) => unsafe { Window::from_cocoa_nsview(view) },
            ParentWindow::Win32(hwnd) => unsafe { Window::from_win32_hwnd(hwnd) },
            ParentWindow::X11(id) => Window::from_x11_handle(id),
        };
        // From here the plugin may ask for another size.
        flags.embedded.store(true, Ordering::Release);
        // SAFETY: the window stays alive in `live` until the editor is
        // destroyed, which is when the plugin is told it is gone.
        unsafe { gui.set_parent(handle, parent) }
            .map_err(|e| HostError(format!("can't embed the editor: {e}")))?;
        gui.show(handle)
            .map_err(|e| HostError(format!("can't show the editor: {e}")))?;
        *live_size = window.size();
        Ok(())
    })();
    if let Err(error) = result {
        flags.embedded.store(false, Ordering::Release);
        flags.resize.store(0, Ordering::Release);
        gui.destroy(handle);
        return Err(error);
    }
    window.show();
    *live_window = Some(window);
    *gui_exists = true;
    flags.embedded.store(true, Ordering::Release);
    flags.editor_closed.store(false, Ordering::Release);
    flags.editor_open.store(true, Ordering::Release);
    Ok(())
}
