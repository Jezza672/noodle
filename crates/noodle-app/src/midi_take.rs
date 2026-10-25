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

use noodle_core::{MidiClip, MidiNote, TICKS_PER_QUARTER, TempoMap, Tick};
use noodle_io::{MidiBus, MidiMessage, MidiReceiver};

/// How recorded notes are snapped to the beat. Only the notes' starts move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
            receiver: bus.subscribe_notes(),
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
    pub fn finish(mut self, finish: &Finish<'_>) -> Option<(Tick, MidiClip)> {
        self.poll(finish.anchor);
        self.clip_so_far(finish)
    }

    /// The clip the take would be if it ended as `finish` says, without
    /// ending it: what is on screen while recording. Notes still held end
    /// at the stop. Call [`poll`](Self::poll) first.
    pub fn clip_so_far(&self, finish: &Finish<'_>) -> Option<(Tick, MidiClip)> {
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
        let stop = tick_at(stop_micros, stop_anchor);

        // (key, start tick, end tick, velocity), starts and ends unquantised.
        let mut open: [[Option<(i64, f32)>; 128]; 16] = [[None; 128]; 16];
        let mut notes: Vec<(u8, i64, i64, f32)> = Vec::new();
        for &(at, [status, key, velocity], anchor) in &self.messages {
            if at > stop_micros {
                break;
            }
            let channel = usize::from(status & 0x0F);
            let key_index = usize::from(key & 0x7F);
            let tick = tick_at(at, anchor);
            let on = status & 0xF0 == 0x90 && velocity > 0;
            let off = status & 0xF0 == 0x80 || (status & 0xF0 == 0x90 && velocity == 0);
            if on {
                // A key struck again before it was let go ends the first.
                if let Some((start, v)) = open[channel][key_index].take() {
                    notes.push((key_index as u8, start, tick, v));
                }
                open[channel][key_index] = Some((tick, f32::from(velocity) / 127.0));
            } else if off && let Some((start, v)) = open[channel][key_index].take() {
                notes.push((key_index as u8, start, tick, v));
            }
        }
        // What is still held ends with the take.
        for channel in &open {
            for (key, held) in channel.iter().enumerate() {
                if let Some((start, v)) = *held {
                    notes.push((key as u8, start, stop, v));
                }
            }
        }
        if notes.is_empty() {
            return None;
        }

        if let Some(grid) = quantise.grid() {
            for note in &mut notes {
                let length = note.2 - note.1;
                let snapped = (note.1 as f64 / grid as f64).round() as i64 * grid;
                note.1 = snapped;
                note.2 = snapped + length;
            }
        }
        // Per key in order, a note ends where the same key next starts, and a
        // second note on the very same tick is dropped.
        notes.sort_by_key(|&(key, start, _, _)| (key, start));
        let mut kept: Vec<(u8, i64, i64, f32)> = Vec::with_capacity(notes.len());
        for note in notes {
            if let Some(last) = kept.last_mut()
                && last.0 == note.0
            {
                if last.1 == note.1 {
                    continue;
                }
                last.2 = last.2.min(note.1);
            }
            kept.push(note);
        }

        let midi_notes: Vec<MidiNote> = kept
            .iter()
            .map(|&(key, start, end, velocity)| {
                let from = (start - clip_start).max(0);
                MidiNote {
                    id: 0,
                    start: Tick(from),
                    length: Tick((end - start).max(1)),
                    key,
                    velocity,
                }
            })
            .collect();
        let last_end = midi_notes
            .iter()
            .map(|n| n.start.0 + n.length.0)
            .max()
            .unwrap_or(0);
        let length = (stop - clip_start).max(last_end).max(1);
        let mut clip = MidiClip::new(Tick(length));
        clip.notes = midi_notes;
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
        let note = clip.notes[0];
        assert_eq!(
            (note.key, note.start, note.length),
            (60, Tick(960), Tick(480))
        );
        assert_eq!(note.velocity, 1.0);
        // The clip runs to the end of the take: 2 seconds is 3840 ticks.
        assert_eq!(clip.length, Tick(3840));
    }

    #[test]
    fn a_take_with_no_notes_makes_no_clip() {
        assert!(take(&[(100, [0xB0, 1, 2])], 1000, Quantise::Off).is_none());
        assert!(take(&[], 1000, Quantise::Off).is_none());
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
        let note = clip.notes[0];
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
}
