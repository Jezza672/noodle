//! MIDI input: ports, and a bus that carries what arrives on one of them to
//! the MIDI In nodes in the graph.
//!
//! The device driver calls back on its own thread. That thread pushes each
//! message into a ring buffer per subscriber, and a MIDI In node pops its
//! own on the audio thread, so neither side waits for the other and the
//! audio thread never locks. Subscribing and sending lock the list of
//! subscribers, which only the driver's thread, the node's constructor and
//! tests do.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use midir::{Ignore, MidiInput, MidiInputConnection};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::ump::Packet;

/// One MIDI message of up to three bytes: a status byte and its data.
/// Messages with one data byte (program change, channel pressure) have a
/// zero in the last.
pub type MidiMessage = [u8; 3];

/// A MIDI 1.0 message and when it arrived, in microseconds on the clock of
/// [`now_micros`].
pub type TimedMessage = (u64, MidiMessage);

/// A packet (MIDI 1.0 or 2.0) and when it arrived.
pub type TimedPacket = (u64, Packet);

/// Microseconds since the first call, on a monotonic clock. Cheap and safe to
/// call on the audio thread once it has been called anywhere before, which
/// opening a [`MidiBus`] does.
pub fn now_micros() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// What the audio thread tells the rest of the program about when its
/// blocks run, on the clock of [`now_micros`]. The device callback writes it
/// and the MIDI In nodes and recordings read it, so nothing waits.
#[derive(Default)]
pub struct AudioClock {
    /// One more than the start of the span of arrival times that the block
    /// about to run plays, or 0 before any callback.
    window: AtomicU64,
    /// One more than when the block about to run is heard on the clock of
    /// [`now_micros`] (before the output latency), or 0 before any callback.
    block: AtomicU64,
    /// How long after the callback its samples are heard, in microseconds.
    latency: AtomicU64,
    /// A seqlock round: odd while the anchor below is being written.
    sequence: AtomicU64,
    anchor_micros: AtomicU64,
    anchor_sample: AtomicU64,
}

impl AudioClock {
    /// Says which arrivals the next block plays: the ones from `micros` on,
    /// for as long as the block lasts.
    pub fn set_window(&self, micros: u64) {
        self.window
            .store(micros.saturating_add(1), Ordering::Relaxed);
    }

    /// Says that the block about to run begins at `micros`, which is the
    /// time of the device callback plus the blocks of it already run.
    pub fn set_block(&self, micros: u64) {
        self.block
            .store(micros.saturating_add(1), Ordering::Relaxed);
    }

    /// How long after a callback the samples it fills are heard, as far as
    /// the driver knows.
    pub fn set_output_latency(&self, micros: u64) {
        self.latency.store(micros, Ordering::Relaxed);
    }

    /// When the first frame of the block being run is heard, in
    /// microseconds on the clock of [`now_micros`], or `None` if no device is
    /// driving the engine (an offline render, say).
    pub fn heard_at(&self) -> Option<u64> {
        let block = self.block.load(Ordering::Relaxed).checked_sub(1)?;
        Some(block.saturating_add(self.latency.load(Ordering::Relaxed)))
    }

    /// The start of the span the block being run plays, if a device is
    /// driving the engine.
    pub fn window(&self) -> Option<u64> {
        self.window.load(Ordering::Relaxed).checked_sub(1)
    }

    /// Records that the transport was at `sample` when the clock read
    /// `micros`. Called once per device callback.
    pub fn publish_anchor(&self, micros: u64, sample: u64) {
        let round = self.sequence.load(Ordering::Relaxed);
        self.sequence.store(round + 1, Ordering::Release);
        self.anchor_micros.store(micros, Ordering::Relaxed);
        self.anchor_sample.store(sample, Ordering::Relaxed);
        self.sequence.store(round + 2, Ordering::Release);
    }

    /// The latest `(micros, transport sample)` pair, or `None` before the
    /// first callback.
    pub fn anchor(&self) -> Option<(u64, u64)> {
        for _ in 0..8 {
            let before = self.sequence.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            if before % 2 == 1 {
                continue;
            }
            let pair = (
                self.anchor_micros.load(Ordering::Relaxed),
                self.anchor_sample.load(Ordering::Relaxed),
            );
            std::sync::atomic::fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == before {
                return Some(pair);
            }
        }
        None
    }
}

/// Messages a subscriber can fall behind by before new ones are dropped.
const QUEUE: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiError(pub String);

impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MidiError {}

/// Turns the bytes a driver delivered into a [`MidiMessage`], or `None` for
/// what the engine doesn't take: system exclusive and real-time messages
/// (clock, active sensing), and anything that isn't a whole channel message.
pub fn parse_message(bytes: &[u8]) -> Option<MidiMessage> {
    let status = *bytes.first()?;
    if !(0x80..0xF0).contains(&status) {
        return None;
    }
    let wanted = match status & 0xF0 {
        0xC0 | 0xD0 => 2,
        _ => 3,
    };
    if bytes.len() < wanted {
        return None;
    }
    let data = |i: usize| bytes.get(i).copied().unwrap_or(0) & 0x7F;
    Some([status, data(1), if wanted == 3 { data(2) } else { 0 }])
}

/// A MIDI timing message: the clock a sequencer or drum machine sends to be
/// followed. These don't go to the MIDI In nodes, which would drown in 24 a
/// beat; they go to [`ClockTap`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockEvent {
    /// One of the 24 pulses to a quarter note.
    Tick,
    /// Play from the start.
    Start,
    /// Play on from where it stopped.
    Continue,
    Stop,
    /// A position, in sixteenth notes from the start.
    SongPosition(u16),
}

/// A clock message, the port it came from, and when it arrived in
/// microseconds on a clock of the driver's. Only differences between times
/// mean anything.
pub type ClockHeard = (String, u64, ClockEvent);

/// Turns the bytes a driver delivered into a [`ClockEvent`], or `None` for
/// anything else.
pub fn parse_clock(bytes: &[u8]) -> Option<ClockEvent> {
    match *bytes.first()? {
        0xF8 => Some(ClockEvent::Tick),
        0xFA => Some(ClockEvent::Start),
        0xFB => Some(ClockEvent::Continue),
        0xFC => Some(ClockEvent::Stop),
        0xF2 if bytes.len() >= 3 => Some(ClockEvent::SongPosition(
            u16::from(bytes[1] & 0x7F) | u16::from(bytes[2] & 0x7F) << 7,
        )),
        _ => None,
    }
}

/// A listener for MIDI clock, from [`MidiBus::clock_tap`]. Dropping it stops
/// listening.
pub struct ClockTap(mpsc::Receiver<ClockHeard>);

impl ClockTap {
    /// The oldest message not yet taken.
    pub fn next(&self) -> Option<ClockHeard> {
        self.0.try_recv().ok()
    }
}

/// Carries MIDI messages from whatever produces them to the MIDI In nodes.
/// Clones share the same subscribers.
#[derive(Clone, Default)]
pub struct MidiBus {
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
    taps: Arc<Mutex<Vec<mpsc::Sender<Heard>>>>,
    clock_taps: Arc<Mutex<Vec<mpsc::Sender<ClockHeard>>>>,
    clock: Arc<AudioClock>,
}

/// A message a [`MidiTap`] heard and the port it came from, `None` for what
/// was not sent from a port (the computer keyboard).
pub type Heard = (Option<String>, MidiMessage);

/// The port name that stands for every open port, in a node's port setting.
pub const ALL_INPUTS: &str = "*";

/// Whether a node's port setting means every port: empty or [`ALL_INPUTS`].
pub fn is_all_inputs(port: &str) -> bool {
    port.is_empty() || port == ALL_INPUTS
}

struct Subscriber {
    producer: Producer<TimedPacket>,
    /// Only note-ons and note-offs are wanted.
    notes_only: bool,
    /// Only messages from this port are wanted; `None` wants them all,
    /// including the ones that come from no port.
    port: Option<String>,
}

/// A listener for MIDI learn: it hears every message with the port it came
/// from. Taken from [`MidiBus::tap`] and read on the UI thread; dropping it
/// stops listening.
pub struct MidiTap(mpsc::Receiver<Heard>);

impl MidiTap {
    /// The oldest message not yet taken.
    pub fn next(&self) -> Option<Heard> {
        self.0.try_recv().ok()
    }
}

impl MidiBus {
    pub fn new() -> Self {
        now_micros();
        Self::default()
    }

    /// A new receiver of every message sent from now on. Dropping it
    /// unsubscribes.
    pub fn subscribe(&self) -> MidiReceiver {
        self.subscribe_with(false, None)
    }

    /// The same for the messages of one port, as a node's port setting
    /// names it (see [`is_all_inputs`]: empty or `*` hears every port).
    pub fn subscribe_port(&self, port: &str) -> MidiReceiver {
        let port = (!is_all_inputs(port)).then(|| port.to_string());
        self.subscribe_with(false, port)
    }

    /// The same for receivers that want only notes, so a stream of
    /// controllers can't crowd them out of the queue.
    pub fn subscribe_notes(&self) -> MidiReceiver {
        self.subscribe_with(true, None)
    }

    fn subscribe_with(&self, notes_only: bool, port: Option<String>) -> MidiReceiver {
        let (producer, consumer) = RingBuffer::new(QUEUE);
        self.subscribers
            .lock()
            .expect("subscribers lock")
            .push(Subscriber {
                producer,
                notes_only,
                port,
            });
        MidiReceiver(consumer)
    }

    /// A listener for MIDI learn, hearing every message sent from now on.
    pub fn tap(&self) -> MidiTap {
        let (sender, receiver) = mpsc::channel();
        self.taps.lock().expect("taps lock").push(sender);
        MidiTap(receiver)
    }

    /// A listener for MIDI clock, hearing every timing message sent from now
    /// on.
    pub fn clock_tap(&self) -> ClockTap {
        let (sender, receiver) = mpsc::channel();
        self.clock_taps.lock().expect("clock lock").push(sender);
        ClockTap(receiver)
    }

    /// Hands a timing message that arrived on `port` to the clock taps.
    pub fn send_clock_from(&self, port: &str, micros: u64, event: ClockEvent) {
        let mut taps = self.clock_taps.lock().expect("clock lock");
        taps.retain(|tap| tap.send((port.to_string(), micros, event)).is_ok());
    }

    /// When the audio thread's blocks run. The device callback fills it in.
    pub fn clock(&self) -> &Arc<AudioClock> {
        &self.clock
    }

    /// Hands a MIDI 1.0 message to every receiver. One that has fallen
    /// `QUEUE` messages behind misses it. The time is read once the lock is
    /// held, so two threads sending at once are stamped in the order they are
    /// queued.
    pub fn send(&self, message: MidiMessage) {
        self.send_packet(Packet::midi1(0, message));
    }

    /// The same for a message that arrived at `micros` on the clock of
    /// [`now_micros`].
    pub fn send_at(&self, message: MidiMessage, micros: u64) {
        self.send_packet_at(Packet::midi1(0, message), micros);
    }

    /// Hands a packet (MIDI 1.0 or 2.0) to every receiver.
    pub fn send_packet(&self, packet: Packet) {
        self.deliver_from(None, packet, None);
    }

    /// The same for a packet that arrived at `micros`.
    pub fn send_packet_at(&self, packet: Packet, micros: u64) {
        self.deliver_from(None, packet, Some(micros));
    }

    /// A message that arrived on the port called `port`, which only the
    /// receivers of that port (and of every port) get.
    pub fn send_from(&self, port: &str, message: MidiMessage) {
        self.deliver_from(Some(port), Packet::midi1(0, message), None);
    }

    /// The same for a packet.
    pub fn send_packet_from(&self, port: &str, packet: Packet) {
        self.deliver_from(Some(port), packet, None);
    }

    fn deliver_from(&self, port: Option<&str>, packet: Packet, micros: Option<u64>) {
        let mut subscribers = self.subscribers.lock().expect("subscribers lock");
        deliver(
            &mut subscribers,
            port,
            packet,
            micros.unwrap_or_else(now_micros),
        );
        drop(subscribers);
        if let Some(message) = packet.to_midi1() {
            let mut taps = self.taps.lock().expect("taps lock");
            taps.retain(|tap| tap.send((port.map(str::to_string), message)).is_ok());
        }
    }

    /// Releases every note on every channel (controller 123), for when the
    /// port that held them down goes away.
    pub fn all_notes_off(&self) {
        for channel in 0..16 {
            self.send([0xB0 | channel, 123, 0]);
        }
    }

    /// The same for the notes one port held, which the receivers of other
    /// ports don't see.
    pub fn all_notes_off_from(&self, port: &str) {
        for channel in 0..16 {
            self.send_from(port, [0xB0 | channel, 123, 0]);
        }
    }

    /// How many receivers are listening.
    pub fn receivers(&self) -> usize {
        let mut subscribers = self.subscribers.lock().expect("subscribers lock");
        subscribers.retain(|s| !s.producer.is_abandoned());
        subscribers.len()
    }
}

fn deliver(subscribers: &mut Vec<Subscriber>, from: Option<&str>, packet: Packet, micros: u64) {
    let note = packet.is_note();
    subscribers.retain(|s| !s.producer.is_abandoned());
    for subscriber in subscribers.iter_mut() {
        let heard = match (&subscriber.port, from) {
            (None, _) => true,
            (Some(wanted), Some(from)) => same_port(from, wanted),
            (Some(_), None) => false,
        };
        if heard && (note || !subscriber.notes_only) {
            let _ = subscriber.producer.push((micros, packet));
        }
    }
}

/// The audio thread's end of a [`MidiBus`] subscription.
pub struct MidiReceiver(Consumer<TimedPacket>);

impl MidiReceiver {
    /// The oldest MIDI 1.0 message not yet taken. Never blocks or allocates.
    pub fn pop(&mut self) -> Option<MidiMessage> {
        self.pop_timed().map(|(_, message)| message)
    }

    /// The same with the time the message arrived. This is lossy: MIDI 2.0
    /// messages are narrowed to the first MIDI 1.0 message they make
    /// ([`Packet::to_midi1`]; a parameter gives only its first controller, a
    /// program change loses its bank) and packets with no MIDI 1.0 form are
    /// skipped. It is for readers of notes, like the take recorder; use
    /// [`pop_packet`](Self::pop_packet) for the rest.
    pub fn pop_timed(&mut self) -> Option<TimedMessage> {
        loop {
            let (at, packet) = self.0.pop().ok()?;
            if let Some(message) = packet.to_midi1() {
                return Some((at, message));
            }
        }
    }

    /// The oldest packet not yet taken, whatever it holds, with the time it
    /// arrived.
    pub fn pop_packet(&mut self) -> Option<TimedPacket> {
        self.0.pop().ok()
    }
}

/// Whether two port names are for the same port. ALSA ends a port's name
/// with its client and port numbers (`Keys MIDI 1 24:0`), and a device that
/// is plugged back in can come back with other numbers, so those are left
/// out of the comparison.
pub fn same_port(a: &str, b: &str) -> bool {
    fn stem(name: &str) -> &str {
        match name.rsplit_once(' ') {
            Some((stem, tail))
                if tail
                    .split_once(':')
                    .is_some_and(|(c, p)| is_number(c) && is_number(p)) =>
            {
                stem
            }
            _ => name,
        }
    }
    fn is_number(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
    }
    a == b || stem(a) == stem(b)
}

/// The names of the MIDI input ports, as the system lists them.
pub fn midi_inputs() -> Result<Vec<String>, MidiError> {
    MidiLister::new()?.names()
}

/// Lists the MIDI input ports, keeping one client open between calls.
/// Making a client each time is slow, and on macOS closing the last one a
/// process has can make the MIDI server quit.
pub struct MidiLister(MidiInput);

impl MidiLister {
    pub fn new() -> Result<Self, MidiError> {
        new_input().map(Self)
    }

    pub fn names(&self) -> Result<Vec<String>, MidiError> {
        Ok(self
            .0
            .ports()
            .iter()
            .filter_map(|port| self.0.port_name(port).ok())
            .collect())
    }
}

/// An open MIDI input port. Messages go to the bus until it is dropped.
pub struct MidiConnection {
    name: String,
    // Never read: dropping it closes the port. None for a detached one.
    _connection: Option<MidiInputConnection<()>>,
}

impl MidiConnection {
    /// A connection to no port, which sends nothing. For tests of code that
    /// opens and drops ports, in places without MIDI hardware.
    pub fn detached(name: &str) -> Self {
        Self {
            name: name.to_string(),
            _connection: None,
        }
    }

    /// The port's name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Opens the MIDI input port called `name` and sends what arrives on it to
/// `bus`.
pub fn connect_midi(name: &str, bus: &MidiBus) -> Result<MidiConnection, MidiError> {
    let input = new_input()?;
    let port = input
        .ports()
        .into_iter()
        .find(|port| input.port_name(port).is_ok_and(|n| n == name))
        .or_else(|| {
            input
                .ports()
                .into_iter()
                .find(|port| input.port_name(port).is_ok_and(|n| same_port(&n, name)))
        })
        .ok_or_else(|| MidiError(format!("no MIDI input called {name:?}")))?;
    // The port's own name, which is what a later look at the list matches.
    let actual = input.port_name(&port).unwrap_or_else(|_| name.to_string());
    let bus = bus.clone();
    let port_name = actual.clone();
    let connection = input
        .connect(
            &port,
            "noodle",
            move |stamp, bytes, _| {
                if let Some(message) = parse_message(bytes) {
                    bus.send_from(&port_name, message);
                } else if let Some(event) = parse_clock(bytes) {
                    bus.send_clock_from(&port_name, stamp, event);
                }
            },
            (),
        )
        .map_err(|e| MidiError(format!("can't open {name:?}: {e}")))?;
    Ok(MidiConnection {
        name: actual,
        _connection: Some(connection),
    })
}

fn new_input() -> Result<MidiInput, MidiError> {
    let mut input =
        MidiInput::new("noodle").map_err(|e| MidiError(format!("no MIDI available: {e}")))?;
    // Timing is wanted (MIDI clock); system exclusive and active sensing
    // are not.
    input.ignore(Ignore::SysexAndActiveSense);
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_messages_are_kept_and_the_rest_dropped() {
        assert_eq!(parse_message(&[0x90, 60, 100]), Some([0x90, 60, 100]));
        assert_eq!(parse_message(&[0xE3, 0, 0x40]), Some([0xE3, 0, 0x40]));
        // One data byte: padded.
        assert_eq!(parse_message(&[0xC1, 5]), Some([0xC1, 5, 0]));
        assert_eq!(parse_message(&[0xD0, 70]), Some([0xD0, 70, 0]));
        // Cut short, a lone data byte, system exclusive, clock.
        assert_eq!(parse_message(&[0x90, 60]), None);
        assert_eq!(parse_message(&[60, 100]), None);
        assert_eq!(parse_message(&[0xF0, 1, 2, 0xF7]), None);
        assert_eq!(parse_message(&[0xF8]), None);
        assert_eq!(parse_message(&[]), None);
        // Data bytes are seven bits.
        assert_eq!(parse_message(&[0x90, 0xFF, 0x80]), Some([0x90, 0x7F, 0]));
    }

    #[test]
    fn timing_messages_are_parsed_and_reach_clock_taps_only() {
        assert_eq!(parse_clock(&[0xF8]), Some(ClockEvent::Tick));
        assert_eq!(parse_clock(&[0xFA]), Some(ClockEvent::Start));
        assert_eq!(parse_clock(&[0xFB]), Some(ClockEvent::Continue));
        assert_eq!(parse_clock(&[0xFC]), Some(ClockEvent::Stop));
        assert_eq!(
            parse_clock(&[0xF2, 0x01, 0x02]),
            Some(ClockEvent::SongPosition(1 + (2 << 7)))
        );
        assert_eq!(parse_clock(&[0xF2, 1]), None);
        assert_eq!(parse_clock(&[0x90, 60, 1]), None);
        let bus = MidiBus::new();
        let mut notes = bus.subscribe();
        let tap = bus.clock_tap();
        bus.send_clock_from("Drums", 100, ClockEvent::Tick);
        assert_eq!(notes.pop(), None);
        assert_eq!(
            tap.next(),
            Some(("Drums".to_string(), 100, ClockEvent::Tick))
        );
        assert_eq!(tap.next(), None);
    }

    #[test]
    fn alsa_numbers_are_left_out_of_port_names() {
        assert!(same_port("Keys MIDI 1 24:0", "Keys MIDI 1 28:0"));
        assert!(same_port("Keys", "Keys"));
        assert!(!same_port("Keys MIDI 1 24:0", "Pads MIDI 1 24:0"));
        assert!(!same_port("Keys 1", "Keys 2"));
        assert!(!same_port("Keys 24:0", "Pads 24:0"));
    }

    #[test]
    fn a_port_receiver_hears_only_its_port_and_a_tap_hears_all_with_the_port() {
        let bus = MidiBus::new();
        let mut all = bus.subscribe_port("*");
        let mut keys = bus.subscribe_port("Keys MIDI 1 24:0");
        let mut pads = bus.subscribe_port("Pads");
        let tap = bus.tap();
        bus.send_from("Keys MIDI 1 28:0", [0xB0, 74, 10]);
        bus.send_from("Pads", [0xB0, 1, 20]);
        bus.send([0xB0, 2, 30]);
        assert_eq!(all.pop(), Some([0xB0, 74, 10]));
        assert_eq!(all.pop(), Some([0xB0, 1, 20]));
        assert_eq!(all.pop(), Some([0xB0, 2, 30]));
        assert_eq!(keys.pop(), Some([0xB0, 74, 10]));
        assert_eq!(keys.pop(), None);
        assert_eq!(pads.pop(), Some([0xB0, 1, 20]));
        assert_eq!(pads.pop(), None);
        assert_eq!(
            tap.next(),
            Some((Some("Keys MIDI 1 28:0".to_string()), [0xB0, 74, 10]))
        );
        assert_eq!(tap.next(), Some((Some("Pads".to_string()), [0xB0, 1, 20])));
        assert_eq!(tap.next(), Some((None, [0xB0, 2, 30])));
        assert_eq!(tap.next(), None);
    }

    #[test]
    fn the_bus_reaches_every_receiver_and_forgets_dropped_ones() {
        let bus = MidiBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        bus.send([0x90, 60, 100]);
        bus.send([0x80, 60, 0]);
        assert_eq!(a.pop(), Some([0x90, 60, 100]));
        assert_eq!(a.pop(), Some([0x80, 60, 0]));
        assert_eq!(a.pop(), None);
        assert_eq!(b.pop(), Some([0x90, 60, 100]));
        drop(b);
        assert_eq!(bus.receivers(), 1);
        // A receiver that never reads loses messages past its queue, and
        // doesn't hold up the others.
        let mut slow = bus.subscribe();
        for _ in 0..QUEUE + 10 {
            bus.send([0xB0, 1, 2]);
            assert_eq!(a.pop(), Some([0xB0, 1, 2]));
        }
        let mut kept = 0;
        while slow.pop().is_some() {
            kept += 1;
        }
        assert_eq!(kept, QUEUE);
    }
}
