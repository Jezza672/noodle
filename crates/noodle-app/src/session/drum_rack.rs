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
use noodle_core::{
    Clip, ClipContent, ClipId, Command, Config, Endpoint, MidiClip, MidiNote, NodeId, Position,
    Project, TICKS_PER_QUARTER, Tick, Value,
};
use noodle_engine::{Job, JobPanicked};
use noodle_io::{DrumCancelled as Cancelled, Hit, HitCache, HitKey, analyse_hits};
use noodle_nodes::{NOTE_SPLIT_ID, SAMPLE_KEY, SAMPLE_SET_ID, SampleRef, SetEntry};
use noodle_stems::drum::{
    DrumModelManifest, DrumPart, HitWindow, PartSeparator, Separation, manifest_by_id,
    separate_hits,
};
use noodle_stems::{ModelId, SeparateError, Stereo};

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
    /// Indices into the analysis' hits, in order: within a velocity layer each
    /// is one round-robin slot.
    pub hits: Vec<usize>,
    /// The velocity layer of each hit (from 1, the softest), parallel to
    /// `hits`; empty when the bin is not layered. Each layer's round-robin slots
    /// count from 1 in the order of `hits`.
    pub layers: Vec<u8>,
}

impl PlannedBin {
    /// The layer of the hit at position `at` of `hits`.
    fn layer_of(&self, at: usize) -> u8 {
        self.layers.get(at).copied().unwrap_or(1).max(1)
    }

    /// How many layers there are.
    fn layer_count(&self) -> usize {
        (0..self.hits.len())
            .map(|i| usize::from(self.layer_of(i)))
            .max()
            .unwrap_or(1)
    }

    /// The most slots any layer has: what a pad's pick can reach.
    fn slots(&self) -> usize {
        let mut counts = std::collections::BTreeMap::new();
        for i in 0..self.hits.len().min(MAX_SLOTS * 3) {
            *counts.entry(self.layer_of(i)).or_insert(0usize) += 1;
        }
        counts.values().copied().max().unwrap_or(1)
    }
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
    /// Also add a MIDI clip beside the rack that plays the clip's groove (see
    /// [`groove_notes`]).
    pub groove: bool,
}

/// The velocity a hit plays at: its level against the loudest of the groove,
/// on a gentle scale (each 12 dB down halves it), from 0.05 to 1.
fn level_velocity(level_db: f32, loudest_db: f32) -> f32 {
    10f32.powf((level_db - loudest_db) / 40.0).clamp(0.05, 1.0)
}

/// `velocity` moved into the band of velocities that play layer `index` (from
/// 0, softest) of `layers`, which the Sampler splits evenly (see
/// `SampleTables::layer`), so a note plays the layer its hit was put in.
fn velocity_in_layer(velocity: f32, index: usize, layers: usize) -> f32 {
    if layers <= 1 {
        return velocity;
    }
    let band: Vec<u32> = (1..=127u32)
        .filter(|midi| noodle_nodes::velocity_layer(*midi as f32 / 127.0, layers) == index)
        .collect();
    let (Some(&lo), Some(&hi)) = (band.first(), band.last()) else {
        return velocity;
    };
    (velocity * 127.0).clamp(lo as f32, hi as f32) / 127.0
}

/// The hits found in a clip, held until the dialog closes.
#[derive(Clone, Debug)]
pub struct DrumAnalysis {
    pub clip: ClipId,
    /// The hash of the clip's source file.
    pub hash: String,
    pub hits: Arc<[Hit]>,
    /// What the separation analyser found in each hit, once it has run (see
    /// [`Session::separate_drum_hits`]).
    pub separation: Option<Arc<Separation>>,
    /// Hits whose audio the user replaced with a separated part, by hit index.
    taken: std::collections::BTreeMap<usize, SampleRef>,
    /// Frames of the file before the first analysed frame (the clip's window
    /// starts there) and in the whole file: hits are counted from the former.
    origin: u64,
    file_frames: u64,
}

impl DrumAnalysis {
    /// Whether the separation found another drum sounding in hit `index`.
    pub fn separated_mixed(&self, index: usize) -> bool {
        self.separation
            .as_ref()
            .and_then(|s| s.hits.get(index))
            .is_some_and(|h| h.is_mixed())
    }

    /// Whether hit `index` plays a separated part instead of its own audio.
    pub fn is_taken(&self, index: usize) -> bool {
        self.taken.contains_key(&index)
    }

    /// An analysis of `hits` that was not made from a clip, for tests of what
    /// the dialog does with one.
    #[cfg(test)]
    pub(crate) fn synthetic(clip: ClipId, hits: Vec<Hit>) -> Self {
        Self {
            clip,
            hash: "synthetic".into(),
            hits: hits.into(),
            separation: None,
            taken: Default::default(),
            origin: 0,
            file_frames: 1_000_000,
        }
    }

    /// The window of the clip's file that hit `index` is cut from.
    pub fn window(&self, index: usize) -> Option<SampleRef> {
        let hit = self.hits.get(index)?;
        if let Some(taken) = self.taken.get(&index) {
            return Some(taken.clone());
        }
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

/// What the separation of one clip window by one model is of.
type SeparationKey = (HitKey, ModelId);

/// Opens a separator for a model, given ONNX Runtime's library if there is one.
pub(crate) type PartOpener = Arc<
    dyn Fn(
            &'static DrumModelManifest,
            Option<&std::path::Path>,
        ) -> Result<Box<dyn PartSeparator>, String>
        + Send
        + Sync,
>;

/// A separation under way.
struct SeparationRun {
    job: Job<Result<Arc<Separation>, SeparateError>>,
    /// The analysis it is for.
    clip: ClipId,
    model: &'static str,
}

#[derive(Default)]
pub(super) struct DrumRack {
    separating: Option<SeparationRun>,
    separations: Arc<std::sync::Mutex<std::collections::HashMap<SeparationKey, Arc<Separation>>>>,
    /// Instead of the real model, in tests.
    opener: Option<PartOpener>,
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

    /// Runs the separation model `model` (a [`DrumModelManifest`] id) over the
    /// clip of the held analysis, as an optional extra analyser: each hit then
    /// knows which drum parts sound in it ([`DrumAnalysis::separation`]). The
    /// result is kept by clip window, file and model id and version, so asking
    /// again is instant. Cancellable, with progress, like the analysis.
    pub fn separate_drum_hits(&mut self, model: &str) {
        if self.drum_rack.separating.is_some() {
            self.message = Some("A separation is already running".into());
            return;
        }
        let Some(manifest) = manifest_by_id(model) else {
            self.message = Some(format!("Unknown separation model {model}"));
            return;
        };
        let Some(analysis) = self.drum_rack.result.as_ref() else {
            return;
        };
        let (clip, hash, origin) = (analysis.clip, analysis.hash.clone(), analysis.origin);
        let windows: Vec<HitWindow> = analysis
            .hits
            .iter()
            .map(|h| HitWindow {
                onset: h.onset,
                start: h.start,
                end: h.end,
            })
            .collect();
        let Some(sample) = self.samples().get(&hash) else {
            self.message = Some("The clip's file can't be read".into());
            return;
        };
        let Some(length) = self
            .project
            .clip(clip)
            .and_then(|c| c.as_audio())
            .map(|a| a.length)
        else {
            return;
        };
        let end = (origin + length).min(sample.frames() as u64);
        let key: SeparationKey = (
            HitKey::new(hash, origin, end),
            ModelId {
                id: manifest.id.into(),
                version: manifest.version.into(),
            },
        );
        let cache = self.drum_rack.separations.clone();
        let (opener, runtime) = match &self.drum_rack.opener {
            Some(opener) => (opener.clone(), None),
            None => {
                let Some(store) = crate::models::default_drum_store() else {
                    self.message =
                        Some("There is nowhere to keep the models on this computer".into());
                    return;
                };
                let opener: PartOpener = Arc::new(move |m, runtime| {
                    noodle_stems::drum::open_separator(m, &store, runtime)
                        .map_err(|e| e.to_string())
                });
                (opener, self.stems.onnx_runtime())
            }
        };
        let job = Job::spawn(move |progress| {
            if let Some(found) = cache.lock().unwrap().get(&key).cloned() {
                return Ok(found);
            }
            // ONNX Runtime is fetched here when the machine has none, as Split
            // to stems does.
            let mut share = 0.0;
            let mut runtime_path = None;
            if let Some((store, spec, needs)) = runtime {
                if needs {
                    share = 0.03;
                    store
                        .download_runtime(&spec, &mut |f| progress.report(f * share).is_ok())
                        .map_err(|e| match e {
                            noodle_stems::DownloadError::Cancelled => SeparateError::Cancelled,
                            other => SeparateError::Failed(other.to_string()),
                        })?;
                }
                runtime_path = super::stems::runtime_in(&store, Some(&spec));
            }
            let mut separator =
                opener(manifest, runtime_path.as_deref()).map_err(SeparateError::Failed)?;
            let (a, b) = (origin as usize, end as usize);
            let audio = Stereo {
                left: sample.left[a..b].to_vec(),
                right: sample.right[a..b].to_vec(),
            };
            let separation = Arc::new(separate_hits(
                &mut *separator,
                &audio,
                sample.sample_rate,
                &windows,
                &mut |f| progress.report(share + (1.0 - share) * f).is_ok(),
            )?);
            cache.lock().unwrap().insert(key, separation.clone());
            Ok(separation)
        });
        self.drum_rack.separating = Some(SeparationRun {
            job,
            clip,
            model: manifest.id,
        });
    }

    /// How far the separation under way has got, from 0 to 1.
    pub fn drum_separation_progress(&self) -> Option<f32> {
        self.drum_rack.separating.as_ref().map(|r| r.job.fraction())
    }

    /// Stops the separation under way. The analysis stays as it was.
    pub fn cancel_drum_separation(&mut self) {
        if let Some(run) = &self.drum_rack.separating {
            run.job.cancel();
        }
    }

    /// Makes hit `index` of the held analysis play `part` of its separation
    /// instead of its own audio, for a hit that is mixed with no clean
    /// example. The part's audio goes into the project's samples.
    pub fn take_separated_part(&mut self, index: usize, part: DrumPart) {
        let Some(analysis) = self.drum_rack.result.as_ref() else {
            return;
        };
        let Some(audio) = analysis
            .separation
            .as_ref()
            .and_then(|s| s.hits.get(index))
            .and_then(|h| h.part_audio(part))
        else {
            self.message = Some(format!("That hit has no {part} part"));
            return;
        };
        let rate = match self.samples().get(&analysis.hash) {
            Some(sample) => sample.sample_rate,
            None => return,
        };
        let hash = self.samples().insert(&noodle_io::Audio {
            samples: audio.to_interleaved(),
            channels: 2,
            sample_rate: rate,
        });
        if let Some(analysis) = self.drum_rack.result.as_mut() {
            analysis.taken.insert(index, SampleRef::whole(&hash));
        }
    }

    /// Makes hit `index` play its own audio again.
    pub fn release_separated_part(&mut self, index: usize) {
        if let Some(analysis) = self.drum_rack.result.as_mut() {
            analysis.taken.remove(&index);
        }
    }

    /// Takes in a finished separation.
    pub(super) fn poll_drum_separation(&mut self) {
        let Some(run) = &mut self.drum_rack.separating else {
            return;
        };
        let Some(done) = run.job.poll() else {
            return;
        };
        let run = self.drum_rack.separating.take().expect("just polled");
        match done {
            Ok(Ok(separation)) => {
                if let Some(analysis) = self
                    .drum_rack
                    .result
                    .as_mut()
                    .filter(|a| a.clip == run.clip)
                {
                    analysis.separation = Some(separation);
                }
            }
            Ok(Err(SeparateError::Cancelled)) => {
                self.message = Some("Separation cancelled".into());
            }
            Ok(Err(error)) => {
                self.message = Some(format!(
                    "Couldn't separate the drums with {}: {error}",
                    run.model
                ));
            }
            Err(JobPanicked) => {
                self.message = Some(format!("Couldn't separate the drums: {JobPanicked}"));
            }
        }
    }

    /// Makes `open` the way a separator is got, instead of the model, so a test
    /// needs no weights.
    #[cfg(test)]
    pub(crate) fn use_part_opener(&mut self, open: PartOpener) {
        self.drum_rack.opener = Some(open);
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
        self.cancel_drum_separation();
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
                    separation: None,
                    taken: Default::default(),
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
            let mut taken: std::collections::BTreeMap<u8, usize> = Default::default();
            let mut entries = Vec::new();
            for (at, &hit) in bin.hits.iter().enumerate() {
                let layer = bin.layer_of(at);
                let slot = taken.entry(layer).or_insert(0);
                if *slot >= MAX_SLOTS {
                    continue;
                }
                *slot += 1;
                entries.push(SetEntry {
                    reference: analysis.window(hit).ok_or("A hit of the bin is gone")?,
                    root: Some(ROOT),
                    layer,
                    rr: *slot as u8,
                });
            }
            Ok(entries)
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
            sets.insert(bin, (id, planned.slots().min(MAX_SLOTS)));
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
        if plan.groove {
            let notes = self.groove_notes(analysis, plan, &keys);
            if notes.is_empty() {
                return Err("No hit of the clip is on a pad, so there is no groove to add".into());
            }
            let map = self.project.tempo_map();
            let warp = self.project.warp_of(clip).map(|(_, w)| w);
            let rate = self
                .samples()
                .get(&analysis.hash)
                .map_or(48_000, |s| s.sample_rate);
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
            let mut groove = MidiClip::new(length);
            groove.notes = notes;
            commands.push(Command::AddClip {
                id: self.project.next_clip_id(),
                clip: Clip {
                    node: input,
                    start: source.start,
                    content: ClipContent::Midi(groove),
                },
            });
        }
        let pads = chosen.len();
        Ok((Command::Batch(commands), pads))
    }
}

/// How long a groove note is: a thirty-second note, the pads play one-shots.
const GROOVE_NOTE: i64 = TICKS_PER_QUARTER / 8;

impl Session {
    /// The notes that play the clip's groove on the rack: one per hit that is
    /// in a bin on a pad, at the hit's place on the timeline (through the
    /// clip's warp), on that bin's pad (the lowest key when it is on several),
    /// with a velocity from the hit's level against the loudest, moved into
    /// the hit's velocity layer when the bin has layers. Hits of no bin, or of
    /// a bin on no pad, get none.
    fn groove_notes(
        &self,
        analysis: &DrumAnalysis,
        plan: &DrumRackPlan,
        keys: &[&PadAssignment],
    ) -> Vec<MidiNote> {
        let Some(source) = self.project.clip(analysis.clip) else {
            return Vec::new();
        };
        let Some(rate) = self.samples().get(&analysis.hash).map(|s| s.sample_rate) else {
            return Vec::new();
        };
        let map = self.project.tempo_map();
        let warp = self.project.warp_of(analysis.clip).map(|(_, w)| w);
        // Each bin's pad: keys are sorted, so the first is the lowest.
        let mut pad_of: std::collections::BTreeMap<usize, u8> = Default::default();
        for pad in keys {
            pad_of.entry(pad.bin).or_insert(pad.key);
        }
        /// A hit, its pad's key, and its layer index and the bin's layer count.
        type Placed = (usize, u8, Option<(usize, usize)>);
        let mut placed: Vec<Placed> = Vec::new();
        for (&bin, &key) in &pad_of {
            let planned = &plan.bins[bin];
            let layers = planned.layer_count();
            for (at, &hit) in planned.hits.iter().enumerate() {
                let layer = (layers > 1).then(|| (usize::from(planned.layer_of(at)) - 1, layers));
                placed.push((hit, key, layer));
            }
        }
        let loudest = placed
            .iter()
            .filter_map(|&(hit, _, _)| analysis.hits.get(hit))
            .map(|h| h.level_db)
            .fold(f32::MIN, f32::max);
        let mut notes: Vec<MidiNote> = placed
            .iter()
            .filter_map(|&(hit, key, layer)| {
                let found = analysis.hits.get(hit)?;
                let tick = crate::timeline::clips::tick_of_frame(
                    map,
                    source,
                    analysis.origin + found.onset,
                    rate,
                    warp,
                )?;
                let mut velocity = level_velocity(found.level_db, loudest);
                if let Some((index, layers)) = layer {
                    velocity = velocity_in_layer(velocity, index, layers);
                }
                let mut note = MidiNote::new(
                    Tick((tick.0 - source.start.0).max(0)),
                    Tick(GROOVE_NOTE),
                    key,
                );
                note.velocity = velocity;
                Some(note)
            })
            .collect();
        notes.sort_by_key(|n| (n.start, n.key));
        notes
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
        rig_with(dir, false)
    }

    /// `rig`, and with `overlap` the hit on beat 4 is a kick with a hat over it.
    fn rig_with(dir: &std::path::Path, overlap: bool) -> (Session, ClipId) {
        let mut session = Session::new(crate::session::Nodes::all());
        let (track, input) =
            add_track::with_input(session.project().graph(), || session.new_node_id());
        session.edit([Edit::Apply(track)]);
        let mut audio = vec![0.0f32; 4 * RATE as usize];
        for beat in 0..8 {
            let at = (0.2 + beat as f32 * 0.5) * RATE as f32;
            if overlap && beat == 4 {
                for (i, v) in kick().iter().enumerate() {
                    audio[at as usize + i] += v;
                }
                for (i, v) in hat().iter().enumerate() {
                    audio[at as usize + i] += 1.5 * v;
                }
                continue;
            }
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
                layers: Vec::new(),
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
            groove: false,
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
                layers: Vec::new(),
            }],
            pads: vec![PadAssignment {
                key: 36,
                bin: 0,
                pick: 1,
            }],
            groove: false,
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

    fn midi_clips(session: &Session) -> Vec<(ClipId, Clip)> {
        session
            .project()
            .clips()
            .filter(|(_, c)| c.as_midi().is_some())
            .map(|(id, c)| (id, c.clone()))
            .collect()
    }

    #[test]
    fn a_layered_bin_writes_a_layer_to_each_entry_and_counts_slots_per_layer() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig(dir.path());
        analysed(&mut session, clip);
        let mut plan = plan_of(&session);
        // The first bin: its four hits as a soft pair and a hard pair.
        plan.bins[0].layers = vec![1, 1, 2, 2];
        // A pick past a layer's slots is clamped by the Sampler; the plan's
        // largest layer is two, so the pad's pick is at most two.
        plan.pads[0].pick = 4;
        session.edit([Edit::CreateDrumRack(clip, plan.clone())]);
        let entries_of = |name: &str| {
            let sets = of_type(&session, SAMPLE_SET_ID);
            let (_, node) = sets
                .iter()
                .find(|(_, n)| n.config.get(PORT_NAME) == Some(&Value::Text(name.into())))
                .expect("the set");
            match node.config.get(SAMPLE_KEY) {
                Some(Value::Text(t)) => noodle_nodes::parse_set(t),
                _ => panic!("no entries"),
            }
        };
        let layered: Vec<(u8, u8)> = entries_of("Bin 1")
            .iter()
            .map(|e| (e.layer, e.rr))
            .collect();
        assert_eq!(layered, [(1, 1), (1, 2), (2, 1), (2, 2)]);
        let flat: Vec<(u8, u8)> = entries_of("Bin 2")
            .iter()
            .map(|e| (e.layer, e.rr))
            .collect();
        assert_eq!(flat, [(1, 1), (1, 2), (1, 3), (1, 4)]);
        let pick = of_type(&session, SAMPLER_ID)
            .iter()
            .filter_map(|(_, n)| n.params.get("rr_pick").copied())
            .fold(0.0f32, f32::max);
        assert!(
            pick <= 2.0,
            "the pick is clamped to a layer's slots: {pick}"
        );
        // The sampler really plays the layers: its table has two for the key.
        let tables = noodle_nodes::SetTables::build(&entries_of("Bin 1"));
        assert_eq!(tables.layer_count(), 2);
        assert_ne!(
            tables.layer(ROOT, 0.1).unwrap().entries[0],
            tables.layer(ROOT, 1.0).unwrap().entries[0]
        );
    }

    #[test]
    fn velocity_is_the_level_against_the_loudest_and_stays_in_its_layer() {
        assert_eq!(level_velocity(-10.0, -10.0), 1.0);
        assert!(
            (level_velocity(-22.0, -10.0) - 0.5).abs() < 0.01,
            "12 dB is half"
        );
        assert_eq!(level_velocity(-90.0, -10.0), 0.05, "never silent");
        // The Sampler splits 1..=127 at 64 for two layers, and 1..=42, 43..=85, 86..=127 for three.
        assert_eq!((velocity_in_layer(1.0, 0, 2) * 127.0).round(), 64.0);
        assert_eq!((velocity_in_layer(0.05, 1, 2) * 127.0).round(), 65.0);
        assert_eq!((velocity_in_layer(0.5, 1, 3) * 127.0).round(), 64.0);
        assert_eq!(
            velocity_in_layer(0.7, 0, 1),
            0.7,
            "one layer leaves it alone"
        );
        for layers in 2..=3usize {
            for index in 0..layers {
                for v in [0.05f32, 0.3, 0.6, 1.0] {
                    let midi = (velocity_in_layer(v, index, layers) * 127.0).round() as usize;
                    assert_eq!(((midi - 1) * layers / 127).min(layers - 1), index);
                }
            }
        }
    }

    #[test]
    fn the_groove_is_a_midi_clip_of_the_hits_on_pads_in_the_same_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig(dir.path());
        analysed(&mut session, clip);
        let mut plan = plan_of(&session);
        plan.bins[0].layers = vec![1, 1, 2, 2];
        // Only the kicks (bin 0) are on a pad: hats get no note.
        plan.pads.truncate(1);
        let hits_of_kicks = plan.bins[0].hits.clone();
        plan.groove = true;
        let before = midi_clips(&session).len();
        session.edit([Edit::CreateDrumRack(clip, plan.clone())]);
        let clips = midi_clips(&session);
        assert_eq!(clips.len(), before + 1, "{:?}", session.message());
        let (_, groove) = &clips[0];
        let notes = &groove.as_midi().unwrap().notes;
        assert_eq!(
            notes.len(),
            hits_of_kicks.len(),
            "one note per hit on a pad"
        );
        assert!(notes.iter().all(|n| n.key == 36));
        // At the hit's time: 120 bpm is two beats a second, and 960 ticks a beat.
        let analysis = session.drum_analysis().unwrap();
        let mut want: Vec<f64> = hits_of_kicks
            .iter()
            .map(|&h| analysis.hits[h].onset as f64 / f64::from(RATE) * 2.0 * 960.0)
            .collect();
        want.sort_by(f64::total_cmp);
        let mut at: Vec<i64> = notes.iter().map(|n| n.start.0).collect();
        at.sort_unstable();
        for (got, want) in at.iter().zip(&want) {
            assert!((*got as f64 - want).abs() < 30.0, "{got} vs {want}");
        }
        // The kicks are level, so they play as loud as the layer allows: the
        // soft pair is in the soft layer's velocities and the hard pair is not.
        let mut by_time: Vec<&MidiNote> = notes.iter().collect();
        by_time.sort_by_key(|n| n.start);
        let soft: Vec<f32> = plan.bins[0]
            .hits
            .iter()
            .zip(&plan.bins[0].layers)
            .filter(|(_, l)| **l == 1)
            .map(|(h, _)| analysis.hits[*h].onset)
            .map(|onset| {
                let tick = onset as f64 / f64::from(RATE) * 2.0 * 960.0;
                by_time
                    .iter()
                    .min_by(|a, b| {
                        (a.start.0 as f64 - tick)
                            .abs()
                            .total_cmp(&(b.start.0 as f64 - tick).abs())
                    })
                    .unwrap()
                    .velocity
            })
            .collect();
        assert!(soft.iter().all(|&v| v <= 64.0 / 127.0 + 1e-6), "{soft:?}");
        // One undo takes the rack and the groove away together.
        session.undo();
        assert_eq!(midi_clips(&session).len(), before);
        assert!(of_type(&session, SAMPLE_SET_ID).is_empty());
    }

    #[test]
    fn a_groove_with_no_hit_on_a_pad_is_refused_and_off_makes_no_clip() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig(dir.path());
        analysed(&mut session, clip);
        let plan = plan_of(&session);
        let before = midi_clips(&session).len();
        session.edit([Edit::CreateDrumRack(clip, plan.clone())]);
        assert_eq!(midi_clips(&session).len(), before, "off by default");
        // A bin that has no hits cannot be played, so nothing is on a pad.
        let empty = DrumRackPlan {
            bins: vec![PlannedBin {
                name: "None".into(),
                hits: vec![99],
                layers: Vec::new(),
            }],
            pads: vec![PadAssignment {
                key: 36,
                bin: 0,
                pick: 1,
            }],
            groove: true,
        };
        session.edit([Edit::CreateDrumRack(clip, empty)]);
        assert_eq!(midi_clips(&session).len(), before);
    }

    #[test]
    fn general_midi_names_cover_35_to_81() {
        assert_eq!(gm_drum_name(36), Some("Bass Drum"));
        assert_eq!(gm_drum_name(81), Some("Open Triangle"));
        assert_eq!(gm_drum_name(34), None);
        assert_eq!(gm_drum_name(82), None);
        assert_eq!(pad_name(34), "A#1");
    }
    /// Splits the mix at 300 Hz into a kick (below) and a hat (the rest), and
    /// counts how often it was opened and run. `gate` holds a run until false.
    struct Bands {
        gate: Arc<std::sync::atomic::AtomicBool>,
    }

    impl PartSeparator for Bands {
        fn model(&self) -> ModelId {
            ModelId {
                id: "larsnet".into(),
                version: "1".into(),
            }
        }
        fn sample_rate(&self) -> u32 {
            RATE
        }
        fn parts(&self) -> Vec<DrumPart> {
            vec![DrumPart::Kick, DrumPart::HiHat]
        }
        fn separate(
            &mut self,
            mix: &Stereo,
            progress: &mut dyn FnMut(f32) -> bool,
        ) -> Result<Vec<(DrumPart, Stereo)>, SeparateError> {
            while self.gate.load(std::sync::atomic::Ordering::SeqCst) {
                if !progress(0.3) {
                    return Err(SeparateError::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let k = 1.0 - (-TAU * 300.0 / RATE as f32).exp();
            let mut low = Stereo::silent(mix.len());
            let mut high = Stereo::silent(mix.len());
            // Four poles, so the kick's upper harmonics stay out of the hat.
            let kh = 1.0 - (-TAU * 2000.0 / RATE as f32).exp();
            let mut state = [0.0f32; 4];
            let mut hp = [0.0f32; 2];
            for i in 0..mix.len() {
                let mut x = mix.left[i];
                for pole in &mut state {
                    *pole += k * (x - *pole);
                    x = *pole;
                }
                low.left[i] = x;
                low.right[i] = x;
                let mut y = mix.left[i];
                for pole in &mut hp {
                    *pole += kh * (y - *pole);
                    y -= *pole;
                }
                high.left[i] = y;
                high.right[i] = y;
            }
            Ok(vec![(DrumPart::Kick, low), (DrumPart::HiHat, high)])
        }
    }

    /// Makes the session separate with `Bands`; returns how many times it was
    /// opened and the gate.
    fn with_bands(
        session: &mut Session,
        held: bool,
    ) -> (
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        let opened = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(held));
        let (o, g) = (opened.clone(), gate.clone());
        session.use_part_opener(Arc::new(move |_, _| {
            o.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Box::new(Bands { gate: g.clone() }) as Box<dyn PartSeparator>)
        }));
        (opened, gate)
    }

    fn separated(session: &mut Session) {
        session.edit([Edit::SeparateDrumHits("larsnet".into())]);
        let deadline = Instant::now() + Duration::from_secs(60);
        while session.drum_separation_progress().is_some() {
            assert!(Instant::now() < deadline, "the separation never finished");
            std::thread::sleep(Duration::from_millis(5));
            session.maintain();
        }
    }

    #[test]
    fn the_separation_marks_the_hit_with_two_parts_sounding_as_mixed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig_with(dir.path(), true);
        with_bands(&mut session, false);
        analysed(&mut session, clip);
        assert!(session.drum_analysis().unwrap().separation.is_none());
        separated(&mut session);
        let analysis = session.drum_analysis().unwrap();
        let separation = analysis.separation.as_ref().expect("a separation");
        assert_eq!(separation.hits.len(), analysis.hits.len());
        let mixed: Vec<usize> = (0..analysis.hits.len())
            .filter(|&h| analysis.separated_mixed(h))
            .collect();
        // The kick over hat is the fifth hit, and the only mixed one.
        assert_eq!(
            mixed,
            vec![4],
            "{:?}",
            separation
                .hits
                .iter()
                .map(|h| &h.levels)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            separation.hits[4].active.len(),
            2,
            "a kick and a hat sound there"
        );
        assert_eq!(separation.hits[0].active, vec![DrumPart::Kick]);
        assert_eq!(separation.hits[1].active, vec![DrumPart::HiHat]);
    }

    #[test]
    fn asking_again_for_the_same_clip_and_model_reuses_the_separation() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig_with(dir.path(), true);
        let (opened, _) = with_bands(&mut session, false);
        analysed(&mut session, clip);
        separated(&mut session);
        session.close_drum_analysis();
        analysed(&mut session, clip);
        separated(&mut session);
        assert!(session.drum_analysis().unwrap().separation.is_some());
        assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn cancelling_a_separation_leaves_the_analysis_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig_with(dir.path(), true);
        let (_, gate) = with_bands(&mut session, true);
        analysed(&mut session, clip);
        session.edit([Edit::SeparateDrumHits("larsnet".into())]);
        assert!(session.drum_separation_progress().is_some());
        session.edit([Edit::CancelDrumSeparation]);
        let deadline = Instant::now() + Duration::from_secs(60);
        while session.drum_separation_progress().is_some() {
            assert!(Instant::now() < deadline, "the cancel never took");
            std::thread::sleep(Duration::from_millis(5));
            session.maintain();
        }
        gate.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(session.message(), Some("Separation cancelled"));
        let analysis = session.drum_analysis().expect("the analysis is kept");
        assert!(analysis.separation.is_none());
        assert_eq!(analysis.hits.len(), 8);
    }

    #[test]
    fn a_model_that_cannot_open_says_so_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig_with(dir.path(), true);
        session.use_part_opener(Arc::new(|_, _| Err("not downloaded".into())));
        analysed(&mut session, clip);
        separated(&mut session);
        assert_eq!(
            session.message(),
            Some("Couldn't separate the drums with larsnet: not downloaded")
        );
        assert!(session.drum_analysis().unwrap().separation.is_none());
    }

    #[test]
    fn taking_a_part_swaps_the_hits_audio_and_leaving_it_swaps_back() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig_with(dir.path(), true);
        with_bands(&mut session, false);
        analysed(&mut session, clip);
        separated(&mut session);
        let own = session.drum_analysis().unwrap().window(4).unwrap();
        session.edit([Edit::TakeSeparatedPart(4, Some(DrumPart::Kick))]);
        let analysis = session.drum_analysis().unwrap();
        assert!(analysis.is_taken(4));
        let taken = analysis.window(4).unwrap();
        assert_ne!(taken.hash, own.hash, "the part is its own sample");
        assert_eq!((taken.start, taken.end), (0.0, 1.0));
        let hit = &analysis.hits[4];
        let sample = session.samples().get(&taken.hash).unwrap();
        assert_eq!(sample.frames() as u64, hit.end - hit.start);
        // The rack plays the part.
        let plan = DrumRackPlan {
            bins: vec![PlannedBin {
                name: "Kick".into(),
                hits: vec![4],
                layers: Vec::new(),
            }],
            pads: vec![PadAssignment {
                key: 36,
                bin: 0,
                pick: 1,
            }],
            groove: false,
        };
        session.edit([Edit::CreateDrumRack(clip, plan)]);
        let sets = of_type(&session, SAMPLE_SET_ID);
        let Some(Value::Text(entries)) = sets[0].1.config.get(SAMPLE_KEY) else {
            panic!("no entries");
        };
        assert_eq!(
            noodle_nodes::parse_set(entries)[0].reference.hash,
            taken.hash
        );
        session.edit([Edit::TakeSeparatedPart(4, None)]);
        let analysis = session.drum_analysis().unwrap();
        assert!(!analysis.is_taken(4));
        assert_eq!(analysis.window(4).unwrap(), own);
    }
}
