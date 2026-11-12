//! Create drum rack from clip: the analysis of a drum clip, and the command
//! that turns bins of its hits into a Drum Rack track (design:
//! docs/design/drum-rack-from-clip.md).
//!
//! * [`Session::analyse_drum_clip`] runs [`noodle_io::analyse_hits`] on a
//!   background [`Job`] with a progress bar. The result is held by the session
//!   (not the project) until the dialog is closed, and the analysis is kept by
//!   clip window in a [`HitCache`], so reopening is instant.
//! * [`Session::create_drum_rack`] builds the rack as one `Command::Batch`, so
//!   undo is one step. It is the existing Drum Rack template, so it is a group
//!   of primitives with its controls exposed, and no new node type: one Sample
//!   Set per used bin at the rack's top level (each hit one round-robin slot, a
//!   `hash:start:end` window of the clip's own file, so nothing is copied), wired
//!   to the `sample` input of every pad that plays it, and the pad's Sampler
//!   plays the slot in `rr_pick` (its `take` control).

use std::sync::Arc;

use noodle_core::group::{GROUP_OUTPUT, PORT_NAME, port_key};
use noodle_core::{ClipId, Command, Config, Endpoint, NodeId, Position, Project, Value};
use noodle_engine::{Job, JobPanicked};
use noodle_io::{DrumCancelled as Cancelled, Hit, HitCache, HitKey, analyse_hits};
use noodle_nodes::{NOTE_SPLIT_ID, SAMPLE_KEY, SAMPLE_SET_ID, SampleRef, SetEntry};

use super::*;
use crate::timeline::add_track;

/// The most keys of the rack: all of MIDI.
pub const MAX_PADS: usize = 128;

/// The most hits one bin's Sample Set holds: a round-robin slot is a `u8`.
const MAX_SLOTS: usize = 255;

/// The root every hit of a bin sits on. The pads' Samplers have keytrack off,
/// so it only has to be the same for all of them.
const ROOT: u8 = 60;

/// The General MIDI percussion name of a key, for the keys 35 to 81.
pub fn gm_drum_name(key: u8) -> Option<&'static str> {
    const NAMES: [&str; 47] = [
        "Acoustic Bass Drum",
        "Bass Drum",
        "Side Stick",
        "Acoustic Snare",
        "Hand Clap",
        "Electric Snare",
        "Low Floor Tom",
        "Closed Hi-Hat",
        "High Floor Tom",
        "Pedal Hi-Hat",
        "Low Tom",
        "Open Hi-Hat",
        "Low-Mid Tom",
        "Hi-Mid Tom",
        "Crash Cymbal 1",
        "High Tom",
        "Ride Cymbal 1",
        "Chinese Cymbal",
        "Ride Bell",
        "Tambourine",
        "Splash Cymbal",
        "Cowbell",
        "Crash Cymbal 2",
        "Vibraslap",
        "Ride Cymbal 2",
        "Hi Bongo",
        "Low Bongo",
        "Mute Hi Conga",
        "Open Hi Conga",
        "Low Conga",
        "High Timbale",
        "Low Timbale",
        "High Agogo",
        "Low Agogo",
        "Cabasa",
        "Maracas",
        "Short Whistle",
        "Long Whistle",
        "Short Guiro",
        "Long Guiro",
        "Claves",
        "Hi Wood Block",
        "Low Wood Block",
        "Mute Cuica",
        "Open Cuica",
        "Mute Triangle",
        "Open Triangle",
    ];
    key.checked_sub(35)
        .and_then(|i| NAMES.get(usize::from(i)))
        .copied()
}

/// What a pad is called for `key`: its General MIDI name, else the note's.
pub fn pad_name(key: u8) -> String {
    gm_drum_name(key).map_or_else(|| crate::piano_roll::key_name(key), str::to_owned)
}

/// The hits of one bin, to become one Sample Set.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedBin {
    /// The Sample Set's name (`Kick`, `Snare 2`).
    pub name: String,
    /// Indices into the analysis' hits, one round-robin slot each, in order.
    pub hits: Vec<usize>,
}

/// A bin dropped on a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PadAssignment {
    /// The MIDI key the pad answers to.
    pub key: u8,
    /// The index into [`DrumRackPlan::bins`].
    pub bin: usize,
    /// Which slot of the bin the pad plays, from 1 (clamped to the bin).
    pub pick: usize,
}

/// What the dialog made of the analysis: the bins that are used, and the pads
/// they are dropped on. A bin on several pads is one Sample Set wired to each.
#[derive(Clone, Debug, PartialEq)]
pub struct DrumRackPlan {
    pub bins: Vec<PlannedBin>,
    pub pads: Vec<PadAssignment>,
}

/// The hits found in a clip, held until the dialog closes.
#[derive(Clone, Debug)]
pub struct DrumAnalysis {
    pub clip: ClipId,
    /// The hash of the clip's source file.
    pub hash: String,
    pub hits: Arc<[Hit]>,
    /// Frames of the file before the first analysed frame (the clip's window
    /// starts there) and in the whole file: hits are counted from the former.
    origin: u64,
    file_frames: u64,
}

impl DrumAnalysis {
    /// An analysis of `hits` that was not made from a clip, for tests of what
    /// the dialog does with one.
    #[cfg(test)]
    pub(crate) fn synthetic(clip: ClipId, hits: Vec<Hit>) -> Self {
        Self {
            clip,
            hash: "synthetic".into(),
            hits: hits.into(),
            origin: 0,
            file_frames: 1_000_000,
        }
    }

    /// The window of the clip's file that hit `index` is cut from.
    pub fn window(&self, index: usize) -> Option<SampleRef> {
        let hit = self.hits.get(index)?;
        let total = self.file_frames.max(1) as f64;
        let at = |frame: u64| ((self.origin + frame) as f64 / total).clamp(0.0, 1.0) as f32;
        Some(SampleRef {
            hash: self.hash.clone(),
            start: at(hit.start),
            end: at(hit.end),
        })
    }
}

/// An analysis under way.
struct Run {
    job: Job<Result<Arc<[Hit]>, Cancelled>>,
    clip: ClipId,
    hash: String,
    origin: u64,
    file_frames: u64,
}

#[derive(Default)]
pub(super) struct DrumRack {
    run: Option<Run>,
    result: Option<DrumAnalysis>,
    cache: Arc<HitCache>,
    /// The hit being auditioned, playing straight to the device.
    preview: Option<noodle_io::Preview>,
}

impl Session {
    /// Starts finding the hits of audio clip `clip`.
    pub fn analyse_drum_clip(&mut self, clip: ClipId) {
        if self.drum_rack.run.is_some() {
            self.message = Some("A drum analysis is already running".into());
            return;
        }
        let dropped = match crate::sample_drop::from_clip(self, clip) {
            Ok(dropped) => dropped,
            Err(why) => {
                self.message = Some(why);
                return;
            }
        };
        let Some(audio) = self.project.clip(clip).and_then(|c| c.as_audio()) else {
            return;
        };
        let (origin, length) = (audio.offset, audio.length);
        let hash = dropped.reference.hash;
        let Some(sample) = self.samples().get(&hash) else {
            self.message = Some("The clip's file can't be read".into());
            return;
        };
        let file_frames = sample.frames() as u64;
        let end = (origin + length).min(file_frames);
        if end <= origin {
            self.message = Some("The clip is empty".into());
            return;
        }
        self.drum_rack.result = None;
        let key = HitKey::new(hash.clone(), origin, end);
        let cache = self.drum_rack.cache.clone();
        let job = Job::spawn(move |progress| {
            cache.get_or_analyse(key, || {
                let (a, b) = (origin as usize, end as usize);
                let mono: Vec<f32> = sample.left[a..b]
                    .iter()
                    .zip(&sample.right[a..b])
                    .map(|(l, r)| 0.5 * (l + r))
                    .collect();
                analyse_hits(&mono, sample.sample_rate, &mut |f| {
                    progress.report(f).is_ok()
                })
            })
        });
        self.drum_rack.run = Some(Run {
            job,
            clip,
            hash,
            origin,
            file_frames,
        });
    }

    /// How far the analysis under way has got, from 0 to 1.
    pub fn drum_progress(&self) -> Option<f32> {
        self.drum_rack.run.as_ref().map(|r| r.job.fraction())
    }

    /// Stops the analysis under way. Nothing is left of it.
    pub fn cancel_drum_analysis(&mut self) {
        if let Some(run) = &self.drum_rack.run {
            run.job.cancel();
        }
    }

    /// The finished analysis, until [`close_drum_analysis`](Self::close_drum_analysis).
    pub fn drum_analysis(&self) -> Option<&DrumAnalysis> {
        self.drum_rack.result.as_ref()
    }

    /// Lets go of the analysis (the dialog was closed).
    pub fn close_drum_analysis(&mut self) {
        self.drum_rack.preview = None;
        self.drum_rack.result = None;
    }

    /// Plays hit `index` of the held analysis once, straight to the monitor's
    /// output device (the main output when none is chosen). It bypasses the
    /// graph and the project entirely: it is only for trying a hit, and a new
    /// audition replaces the one playing. Silent on the virtual output.
    pub fn audition_drum_hit(&mut self, index: usize) {
        let Some(window) = self.drum_rack.result.as_ref().and_then(|a| a.window(index)) else {
            return;
        };
        let Some(sample) = self.samples().get(&window.hash) else {
            return;
        };
        let device = match self.monitor_device() {
            "" => self.audio_config.output.clone(),
            chosen => Some(chosen.to_owned()),
        };
        if device.as_deref() == Some(noodle_io::VIRTUAL_OUTPUT) {
            return;
        }
        let frames = sample.frames() as f32;
        let (a, b) = (
            (window.start * frames) as usize,
            ((window.end * frames) as usize).min(sample.frames()),
        );
        if a >= b {
            return;
        }
        // Stop the one playing first, so two never overlap.
        self.drum_rack.preview = None;
        match noodle_io::Preview::play(
            self.audio_config.host.as_deref(),
            device.as_deref(),
            &sample.left[a..b],
            &sample.right[a..b],
            sample.sample_rate,
        ) {
            Ok(preview) => self.drum_rack.preview = Some(preview),
            Err(error) => self.message = Some(format!("Can't play that hit: {error}")),
        }
    }

    /// Takes in a finished analysis.
    pub(super) fn poll_drum_analysis(&mut self) {
        let Some(run) = &mut self.drum_rack.run else {
            return;
        };
        let Some(done) = run.job.poll() else {
            return;
        };
        let run = self.drum_rack.run.take().expect("just polled");
        match done {
            Ok(Ok(hits)) if hits.is_empty() => {
                self.message = Some("No drum hits found in that clip".into());
            }
            Ok(Ok(hits)) => {
                self.drum_rack.result = Some(DrumAnalysis {
                    clip: run.clip,
                    hash: run.hash,
                    hits,
                    origin: run.origin,
                    file_frames: run.file_frames,
                });
            }
            Ok(Err(Cancelled)) => self.message = Some("Drum analysis cancelled".into()),
            Err(JobPanicked) => {
                self.message = Some(format!("Couldn't analyse the clip: {JobPanicked}"));
            }
        }
    }

    /// A new track holding a Drum Rack that plays `plan`'s bins, from the
    /// held analysis of `clip`, as one undo step.
    pub fn create_drum_rack(&mut self, clip: ClipId, plan: &DrumRackPlan) {
        match self.drum_rack_command(clip, plan) {
            Ok((command, pads)) => {
                self.edit([Edit::Apply(command)]);
                self.message = Some(format!("Made a Drum Rack of {pads} pads on a new track"));
            }
            Err(why) => self.message = Some(why),
        }
    }

    fn drum_rack_command(
        &self,
        clip: ClipId,
        plan: &DrumRackPlan,
    ) -> Result<(Command, usize), String> {
        let analysis = self
            .drum_analysis()
            .filter(|a| a.clip == clip)
            .ok_or("Analyse the clip first")?;
        // Last assignment of a key wins; a bin that is empty or out of range
        // plays nothing.
        let mut keys: Vec<&PadAssignment> = Vec::new();
        for pad in &plan.pads {
            if !plan.bins.get(pad.bin).is_some_and(|b| !b.hits.is_empty()) {
                continue;
            }
            keys.retain(|p| p.key != pad.key);
            keys.push(pad);
        }
        if keys.is_empty() {
            return Err("Drop a bin on a pad first".into());
        }
        keys.sort_by_key(|p| p.key);
        let keys: Vec<_> = keys.into_iter().take(MAX_PADS).collect();
        let entries_of = |bin: &PlannedBin| -> Result<Vec<SetEntry>, String> {
            bin.hits
                .iter()
                .take(MAX_SLOTS)
                .enumerate()
                .map(|(slot, &hit)| {
                    Ok(SetEntry {
                        reference: analysis.window(hit).ok_or("A hit of the bin is gone")?,
                        root: Some(ROOT),
                        layer: 1,
                        rr: slot as u8 + 1,
                    })
                })
                .collect()
        };

        let source = self.project.clip(clip).ok_or("That clip is gone")?;
        let audio = source.as_audio().ok_or("Only audio clips can be used")?;
        let name = std::path::Path::new(&audio.source)
            .file_stem()
            .map_or_else(|| "clip".to_owned(), |s| s.to_string_lossy().into_owned());

        let mut scratch = self.project.clone();
        let (track, input) = add_track::with_input(scratch.graph(), || self.new_node_id());
        track
            .clone()
            .apply(&mut scratch)
            .map_err(|e| e.to_string())?;
        let track_group = scratch
            .graph()
            .node(input)
            .and_then(|n| n.parent)
            .ok_or("the new track has no group")?;
        let track_output = scratch
            .graph()
            .children(Some(track_group))
            .find(|(_, n)| n.type_id == GROUP_OUTPUT)
            .map(|(id, _)| id)
            .ok_or("the new track has no output")?;
        let mut commands = vec![
            track,
            Command::SetConfig {
                node: track_group,
                key: PORT_NAME.into(),
                value: Some(Value::Text(format!("{name} (drum rack)"))),
            },
        ];

        // The Drum Rack template, inside the track.
        let (template_id, template) = self
            .library()
            .templates()
            .find(|(_, t)| t.name == "Drum Rack")
            .map(|(id, t)| (id.to_string(), t.clone()))
            .ok_or("The library has no Drum Rack")?;
        let (rack, add) = template.instantiate(
            &template_id,
            Some(track_group),
            Position { x: 300.0, y: 0.0 },
            || self.new_node_id(),
        );
        add.clone().apply(&mut scratch).map_err(|e| e.to_string())?;
        commands.push(add);
        // The mixer grows before any copy is wired into it.
        let after_rack = commands.len();
        let graph = scratch.graph();

        // The template's pads, with the key each answers to.
        let mut spare: Vec<Pad> = graph
            .children(Some(rack))
            .filter_map(|(id, _)| Pad::of(&scratch, id))
            .collect();
        let mut chosen: Vec<(u8, Pad)> = Vec::new();
        for pad in &keys {
            if let Some(i) = spare.iter().position(|p| p.default_key == pad.key) {
                chosen.push((pad.key, spare.remove(i)));
            }
        }
        // A key no pad answers to takes a pad nothing else wants, else a copy.
        let mix = graph
            .children(Some(rack))
            .find(|(_, n)| n.type_id == "noodle.util.mix")
            .map(|(id, _)| id)
            .ok_or("the rack has no mixer")?;
        let mut mix_inputs = graph
            .node(mix)
            .and_then(|n| n.config.get("inputs"))
            .and_then(|v| match v {
                Value::Int(i) => Some(*i as usize),
                _ => None,
            })
            .unwrap_or(8);
        let template_pad = spare
            .first()
            .or_else(|| chosen.first().map(|(_, p)| p))
            .cloned();
        let mut extra = 0;
        for pad in &keys {
            if chosen.iter().any(|(k, _)| *k == pad.key) {
                continue;
            }
            let pad_of = if spare.is_empty() {
                let original = template_pad.clone().ok_or("the rack has no pads")?;
                extra += 1;
                mix_inputs += 1;
                original.copy(
                    &scratch,
                    mix,
                    mix_inputs,
                    extra,
                    || self.new_node_id(),
                    &mut commands,
                )?
            } else {
                spare.remove(0)
            };
            chosen.push((pad.key, pad_of));
        }
        if mix_inputs > 8 {
            commands.insert(
                after_rack,
                Command::SetConfig {
                    node: mix,
                    key: "inputs".into(),
                    value: Some(Value::Int(mix_inputs as i64)),
                },
            );
        }
        // Pads nobody asked for are not added.
        for pad in &spare {
            commands.push(Command::RemoveNode { id: pad.group });
        }
        // Each pad answers to its key and is named for it.
        for (key, pad) in &chosen {
            if pad.default_key != *key {
                commands.push(Command::SetConfig {
                    node: pad.group,
                    key: PORT_NAME.into(),
                    value: Some(Value::Text(pad_name(*key))),
                });
            }
            for param in ["low1", "high1"] {
                commands.push(Command::SetParam {
                    node: pad.split,
                    key: param.into(),
                    value: Some(f32::from(*key)),
                });
            }
        }
        // One Sample Set per used bin, wired to each pad that plays it.
        let mut used: Vec<usize> = keys.iter().map(|p| p.bin).collect();
        used.sort_unstable();
        used.dedup();
        let mut sets = std::collections::BTreeMap::new();
        for (row, &bin) in used.iter().enumerate() {
            let planned = &plan.bins[bin];
            let entries = entries_of(planned)?;
            let id = self.new_node_id();
            let mut node = noodle_core::Node::new(SAMPLE_SET_ID).with_config(
                Config::new()
                    .with(SAMPLE_KEY, Value::Text(noodle_nodes::set_to_text(&entries)))
                    .with(PORT_NAME, Value::Text(planned.name.clone())),
            );
            node.position = Position {
                x: 300.0 - 250.0,
                y: 1100.0 + 200.0 * row as f32,
            };
            node.parent = Some(rack);
            commands.push(Command::AddNode { id, node });
            sets.insert(bin, (id, entries.len()));
        }
        for assignment in &keys {
            let (set, slots) = sets[&assignment.bin];
            let (_, pad) = chosen
                .iter()
                .find(|(k, _)| *k == assignment.key)
                .expect("every key has a pad");
            commands.push(noodle_core::spare::wire(
                Endpoint::new(set, "sample"),
                Endpoint::new(pad.group, "sample"),
            ));
            commands.push(Command::SetParam {
                node: pad.sampler,
                key: "rr_pick".into(),
                value: Some(assignment.pick.clamp(1, slots) as f32),
            });
        }
        // And around it: the track's own MIDI in, the sound out.
        commands.push(noodle_core::spare::wire(
            Endpoint::new(input, "midi"),
            Endpoint::new(rack, "notes"),
        ));
        commands.push(noodle_core::spare::wire(
            Endpoint::new(rack, "out"),
            Endpoint::new(track_output, port_key(1)),
        ));
        let pads = chosen.len();
        Ok((Command::Batch(commands), pads))
    }
}

/// A pad of the template, and what in it is changed.
#[derive(Clone)]
struct Pad {
    group: NodeId,
    /// The Note Split that filters the pad's key.
    split: NodeId,
    sampler: NodeId,
    /// The key the template has it answer to.
    default_key: u8,
}

impl Pad {
    fn of(project: &Project, group: NodeId) -> Option<Self> {
        let graph = project.graph();
        let inside = |ty: &str| {
            graph
                .children(Some(group))
                .find(|(_, n)| n.type_id == ty)
                .map(|(id, _)| id)
        };
        let split = inside(NOTE_SPLIT_ID)?;
        let sampler = inside(noodle_nodes::SAMPLER_ID)?;
        let key = graph.node(split)?.params.get("low1").copied()?;
        Some(Self {
            group,
            split,
            sampler,
            default_key: key.round() as u8,
        })
    }

    /// Adds a copy of the pad (and what is in it) to the rack, wired like the
    /// original from the rack's swing, and into input `mix_input` of `mix`.
    fn copy(
        &self,
        project: &Project,
        mix: NodeId,
        mix_input: usize,
        nth: usize,
        mut new_id: impl FnMut() -> NodeId,
        commands: &mut Vec<Command>,
    ) -> Result<Self, String> {
        let graph = project.graph();
        let mut old = vec![self.group];
        old.extend(graph.descendants(self.group));
        let ids: std::collections::BTreeMap<NodeId, NodeId> =
            old.iter().map(|&o| (o, new_id())).collect();
        for &o in &old {
            let mut node = graph.node(o).ok_or("the pad is gone")?.clone();
            if o == self.group {
                node.position.y += 130.0 * (8 + nth) as f32;
            } else {
                node.parent = node.parent.map(|p| ids[&p]);
            }
            commands.push(Command::AddNode { id: ids[&o], node });
        }
        for c in graph.connections() {
            if let (Some(&from), Some(&to)) = (ids.get(&c.from.node), ids.get(&c.to.node)) {
                commands.push(noodle_core::spare::wire(
                    Endpoint::new(from, c.from.port),
                    Endpoint::new(to, c.to.port),
                ));
            }
        }
        let group = ids[&self.group];
        if let Some(swing) = graph.source(&Endpoint::new(self.group, "notes")) {
            commands.push(noodle_core::spare::wire(
                swing.clone(),
                Endpoint::new(group, "notes"),
            ));
        }
        commands.push(noodle_core::spare::wire(
            Endpoint::new(group, "out"),
            Endpoint::new(mix, noodle_core::spare::mixer_input_key(mix_input)),
        ));
        Ok(Self {
            group,
            split: ids[&self.split],
            sampler: ids[&self.sampler],
            default_key: self.default_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::{Clip, Tick};
    use noodle_nodes::SAMPLER_ID;
    use std::f32::consts::TAU;
    use std::time::{Duration, Instant};

    const RATE: u32 = 44_100;

    fn kick() -> Vec<f32> {
        let mut phase = 0.0f32;
        (0..RATE as usize / 4)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                phase += TAU * (45.0 + 110.0 * (-t * 40.0).exp()) / RATE as f32;
                0.8 * phase.sin() * (-t * 14.0).exp()
            })
            .collect()
    }

    fn hat() -> Vec<f32> {
        let mut state = 99u32;
        let mut low = 0.0f32;
        (0..RATE as usize / 10)
            .map(|i| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let n = (state >> 8) as f32 / (1u32 << 23) as f32 - 1.0;
                low += 0.6 * (n - low);
                let t = i as f32 / RATE as f32;
                0.5 * (n - low) * (-t * 60.0).exp()
            })
            .collect()
    }

    /// A saved project with a clip of kicks and hats, four seconds long.
    fn rig(dir: &std::path::Path) -> (Session, ClipId) {
        let mut session = Session::new(crate::session::Nodes::all());
        let (track, input) =
            add_track::with_input(session.project().graph(), || session.new_node_id());
        session.edit([Edit::Apply(track)]);
        let mut audio = vec![0.0f32; 4 * RATE as usize];
        for beat in 0..8 {
            let at = (0.2 + beat as f32 * 0.5) * RATE as f32;
            let (hit, gain) = if beat % 2 == 0 {
                (kick(), 1.0)
            } else {
                (hat(), 1.0)
            };
            for (i, v) in hit.iter().enumerate() {
                audio[at as usize + i] += gain * v;
            }
        }
        noodle_io::write_wav(&dir.join("drums.wav"), &audio, 1, RATE).unwrap();
        let id = session.project().next_clip_id();
        let clip = Clip::audio(input, Tick(0), "drums.wav", audio.len() as u64);
        session.edit([Edit::Apply(Command::AddClip { id, clip })]);
        assert!(session.save_as(&dir.join("project.ron")));
        (session, id)
    }

    fn analysed(session: &mut Session, clip: ClipId) {
        session.edit([Edit::AnalyseDrumClip(clip)]);
        let deadline = Instant::now() + Duration::from_secs(60);
        while session.drum_progress().is_some() {
            assert!(Instant::now() < deadline, "the analysis never finished");
            std::thread::sleep(Duration::from_millis(5));
            session.maintain();
        }
    }

    /// The plan the default binning suggests: a bin each, on kick and hat keys.
    fn plan_of(session: &Session) -> DrumRackPlan {
        let analysis = session.drum_analysis().expect("an analysis");
        let binning = noodle_io::bin_hits(&analysis.hits, noodle_io::DEFAULT_MERGE);
        let bins: Vec<PlannedBin> = binning
            .bins
            .iter()
            .enumerate()
            .map(|(i, b)| PlannedBin {
                name: format!("Bin {}", i + 1),
                hits: b.members.clone(),
            })
            .collect();
        assert_eq!(bins.len(), 2, "a kick bin and a hat bin");
        DrumRackPlan {
            pads: vec![
                PadAssignment {
                    key: 36,
                    bin: 0,
                    pick: 2,
                },
                PadAssignment {
                    key: 42,
                    bin: 1,
                    pick: 1,
                },
            ],
            bins,
        }
    }

    fn of_type<'a>(session: &'a Session, ty: &str) -> Vec<(NodeId, &'a noodle_core::Node)> {
        session
            .project()
            .graph()
            .nodes()
            .filter(|(_, n)| n.type_id == ty)
            .collect()
    }

    #[test]
    fn analysing_a_clip_finds_its_hits_and_holds_them_until_closed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig(dir.path());
        assert!(session.drum_analysis().is_none());
        analysed(&mut session, clip);
        let analysis = session.drum_analysis().expect("an analysis");
        assert_eq!(analysis.clip, clip);
        assert_eq!(analysis.hits.len(), 8);
        // A hit's window is a part of the clip's own file.
        let window = analysis.window(1).unwrap();
        assert!(window.start < window.end && window.end <= 1.0);
        assert_eq!(analysis.window(99), None);
        session.close_drum_analysis();
        assert!(session.drum_analysis().is_none());
    }

    #[test]
    fn a_drum_rack_is_one_undo_step_of_shared_sample_sets_wired_to_pads() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig(dir.path());
        analysed(&mut session, clip);
        let plan = plan_of(&session);
        let tracks = crate::timeline::tracks(session.project()).len();
        session.edit([Edit::CreateDrumRack(clip, plan.clone())]);
        assert_eq!(
            session.message(),
            Some("Made a Drum Rack of 2 pads on a new track")
        );
        assert_eq!(crate::timeline::tracks(session.project()).len(), tracks + 1);
        let graph = session.project().graph();
        // One shared Sample Set per bin, one slot per hit, nothing rendered.
        let sets = of_type(&session, SAMPLE_SET_ID);
        assert_eq!(sets.len(), 2);
        let entries = |node: &noodle_core::Node| match node.config.get(SAMPLE_KEY) {
            Some(Value::Text(t)) => noodle_nodes::parse_set(t),
            _ => panic!("no entries"),
        };
        let slots: Vec<usize> = sets.iter().map(|(_, n)| entries(n).len()).collect();
        assert_eq!(slots, [4, 4]);
        for (_, set) in &sets {
            let e = entries(set);
            let hash = &session.drum_analysis().unwrap().hash;
            assert!(e.iter().all(|e| &e.reference.hash == hash));
            let rrs: Vec<u8> = e.iter().map(|e| e.rr).collect();
            assert_eq!(rrs, [1, 2, 3, 4]);
        }
        // Only the pads that were asked for are left, each on its key, named
        // for it, with a set wired to its sample input and its pick set.
        let rack = of_type(&session, SAMPLER_ID)
            .first()
            .and_then(|(id, _)| graph.node(*id)?.parent)
            .and_then(|pad| graph.node(pad)?.parent)
            .expect("the samplers are in pads in the rack");
        let pads: Vec<NodeId> = graph
            .children(Some(rack))
            .filter(|(id, _)| Pad::of(session.project(), *id).is_some())
            .map(|(id, _)| id)
            .collect();
        assert_eq!(pads.len(), 2);
        for (assignment, set_slots) in plan.pads.iter().zip([4, 4]) {
            let pad = pads
                .iter()
                .map(|&g| Pad::of(session.project(), g).unwrap())
                .find(|p| {
                    graph.node(p.split).unwrap().params.get("low1").copied()
                        == Some(f32::from(assignment.key))
                })
                .expect("a pad on the key");
            let from = graph
                .source(&Endpoint::new(pad.group, "sample"))
                .expect("the pad's sample is wired");
            assert_eq!(graph.node(from.node).unwrap().type_id, SAMPLE_SET_ID);
            let pick = graph.node(pad.sampler).unwrap().params["rr_pick"];
            assert_eq!(pick as usize, assignment.pick.clamp(1, set_slots));
        }
        let names: Vec<_> = pads
            .iter()
            .map(|&p| crate::editor::group_title(graph, p))
            .collect();
        assert_eq!(names, ["Kick", "Closed Hat"]);
        // It compiles clean.
        let (_, diagnostics) = noodle_engine::compile(graph, session.registry());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        // And undoing it leaves nothing.
        session.undo();
        assert_eq!(crate::timeline::tracks(session.project()).len(), tracks);
        assert!(of_type(&session, SAMPLE_SET_ID).is_empty());
        assert!(of_type(&session, SAMPLER_ID).is_empty());
    }

    #[test]
    fn a_bin_on_two_pads_is_one_set_wired_to_both_and_odd_keys_get_pads_named_for_them() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig(dir.path());
        analysed(&mut session, clip);
        let mut plan = plan_of(&session);
        // The kick bin also on a snare key, and on keys no template pad has:
        // eleven pads in all, past the template's eight.
        plan.pads = (0..11)
            .map(|i| PadAssignment {
                key: [36, 38, 35, 37, 40, 41, 43, 44, 47, 48, 52][i],
                bin: 0,
                pick: i + 1,
            })
            .collect();
        session.edit([Edit::CreateDrumRack(clip, plan)]);
        assert_eq!(
            session.message(),
            Some("Made a Drum Rack of 11 pads on a new track")
        );
        assert_eq!(of_type(&session, SAMPLE_SET_ID).len(), 1, "one shared set");
        let graph = session.project().graph();
        let samplers = of_type(&session, SAMPLER_ID);
        assert_eq!(samplers.len(), 11);
        for (id, _) in &samplers {
            let from = graph.source(&Endpoint::new(
                graph.node(*id).unwrap().parent.unwrap(),
                "sample",
            ));
            assert!(from.is_some(), "every pad reads the set");
        }
        // The extras are named for their General MIDI drum.
        let titles: Vec<String> = samplers
            .iter()
            .map(|(id, n)| crate::editor::group_title(graph, n.parent.unwrap_or(*id)))
            .collect();
        for name in [
            "Acoustic Bass Drum",
            "Side Stick",
            "Electric Snare",
            "Low Floor Tom",
        ] {
            assert!(titles.iter().any(|t| t == name), "{name} in {titles:?}");
        }
        let (_, diagnostics) = noodle_engine::compile(graph, session.registry());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn nothing_is_made_without_an_analysis_or_a_pad() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig(dir.path());
        let plan = DrumRackPlan {
            bins: vec![PlannedBin {
                name: "Kick".into(),
                hits: vec![0],
            }],
            pads: vec![PadAssignment {
                key: 36,
                bin: 0,
                pick: 1,
            }],
        };
        session.edit([Edit::CreateDrumRack(clip, plan.clone())]);
        assert_eq!(session.message(), Some("Analyse the clip first"));
        analysed(&mut session, clip);
        session.edit([Edit::CreateDrumRack(
            clip,
            DrumRackPlan {
                pads: vec![],
                ..plan
            },
        )]);
        assert_eq!(session.message(), Some("Drop a bin on a pad first"));
        assert!(of_type(&session, SAMPLE_SET_ID).is_empty());
    }

    #[test]
    fn general_midi_names_cover_35_to_81() {
        assert_eq!(gm_drum_name(36), Some("Bass Drum"));
        assert_eq!(gm_drum_name(81), Some("Open Triangle"));
        assert_eq!(gm_drum_name(34), None);
        assert_eq!(gm_drum_name(82), None);
        assert_eq!(pad_name(34), "A#1");
    }
}
