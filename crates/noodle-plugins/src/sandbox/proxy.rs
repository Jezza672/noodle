//! The app's side of a sandboxed plugin: a [`Node`] that hands each block to a
//! plugin host process and takes the result back, and the thread that keeps
//! that process alive.
//!
//! ## One block
//!
//! The audio thread copies the block's inputs, events and transport into the
//! shared file (`shm.rs`), bumps `req_seq`, wakes the host if it is asleep
//! (one byte down its stdin), and waits for `done_seq` to catch up, spinning
//! and then yielding. In live use it gives up when a block's time is up and
//! plays silence for the block, as a plugin that misses its deadline would
//! have made the device do; offline renders ("patient" nodes) wait as long
//! as the host is alive. While the host is still on an earlier block, later
//! blocks are skipped without touching the file. Nothing on this path
//! allocates, locks or waits on a lock.
//!
//! ## A host that dies or hangs
//!
//! A monitor thread per node watches the process. When it exits, or a block
//! has been pending for too long, it kills it, and starts another one built
//! from the node's original settings (so the plugin comes back as saved in
//! the project, not as it was a moment ago), up to [`MAX_RESTARTS`] times.
//! Meanwhile the node plays silence. The exclusion between the audio thread
//! and the monitor, which replaces the file mapping, is `state` plus
//! `in_audio`: the monitor flips `state` away from [`RUNNING`], waits for
//! `in_audio` to drop to zero, and only then touches the link.

use std::cell::UnsafeCell;
use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use noodle_core::NodeId;
use noodle_engine::{Config, Context, Io, Node};

use super::proto::{Channel, FromHost, Hello, ToHost, decode};
use super::shm::{BlockParams, InputMeta, MAX_EVENTS_IN, MAX_EVENTS_OUT, Mapping, Spec, WireEvent};
use super::{SandboxCommand, SandboxStatus};
use crate::descriptor::Descriptor;
use crate::editor::Captured;
use crate::host::{HostError, LOAD_TIMEOUT};

/// How many times a host that died is started again before the node gives up
/// and stays silent.
pub(crate) const MAX_RESTARTS: u32 = 3;

/// How long a block may stay unfinished before the host is taken to be hung,
/// live and in an offline render (where a block is a lot of audio).
const HUNG_LIVE: Duration = Duration::from_secs(2);
const HUNG_PATIENT: Duration = Duration::from_secs(60);

/// How long an editor request (open, capture) may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

const TICK: Duration = Duration::from_millis(20);

const STARTING: u8 = 0;
const RUNNING: u8 = 1;
const DOWN: u8 = 2;
const FAILED: u8 = 3;

/// What the node was built from, kept so the host can be started again.
pub(crate) struct Params {
    pub command: Arc<SandboxCommand>,
    pub plugin_path: std::path::PathBuf,
    pub descriptor: Descriptor,
    pub config: Config,
    pub node: NodeId,
    pub sample_rate: f32,
    pub seed: u64,
    pub position: u64,
    pub spec: Spec,
    /// Offline: never drop a block for being late.
    pub patient: bool,
}

/// The shared file and the way to wake the host.
struct Link {
    map: Mapping,
    wake: ChildStdin,
}

pub(crate) struct Shared {
    state: AtomicU8,
    /// 1 while the audio thread may be using `link`.
    in_audio: AtomicU32,
    shutdown: AtomicBool,
    /// SAFETY: see the module docs. Only the monitor replaces it, and only
    /// while `state` is not [`RUNNING`] and `in_audio` is zero.
    link: UnsafeCell<Option<Link>>,
    control: Mutex<Option<Channel>>,
    restarts: AtomicU32,
    pid: AtomicU32,
    dropped: AtomicU64,
    editor_open: AtomicBool,
    last_error: Mutex<Option<String>>,
    monitor: Mutex<Option<Thread>>,
}

// SAFETY: `link` is guarded as described above; everything else is atomic or
// behind a mutex.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

impl Shared {
    fn set_error(&self, error: String) {
        *self
            .last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(error);
    }

    /// Asks the host for something and waits for its one-line answer.
    fn request(&self, message: ToHost) -> Result<FromHost, HostError> {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        let channel = control
            .as_mut()
            .ok_or_else(|| HostError("the plugin's host process isn't running".into()))?;
        channel.set_timeout(Some(REQUEST_TIMEOUT));
        channel
            .send(&message)
            .map_err(|e| HostError(format!("the plugin's host process is gone: {e}")))?;
        match channel.receive::<FromHost>() {
            Ok(Some(FromHost::Failed { error })) => Err(HostError(error)),
            Ok(Some(reply)) => Ok(reply),
            Ok(None) => Err(HostError("the plugin's host process exited".into())),
            Err(e) => Err(HostError(format!(
                "the plugin's host process didn't answer: {e}"
            ))),
        }
    }

    pub fn status(&self) -> SandboxStatus {
        let state = self.state.load(Ordering::Acquire);
        SandboxStatus {
            running: state == RUNNING,
            failed: state == FAILED,
            restarts: self.restarts.load(Ordering::Relaxed),
            pid: Some(self.pid.load(Ordering::Relaxed)).filter(|&p| p != 0),
            dropped_blocks: self.dropped.load(Ordering::Relaxed),
            last_error: self
                .last_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }
}

/// The handle the editor functions use for a sandboxed plugin.
pub(crate) struct Remote(pub Arc<Shared>);

impl Remote {
    pub fn open_editor(&self) -> Result<(), HostError> {
        // The host has no window factory, so on macOS and Windows its
        // editors would be floating windows of a process with no event
        // loop. Linux plugins draw their own windows.
        if !cfg!(target_os = "linux") {
            return Err(HostError(
                "editors of plugins run in a separate process aren't supported on this \
                 system yet; turn that option off to use the editor"
                    .into(),
            ));
        }
        self.0.request(ToHost::OpenEditor).map(|_| ())
    }

    pub fn close_editor(&self) {
        let _ = self.0.request(ToHost::CloseEditor);
    }

    pub fn is_open(&self) -> bool {
        self.0.editor_open.load(Ordering::Acquire)
    }

    pub fn capture(&self) -> Result<Captured, HostError> {
        match self.0.request(ToHost::Capture)? {
            FromHost::Captured { state, params } => Ok(Captured {
                state: decode(&state)
                    .ok_or_else(|| HostError("the host sent a damaged state".into()))?,
                params,
            }),
            other => Err(HostError(format!("unexpected reply {other:?}"))),
        }
    }
}

/// The node's `Node` implementation.
pub(crate) struct SandboxedNode {
    shared: Arc<Shared>,
    spec: Spec,
    latency: u32,
    patient: bool,
    sample_rate: f32,
    /// The editor registry entry to remove when this goes.
    key: u64,
}

impl SandboxedNode {
    /// Starts the host and waits until the plugin has loaded in it. Runs off
    /// the audio thread.
    pub fn start(params: Params) -> Result<(Self, Arc<Shared>), String> {
        let params = Arc::new(params);
        let spawned = spawn_host(&params)?;
        let shared = Arc::new(Shared {
            state: AtomicU8::new(STARTING),
            in_audio: AtomicU32::new(0),
            shutdown: AtomicBool::new(false),
            link: UnsafeCell::new(Some(spawned.link)),
            control: Mutex::new(Some(spawned.channel)),
            restarts: AtomicU32::new(0),
            pid: AtomicU32::new(spawned.child.id()),
            dropped: AtomicU64::new(0),
            editor_open: AtomicBool::new(false),
            last_error: Mutex::new(None),
            monitor: Mutex::new(None),
        });
        shared.state.store(RUNNING, Ordering::SeqCst);
        let monitor = Monitor {
            params: Arc::clone(&params),
            shared: Arc::clone(&shared),
            child: spawned.child,
        };
        let handle = thread::Builder::new()
            .name("noodle-plugin-monitor".into())
            .spawn(move || monitor.run())
            .map_err(|e| e.to_string())?;
        *shared
            .monitor
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(handle.thread().clone());
        let node = Self {
            shared: Arc::clone(&shared),
            spec: params.spec.clone(),
            latency: spawned.latency,
            patient: params.patient,
            sample_rate: params.sample_rate,
            key: 0,
        };
        Ok((node, shared))
    }

    pub fn with_key(mut self, key: u64) -> Self {
        self.key = key;
        self
    }

    /// Runs `body` with the link if the host is up, on the audio thread.
    fn with_link<R>(&self, body: impl FnOnce(&Link) -> R) -> Option<R> {
        let shared = &*self.shared;
        if shared.state.load(Ordering::SeqCst) != RUNNING {
            return None;
        }
        shared.in_audio.store(1, Ordering::SeqCst);
        // The monitor takes `state` away before it waits for `in_audio`, so
        // after this second look the link is ours until we let go.
        let result = if shared.state.load(Ordering::SeqCst) == RUNNING {
            // SAFETY: see `Shared::link`.
            unsafe { (*shared.link.get()).as_ref() }.map(body)
        } else {
            None
        };
        shared.in_audio.store(0, Ordering::SeqCst);
        result
    }

    fn silence(io: &mut Io<'_, '_>) {
        for output in io.outputs.iter_mut() {
            let shape = output.shape();
            for voice in 0..shape.voices {
                for channel in 0..shape.channels {
                    output.silence(voice, channel);
                }
            }
        }
    }

    fn exchange(&self, link: &Link, ctx: &Context, io: &mut Io<'_, '_>) -> bool {
        let spec = &self.spec;
        let map = &link.map;
        let header = map.header();
        let request = header.req_seq.load(Ordering::Relaxed);
        if header.done_seq.load(Ordering::Acquire) != request {
            // Still on an earlier block: leave its regions alone.
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        debug_assert!(
            ctx.frames <= spec.max_frames,
            "a block longer than max_frames"
        );
        let frames = ctx.frames.min(spec.max_frames);

        // SAFETY (this whole block): the host is idle until `req_seq` moves,
        // so these regions are ours.
        unsafe {
            map.write_block(BlockParams::new(frames, &ctx.transport));
            for (index, input) in io.inputs.iter().enumerate().take(spec.inputs.len()) {
                let shape = spec.input_shape(index);
                let silent = map.input_silent(index);
                let mut lane_index = 0;
                for voice in 0..shape.voices {
                    for channel in 0..shape.channels {
                        silent[lane_index] = u8::from(input.is_silent(voice, channel));
                        lane_index += 1;
                    }
                }
                match input.constant() {
                    Some(value) => map.write_input_meta(index, InputMeta { constant: 1, value }),
                    None => {
                        map.write_input_meta(
                            index,
                            InputMeta {
                                constant: 0,
                                value: 0.0,
                            },
                        );
                        let floats = map.input_floats(index);
                        let mut lane_index = 0;
                        for voice in 0..shape.voices {
                            for channel in 0..shape.channels {
                                floats[lane_index * frames..(lane_index + 1) * frames]
                                    .copy_from_slice(&input.lane(voice, channel)[..frames]);
                                lane_index += 1;
                            }
                        }
                    }
                }
            }
            for port in 0..spec.event_inputs {
                let events = io.event_inputs.get(port).copied().unwrap_or(&[]);
                let wire = map.events_in(port);
                let count = events.len().min(MAX_EVENTS_IN);
                for (slot, event) in wire.iter_mut().zip(events) {
                    *slot = WireEvent::new(event);
                }
                *map.event_in_count(port) = count as u32;
            }
        }

        header.req_seq.store(request + 1, Ordering::SeqCst);
        if header.sleeping.load(Ordering::SeqCst) != 0 {
            // One byte down the pipe wakes the host. A full or closed pipe
            // only means it is gone or busy, which the wait below handles.
            let _ = (&link.wake).write(&[1]);
        }

        let started = Instant::now();
        // Half the block's time: a late node gives up while the rest of the
        // graph can still finish, instead of using up the device's deadline
        // (and again for each sandboxed node in series).
        let limit = (!self.patient).then(|| {
            Duration::from_secs_f64(frames as f64 / f64::from(self.sample_rate.max(1.0)) * 0.5)
                .max(Duration::from_micros(500))
        });
        let mut spins = 0u32;
        loop {
            if header.done_seq.load(Ordering::Acquire) == request + 1 {
                break;
            }
            if self.shared.state.load(Ordering::SeqCst) != RUNNING {
                return false;
            }
            spins = spins.wrapping_add(1);
            if spins < 2000 {
                std::hint::spin_loop();
            } else {
                thread::yield_now();
            }
            if let Some(limit) = limit
                && spins.is_multiple_of(64)
                && started.elapsed() > limit
            {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                return false;
            }
        }

        // SAFETY: the host has finished the block and waits for the next.
        unsafe {
            for (index, output) in io.outputs.iter_mut().enumerate().take(spec.outputs.len()) {
                let shape = spec.output_shape(index);
                let floats = map.output_floats(index);
                let silent = map.output_silent(index);
                let mut lane_index = 0;
                for voice in 0..shape.voices {
                    for channel in 0..shape.channels {
                        output.lane_mut(voice, channel)[..frames].copy_from_slice(
                            &floats[lane_index * frames..(lane_index + 1) * frames],
                        );
                        if silent[lane_index] != 0 {
                            output.set_silent(voice, channel);
                        }
                        lane_index += 1;
                    }
                }
            }
            for (port, out) in io
                .event_outputs
                .iter_mut()
                .enumerate()
                .take(spec.event_outputs)
            {
                let count = (*map.event_out_count(port) as usize).min(MAX_EVENTS_OUT);
                for wire in &map.events_out(port)[..count] {
                    if out.push(wire.event()).is_err() {
                        break;
                    }
                }
            }
        }
        true
    }
}

impl Node for SandboxedNode {
    fn process(&mut self, ctx: &Context, mut io: Io<'_, '_>) {
        let this = &*self;
        let mut ok = this
            .with_link(|link| this.exchange(link, ctx, &mut io))
            .unwrap_or(false);
        // An offline render waits out a restart rather than render the rest
        // of the file as silence (it isn't against the clock).
        while !ok
            && this.patient
            && this.shared.state.load(Ordering::SeqCst) == DOWN
            && !this.shared.shutdown.load(Ordering::SeqCst)
        {
            thread::sleep(Duration::from_millis(2));
            ok = this
                .with_link(|link| this.exchange(link, ctx, &mut io))
                .unwrap_or(false);
        }
        if !ok {
            Self::silence(&mut io);
        }
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn reset(&mut self) {
        self.with_link(|link| {
            link.map.header().reset_seq.fetch_add(1, Ordering::AcqRel);
        });
    }
}

impl Drop for SandboxedNode {
    fn drop(&mut self) {
        super::unregister(self.key);
        self.shared.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self
            .shared
            .monitor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            thread.unpark();
        }
    }
}

struct Spawned {
    child: Child,
    link: Link,
    channel: Channel,
    latency: u32,
}

/// Starts a host process and waits until its plugin is loaded.
fn spawn_host(params: &Params) -> Result<Spawned, String> {
    let map = Mapping::create(&params.spec).map_err(|e| format!("shared memory: {e}"))?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("socket: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let token = format!(
        "{:016x}",
        std::hash::RandomState::new().build_hasher().finish()
    );

    let mut command = Command::new(&params.command.program);
    command
        .args(&params.command.args)
        .arg(port.to_string())
        .arg(&token)
        .stdin(Stdio::piped())
        .stdout(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| format!("can't start the plugin host: {e}"))?;
    let wake = child.stdin.take().ok_or("no pipe to the host")?;

    let result = handshake(params, &map, &listener, &mut child, &token);
    match result {
        Ok((channel, latency)) => {
            map.unlink();
            Ok(Spawned {
                child,
                link: Link { map, wake },
                channel,
                latency,
            })
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

fn handshake(
    params: &Params,
    map: &Mapping,
    listener: &TcpListener,
    child: &mut Child,
    token: &str,
) -> Result<(Channel, u32), String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let stream: TcpStream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(format!("the plugin host exited at start ({status})"));
                }
                if started.elapsed() > LOAD_TIMEOUT {
                    return Err("the plugin host didn't start in time".into());
                }
                thread::sleep(Duration::from_millis(2));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    stream.set_nonblocking(false).map_err(|e| e.to_string())?;
    let mut channel = Channel::new(stream).map_err(|e| e.to_string())?;
    channel.set_timeout(Some(LOAD_TIMEOUT));
    match channel.receive_line() {
        Ok(Some(line)) if line == token => {}
        _ => return Err("the plugin host didn't identify itself".into()),
    }
    channel
        .send(&ToHost::Hello(Box::new(Hello {
            shm: map.path().to_owned(),
            spec: params.spec.clone(),
            plugin_path: params.plugin_path.clone(),
            descriptor: params.descriptor.clone(),
            config: params.config.clone(),
            node: params.node.0,
            sample_rate: params.sample_rate,
            seed: params.seed,
            position: params.position,
        })))
        .map_err(|e| e.to_string())?;
    match channel.receive::<FromHost>() {
        Ok(Some(FromHost::Ready { latency })) => {
            channel.set_timeout(Some(REQUEST_TIMEOUT));
            Ok((channel, latency))
        }
        Ok(Some(FromHost::Failed { error })) => Err(error),
        Ok(other) => Err(format!("the plugin host said {other:?}")),
        Err(e) => Err(format!("the plugin host didn't load the plugin: {e}")),
    }
}

/// Keeps one host alive: notices when it dies or hangs and starts another.
struct Monitor {
    params: Arc<Params>,
    shared: Arc<Shared>,
    child: Child,
}

impl Monitor {
    fn run(mut self) {
        let mut pending: Option<(u64, Instant)> = None;
        loop {
            thread::park_timeout(TICK);
            if self.shared.shutdown.load(Ordering::SeqCst) {
                self.teardown();
                return;
            }
            match self.shared.state.load(Ordering::SeqCst) {
                RUNNING => {
                    let reason = self.check(&mut pending);
                    if let Some(reason) = reason {
                        self.go_down(reason);
                        pending = None;
                    }
                }
                DOWN => self.restart(),
                _ => {}
            }
        }
    }

    /// Why the host should be replaced, if it should.
    fn check(&mut self, pending: &mut Option<(u64, Instant)>) -> Option<String> {
        // SAFETY: the state is RUNNING and only this thread replaces the
        // link, so it is there; the header is atomics.
        let link = unsafe { (*self.shared.link.get()).as_ref() }?;
        let header = link.map.header();
        self.shared.editor_open.store(
            header.editor_open.load(Ordering::Acquire) != 0,
            Ordering::Release,
        );
        match self.child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => return Some(format!("the plugin host stopped ({status})")),
            Err(e) => return Some(format!("can't watch the plugin host: {e}")),
        }
        let request = header.req_seq.load(Ordering::Acquire);
        if header.done_seq.load(Ordering::Acquire) == request {
            *pending = None;
            return None;
        }
        let limit = if self.params.patient {
            HUNG_PATIENT
        } else {
            HUNG_LIVE
        };
        match pending {
            Some((seen, since)) if *seen == request => {
                (since.elapsed() > limit).then(|| "the plugin stopped answering".to_owned())
            }
            _ => {
                *pending = Some((request, Instant::now()));
                None
            }
        }
    }

    fn go_down(&mut self, reason: String) {
        let shared = &self.shared;
        shared.state.store(DOWN, Ordering::SeqCst);
        shared.set_error(reason);
        shared.restarts.fetch_add(1, Ordering::Relaxed);
        *shared
            .control
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        shared.editor_open.store(false, Ordering::Release);
    }

    fn restart(&mut self) {
        // The audio thread leaves its wait as soon as it sees the state, but
        // may be descheduled: the link is not ours to replace until it has
        // left, however long that takes.
        if self.shared.in_audio.load(Ordering::SeqCst) != 0 {
            return;
        }
        let attempts = self.shared.restarts.load(Ordering::Relaxed);
        if attempts > MAX_RESTARTS {
            self.shared.state.store(FAILED, Ordering::SeqCst);
            self.shared.restarts.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // Back off a little more each time, but stay ready to leave.
        let wait = Duration::from_millis(250 * u64::from(attempts));
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if self.shared.shutdown.load(Ordering::SeqCst) {
                return;
            }
            thread::park_timeout(TICK);
        }
        match spawn_host(&self.params) {
            Ok(spawned) => {
                // SAFETY: the state is DOWN and `in_audio` is zero (see
                // `go_down`), so nothing else is using the link.
                unsafe { *self.shared.link.get() = Some(spawned.link) };
                *self
                    .shared
                    .control
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(spawned.channel);
                self.shared.pid.store(spawned.child.id(), Ordering::Relaxed);
                self.child = spawned.child;
                self.shared.state.store(RUNNING, Ordering::SeqCst);
            }
            Err(error) => {
                self.shared.set_error(error);
                self.shared.restarts.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn teardown(&mut self) {
        // The node is gone, so nothing runs blocks any more.
        if let Ok(mut control) = self.shared.control.try_lock()
            && let Some(channel) = control.as_mut()
        {
            channel.set_timeout(Some(Duration::from_millis(200)));
            let _ = channel.send(&ToHost::Quit);
        }
        // SAFETY: as above; the file is ours to flag.
        if let Some(link) = unsafe { (*self.shared.link.get()).as_ref() } {
            link.map.header().shutdown.store(1, Ordering::SeqCst);
            let _ = (&link.wake).write(&[1]);
        }
        let waited = Instant::now();
        while waited.elapsed() < Duration::from_millis(300) {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        // SAFETY: nothing else can reach the link now.
        unsafe { *self.shared.link.get() = None };
    }
}
