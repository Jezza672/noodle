//! The plugin host process: runs one plugin node for the app that started it.
//!
//! It is the app's own program started with [`HOST_ARG`] (or the
//! `noodle-plugin-scan` program), so a plugin that crashes, hangs or leaks
//! takes down only this process. It builds exactly the node the app would
//! have built in-process (the same code, `PluginNodeType::instantiate_local`)
//! and runs it block by block on the signals in the shared file.
//!
//! Two threads: the main one runs blocks, and `control` serves the
//! main-thread requests (editor, state capture) that arrive on the socket.

use std::io::Read;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use noodle_core::NodeId;
use noodle_engine::{
    Context, Event, EventsOut, Instance, Io, Node, Priority, Setup, SignalIn, SignalOut,
    raise_current_thread,
};

use super::proto::{Channel, FromHost, Hello, ToHost, encode};
use super::shm::{MAX_EVENTS_IN, MAX_EVENTS_OUT, Mapping, WireEvent};
use crate::editor;
use crate::node::PluginNodeType;

/// The argument that makes a program run as a plugin host: it is followed by
/// the port to connect to and a token proving who started it.
pub const HOST_ARG: &str = "--noodle-plugin-host";

/// If this program was started as a plugin host, runs it and never returns.
pub fn run_host_if_requested() {
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|arg| arg == HOST_ARG) {
        let port = args.next().and_then(|a| a.to_str()?.parse().ok());
        let token = args.next().and_then(|a| a.into_string().ok());
        let (Some(port), Some(token)) = (port, token) else {
            eprintln!("usage: {HOST_ARG} <port> <token>");
            std::process::exit(2);
        };
        host_main(port, &token);
    }
}

/// How long the main loop spins for the next block before it blocks. A
/// block's work is posted a few microseconds after the last one finished
/// only on very short blocks; otherwise this is wasted, so keep it small.
const SPIN: Duration = Duration::from_micros(50);

/// Runs the host; exits the process when done.
pub fn host_main(port: u16, token: &str) -> ! {
    let code = match run(port, token) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("noodle plugin host: {error}");
            1
        }
    };
    std::process::exit(code)
}

fn run(port: u16, token: &str) -> Result<(), String> {
    let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    let mut channel = Channel::new(stream).map_err(|e| e.to_string())?;
    channel.send_line(token).map_err(|e| e.to_string())?;
    let hello = match channel.receive::<ToHost>() {
        Ok(Some(ToHost::Hello(hello))) => hello,
        other => return Err(format!("expected a hello, got {other:?}")),
    };
    let (mapping, mut node, latency) = match start(&hello) {
        Ok(started) => started,
        Err(error) => {
            let _ = channel.send(&FromHost::Failed {
                error: error.clone(),
            });
            return Err(error);
        }
    };
    channel
        .send(&FromHost::Ready { latency })
        .map_err(|e| e.to_string())?;

    let node_id = NodeId(hello.node);
    let control_mapping = Arc::clone(&mapping);
    std::thread::Builder::new()
        .name("noodle-host-control".into())
        .spawn(move || control(channel, node_id, &control_mapping))
        .map_err(|e| e.to_string())?;

    let period = Duration::from_secs_f64(
        hello.spec.max_frames as f64 / f64::from(hello.sample_rate.max(1.0)),
    );
    if raise_current_thread(period) == Priority::Normal {
        // Common in a sandbox; the plugin still runs.
    }
    audio_loop(&mapping, node.as_mut(), hello.sample_rate);
    Ok(())
}

/// The mapping, the node and its latency.
type Started = (Arc<Mapping>, Box<dyn Node>, u32);

fn start(hello: &Hello) -> Result<Started, String> {
    let mapping = Mapping::open(&hello.shm, &hello.spec).map_err(|e| e.to_string())?;
    let node_type = PluginNodeType::new(hello.plugin_path.clone(), hello.descriptor.clone());
    let input_shapes: Vec<_> = (0..hello.spec.inputs.len())
        .map(|i| hello.spec.input_shape(i))
        .collect();
    let output_shapes: Vec<_> = (0..hello.spec.outputs.len())
        .map(|i| hello.spec.output_shape(i))
        .collect();
    let setup = Setup {
        node: NodeId(hello.node),
        config: &hello.config,
        sample_rate: hello.sample_rate,
        max_frames: hello.spec.max_frames,
        input_shapes: &input_shapes,
        output_shapes: &output_shapes,
        seed: hello.seed,
        position: hello.position,
    };
    match node_type
        .instantiate_local(&setup)
        .map_err(|e| e.to_string())?
    {
        Instance::Realtime(node) => {
            let latency = node.latency();
            Ok((Arc::new(mapping), node, latency))
        }
        Instance::Offline(_) => Err("a plugin node can't be offline".into()),
    }
}

/// Serves the requests that arrive after the handshake, and keeps the
/// header's `editor_open` flag current. Exits the process when the app goes.
fn control(mut channel: Channel, node: NodeId, mapping: &Mapping) {
    channel.set_timeout(Some(Duration::from_millis(100)));
    loop {
        let message = channel.receive::<ToHost>();
        mapping
            .header()
            .editor_open
            .store(u32::from(editor::editor_is_open(node)), Ordering::Release);
        match message {
            Ok(Some(ToHost::OpenEditor)) => {
                let reply = match editor::open_editor(node) {
                    Ok(_) => FromHost::Done,
                    Err(error) => FromHost::Failed { error: error.0 },
                };
                let _ = channel.send(&reply);
            }
            Ok(Some(ToHost::CloseEditor)) => {
                editor::close_editor(node);
                let _ = channel.send(&FromHost::Done);
            }
            Ok(Some(ToHost::Capture)) => {
                let reply = match editor::capture(node) {
                    Ok(captured) => FromHost::Captured {
                        state: encode(&captured.state),
                        params: captured.params,
                    },
                    Err(error) => FromHost::Failed { error: error.0 },
                };
                let _ = channel.send(&reply);
            }
            Ok(Some(ToHost::Quit)) | Ok(None) => std::process::exit(0),
            Ok(Some(ToHost::Hello(_))) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => std::process::exit(0),
        }
        mapping
            .header()
            .editor_open
            .store(u32::from(editor::editor_is_open(node)), Ordering::Release);
    }
}

/// Everything a block needs, made once so the loop never allocates.
struct Buffers {
    inputs: Vec<SignalIn<'static>>,
    outputs: Vec<SignalOut<'static>>,
    event_in: Vec<Vec<Event>>,
    event_in_refs: Vec<&'static [Event]>,
    event_out: Vec<Vec<Event>>,
    event_out_handles: Vec<EventsOut<'static>>,
    /// Backing for inputs that are one value for the block.
    constants: Vec<Vec<f32>>,
    /// What each constant buffer holds now: the value's bits and the frames.
    filled: Vec<Option<(u32, usize)>>,
    silent_in: Vec<Vec<bool>>,
    silent_out: Vec<Vec<bool>>,
}

impl Buffers {
    fn new(mapping: &Mapping) -> Self {
        let spec = &mapping.layout.spec;
        let lanes_in: Vec<usize> = (0..spec.inputs.len())
            .map(|i| spec.input_shape(i).lanes())
            .collect();
        let lanes_out: Vec<usize> = (0..spec.outputs.len())
            .map(|i| spec.output_shape(i).lanes())
            .collect();
        Self {
            inputs: Vec::with_capacity(spec.inputs.len()),
            outputs: Vec::with_capacity(spec.outputs.len()),
            event_in: (0..spec.event_inputs)
                .map(|_| Vec::with_capacity(MAX_EVENTS_IN))
                .collect(),
            event_in_refs: Vec::with_capacity(spec.event_inputs),
            event_out: (0..spec.event_outputs)
                .map(|_| Vec::with_capacity(MAX_EVENTS_OUT))
                .collect(),
            event_out_handles: Vec::with_capacity(spec.event_outputs),
            constants: lanes_in
                .iter()
                .map(|lanes| vec![0.0; lanes * spec.max_frames])
                .collect(),
            filled: vec![None; spec.inputs.len()],
            silent_in: lanes_in.iter().map(|&l| vec![false; l]).collect(),
            silent_out: lanes_out.iter().map(|&l| vec![false; l]).collect(),
        }
    }
}

fn audio_loop(mapping: &Mapping, node: &mut dyn Node, sample_rate: f32) {
    let header = mapping.header();
    let spec = mapping.layout.spec.clone();
    let mut buffers = Buffers::new(mapping);
    let mut stdin = std::io::stdin().lock();
    let mut seen_reset = header.reset_seq.load(Ordering::Acquire);
    let mut wake = [0u8; 64];
    loop {
        // Wait for the next block: spin a moment, then sleep until the node
        // sends a byte down our stdin (or closes it, because it is gone).
        let done = header.done_seq.load(Ordering::Relaxed);
        let waiting_since = Instant::now();
        let request = loop {
            if header.shutdown.load(Ordering::Acquire) != 0 {
                return;
            }
            let request = header.req_seq.load(Ordering::Acquire);
            if request != done {
                break request;
            }
            if waiting_since.elapsed() < SPIN {
                std::hint::spin_loop();
                continue;
            }
            header.sleeping.store(1, Ordering::SeqCst);
            if header.req_seq.load(Ordering::SeqCst) == done
                && header.shutdown.load(Ordering::SeqCst) == 0
            {
                match stdin.read(&mut wake) {
                    Ok(0) => return, // the app has gone
                    Err(error) if error.kind() != std::io::ErrorKind::Interrupted => return,
                    _ => {}
                }
            }
            header.sleeping.store(0, Ordering::SeqCst);
        };

        // SAFETY: the node posted the block and won't touch its regions
        // until it sees `done_seq` catch up.
        unsafe {
            run_block(
                mapping,
                &spec,
                node,
                &mut buffers,
                sample_rate,
                header,
                &mut seen_reset,
            )
        };
        header.done_seq.store(request, Ordering::Release);
    }
}

unsafe fn run_block(
    mapping: &Mapping,
    spec: &super::shm::Spec,
    node: &mut dyn Node,
    b: &mut Buffers,
    sample_rate: f32,
    header: &super::shm::Header,
    seen_reset: &mut u64,
) {
    let reset = header.reset_seq.load(Ordering::Acquire);
    if reset != *seen_reset {
        *seen_reset = reset;
        node.reset();
    }
    let params = unsafe { mapping.read_block() };
    let frames = (params.frames as usize).min(spec.max_frames);

    b.inputs.clear();
    for index in 0..spec.inputs.len() {
        let shape = spec.input_shape(index);
        let len = shape.lanes() * frames;
        let meta = unsafe { mapping.read_input_meta(index) };
        let data: &'static [f32] = if meta.constant != 0 {
            let key = (meta.value.to_bits(), frames);
            let buffer = &mut b.constants[index];
            if b.filled[index] != Some(key) {
                buffer[..len].fill(meta.value);
                b.filled[index] = Some(key);
            }
            // SAFETY: the buffers outlive the block and aren't touched
            // again until the next one.
            unsafe { std::slice::from_raw_parts(buffer.as_ptr(), len) }
        } else {
            let floats = unsafe { mapping.input_floats(index) };
            unsafe { std::slice::from_raw_parts(floats.as_ptr(), len) }
        };
        let flags = unsafe { mapping.input_silent(index) };
        for (flag, byte) in b.silent_in[index].iter_mut().zip(flags.iter()) {
            *flag = *byte != 0;
        }
        let silent = &b.silent_in[index];
        // SAFETY: as above.
        let silent: &'static [bool] =
            unsafe { std::slice::from_raw_parts(silent.as_ptr(), silent.len()) };
        let mut signal = SignalIn::new(data, shape, frames).with_silent(silent);
        if meta.constant != 0 {
            signal = signal.with_constant(meta.value);
        }
        b.inputs.push(signal);
    }

    b.outputs.clear();
    for index in 0..spec.outputs.len() {
        let shape = spec.output_shape(index);
        let floats = unsafe { mapping.output_floats(index) };
        let data: &'static mut [f32] =
            unsafe { std::slice::from_raw_parts_mut(floats.as_mut_ptr(), shape.lanes() * frames) };
        let silent = &mut b.silent_out[index];
        // SAFETY: as above.
        let silent: &'static mut [bool] =
            unsafe { std::slice::from_raw_parts_mut(silent.as_mut_ptr(), silent.len()) };
        b.outputs
            .push(SignalOut::new(data, shape, frames).with_silent(silent));
    }

    b.event_in_refs.clear();
    for port in 0..spec.event_inputs {
        let count = (unsafe { *mapping.event_in_count(port) } as usize).min(MAX_EVENTS_IN);
        let wire = unsafe { mapping.events_in(port) };
        let events = &mut b.event_in[port];
        events.clear();
        events.extend(wire[..count].iter().map(WireEvent::event));
        // SAFETY: as above.
        b.event_in_refs
            .push(unsafe { std::slice::from_raw_parts(events.as_ptr(), events.len()) });
    }
    b.event_out_handles.clear();
    for events in &mut b.event_out {
        // SAFETY: as above; the handles are dropped before the vectors are
        // read.
        let events: &'static mut Vec<Event> = unsafe { &mut *(events as *mut Vec<Event>) };
        b.event_out_handles.push(EventsOut::new(events));
    }

    let ctx = Context {
        sample_rate,
        frames,
        transport: params.transport(),
    };
    node.process(
        &ctx,
        Io {
            inputs: &b.inputs,
            outputs: &mut b.outputs,
            event_inputs: &b.event_in_refs,
            event_outputs: &mut b.event_out_handles,
        },
    );

    b.event_out_handles.clear();
    b.outputs.clear();
    for (port, events) in b.event_out.iter().enumerate() {
        let wire = unsafe { mapping.events_out(port) };
        let count = events.len().min(MAX_EVENTS_OUT);
        for (slot, event) in wire.iter_mut().zip(events) {
            *slot = WireEvent::new(event);
        }
        unsafe { *mapping.event_out_count(port) = count as u32 };
    }
    for (index, flags) in b.silent_out.iter().enumerate() {
        let bytes = unsafe { mapping.output_silent(index) };
        for (byte, flag) in bytes.iter_mut().zip(flags) {
            *byte = u8::from(*flag);
        }
    }
}
