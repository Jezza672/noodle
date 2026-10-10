//! Freezing in the app: keeps the cache of renders in step with the project.
//!
//! The session asks [`Freezing::refresh`] to look at the project whenever it
//! compiles. That works out which offline nodes and frozen nodes have a render
//! in the cache and returns what plays in their place. If any are missing it
//! starts one background render for all of them (see
//! [`noodle_nodes::spawn_freeze`]) and shows its progress on the nodes. When
//! the render finishes the session compiles again, and the cached audio takes
//! over. A render for a project that has since changed is cancelled, and a
//! render that failed isn't tried again until the project changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use noodle_core::{NodeId, Project};
use noodle_engine::{Job, JobPanicked, Mode, Registry, Settings, TargetKind};
use noodle_nodes::{
    FreezeError, FreezePlan, FreezeReport, Freezer, RenderRequest, TargetState, spawn_freeze,
};

/// How long after the last edit that doesn't recompile (a parameter, say) the
/// renders are looked at again, so dragging a knob doesn't start one per frame.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// The cache is trimmed to this on start-up.
const CACHE_LIMIT: u64 = 4 << 30;

/// What the editor shows on a node that is frozen or offline.
#[derive(Clone, Debug, PartialEq)]
pub struct Badge {
    /// Marked frozen by the user, as opposed to an offline node.
    pub frozen: bool,
    pub state: BadgeState,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BadgeState {
    /// The render is in the cache and plays in place of the node.
    Ready,
    /// Being rendered, from 0 to 1.
    Rendering(f32),
    /// No render yet and none under way.
    Waiting,
    /// It can't be rendered, or the render failed, and why.
    Failed(String),
}

struct Running {
    job: Job<Result<FreezeReport, FreezeError>>,
    /// The project it renders.
    project: Project,
}

/// What [`Freezing::poll`] saw.
#[derive(Debug, PartialEq)]
pub enum Polled {
    Nothing,
    /// A render finished: compile again to pick it up.
    Finished,
    /// A render failed, with why.
    Failed(String),
}

pub struct Freezing {
    cache_dir: Option<PathBuf>,
    freezer: Option<Freezer>,
    running: Option<Running>,
    badges: BTreeMap<NodeId, Badge>,
    /// The project a render failed for, and why. It isn't retried until the
    /// project changes.
    failed: Option<(Project, String)>,
    dirty_since: Option<Instant>,
}

/// What [`Freezing::refresh`] needs to know.
pub struct Look<'a> {
    pub project: &'a Project,
    pub registry: &'a Registry,
    pub settings: Settings,
    pub base: &'a Path,
    /// How long a render is, in frames.
    pub frames: usize,
}

impl Freezing {
    pub fn new() -> Self {
        Self {
            cache_dir: None,
            freezer: None,
            running: None,
            badges: BTreeMap::new(),
            failed: None,
            dirty_since: None,
        }
    }

    /// Keeps renders in `dir` instead of the user's cache folder.
    #[cfg(test)]
    pub fn use_cache_dir(&mut self, dir: PathBuf) {
        self.cache_dir = Some(dir);
        self.freezer = None;
    }

    pub fn badges(&self) -> &BTreeMap<NodeId, Badge> {
        &self.badges
    }

    /// How far the render under way has got, if one is running.
    pub fn progress(&self) -> Option<f32> {
        self.running.as_ref().map(|r| r.job.fraction())
    }

    /// A parameter or clip changed without a recompile: look again soon.
    pub fn touch(&mut self) {
        self.dirty_since = Some(Instant::now());
    }

    /// Whether an edit is waiting for the debounce to pass.
    pub fn due(&mut self) -> bool {
        match self.dirty_since {
            Some(since) if since.elapsed() >= DEBOUNCE => {
                self.dirty_since = None;
                true
            }
            _ => false,
        }
    }

    pub fn is_waiting(&self) -> bool {
        self.dirty_since.is_some()
    }

    /// Whether the project has anything to freeze or render.
    pub fn in_use(project: &Project, registry: &Registry) -> bool {
        project
            .frozen()
            .iter()
            .any(|&n| project.graph().node(n).is_some())
            || project.graph().nodes().any(|(_, node)| {
                registry
                    .get(&node.type_id)
                    .and_then(|t| t.layout(&node.config).ok())
                    .is_some_and(|layout| layout.mode == Mode::Offline)
            })
    }

    fn freezer(&mut self) -> Result<&Freezer, String> {
        if self.freezer.is_none() {
            let dir = self.cache_dir.clone().unwrap_or_else(default_cache_dir);
            let store = noodle_io::CacheStore::open(&dir)
                .map_err(|e| format!("Can't use the render cache at {}: {e}", dir.display()))?;
            // Best effort: a cache that can't be trimmed still works.
            let _ = store.evict_to(CACHE_LIMIT);
            self.freezer = Some(Freezer::new(store));
        }
        Ok(self.freezer.as_ref().expect("just made"))
    }

    /// Works out which renders exist, starts one for the rest, and returns
    /// what should play in place of the cached nodes. `None` if the project
    /// uses no freezing, in which case nothing runs.
    pub fn refresh(&mut self, look: &Look<'_>) -> Result<Option<FreezePlan>, String> {
        self.dirty_since = None;
        if !Self::in_use(look.project, look.registry) {
            self.running = None;
            self.badges.clear();
            self.failed = None;
            return Ok(None);
        }
        let freezer = self.freezer()?.clone();
        let analysis = freezer.analyze(
            look.project,
            look.registry,
            look.settings,
            look.frames,
            look.base,
        );
        let plan = freezer.plan(&analysis, look.settings, look.frames, false);
        let missing = plan
            .states
            .iter()
            .any(|(_, state)| *state == TargetState::Missing);

        // A render of an older project is stale.
        if self
            .running
            .as_ref()
            .is_some_and(|r| r.project != *look.project)
        {
            self.running = None;
        }
        if self
            .failed
            .as_ref()
            .is_some_and(|(project, _)| project != look.project)
        {
            self.failed = None;
        }
        if missing && self.running.is_none() && self.failed.is_none() {
            let request = RenderRequest {
                project: look.project.clone(),
                base: look.base.to_owned(),
                settings: look.settings,
                frames: look.frames,
                extend_registry: None,
            };
            self.running = Some(Running {
                job: spawn_freeze(freezer, request),
                project: look.project.clone(),
            });
        }

        let fraction = self.progress();
        let failure = self.failed.as_ref().map(|(_, why)| why.clone());
        self.badges = plan
            .states
            .iter()
            .map(|(kind, state)| {
                let state = match state {
                    TargetState::Ready => BadgeState::Ready,
                    TargetState::Blocked(why) => BadgeState::Failed(why.to_string()),
                    TargetState::Missing => match (&failure, fraction) {
                        (Some(why), _) => BadgeState::Failed(why.clone()),
                        (None, Some(fraction)) => BadgeState::Rendering(fraction),
                        (None, None) => BadgeState::Waiting,
                    },
                };
                let badge = Badge {
                    frozen: matches!(kind, TargetKind::Frozen(_)),
                    state,
                };
                (kind.node(), badge)
            })
            .collect();
        Ok(Some(plan))
    }

    /// Looks at the render under way. Call it every frame.
    pub fn poll(&mut self) -> Polled {
        let Some(running) = &mut self.running else {
            return Polled::Nothing;
        };
        // Keep the bars moving.
        let fraction = running.job.fraction();
        for badge in self.badges.values_mut() {
            if let BadgeState::Rendering(shown) = &mut badge.state {
                *shown = fraction;
            }
        }
        let Some(done) = running.job.poll() else {
            return Polled::Nothing;
        };
        let project = self.running.take().expect("just polled").project;
        match done {
            Ok(Ok(_)) | Ok(Err(FreezeError::Cancelled)) => Polled::Finished,
            Ok(Err(error)) => {
                let why = error.to_string();
                self.failed = Some((project, why.clone()));
                Polled::Failed(why)
            }
            Err(JobPanicked) => {
                let why = JobPanicked.to_string();
                self.failed = Some((project, why.clone()));
                Polled::Failed(why)
            }
        }
    }
}

impl Default for Freezing {
    fn default() -> Self {
        Self::new()
    }
}

/// Where renders are kept unless told otherwise: the user's cache folder,
/// shared by every project since entries are named by what they contain.
fn default_cache_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "Noodle").map_or_else(
        || std::env::temp_dir().join("noodle-renders"),
        |dirs| dirs.cache_dir().join("renders"),
    )
}
