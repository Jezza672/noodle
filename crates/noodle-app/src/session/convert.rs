//! Clip menu actions that make new material from an audio clip: Convert to
//! MIDI (a hummed line or a bass becomes notes) and Slice to Sampler (a loop
//! becomes pads). Both are one undo step.

use super::*;
use crate::timeline::add_track;
use noodle_core::group::{GROUP_OUTPUT, PORT_NAME, port_key};
use noodle_core::{
    Clip, ClipContent, ClipWarp, Config, Endpoint, MidiClip, MidiNote, Node, Position, Value,
};
use noodle_io::{Transcription, decode_file, transcribe};
use noodle_nodes::{
    MONO_NOTE_ID, SAMPLE_INPUT_ID, SAMPLE_KEY, SAMPLER_ID, SLICE_ID, SLICE_SENSITIVITY_KEY,
};

/// The factory synth Convert to MIDI plays its notes with.
const SYNTH: &str = "system:template:mono-synth";

/// What quantising a clip's transients does to its warp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuantiseHits {
    /// The grid, in beats of the clip.
    pub grid: f64,
    /// The strength, 0 to 1: how far each hit is pulled onto the grid.
    pub amount: f64,
    /// How the slices are joined once they have moved.
    pub join: SliceJoin,
}

impl QuantiseHits {
    fn apply(self, warp: &mut ClipWarp) {
        let settings = warp.quantise.get_or_insert_with(Default::default);
        settings.grid = self.grid;
        settings.amount = self.amount.clamp(0.0, 1.0);
        if let Some(mode) = self.join.mode() {
            warp.mode = mode;
        }
    }
}

/// How the slices of a quantised clip join where they have been moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliceJoin {
    /// Leave the warp's mode as it is.
    Keep,
    /// Slices mode: each slice plays at its natural speed where the hit now
    /// is, and slices that overlap are crossfaded. Where a slice moved late
    /// it leaves a gap, which the slice before runs on into and then goes
    /// silent.
    Crossfade,
    /// Beats mode: the audio between two hits is stretched to fill the gap
    /// the move left (or squeezed to fit), so nothing overlaps and nothing is
    /// silent.
    FillGaps,
}

impl SliceJoin {
    pub const ALL: [Self; 3] = [Self::Crossfade, Self::FillGaps, Self::Keep];

    pub fn name(self) -> &'static str {
        match self {
            Self::Keep => "Keep the warp mode",
            Self::Crossfade => "Crossfade slices",
            Self::FillGaps => "Fill gaps by stretching",
        }
    }

    pub fn mode(self) -> Option<noodle_core::WarpMode> {
        match self {
            Self::Keep => None,
            Self::Crossfade => Some(noodle_core::WarpMode::Slices),
            Self::FillGaps => Some(noodle_core::WarpMode::Beats),
        }
    }
}

/// Why pinning came to nothing.
enum PinError {
    /// Every hit already has a marker.
    NothingNew,
    Other(String),
}

/// How readily Convert to MIDI hears a note, as the menu offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detect {
    /// Fewer notes: only clear, loud pitches.
    Clean,
    Normal,
    /// More notes: quiet and rough sounds count too.
    More,
}

impl Detect {
    pub const ALL: [Detect; 3] = [Detect::Clean, Detect::Normal, Detect::More];

    pub fn sensitivity(self) -> f32 {
        match self {
            Detect::Clean => 0.25,
            Detect::Normal => noodle_io::DEFAULT_SENSITIVITY,
            Detect::More => 0.8,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Detect::Clean => "Fewer notes",
            Detect::Normal => "Normal",
            Detect::More => "More notes",
        }
    }
}

/// The shortest a converted note is, in ticks.
const MIN_NOTE: i64 = 60;

impl Session {
    /// Converts the monophonic audio of `clip` to MIDI notes.
    ///
    /// The conversion is destructive: it keeps nothing of the audio's
    /// analysis, and the notes are plain notes to edit. With `replace` (a
    /// MIDI clip), that clip's notes are swapped for the new ones in place, so
    /// converting again at another sensitivity replaces the first attempt;
    /// without it, the notes go on a new track that plays them with the
    /// Mono Synth. Either is one undo step.
    pub fn convert_to_midi(&mut self, clip: ClipId, detect: Detect, replace: Option<ClipId>) {
        match self.conversion(clip, detect, replace) {
            Ok((command, notes)) => {
                self.edit([Edit::Apply(command)]);
                self.message = Some(format!(
                    "Converted to {notes} note{}",
                    if notes == 1 { "" } else { "s" }
                ));
            }
            Err(why) => self.message = Some(why),
        }
    }

    fn conversion(
        &self,
        id: ClipId,
        detect: Detect,
        replace: Option<ClipId>,
    ) -> Result<(Command, usize), String> {
        let project = &self.project;
        let source = project.clip(id).ok_or("That clip is gone")?;
        let audio = source
            .as_audio()
            .ok_or("Only audio clips can be converted")?;
        let path = audio.file(self.directory().unwrap_or(Path::new("")));
        let file =
            decode_file(&path).map_err(|e| format!("Couldn't read {}: {e}", audio.source))?;
        let channels = file.channels.max(1);
        let rate = file.sample_rate;
        let frames = file.samples.len() / channels;
        let from = (audio.offset as usize).min(frames);
        let to = ((audio.offset + audio.length) as usize).min(frames);
        let mono: Vec<f32> = (from..to)
            .map(|f| {
                file.samples[f * channels..(f + 1) * channels]
                    .iter()
                    .sum::<f32>()
                    / channels as f32
            })
            .collect();
        let found = transcribe(
            &mono,
            rate,
            &Transcription {
                sensitivity: detect.sensitivity(),
                ..Transcription::default()
            },
        );
        if found.is_empty() {
            return Err(match detect {
                Detect::More => "No notes found: is it a single, pitched voice?".into(),
                _ => "No notes found. Try \"More notes\".".into(),
            });
        }
        let map = project.tempo_map();
        let warp = project.warp_of(id).map(|(_, w)| w);
        let tick_at = |frame: u64| {
            crate::timeline::clips::tick_of_frame(map, source, audio.offset + frame, rate, warp)
                .unwrap_or(source.start)
        };
        let notes: Vec<MidiNote> = found
            .iter()
            .map(|n| {
                let start = Tick((tick_at(n.start).0 - source.start.0).max(0));
                let end = Tick(tick_at(n.start + n.length).0 - source.start.0);
                let mut note = MidiNote::new(start, Tick((end.0 - start.0).max(MIN_NOTE)), n.key);
                note.velocity = f32::from(n.velocity) / 127.0;
                note
            })
            .collect();
        let count = notes.len();
        let end = crate::timeline::clips::clip_end(map, source, rate, warp);
        let length = Tick(
            (end.0 - source.start.0).max(
                notes
                    .iter()
                    .map(|n| n.start.0 + n.length.0)
                    .max()
                    .unwrap_or(0),
            ),
        );
        let content = ClipContent::Midi(MidiClip {
            length,
            notes,
            loop_on: false,
            loop_start: Tick::ZERO,
            loop_len: Tick::ZERO,
            controls: Vec::new(),
            takes: None,
            humanise_timing: Tick::ZERO,
            humanise_velocity: 0.0,
        });

        if let Some(target) = replace {
            let old = project.clip(target).ok_or("The MIDI clip is gone")?;
            if old.as_midi().is_none() {
                return Err("Pick a MIDI clip to replace".into());
            }
            let clip = Clip {
                node: old.node,
                start: source.start,
                content,
            };
            return Ok((Command::SetClip { id: target, clip }, count));
        }

        // A new track under the source, playing the notes with a mono synth.
        let mut scratch = project.clone();
        let (track, input) = add_track::with_input(scratch.graph(), || self.new_node_id());
        track
            .clone()
            .apply(&mut scratch)
            .map_err(|e| e.to_string())?;
        let group = scratch
            .graph()
            .node(input)
            .and_then(|n| n.parent)
            .ok_or("the new track has no group")?;
        let name = Path::new(&audio.source)
            .file_stem()
            .map_or_else(|| "clip".to_owned(), |s| s.to_string_lossy().into_owned());
        let mut commands = vec![
            track,
            Command::SetConfig {
                node: group,
                key: PORT_NAME.into(),
                value: Some(Value::Text(format!("{name} (MIDI)"))),
            },
        ];
        let output = scratch
            .graph()
            .children(Some(group))
            .find(|(_, n)| n.type_id == GROUP_OUTPUT)
            .map(|(id, _)| id)
            .ok_or("the new track has no output")?;
        let mono = self.new_node_id();
        let mut note = Node::new(MONO_NOTE_ID).at(300.0, 0.0);
        note.parent = Some(group);
        commands.push(Command::AddNode {
            id: mono,
            node: note,
        });
        let synth = self
            .library()
            .template(SYNTH)
            .ok_or("The Mono Synth is missing from the library")?;
        let (synth_group, add_synth) =
            synth.instantiate(SYNTH, Some(group), Position { x: 600.0, y: 0.0 }, || {
                self.new_node_id()
            });
        commands.push(add_synth);
        let wire = |from: Endpoint, to: Endpoint| noodle_core::spare::wire(from, to);
        commands.push(wire(
            Endpoint::new(input, "midi"),
            Endpoint::new(mono, "in"),
        ));
        for port in ["pitch", "gate", "velocity"] {
            commands.push(wire(
                Endpoint::new(mono, port),
                Endpoint::new(synth_group, port),
            ));
        }
        commands.push(wire(
            Endpoint::new(synth_group, "out"),
            Endpoint::new(output, port_key(1)),
        ));
        commands.push(Command::AddClip {
            id: project.next_clip_id(),
            clip: Clip {
                node: input,
                start: source.start,
                content,
            },
        });
        Ok((Command::Batch(commands), count))
    }

    /// Pins the transients of `clip` and of every clip linked to it as warp
    /// markers, where they play now, in one undo step: the hits are found in
    /// each clip's own file at `percent` sensitivity (the scale Slice and
    /// Convert to MIDI use), and hits within a few milliseconds of each other
    /// in different clips count as one, so the close mic and the overheads
    /// get one marker per hit and stay in phase when the markers are moved.
    pub fn pin_transients(&mut self, clip: ClipId, percent: u8) {
        match self.pinning(clip, percent) {
            Ok((command, pinned)) => {
                self.edit([Edit::Apply(command)]);
                self.message = Some(format!(
                    "Pinned {pinned} transient{}",
                    if pinned == 1 { "" } else { "s" }
                ));
            }
            Err(why) => self.message = Some(why),
        }
    }

    /// Quantises the transients of `clip` and of every clip linked to it, in
    /// one undo step: the hits that have no marker yet are pinned where they
    /// play now, then warp quantise is turned on (or its grid and strength
    /// changed) and the warp's mode is set to `join`. `amount` is the
    /// strength, 0 to 1: how far each marker is pulled onto the grid.
    pub fn quantise_transients(&mut self, clip: ClipId, percent: u8, quantise: QuantiseHits) {
        let result = match self.pinning_with(clip, percent, |warp| quantise.apply(warp)) {
            // Already pinned is not a failure here: the quantise still applies.
            Err(PinError::NothingNew) => self
                .project
                .warp_edit(clip, |warp| quantise.apply(warp))
                .map(|command| (command, 0))
                .map_err(|e| e.to_string()),
            Ok(done) => Ok(done),
            Err(PinError::Other(why)) => Err(why),
        };
        match result {
            Ok((command, pinned)) => {
                self.edit([Edit::Apply(command)]);
                let pins = match pinned {
                    0 => String::new(),
                    1 => ", pinning 1 new transient".into(),
                    n => format!(", pinning {n} new transients"),
                };
                self.message = Some(format!(
                    "Quantised to {} at {:.0} %{pins}",
                    crate::clip_panel::grid_name(quantise.grid),
                    quantise.amount * 100.0,
                ));
            }
            Err(why) => self.message = Some(why),
        }
    }

    fn pinning(&self, id: ClipId, percent: u8) -> Result<(Command, usize), String> {
        self.pinning_with(id, percent, |_| {}).map_err(|e| match e {
            PinError::NothingNew => "No new transients to pin".into(),
            PinError::Other(why) => why,
        })
    }

    fn pinning_with(
        &self,
        id: ClipId,
        percent: u8,
        after: impl FnOnce(&mut ClipWarp),
    ) -> Result<(Command, usize), PinError> {
        let other = |why: String| PinError::Other(why);
        let project = &self.project;
        let sensitivity = f32::from(percent.min(100)) / 100.0;
        let mut moments: Vec<f64> = Vec::new();
        for member in project.linked_with(id) {
            let Some(audio) = project.clip(member).and_then(|c| c.as_audio()) else {
                continue;
            };
            let path = audio.file(self.directory().unwrap_or(Path::new("")));
            let file = decode_file(&path)
                .map_err(|e| other(format!("Couldn't read {}: {e}", audio.source)))?;
            let channels = file.channels.max(1);
            let frames = file.samples.len() / channels;
            let from = (audio.offset as usize).min(frames);
            let to = ((audio.offset + audio.length) as usize).min(frames);
            let mono: Vec<f32> = (from..to)
                .map(|f| {
                    file.samples[f * channels..(f + 1) * channels]
                        .iter()
                        .sum::<f32>()
                        / channels as f32
                })
                .collect();
            let rate = f64::from(file.sample_rate);
            moments.extend(
                noodle_io::detect_onsets_with(&mono, file.sample_rate, sensitivity)
                    .into_iter()
                    .map(|frame| (from as u64 + frame) as f64 / rate),
            );
        }
        let mut pinned = 0;
        let command = project
            .warp_edit(id, |warp| {
                pinned = warp.pin_transients(&moments);
                after(warp);
            })
            .map_err(|e| other(e.to_string()))?;
        if pinned == 0 {
            return Err(PinError::NothingNew);
        }
        Ok((command, pinned))
    }

    /// Slices the audio of `clip` at its transients onto a new track that
    /// plays the slices from the keys: a Sample node (the clip's part of the
    /// file) into a Slice node into a Sampler with keytrack off, inside a new
    /// track fed by the track's own MIDI. One undo step.
    pub fn slice_to_sampler(&mut self, clip: ClipId) {
        match self.slicer_command(clip) {
            Ok(command) => {
                self.edit([Edit::Apply(command)]);
                self.message = Some("Sliced onto a new Sampler track".into());
            }
            Err(why) => self.message = Some(why),
        }
    }

    fn slicer_command(&self, clip: ClipId) -> Result<Command, String> {
        let dropped = crate::sample_drop::from_clip(self, clip)?;
        let project = &self.project;
        let source = project.clip(clip).ok_or("That clip is gone")?;
        let audio = source.as_audio().ok_or("Only audio clips can be sliced")?;
        let mut scratch = project.clone();
        let (track, input) = add_track::with_input(scratch.graph(), || self.new_node_id());
        track
            .clone()
            .apply(&mut scratch)
            .map_err(|e| e.to_string())?;
        let group = scratch
            .graph()
            .node(input)
            .and_then(|n| n.parent)
            .ok_or("the new track has no group")?;
        let output = scratch
            .graph()
            .children(Some(group))
            .find(|(_, n)| n.type_id == GROUP_OUTPUT)
            .map(|(id, _)| id)
            .ok_or("the new track has no output")?;
        let name = Path::new(&audio.source)
            .file_stem()
            .map_or_else(|| "clip".to_owned(), |s| s.to_string_lossy().into_owned());
        let mut commands = vec![
            track,
            Command::SetConfig {
                node: group,
                key: PORT_NAME.into(),
                value: Some(Value::Text(format!("{name} (slices)"))),
            },
        ];
        let mut add = |type_id: &str, x: f32, config: Config, params: &[(&str, f32)]| {
            let id = self.new_node_id();
            let mut node = Node::new(type_id).with_config(config).at(x, 0.0);
            for (key, value) in params {
                node = node.with_param(*key, *value);
            }
            node.parent = Some(group);
            commands.push(Command::AddNode { id, node });
            id
        };
        let sample = add(
            SAMPLE_INPUT_ID,
            0.0,
            Config::new().with(SAMPLE_KEY, Value::Text(dropped.reference.to_text())),
            &[],
        );
        let slice = add(
            SLICE_ID,
            300.0,
            Config::new().with(SLICE_SENSITIVITY_KEY, Value::Int(50)),
            &[],
        );
        let sampler = add(SAMPLER_ID, 600.0, Config::new(), &[("keytrack", 0.0)]);
        let wire = |from: Endpoint, to: Endpoint| noodle_core::spare::wire(from, to);
        commands.push(wire(
            Endpoint::new(sample, "sample"),
            Endpoint::new(slice, "sample"),
        ));
        commands.push(wire(
            Endpoint::new(slice, "slices"),
            Endpoint::new(sampler, "sample"),
        ));
        commands.push(wire(
            Endpoint::new(input, "midi"),
            Endpoint::new(sampler, "in"),
        ));
        commands.push(wire(
            Endpoint::new(sampler, "out"),
            Endpoint::new(output, port_key(1)),
        ));
        Ok(Command::Batch(commands))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// A saved project with a track holding a clip of `file`, a mono WAV of
    /// `samples`.
    fn rig(dir: &Path, samples: &[f32]) -> (Session, ClipId, NodeId) {
        let mut session = Session::new(crate::session::Nodes::all());
        let (track, input) =
            add_track::with_input(session.project().graph(), || session.new_node_id());
        session.edit([Edit::Apply(track)]);
        noodle_io::write_wav(&dir.join("hum.wav"), samples, 1, RATE).unwrap();
        let id = session.project().next_clip_id();
        let clip = Clip::audio(input, Tick(0), "hum.wav", samples.len() as u64);
        session.edit([Edit::Apply(Command::AddClip { id, clip })]);
        assert!(session.save_as(&dir.join("project.ron")));
        (session, id, input)
    }

    fn voice(hz: f32, seconds: f32) -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                0.3 * (std::f32::consts::TAU * hz * t).sin()
                    + 0.15 * (std::f32::consts::TAU * 2.0 * hz * t).sin()
            })
            .collect()
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (seconds * RATE as f32) as usize]
    }

    /// C4, a rest, then E4, at 120 bpm: a quarter note is half a second.
    fn phrase() -> Vec<f32> {
        let mut x = silence(0.1);
        x.extend(voice(261.63, 0.5));
        x.extend(silence(0.2));
        x.extend(voice(329.63, 0.5));
        x.extend(silence(0.1));
        x
    }

    fn midi_clips(session: &Session) -> Vec<(ClipId, Clip)> {
        session
            .project()
            .clips()
            .filter(|(_, c)| c.as_midi().is_some())
            .map(|(id, c)| (id, c.clone()))
            .collect()
    }

    #[test]
    fn converting_a_phrase_makes_a_track_that_plays_its_notes_in_one_undo() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), &phrase());
        let tracks_before = crate::timeline::tracks(session.project()).len();
        session.convert_to_midi(id, Detect::Normal, None);
        assert_eq!(session.message(), Some("Converted to 2 notes"));
        assert_eq!(
            crate::timeline::tracks(session.project()).len(),
            tracks_before + 1
        );
        let clips = midi_clips(&session);
        assert_eq!(clips.len(), 1);
        let midi = clips[0].1.as_midi().unwrap();
        let mut notes = midi.notes.clone();
        notes.sort_by_key(|n| n.start);
        assert_eq!(notes.iter().map(|n| n.key).collect::<Vec<_>>(), [60, 64]);
        // 120 bpm: 960 ticks a half second. The first starts at 0.1 s.
        assert!((notes[0].start.0 - 192).abs() < 60, "{:?}", notes[0]);
        assert!((notes[0].length.0 - 960).abs() < 120, "{:?}", notes[0]);
        assert!((notes[1].start.0 - 1_536).abs() < 60, "{:?}", notes[1]);

        // The new track plays: the engine takes the project without complaint.
        assert!(session.clip_problems().is_empty());
        // Its track input feeds a Mono Note into the synth, inside the group.
        let graph = session.project().graph();
        let input = clips[0].1.node;
        let group = graph.node(input).and_then(|n| n.parent).unwrap();
        assert!(
            graph
                .children(Some(group))
                .any(|(_, n)| n.type_id == MONO_NOTE_ID)
        );
        let fed = graph
            .connections()
            .filter(|c| c.from.node == input && c.from.port == "midi")
            .count();
        assert_eq!(fed, 1);

        session.undo();
        assert!(midi_clips(&session).is_empty());
        assert_eq!(
            crate::timeline::tracks(session.project()).len(),
            tracks_before
        );
    }

    #[test]
    fn converting_again_replaces_the_clip_and_the_audio_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), &phrase());
        session.convert_to_midi(id, Detect::Normal, None);
        let (midi_id, _) = midi_clips(&session).remove(0);
        // Edit a note by hand: the replacement discards that, being destructive.
        let before = session.project().clone();
        session.convert_to_midi(id, Detect::Clean, Some(midi_id));
        let clips = midi_clips(&session);
        assert_eq!(clips.len(), 1, "replaced, not added to");
        assert_eq!(clips[0].0, midi_id);
        assert_eq!(
            crate::timeline::tracks(session.project()).len(),
            crate::timeline::tracks(&before).len()
        );
        assert_eq!(session.project().clip(id), before.clip(id));
        session.undo();
        assert_eq!(session.project().clip(midi_id), before.clip(midi_id));
    }

    #[test]
    fn noise_has_no_notes_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = 9u32;
        let noise: Vec<f32> = (0..RATE as usize)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                ((state >> 8) as f32 / (1 << 23) as f32 - 1.0) * 0.3
            })
            .collect();
        let (mut session, id, _) = rig(dir.path(), &noise);
        let before = session.project().clone();
        session.convert_to_midi(id, Detect::Normal, None);
        assert!(session.message().unwrap().starts_with("No notes found"));
        assert_eq!(*session.project(), before);
    }

    #[test]
    fn slicing_a_clip_builds_a_sampler_track_that_slices_by_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut x = silence(2.0);
        for n in 0..4 {
            let at = 2_000 + n * RATE as usize / 2;
            for i in 0..3_000 {
                let t = i as f32 / RATE as f32;
                x[at + i] += 0.8
                    * (std::f32::consts::TAU * (200.0 + 80.0 * n as f32) * t).sin()
                    * (-t * 50.0).exp();
            }
        }
        let (mut session, id, _) = rig(dir.path(), &x);
        let tracks_before = crate::timeline::tracks(session.project()).len();
        session.slice_to_sampler(id);
        assert_eq!(session.message(), Some("Sliced onto a new Sampler track"));
        assert_eq!(
            crate::timeline::tracks(session.project()).len(),
            tracks_before + 1
        );
        let graph = session.project().graph();
        let of = |ty: &str| graph.nodes().find(|(_, n)| n.type_id == ty).map(|(i, _)| i);
        let (sample, slice, sampler) = (
            of(SAMPLE_INPUT_ID).unwrap(),
            of(SLICE_ID).unwrap(),
            of(SAMPLER_ID).unwrap(),
        );
        let wired = |from, fp: &str, to, tp: &str| {
            graph
                .connections()
                .any(|c| c.from == Endpoint::new(from, fp) && c.to == Endpoint::new(to, tp))
        };
        assert!(wired(sample, "sample", slice, "sample"));
        assert!(wired(slice, "slices", sampler, "sample"));
        // The Sampler gets the slices when the project compiles: one a hit.
        let registry = session.registry();
        let (schedule, diagnostics) = noodle_engine::compile(graph, registry);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let node = schedule.nodes.iter().find(|n| n.id == sampler).unwrap();
        let Some(Value::Text(set)) = node.config.get(SAMPLE_KEY) else {
            panic!("the sampler has no sample")
        };
        assert_eq!(noodle_nodes::parse_set(set).len(), 4, "{set}");
        session.undo();
        assert_eq!(
            crate::timeline::tracks(session.project()).len(),
            tracks_before
        );
    }

    /// Clicks at `times` seconds on a quiet bed.
    fn hits(times: &[f32], seconds: f32) -> Vec<f32> {
        let mut x = silence(seconds);
        for &t in times {
            let at = (t * RATE as f32) as usize;
            for i in 0..2_000 {
                let u = i as f32 / RATE as f32;
                x[at + i] += 0.8 * (std::f32::consts::TAU * 300.0 * u).sin() * (-u * 60.0).exp();
            }
        }
        x
    }

    #[test]
    fn pinning_across_a_link_pins_each_hit_once_even_when_the_mics_hear_it_apart() {
        let dir = tempfile::tempdir().unwrap();
        let close = hits(&[0.5, 1.0, 1.5], 2.0);
        // The overheads hear every hit 3 ms later, and one the close mic missed.
        let over = hits(&[0.503, 1.003, 1.503, 1.75], 2.0);
        let (mut session, a, input_a) = rig(dir.path(), &close);
        noodle_io::write_wav(&dir.path().join("over.wav"), &over, 1, RATE).unwrap();
        let (track, input_b) =
            add_track::with_input(session.project().graph(), || session.new_node_id());
        session.edit([Edit::Apply(track)]);
        let b = session.project().next_clip_id();
        session.edit([Edit::Apply(Command::AddClip {
            id: b,
            clip: Clip::audio(input_b, Tick(0), "over.wav", over.len() as u64),
        })]);
        let link = session.project().link_command(&[a, b]).unwrap();
        session.edit([Edit::Apply(link)]);
        let _ = input_a;

        session.pin_transients(a, 50);
        assert_eq!(session.message(), Some("Pinned 4 transients"));
        // Both clips share the one warp, with a marker at each distinct hit.
        let (warp_a, warp) = session.project().warp_of(a).unwrap();
        assert_eq!(session.project().warp_of(b).unwrap().0, warp_a);
        let seconds: Vec<f64> = warp.markers.iter().map(|m| m.seconds).collect();
        assert_eq!(seconds.len(), 4, "{seconds:?}");
        for (got, want) in seconds.iter().zip([0.5, 1.0, 1.5, 1.75]) {
            assert!((got - want).abs() < 0.004, "{seconds:?}");
        }
        // Pinning again finds nothing new and changes nothing.
        let before = session.project().clone();
        session.pin_transients(a, 50);
        assert_eq!(session.message(), Some("No new transients to pin"));
        assert_eq!(*session.project(), before);
        session.undo();
        assert!(
            session
                .project()
                .warp_of(a)
                .is_none_or(|(_, w)| w.markers.is_empty())
        );
    }

    #[test]
    fn quantising_transients_pins_and_quantises_at_a_strength_in_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let x = hits(&[0.52, 1.03, 1.49], 2.0);
        let (mut session, id, _) = rig(dir.path(), &x);
        let before = session.project().clone();
        let quantise = QuantiseHits {
            grid: 1.0,
            amount: 0.5,
            join: SliceJoin::Crossfade,
        };
        session.quantise_transients(id, 50, quantise);
        assert!(
            session
                .message()
                .is_some_and(|m| m.starts_with("Quantised to 1/4 note at 50 %, pinning 3")),
            "{:?}",
            session.message()
        );
        let (_, warp) = session.project().warp_of(id).unwrap();
        assert_eq!(warp.markers.len(), 3);
        assert_eq!(warp.mode, noodle_core::WarpMode::Slices);
        let settings = warp.quantise.as_ref().unwrap();
        assert_eq!((settings.grid, settings.amount), (1.0, 0.5));
        // Each marker plays halfway to its beat line, so none sits where it
        // was pinned (these hits are not on the grid) nor on a line.
        let played = warp.effective_markers();
        for (stored, moved) in warp.markers.iter().zip(played.iter()) {
            let line = stored.beat.round();
            if (stored.beat - line).abs() > 1e-6 {
                let want = stored.beat + (line - stored.beat) * 0.5;
                assert!((moved.beat - want).abs() < 1e-6, "{stored:?} {moved:?}");
            }
        }
        // Quantising again, with every hit already pinned, still applies the
        // new strength and join.
        let again = QuantiseHits {
            amount: 1.0,
            join: SliceJoin::FillGaps,
            ..quantise
        };
        session.quantise_transients(id, 50, again);
        let (_, warp) = session.project().warp_of(id).unwrap();
        assert_eq!(warp.markers.len(), 3);
        assert_eq!(warp.quantise.as_ref().unwrap().amount, 1.0);
        assert_eq!(warp.mode, noodle_core::WarpMode::Beats);
        // Each is one undo step.
        session.undo();
        session.undo();
        assert_eq!(*session.project(), before);
    }
}
