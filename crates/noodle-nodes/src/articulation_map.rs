//! The Articulation Map node: turns the articulation a track plays a note
//! with into what the instrument understands.

use noodle_core::{ARTICULATION_MAP_ID, ARTICULATION_MAP_KEY};
use noodle_engine::{
    ChannelControl, Config, ConfigInfo, Context, Event, EventKind, Instance, Io, Layout, Node,
    NodeError, NodeInfo, NodeType, NoteId, Setup,
};

/// A note-wire transformer between a track's input and an instrument. A track
/// sends, ahead of each note whose articulation differs from the last it
/// sent, a change of articulation (and, after a jump, the articulation in
/// force at the playhead). This node reads it and asks the instrument for
/// that articulation the way the instrument wants to be asked:
///
/// - **Keyswitch**: a note on `key` at `velocity`, held for `length` ms, sent
///   just before the note it applies to.
/// - **Controller**: a controller set to `value` (for libraries that switch
///   on CC32 and the like).
/// - **Program**: a program change.
///
/// The table is config, one line per articulation: `<id> key <key> <velocity>
/// <length ms>`, `<id> cc <controller> <value>` or `<id> program <number>`,
/// with 0 to 127 for the key, velocity, value and program. An articulation
/// with no line sends nothing. The ID is the project's articulation ID, and 0
/// stands for "no articulation", so the line `0 key 24 100 50` is the switch
/// back to the default. The table keeps IDs rather than names, so renaming an
/// articulation in the project changes nothing here.
///
/// The change of articulation is consumed: it does not go on. Everything else
/// (notes, controllers, expression) passes through.
pub struct ArticulationMap;

const MAP: ConfigInfo = ConfigInfo::text(ARTICULATION_MAP_KEY, "Map");
static CONFIG: [ConfigInfo; 1] = [MAP];

static INFO: NodeInfo = NodeInfo {
    id: ARTICULATION_MAP_ID,
    version: 1,
    name: "Articulation Map",
    category: "Events",
};

/// What an articulation is asked for with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// A note on `key`, held `length_ms`.
    Keyswitch {
        key: u8,
        velocity: u8,
        length_ms: u16,
    },
    Controller {
        number: u8,
        value: u8,
    },
    Program(u8),
}

/// One line of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The project's articulation ID, or 0 for none.
    pub id: u16,
    pub action: Action,
}

/// The longest a keyswitch is held.
pub const MAX_KEYSWITCH_MS: u16 = 5000;

impl Entry {
    fn to_text(self) -> String {
        match self.action {
            Action::Keyswitch {
                key,
                velocity,
                length_ms,
            } => format!("{} key {key} {velocity} {length_ms}", self.id),
            Action::Controller { number, value } => format!("{} cc {number} {value}", self.id),
            Action::Program(n) => format!("{} program {n}", self.id),
        }
    }

    fn from_line(line: &str) -> Option<Entry> {
        let mut words = line.split_whitespace();
        let id = words.next()?.parse().ok()?;
        let kind = words.next()?;
        let mut number = || -> Option<u32> { words.next()?.parse().ok() };
        let action = match kind {
            "key" => Action::Keyswitch {
                key: number()?.min(127) as u8,
                velocity: number()?.min(127) as u8,
                length_ms: number()?.min(u32::from(MAX_KEYSWITCH_MS)) as u16,
            },
            "cc" => Action::Controller {
                number: number()?.min(127) as u8,
                value: number()?.min(127) as u8,
            },
            "program" => Action::Program(number()?.min(127) as u8),
            _ => return None,
        };
        Some(Entry { id, action })
    }
}

/// The table in the node's config text. A line that doesn't read is
/// dropped, and a repeated ID keeps its last line.
pub fn map_from_text(text: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for entry in text.lines().filter_map(Entry::from_line) {
        match entries.iter_mut().find(|e| e.id == entry.id) {
            Some(existing) => *existing = entry,
            None => entries.push(entry),
        }
    }
    entries
}

pub fn map_to_text(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|e| e.to_text())
        .collect::<Vec<_>>()
        .join("\n")
}

impl NodeType for ArticulationMap {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime()
            .event_input("in", "Notes")
            .event_output("out", "Notes"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ArticulationMapNode {
            map: map_from_text(&MAP.get_text(setup.config)),
            sample_rate: setup.sample_rate,
            held: [None; HELD],
        }))
    }
}

/// The keyswitches held at once. A seventeenth is not held: its note-on goes
/// out and its off at once.
const HELD: usize = 16;

/// Keyswitch notes have IDs of their own: all three top bits set, which no
/// other source of notes uses together.
const KEYSWITCH_ID: u32 = 0xE000_0000;

#[derive(Clone, Copy)]
struct Held {
    key: u8,
    /// Frames from the start of the block until the off.
    remaining: u32,
}

struct ArticulationMapNode {
    map: Vec<Entry>,
    sample_rate: f32,
    held: [Option<Held>; HELD],
}

impl ArticulationMapNode {
    fn off(key: u8, time: u32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOff {
                note: NoteId(KEYSWITCH_ID | u32::from(key)),
                velocity: 0.0,
            },
        }
    }
}

impl Node for ArticulationMapNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let Io {
            event_inputs,
            event_outputs,
            ..
        } = io;
        let out = &mut event_outputs[0];
        let ctx_frames = _ctx.frames as u32;
        for event in event_inputs[0] {
            let EventKind::Channel {
                control: ChannelControl::Articulation,
                value,
                ..
            } = event.kind
            else {
                let _ = out.insert(*event);
                continue;
            };
            let Some(entry) = self.map.iter().find(|e| u32::from(e.id) == value) else {
                continue;
            };
            let time = event.time;
            match entry.action {
                Action::Keyswitch {
                    key,
                    velocity,
                    length_ms,
                } => {
                    // The same key again lets go first.
                    if let Some(slot) = self
                        .held
                        .iter_mut()
                        .find(|h| h.is_some_and(|h| h.key == key))
                    {
                        *slot = None;
                        let _ = out.insert(Self::off(key, time));
                    }
                    let on = Event {
                        time,
                        kind: EventKind::NoteOn {
                            note: NoteId(KEYSWITCH_ID | u32::from(key)),
                            channel: 0,
                            key,
                            velocity: f32::from(velocity) / 127.0,
                        },
                    };
                    let _ = out.insert(on);
                    let length = (f32::from(length_ms) * 0.001 * self.sample_rate) as u32;
                    match self.held.iter_mut().find(|h| h.is_none()) {
                        Some(slot) => {
                            *slot = Some(Held {
                                key,
                                remaining: time + length.max(1),
                            });
                        }
                        None => {
                            let _ = out.insert(Self::off(key, time));
                        }
                    }
                }
                Action::Controller { number, value } => {
                    let _ = out.insert(Event {
                        time,
                        kind: EventKind::Channel {
                            channel: 0,
                            control: ChannelControl::Controller(number),
                            value: u32::from(value) << 25,
                        },
                    });
                }
                Action::Program(n) => {
                    let _ = out.insert(Event {
                        time,
                        kind: EventKind::Midi([0xC0, n, 0]),
                    });
                }
            }
        }
        for slot in &mut self.held {
            if let Some(held) = slot {
                if held.remaining < ctx_frames {
                    let _ = out.insert(Self::off(held.key, held.remaining));
                    *slot = None;
                } else {
                    held.remaining -= ctx_frames;
                }
            }
        }
    }

    /// A jump lets go of the keyswitches held, in the next block.
    fn reset(&mut self) {
        for held in self.held.iter_mut().flatten() {
            held.remaining = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::testing::Harness;

    fn harness(map: &str) -> Harness {
        let config = Config::new().with(ARTICULATION_MAP_KEY, Value::Text(map.into()));
        Harness::new(&ArticulationMap, &config, &[], 48_000.0, 64).unwrap()
    }

    fn switch(time: u32, id: u32) -> Event {
        Event {
            time,
            kind: EventKind::Channel {
                channel: 0,
                control: ChannelControl::Articulation,
                value: id,
            },
        }
    }

    fn note_on(time: u32, key: u8) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(1),
                channel: 0,
                key,
                velocity: 0.8,
            },
        }
    }

    fn kinds(h: &Harness) -> Vec<(u32, String)> {
        h.events(0)
            .iter()
            .map(|e| {
                let what = match e.kind {
                    EventKind::NoteOn { key, .. } => format!("on {key}"),
                    EventKind::NoteOff { note, .. } => format!("off {}", note.0 & 0x7F),
                    EventKind::Channel {
                        control: ChannelControl::Controller(n),
                        value,
                        ..
                    } => format!("cc {n} {}", value >> 25),
                    EventKind::Midi(m) => format!("midi {:02x} {}", m[0], m[1]),
                    _ => "other".into(),
                };
                (e.time, what)
            })
            .collect()
    }

    #[test]
    fn a_keyswitch_comes_just_before_the_note_and_is_let_go_after_its_length() {
        let mut h = harness("1 key 24 100 1\n2 cc 32 64");
        h.send_events(0, &[switch(10, 1), note_on(10, 60)]);
        h.run(64).unwrap();
        assert_eq!(
            kinds(&h),
            [
                (10, "on 24".into()),
                (10, "on 60".into()),
                // 1 ms is 48 frames.
                (58, "off 24".into()),
            ]
        );
    }

    #[test]
    fn a_keyswitch_held_past_the_block_is_let_go_in_the_next() {
        let mut h = harness("1 key 24 100 5");
        h.send_events(0, &[switch(0, 1)]);
        h.run(64).unwrap();
        assert_eq!(kinds(&h), [(0, "on 24".into())]);
        h.send_events(0, &[]);
        // 5 ms is 240 frames: 176 remain after the first block.
        h.run(64).unwrap();
        h.run(64).unwrap();
        assert!(kinds(&h).is_empty());
        h.run(64).unwrap();
        assert_eq!(kinds(&h), [(48, "off 24".into())]);
    }

    #[test]
    fn controllers_and_programs_are_sent_and_unmapped_articulations_say_nothing() {
        let mut h = harness("2 cc 32 64\n3 program 5");
        h.send_events(
            0,
            &[switch(1, 2), switch(2, 3), switch(3, 9), note_on(4, 60)],
        );
        h.run(64).unwrap();
        assert_eq!(
            kinds(&h),
            [
                (1, "cc 32 64".into()),
                (2, "midi c0 5".into()),
                (4, "on 60".into())
            ]
        );
    }

    #[test]
    fn the_default_can_be_mapped_too() {
        let mut h = harness("0 key 20 90 10");
        h.send_events(0, &[switch(0, 0)]);
        h.run(64).unwrap();
        assert_eq!(kinds(&h), [(0, "on 20".into())]);
    }

    #[test]
    fn a_jump_lets_go_of_a_held_keyswitch() {
        let mut h = harness("1 key 24 100 500");
        h.send_events(0, &[switch(0, 1)]);
        h.run(64).unwrap();
        h.send_events(0, &[]);
        h.seek(0);
        h.run(64).unwrap();
        assert_eq!(kinds(&h), [(0, "off 24".into())]);
    }

    #[test]
    fn the_table_round_trips_through_text() {
        let text = "0 key 20 90 10\n1 key 24 100 50\n2 cc 32 64\n3 program 5";
        assert_eq!(map_to_text(&map_from_text(text)), text);
        // Lines that don't read are dropped; the last of a repeated ID wins.
        let entries = map_from_text("x\n1 key 1 2\n1 program 3\n1 program 4");
        assert_eq!(
            entries,
            [Entry {
                id: 1,
                action: Action::Program(4)
            }]
        );
    }
}
