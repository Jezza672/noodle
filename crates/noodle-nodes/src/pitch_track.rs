//! The Pitch Track and To Notes nodes: a hummed line or a bass becomes control
//! signals, and the signals become notes, live.
//!
//! Both are built on the same `noodle_io` pieces as Convert to MIDI (which
//! runs them over a recording): [`PitchTracker`] and [`NoteSegmenter`]. The
//! sensitivity is the one scale the slicer and the transient pins use too.

use noodle_engine::{
    Config, Context, Event, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    NoteId, ParamInfo, Ports, Setup, Shape,
};
use noodle_io::{HOP_SECONDS, NoteSegmenter, PitchFrame, PitchTracker};

pub const PITCH_TRACK_ID: &str = "noodle.analysis.pitch_track";
pub const TO_NOTES_ID: &str = "noodle.event.to_notes";

/// Follows the pitch of a single voice (a hum, a whistle, a bass line) and
/// puts it out as three control signals, one new value every 10 ms:
///
/// - `pitch`: the key number, fractional (69 is A 440, so 1 is a semitone).
///   Between sounds it holds the last pitch.
/// - `confidence`: 0 to 1, how sure the pitch is; 0 when there is no pitched
///   sound (silence, noise, a click).
/// - `level`: the level of the sound in decibels relative to full scale.
///
/// `sensitivity` (0 to 1) is how readily a rough sound counts as pitched. The
/// values describe the sound about 25 ms before it is heard (the tracker
/// listens to a 50 ms window). Stereo and multi-voice inputs are mixed to one.
/// Wire the signals to [To Notes](ToNotes) to play a synth from the voice, or
/// use them to modulate anything.
pub struct PitchTrack;

#[derive(Ports)]
struct PitchTrackPorts {
    #[input("in", "In")]
    input: (),
    #[param("sensitivity", "Sensitivity", ParamInfo::new(0.0, 1.0, 0.5))]
    sensitivity: (),
    #[control_output("pitch", "Pitch")]
    pitch: (),
    #[control_output("confidence", "Confidence")]
    confidence: (),
    #[control_output("level", "Level")]
    level: (),
}

static PITCH_INFO: NodeInfo = NodeInfo {
    id: PITCH_TRACK_ID,
    version: 1,
    name: "Pitch Track",
    category: "Analysis",
};

impl NodeType for PitchTrack {
    fn info(&self) -> &NodeInfo {
        &PITCH_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(PitchTrackPorts::layout())
    }

    fn output_shapes(
        &self,
        _config: &Config,
        layout: &Layout,
        _inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        Ok(vec![Shape::MONO; layout.outputs.len()])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PitchTrackNode {
            tracker: PitchTracker::new(setup.sample_rate as u32, 0.5),
            held: PitchFrame {
                key: None,
                confidence: 0.0,
                level_db: -120.0,
            },
            pitch: 0.0,
        }))
    }
}

struct PitchTrackNode {
    tracker: PitchTracker,
    held: PitchFrame,
    pitch: f32,
}

impl Node for PitchTrackNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let input = &io.inputs[PitchTrackPorts::INPUT];
        let shape = input.shape();
        let channels = shape.channels.max(1);
        let lanes = (shape.voices * channels) as f32;
        let sensitivity = io.inputs[PitchTrackPorts::SENSITIVITY]
            .lane(0, 0)
            .first()
            .copied()
            .unwrap_or(0.5);
        self.tracker.set_sensitivity(sensitivity);
        for i in 0..ctx.frames {
            let mut sum = 0.0;
            for v in 0..shape.voices {
                for c in 0..channels {
                    sum += input.lane(v, c)[i];
                }
            }
            let x = sum / lanes;
            if let Some(frame) = self.tracker.push(if x.is_finite() { x } else { 0.0 }) {
                self.held = frame;
                if let Some(key) = frame.key {
                    self.pitch = key;
                }
            }
            io.outputs[PitchTrackPorts::PITCH].lane_mut(0, 0)[i] = self.pitch;
            io.outputs[PitchTrackPorts::CONFIDENCE].lane_mut(0, 0)[i] = self.held.confidence;
            io.outputs[PitchTrackPorts::LEVEL].lane_mut(0, 0)[i] = self.held.level_db;
        }
    }

    fn reset(&mut self) {
        self.tracker.reset();
        self.held = PitchFrame {
            key: None,
            confidence: 0.0,
            level_db: -120.0,
        };
        self.pitch = 0.0;
    }
}

/// Plays notes from a pitch signal: it reads `pitch`, `confidence` and `level`
/// (the outputs of [Pitch Track](PitchTrack)) every 10 ms and sends a note on
/// when the pitch has held steady for 30 ms, a note off when the pitch is
/// lost, a new note when the pitch moves off by more than a semitone for 30
/// ms, and a restart when the level jumps (a repeated note). The velocity is
/// the peak level at the start of the note.
///
/// `sensitivity` (0 to 1) sets the quietest sound that counts and how small a
/// jump in level restarts a note; at 0.5 that is about -42 dB and 8 dB.
/// The notes come about 30 ms after the voice begins.
pub struct ToNotes;

#[derive(Ports)]
struct ToNotesPorts {
    #[input("pitch", "Pitch")]
    pitch: (),
    #[input("confidence", "Confidence")]
    confidence: (),
    #[input("level", "Level")]
    level: (),
    #[param("sensitivity", "Sensitivity", ParamInfo::new(0.0, 1.0, 0.5))]
    sensitivity: (),
    #[event_output("out", "Notes")]
    out: (),
}

static NOTES_INFO: NodeInfo = NodeInfo {
    id: TO_NOTES_ID,
    version: 1,
    name: "To Notes",
    category: "Events",
};

/// A pitch is trusted from this confidence up.
const MIN_CONFIDENCE: f32 = 0.3;

impl NodeType for ToNotes {
    fn info(&self) -> &NodeInfo {
        &NOTES_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(ToNotesPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ToNotesNode {
            hop: ((HOP_SECONDS * setup.sample_rate).round() as u32).max(1),
            until: 0,
            segmenter: NoteSegmenter::new(0.5),
            next_id: 0,
            sounding: None,
        }))
    }
}

struct ToNotesNode {
    hop: u32,
    /// Samples until the next frame is read.
    until: u32,
    segmenter: NoteSegmenter,
    next_id: u32,
    /// The id of the note sounding.
    sounding: Option<u32>,
}

impl ToNotesNode {
    fn note_off(&mut self, time: u32, out: &mut noodle_engine::EventsOut<'_>) {
        if let Some(id) = self.sounding.take() {
            let _ = out.push(Event {
                time,
                kind: EventKind::NoteOff {
                    note: NoteId(id),
                    velocity: 0.0,
                },
            });
        }
    }
}

impl Node for ToNotesNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_outputs,
            ..
        } = io;
        let out = &mut event_outputs[ToNotesPorts::OUT];
        let at = |port: usize, i: usize| inputs[port].lane(0, 0).get(i).copied().unwrap_or(0.0);
        let sensitivity = at(ToNotesPorts::SENSITIVITY, 0);
        self.segmenter.set_sensitivity(sensitivity);
        for i in 0..ctx.frames {
            if self.until > 0 {
                self.until -= 1;
                continue;
            }
            self.until = self.hop - 1;
            let confidence = at(ToNotesPorts::CONFIDENCE, i);
            let frame = PitchFrame {
                key: (confidence >= MIN_CONFIDENCE).then(|| at(ToNotesPorts::PITCH, i)),
                confidence,
                level_db: at(ToNotesPorts::LEVEL, i),
            };
            let step = self.segmenter.step(frame, None);
            let time = i as u32;
            if step.off.is_some() {
                self.note_off(time, out);
            }
            if let Some((key, velocity)) = step.on {
                self.note_off(time, out);
                let id = self.next_id;
                self.next_id = self.next_id.wrapping_add(1);
                self.sounding = Some(id);
                let _ = out.push(Event {
                    time,
                    kind: EventKind::NoteOn {
                        note: NoteId(id),
                        channel: 0,
                        key,
                        velocity: f32::from(velocity) / 127.0,
                    },
                });
            }
        }
    }

    fn reset(&mut self) {
        // A seek starts afresh; the sensitivity is read again next block.
        self.segmenter = NoteSegmenter::new(0.5);
        self.until = 0;
        self.next_id = 0;
        self.sounding = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use std::f32::consts::TAU;

    const RATE: f32 = 48_000.0;
    const BLOCK: usize = 480;

    fn voice(hz: f32, from: usize, frames: usize) -> Vec<f32> {
        (from..from + frames)
            .map(|i| {
                let t = i as f32 / RATE;
                0.3 * (TAU * hz * t).sin() + 0.15 * (TAU * 2.0 * hz * t).sin()
            })
            .collect()
    }

    fn feed(h: &mut Harness, x: &[f32]) -> Vec<Vec<f32>> {
        let mut seen = vec![Vec::new(), Vec::new(), Vec::new()];
        for chunk in x.chunks(BLOCK) {
            h.input(PitchTrackPorts::INPUT, chunk.len())
                .lane_mut(0, 0)
                .copy_from_slice(chunk);
            h.run(chunk.len()).unwrap();
            for (p, s) in [
                PitchTrackPorts::PITCH,
                PitchTrackPorts::CONFIDENCE,
                PitchTrackPorts::LEVEL,
            ]
            .into_iter()
            .zip(&mut seen)
            {
                s.extend_from_slice(h.output(p).lane(0, 0));
            }
        }
        seen
    }

    fn tracker() -> Harness {
        Harness::new(
            &PitchTrack,
            &Config::new(),
            &[(PitchTrackPorts::INPUT, Shape::MONO)],
            RATE,
            BLOCK,
        )
        .unwrap()
    }

    #[test]
    fn pitch_track_puts_out_the_key_confidence_and_level() {
        let mut h = tracker();
        let [pitch, confidence, level] = feed(&mut h, &voice(220.0, 0, 24_000)).try_into().unwrap();
        let end = pitch.len() - 1;
        assert!((pitch[end] - 57.0).abs() < 0.2, "{}", pitch[end]);
        assert!(confidence[end] > 0.8, "{}", confidence[end]);
        assert!(level[end] < -10.0 && level[end] > -25.0, "{}", level[end]);
    }

    #[test]
    fn silence_has_no_confidence_and_holds_the_last_pitch() {
        let mut h = tracker();
        let mut x = voice(220.0, 0, 12_000);
        x.extend(vec![0.0; 12_000]);
        let [pitch, confidence, _] = feed(&mut h, &x).try_into().unwrap();
        let end = pitch.len() - 1;
        assert_eq!(confidence[end], 0.0);
        assert!((pitch[end] - 57.0).abs() < 0.3, "{}", pitch[end]);
    }

    #[test]
    fn to_notes_plays_a_note_for_a_held_pitch_and_ends_it_with_the_sound() {
        let shapes = [
            (ToNotesPorts::PITCH, Shape::MONO),
            (ToNotesPorts::CONFIDENCE, Shape::MONO),
            (ToNotesPorts::LEVEL, Shape::MONO),
        ];
        let mut t = tracker();
        let mut n = Harness::new(&ToNotes, &Config::new(), &shapes, RATE, BLOCK).unwrap();
        let mut x = vec![0.0; 4_800];
        x.extend(voice(220.0, 0, 24_000));
        x.extend(vec![0.0; 9_600]);
        let mut played: Vec<(usize, EventKind)> = Vec::new();
        for (b, chunk) in x.chunks(BLOCK).enumerate() {
            t.input(PitchTrackPorts::INPUT, chunk.len())
                .lane_mut(0, 0)
                .copy_from_slice(chunk);
            t.run(chunk.len()).unwrap();
            for (src, dst) in [
                (PitchTrackPorts::PITCH, ToNotesPorts::PITCH),
                (PitchTrackPorts::CONFIDENCE, ToNotesPorts::CONFIDENCE),
                (PitchTrackPorts::LEVEL, ToNotesPorts::LEVEL),
            ] {
                let values = t.output(src).lane(0, 0).to_vec();
                n.input(dst, chunk.len())
                    .lane_mut(0, 0)
                    .copy_from_slice(&values);
            }
            n.run(chunk.len()).unwrap();
            for e in n.events(ToNotesPorts::OUT) {
                played.push((b * BLOCK + e.time as usize, e.kind));
            }
        }
        assert_eq!(played.len(), 2, "{played:?}");
        match played[0] {
            (at, EventKind::NoteOn { key, velocity, .. }) => {
                assert_eq!(key, 57);
                assert!(velocity > 0.3);
                // After the voice starts at 4800, within about 80 ms.
                assert!((4_800..8_800).contains(&at), "{at}");
            }
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(played[1].1, EventKind::NoteOff { .. }),
            "{played:?}"
        );
        assert!(played[1].0 > 28_800 && played[1].0 < 33_000, "{played:?}");
    }
}
