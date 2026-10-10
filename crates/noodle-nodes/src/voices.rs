//! The Voices node: turns note events into polyphonic pitch, gate and
//! velocity signals, allocating a voice to each note and stealing one when
//! they run out.

use noodle_engine::{
    Config, ConfigInfo, Context, Event, EventKind, Expression, Instance, Io, Layout, Node,
    NodeError, NodeInfo, NodeType, ParamInfo, Ports, Setup, Shape, Unit,
};

/// Plays up to `voices` notes at once. Each note gets a voice, and the
/// outputs are polyphonic signals with one lane per voice:
///
/// - `pitch` in hertz, which includes a pitch expression's bend. It holds
///   after the note ends, so an envelope's release stays in tune.
/// - `gate`, high while the voice's note is held.
/// - `velocity` of the voice's last note.
///
/// **Allocation.** A new note takes a free voice, preferring the one that
/// has been free longest, so releases ring out as long as possible. With
/// none free, it steals the voice whose note started first. A stolen voice's
/// gate drops for one sample before it rises again, so envelopes restart.
/// Stealing is hard for now: the pitch changes at once, with no fade-out.
///
/// **Finished voices.** A voice that has been free for longer than `tail`
/// seconds is inactive: its three outputs are exactly 0 and flagged silent, so
/// oscillators and everything else fed by the voice skip it until a note takes
/// it. `tail` should be at least as long as the longest envelope release in
/// the patch, since a voice stops sounding when it goes inactive. (The
/// envelope can't tell the Voices node when it is done, because that would
/// be a feedback wire.) A voice that isn't ringing also flags its `gate`
/// silent for the blocks in which it stays low.
///
/// The voice count is config, not a parameter, since it is the outputs'
/// shape.
pub struct Voices;

pub const VOICES_ID: &str = "noodle.poly.voices";

#[derive(Ports)]
struct VoicesPorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[param(
        "tail",
        "Tail",
        ParamInfo::new(0.0, 30.0, 10.0).unit(Unit::Seconds)
    )]
    tail: (),
    #[output("pitch", "Pitch")]
    pitch: (),
    #[output("gate", "Gate")]
    gate: (),
    #[output("velocity", "Velocity")]
    velocity: (),
}

const VOICE_COUNT: ConfigInfo = ConfigInfo::int("voices", "Voices", 8);
const MAX_VOICES: usize = 64;

static CONFIG: [ConfigInfo; 1] = [VOICE_COUNT];

static INFO: NodeInfo = NodeInfo {
    id: VOICES_ID,
    version: 1,
    name: "Voices",
    category: "Polyphony",
};

impl Voices {
    fn count(config: &Config) -> Result<usize, NodeError> {
        let voices = VOICE_COUNT.get_int(config);
        match usize::try_from(voices) {
            Ok(n @ 1..=MAX_VOICES) => Ok(n),
            _ => Err(NodeError::config(format!(
                "Voices needs between 1 and {MAX_VOICES} voices, not {voices}"
            ))),
        }
    }
}

impl NodeType for Voices {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        Self::count(config)?;
        Ok(VoicesPorts::layout())
    }

    fn output_shapes(
        &self,
        config: &Config,
        layout: &Layout,
        _inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        Ok(vec![
            Shape::new(Self::count(config)?, 1);
            layout.outputs.len()
        ])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(VoicesNode {
            voices: vec![Voice::default(); Self::count(setup.config)?],
            clock: 0,
        }))
    }
}

#[derive(Clone, Copy, Default)]
struct Voice {
    /// The note it plays or last played.
    id: u32,
    key: u8,
    velocity: f32,
    /// A pitch expression, in semitones.
    bend: f32,
    held: bool,
    /// Taken by a note and not yet free for `tail` seconds. An inactive voice
    /// is silent.
    active: bool,
    /// Frames since the note ended.
    since_off: u64,
    /// The voice's outputs were silent at the start of this block and no note
    /// has touched it since, so the flags can say so.
    quiet: bool,
    /// The same for the gate alone, which falls the moment a note ends.
    gate_quiet: bool,
    /// The gate must fall for the next sample, so the note restarts.
    rearm: bool,
    /// When the note started, or (once released) ended, in allocation order.
    /// Never-used voices have 0 and so are taken first.
    stamp: u64,
}

impl Voice {
    fn pitch(&self) -> f32 {
        let semitones = f32::from(self.key) + self.bend - 69.0;
        440.0 * (semitones / 12.0).exp2()
    }
}

struct VoicesNode {
    voices: Vec<Voice>,
    clock: u64,
}

impl VoicesNode {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// The voice a new note takes.
    fn allocate(&self) -> usize {
        let by_stamp = |held: bool| {
            self.voices
                .iter()
                .enumerate()
                .filter(|(_, v)| v.held == held)
                .min_by_key(|(_, v)| v.stamp)
                .map(|(i, _)| i)
        };
        by_stamp(false)
            .or_else(|| by_stamp(true))
            .expect("there is at least one voice")
    }

    fn apply(&mut self, kind: EventKind) {
        match kind {
            EventKind::NoteOn {
                note,
                key,
                velocity,
                ..
            } => {
                // A key that is already down (a retrigger, or two sources on
                // one port) retakes its own voice rather than taking a second.
                let i = self
                    .voices
                    .iter()
                    .position(|v| v.held && v.id == note.0)
                    .unwrap_or_else(|| self.allocate());
                let stamp = self.tick();
                let voice = &mut self.voices[i];
                voice.rearm = voice.held;
                *voice = Voice {
                    id: note.0,
                    key,
                    velocity,
                    bend: 0.0,
                    held: true,
                    active: true,
                    since_off: 0,
                    quiet: false,
                    gate_quiet: false,
                    rearm: voice.rearm,
                    stamp,
                };
            }
            EventKind::NoteOff { note, .. } => {
                let stamp = self.tick();
                if let Some(voice) = self.voices.iter_mut().find(|v| v.held && v.id == note.0) {
                    voice.held = false;
                    voice.rearm = false;
                    voice.stamp = stamp;
                    voice.since_off = 0;
                    voice.gate_quiet = false;
                }
            }
            EventKind::Expression {
                note,
                expression: Expression::Pitch,
                value,
            } => {
                for voice in self.voices.iter_mut().filter(|v| v.held && v.id == note.0) {
                    voice.bend = value;
                }
            }
            _ => {}
        }
    }

    /// Writes the voices' current state to frames `start..end`.
    fn fill(&mut self, io: &mut [noodle_engine::SignalOut<'_>], start: usize, end: usize) {
        if start >= end {
            return;
        }
        let [pitch, gate, velocity] = io
            .get_disjoint_mut([VoicesPorts::PITCH, VoicesPorts::GATE, VoicesPorts::VELOCITY])
            .expect("voices outputs are distinct");
        for (i, voice) in self.voices.iter_mut().enumerate() {
            pitch.lane_mut(i, 0)[start..end].fill(if voice.active { voice.pitch() } else { 0.0 });
            let gate = &mut gate.lane_mut(i, 0)[start..end];
            gate.fill(if voice.held { 1.0 } else { 0.0 });
            if voice.rearm {
                gate[0] = 0.0;
                voice.rearm = false;
            }
            let velocity_now = if voice.active { voice.velocity } else { 0.0 };
            velocity.lane_mut(i, 0)[start..end].fill(velocity_now);
        }
    }

    /// Before a block's events: what the flags may claim for it.
    fn begin_block(&mut self) {
        for voice in &mut self.voices {
            voice.quiet = !voice.active;
            voice.gate_quiet = !voice.held && !voice.rearm;
        }
    }

    /// After a block's events: flags the lanes that stayed silent, and makes
    /// the voices that have been free for `tail` inactive.
    fn end_block(&mut self, io: &mut [noodle_engine::SignalOut<'_>], frames: usize, tail: u64) {
        for (i, voice) in self.voices.iter_mut().enumerate() {
            if voice.quiet {
                for output in io.iter_mut() {
                    output.set_silent(i, 0);
                }
            } else if voice.gate_quiet {
                io[VoicesPorts::GATE].set_silent(i, 0);
            }
            if voice.active && !voice.held {
                voice.since_off = voice.since_off.saturating_add(frames as u64);
                if voice.since_off >= tail {
                    voice.active = false;
                }
            }
        }
    }
}

impl Node for VoicesNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let events: &[Event] = io.event_inputs[VoicesPorts::NOTES];
        let tail = io.inputs[VoicesPorts::TAIL]
            .lane(0, 0)
            .first()
            .copied()
            .unwrap_or(0.0);
        let tail = (f64::from(tail.max(0.0)) * f64::from(ctx.sample_rate)) as u64;
        let outputs = io.outputs;
        self.begin_block();
        let mut cursor = 0;
        for event in events {
            let time = (event.time as usize).min(ctx.frames);
            self.fill(outputs, cursor, time);
            cursor = cursor.max(time);
            self.apply(event.kind);
        }
        self.fill(outputs, cursor, ctx.frames);
        self.end_block(outputs, ctx.frames, tail);
    }

    fn reset(&mut self) {
        self.voices.fill(Voice::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::NoteId;
    use noodle_engine::testing::Harness;

    fn on(time: u32, id: u32, key: u8, velocity: f32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity,
            },
        }
    }

    fn off(time: u32, id: u32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOff {
                note: NoteId(id),
                velocity: 0.0,
            },
        }
    }

    fn voices(count: i64) -> Harness {
        let mut config = Config::new();
        config.set("voices", noodle_core::Value::Int(count));
        Harness::new(&Voices, &config, &[], 48_000.0, 16).unwrap()
    }

    fn gate(h: &Harness, voice: usize) -> Vec<f32> {
        h.output(VoicesPorts::GATE).lane(voice, 0).to_vec()
    }

    fn pitch(h: &Harness, voice: usize, frame: usize) -> f32 {
        h.output(VoicesPorts::PITCH).lane(voice, 0)[frame]
    }

    #[test]
    fn outputs_have_one_lane_per_voice() {
        let h = voices(5);
        for port in [VoicesPorts::PITCH, VoicesPorts::GATE, VoicesPorts::VELOCITY] {
            assert_eq!(h.output(port).shape(), Shape::new(5, 1));
        }
    }

    #[test]
    fn rejects_a_bad_voice_count() {
        for count in [0, 65, -1] {
            let mut config = Config::new();
            config.set("voices", noodle_core::Value::Int(count));
            assert!(Voices.layout(&config).is_err(), "{count}");
        }
    }

    #[test]
    fn each_note_takes_its_own_voice() {
        let mut h = voices(4);
        h.send_events(
            VoicesPorts::NOTES,
            &[on(2, 1, 69, 0.5), on(4, 2, 81, 0.7), off(8, 1)],
        );
        h.run(16).unwrap();
        let g0 = gate(&h, 0);
        assert_eq!(&g0[..2], &[0.0, 0.0]);
        assert!(g0[2..8].iter().all(|&g| g == 1.0));
        assert!(g0[8..].iter().all(|&g| g == 0.0));
        assert!(gate(&h, 1)[4..].iter().all(|&g| g == 1.0));
        assert!(gate(&h, 2).iter().all(|&g| g == 0.0));
        assert!((pitch(&h, 0, 3) - 440.0).abs() < 1e-3);
        assert!((pitch(&h, 1, 5) - 880.0).abs() < 1e-2);
        // The released voice keeps its pitch, and velocities stay per voice.
        assert!((pitch(&h, 0, 15) - 440.0).abs() < 1e-3);
        assert_eq!(h.output(VoicesPorts::VELOCITY).lane(0, 0)[10], 0.5);
        assert_eq!(h.output(VoicesPorts::VELOCITY).lane(1, 0)[10], 0.7);
    }

    #[test]
    fn a_new_note_takes_the_voice_free_longest() {
        let mut h = voices(3);
        h.send_events(
            VoicesPorts::NOTES,
            &[
                on(0, 1, 60, 1.0),
                on(0, 2, 62, 1.0),
                off(2, 2),
                off(4, 1),
                on(6, 3, 64, 1.0),
            ],
        );
        h.run(16).unwrap();
        // Voice 2 never played; then voice 1 was released before voice 0, so
        // the third note goes to the never-used voice 2.
        assert!(gate(&h, 2)[6..].iter().all(|&g| g == 1.0));
        h.send_events(VoicesPorts::NOTES, &[on(0, 4, 65, 1.0)]);
        h.run(16).unwrap();
        // Of the free voices (0 and 1), 1 was freed first.
        assert!(gate(&h, 1).iter().all(|&g| g == 1.0));
        assert!(gate(&h, 0).iter().all(|&g| g == 0.0));
    }

    #[test]
    fn a_full_set_steals_the_oldest_note_and_retriggers() {
        let mut h = voices(2);
        h.send_events(
            VoicesPorts::NOTES,
            &[on(0, 1, 60, 1.0), on(1, 2, 62, 1.0), on(6, 3, 72, 1.0)],
        );
        h.run(16).unwrap();
        let g0 = gate(&h, 0);
        // Voice 0 held note 1; the steal drops its gate for one sample.
        assert_eq!(g0[5], 1.0);
        assert_eq!(g0[6], 0.0);
        assert!(g0[7..].iter().all(|&g| g == 1.0));
        assert!((pitch(&h, 0, 7) - 523.25).abs() < 0.1);
        assert!((pitch(&h, 1, 15) - 293.66).abs() < 0.1);
        // The stolen note's own release does nothing.
        h.send_events(VoicesPorts::NOTES, &[off(0, 1)]);
        h.run(16).unwrap();
        assert!(gate(&h, 0).iter().all(|&g| g == 1.0));
    }

    #[test]
    fn a_steal_in_a_blocks_last_frame_still_retriggers() {
        let mut h = voices(1);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 60, 1.0), on(15, 2, 64, 1.0)]);
        h.run(16).unwrap();
        // The gate drops on the steal's frame and is back by the next block.
        assert_eq!(gate(&h, 0)[14], 1.0);
        assert_eq!(gate(&h, 0)[15], 0.0);
        h.send_events(VoicesPorts::NOTES, &[]);
        h.run(16).unwrap();
        assert!(gate(&h, 0).iter().all(|&g| g == 1.0));
    }

    #[test]
    fn a_repeated_note_on_retakes_its_voice_instead_of_sticking() {
        let mut h = voices(4);
        h.send_events(
            VoicesPorts::NOTES,
            &[on(0, 7, 60, 0.5), on(2, 7, 64, 0.9), off(8, 7)],
        );
        h.run(16).unwrap();
        for voice in 0..4 {
            assert_eq!(*gate(&h, voice).last().unwrap(), 0.0, "voice {voice}");
        }
        // The second note-on restarted voice 0 with its own key and velocity.
        let g0 = gate(&h, 0);
        assert_eq!((g0[1], g0[2], g0[3]), (1.0, 0.0, 1.0));
        assert!((pitch(&h, 0, 3) - 261.63 * 1.2599).abs() < 0.1);
        assert_eq!(h.output(VoicesPorts::VELOCITY).lane(0, 0)[3], 0.9);
        assert!(gate(&h, 1).iter().all(|&g| g == 0.0));
    }

    #[test]
    fn pitch_expression_bends_only_its_note() {
        let mut h = voices(2);
        let bend = Event {
            time: 4,
            kind: EventKind::Expression {
                note: NoteId(1),
                expression: Expression::Pitch,
                value: 12.0,
            },
        };
        h.send_events(
            VoicesPorts::NOTES,
            &[on(0, 1, 69, 1.0), on(0, 2, 69, 1.0), bend],
        );
        h.run(16).unwrap();
        assert!((pitch(&h, 0, 3) - 440.0).abs() < 1e-3);
        assert!((pitch(&h, 0, 4) - 880.0).abs() < 1e-2);
        assert!((pitch(&h, 1, 15) - 440.0).abs() < 1e-3);
    }

    fn silent(h: &Harness, port: usize, voice: usize) -> bool {
        h.output(port).is_silent(voice, 0)
    }

    #[test]
    fn unused_voices_are_flagged_silent_and_a_note_wakes_its_voice() {
        let mut h = voices(3);
        h.run(16).unwrap();
        for port in [VoicesPorts::PITCH, VoicesPorts::GATE, VoicesPorts::VELOCITY] {
            for voice in 0..3 {
                assert!(silent(&h, port, voice), "port {port} voice {voice}");
                assert!(h.output(port).lane(voice, 0).iter().all(|&x| x == 0.0));
            }
        }

        h.send_events(VoicesPorts::NOTES, &[on(4, 1, 69, 0.5)]);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[]);
        // The note starts mid-block: the voice isn't claimed to be silent.
        for port in [VoicesPorts::PITCH, VoicesPorts::GATE, VoicesPorts::VELOCITY] {
            assert!(!silent(&h, port, 0), "port {port}");
        }
        assert!((pitch(&h, 0, 8) - 440.0).abs() < 1e-3);
        // The others still are.
        assert!(silent(&h, VoicesPorts::PITCH, 1));
        assert!(silent(&h, VoicesPorts::GATE, 2));
    }

    #[test]
    fn a_released_voice_keeps_its_pitch_until_the_tail_has_passed() {
        let mut h = voices(2);
        // 0.01 s at 48 kHz is 480 frames: 30 blocks of 16.
        h.set(VoicesPorts::TAIL, 0.01);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 69, 0.5), off(8, 1)]);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[]);

        // Gate fell mid-block, so it isn't flagged then; next block it is.
        assert!(!silent(&h, VoicesPorts::GATE, 0));
        h.run(16).unwrap();
        assert!(silent(&h, VoicesPorts::GATE, 0), "low all block");
        assert!(!silent(&h, VoicesPorts::PITCH, 0), "still ringing");
        assert!((pitch(&h, 0, 3) - 440.0).abs() < 1e-3);

        for _ in 0..40 {
            h.run(16).unwrap();
        }
        assert!(silent(&h, VoicesPorts::PITCH, 0), "tail over");
        assert_eq!(pitch(&h, 0, 3), 0.0);
        assert!(silent(&h, VoicesPorts::VELOCITY, 0));
    }

    #[test]
    fn a_voice_held_for_longer_than_the_tail_is_never_inactive() {
        let mut h = voices(1);
        h.set(VoicesPorts::TAIL, 0.0);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 60, 0.5)]);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[]);
        for _ in 0..20 {
            h.run(16).unwrap();
            assert!(!silent(&h, VoicesPorts::PITCH, 0));
            assert!(pitch(&h, 0, 0) > 200.0);
        }
    }

    #[test]
    fn an_inactive_voice_that_is_retaken_in_the_same_block_as_a_steal_stays_correct() {
        let mut h = voices(1);
        h.set(VoicesPorts::TAIL, 0.0);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 60, 0.5), off(4, 1)]);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[]);
        h.run(16).unwrap();
        assert!(silent(&h, VoicesPorts::PITCH, 0));
        h.send_events(
            VoicesPorts::NOTES,
            &[on(0, 2, 72, 0.9), off(8, 2), on(12, 3, 48, 0.4)],
        );
        h.run(16).unwrap();
        assert!(!silent(&h, VoicesPorts::PITCH, 0));
        assert!(!silent(&h, VoicesPorts::GATE, 0));
        assert!((pitch(&h, 0, 2) - 523.25).abs() < 0.1);
        assert!((pitch(&h, 0, 14) - 130.81).abs() < 0.1);
    }
}
