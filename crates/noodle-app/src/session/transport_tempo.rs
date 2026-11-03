//! The transport's tempo source: tap tempo, MIDI clock in, and writing the
//! live tempo back to the tempo map (see `tempo_source.rs`).

use std::time::Instant;

use noodle_core::{Command, Tick};

use super::{Edit, Session};
use crate::tempo_source::{ClockAction, TempoSource};

/// Sixteenth notes to ticks (960 to a quarter note).
const TICKS_PER_SIXTEENTH: i64 = 240;

impl Session {
    pub fn tempo_source(&self) -> TempoSource {
        self.tempo_source
    }

    /// The live tempo, if the source is not the map and has given one.
    pub fn live_tempo(&self) -> Option<f64> {
        self.live_bpm
    }

    /// Chooses where the tempo comes from. Map goes back to the project's
    /// tempo map. Tap keeps the tempo in force until the next taps. MIDI
    /// clock waits for a clock, keeping the tempo meanwhile.
    pub fn set_tempo_source(&mut self, source: TempoSource) {
        if source == self.tempo_source {
            return;
        }
        self.tempo_source = source;
        self.taps.clear();
        self.clock.reset();
        match source {
            TempoSource::Map => self.live_bpm = None,
            TempoSource::Tap | TempoSource::MidiClock => {
                self.live_bpm = self
                    .live_bpm
                    .or_else(|| Some(self.project.tempo_map().bpm_at(self.playhead())));
            }
        }
        self.apply_live_tempo();
    }

    /// Counts a tap. The first switches the source to Tap, and the third
    /// onwards give the tempo.
    pub fn tap_tempo(&mut self, now: Instant) {
        if self.tempo_source != TempoSource::Tap {
            self.set_tempo_source(TempoSource::Tap);
        }
        if let Some(bpm) = self.taps.tap(now) {
            self.live_bpm = Some(bpm);
            self.apply_live_tempo();
        }
    }

    /// Makes the live tempo a tempo change in the map at the playhead, one
    /// undo step, and goes back to following the map.
    pub fn write_tempo_to_map(&mut self) {
        let Some(bpm) = self.live_bpm else {
            return;
        };
        let tick = self.playhead();
        if let Some((map, _)) = crate::tempo::with_change(self.project.tempo_map(), tick, bpm) {
            self.edit([Edit::Apply(Command::SetTempoMap(map))]);
        }
        self.set_tempo_source(TempoSource::Map);
    }

    /// Pushes the live tempo to the engine, if one is running.
    pub(super) fn apply_live_tempo(&mut self) {
        if let Some(audio) = &mut self.audio {
            audio.controller.set_live_tempo(self.live_bpm);
        }
    }

    /// Reads the MIDI clock heard since the last frame. Messages are read
    /// whatever the source, so that none are queued up for a later switch,
    /// but only act when the source is MIDI clock.
    pub(super) fn follow_clock(&mut self) {
        while let Some((port, micros, event)) = self.clock_tap.next() {
            if self.tempo_source != TempoSource::MidiClock {
                continue;
            }
            match self.clock.hear(&port, micros, event) {
                Some(ClockAction::Tempo(bpm)) => {
                    self.live_bpm = Some(bpm);
                    self.apply_live_tempo();
                }
                Some(ClockAction::Start) => {
                    self.seek(Tick::ZERO);
                    self.play();
                }
                Some(ClockAction::Continue(position)) => {
                    if let Some(sixteenths) = position {
                        self.seek(Tick(i64::from(sixteenths) * TICKS_PER_SIXTEENTH));
                    }
                    self.play();
                }
                Some(ClockAction::Stop) => self.stop(),
                None => {}
            }
        }
    }
}
