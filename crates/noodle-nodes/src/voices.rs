//! The Voices node: turns note events into polyphonic pitch, gate and
//! velocity signals, allocating a voice to each note and stealing one when
//! they run out.

use noodle_engine::{
    Config, ConfigInfo, Context, Event, EventKind, Expression, Instance, Io, Layout, Node,
    NodeError, NodeInfo, NodeType, ParamInfo, Ports, Setup, Shape, ShapeError, SignalIn, SignalOut,
    Unit,
};

/// Plays up to `voices` notes at once. Each note gets a voice, and the
/// outputs are polyphonic signals with one lane per voice:
///
/// - `pitch` in hertz, which includes a pitch expression's bend. It holds
///   after the note ends, so an envelope's release stays in tune.
/// - `gate`, high while the voice's note is held.
/// - `velocity` of the voice's last note.
/// - `fade`, a gain that is 1 while a voice plays and dips to 0 and back
///   around a steal (see below). Multiply it into the voice, after the
///   envelope (a VCA with `fade` as its level).
///
/// **Allocation.** A new note takes a free voice, preferring the one that
/// has been free longest, so releases ring out as long as possible. With
/// none free, it steals the voice whose note started first.
///
/// **Stealing.** With `steal_fade` above 0, a stolen voice keeps playing its
/// old note while `fade` ramps down over that time, then switches to the new
/// note with the gate dropping for one sample (so the envelope restarts) as
/// `fade` ramps back up. The new note therefore starts `steal_fade` late.
/// With 0 the steal is hard: the pitch changes at once, which can click.
///
/// **Finished voices.** A voice that has been free for longer than `tail`
/// seconds is inactive: its outputs are exactly 0 and flagged silent, so
/// oscillators and everything else fed by the voice skip it until a note takes
/// it. A voice stops sounding when it goes inactive, so there are two ways to
/// say when it is done:
///
/// - Wire the envelope's `active` output into `busy`. A voice that has been
///   busy since its note began goes inactive the block its envelope finishes,
///   however long the release was. This is a feedback loop (the envelope
///   follows the voice), which `busy` allows: Voices writes its outputs first
///   and reads `busy` afterwards, in the same block.
/// - Without `busy`, set `tail` to at least the longest release in the patch.
///
/// `tail` also caps `busy`: a voice is retired after `tail` whatever `busy`
/// says, in case something holds it high.
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
        ParamInfo::new(MIN_TAIL, 30.0, 10.0).unit(Unit::Seconds)
    )]
    tail: (),
    #[param(
        "steal_fade",
        "Steal fade",
        ParamInfo::new(0.0, 0.05, 0.005).unit(Unit::Seconds)
    )]
    steal_fade: (),
    #[input("busy", "Busy")]
    busy: (),
    #[output("pitch", "Pitch")]
    pitch: (),
    #[output("gate", "Gate")]
    gate: (),
    #[output("velocity", "Velocity")]
    velocity: (),
    #[output("fade", "Fade")]
    fade: (),
}

/// The shortest tail. A voice that goes inactive the moment its note ends would
/// cut every release, so there is always a little.
const MIN_TAIL: f32 = 0.05;

const VOICE_COUNT: ConfigInfo = ConfigInfo::int("voices", "Voices", 8);
const MAX_VOICES: usize = 64;

static CONFIG: [ConfigInfo; 1] = [VOICE_COUNT];

static INFO: NodeInfo = NodeInfo {
    id: VOICES_ID,
    version: 2,
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

    fn loop_input(&self, _config: &Config) -> Option<&'static str> {
        Some("busy")
    }

    fn output_shapes(
        &self,
        config: &Config,
        layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let shape = Shape::new(Self::count(config)?, 1);
        // `busy` is read a lane per voice, so it must be one signal for all
        // voices or one per voice.
        let busy = inputs[VoicesPorts::BUSY];
        if busy.voices != 1 && busy.voices != shape.voices {
            return Err(ShapeError(shape, busy).into());
        }
        Ok(vec![shape; layout.outputs.len()])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(VoicesNode {
            voices: vec![Voice::default(); Self::count(setup.config)?],
            clock: 0,
            tail: 0,
        }))
    }
}

/// A note waiting for a stolen voice to fade out.
#[derive(Clone, Copy, Default)]
struct Pending {
    key: u8,
    velocity: f32,
    bend: f32,
    /// Its note ended before the voice switched to it.
    released: bool,
}

/// Where a voice is in a steal.
#[derive(Clone, Copy, Default)]
enum Fade {
    #[default]
    Steady,
    /// Playing the old note, `fade` ramping down: `pos` of `len` samples done.
    Out { pos: u32, len: u32, next: Pending },
    /// Playing the new note, `fade` ramping up.
    In { pos: u32, len: u32 },
}

impl Fade {
    /// The gain now.
    fn level(&self) -> f32 {
        match *self {
            Self::Steady => 1.0,
            Self::Out { pos, len, .. } => (len - pos.min(len)) as f32 / len as f32,
            Self::In { pos, len } => pos as f32 / len as f32,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Voice {
    /// The note it plays or last played (once stolen, the note it will play).
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
    /// The envelope has said it is busy since this note began (see `busy`).
    reported: bool,
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
    fade: Fade,
}

impl Voice {
    fn pitch(&self) -> f32 {
        let semitones = f32::from(self.key) + self.bend - 69.0;
        440.0 * (semitones / 12.0).exp2()
    }

    /// Switches a faded-out voice to the note it was waiting for.
    fn switch(&mut self, next: Pending) {
        self.key = next.key;
        self.velocity = next.velocity;
        self.bend = next.bend;
        self.held = !next.released;
        self.since_off = 0;
        self.reported = false;
    }
}

struct VoicesNode {
    voices: Vec<Voice>,
    clock: u64,
    /// The tail of the block in progress, in frames.
    tail: u64,
}

impl VoicesNode {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// The voice a new note takes: an inactive one first, then the
    /// longest-released one still ringing, then (stealing) the oldest held.
    /// Within each group the oldest goes first.
    fn allocate(&self) -> usize {
        self.voices
            .iter()
            .enumerate()
            .min_by_key(|(_, v)| (v.active, v.held, v.stamp))
            .map(|(i, _)| i)
            .expect("there is at least one voice")
    }

    /// `fade_len` is the steal fade in frames; 0 steals hard.
    fn apply(&mut self, kind: EventKind, fade_len: u32) {
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
                if voice.held && voice.id != note.0 && fade_len > 0 {
                    // A steal: fade the old note out first. A voice that is
                    // itself mid-fade carries on from the level it has.
                    let next = Pending {
                        key,
                        velocity,
                        bend: 0.0,
                        released: false,
                    };
                    let (pos, len) = match voice.fade {
                        Fade::Steady => (0, fade_len),
                        Fade::Out { pos, len, .. } => (pos, len),
                        Fade::In { pos, len } => (len - pos.min(len), len),
                    };
                    voice.fade = Fade::Out { pos, len, next };
                    voice.id = note.0;
                    voice.stamp = stamp;
                    voice.quiet = false;
                    voice.gate_quiet = false;
                    return;
                }
                voice.rearm = voice.held;
                *voice = Voice {
                    id: note.0,
                    key,
                    velocity,
                    bend: 0.0,
                    held: true,
                    active: true,
                    since_off: 0,
                    reported: false,
                    quiet: false,
                    gate_quiet: false,
                    rearm: voice.rearm,
                    stamp,
                    // A voice still fading keeps its level, so reusing it
                    // doesn't snap the gain.
                    fade: match voice.fade {
                        fade @ Fade::In { .. } => fade,
                        Fade::Out { pos, len, .. } => Fade::In {
                            pos: len - pos.min(len),
                            len,
                        },
                        Fade::Steady => Fade::Steady,
                    },
                };
            }
            EventKind::NoteOff { note, .. } => {
                let stamp = self.tick();
                if let Some(voice) = self.voices.iter_mut().find(|v| v.held && v.id == note.0) {
                    if let Fade::Out { next, .. } = &mut voice.fade {
                        // The note ends before it began: the voice will
                        // switch to it and let go at once.
                        next.released = true;
                        return;
                    }
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
                    match &mut voice.fade {
                        Fade::Out { next, .. } => next.bend = value,
                        _ => voice.bend = value,
                    }
                }
            }
            _ => {}
        }
    }

    /// Writes the voices' current state to frames `start..end`.
    fn fill(&mut self, io: &mut [SignalOut<'_>], start: usize, end: usize) {
        if start >= end {
            return;
        }
        let [pitch, gate, velocity, fade] = io
            .get_disjoint_mut([
                VoicesPorts::PITCH,
                VoicesPorts::GATE,
                VoicesPorts::VELOCITY,
                VoicesPorts::FADE,
            ])
            .expect("voices outputs are distinct");
        for (i, voice) in self.voices.iter_mut().enumerate() {
            if !matches!(voice.fade, Fade::Steady) {
                voice.fill_fading(
                    &mut pitch.lane_mut(i, 0)[start..end],
                    &mut gate.lane_mut(i, 0)[start..end],
                    &mut velocity.lane_mut(i, 0)[start..end],
                    &mut fade.lane_mut(i, 0)[start..end],
                );
                continue;
            }
            pitch.lane_mut(i, 0)[start..end].fill(if voice.active { voice.pitch() } else { 0.0 });
            let gate = &mut gate.lane_mut(i, 0)[start..end];
            gate.fill(if voice.held { 1.0 } else { 0.0 });
            if voice.rearm {
                gate[0] = 0.0;
                voice.rearm = false;
            }
            let velocity_now = if voice.active { voice.velocity } else { 0.0 };
            velocity.lane_mut(i, 0)[start..end].fill(velocity_now);
            fade.lane_mut(i, 0)[start..end].fill(if voice.active { 1.0 } else { 0.0 });
        }
    }

    /// Before a block's events: what the flags may claim for it.
    fn begin_block(&mut self) {
        for voice in &mut self.voices {
            voice.quiet = !voice.active;
            voice.gate_quiet = !voice.held && !voice.rearm;
        }
    }

    /// After a block's events: flags the lanes that stayed silent.
    fn flag_silence(&mut self, io: &mut [SignalOut<'_>]) {
        for (i, voice) in self.voices.iter().enumerate() {
            if voice.quiet {
                for output in io.iter_mut() {
                    output.set_silent(i, 0);
                }
            } else if voice.gate_quiet {
                io[VoicesPorts::GATE].set_silent(i, 0);
            }
        }
    }

    /// Writes the outputs for the block. Everything but `busy` is read here.
    fn output_half(&mut self, ctx: &Context, io: &Io<'_, '_>, outputs: &mut [SignalOut<'_>]) {
        let first = |port: usize| io.inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let rate = f64::from(ctx.sample_rate);
        self.tail = (f64::from(first(VoicesPorts::TAIL).max(MIN_TAIL)) * rate) as u64;
        let fade_len = (f64::from(first(VoicesPorts::STEAL_FADE).max(0.0)) * rate).round() as u32;

        let events: &[Event] = io.event_inputs[VoicesPorts::NOTES];
        self.begin_block();
        let mut cursor = 0;
        for event in events {
            let time = (event.time as usize).min(ctx.frames);
            self.fill(outputs, cursor, time);
            cursor = cursor.max(time);
            self.apply(event.kind, fade_len);
        }
        self.fill(outputs, cursor, ctx.frames);
        self.flag_silence(outputs);
    }

    /// After the envelopes have run: makes voices inactive once they are done.
    fn input_half(&mut self, ctx: &Context, busy: &SignalIn<'_>) {
        let frames = ctx.frames as u64;
        for (i, voice) in self.voices.iter_mut().enumerate() {
            if !voice.active {
                continue;
            }
            let busy_now = busy.lane(i, 0).iter().any(|&b| b >= 0.5);
            voice.reported |= busy_now;
            if voice.held || !matches!(voice.fade, Fade::Steady) {
                continue;
            }
            voice.since_off = voice.since_off.saturating_add(frames);
            if voice.since_off >= self.tail || (voice.reported && !busy_now) {
                voice.active = false;
            }
        }
    }
}

impl Voice {
    /// Writes a voice that is fading, a sample at a time.
    fn fill_fading(
        &mut self,
        pitch: &mut [f32],
        gate: &mut [f32],
        velocity: &mut [f32],
        fade: &mut [f32],
    ) {
        let samples = pitch.iter_mut().zip(gate).zip(velocity).zip(fade);
        for (((pitch, gate), velocity), fade) in samples {
            let mut low = std::mem::take(&mut self.rearm);
            if let Fade::Out { pos, len, next } = self.fade
                && pos >= len
            {
                self.switch(next);
                self.fade = Fade::In { pos: 0, len };
                // The envelope restarts, as for a hard steal.
                low = true;
            }
            *pitch = if self.active { self.pitch() } else { 0.0 };
            *gate = if self.held && !low { 1.0 } else { 0.0 };
            *velocity = if self.active { self.velocity } else { 0.0 };
            *fade = self.fade.level();
            self.fade = match self.fade {
                Fade::Out { pos, len, next } => Fade::Out {
                    pos: pos + 1,
                    len,
                    next,
                },
                Fade::In { pos, len } if pos + 1 < len => Fade::In { pos: pos + 1, len },
                Fade::In { .. } | Fade::Steady => Fade::Steady,
            };
        }
    }
}

impl Node for VoicesNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let outputs = io.outputs;
        let io = Io {
            outputs: &mut [],
            ..io
        };
        self.output_half(ctx, &io, outputs);
        self.input_half(ctx, &io.inputs[VoicesPorts::BUSY]);
    }

    fn process_output(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let outputs = io.outputs;
        let io = Io {
            outputs: &mut [],
            ..io
        };
        self.output_half(ctx, &io, outputs);
    }

    fn process_input(&mut self, ctx: &Context, io: Io<'_, '_>) {
        self.input_half(ctx, &io.inputs[VoicesPorts::BUSY]);
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
        let mut h = voices_with(count, &[]);
        // Most tests are about the notes, not the fade.
        h.set(VoicesPorts::STEAL_FADE, 0.0);
        h
    }

    fn voices_with(count: i64, connected: &[(usize, Shape)]) -> Harness {
        let mut config = Config::new();
        config.set("voices", noodle_core::Value::Int(count));
        Harness::new(&Voices, &config, connected, 48_000.0, 16).unwrap()
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
        // 0.05 s at 48 kHz is 2400 frames: 150 blocks of 16.
        h.set(VoicesPorts::TAIL, 0.05);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 69, 0.5), off(8, 1)]);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[]);

        // Gate fell mid-block, so it isn't flagged then; next block it is.
        assert!(!silent(&h, VoicesPorts::GATE, 0));
        h.run(16).unwrap();
        assert!(silent(&h, VoicesPorts::GATE, 0), "low all block");
        assert!(!silent(&h, VoicesPorts::PITCH, 0), "still ringing");
        assert!((pitch(&h, 0, 3) - 440.0).abs() < 1e-3);

        for _ in 0..160 {
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
        h.set(VoicesPorts::TAIL, 0.05);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 60, 0.5), off(4, 1)]);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[]);
        for _ in 0..160 {
            h.run(16).unwrap();
        }
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

    fn fade(h: &Harness, voice: usize) -> Vec<f32> {
        h.output(VoicesPorts::FADE).lane(voice, 0).to_vec()
    }

    #[test]
    fn a_steal_fades_the_old_note_out_then_switches_and_fades_in() {
        // Four frames of fade at 48 kHz.
        let mut h = voices_with(1, &[]);
        h.set(VoicesPorts::STEAL_FADE, 4.0 / 48_000.0);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 69, 1.0), on(4, 2, 81, 1.0)]);
        h.run(16).unwrap();
        let (g, f) = (gate(&h, 0), fade(&h, 0));
        // The old note plays on at its pitch while the gain ramps down...
        for (k, &gate) in g.iter().enumerate().take(8).skip(4) {
            assert!((pitch(&h, 0, k) - 440.0).abs() < 1e-3, "frame {k}");
            assert_eq!(gate, 1.0, "frame {k}");
        }
        assert_eq!(&f[..4], &[1.0; 4]);
        assert_eq!(&f[4..8], &[1.0, 0.75, 0.5, 0.25]);
        // ...then the new one takes over with the gate low for a sample and
        // the gain climbing back.
        assert_eq!(g[8], 0.0);
        assert!(g[9..].iter().all(|&g| g == 1.0));
        assert!((pitch(&h, 0, 8) - 880.0).abs() < 1e-2);
        assert_eq!(&f[8..12], &[0.0, 0.25, 0.5, 0.75]);
        assert!(f[12..].iter().all(|&f| f == 1.0));
    }

    #[test]
    fn a_fading_steal_spans_blocks_and_the_new_note_can_end_before_it_starts() {
        let mut h = voices_with(1, &[]);
        h.set(VoicesPorts::STEAL_FADE, 8.0 / 48_000.0);
        h.send_events(
            VoicesPorts::NOTES,
            &[on(0, 1, 69, 1.0), on(12, 2, 81, 1.0), off(14, 2)],
        );
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[]);
        h.run(16).unwrap();
        // The switch happens at frame 4 of the second block; the note is
        // already over, so the gate never rises, but the pitch has changed.
        let g = gate(&h, 0);
        assert!(g[..4].iter().all(|&g| g == 1.0), "{g:?}");
        assert!(g[4..].iter().all(|&g| g == 0.0), "{g:?}");
        assert!((pitch(&h, 0, 15) - 880.0).abs() < 1e-2);
        assert!(!silent(&h, VoicesPorts::PITCH, 0));
    }

    #[test]
    fn the_old_notes_off_during_a_steal_fade_is_ignored() {
        let mut h = voices_with(1, &[]);
        h.set(VoicesPorts::STEAL_FADE, 8.0 / 48_000.0);
        h.send_events(
            VoicesPorts::NOTES,
            &[on(0, 1, 69, 1.0), on(2, 2, 81, 1.0), off(4, 1)],
        );
        h.run(16).unwrap();
        let g = gate(&h, 0);
        assert!(g[2..10].iter().all(|&g| g == 1.0), "{g:?}");
        assert_eq!(g[10], 0.0);
        assert!(g[11..].iter().all(|&g| g == 1.0));
    }

    fn busy_rig(tail: f32) -> Harness {
        let mut h = voices_with(2, &[(VoicesPorts::BUSY, Shape::new(2, 1))]);
        h.set(VoicesPorts::STEAL_FADE, 0.0);
        h.set(VoicesPorts::TAIL, tail);
        h
    }

    fn feed_busy(h: &mut Harness, voice0: f32, voice1: f32) {
        let mut busy = h.input(VoicesPorts::BUSY, 16);
        busy.lane_mut(0, 0).fill(voice0);
        busy.lane_mut(1, 0).fill(voice1);
    }

    #[test]
    fn a_voice_goes_inactive_when_its_envelope_finishes_not_at_the_tail() {
        let mut h = busy_rig(30.0);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 69, 0.5), off(8, 1)]);
        feed_busy(&mut h, 1.0, 0.0);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[]);
        // The envelope is releasing: the voice keeps its pitch.
        for _ in 0..50 {
            feed_busy(&mut h, 1.0, 0.0);
            h.run(16).unwrap();
            assert!(!silent(&h, VoicesPorts::PITCH, 0));
        }
        // It finishes; the voice goes inactive after that block...
        feed_busy(&mut h, 0.0, 0.0);
        h.run(16).unwrap();
        assert!(
            !silent(&h, VoicesPorts::PITCH, 0),
            "still the block it ended"
        );
        feed_busy(&mut h, 0.0, 0.0);
        h.run(16).unwrap();
        // ...long before the 30 second tail.
        assert!(silent(&h, VoicesPorts::PITCH, 0));
        assert_eq!(pitch(&h, 0, 3), 0.0);
    }

    #[test]
    fn a_voice_whose_envelope_never_reported_waits_for_the_tail() {
        let mut h = busy_rig(0.05);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 69, 0.5), off(8, 1)]);
        feed_busy(&mut h, 0.0, 0.0);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[]);
        for _ in 0..100 {
            feed_busy(&mut h, 0.0, 0.0);
            h.run(16).unwrap();
        }
        assert!(!silent(&h, VoicesPorts::PITCH, 0), "tail is 150 blocks");
        for _ in 0..60 {
            feed_busy(&mut h, 0.0, 0.0);
            h.run(16).unwrap();
        }
        assert!(silent(&h, VoicesPorts::PITCH, 0));
    }

    #[test]
    fn the_tail_caps_a_busy_that_never_falls() {
        let mut h = busy_rig(0.05);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 69, 0.5), off(8, 1)]);
        for _ in 0..170 {
            feed_busy(&mut h, 1.0, 0.0);
            h.run(16).unwrap();
            h.send_events(VoicesPorts::NOTES, &[]);
        }
        assert!(silent(&h, VoicesPorts::PITCH, 0));
    }

    #[test]
    fn a_note_while_the_envelope_is_still_busy_retakes_the_voice_cleanly() {
        let mut h = voices_with(1, &[(VoicesPorts::BUSY, Shape::MONO)]);
        h.set(VoicesPorts::STEAL_FADE, 0.0);
        h.set(VoicesPorts::TAIL, 30.0);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 69, 0.5), off(2, 1)]);
        h.input(VoicesPorts::BUSY, 16).fill(1.0);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[on(0, 2, 72, 0.5)]);
        h.input(VoicesPorts::BUSY, 16).fill(1.0);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[]);
        // Busy falls, but the new note is held, so the voice stays.
        for _ in 0..5 {
            h.input(VoicesPorts::BUSY, 16).fill(0.0);
            h.run(16).unwrap();
            assert!(!silent(&h, VoicesPorts::PITCH, 0));
        }
    }

    #[test]
    fn a_new_note_prefers_an_inactive_voice_to_one_still_ringing() {
        let mut h = busy_rig(30.0);
        // Voice 0 is released first (so the older), voice 1 after.
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 60, 0.5), on(0, 2, 62, 0.5)]);
        feed_busy(&mut h, 1.0, 1.0);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[off(0, 1), off(1, 2)]);
        feed_busy(&mut h, 1.0, 1.0);
        h.run(16).unwrap();
        h.send_events(VoicesPorts::NOTES, &[]);
        // Voice 1's envelope finishes; voice 0's is still releasing.
        for _ in 0..2 {
            feed_busy(&mut h, 1.0, 0.0);
            h.run(16).unwrap();
        }
        assert!(silent(&h, VoicesPorts::PITCH, 1));
        assert!(!silent(&h, VoicesPorts::PITCH, 0));
        h.send_events(VoicesPorts::NOTES, &[on(0, 3, 72, 0.5)]);
        feed_busy(&mut h, 1.0, 0.0);
        h.run(16).unwrap();
        assert_eq!(pitch(&h, 1, 0), 523.2511);
        assert!(pitch(&h, 0, 0) < 270.0, "voice 0 keeps its note");
    }

    #[test]
    fn a_busy_signal_with_the_wrong_number_of_voices_is_refused() {
        let mut config = Config::new();
        config.set("voices", noodle_core::Value::Int(4));
        for voices in [2, 8] {
            let wrong = Harness::new(
                &Voices,
                &config,
                &[(VoicesPorts::BUSY, Shape::new(voices, 1))],
                48_000.0,
                16,
            );
            assert!(wrong.is_err(), "{voices} voices");
        }
        for voices in [1, 4] {
            Harness::new(
                &Voices,
                &config,
                &[(VoicesPorts::BUSY, Shape::new(voices, 1))],
                48_000.0,
                16,
            )
            .unwrap();
        }
    }

    #[test]
    fn reusing_a_voice_mid_fade_in_keeps_the_gain_continuous() {
        let mut h = voices_with(1, &[]);
        h.set(VoicesPorts::STEAL_FADE, 16.0 / 48_000.0);
        h.send_events(VoicesPorts::NOTES, &[on(0, 1, 69, 1.0)]);
        h.run(16).unwrap();
        // A steal; the new note ends at once, so the voice is free while it
        // fades in over the next block.
        h.send_events(VoicesPorts::NOTES, &[on(0, 2, 81, 1.0), off(1, 2)]);
        h.run(16).unwrap();
        // A new note takes the voice halfway through the fade-in.
        h.send_events(VoicesPorts::NOTES, &[on(8, 3, 60, 1.0)]);
        h.run(16).unwrap();
        let f = fade(&h, 0);
        assert!(f[7] < 0.6, "{f:?}");
        assert!(f.windows(2).all(|w| (w[1] - w[0]).abs() < 0.07), "{f:?}");
    }
}
