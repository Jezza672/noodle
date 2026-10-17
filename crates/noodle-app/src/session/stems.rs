//! Split to stems: an audio clip becomes one new track under its own for each
//! stem and for the residual, and the clip itself is muted, as one undo step.
//!
//! The separating runs on a background job (and downloads the model first,
//! once the user has agreed). It writes the parts as WAV files into the
//! project's `stems` folder; when it is done the clips are added in a single
//! command.

use std::sync::Arc;

use noodle_core::group::PORT_NAME;
use noodle_core::{Clip, ClipContent, ClipId, NodeId, Value};
use noodle_engine::{Job, JobPanicked};
use noodle_stems::{
    DownloadError, ModelSpec, ModelStore, RuntimeSpec, SplitError, SplitOutput, SplitRequest, split,
};

use super::*;
use crate::timeline::add_track;

/// The folder, next to the project file, that stems are written to.
pub const STEMS_DIR: &str = "stems";

type OpenSeparator =
    Arc<dyn Fn() -> Result<Box<dyn noodle_stems::Separator>, String> + Send + Sync>;

/// Where the separator comes from.
enum Source {
    /// HT-Demucs through ONNX Runtime, from weights that may need downloading.
    Model {
        store: Option<ModelStore>,
        spec: ModelSpec,
        /// ONNX Runtime for this platform, downloaded when the machine has none.
        runtime: Option<RuntimeSpec>,
    },
    /// Whatever opens one: tests use a fake that needs no weights.
    #[cfg(test)]
    Custom(OpenSeparator),
}

/// Whether `ORT_DYLIB_PATH` names a runtime to use instead of the pinned one.
fn ort_path_set() -> bool {
    std::env::var("ORT_DYLIB_PATH").is_ok_and(|p| !p.is_empty())
}

/// The runtime in `store`: the pinned download, else a library put in the
/// folder by hand.
fn runtime_in(store: &ModelStore, spec: Option<&RuntimeSpec>) -> Option<PathBuf> {
    let pinned = spec.map(|r| store.runtime_path(r));
    pinned.filter(|p| p.is_file()).or_else(|| {
        [
            "libonnxruntime.so",
            "libonnxruntime.dylib",
            "onnxruntime.dll",
        ]
        .into_iter()
        .map(|name| store.dir().join(name))
        .find(|path| path.is_file())
    })
}

/// What a split has to download first.
#[derive(Default)]
struct Downloads {
    model: Option<(ModelStore, ModelSpec)>,
    runtime: Option<(ModelStore, RuntimeSpec)>,
}

/// A split under way.
struct Run {
    job: Job<Result<SplitOutput, SplitError>>,
    clip: ClipId,
    name: String,
}

pub(super) struct Stems {
    source: Source,
    run: Option<Run>,
    /// The clip whose split waits for the user to agree to the download.
    asking: Option<ClipId>,
    /// Tests of the asking don't have an ONNX Runtime to load.
    #[cfg(test)]
    skip_runtime_check: bool,
}

impl Stems {
    pub(super) fn new() -> Self {
        let store = directories::ProjectDirs::from("", "", "Noodle")
            .map(|dirs| ModelStore::new(dirs.data_dir().join("models")));
        Self {
            source: Source::Model {
                store,
                spec: ModelSpec::htdemucs(),
                runtime: RuntimeSpec::current(),
            },
            run: None,
            asking: None,
            #[cfg(test)]
            skip_runtime_check: false,
        }
    }

    /// Whether something (the weights, or ONNX Runtime) has to be downloaded
    /// before a split can start.
    fn needs_download(&self) -> bool {
        match &self.source {
            Source::Model { store, spec, .. } => {
                store.as_ref().is_none_or(|s| !s.is_present(spec)) || self.runtime_to_download()
            }
            #[cfg(test)]
            Source::Custom(_) => false,
        }
    }

    /// Whether ONNX Runtime has to be downloaded: the pinned one is not in the
    /// model folder and `ORT_DYLIB_PATH` doesn't name one. The system's own
    /// library is never probed: `ort` keeps whatever it loads first for the
    /// whole process (even one it refuses), which would shadow the pinned
    /// runtime, and a machine like a Windows one ships an unpinned
    /// `onnxruntime.dll` of its own.
    fn runtime_to_download(&self) -> bool {
        #[cfg(test)]
        if self.skip_runtime_check {
            return false;
        }
        match &self.source {
            Source::Model {
                store: Some(store),
                runtime: Some(runtime),
                ..
            } => !store.has_runtime(runtime) && !ort_path_set(),
            _ => false,
        }
    }

    /// The ONNX Runtime library to open the model with: the pinned download,
    /// else one put in the model folder by hand. `None` leaves it to
    /// `ORT_DYLIB_PATH` and the system.
    fn runtime_in_folder(&self) -> Option<PathBuf> {
        match &self.source {
            Source::Model {
                store: Some(store),
                runtime,
                ..
            } => runtime_in(store, runtime.as_ref()),
            _ => None,
        }
    }

    /// Why the model can't be used before it is downloaded: no address, or no
    /// ONNX Runtime to run it with. Checked before asking, so nothing large is
    /// downloaded for a machine that couldn't use it.
    fn cannot_start(&self) -> Option<String> {
        if let Some(why) = self.cannot_download() {
            return Some(why);
        }
        #[cfg(test)]
        if self.skip_runtime_check {
            return None;
        }
        match &self.source {
            // Downloaded along with the weights.
            Source::Model { .. } if self.runtime_to_download() => None,
            Source::Model { .. } => {
                noodle_stems::OnnxSeparator::load_runtime(self.runtime_in_folder().as_deref())
                    .err()
                    .map(|e| e.to_string())
            }
            #[cfg(test)]
            Source::Custom(_) => None,
        }
    }

    /// Why the model can't be downloaded, if no address is set for it.
    fn cannot_download(&self) -> Option<String> {
        match &self.source {
            Source::Model { store, spec, .. }
                if spec.url.is_none() && store.as_ref().is_none_or(|s| !s.is_present(spec)) =>
            {
                Some(format!(
                    "The stem model isn't installed. Put {} in {}, or set {} to where it can be downloaded from",
                    spec.file,
                    store.as_ref().map_or_else(
                        || "the Noodle data folder".to_owned(),
                        |s| s.dir().display().to_string()
                    ),
                    noodle_stems::URL_VARIABLE,
                ))
            }
            Source::Model { .. } => None,
            #[cfg(test)]
            Source::Custom(_) => None,
        }
    }

    /// What the job needs to get a separator, and the download it may have to
    /// make first.
    fn job_parts(&self) -> Result<(OpenSeparator, Downloads), String> {
        match &self.source {
            Source::Model {
                store,
                spec,
                runtime,
            } => {
                let store = store
                    .clone()
                    .ok_or("There is nowhere to keep the stem model on this computer")?;
                let downloads = Downloads {
                    model: (!store.is_present(spec)).then(|| (store.clone(), spec.clone())),
                    runtime: runtime
                        .clone()
                        .filter(|_| self.runtime_to_download())
                        .map(|r| (store.clone(), r)),
                };
                let (weights, spec, pinned) = (store.path(spec), spec.clone(), runtime.clone());
                let open: OpenSeparator = Arc::new(move || {
                    // Looked for when opening, after any download.
                    let runtime = runtime_in(&store, pinned.as_ref());
                    noodle_stems::OnnxSeparator::open(&weights, runtime.as_deref(), spec.model_id())
                        .map(|s| Box::new(s) as Box<dyn noodle_stems::Separator>)
                        .map_err(|e| e.to_string())
                });
                Ok((open, downloads))
            }
            #[cfg(test)]
            Source::Custom(open) => Ok((open.clone(), Downloads::default())),
        }
    }
}

impl Session {
    /// Starts Split to stems on audio clip `clip`. When the model isn't on
    /// this computer yet, this asks first (see [`stems_consent`](Self::stems_consent)).
    pub fn split_to_stems(&mut self, clip: ClipId) {
        if self.stems.run.is_some() {
            self.message = Some("A split is already running".into());
            return;
        }
        if self.project.clip(clip).and_then(|c| c.as_audio()).is_none() {
            self.message = Some("Only audio clips can be split into stems".into());
            return;
        }
        if self.directory().is_none() {
            self.message =
                Some("Save the project first: the stems are written next to its file".into());
            return;
        }
        if self.stems.needs_download() {
            if let Some(why) = self.stems.cannot_start() {
                self.message = Some(why);
                return;
            }
            self.stems.asking = Some(clip);
        } else {
            self.start_split(clip);
        }
    }

    /// The clip whose split is waiting for the user to agree to downloading
    /// the model, if there is one.
    pub fn stems_consent(&self) -> Option<ClipId> {
        self.stems.asking
    }

    /// Where the model would be downloaded to, for the dialog to say.
    pub fn stems_model_folder(&self) -> Option<PathBuf> {
        match &self.stems.source {
            Source::Model { store, .. } => store.as_ref().map(|s| s.dir().to_owned()),
            #[cfg(test)]
            Source::Custom(_) => None,
        }
    }

    /// The user agreed: download the model and split.
    pub fn allow_stem_download(&mut self) {
        if let Some(clip) = self.stems.asking.take() {
            self.start_split(clip);
        }
    }

    /// The user declined: nothing is downloaded and nothing is split.
    pub fn decline_stem_download(&mut self) {
        self.stems.asking = None;
    }

    fn start_split(&mut self, clip: ClipId) {
        let Some(dir) = self.directory().map(Path::to_path_buf) else {
            return;
        };
        let Some(audio) = self.project.clip(clip).and_then(|c| c.as_audio()) else {
            return;
        };
        let (open, downloads) = match self.stems.job_parts() {
            Ok(parts) => parts,
            Err(why) => {
                self.message = Some(why);
                return;
            }
        };
        let name = Path::new(&audio.source)
            .file_stem()
            .map_or_else(|| "clip".to_owned(), |s| s.to_string_lossy().into_owned());
        let request = SplitRequest {
            source: dir.join(&audio.source),
            frames: audio.offset + audio.length,
            out_dir: dir.join(STEMS_DIR),
            name: name.clone(),
        };
        // The downloads, when there are any, are the first part of the bar: the
        // runtime (a few to 70 MB) then the weights (over 300 MB).
        let runtime_share = if downloads.runtime.is_some() {
            0.03
        } else {
            0.0
        };
        let model_share = if downloads.model.is_some() { 0.27 } else { 0.0 };
        let share = runtime_share + model_share;
        let failed = |e: DownloadError| match e {
            DownloadError::Cancelled => SplitError::Cancelled,
            other => SplitError::Failed(other.to_string()),
        };
        let job = Job::spawn(move |progress| {
            if let Some((store, spec)) = downloads.runtime {
                store
                    .download_runtime(&spec, &mut |f| progress.report(f * runtime_share).is_ok())
                    .map_err(failed)?;
            }
            if let Some((store, spec)) = downloads.model {
                store
                    .download(&spec, &mut |f| {
                        progress.report(runtime_share + f * model_share).is_ok()
                    })
                    .map_err(failed)?;
            }
            let mut separator = open().map_err(SplitError::Failed)?;
            split(&request, &mut *separator, &mut |f| {
                progress.report(share + (1.0 - share) * f).is_ok()
            })
        });
        self.stems.run = Some(Run { job, clip, name });
    }

    /// How far the split under way has got, from 0 to 1, and what it is
    /// splitting.
    pub fn split_progress(&self) -> Option<(f32, &str)> {
        self.stems
            .run
            .as_ref()
            .map(|run| (run.job.fraction(), run.name.as_str()))
    }

    /// Stops the split under way. Nothing is left of it.
    pub fn cancel_split(&mut self) {
        if let Some(run) = &self.stems.run {
            run.job.cancel();
        }
    }

    /// Takes in a finished split: adds the tracks and clips, or says why not.
    pub(super) fn poll_split(&mut self) {
        let Some(run) = &mut self.stems.run else {
            return;
        };
        let Some(done) = run.job.poll() else {
            return;
        };
        let Run { clip, name, .. } = self.stems.run.take().expect("just polled");
        match done {
            Ok(Ok(output)) => match self.stems_command(clip, &output) {
                Ok(command) => {
                    self.edit([Edit::Apply(command)]);
                    self.message = Some(format!("Split {name} into stems"));
                }
                Err(why) => {
                    for file in &output.files {
                        let _ = std::fs::remove_file(&file.path);
                    }
                    self.message = Some(format!("Couldn't split {name}: {why}"));
                }
            },
            Ok(Err(SplitError::Cancelled)) => self.message = Some("Split cancelled".into()),
            Ok(Err(error)) => self.message = Some(format!("Couldn't split {name}: {error}")),
            Err(JobPanicked) => {
                self.message = Some(format!("Couldn't split {name}: {JobPanicked}"))
            }
        }
    }

    /// The command that adds a track and a clip for each part in `output`
    /// under the track of `clip`, and mutes `clip`: one undo step.
    ///
    /// Each new clip is the source's section again (its place, window, gain
    /// and fades) over the part's file, with a warp of its own copied from the
    /// source's, never the source's warp itself, which would link them.
    fn stems_command(&self, id: ClipId, output: &SplitOutput) -> Result<Command, String> {
        let project = &self.project;
        let source = project
            .clip(id)
            .ok_or("the clip was removed while it was being split")?;
        let audio = source
            .as_audio()
            .ok_or("the clip was removed while it was being split")?;
        if audio.offset + audio.length > output.frames {
            return Err("the clip was changed while it was being split".into());
        }
        let name = Path::new(&audio.source)
            .file_stem()
            .map_or_else(|| "clip".to_owned(), |s| s.to_string_lossy().into_owned());
        let source_group = project
            .graph()
            .node(source.node)
            .and_then(|n| n.parent)
            .unwrap_or(source.node);
        let warp = audio.warp.and_then(|w| project.warp(w)).cloned();
        let (mut next_clip, mut next_warp) = (project.next_clip_id(), project.next_warp_id());

        // Tracks are added one after another to a copy, so that each finds the
        // mixer input the one before took.
        let mut scratch = project.clone();
        let mut commands = Vec::new();
        let mut groups = Vec::new();
        for file in &output.files {
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
            let rename = Command::SetConfig {
                node: group,
                key: PORT_NAME.into(),
                value: Some(Value::Text(format!("{name} ({})", file.part.label()))),
            };
            rename
                .clone()
                .apply(&mut scratch)
                .map_err(|e| e.to_string())?;
            commands.push(track);
            commands.push(rename);
            groups.push(group);

            let mut part = audio.clone();
            part.source = format!("{STEMS_DIR}/{}", file.file);
            part.muted = false;
            part.warp = warp.as_ref().map(|warp| {
                let warp_id = next_warp;
                next_warp.0 += 1;
                commands.push(Command::AddWarp {
                    id: warp_id,
                    warp: warp.clone(),
                });
                warp_id
            });
            commands.push(Command::AddClip {
                id: next_clip,
                clip: Clip {
                    node: input,
                    start: source.start,
                    content: ClipContent::Audio(part),
                },
            });
            next_clip.0 += 1;
        }

        let mut muted = source.clone();
        if let ClipContent::Audio(audio) = &mut muted.content {
            audio.muted = true;
        }
        commands.push(Command::SetClip { id, clip: muted });
        commands.push(Command::SetTrackOrder(order_after(
            &scratch,
            source_group,
            &groups,
        )));
        Ok(Command::Batch(commands))
    }

    /// Makes `open` the way the next split gets its separator, instead of the
    /// model, so a test needs no weights.
    #[cfg(test)]
    pub(crate) fn use_separator(
        &mut self,
        open: impl Fn() -> Box<dyn noodle_stems::Separator> + Send + Sync + 'static,
    ) {
        self.stems.source = Source::Custom(Arc::new(move || Ok(open())));
    }
}

/// The track order of `project` with `new` (which are last) moved to just
/// after `after`.
fn order_after(project: &Project, after: NodeId, new: &[NodeId]) -> Vec<NodeId> {
    let mut order: Vec<NodeId> = crate::timeline::tracks(project)
        .into_iter()
        .map(|input| {
            project
                .graph()
                .node(input)
                .and_then(|n| n.parent)
                .unwrap_or(input)
        })
        .filter(|group| !new.contains(group))
        .collect();
    let at = order
        .iter()
        .position(|&group| group == after)
        .map_or(order.len(), |i| i + 1);
    order.splice(at..at, new.iter().copied());
    order
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use noodle_stems::{FakeSeparator, ModelId, SeparateError, Separator, Stereo};

    use super::*;

    const RATE: u32 = 44_100;

    /// A saved project with a one-second file on a track, as a clip of
    /// `window` frames from `offset`.
    fn rig(dir: &Path, offset: u64, window: u64) -> (Session, ClipId, NodeId) {
        let mut session = Session::new(crate::session::Nodes::all());
        let (track, input) =
            add_track::with_input(session.project().graph(), || session.new_node_id());
        session.edit([Edit::Apply(track)]);
        let samples: Vec<f32> = (0..RATE as usize)
            .flat_map(|i| {
                let t = i as f32 / RATE as f32;
                let tone = |hz: f32| (std::f32::consts::TAU * hz * t).sin();
                let x = 0.3 * tone(80.0) + 0.2 * tone(900.0) + 0.1 * tone(7_000.0);
                [x, 0.5 * x]
            })
            .collect();
        noodle_io::write_wav(&dir.join("song.wav"), &samples, 2, RATE).unwrap();
        let id = session.project().next_clip_id();
        let mut clip = Clip::audio(input, Tick(960), "song.wav", window);
        if let ClipContent::Audio(audio) = &mut clip.content {
            audio.offset = offset;
            audio.gain = 0.5;
            audio.fade_in = 100;
            audio.fade_out = 200;
        }
        session.edit([Edit::Apply(Command::AddClip { id, clip })]);
        assert!(session.save_as(&dir.join("project.ron")));
        (session, id, input)
    }

    fn wait(session: &mut Session) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while session.split_progress().is_some() {
            assert!(Instant::now() < deadline, "the split never finished");
            std::thread::sleep(Duration::from_millis(5));
            session.maintain();
        }
    }

    fn fake() -> Box<dyn Separator> {
        Box::new(FakeSeparator::new())
    }

    fn names(session: &Session) -> Vec<String> {
        let project = session.project();
        crate::timeline::tracks(project)
            .into_iter()
            .map(|input| {
                let group = project.graph().node(input).and_then(|n| n.parent).unwrap();
                match project.graph().node(group).unwrap().config.get(PORT_NAME) {
                    Some(Value::Text(name)) => name.clone(),
                    _ => String::new(),
                }
            })
            .collect()
    }

    #[test]
    fn splitting_a_clip_adds_a_track_for_each_part_and_mutes_the_source_in_one_undo() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), 4_000, 30_000);
        // Warp the source, so its copies have something to copy.
        let command = session
            .project()
            .warp_edit(id, |warp| {
                warp.tempo = noodle_core::TempoCurve::constant(100.0);
                warp.mode = noodle_core::WarpMode::Grain;
            })
            .unwrap();
        session.edit([Edit::Apply(command)]);
        let before = session.project().clone();
        let source_warp = before.clip(id).unwrap().as_audio().unwrap().warp.unwrap();
        session.use_separator(fake);

        session.split_to_stems(id);
        assert!(session.split_progress().is_some());
        wait(&mut session);
        assert_eq!(session.message(), Some("Split song into stems"));

        // A track each, named for the clip and the part, right under the source.
        assert_eq!(
            names(&session),
            [
                "Track 1",
                "song (drums)",
                "song (bass)",
                "song (vocals)",
                "song (other)",
                "song (residual)"
            ]
        );
        let project = session.project();
        let source = project.clip(id).unwrap().as_audio().unwrap();
        assert!(source.muted, "the source stays, muted");
        let parts: Vec<_> = project.clips().filter(|(other, _)| *other != id).collect();
        assert_eq!(parts.len(), 5);
        let mut warps = vec![source_warp];
        for (n, label) in ["drums", "bass", "vocals", "other", "residual"]
            .into_iter()
            .enumerate()
        {
            let (_, clip) = parts
                .iter()
                .find(|(_, c)| c.as_audio().unwrap().source == format!("stems/song-{label}.wav"))
                .unwrap_or_else(|| panic!("no clip for {label}"));
            let audio = clip.as_audio().unwrap();
            assert_eq!(clip.start, Tick(960));
            assert_eq!(
                (audio.offset, audio.length, audio.fade_in, audio.fade_out),
                (4_000, 30_000, 100, 200)
            );
            assert_eq!(audio.gain, 0.5);
            assert!(!audio.muted);
            let warp = audio.warp.expect("copied the warp");
            assert_eq!(project.warp(warp), project.warp(source_warp));
            warps.push(warp);
            assert!(dir.path().join(format!("stems/song-{label}.wav")).is_file());
            // Each is on a track of its own.
            assert_eq!(
                crate::timeline::tracks(project)
                    .iter()
                    .filter(|t| **t == clip.node)
                    .count(),
                1,
                "{n}"
            );
        }
        warps.sort();
        warps.dedup();
        assert_eq!(warps.len(), 6, "every clip has its own warp");
        let distinct: BTreeSet<_> = parts.iter().map(|(_, c)| c.node).collect();
        assert_eq!(distinct.len(), 5);

        // The stems play: the engine takes the clips without complaint.
        assert!(
            session.clip_problems().is_empty(),
            "{:?}",
            session.clip_problems()
        );

        // One undo takes all of it back, and one redo restores it.
        session.undo();
        assert_eq!(session.project().clips().count(), 1);
        assert_eq!(*session.project(), before);
        session.redo();
        assert_eq!(session.project().clips().count(), 6);
        assert!(
            session
                .project()
                .clip(id)
                .unwrap()
                .as_audio()
                .unwrap()
                .muted
        );
    }

    #[test]
    fn an_unwarped_clip_gets_unwarped_stems() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), 0, RATE as u64);
        session.use_separator(fake);
        session.split_to_stems(id);
        wait(&mut session);
        assert!(
            session
                .project()
                .clips()
                .all(|(_, c)| c.as_audio().unwrap().warp.is_none())
        );
        assert_eq!(session.project().clips().count(), 6);
    }

    #[test]
    fn splitting_needs_a_saved_project_and_an_audio_clip() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _, input) = rig(dir.path(), 0, 1_000);
        session.use_separator(fake);
        let midi = session.project().next_clip_id();
        session.edit([Edit::Apply(Command::AddClip {
            id: midi,
            clip: Clip::midi(input, Tick(0), Tick(960)),
        })]);
        session.split_to_stems(midi);
        assert_eq!(
            session.message(),
            Some("Only audio clips can be split into stems")
        );
        assert!(session.split_progress().is_none());

        let mut unsaved = Session::new(crate::session::Nodes::all());
        unsaved.use_separator(fake);
        let (track, input) =
            add_track::with_input(unsaved.project().graph(), || unsaved.new_node_id());
        unsaved.edit([Edit::Apply(track)]);
        let clip = unsaved.project().next_clip_id();
        unsaved.edit([Edit::Apply(Command::AddClip {
            id: clip,
            clip: Clip::audio(
                input,
                Tick(0),
                dir.path().join("song.wav").to_string_lossy(),
                1_000,
            ),
        })]);
        unsaved.split_to_stems(clip);
        assert!(
            unsaved
                .message()
                .unwrap()
                .starts_with("Save the project first")
        );
        assert!(unsaved.split_progress().is_none());
    }

    /// Runs until told to go, then cancels or finishes as it is asked.
    struct Gate {
        go: Arc<AtomicBool>,
        inner: FakeSeparator,
    }

    impl Separator for Gate {
        fn model(&self) -> ModelId {
            self.inner.model()
        }

        fn separate(
            &mut self,
            mix: &Stereo,
            progress: &mut dyn FnMut(f32) -> bool,
        ) -> Result<[Stereo; 4], SeparateError> {
            while !self.go.load(Ordering::SeqCst) {
                if !progress(0.5) {
                    return Err(SeparateError::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            self.inner.separate(mix, progress)
        }
    }

    fn gated(session: &mut Session) -> Arc<AtomicBool> {
        let go = Arc::new(AtomicBool::new(false));
        let theirs = go.clone();
        session.use_separator(move || {
            Box::new(Gate {
                go: theirs.clone(),
                inner: FakeSeparator::new(),
            })
        });
        go
    }

    #[test]
    fn a_cancelled_split_changes_nothing_and_leaves_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), 0, 20_000);
        let _go = gated(&mut session);
        let before = session.project().clone();
        session.split_to_stems(id);
        assert!(session.split_progress().is_some());
        session.split_to_stems(id);
        assert_eq!(session.message(), Some("A split is already running"));
        session.cancel_split();
        wait(&mut session);
        assert_eq!(session.message(), Some("Split cancelled"));
        assert_eq!(*session.project(), before);
        let left = std::fs::read_dir(dir.path().join("stems"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(left, 0);
    }

    #[test]
    fn a_clip_removed_during_the_split_adds_nothing_and_leaves_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), 0, 20_000);
        let go = gated(&mut session);
        session.split_to_stems(id);
        session.edit([Edit::Apply(Command::RemoveClip { id })]);
        let before = session.project().clone();
        go.store(true, Ordering::SeqCst);
        wait(&mut session);
        assert!(
            session
                .message()
                .unwrap()
                .contains("removed while it was being split"),
            "{:?}",
            session.message()
        );
        assert_eq!(*session.project(), before);
        let left = std::fs::read_dir(dir.path().join("stems")).unwrap().count();
        assert_eq!(left, 0);
    }

    #[test]
    fn the_download_is_asked_for_first_and_declining_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), 0, 20_000);
        let spec = ModelSpec {
            id: "test",
            version: "1",
            file: "test.onnx",
            // Nothing listens here: allowed, the download fails.
            url: Some("http://127.0.0.1:1/model.onnx".into()),
            sha256: None,
        };
        session.stems.source = Source::Model {
            store: Some(ModelStore::new(dir.path().join("models"))),
            spec,
            runtime: None,
        };
        session.stems.skip_runtime_check = true;
        let before = session.project().clone();

        session.split_to_stems(id);
        assert_eq!(session.stems_consent(), Some(id));
        assert!(session.split_progress().is_none(), "nothing starts unasked");
        session.decline_stem_download();
        assert_eq!(session.stems_consent(), None);
        assert!(session.split_progress().is_none());
        assert!(!dir.path().join("models").exists(), "nothing downloaded");

        session.split_to_stems(id);
        session.allow_stem_download();
        wait(&mut session);
        assert!(
            session.message().unwrap().contains("the download failed"),
            "{:?}",
            session.message()
        );
        assert_eq!(*session.project(), before);
    }

    #[test]
    fn a_missing_runtime_is_downloaded_with_the_weights_after_asking() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), 0, 20_000);
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        // The weights are here already; only the runtime is missing.
        std::fs::write(models.join("test.onnx"), b"not a model").unwrap();
        let mut runtime = RuntimeSpec::for_target("linux", "x86_64").unwrap();
        // Nothing listens here: allowed, the download fails.
        runtime.url = "http://127.0.0.1:1/onnxruntime.tgz".into();
        session.stems.source = Source::Model {
            store: Some(ModelStore::new(&models)),
            spec: ModelSpec {
                id: "test",
                version: "1",
                file: "test.onnx",
                url: None,
                sha256: None,
            },
            runtime: Some(runtime),
        };
        // The system's own runtime (if any) is never consulted, so this holds
        // on every machine.
        session.split_to_stems(id);
        assert_eq!(
            session.stems_consent(),
            Some(id),
            "asked before downloading"
        );
        session.allow_stem_download();
        wait(&mut session);
        assert!(
            session.message().unwrap().contains("the download failed"),
            "{:?}",
            session.message()
        );
    }

    #[test]
    fn without_weights_or_an_address_it_says_where_to_put_them() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), 0, 20_000);
        session.stems.source = Source::Model {
            store: Some(ModelStore::new(dir.path().join("models"))),
            spec: ModelSpec {
                id: "test",
                version: "1",
                file: "test.onnx",
                url: None,
                sha256: None,
            },
            runtime: None,
        };
        session.split_to_stems(id);
        let message = session.message().unwrap();
        assert!(message.contains("test.onnx") && message.contains("NOODLE_STEM_MODEL_URL"));
        assert_eq!(session.stems_consent(), None);
    }

    #[test]
    fn a_runtime_that_does_not_load_is_found_out_before_anything_is_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), 0, 20_000);
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        // Whatever the platform calls the library, this one isn't.
        for name in [
            "libonnxruntime.so",
            "libonnxruntime.dylib",
            "onnxruntime.dll",
        ] {
            std::fs::write(models.join(name), b"not a library").unwrap();
        }
        session.stems.source = Source::Model {
            store: Some(ModelStore::new(&models)),
            spec: ModelSpec {
                id: "test",
                version: "1",
                file: "test.onnx",
                url: Some("http://127.0.0.1:1/model.onnx".into()),
                sha256: None,
            },
            runtime: None,
        };
        session.split_to_stems(id);
        assert!(
            session
                .message()
                .unwrap()
                .contains("can't load ONNX Runtime"),
            "{:?}",
            session.message()
        );
        assert_eq!(session.stems_consent(), None, "not asked to download");
        assert!(session.split_progress().is_none());
    }

    #[test]
    fn weights_that_are_there_but_no_runtime_fail_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), 0, 20_000);
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("test.onnx"), b"not a model").unwrap();
        session.stems.source = Source::Model {
            store: Some(ModelStore::new(&models)),
            spec: ModelSpec {
                id: "test",
                version: "1",
                file: "test.onnx",
                url: None,
                sha256: None,
            },
            runtime: None,
        };
        session.split_to_stems(id);
        assert_eq!(session.stems_consent(), None, "present: no need to ask");
        wait(&mut session);
        assert!(
            session
                .message()
                .unwrap()
                .starts_with("Couldn't split song"),
            "{:?}",
            session.message()
        );
    }

    #[test]
    fn a_new_clip_for_each_part_keeps_the_track_order_of_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, id, _) = rig(dir.path(), 0, 20_000);
        // Two more tracks, the source's in the middle.
        for _ in 0..2 {
            let (track, _) =
                add_track::with_input(session.project().graph(), || session.new_node_id());
            session.edit([Edit::Apply(track)]);
        }
        let names_before = names(&session);
        session.use_separator(fake);
        session.split_to_stems(id);
        wait(&mut session);
        let names_after = names(&session);
        assert_eq!(names_after.len(), names_before.len() + 5);
        assert_eq!(names_after[0], names_before[0]);
        assert_eq!(names_after[1], "song (drums)");
        assert_eq!(&names_after[6..], &names_before[1..]);
    }
}
