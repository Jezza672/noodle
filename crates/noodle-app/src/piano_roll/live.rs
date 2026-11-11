//! The notes being played now, from any MIDI input (a hardware port or the
//! computer keyboard), so the roll's keys can light up under them.

use std::collections::BTreeSet;

use noodle_io::{MidiBus, MidiReceiver};

/// Listens to the session's MIDI bus while the roll is open.
#[derive(Default)]
pub(super) struct Live {
    receiver: Option<MidiReceiver>,
    down: BTreeSet<u8>,
}

impl Live {
    /// Takes the notes that arrived since the last frame, and returns whether
    /// any are down.
    pub fn poll(&mut self, bus: &MidiBus) -> bool {
        let receiver = self.receiver.get_or_insert_with(|| bus.subscribe_notes());
        while let Some([status, key, velocity]) = receiver.pop() {
            match (status & 0xF0, velocity) {
                (0x90, 1..) => {
                    self.down.insert(key);
                }
                (0x80, _) | (0x90, 0) => {
                    self.down.remove(&key);
                }
                _ => {}
            }
        }
        !self.down.is_empty()
    }

    pub fn is_down(&self, key: u8) -> bool {
        self.down.contains(&key)
    }

    /// Stops listening, for when the roll closes.
    pub fn stop(&mut self) {
        self.receiver = None;
        self.down.clear();
    }
}
