//! MIDI output: a hub the MIDI Out nodes hand timestamped messages to, and
//! a sender thread that gives them to the system's MIDI ports when they are
//! due.
//!
//! Each node owns a preallocated ring buffer, so the audio thread pushes a
//! message without locking or allocating. One thread (started with the first
//! node, gone with the last) drains every ring into a queue ordered by time,
//! sleeps until the next message is due and hands it to the port. A message
//! says when it should leave, in microseconds on the clock of
//! [`now_micros`]: when the audio it goes with is heard (see
//! [`AudioClock::heard_at`](crate::AudioClock::heard_at)), so an external
//! synth sounds with the project, not one output latency ahead of it.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use midir::{MidiOutput, MidiOutputConnection};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::midi::{AudioClock, MidiError, MidiMessage, now_micros};

/// Messages a node can queue before new ones are dropped.
const RING: usize = 4096;

/// How long the sender sleeps at most, which bounds how soon it notices a
/// message from a node.
const IDLE_MICROS: u64 = 2_000;

/// How long to leave a port that would not open alone before trying again.
const RETRY_MICROS: u64 = 1_000_000;

/// One message waiting to leave.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutMessage {
    /// When it leaves, in microseconds on the clock of [`now_micros`].
    pub due: u64,
    bytes: MidiMessage,
    len: u8,
}

impl OutMessage {
    /// A message of the length its status byte says (two bytes for a
    /// program change or channel pressure, one for system real-time).
    pub fn new(due: u64, bytes: MidiMessage) -> Self {
        let len = match bytes[0] {
            0xF8..=0xFF => 1,
            0xC0..=0xDF => 2,
            _ => 3,
        };
        Self { due, bytes, len }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// Where the sender thread takes messages. A real port, or a recorder in
/// tests.
pub trait MidiSink: Send {
    fn send(&mut self, bytes: &[u8]);
}

impl MidiSink for MidiOutputConnection {
    fn send(&mut self, bytes: &[u8]) {
        let _ = MidiOutputConnection::send(self, bytes);
    }
}

/// Opens the output port called `name`.
pub type OpenSink = fn(&str) -> Result<Box<dyn MidiSink>, MidiError>;

/// The names of the MIDI output ports, as the system lists them.
pub fn midi_outputs() -> Result<Vec<String>, MidiError> {
    let output =
        MidiOutput::new("noodle").map_err(|e| MidiError(format!("no MIDI available: {e}")))?;
    Ok(output
        .ports()
        .iter()
        .filter_map(|port| output.port_name(port).ok())
        .collect())
}

/// Opens the MIDI output port called `name`, or one that
/// [`same_port`](crate::same_port) calls the same.
pub fn open_midi_output(name: &str) -> Result<Box<dyn MidiSink>, MidiError> {
    let output =
        MidiOutput::new("noodle").map_err(|e| MidiError(format!("no MIDI available: {e}")))?;
    let ports = output.ports();
    let named = |exact: bool| {
        ports.iter().find(|port| {
            output.port_name(port).is_ok_and(|n| {
                if exact {
                    n == name
                } else {
                    crate::same_port(&n, name)
                }
            })
        })
    };
    let port = named(true)
        .or_else(|| named(false))
        .ok_or_else(|| MidiError(format!("no MIDI output called {name:?}")))?;
    let connection = output
        .connect(port, "noodle")
        .map_err(|e| MidiError(format!("can't open {name:?}: {e}")))?;
    Ok(Box::new(connection))
}

struct Source {
    port: Arc<str>,
    ring: Consumer<OutMessage>,
}

#[derive(Default)]
struct State {
    sources: Vec<Source>,
    running: bool,
}

struct Shared {
    clock: Arc<AudioClock>,
    open: OpenSink,
    state: Mutex<State>,
}

/// What the MIDI Out nodes send through. Cloning gives another handle to the
/// same hub.
#[derive(Clone)]
pub struct MidiOutHub {
    shared: Arc<Shared>,
}

impl MidiOutHub {
    /// A hub that sends to the system's ports, timing messages by `clock`.
    pub fn new(clock: Arc<AudioClock>) -> Self {
        Self::with_opener(clock, open_midi_output)
    }

    /// A hub that gets its sinks from `open`.
    pub fn with_opener(clock: Arc<AudioClock>, open: OpenSink) -> Self {
        Self {
            shared: Arc::new(Shared {
                clock,
                open,
                state: Mutex::new(State::default()),
            }),
        }
    }

    pub fn clock(&self) -> &Arc<AudioClock> {
        &self.shared.clock
    }

    /// A way to send to the port called `port`; an empty name sends nowhere.
    /// Starts the sender thread if it isn't running. Locks, so not for the
    /// audio thread.
    pub fn sender(&self, port: &str) -> MidiOutSender {
        let (producer, ring) = RingBuffer::new(RING);
        let mut state = self.shared.state.lock().expect("midi out lock");
        if !port.is_empty() {
            state.sources.push(Source {
                port: port.into(),
                ring,
            });
            if !state.running {
                state.running = true;
                let shared = Arc::clone(&self.shared);
                std::thread::Builder::new()
                    .name("noodle-midi-out".into())
                    .spawn(move || run(&shared))
                    .expect("spawn the MIDI out thread");
            }
        }
        MidiOutSender {
            producer,
            enabled: !port.is_empty(),
            clock: Arc::clone(&self.shared.clock),
        }
    }
}

/// One node's way to the sender thread. Pushing never locks or allocates.
pub struct MidiOutSender {
    producer: Producer<OutMessage>,
    enabled: bool,
    clock: Arc<AudioClock>,
}

impl MidiOutSender {
    /// A sender that queues into the returned ring and nothing reads it, for
    /// tests of what a node would send.
    pub fn capture(clock: Arc<AudioClock>) -> (Self, Consumer<OutMessage>) {
        let (producer, ring) = RingBuffer::new(RING);
        (
            Self {
                producer,
                enabled: true,
                clock,
            },
            ring,
        )
    }

    /// Queues `message`. Returns false if it was dropped: no port, or the
    /// sender thread has fallen `4096` messages behind.
    pub fn send(&mut self, message: OutMessage) -> bool {
        self.enabled && self.producer.push(message).is_ok()
    }

    /// When the first frame of the block being run is heard, or `None` if no
    /// device is driving the engine.
    pub fn heard_at(&self) -> Option<u64> {
        self.clock.heard_at()
    }
}

struct Port {
    sink: Option<Box<dyn MidiSink>>,
    retry_at: u64,
}

/// Ordered by time, then arrival, so messages for one instant leave as sent.
type Queued = Reverse<(u64, u64, Arc<str>, OutMessage)>;

fn run(shared: &Shared) {
    let mut queue: BinaryHeap<Queued> = BinaryHeap::new();
    let mut ports: HashMap<Arc<str>, Port> = HashMap::new();
    let mut sequence = 0u64;
    loop {
        {
            let mut state = shared.state.lock().expect("midi out lock");
            for source in &mut state.sources {
                while let Ok(message) = source.ring.pop() {
                    sequence += 1;
                    queue.push(Reverse((
                        message.due,
                        sequence,
                        source.port.clone(),
                        message,
                    )));
                }
            }
            state
                .sources
                .retain(|s| !(s.ring.is_abandoned() && s.ring.is_empty()));
            if state.sources.is_empty() && queue.is_empty() {
                state.running = false;
                return;
            }
        }
        let now = now_micros();
        while queue.peek().is_some_and(|Reverse((due, ..))| *due <= now) {
            let Some(Reverse((_, _, name, message))) = queue.pop() else {
                break;
            };
            let port = ports.entry(name.clone()).or_insert(Port {
                sink: None,
                retry_at: 0,
            });
            if port.sink.is_none() && now >= port.retry_at {
                port.sink = (shared.open)(&name).ok();
                port.retry_at = now + RETRY_MICROS;
            }
            if let Some(sink) = &mut port.sink {
                sink.send(message.bytes());
            }
        }
        let wait = queue.peek().map_or(IDLE_MICROS, |Reverse((due, ..))| {
            due.saturating_sub(now_micros()).min(IDLE_MICROS)
        });
        if wait > 300 {
            std::thread::sleep(Duration::from_micros(wait - 300));
        } else {
            std::thread::yield_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::sync::OnceLock;

    type Log = Vec<(u64, Vec<u8>)>;

    fn log() -> &'static StdMutex<Log> {
        static LOG: OnceLock<StdMutex<Log>> = OnceLock::new();
        LOG.get_or_init(|| StdMutex::new(Vec::new()))
    }

    struct Recorder;

    impl MidiSink for Recorder {
        fn send(&mut self, bytes: &[u8]) {
            log().lock().unwrap().push((now_micros(), bytes.to_vec()));
        }
    }

    fn recorder(_: &str) -> Result<Box<dyn MidiSink>, MidiError> {
        Ok(Box::new(Recorder))
    }

    fn wait_for(count: usize) {
        for _ in 0..500 {
            if log().lock().unwrap().len() >= count {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("only {} messages left", log().lock().unwrap().len());
    }

    #[test]
    fn messages_leave_in_time_order_and_not_early() {
        let hub = MidiOutHub::with_opener(Arc::new(AudioClock::default()), recorder);
        let mut sender = hub.sender("synth");
        let start = now_micros();
        // Queued out of order; the later one first.
        assert!(sender.send(OutMessage::new(start + 60_000, [0x80, 60, 0])));
        assert!(sender.send(OutMessage::new(start + 30_000, [0x90, 60, 100])));
        assert!(sender.send(OutMessage::new(start + 30_000, [0xC1, 5, 0])));
        wait_for(3);
        let log = log().lock().unwrap();
        let sent: Vec<_> = log.iter().map(|(_, b)| b.clone()).collect();
        assert_eq!(
            sent,
            [vec![0x90, 60, 100], vec![0xC1, 5], vec![0x80, 60, 0]],
        );
        assert!(log[0].0 >= start + 30_000, "left early");
        assert!(log[2].0 >= start + 60_000, "left early");
        assert!(log[2].0 < start + 60_000 + 20_000, "left late");
    }

    #[test]
    fn a_message_is_the_length_its_status_says() {
        assert_eq!(OutMessage::new(0, [0xF8, 0, 0]).bytes(), [0xF8]);
        assert_eq!(OutMessage::new(0, [0xD0, 9, 0]).bytes(), [0xD0, 9]);
        assert_eq!(OutMessage::new(0, [0xE0, 1, 2]).bytes(), [0xE0, 1, 2]);
    }

    #[test]
    fn the_time_a_block_is_heard_is_its_start_and_the_latency() {
        let clock = AudioClock::default();
        assert_eq!(clock.heard_at(), None);
        clock.set_block(1_000);
        clock.set_output_latency(12_000);
        assert_eq!(clock.heard_at(), Some(13_000));
    }

    #[test]
    fn no_port_sends_nowhere() {
        let hub = MidiOutHub::with_opener(Arc::new(AudioClock::default()), recorder);
        let mut sender = hub.sender("");
        assert!(!sender.send(OutMessage::new(0, [0xFA, 0, 0])));
    }

    /// Sends to a virtual port the test makes. Skips where the system has no
    /// MIDI (a container) or no virtual ports (Windows).
    #[cfg(unix)]
    #[test]
    fn loopback_through_a_virtual_port() {
        use midir::MidiInput;
        use midir::os::unix::VirtualInput;
        let Ok(input) = MidiInput::new("noodle-test") else {
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let Ok(_port) = input.create_virtual(
            "noodle loopback",
            move |_, bytes, _| {
                let _ = sender.send((now_micros(), bytes.to_vec()));
            },
            (),
        ) else {
            return;
        };
        std::thread::sleep(Duration::from_millis(200));
        let Ok(names) = midi_outputs() else { return };
        let Some(name) = names.iter().find(|n| n.contains("noodle loopback")) else {
            return;
        };
        let hub = MidiOutHub::new(Arc::new(AudioClock::default()));
        let mut out = hub.sender(name);
        let due = now_micros() + 50_000;
        out.send(OutMessage::new(due, [0x90, 64, 90]));
        let (arrived, bytes) = receiver
            .recv_timeout(Duration::from_secs(3))
            .expect("the message arrives");
        assert_eq!(bytes, [0x90, 64, 90]);
        assert!(arrived >= due, "arrived early");
    }
}
