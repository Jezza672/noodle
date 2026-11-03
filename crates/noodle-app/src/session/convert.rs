//! Clip menu actions that make new material from an audio clip: Convert to
//! MIDI (a hummed line or a bass becomes notes) and Slice to Sampler (a loop
//! becomes pads). Both are one undo step.

use super::*;
use crate::timeline::add_track;
use noodle_core::group::{GROUP_OUTPUT, PORT_NAME, port_key};
use noodle_core::{Clip, ClipContent, Config, Endpoint, MidiClip, MidiNote, Node, Position, Value};
use noodle_io::{Transcription, decode_file, transcribe};
use noodle_nodes::{
    MONO_NOTE_ID, SAMPLE_INPUT_ID, SAMPLE_KEY, SAMPLER_ID, SLICE_ID, SLICE_SENSITIVITY_KEY,
};

/// The factory synth Convert to MIDI plays its notes with.
const SYNTH: &str = "system:template:mono-synth";

/// How readily Convert to MIDI hears a note, as the menu offers it.
#[derive(Clone, Copy, Debug, PartialEq)]
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
        let path = match self.directory() {
            Some(dir) => dir.join(&audio.source),
            None => PathBuf::from(&audio.source),
        };
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
}
