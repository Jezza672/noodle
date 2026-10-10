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
use std::sync::{Arc, Mutex};

use midir::{Ignore, MidiInput, MidiInputConnection};
use rtrb::{Consumer, Producer, RingBuffer};

/// One MIDI message of up to three bytes: a status byte and its data.
/// Messages with one data byte (program change, channel pressure) have a
/// zero in the last.
pub type MidiMessage = [u8; 3];

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

/// Carries MIDI messages from whatever produces them to the MIDI In nodes.
/// Clones share the same subscribers.
#[derive(Clone, Default)]
pub struct MidiBus {
    subscribers: Arc<Mutex<Vec<Producer<MidiMessage>>>>,
}

impl MidiBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new receiver of every message sent from now on. Dropping it
    /// unsubscribes.
    pub fn subscribe(&self) -> MidiReceiver {
        let (producer, consumer) = RingBuffer::new(QUEUE);
        self.subscribers
            .lock()
            .expect("subscribers lock")
            .push(producer);
        MidiReceiver(consumer)
    }

    /// Hands a message to every receiver. One that has fallen `QUEUE`
    /// messages behind misses it.
    pub fn send(&self, message: MidiMessage) {
        let mut subscribers = self.subscribers.lock().expect("subscribers lock");
        subscribers.retain(|p| !p.is_abandoned());
        for producer in subscribers.iter_mut() {
            let _ = producer.push(message);
        }
    }

    /// Releases every note on every channel (controller 123), for when the
    /// port that held them down goes away.
    pub fn all_notes_off(&self) {
        for channel in 0..16 {
            self.send([0xB0 | channel, 123, 0]);
        }
    }

    /// How many receivers are listening.
    pub fn receivers(&self) -> usize {
        let mut subscribers = self.subscribers.lock().expect("subscribers lock");
        subscribers.retain(|p| !p.is_abandoned());
        subscribers.len()
    }
}

/// The audio thread's end of a [`MidiBus`] subscription.
pub struct MidiReceiver(Consumer<MidiMessage>);

impl MidiReceiver {
    /// The oldest message not yet taken. Never blocks or allocates.
    pub fn pop(&mut self) -> Option<MidiMessage> {
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
    let connection = input
        .connect(
            &port,
            "noodle",
            move |_, bytes, _| {
                if let Some(message) = parse_message(bytes) {
                    bus.send(message);
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
    input.ignore(Ignore::All);
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
    fn alsa_numbers_are_left_out_of_port_names() {
        assert!(same_port("Keys MIDI 1 24:0", "Keys MIDI 1 28:0"));
        assert!(same_port("Keys", "Keys"));
        assert!(!same_port("Keys MIDI 1 24:0", "Pads MIDI 1 24:0"));
        assert!(!same_port("Keys 1", "Keys 2"));
        assert!(!same_port("Keys 24:0", "Pads 24:0"));
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
