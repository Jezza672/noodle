//! Freezing and offline nodes: the glue between cache keys, the cache on
//! disk and the renders that fill it. See "Caching" in docs/ARCHITECTURE.md.
//!
//! The pieces:
//!
//! - [`Freezer::analyze`] keys the project and finds its *targets*: offline
//!   nodes, and nodes or groups marked frozen.
//! - [`Freezer::plan`] says which targets have a render in the cache and
//!   builds the [`Replacements`] that make the live engine play them. A
//!   frozen target without a render keeps playing live; an offline node
//!   without one plays silence.
//! - [`spawn_freeze`] renders the targets that are missing, upstream first,
//!   on a background thread with progress. Each target is rendered with the
//!   renders upstream of it already standing in, so freezing two things in a
//!   chain costs each only its own work.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use noodle_core::{CacheKey, Endpoint, NodeId, Project};
use noodle_engine::{
    Analysis, CacheEnv, Cancelled, Diagnostic, InputKind, InputOrigin, Job, OfflineError,
    OfflineInput, Problem, Progress, Registry, Replacements, Settings, StreamError, Tap, Target,
    TargetKind, TempoTable, Uncacheable, analyze, apply_offset, render_offline_node, render_taps,
};
use noodle_io::{CacheInfo, CacheStore, CacheWriter, FileHasher, Peek};

use crate::cached::CachedPlayer;
use crate::{RenderRequest, register_library_blocking};

/// How far along a target is.
#[derive(Clone, Debug, PartialEq)]
pub enum TargetState {
    /// The render is in the cache.
    Ready,
    /// No render yet. A frozen target plays live meanwhile, and an offline
    /// node plays silence.
    Missing,
    /// It can't be rendered, and why.
    Blocked(Uncacheable),
}

/// What the live engine needs to know about the cache.
pub struct FreezePlan {
    /// Outputs to play from the cache; give to
    /// [`Controller::update_project_replacing`](noodle_engine::Controller::update_project_replacing).
    pub replacements: Replacements,
    pub states: Vec<(TargetKind, TargetState)>,
    /// Problems with offline nodes that can't be cached, to show with the
    /// compile's own.
    pub diagnostics: Vec<Diagnostic>,
}

impl FreezePlan {
    pub fn state(&self, kind: TargetKind) -> Option<&TargetState> {
        self.states
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, state)| state)
    }
}

/// The cache on disk, and what it takes to key a project against it. Cheap
/// to clone; clones share the file-hash memory.
#[derive(Clone)]
pub struct Freezer {
    store: CacheStore,
    hasher: Arc<FileHasher>,
}

impl Freezer {
    pub fn new(store: CacheStore) -> Self {
        Self {
            store,
            hasher: Arc::new(FileHasher::new()),
        }
    }

    pub fn store(&self) -> &CacheStore {
        &self.store
    }

    /// Keys and targets for a render of `frames` frames from the start.
    /// Clip files are looked up relative to `base`.
    pub fn analyze(
        &self,
        project: &Project,
        registry: &Registry,
        settings: Settings,
        frames: usize,
        base: &Path,
    ) -> Analysis {
        let file_key = |source: &str| self.hasher.hash(&base.join(source)).ok();
        analyze(
            project,
            registry,
            &CacheEnv {
                project,
                sample_rate: settings.sample_rate,
                frames: frames as u64,
                file_key: &file_key,
            },
        )
    }

    /// [`analyze`](Self::analyze) for a thread that mustn't wait on the disk:
    /// it never reads a clip file. A file whose hash isn't known yet (new, or
    /// changed since) is left out of the keys, and comes back in the second
    /// part. Until [`hash_files`](Self::hash_files) has run on them, the
    /// analysis says nothing reliable about anything that plays those files:
    /// don't render from it.
    pub fn analyze_known(
        &self,
        project: &Project,
        registry: &Registry,
        settings: Settings,
        frames: usize,
        base: &Path,
    ) -> (Analysis, Vec<PathBuf>) {
        let unknown = RefCell::new(Vec::new());
        let file_key = |source: &str| {
            let path = base.join(source);
            match self.hasher.peek(&path) {
                Peek::Known(key) => Some(key),
                Peek::Unknown => {
                    let mut unknown = unknown.borrow_mut();
                    if !unknown.contains(&path) {
                        unknown.push(path);
                    }
                    None
                }
                Peek::Unreadable => None,
            }
        };
        let analysis = analyze(
            project,
            registry,
            &CacheEnv {
                project,
                sample_rate: settings.sample_rate,
                frames: frames as u64,
                file_key: &file_key,
            },
        );
        (analysis, unknown.into_inner())
    }

    /// Reads and hashes `files`, so [`analyze_known`](Self::analyze_known)
    /// knows them. Slow for big files: run it on a thread of its own.
    pub fn hash_files(&self, files: &[PathBuf], progress: &Progress) -> Result<(), Cancelled> {
        progress.report(0.0)?;
        for (i, path) in files.iter().enumerate() {
            // A file that can't be read is remembered as such.
            let _ = self.hasher.hash(path);
            progress.report((i + 1) as f32 / files.len() as f32)?;
        }
        Ok(())
    }

    /// [`hash_files`](Self::hash_files) on a background thread.
    pub fn spawn_hash_files(&self, files: Vec<PathBuf>) -> Job<Result<(), Cancelled>> {
        let freezer = self.clone();
        Job::spawn(move |progress| freezer.hash_files(&files, progress))
    }

    fn wanted(settings: Settings, frames: usize, lanes: usize) -> CacheInfo {
        CacheInfo {
            channels: lanes,
            sample_rate: settings.sample_rate.round() as u32,
            frames: frames as u64,
        }
    }

    /// Which targets are in the cache, and what plays in place of them.
    /// `blocking` makes the players read in the calling thread, for renders.
    pub fn plan(
        &self,
        analysis: &Analysis,
        settings: Settings,
        frames: usize,
        blocking: bool,
    ) -> FreezePlan {
        let mut plan = FreezePlan {
            replacements: Replacements::new(),
            states: Vec::new(),
            diagnostics: Vec::new(),
        };
        for target in &analysis.targets {
            let offline = matches!(target.kind, TargetKind::Offline(_));
            let replace =
                |plan: &mut FreezePlan, key: Option<CacheKey>, node: NodeId, output: usize| {
                    let info = &analysis.nodes[&node];
                    let shape = info.output_shapes[output];
                    let mut player = match key {
                        Some(key) => CachedPlayer::new(self.store.clone(), key, shape),
                        None => CachedPlayer::silent(self.store.clone(), shape),
                    };
                    if blocking {
                        player = player.blocking();
                    }
                    plan.replacements.insert(
                        (node, info.output_ports[output].clone()),
                        player.replacement(),
                    );
                };
            if let Some(why) = analysis.blocked(target) {
                if offline {
                    plan.diagnostics.push(Diagnostic {
                        location: noodle_engine::Location::Node(target.kind.node()),
                        problem: Problem::NotCacheable(why.to_string()),
                    });
                    for &(node, output) in &target.outputs {
                        replace(&mut plan, None, node, output);
                    }
                }
                plan.states.push((target.kind, TargetState::Blocked(why)));
                continue;
            }
            let keys: Vec<(NodeId, usize, CacheKey)> = target
                .outputs
                .iter()
                .map(|&(node, output)| {
                    let key = analysis
                        .output_key(node, output)
                        .expect("a target that isn't blocked has keys");
                    (node, output, key)
                })
                .collect();
            let ready = keys.iter().all(|&(node, output, key)| {
                let lanes = analysis.nodes[&node].output_shapes[output].lanes();
                self.store
                    .get(&key)
                    .is_some_and(|hit| hit.info() == Self::wanted(settings, frames, lanes))
            });
            if ready {
                for &(node, output, key) in &keys {
                    replace(&mut plan, Some(key), node, output);
                }
                plan.states.push((target.kind, TargetState::Ready));
            } else {
                if offline {
                    for &(node, output, _) in &keys {
                        replace(&mut plan, None, node, output);
                    }
                }
                plan.states.push((target.kind, TargetState::Missing));
            }
        }
        plan
    }
}

/// How a freeze ended.
#[derive(Debug, Default)]
pub struct FreezeReport {
    /// Targets rendered and stored.
    pub rendered: Vec<TargetKind>,
    /// Targets that were in the cache already.
    pub cached: Vec<TargetKind>,
    /// Targets that can't be cached, and why.
    pub blocked: Vec<(TargetKind, Uncacheable)>,
}

#[derive(Debug)]
pub enum FreezeError {
    Cancelled,
    Io(io::Error),
    /// A node refused to render, or the engine wouldn't start.
    Render(String),
    /// A clip's file couldn't be read, so the audio would be incomplete and
    /// must not stand in for the real thing.
    Incomplete(TargetKind),
}

impl std::fmt::Display for FreezeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("the freeze was cancelled"),
            Self::Io(error) => write!(f, "the cache couldn't be written: {error}"),
            Self::Render(message) => f.write_str(message),
            Self::Incomplete(kind) => write!(
                f,
                "node {} wasn't rendered, because an audio file it plays couldn't be read",
                kind.node()
            ),
        }
    }
}

impl std::error::Error for FreezeError {}

impl From<Cancelled> for FreezeError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

impl From<io::Error> for FreezeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl<E: Into<FreezeError>> From<StreamError<E>> for FreezeError {
    fn from(error: StreamError<E>) -> Self {
        match error {
            StreamError::Cancelled => Self::Cancelled,
            StreamError::Render(error) => Self::Render(error.to_string()),
            StreamError::Sink(error) => error.into(),
        }
    }
}

impl From<std::convert::Infallible> for FreezeError {
    fn from(never: std::convert::Infallible) -> Self {
        match never {}
    }
}

impl From<OfflineError> for FreezeError {
    fn from(error: OfflineError) -> Self {
        match error {
            OfflineError::Cancelled => Self::Cancelled,
            other => Self::Render(other.to_string()),
        }
    }
}

/// Renders every target the cache is missing, on a background thread, and
/// stores the results. Progress covers all of them. Recompile with
/// [`Freezer::plan`] when it finishes.
pub fn spawn_freeze(
    freezer: Freezer,
    request: RenderRequest,
) -> Job<Result<FreezeReport, FreezeError>> {
    Job::spawn(move |progress| freeze(&freezer, &request, progress))
}

/// [`spawn_freeze`] on the calling thread.
pub fn freeze(
    freezer: &Freezer,
    request: &RenderRequest,
    progress: &Progress,
) -> Result<FreezeReport, FreezeError> {
    freeze_within(freezer, request, progress, (0.0, 1.0))
}

/// [`freeze`] reporting only into the part of `progress` from `outer.0` to
/// `outer.0 + outer.1`, for a job that does more afterwards (an export).
pub(crate) fn freeze_within(
    freezer: &Freezer,
    request: &RenderRequest,
    progress: &Progress,
    outer: (f32, f32),
) -> Result<FreezeReport, FreezeError> {
    let settings = request.settings;
    let frames = request.frames;
    let mut registry = Registry::with_builtins();
    let library = register_library_blocking(&mut registry);
    if let Some(extend) = &request.extend_registry {
        extend(&mut registry);
    }
    let table = TempoTable::new(request.project.tempo_map(), settings.sample_rate);
    let rate = settings.sample_rate.round() as u32;
    library
        .clips
        .update(&request.project, &table, rate, &request.base);

    let analysis = freezer.analyze(&request.project, &registry, settings, frames, &request.base);
    let mut report = FreezeReport::default();
    let mut todo: Vec<&Target> = Vec::new();
    {
        let plan = freezer.plan(&analysis, settings, frames, true);
        for target in &analysis.targets {
            match plan.state(target.kind) {
                Some(TargetState::Ready) => report.cached.push(target.kind),
                Some(TargetState::Blocked(why)) => {
                    report.blocked.push((target.kind, why.clone()));
                }
                _ => todo.push(target),
            }
        }
    }
    let count = todo.len().max(1) as f32;
    progress.set_window(outer.0, outer.1);
    progress.report(0.0)?;
    for (i, target) in todo.into_iter().enumerate() {
        let window = (outer.0 + outer.1 * i as f32 / count, outer.1 / count);
        progress.set_window(window.0, window.1);
        // Rebuilt for each target: the one before it is in the cache now.
        let plan = freezer.plan(&analysis, settings, frames, true);
        match target.kind {
            TargetKind::Frozen(_) => render_frozen(
                freezer,
                request,
                &registry,
                &library,
                &analysis,
                &plan.replacements,
                target,
                progress,
            )?,
            TargetKind::Offline(node) => render_offline(
                freezer,
                request,
                &registry,
                &library,
                &analysis,
                &plan.replacements,
                node,
                target,
                progress,
                window,
            )?,
        }
        report.rendered.push(target.kind);
    }
    progress.set_window(outer.0, outer.1);
    progress.report(1.0)?;
    Ok(report)
}

type Library = crate::Library;

/// An entry being written. The tap's sink and the code that commits it both
/// hold it.
struct Entry {
    writer: Mutex<Option<CacheWriter>>,
    error: Mutex<Option<io::Error>>,
    lanes: usize,
    /// Frames written so far.
    frames: Mutex<usize>,
}

impl Entry {
    fn open(
        freezer: &Freezer,
        key: &CacheKey,
        lanes: usize,
        settings: Settings,
    ) -> io::Result<Arc<Self>> {
        let writer = freezer
            .store
            .writer(key, lanes, settings.sample_rate.round() as u32)?;
        Ok(Arc::new(Self {
            writer: Mutex::new(Some(writer)),
            error: Mutex::new(None),
            lanes,
            frames: Mutex::new(0),
        }))
    }

    fn write(&self, samples: &[f32]) {
        let mut writer = self.writer.lock().expect("entry lock");
        if let Some(w) = writer.as_mut() {
            match w.write(samples) {
                Ok(()) => *self.frames.lock().expect("entry lock") += samples.len() / self.lanes,
                Err(error) => {
                    *self.error.lock().expect("entry lock") = Some(error);
                    *writer = None;
                }
            }
        }
    }

    /// Frames written so far.
    fn frames(&self) -> usize {
        *self.frames.lock().expect("entry lock")
    }

    /// Stores the entry, unless writing failed along the way.
    fn commit(&self) -> io::Result<()> {
        if let Some(error) = self.error.lock().expect("entry lock").take() {
            return Err(error);
        }
        match self.writer.lock().expect("entry lock").take() {
            Some(writer) => writer.commit().map(|_| ()),
            None => Err(io::Error::other("the entry was already closed")),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_frozen(
    freezer: &Freezer,
    request: &RenderRequest,
    registry: &Registry,
    library: &Library,
    analysis: &Analysis,
    replacements: &Replacements,
    target: &Target,
    progress: &Progress,
) -> Result<(), FreezeError> {
    let settings = request.settings;
    let mut entries = Vec::new();
    let mut taps = Vec::new();
    for &(node, output) in &target.outputs {
        let info = &analysis.nodes[&node];
        let key = analysis
            .output_key(node, output)
            .expect("a target to render has keys");
        let lanes = info.output_shapes[output].lanes();
        let entry = Entry::open(freezer, &key, lanes, settings)?;
        let sink_entry = Arc::clone(&entry);
        let mut interleaved = Vec::new();
        taps.push(Tap {
            endpoint: Endpoint::new(node, info.output_ports[output].clone()),
            sink: Box::new(move |signal| {
                let shape = signal.shape();
                let frames = signal.frames();
                interleaved.resize(frames * lanes, 0.0);
                for lane in 0..lanes {
                    let samples = signal.lane(lane / shape.channels, lane % shape.channels);
                    for (i, sample) in samples.iter().enumerate() {
                        interleaved[i * lanes + lane] = *sample;
                    }
                }
                sink_entry.write(&interleaved);
            }),
        });
        entries.push(entry);
    }
    let diagnostics = render_taps::<io::Error>(
        &request.project,
        registry,
        settings,
        request.frames,
        progress,
        replacements,
        taps,
    )?;
    if !library.clips.errors().is_empty() || library.clips.underruns() > 0 {
        return Err(FreezeError::Incomplete(target.kind));
    }
    // A tap that never ran would store a short entry, which the app would
    // then render again forever.
    if entries.iter().any(|entry| entry.frames() != request.frames) {
        return Err(FreezeError::Render(match diagnostics.first() {
            Some(d) => format!(
                "node {} produced no audio: {}",
                target.kind.node(),
                d.problem
            ),
            None => format!("node {} produced no audio", target.kind.node()),
        }));
    }
    for entry in entries {
        entry.commit()?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_offline(
    freezer: &Freezer,
    request: &RenderRequest,
    registry: &Registry,
    library: &Library,
    analysis: &Analysis,
    replacements: &Replacements,
    node: NodeId,
    target: &Target,
    progress: &Progress,
    (start, span): (f32, f32),
) -> Result<(), FreezeError> {
    let settings = request.settings;
    let frames = request.frames;
    let info = &analysis.nodes[&node];
    let project_node = request
        .project
        .graph()
        .node(node)
        .ok_or_else(|| FreezeError::Render(format!("node {node} is gone")))?;
    let node_type = registry
        .get(&info.type_id)
        .ok_or_else(|| FreezeError::Render(format!("unknown node type `{}`", info.type_id)))?;

    let layout = node_type
        .layout(&project_node.config)
        .map_err(|e| FreezeError::Render(e.to_string()))?;
    // What feeds each input: rendered over the whole range, or a constant.
    // The renders up to here take the first part of the target's stretch and
    // the node itself the rest.
    type Lanes = Arc<Mutex<Vec<Vec<f32>>>>;
    let mut captured: Vec<Option<(noodle_engine::Shape, Lanes)>> = Vec::new();
    let mut taps = Vec::new();
    for origin in &info.inputs {
        match *origin {
            InputOrigin::Value(_) => captured.push(None),
            InputOrigin::Wire(source, port) | InputOrigin::Modulated(source, port, _) => {
                let source_info = &analysis.nodes[&source];
                let shape = source_info.output_shapes[port];
                let lanes: Lanes = Arc::new(Mutex::new(vec![Vec::new(); shape.lanes()]));
                let sink_lanes = Arc::clone(&lanes);
                taps.push(Tap {
                    endpoint: Endpoint::new(source, source_info.output_ports[port].clone()),
                    sink: Box::new(move |signal| {
                        let shape = signal.shape();
                        let mut lanes = sink_lanes.lock().expect("capture lock");
                        for (lane, data) in lanes.iter_mut().enumerate() {
                            data.extend_from_slice(
                                signal.lane(lane / shape.channels, lane % shape.channels),
                            );
                        }
                    }),
                });
                captured.push(Some((shape, lanes)));
            }
        }
    }
    // The renders that feed the node take most of the time.
    progress.set_window(start, span * 0.6);
    let mut diagnostics = Vec::new();
    if !taps.is_empty() {
        diagnostics = render_taps::<io::Error>(
            &request.project,
            registry,
            settings,
            frames,
            progress,
            replacements,
            taps,
        )?;
    }
    if !library.clips.errors().is_empty() || library.clips.underruns() > 0 {
        return Err(FreezeError::Incomplete(target.kind));
    }
    // A tap that never ran leaves a short capture, which the node would
    // index past.
    let short = captured.iter().flatten().any(|(_, lanes)| {
        lanes
            .lock()
            .expect("capture lock")
            .iter()
            .any(|lane| lane.len() != frames)
    });
    if short {
        return Err(FreezeError::Render(match diagnostics.first() {
            Some(d) => format!("node {node} got no audio on an input: {}", d.problem),
            None => format!("node {node} got no audio on an input"),
        }));
    }

    let inputs: Vec<OfflineInput> = info
        .inputs
        .iter()
        .zip(captured)
        .zip(&layout.inputs)
        .map(|((origin, captured), port)| match (origin, captured) {
            (InputOrigin::Value(v), _) => OfflineInput::Constant(*v),
            (_, Some((shape, lanes))) => {
                let mut data: Vec<f32> = std::mem::take(&mut *lanes.lock().expect("capture lock"))
                    .into_iter()
                    .flatten()
                    .collect();
                // A wire into an offsetting parameter moves its value, as it
                // does live.
                if let (InputOrigin::Modulated(_, _, base), InputKind::Param(param)) =
                    (origin, &port.kind)
                {
                    apply_offset(param, *base, &mut data);
                }
                OfflineInput::Signal { shape, data }
            }
            (_, None) => OfflineInput::Constant(0.0),
        })
        .collect();
    progress.set_window(start + span * 0.6, span * 0.3);
    let outputs = render_offline_node(
        node_type.as_ref(),
        &project_node.config,
        node,
        &inputs,
        frames,
        settings.sample_rate,
        progress,
    )?;
    progress.set_window(start + span * 0.9, span * 0.1);
    for &(_, output) in &target.outputs {
        let key = analysis
            .output_key(node, output)
            .expect("a target to render has keys");
        let planar = &outputs[output];
        let lanes = planar.shape.lanes();
        let entry = Entry::open(freezer, &key, lanes, settings)?;
        let mut chunk = Vec::new();
        let mut start = 0;
        while start < frames {
            let n = (frames - start).min(16_384);
            chunk.resize(n * lanes, 0.0);
            for lane in 0..lanes {
                let samples = &planar.data[lane * frames + start..lane * frames + start + n];
                for (i, sample) in samples.iter().enumerate() {
                    chunk[i * lanes + lane] = *sample;
                }
            }
            entry.write(&chunk);
            start += n;
            progress.report(start_f(start, frames))?;
        }
        entry.commit()?;
    }
    Ok(())
}

fn start_f(done: usize, total: usize) -> f32 {
    done as f32 / total.max(1) as f32
}

/// The folder cached renders go in, beside the project file's, when the user
/// hasn't chosen one.
pub fn default_cache_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".noodle-cache")
}
