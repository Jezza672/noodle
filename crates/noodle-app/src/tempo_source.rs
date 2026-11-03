//! Where the transport's tempo comes from: the tempo map, taps, or an
//! external MIDI clock. The pure parts live here, the session's wiring in
//! `session/transport_tempo.rs`.
//!
//! Tap and clock set a *live tempo* on the engine that stands in for the
//! map. It is session state and not a command, so tapping adds nothing to the
//! undo history and invalidates no cache keyed on the tempo map. "Write to
//! map" turns it into a [`SetTempoMap`](noodle_core::Command) when the user
//! wants to keep it.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use noodle_io::ClockEvent;

/// The lowest and highest live tempo, in beats per minute.
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 400.0;

/// Where the tempo comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TempoSource {
    /// The project's tempo map.
    #[default]
    Map,
    /// Taps on the Tap button or the `T` key.
    Tap,
    /// An external MIDI clock, which also starts and stops the transport.
    MidiClock,
}

impl TempoSource {
    pub const ALL: [TempoSource; 3] = [Self::Map, Self::Tap, Self::MidiClock];

    pub fn label(self) -> &'static str {
        match self {
            Self::Map => "Map",
            Self::Tap => "Tap",
            Self::MidiClock => "MIDI clock",
        }
    }
}

/// Taps are forgotten after this long without one.
const TAP_RESET: Duration = Duration::from_secs(2);
/// The most taps the tempo is averaged over.
const TAPS: usize = 8;

/// The tempo a run of taps makes.
#[derive(Clone, Debug, Default)]
pub struct Taps(Vec<Instant>);

impl Taps {
    /// Counts a tap at `now` and says the tempo the taps make, once there are
    /// three of them and the tempo is in range.
    pub fn tap(&mut self, now: Instant) -> Option<f64> {
        if self
            .0
            .last()
            .is_some_and(|last| now.duration_since(*last) > TAP_RESET)
        {
            self.0.clear();
        }
        self.0.push(now);
        if self.0.len() > TAPS {
            self.0.remove(0);
        }
        if self.0.len() < 3 {
            return None;
        }
        let span = now.duration_since(self.0[0]).as_secs_f64();
        let bpm = 60.0 * (self.0.len() - 1) as f64 / span;
        (MIN_BPM..=MAX_BPM).contains(&bpm).then_some(bpm)
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// Pulses to a quarter note in MIDI clock.
const PPQN: usize = 24;
/// A clock quieter than this has stopped.
const SILENCE_MICROS: u64 = 1_000_000;
/// A new estimate is published only when it differs from the last by this
/// much, so a steady clock doesn't rebuild the engine's tempo table over
/// jitter.
const HYSTERESIS: f64 = 0.1;

/// What following a clock asks the session to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClockAction {
    /// Use this tempo.
    Tempo(f64),
    /// Play from the start.
    Start,
    /// Play on, from the sixteenth note position the clock gave if it did.
    Continue(Option<u16>),
    Stop,
}

/// Follows a MIDI clock. It locks to the first port that sends timing and
/// keeps to it until that has been silent for a second. The tempo is the
/// average over the last beat of pulses (24 of them), which smooths a
/// driver's timing jitter without lagging a tempo change by more than a
/// beat. The beat phase is not locked: Start puts the playhead at the start,
/// and the tempo then keeps it near enough.
#[derive(Debug, Default)]
pub struct ClockFollower {
    port: Option<String>,
    /// Arrival times of the last 25 pulses, the 24 between them a beat.
    pulses: VecDeque<u64>,
    published: Option<f64>,
    /// A position the clock gave, for the Continue that follows.
    song_position: Option<u16>,
}

impl ClockFollower {
    /// Feeds one message in, and says what to do about it.
    pub fn hear(&mut self, port: &str, micros: u64, event: ClockEvent) -> Option<ClockAction> {
        if let Some(own) = &self.port {
            let silent = self
                .pulses
                .back()
                .is_none_or(|&last| micros.saturating_sub(last) > SILENCE_MICROS);
            if !noodle_io::same_port(own, port) && !silent {
                return None;
            }
            if silent && !noodle_io::same_port(own, port) {
                self.reset();
            }
        }
        self.port = Some(port.to_string());
        match event {
            ClockEvent::Tick => {
                if self
                    .pulses
                    .back()
                    .is_some_and(|&last| micros.saturating_sub(last) > SILENCE_MICROS)
                {
                    self.pulses.clear();
                }
                self.pulses.push_back(micros);
                if self.pulses.len() > PPQN + 1 {
                    self.pulses.pop_front();
                }
                if self.pulses.len() <= PPQN {
                    return None;
                }
                let span = micros.saturating_sub(self.pulses[0]) as f64;
                if span <= 0.0 {
                    return None;
                }
                let bpm = 60e6 / span;
                if !(MIN_BPM..=MAX_BPM).contains(&bpm) {
                    return None;
                }
                if self
                    .published
                    .is_some_and(|last| (last - bpm).abs() < HYSTERESIS)
                {
                    return None;
                }
                self.published = Some(bpm);
                Some(ClockAction::Tempo(bpm))
            }
            ClockEvent::Start => {
                self.song_position = None;
                Some(ClockAction::Start)
            }
            ClockEvent::Continue => Some(ClockAction::Continue(self.song_position.take())),
            ClockEvent::Stop => Some(ClockAction::Stop),
            ClockEvent::SongPosition(sixteenths) => {
                self.song_position = Some(sixteenths);
                None
            }
        }
    }

    /// Forgets the clock, so the next one heard is followed afresh.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pulses(follower: &mut ClockFollower, from: u64, count: usize, bpm: f64) -> Vec<ClockAction> {
        let period = 60e6 / bpm / 24.0;
        (0..count)
            .filter_map(|i| {
                follower.hear(
                    "Drums",
                    from + (i as f64 * period).round() as u64,
                    ClockEvent::Tick,
                )
            })
            .collect()
    }

    #[test]
    fn taps_make_a_tempo_from_the_third() {
        let mut taps = Taps::default();
        let start = Instant::now();
        let beat = Duration::from_millis(500);
        assert_eq!(taps.tap(start), None);
        assert_eq!(taps.tap(start + beat), None);
        let bpm = taps.tap(start + 2 * beat).unwrap();
        assert!((bpm - 120.0).abs() < 1e-6, "{bpm}");
        // A pause starts over.
        assert_eq!(taps.tap(start + 10 * beat), None);
    }

    #[test]
    fn taps_outside_the_tempo_range_are_ignored() {
        let mut taps = Taps::default();
        let start = Instant::now();
        taps.tap(start);
        taps.tap(start + Duration::from_millis(20));
        // 50 ms a beat is 1200 bpm.
        assert_eq!(taps.tap(start + Duration::from_millis(40)), None);
    }

    #[test]
    fn a_steady_clock_gives_its_tempo_once() {
        let mut follower = ClockFollower::default();
        let seen = pulses(&mut follower, 5_000, 100, 133.0);
        assert_eq!(seen.len(), 1, "{seen:?}");
        let ClockAction::Tempo(bpm) = seen[0] else {
            panic!("{seen:?}")
        };
        assert!((bpm - 133.0).abs() < 0.01, "{bpm}");
    }

    #[test]
    fn jitter_is_smoothed_and_a_tempo_change_is_followed() {
        let mut follower = ClockFollower::default();
        let period = 60e6 / 120.0 / 24.0;
        let mut found = Vec::new();
        for i in 0..200 {
            // Up to a millisecond of jitter either way.
            let jitter = if i % 2 == 0 { 900.0 } else { -900.0 };
            let at = (10_000.0 + i as f64 * period + jitter) as u64;
            found.extend(follower.hear("Drums", at, ClockEvent::Tick));
        }
        let last = found.last().copied();
        let Some(ClockAction::Tempo(bpm)) = last else {
            panic!("{found:?}")
        };
        assert!((bpm - 120.0).abs() < 1.0, "{bpm}");
        let from = 10_000 + (200.0 * period) as u64;
        let faster = pulses(&mut follower, from, 80, 150.0);
        let Some(ClockAction::Tempo(bpm)) = faster.last().copied() else {
            panic!("{faster:?}")
        };
        assert!((bpm - 150.0).abs() < 0.5, "{bpm}");
    }

    #[test]
    fn start_stop_and_song_position_are_passed_on() {
        let mut follower = ClockFollower::default();
        assert_eq!(
            follower.hear("Drums", 0, ClockEvent::Start),
            Some(ClockAction::Start)
        );
        assert_eq!(
            follower.hear("Drums", 1, ClockEvent::SongPosition(16)),
            None
        );
        assert_eq!(
            follower.hear("Drums", 2, ClockEvent::Continue),
            Some(ClockAction::Continue(Some(16)))
        );
        // The position is used once.
        assert_eq!(
            follower.hear("Drums", 3, ClockEvent::Continue),
            Some(ClockAction::Continue(None))
        );
        assert_eq!(
            follower.hear("Drums", 4, ClockEvent::Stop),
            Some(ClockAction::Stop)
        );
    }

    #[test]
    fn a_second_port_is_ignored_until_the_first_goes_quiet() {
        let mut follower = ClockFollower::default();
        pulses(&mut follower, 0, 30, 120.0);
        let now = 30 * 20_833;
        assert_eq!(follower.hear("Other", now, ClockEvent::Stop), None);
        // A second later the first has gone, and the other is followed.
        let later = now + 2_000_000;
        assert_eq!(
            follower.hear("Other", later, ClockEvent::Stop),
            Some(ClockAction::Stop)
        );
    }
}
