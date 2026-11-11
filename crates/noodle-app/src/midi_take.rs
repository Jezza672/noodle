//! Recording MIDI: what arrives on the session's MIDI bus while a take runs,
//! turned into a MIDI clip when it ends.
//!
//! Messages carry the time they arrived on the monotonic clock of
//! [`noodle_io::now_micros`]. The audio thread publishes, every device
//! callback, a pair of that clock and the transport's position (the
//! [`AudioClock`](noodle_io::AudioClock)'s anchor). Each message is mapped to
//! the timeline through the newest pair when it was collected, which is at
//! most a UI frame after it arrived, so neither the stream's startup nor the
//! two clocks' drift over a long take matters. Notes are moved earlier by the
//! output latency, since the player was playing along to what they heard,
//! and the clip starts that much earlier too, then the starts are optionally
//! snapped to a grid.

use noodle_core::{
    ControlKind, ControlPoint, ExpressionKind, MidiClip, MidiNote, NoteExpression,
    TICKS_PER_QUARTER, TempoMap, Tick,
};
use noodle_io::{MidiBus, MidiMessage, MidiReceiver};

/// How recorded notes are snapped to the beat. Only the notes' starts move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Quantise {
    #[default]
    Off,
    Quarter,
    Eighth,
    Sixteenth,
    ThirtySecond,
    EighthTriplet,
    SixteenthTriplet,
}

impl Quantise {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Quarter,
        Self::Eighth,
        Self::Sixteenth,
        Self::ThirtySecond,
        Self::EighthTriplet,
        Self::SixteenthTriplet,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Quarter => "1/4",
            Self::Eighth => "1/8",
            Self::Sixteenth => "1/16",
            Self::ThirtySecond => "1/32",
            Self::EighthTriplet => "1/8T",
            Self::SixteenthTriplet => "1/16T",
        }
    }

    /// The grid's spacing in ticks, if any.
    pub fn grid(self) -> Option<i64> {
        let whole = TICKS_PER_QUARTER * 4;
        Some(match self {
            Self::Off => return None,
            Self::Quarter => whole / 4,
            Self::Eighth => whole / 8,
            Self::Sixteenth => whole / 16,
            Self::ThirtySecond => whole / 32,
            Self::EighthTriplet => whole / 12,
            Self::SixteenthTriplet => whole / 24,
        })
    }
}

/// What a loop recording of MIDI makes of its passes through the loop: a take
/// each, in a take folder to comp from, or one clip with every pass merged
/// (overdub). An app preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MidiRecord {
    #[default]
    Takes,
    Merge,
}

impl MidiRecord {
    pub const ALL: [Self; 2] = [Self::Takes, Self::Merge];

    pub fn label(self) -> &'static str {
        match self {
            Self::Takes => "Takes",
            Self::Merge => "Merge",
        }
    }
}

/// A take of MIDI being recorded.
pub struct MidiTake {
    receiver: MidiReceiver,
    /// The clock when the take started.
    start_micros: u64,
    /// The transport's position then, in samples. Used for messages that
    /// arrive before the audio thread has published an anchor.
    start_sample: u64,
    /// Each message with the arrival time and the anchor current when it
    /// was collected.
    messages: Vec<(u64, MidiMessage, Anchor)>,
}

/// The passes through a loop that a loop recording makes.
#[derive(Clone, Copy, Debug)]
pub struct Cycles {
    /// Samples from the take's start to the loop's end.
    pub first: f64,
    /// Samples in one pass of the loop.
    pub cycle: f64,
    /// The loop's start and end.
    pub start: Tick,
    pub end: Tick,
}

impl Cycles {
    /// The pass that `samples` (counted from the take's start, as heard)
    /// fall in.
    fn index(&self, samples: f64) -> usize {
        if samples < self.first || self.cycle <= 0.0 {
            0
        } else {
            1 + ((samples - self.first) / self.cycle) as usize
        }
    }
}

/// One pass through the loop, to be built from the take.
struct Pass<'a> {
    cycles: &'a Cycles,
    index: usize,
}

/// A `(micros, transport sample)` pair from the audio thread.
type Anchor = Option<(u64, u64)>;

/// The settings a take is turned into a clip with.
pub struct Finish<'a> {
    pub stop_micros: u64,
    pub output_latency_micros: u64,
    pub sample_rate: f64,
    pub tempo_map: &'a TempoMap,
    pub quantise: Quantise,
    /// The newest anchor when the take ended.
    pub anchor: Anchor,
}

impl MidiTake {
    /// Starts listening on `bus`.
    pub fn start(bus: &MidiBus, start_micros: u64, start_sample: u64) -> Self {
        Self {
            receiver: bus.subscribe(),
            start_micros,
            start_sample,
            messages: Vec::new(),
        }
    }

    /// Moves what has arrived out of the bus's queue, which is only so long,
    /// noting `anchor`, the audio thread's newest, with it. Call it every UI
    /// frame.
    pub fn poll(&mut self, anchor: Anchor) {
        while let Some((at, message)) = self.receiver.pop_timed() {
            if at >= self.start_micros {
                self.messages.push((at, message, anchor));
            }
        }
    }

    /// Ends the take. The clip starts where the take did and runs to where it
    /// ended, or `None` if no note was played.
    #[cfg(test)]
    pub fn finish(mut self, finish: &Finish<'_>) -> Option<(Tick, MidiClip)> {
        self.poll(finish.anchor);
        self.clip_so_far(finish)
    }

    /// The clip the take would be if it ended as `finish` says, without
    /// ending it: what is on screen while recording. Notes still held end
    /// at the stop. Call [`poll`](Self::poll) first.
    pub fn clip_so_far(&self, finish: &Finish<'_>) -> Option<(Tick, MidiClip)> {
        self.build(finish, None)
    }

    /// The take as one take per pass through the loop `cycles` describes,
    /// ended as `finish` says: the tick the first starts at and the
    /// passes' clips, which all run to the loop's end and are windows onto
    /// the same stretch of music. A pass in which nothing was played is an
    /// empty clip. `None` if no pass has anything in it. Call
    /// [`poll`](Self::poll) first (or use this to end the take).
    pub fn passes(&self, finish: &Finish<'_>, cycles: &Cycles) -> Option<(Tick, Vec<MidiClip>)> {
        let heard = (finish.stop_micros.saturating_sub(self.start_micros)) as f64
            * finish.sample_rate
            / 1e6
            - finish.output_latency_micros as f64 * finish.sample_rate / 1e6;
        let mut count = cycles.index(heard) + 1;
        // A sliver at the end, from stopping just after a wrap, is not a take.
        if count > 1 {
            let into = heard - cycles.first - (count - 2) as f64 * cycles.cycle;
            if into * 4.0 < cycles.first {
                count -= 1;
            }
        }
        let mut start = None;
        let mut passes = Vec::new();
        for index in 0..count {
            let clip = self.build(finish, Some(Pass { cycles, index }));
            let (at, clip) = match clip {
                Some((at, clip)) => (Some(at), clip),
                None => (None, MidiClip::new(Tick(1))),
            };
            start = start.or(at);
            passes.push(clip);
        }
        let start = start?;
        // Every pass runs to the loop's end.
        let length = Tick((cycles.end.0 - start.0).max(1));
        for clip in &mut passes {
            clip.length = length;
        }
        if passes
            .iter()
            .all(|p| p.notes.is_empty() && p.controls.is_empty())
        {
            return None;
        }
        Some((start, passes))
    }

    fn build(&self, finish: &Finish<'_>, pass: Option<Pass<'_>>) -> Option<(Tick, MidiClip)> {
        let Finish {
            stop_micros,
            output_latency_micros,
            sample_rate,
            tempo_map,
            quantise,
            anchor: stop_anchor,
        } = *finish;
        let latency_samples = output_latency_micros as f64 * sample_rate / 1e6;
        let fallback = (self.start_micros, self.start_sample);
        let tick_at = |micros: u64, anchor: Anchor| -> i64 {
            let (anchor_micros, anchor_sample) = anchor.unwrap_or(fallback);
            let since = micros as f64 - anchor_micros as f64;
            let sample = anchor_sample as f64 + since * sample_rate / 1e6 - latency_samples;
            tempo_map
                .tick_at_sample(sample.max(0.0).round() as u64, sample_rate)
                .round() as i64
        };
        // What was heard when Record was pressed is what the transport had
        // rendered a device latency before.
        let clip_start = tempo_map
            .tick_at_sample(
                (self.start_sample as f64 - latency_samples)
                    .max(0.0)
                    .round() as u64,
                sample_rate,
            )
            .round() as i64;
        let stop = match &pass {
            Some(pass) => pass.cycles.end.0,
            None => tick_at(stop_micros, stop_anchor),
        };
        let heard = |micros: u64| {
            micros.saturating_sub(self.start_micros) as f64 * sample_rate / 1e6 - latency_samples
        };
        // In a pass through a loop, a tick stays inside the loop.
        let tick_at = |micros: u64, anchor: Anchor| -> i64 {
            match &pass {
                // Past the first pass the loop has wrapped: a tick counts
                // from the loop's start by the time since the last wrap.
                Some(pass) if pass.index > 0 => {
                    let into = heard(micros)
                        - pass.cycles.first
                        - (pass.index - 1) as f64 * pass.cycles.cycle;
                    let from = tempo_map.sample_at(pass.cycles.start, sample_rate) as f64;
                    tempo_map
                        .tick_at_sample((from + into).max(0.0).round() as u64, sample_rate)
                        .round() as i64
                }
                Some(pass) => tick_at(micros, anchor).min(pass.cycles.end.0),
                None => tick_at(micros, anchor),
            }
        };

        // A note as played: its starts and ends unquantised. The expressions
        // are counted from its start, so they move with it.
        struct Played {
            key: u8,
            start: i64,
            end: i64,
            velocity: f32,
            expressions: Vec<NoteExpression>,
        }
        let mut open: [[Option<Played>; 128]; 16] =
            std::array::from_fn(|_| std::array::from_fn(|_| None));
        let mut notes: Vec<Played> = Vec::new();
        let mut controls: Vec<ControlPoint> = Vec::new();
        for &(at, [status, a, b], anchor) in &self.messages {
            if at > stop_micros {
                break;
            }
            if let Some(pass) = &pass
                && pass.cycles.index(heard(at)) != pass.index
            {
                continue;
            }
            let channel = status & 0x0F;
            let tick = tick_at(at, anchor);
            match status & 0xF0 {
                0x90 | 0x80 => {
                    let (c, key_index) = (usize::from(channel), usize::from(a & 0x7F));
                    if status & 0xF0 == 0x90 && b > 0 {
                        // A key struck again before it was let go ends the
                        // first.
                        if let Some(mut held) = open[c][key_index].take() {
                            held.end = tick;
                            notes.push(held);
                        }
                        open[c][key_index] = Some(Played {
                            key: key_index as u8,
                            start: tick,
                            end: tick,
                            velocity: f32::from(b) / 127.0,
                            expressions: Vec::new(),
                        });
                    } else if let Some(mut held) = open[c][key_index].take() {
                        held.end = tick;
                        notes.push(held);
                    }
                }
                // Poly pressure goes to the note it is for.
                0xA0 => {
                    if let Some(held) = &mut open[usize::from(channel)][usize::from(a & 0x7F)] {
                        held.expressions.push(NoteExpression {
                            tick: Tick((tick - held.start).max(0)),
                            kind: ExpressionKind::Pressure,
                            value: f32::from(b & 0x7F) / 127.0,
                            ramp: false,
                        });
                    }
                }
                status_kind @ (0xB0 | 0xC0 | 0xD0 | 0xE0) => {
                    let (kind, value) = match status_kind {
                        // Channel mode messages (all notes off and the like)
                        // are not performance.
                        0xB0 if a >= 120 => continue,
                        0xB0 => (
                            ControlKind::Controller(a & 0x7F),
                            f32::from(b & 0x7F) / 127.0,
                        ),
                        0xC0 => (ControlKind::ProgramChange, f32::from(a & 0x7F) / 127.0),
                        0xD0 => (ControlKind::ChannelPressure, f32::from(a & 0x7F) / 127.0),
                        _ => {
                            let bend = f32::from(u16::from(b & 0x7F) << 7 | u16::from(a & 0x7F));
                            (ControlKind::PitchBend, ((bend - 8192.0) / 8192.0).min(1.0))
                        }
                    };
                    // A repeat of the value a controller already has adds
                    // nothing.
                    let repeated = controls
                        .iter()
                        .rev()
                        .find(|p| p.channel == channel && p.kind == kind)
                        .is_some_and(|p| p.value == value);
                    if !repeated {
                        controls.push(ControlPoint::new(Tick(tick), channel, kind, value));
                    }
                }
                _ => {}
            }
        }
        // What is still held ends with the take.
        for held in open.iter_mut().flatten() {
            if let Some(mut held) = held.take() {
                held.end = stop;
                notes.push(held);
            }
        }
        if notes.is_empty() && controls.is_empty() {
            return None;
        }

        if let Some(grid) = quantise.grid() {
            for note in &mut notes {
                let length = note.end - note.start;
                let snapped = (note.start as f64 / grid as f64).round() as i64 * grid;
                note.start = snapped;
                note.end = snapped + length;
            }
        }
        // Per key in order, a note ends where the same key next starts, and a
        // second note on the very same tick is dropped.
        notes.sort_by_key(|n| (n.key, n.start));
        let mut kept: Vec<Played> = Vec::with_capacity(notes.len());
        for note in notes {
            if let Some(last) = kept.last_mut()
                && last.key == note.key
            {
                if last.start == note.start {
                    continue;
                }
                last.end = last.end.min(note.start);
            }
            kept.push(note);
        }

        // A later pass has nothing from before the take began.
        let midi_notes: Vec<MidiNote> = kept
            .into_iter()
            .filter(|n| pass.is_none() || n.start >= clip_start)
            .map(|n| {
                let from = (n.start - clip_start).max(0);
                let length = (n.end - n.start).max(1);
                MidiNote {
                    id: 0,
                    start: Tick(from),
                    length: Tick(length),
                    key: n.key,
                    velocity: n.velocity,
                    expressions: n
                        .expressions
                        .into_iter()
                        .filter(|e| e.tick.0 < length)
                        .collect(),
                    articulation: None,
                    humanise: None,
                }
            })
            .collect();
        // Controllers stay where they were played, quantised or not.
        let control_points: Vec<ControlPoint> = controls
            .into_iter()
            .filter(|p| pass.is_none() || p.tick.0 >= clip_start)
            .map(|p| ControlPoint {
                tick: Tick((p.tick.0 - clip_start).max(0)),
                ..p
            })
            .collect();
        let last_end = midi_notes
            .iter()
            .map(|n| n.start.0 + n.length.0)
            .max()
            .unwrap_or(0);
        let last_control = control_points
            .iter()
            .map(|p| p.tick.0 + 1)
            .max()
            .unwrap_or(0);
        let length = (stop - clip_start).max(last_end).max(last_control).max(1);
        let mut clip = MidiClip::new(Tick(length));
        clip.notes = midi_notes;
        clip.controls = control_points;
        Some((Tick(clip_start), clip))
    }
}

#[cfg(test)]
mod tests {
    use noodle_core::TimeSignature;

    use super::*;

    const RATE: f64 = 48_000.0;

    /// 120 bpm: a quarter note is half a second, 960 ticks.
    fn map() -> TempoMap {
        TempoMap::constant(120.0, TimeSignature::COMMON).unwrap()
    }

    /// A take from the start of the timeline, with messages at the given
    /// milliseconds, finished at `stop_ms`.
    fn take(messages: &[(u64, MidiMessage)], stop_ms: u64, quantise: Quantise) -> Option<MidiClip> {
        take_with_latency(messages, stop_ms, 0, quantise)
    }

    fn take_with_latency(
        messages: &[(u64, MidiMessage)],
        stop_ms: u64,
        latency_micros: u64,
        quantise: Quantise,
    ) -> Option<MidiClip> {
        let bus = MidiBus::new();
        let start = 1_000_000;
        let take = MidiTake::start(&bus, start, 0);
        for &(ms, message) in messages {
            bus.send_at(message, start + ms * 1000);
        }
        let map = map();
        take.finish(&Finish {
            stop_micros: start + stop_ms * 1000,
            output_latency_micros: latency_micros,
            sample_rate: RATE,
            tempo_map: &map,
            quantise,
            anchor: None,
        })
        .map(|(start, clip)| {
            assert_eq!(start, Tick(0));
            clip
        })
    }

    /// Takes of a loop one second (two beats, 1920 ticks) long, recorded from
    /// the start, with messages at the given milliseconds, stopped at `stop_ms`.
    fn passes(messages: &[(u64, MidiMessage)], stop_ms: u64) -> Option<(Tick, Vec<MidiClip>)> {
        let bus = MidiBus::new();
        let start = 1_000_000;
        let take = MidiTake::start(&bus, start, 0);
        for &(ms, message) in messages {
            bus.send_at(message, start + ms * 1000);
        }
        let map = map();
        let mut take = take;
        let finish = Finish {
            stop_micros: start + stop_ms * 1000,
            output_latency_micros: 0,
            sample_rate: RATE,
            tempo_map: &map,
            quantise: Quantise::Off,
            anchor: None,
        };
        take.poll(None);
        take.passes(
            &finish,
            &Cycles {
                first: RATE,
                cycle: RATE,
                start: Tick(0),
                end: Tick(1_920),
            },
        )
    }

    #[test]
    fn each_pass_through_the_loop_is_a_take_with_its_own_notes() {
        // Three passes of a second: the third is half played.
        let (start, takes) = passes(
            &[
                (250, [ON, 60, 100]),
                (400, [OFF, 60, 0]),
                (1_250, [ON, 62, 100]),
                (1_400, [OFF, 62, 0]),
                (2_250, [ON, 64, 100]),
                (2_400, [OFF, 64, 0]),
            ],
            2_500,
        )
        .unwrap();
        assert_eq!(start, Tick(0));
        assert_eq!(takes.len(), 3);
        let keys = |clip: &MidiClip| {
            clip.notes
                .iter()
                .map(|n| (n.key, n.start))
                .collect::<Vec<_>>()
        };
        // A quarter second in is 480 ticks, in every pass.
        assert_eq!(keys(&takes[0]), [(60, Tick(480))]);
        assert_eq!(keys(&takes[1]), [(62, Tick(480))]);
        assert_eq!(keys(&takes[2]), [(64, Tick(480))]);
        // Every take runs to the loop's end.
        assert!(takes.iter().all(|t| t.length == Tick(1_920)));
    }

    #[test]
    fn a_sliver_of_a_pass_at_the_end_is_not_a_take_and_a_silent_pass_is_empty() {
        let (_, takes) = passes(&[(250, [ON, 60, 100]), (400, [OFF, 60, 0])], 2_050).unwrap();
        // Two whole passes (the second silent) and 50 ms of a third.
        assert_eq!(takes.len(), 2);
        assert!(takes[1].notes.is_empty());
        assert!(passes(&[], 2_500).is_none());
    }

    const ON: u8 = 0x90;
    const OFF: u8 = 0x80;

    #[test]
    fn notes_land_where_they_were_played() {
        // 500 ms is a quarter note: 960 ticks.
        let clip = take(
            &[(500, [ON, 60, 127]), (750, [OFF, 60, 0])],
            2000,
            Quantise::Off,
        )
        .unwrap();
        assert_eq!(clip.notes.len(), 1);
        let note = clip.notes[0].clone();
        assert_eq!(
            (note.key, note.start, note.length),
            (60, Tick(960), Tick(480))
        );
        assert_eq!(note.velocity, 1.0);
        // The clip runs to the end of the take: 2 seconds is 3840 ticks.
        assert_eq!(clip.length, Tick(3840));
    }

    #[test]
    fn a_take_with_nothing_played_makes_no_clip() {
        assert!(take(&[], 1000, Quantise::Off).is_none());
        // Channel mode messages (all notes off) are not performance.
        assert!(take(&[(100, [0xB0, 123, 0])], 1000, Quantise::Off).is_none());
    }

    #[test]
    fn a_note_held_at_the_end_ends_with_the_take() {
        let clip = take(&[(500, [ON, 60, 100])], 1000, Quantise::Off).unwrap();
        assert_eq!(clip.notes[0].start, Tick(960));
        assert_eq!(clip.notes[0].length, Tick(960));
    }

    #[test]
    fn a_note_off_with_no_note_on_is_ignored_and_velocity_zero_is_an_off() {
        let clip = take(
            &[
                (100, [OFF, 61, 0]),
                (500, [ON, 60, 100]),
                (1000, [ON, 60, 0]),
            ],
            2000,
            Quantise::Off,
        )
        .unwrap();
        assert_eq!(clip.notes.len(), 1);
        assert_eq!(clip.notes[0].length, Tick(960));
    }

    #[test]
    fn output_latency_moves_notes_earlier_but_not_before_the_start() {
        // 100 ms of latency is 192 ticks.
        let clip = take_with_latency(
            &[(500, [ON, 60, 100]), (550, [OFF, 60, 0])],
            2000,
            100_000,
            Quantise::Off,
        )
        .unwrap();
        assert_eq!(clip.notes[0].start, Tick(768));
        let early = take_with_latency(
            &[(50, [ON, 60, 100]), (80, [OFF, 60, 0])],
            2000,
            100_000,
            Quantise::Off,
        )
        .unwrap();
        assert_eq!(early.notes[0].start, Tick(0));
    }

    #[test]
    fn quantise_snaps_starts_and_keeps_lengths() {
        // 1/16 is 240 ticks. 520 ms is 998 ticks, which snaps to 960.
        let clip = take(
            &[(520, [ON, 60, 100]), (770, [OFF, 60, 0])],
            2000,
            Quantise::Sixteenth,
        )
        .unwrap();
        let note = clip.notes[0].clone();
        assert_eq!(note.start, Tick(960));
        // 250 ms is 480 ticks, which the snap of the start does not change.
        assert_eq!(note.length, Tick(480));
    }

    #[test]
    fn quantised_notes_of_a_key_do_not_overlap_and_a_double_is_dropped() {
        // Both starts snap to 960 on a quarter grid: one is kept.
        let doubled = take(
            &[
                (480, [ON, 60, 100]),
                (490, [OFF, 60, 0]),
                (520, [ON, 60, 90]),
                (560, [OFF, 60, 0]),
            ],
            2000,
            Quantise::Quarter,
        )
        .unwrap();
        assert_eq!(doubled.notes.len(), 1);
        // A long note followed by the same key is cut where the next begins.
        let cut = take(
            &[
                (0, [ON, 60, 100]),
                (400, [ON, 60, 100]),
                (600, [OFF, 60, 0]),
            ],
            2000,
            Quantise::Off,
        )
        .unwrap();
        let mut notes = cut.notes.clone();
        notes.sort_by_key(|n| n.start);
        assert_eq!(notes[0].start.0 + notes[0].length.0, notes[1].start.0);
    }

    #[test]
    fn notes_follow_the_audio_threads_anchor_not_the_moment_record_was_pressed() {
        // Record was pressed at 1 s with the transport at 0, but the stream
        // took 200 ms to start: its first callback, at 1.2 s, found the
        // transport at 0. A note at 1.7 s is half a second in: 960 ticks.
        let bus = MidiBus::new();
        let mut take = MidiTake::start(&bus, 1_000_000, 0);
        bus.send_at([ON, 60, 100], 1_700_000);
        take.poll(Some((1_200_000, 0)));
        bus.send_at([OFF, 60, 0], 1_900_000);
        let map = map();
        let (start, clip) = take
            .finish(&Finish {
                stop_micros: 2_000_000,
                output_latency_micros: 0,
                sample_rate: RATE,
                tempo_map: &map,
                quantise: Quantise::Off,
                anchor: Some((1_950_000, 36_000)),
            })
            .unwrap();
        assert_eq!(start, Tick(0));
        assert_eq!(clip.notes[0].start, Tick(960));
        // The off came after the last poll, so it was mapped through the
        // stopping anchor: 1.9 s is 50 ms before 36000 samples (0.75 s), so
        // 0.7 s: 1344 ticks.
        assert_eq!(clip.notes[0].start.0 + clip.notes[0].length.0, 1344);
    }

    #[test]
    fn the_clip_starts_a_latency_before_the_press_and_keeps_early_notes() {
        // Pressed with the transport at 1 s (2 quarter notes): with 100 ms of
        // latency the heard position was 0.9 s, 1728 ticks.
        let bus = MidiBus::new();
        let take = MidiTake::start(&bus, 1_000_000, 48_000);
        bus.send_at([ON, 60, 100], 1_010_000);
        let map = map();
        let (start, clip) = take
            .finish(&Finish {
                stop_micros: 1_500_000,
                output_latency_micros: 100_000,
                sample_rate: RATE,
                tempo_map: &map,
                quantise: Quantise::Off,
                anchor: None,
            })
            .unwrap();
        assert_eq!(start, Tick(1728));
        // The note at 10 ms is 1.01 s less 100 ms of latency = 0.91 s: 1747.
        assert_eq!(clip.notes[0].start, Tick(1747 - 1728));
    }

    #[test]
    fn grids_are_in_ticks() {
        assert_eq!(Quantise::Off.grid(), None);
        assert_eq!(Quantise::Quarter.grid(), Some(960));
        assert_eq!(Quantise::Sixteenth.grid(), Some(240));
        assert_eq!(Quantise::ThirtySecond.grid(), Some(120));
        assert_eq!(Quantise::EighthTriplet.grid(), Some(320));
        assert_eq!(Quantise::SixteenthTriplet.grid(), Some(160));
    }
    #[test]
    fn controllers_pressure_and_bend_are_kept_as_played() {
        let clip = take(
            &[
                (0, [0xB0, 64, 127]),
                (100, [0x90, 60, 100]),
                (200, [0xB0, 1, 64]),
                (250, [0xB0, 1, 64]),
                (300, [0xE0, 0, 0x40]),
                (350, [0xD0, 100, 0]),
                (400, [0xC0, 5, 0]),
                (450, [0x80, 60, 0]),
                (500, [0xB0, 64, 0]),
                (550, [0xB0, 123, 0]),
            ],
            1000,
            Quantise::Off,
        )
        .unwrap();
        let points: Vec<_> = clip
            .controls
            .iter()
            .map(|p| (p.tick.0, p.channel, p.kind))
            .collect();
        assert_eq!(
            points,
            [
                (0, 0, ControlKind::Controller(64)),
                // The second 64 on the wheel adds nothing; all-notes-off is
                // not performance.
                (384, 0, ControlKind::Controller(1)),
                (576, 0, ControlKind::PitchBend),
                (672, 0, ControlKind::ChannelPressure),
                (768, 0, ControlKind::ProgramChange),
                (960, 0, ControlKind::Controller(64)),
            ]
        );
        let values: Vec<f32> = clip.controls.iter().map(|p| p.value).collect();
        assert_eq!(values[0], 1.0);
        assert!((values[1] - 64.0 / 127.0).abs() < 1e-6);
        // 0x40 in the high byte is the centre: no bend.
        assert_eq!(values[2], 0.0);
        assert!((values[3] - 100.0 / 127.0).abs() < 1e-6);
        assert!((values[4] - 5.0 / 127.0).abs() < 1e-6);
        assert_eq!(values[5], 0.0);
    }

    #[test]
    fn a_take_of_only_controllers_is_a_clip_and_poly_pressure_goes_to_its_note() {
        let clip = take(&[(100, [0xB0, 64, 127])], 500, Quantise::Off).unwrap();
        assert!(clip.notes.is_empty());
        assert_eq!(clip.controls.len(), 1);
        let clip = take(
            &[
                (0, [0x90, 60, 100]),
                // Pressure on a key that is not held goes nowhere.
                (50, [0xA0, 62, 90]),
                (250, [0xA0, 60, 127]),
                (500, [0x80, 60, 0]),
            ],
            600,
            Quantise::Off,
        )
        .unwrap();
        assert!(clip.controls.is_empty());
        let e = &clip.notes[0].expressions;
        assert_eq!(e.len(), 1);
        assert_eq!(
            (e[0].tick, e[0].kind, e[0].value),
            (Tick(480), ExpressionKind::Pressure, 1.0)
        );
    }

    #[test]
    fn quantising_moves_a_notes_expressions_with_it_and_leaves_controllers_alone() {
        let messages = [
            (20, [0xB0, 1, 127]),
            // Played 20 ms (38 ticks) late.
            (520, [0x90, 60, 100]),
            (770, [0xA0, 60, 80]),
            (1000, [0x80, 60, 0]),
        ];
        let off = take(&messages, 1200, Quantise::Off).unwrap();
        let snapped = take(&messages, 1200, Quantise::Quarter).unwrap();
        // 520 ms is 998 ticks; the quarter grid pulls it back to 960.
        assert_eq!(off.notes[0].start, Tick(998));
        assert_eq!(snapped.notes[0].start, Tick(960));
        // The pressure stays the same distance into the note...
        assert_eq!(
            off.notes[0].expressions[0].tick,
            snapped.notes[0].expressions[0].tick
        );
        // ...and the wheel stays where it was played.
        assert_eq!(off.controls[0].tick, snapped.controls[0].tick);
        assert_eq!(snapped.controls[0].tick, Tick(38));
    }
}
