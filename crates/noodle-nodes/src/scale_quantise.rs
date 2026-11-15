//! The Scale Quantise node.

use noodle_engine::{
    Config, Context, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, Ports, Setup,
};

/// Snaps the notes that pass through it to a scale.
///
/// - `root` is the scale's first note (C to B).
/// - `scale` is which scale; its index is what the Song Key node's `scale`
///   output counts in, so wiring one into the other follows the song's key.
/// - `direction` is where a note outside the scale goes: *Nearest* (a tie
///   goes down), *Down* or *Up*.
///
/// A note's ID and velocity are kept, so its note-off and expression need
/// nothing from the node. The pitch is read at each note-on. Anything that
/// is not a note-on passes through.
pub struct ScaleQuantise;

pub const SCALE_QUANTISE_ID: &str = "noodle.event.scale_quantise";

pub const ROOT_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// The scales: a name and which of the twelve semitones above the root
/// belong to it (bit 0 is the root). Major is 0 and minor is 1, as the Song
/// Key node's `scale` counts.
pub const SCALES: [(&str, u16); 14] = [
    ("Major", mask(&[0, 2, 4, 5, 7, 9, 11])),
    ("Minor", mask(&[0, 2, 3, 5, 7, 8, 10])),
    ("Dorian", mask(&[0, 2, 3, 5, 7, 9, 10])),
    ("Phrygian", mask(&[0, 1, 3, 5, 7, 8, 10])),
    ("Lydian", mask(&[0, 2, 4, 6, 7, 9, 11])),
    ("Mixolydian", mask(&[0, 2, 4, 5, 7, 9, 10])),
    ("Locrian", mask(&[0, 1, 3, 5, 6, 8, 10])),
    ("Harmonic minor", mask(&[0, 2, 3, 5, 7, 8, 11])),
    ("Melodic minor", mask(&[0, 2, 3, 5, 7, 9, 11])),
    ("Major pentatonic", mask(&[0, 2, 4, 7, 9])),
    ("Minor pentatonic", mask(&[0, 3, 5, 7, 10])),
    ("Blues", mask(&[0, 3, 5, 6, 7, 10])),
    ("Whole tone", mask(&[0, 2, 4, 6, 8, 10])),
    ("Chromatic", mask(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11])),
];

const fn mask(degrees: &[u8]) -> u16 {
    let mut bits = 0;
    let mut i = 0;
    while i < degrees.len() {
        bits |= 1 << degrees[i];
        i += 1;
    }
    bits
}

#[derive(Ports)]
struct ScaleQuantisePorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[param("root", "Root", ParamInfo::choice(ROOT_NAMES))]
    root: (),
    #[param("scale", "Scale", ParamInfo::choice(SCALES.map(|(name, _)| name)))]
    scale: (),
    #[param(
        "direction",
        "Direction",
        ParamInfo::choice(["Nearest", "Down", "Up"])
    )]
    direction: (),
    #[event_output("out", "Notes")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: SCALE_QUANTISE_ID,
    version: 1,
    name: "Scale Quantise",
    category: "Events",
};

impl NodeType for ScaleQuantise {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(ScaleQuantisePorts::layout())
    }

    /// Notes pass straight through; nothing is kept from block to block.
    fn stateless(&self) -> bool {
        true
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ScaleQuantiseNode))
    }
}

struct ScaleQuantiseNode;

/// `key` moved onto the scale (`mask` from `root`), going `direction`
/// (0 nearest, 1 down, 2 up). A direction that would leave the MIDI range
/// goes the other way.
pub(crate) fn quantise(key: u8, root: u8, mask: u16, direction: u8) -> u8 {
    let degree = |k: i32| (k - i32::from(root)).rem_euclid(12);
    let inside = |k: i32| (0..=127).contains(&k) && mask & (1 << degree(k)) != 0;
    let key = i32::from(key);
    if inside(key) {
        return key as u8;
    }
    let down = (1..=12).find(|d| inside(key - d));
    let up = (1..=12).find(|d| inside(key + d));
    let moved = match (direction, down, up) {
        (1, Some(d), _) | (2, Some(d), None) => key - d,
        (2, _, Some(u)) | (1, None, Some(u)) => key + u,
        (_, Some(d), Some(u)) if d <= u => key - d,
        (_, _, Some(u)) => key + u,
        (_, Some(d), None) => key - d,
        // A scale with no notes: leave it be.
        (_, None, None) => key,
    };
    moved as u8
}

impl Node for ScaleQuantiseNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_inputs,
            event_outputs,
            ..
        } = io;
        let first = |port: usize| inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let root = first(ScaleQuantisePorts::ROOT).round().clamp(0.0, 11.0) as u8;
        let scale = first(ScaleQuantisePorts::SCALE)
            .round()
            .clamp(0.0, (SCALES.len() - 1) as f32) as usize;
        let direction = first(ScaleQuantisePorts::DIRECTION).round().clamp(0.0, 2.0) as u8;
        let mask = SCALES[scale].1;
        for event in event_inputs[ScaleQuantisePorts::NOTES] {
            let mut event = *event;
            if let EventKind::NoteOn { key, .. } = &mut event.kind {
                *key = quantise(*key, root, mask, direction);
            }
            let _ = event_outputs[ScaleQuantisePorts::OUT].push(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{Event, NoteId};

    fn on(id: u32, key: u8) -> Event {
        Event {
            time: 0,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity: 0.8,
            },
        }
    }

    fn off(id: u32) -> Event {
        Event {
            time: 0,
            kind: EventKind::NoteOff {
                note: NoteId(id),
                velocity: 0.0,
            },
        }
    }

    fn keys(h: &Harness) -> Vec<u8> {
        h.events(ScaleQuantisePorts::OUT)
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::NoteOn { key, .. } => Some(key),
                _ => None,
            })
            .collect()
    }

    fn rig(root: f32, scale: f32, direction: f32) -> Harness {
        let mut h = Harness::new(&ScaleQuantise, &Config::new(), &[], 48_000.0, 64).unwrap();
        h.set(ScaleQuantisePorts::ROOT, root);
        h.set(ScaleQuantisePorts::SCALE, scale);
        h.set(ScaleQuantisePorts::DIRECTION, direction);
        h
    }

    fn snap(h: &mut Harness, key: u8) -> u8 {
        h.send_events(ScaleQuantisePorts::NOTES, &[on(1, key)]);
        h.run(64).unwrap();
        keys(h)[0]
    }

    #[test]
    fn the_scale_masks_are_the_scales() {
        let steps = |mask: u16| -> Vec<u32> { (0..12).filter(|i| mask & (1 << i) != 0).collect() };
        let by_name = |name: &str| SCALES.iter().find(|s| s.0 == name).unwrap().1;
        assert_eq!(steps(by_name("Major")), [0, 2, 4, 5, 7, 9, 11]);
        assert_eq!(steps(by_name("Minor")), [0, 2, 3, 5, 7, 8, 10]);
        assert_eq!(steps(by_name("Dorian")), [0, 2, 3, 5, 7, 9, 10]);
        assert_eq!(steps(by_name("Phrygian")), [0, 1, 3, 5, 7, 8, 10]);
        assert_eq!(steps(by_name("Lydian")), [0, 2, 4, 6, 7, 9, 11]);
        assert_eq!(steps(by_name("Mixolydian")), [0, 2, 4, 5, 7, 9, 10]);
        assert_eq!(steps(by_name("Locrian")), [0, 1, 3, 5, 6, 8, 10]);
        assert_eq!(steps(by_name("Harmonic minor")), [0, 2, 3, 5, 7, 8, 11]);
        assert_eq!(steps(by_name("Melodic minor")), [0, 2, 3, 5, 7, 9, 11]);
        assert_eq!(steps(by_name("Major pentatonic")), [0, 2, 4, 7, 9]);
        assert_eq!(steps(by_name("Minor pentatonic")), [0, 3, 5, 7, 10]);
        assert_eq!(steps(by_name("Blues")), [0, 3, 5, 6, 7, 10]);
        assert_eq!(steps(by_name("Whole tone")), [0, 2, 4, 6, 8, 10]);
        assert_eq!(steps(by_name("Chromatic")).len(), 12);
    }

    #[test]
    fn notes_in_the_scale_stay_and_the_rest_snap_nearest_with_ties_down() {
        let mut h = rig(0.0, 0.0, 0.0);
        // C major: C#4 (61) is between C and D, a tie, so down; D#4 (63) too
        // is a tie between D and E, so D; F#4 (66) between F and G, so F.
        assert_eq!(snap(&mut h, 60), 60);
        assert_eq!(snap(&mut h, 61), 60);
        assert_eq!(snap(&mut h, 63), 62);
        assert_eq!(snap(&mut h, 66), 65);
        // A pentatonic scale is wider: F (65) is nearer E... the major
        // pentatonic has C D E G A, so F is a tie of E and G: down.
        let mut h = rig(0.0, 9.0, 0.0);
        assert_eq!(snap(&mut h, 65), 64);
        // B (71) is nearest A (2) against C (1): up.
        assert_eq!(snap(&mut h, 71), 72);
    }

    #[test]
    fn the_root_moves_the_scale_and_direction_picks_a_side() {
        // D major has F# and C#; F (65) is not in it.
        let mut h = rig(2.0, 0.0, 1.0);
        assert_eq!(snap(&mut h, 65), 64);
        let mut h = rig(2.0, 0.0, 2.0);
        assert_eq!(snap(&mut h, 65), 66);
    }

    #[test]
    fn a_direction_that_leaves_the_range_goes_the_other_way() {
        // Key 127 is G; in C major with Up nothing above is in range.
        // 126 is F#, Up would be 127 (G), fine; use C# major pentatonic
        // style: A scale where the top has no note above.
        assert_eq!(quantise(127, 0, 0b0000_0000_0001, 2), 120);
        assert_eq!(quantise(0, 1, 0b0000_0000_0001, 1), 1);
    }

    #[test]
    fn velocity_ids_and_offs_pass_and_a_parameter_change_strands_nothing() {
        let mut h = rig(0.0, 0.0, 0.0);
        h.send_events(ScaleQuantisePorts::NOTES, &[on(7, 61)]);
        h.run(64).unwrap();
        assert!(matches!(
            h.events(ScaleQuantisePorts::OUT)[0].kind,
            EventKind::NoteOn {
                note: NoteId(7),
                key: 60,
                velocity,
                ..
            } if velocity == 0.8
        ));
        // Change the scale while it is held; the off still names the note.
        h.set(ScaleQuantisePorts::SCALE, 13.0);
        h.send_events(ScaleQuantisePorts::NOTES, &[off(7)]);
        h.run(64).unwrap();
        assert_eq!(h.events(ScaleQuantisePorts::OUT), &[off(7)]);
    }
}
