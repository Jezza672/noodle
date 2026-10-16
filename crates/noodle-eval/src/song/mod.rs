//! Generated songs: the evaluation's long test material (ROADMAP M8, batch 3).
//!
//! A song is written once, as parts and patterns in beats, and then made into
//! a Noodle project through the normal commands: a track per part, each
//! playing a synth that is a group of primitive nodes (see [`kit`]). The same
//! song can be rendered at any tempo map, so a *native render* at a target
//! tempo exists for every song, and is the answer key for warping the song's
//! source render to that tempo.
//!
//! The committed files in `songs/` are the songs at their own tempo, as
//! projects `noodle render` and the app can open. A test keeps them in step
//! with this code (`UPDATE_SONGS=1 cargo test -p noodle-eval --test songs`
//! rewrites them).

pub mod kit;
pub mod library;
pub mod music;
pub mod node_presets;
mod patch;
pub mod samples;
pub mod system;

use crate::tempo::steady;
use kit::Instrument;
use music::{Bar, Chord, Note, Pattern};
use noodle_core::group::create_track;
use noodle_core::{
    Clip, ClipContent, Command, History, MidiNote, NodeId, Position, Project, TempoMap, Tick,
    group::MIDI,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};
use noodle_nodes::{VOICE_MIX_ID, VOICES_ID, render_project_with_clips};
use patch::{Patch, spec};

pub use library::all;

/// A named stretch of bars, such as the chorus.
#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub name: &'static str,
    pub bars: u32,
}

/// One instrument playing one line.
pub struct Part {
    pub name: &'static str,
    pub instrument: Instrument,
    /// The track's level, in decibels, on its output control.
    pub db: f32,
    pub pan: f32,
    /// What it plays in which section; a section it is not listed in is a
    /// rest.
    pub plays: Vec<(&'static str, Pattern)>,
    /// Effects on the track, after its pan.
    pub fx: Vec<kit::Fx>,
}

/// A whole song.
pub struct Song {
    pub name: &'static str,
    pub bpm: f64,
    pub sections: Vec<Section>,
    /// The chords of each section, one per bar, going round.
    pub chords: Vec<(&'static str, Vec<Chord>)>,
    pub parts: Vec<Part>,
    /// The master level after the mixer, in decibels, which keeps the loudest
    /// moment of the song under full scale.
    pub master_db: f32,
    /// Effects on the mix, before the master level.
    pub master_fx: Vec<kit::Fx>,
    /// The excerpt that joins the CI gate: its first bar and how many bars.
    pub excerpt: (u32, u32),
}

/// A stretch of the song in quarter notes, from `start` up to `end`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub start: f64,
    pub end: f64,
}

impl Range {
    pub fn beats(&self) -> f64 {
        self.end - self.start
    }
}

impl Song {
    pub fn bars(&self) -> u32 {
        self.sections.iter().map(|s| s.bars).sum()
    }

    pub fn beats(&self) -> f64 {
        f64::from(self.bars()) * 4.0
    }

    pub fn seconds(&self) -> f64 {
        self.beats() * 60.0 / self.bpm
    }

    /// The whole song.
    pub fn whole(&self) -> Range {
        Range {
            start: 0.0,
            end: self.beats(),
        }
    }

    /// The excerpt that joins the CI gate.
    pub fn excerpt_range(&self) -> Range {
        let (first, bars) = self.excerpt;
        Range {
            start: f64::from(first) * 4.0,
            end: f64::from(first + bars) * 4.0,
        }
    }

    /// The first bar of each section, from 0.
    pub fn section_starts(&self) -> Vec<(&'static str, u32)> {
        let mut bar = 0;
        self.sections
            .iter()
            .map(|s| {
                let start = (s.name, bar);
                bar += s.bars;
                start
            })
            .collect()
    }

    fn chord(&self, section: &str, index: u32) -> Chord {
        let (_, chords) = self
            .chords
            .iter()
            .find(|(name, _)| *name == section)
            .unwrap_or_else(|| panic!("{}: no chords for {section}", self.name));
        chords[index as usize % chords.len()]
    }

    /// A part's notes, in beats from the start of the song.
    pub fn notes(&self, part: &Part) -> Vec<Note> {
        let mut notes = Vec::new();
        let mut first_bar = 0;
        for section in &self.sections {
            if let Some((_, pattern)) = part.plays.iter().find(|(name, _)| *name == section.name) {
                for index in 0..section.bars {
                    let bar = Bar {
                        index,
                        bars: section.bars,
                        chord: self.chord(section.name, index),
                    };
                    let origin = f64::from(first_bar + index) * 4.0;
                    notes.extend(pattern(&bar).into_iter().map(|n| Note {
                        beat: origin + n.beat,
                        ..n
                    }));
                }
            }
            first_bar += section.bars;
        }
        notes
    }

    /// The song as a project with `map` as its tempo: the notes of `range`
    /// only, moved so the range starts at the beginning of the timeline.
    pub fn project(&self, map: &TempoMap, range: Range) -> Project {
        self.project_of(map, range, |_| true)
    }

    /// Like [`Song::project`] but only the parts `keep` picks play: the other
    /// tracks stay in the project, empty. Used for stems.
    pub fn project_of(
        &self,
        map: &TempoMap,
        range: Range,
        keep: impl Fn(&Part) -> bool,
    ) -> Project {
        let mut project = Project::new();
        let mut history = History::new();
        history
            .apply(&mut project, Command::SetTempoMap(map.clone()))
            .expect("a valid tempo map");

        // The top level: every track into a mixer that goes to the output.
        let mut p = Patch::new(&mut project, &mut history);
        let mix = p.add(
            spec("noodle.util.mix")
                .config("inputs", noodle_core::Value::Int(self.parts.len() as i64)),
            12.0,
            0.0,
        );
        let master = p.add(
            spec("noodle.util.gain").param("gain", self.master_db),
            13.0,
            0.0,
        );
        let out = p.add(spec(OUTPUT_ID), 14.0, 0.0);
        let mixed = kit::chain(&mut p, &self.master_fx, (mix, "out"), 12.5, 1.0);
        p.connect(mixed, (master, "in"));
        p.connect((master, "out"), (out, "in"));

        let mut sources = Vec::new();
        for (i, part) in self.parts.iter().enumerate() {
            let source = add_track(&mut p, part, i);
            p.connect((source.group, "out"), (mix, MIX_INPUTS[i]));
            sources.push((source.input, part));
        }

        // The notes, as a MIDI clip on each track.
        let length = Tick::from_quarters(range.beats());
        // Every part takes a clip ID, kept or not. A note's identity is a hash
        // of its clip ID, so the IDs set the order of notes that start together
        // and so which voice (with its free-running oscillator) each gets: a
        // part sounds the same alone as in the mix only if its clip has the same
        // ID in both.
        for (input, part) in sources {
            let mut clip = Clip::midi(input, Tick::ZERO, length);
            let ClipContent::Midi(midi) = &mut clip.content else {
                unreachable!("a MIDI clip")
            };
            for note in self.notes(part) {
                if note.beat < range.start || note.beat >= range.end {
                    continue;
                }
                let mut n = MidiNote::new(
                    Tick::from_quarters(note.beat - range.start),
                    Tick::from_quarters(note.len),
                    note.key,
                );
                n.velocity = note.vel;
                midi.notes.push(n);
            }
            // Taken even for a part with no notes in `range`, so a part's ID
            // is the same whatever the range.
            let id = project.new_clip_id();
            if midi.notes.is_empty() || !keep(part) {
                continue;
            }
            history
                .apply(&mut project, Command::AddClip { id, clip })
                .expect("a valid clip");
        }
        project
    }

    /// The song at its own tempo, whole: what the committed file holds.
    pub fn source_project(&self) -> Project {
        self.project(&steady(self.bpm), self.whole())
    }

    /// `range` of the song at `map`, as interleaved stereo, rendered offline
    /// by the engine.
    pub fn render(&self, map: &TempoMap, range: Range, rate: u32) -> Result<Vec<f32>, String> {
        self.render_of(map, range, rate, |_| true)
    }

    /// [`Song::render`] of only the parts `keep` picks.
    pub fn render_of(
        &self,
        map: &TempoMap,
        range: Range,
        rate: u32,
        keep: impl Fn(&Part) -> bool,
    ) -> Result<Vec<f32>, String> {
        let project = self.project_of(map, range, keep);
        let frames = map.sample_at(Tick::from_quarters(range.beats()), f64::from(rate)) as usize;
        let settings = Settings {
            sample_rate: rate as f32,
            max_frames: 512,
            channels: 2,
        };
        let mut registry = Registry::with_builtins();
        let rendered = render_project_with_clips(
            &project,
            &mut registry,
            samples::dir().parent().expect("a songs folder"),
            settings,
            frames,
        )
        .map_err(|e| format!("{}: {e}", self.name))?;
        if !rendered.render.diagnostics.is_empty() {
            return Err(format!("{}: {:?}", self.name, rendered.render.diagnostics));
        }
        Ok(rendered.render.samples)
    }
}

const MIX_INPUTS: [&str; 12] = [
    "in1", "in2", "in3", "in4", "in5", "in6", "in7", "in8", "in9", "in10", "in11", "in12",
];

struct Track {
    group: NodeId,
    /// The track input node the clips play on.
    input: NodeId,
}

/// A track for `part`: a track group (as the arrangement view makes) with the
/// instrument inside, wired from the track's MIDI to the group's output.
fn add_track(p: &mut Patch, part: &Part, index: usize) -> Track {
    // `create_track` hands out IDs in order: group, group input, track input,
    // group output.
    let mut ids = Vec::new();
    let position = Position {
        x: 0.0,
        y: index as f32 * 520.0,
    };
    let (group, command) = {
        let project = &mut *p.project;
        create_track(None, position, || {
            let id = project.new_node_id();
            ids.push(id);
            id
        })
    };
    p.history
        .apply(p.project, command)
        .expect("a track can be made");
    let (input, output) = (ids[2], ids[3]);
    set_name(p, group, part.name);
    p.history
        .apply(
            p.project,
            // The output follows the track input's gain.
            Command::SetParam {
                node: input,
                key: noodle_core::group::TRACK_GAIN.into(),
                value: Some(part.db),
            },
        )
        .expect("the track input has a gain");

    p.parent = Some(group);
    let pan = p.add(spec("noodle.util.pan").param("pan", part.pan), 8.0, 1.0);
    match &part.instrument {
        Instrument::Tone(tone) if tone.voices > 1 => {
            let voices = p.add(
                spec(VOICES_ID).config("voices", noodle_core::Value::Int(i64::from(tone.voices))),
                2.0,
                1.0,
            );
            let synth = kit::build(p, &part.instrument, 4.0, 1.0);
            let sum = p.add(spec(VOICE_MIX_ID), 6.0, 1.0);
            p.connect((input, MIDI), (voices, "in"));
            for (from, to) in [
                ("pitch", "pitch"),
                ("gate", "gate"),
                ("velocity", "velocity"),
                ("fade", "fade"),
            ] {
                p.connect((voices, from), (synth, to));
            }
            p.connect((synth, "active"), (voices, "busy"));
            p.connect((synth, "out"), (sum, "in"));
            p.connect((sum, "out"), (pan, "in"));
        }
        Instrument::Rack(_) => {
            let rack = kit::build(p, &part.instrument, 4.0, 1.0);
            p.connect((input, MIDI), (rack, "notes"));
            p.connect((rack, "out"), (pan, "in"));
        }
        _ => {
            let mono = p.add(spec("noodle.event.mono"), 2.0, 1.0);
            let synth = kit::build(p, &part.instrument, 4.0, 1.0);
            p.connect((input, MIDI), (mono, "in"));
            for (from, to) in [
                ("pitch", "pitch"),
                ("gate", "gate"),
                ("velocity", "velocity"),
            ] {
                p.connect((mono, from), (synth, to));
            }
            p.connect((synth, "out"), (pan, "in"));
        }
    }
    let last = kit::chain(p, &part.fx, (pan, "out"), 9.0, 1.0);
    p.connect(last, (output, "p1"));
    p.parent = None;
    Track { group, input }
}

fn set_name(p: &mut Patch, group: NodeId, name: &str) {
    p.history
        .apply(
            p.project,
            Command::SetConfig {
                node: group,
                key: "name".into(),
                value: Some(noodle_core::Value::Text(name.into())),
            },
        )
        .expect("a group has a name");
}
