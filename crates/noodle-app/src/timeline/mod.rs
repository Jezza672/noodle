//! The arrangement view: a lane per track, with the clips each track plays,
//! on a ruler of bars and beats.
//!
//! The view works in real time. Its x axis is linear in seconds, so a clip is
//! as wide as its audio is long, and a bar is wider where the tempo is slower
//! (its bar lines sit further apart). Everything that converts between ticks
//! and screen x goes through the [`Axis`], which follows the tempo map.
//! Every change is an [`Edit`] using the core's clip commands, so it can be
//! undone.

pub(crate) mod add_track;
mod automation;
pub(crate) mod clips;
pub(crate) mod comp;
mod crossfade;
pub(crate) mod grid;
pub(crate) mod header;
pub mod import;
mod midi_preview;
mod sections;
mod sources;
mod take;
mod takes;
mod tempo_lane;
mod warped;
mod warped_waves;
mod waveform;

#[cfg(test)]
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};

use egui::{
    Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, vec2,
};
use noodle_core::{
    Clip, ClipId, ClipWarp, Command, Endpoint, FolderId, LaneId, NodeId, Project, TempoMap, Tick,
    WarpId,
};

use crate::session::{ArmKind, Edit, Session};
use crate::theme::timeline as colors;
use clips::Source;
use sources::Sources;
pub use sources::{Analysis, Loaded};

/// The type of the node inside a track's group that plays its clips.
pub const TRACK_INPUT: &str = "noodle.track.input";

/// How far from a clip's edge a drag trims it instead of moving it.
const EDGE: f32 = 6.0;
/// Clips narrower than this can only be moved.
const MIN_TRIMMABLE: f32 = 24.0;
/// The warp of an audio clip, if it has one.
/// The command that mutes audio clip `id` (shown as `clip`), or unmutes it if it
/// is muted.
pub(crate) fn toggle_mute(id: ClipId, clip: &Clip) -> Command {
    let mut changed = clip.clone();
    if let noodle_core::ClipContent::Audio(audio) = &mut changed.content {
        audio.muted = !audio.muted;
    }
    Command::SetClip { id, clip: changed }
}

/// The sample rate of every audio file the project's clips play, for laying
/// the clips out in ticks.
fn source_rates(
    project: &Project,
    sources: &mut Sources,
    ctx: &egui::Context,
    directory: Option<&std::path::Path>,
) -> std::collections::HashMap<String, u32> {
    project
        .clips()
        .filter_map(|(_, clip)| Some(clip.as_audio()?.source.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|source| {
            let rate = sources
                .get(ctx, directory, &source)
                .map_or(FALLBACK_RATE, |loaded| loaded.source.sample_rate);
            (source, rate)
        })
        .collect()
}

fn warp_of<'a>(project: &'a Project, clip: &Clip) -> Option<&'a noodle_core::ClipWarp> {
    project.warp(clip.as_audio()?.warp?)
}

/// The sample rate to lay a clip out at when its file can't be read.
const FALLBACK_RATE: u32 = 48_000;
/// A new MIDI clip is a bar of 4/4 long.
const NEW_MIDI_CLIP: i64 = 3840;
const MIN_PPS: f32 = 16.0;
const MAX_PPS: f32 = 1200.0;

pub struct TimelineState {
    /// The track list for the project revision it was worked out at.
    tracks_cache: Option<(u64, std::rc::Rc<[NodeId]>)>,
    /// Zoom: screen points per second.
    pps: f32,
    /// How far the view is scrolled, in screen points.
    scroll_x: f32,
    scroll_y: f32,
    /// Vertical zoom: a factor on every track's lane height (Alt+scroll).
    v_zoom: f32,
    /// The lane heights tracks were dragged to, before vertical zoom. A track
    /// not here has the default.
    lane_heights: std::collections::BTreeMap<NodeId, f32>,
    selected: BTreeSet<ClipId>,
    /// The track (its input node) whose header was last clicked.
    selected_track: Option<NodeId>,
    drag: Option<Drag>,
    /// A box being dragged over empty space to select the clips it touches.
    band: Option<Band>,
    /// The clips last copied or cut, as they were.
    /// An edit-menu command for the next frame to carry out.
    pending: Option<ClipAction>,
    /// Where the arrangement was last right-clicked.
    menu_at: Option<Pos2>,
    /// The tick the ruler last asked for, while the button is held.
    last_seek: Option<Tick>,
    sources: Sources,
    /// A track name being typed.
    renaming: Option<header::Rename>,
    /// A folder name being typed.
    renaming_folder: Option<(FolderId, String)>,
    waveforms: waveform::Cache,
    /// Sections' waveforms as they play, once warped.
    warped_waves: warped_waves::WarpedWaves,
    automation: automation::State,
    crossfade: crossfade::State,
    takes: takes::State,
    /// The ruler last drawn, with the screen x of second 0 and points per second.
    #[cfg(test)]
    ruler_seen: Option<(Rect, f32, f32)>,
    /// The bar a right-click on the ruler opened its menu at.
    ruler_menu_bar: Option<u32>,
    /// The span selected across all tracks (a section between two markers, or
    /// one picked on the ruler), which copy, cut, paste, insert time and delete
    /// time work on. Clips, automation points and a range are never selected
    /// together.
    range: Option<sections::Range>,
    /// The marker a right-click on the ruler opened its menu at, and the name
    /// being typed for it.
    marker_menu: Option<usize>,
    marker_name: String,
    /// Whether the tempo lane is shown under the ruler.
    show_tempo: bool,
    tempo: tempo_lane::State,
    #[cfg(test)]
    clip_rects: HashMap<ClipId, Rect>,
    #[cfg(test)]
    take_rects: HashMap<NodeId, Rect>,
    #[cfg(test)]
    take_lane_rects: HashMap<(ClipId, usize), Rect>,
    #[cfg(test)]
    join_rects: HashMap<(ClipId, usize), Rect>,
    #[cfg(test)]
    fade_handles: HashMap<(ClipId, bool), Rect>,
    /// The text drawn on the lanes last frame; painted text isn't in the
    /// accessibility tree.
    #[cfg(test)]
    drawn_text: Vec<String>,
    /// How many clips drew a waveform last frame.
    #[cfg(test)]
    waveforms_drawn: usize,
}

impl Default for TimelineState {
    fn default() -> Self {
        Self {
            tracks_cache: None,
            pps: 120.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            v_zoom: 1.0,
            lane_heights: Default::default(),
            selected: BTreeSet::new(),
            selected_track: None,
            drag: None,
            band: None,
            pending: None,
            menu_at: None,
            last_seek: None,
            sources: Sources::default(),
            renaming: None,
            renaming_folder: None,
            waveforms: waveform::Cache::default(),
            warped_waves: warped_waves::WarpedWaves::default(),
            automation: automation::State::default(),
            crossfade: crossfade::State::default(),
            takes: takes::State::default(),
            show_tempo: false,
            #[cfg(test)]
            ruler_seen: None,
            ruler_menu_bar: None,
            range: None,
            marker_menu: None,
            marker_name: String::new(),
            tempo: tempo_lane::State::default(),
            #[cfg(test)]
            clip_rects: HashMap::new(),
            #[cfg(test)]
            take_rects: HashMap::new(),
            #[cfg(test)]
            take_lane_rects: HashMap::new(),
            #[cfg(test)]
            join_rects: HashMap::new(),
            #[cfg(test)]
            fade_handles: HashMap::new(),
            #[cfg(test)]
            drawn_text: Vec::new(),
            #[cfg(test)]
            waveforms_drawn: 0,
        }
    }
}

impl TimelineState {
    /// [`tracks`], worked out again only when the project changed.
    fn tracks(&mut self, project: &Project) -> std::rc::Rc<[NodeId]> {
        match &self.tracks_cache {
            Some((revision, tracks)) if *revision == project.revision() => tracks.clone(),
            _ => {
                let tracks: std::rc::Rc<[NodeId]> = visible_tracks(project).into();
                self.tracks_cache = Some((project.revision(), tracks.clone()));
                tracks
            }
        }
    }

    /// How far the tracks are scrolled down, for tests.
    #[cfg(test)]
    pub fn scroll_y(&self) -> f32 {
        self.scroll_y
    }

    /// How tall `track`'s own lane is, with vertical zoom.
    fn lane_height(&self, track: NodeId) -> f32 {
        let own = self
            .lane_heights
            .get(&track)
            .copied()
            .unwrap_or(colors::LANE_HEIGHT);
        own * self.v_zoom
    }

    /// Drags `track`'s lane taller by `dy` screen points.
    fn resize_track(&mut self, track: NodeId, dy: f32) {
        let own = self.lane_height(track) + dy;
        self.lane_heights
            .insert(track, (own / self.v_zoom).clamp(MIN_LANE, MAX_LANE));
    }

    /// Whether `track`'s lane is down to just its name.
    fn name_only(&self, track: NodeId) -> bool {
        self.lane_heights
            .get(&track)
            .is_some_and(|&h| h <= MIN_LANE)
    }

    /// Shrinks `track`'s lane to its name, or gives it its usual height back.
    fn toggle_name_only(&mut self, track: NodeId) {
        if self.name_only(track) {
            self.lane_heights.remove(&track);
        } else {
            self.lane_heights.insert(track, MIN_LANE);
        }
    }

    /// Forgets clips that no longer exist, e.g. after an undo.
    fn retain_existing(&mut self, project: &Project) {
        self.selected.retain(|&id| project.clip(id).is_some());
        if self
            .selected_track
            .is_some_and(|input| project.graph().node(input).is_none())
        {
            self.selected_track = None;
        }
        self.waveforms.retain(|id| project.clip(id).is_some());
        self.automation.retain_existing(project);
        self.tempo.retain_existing(project.tempo_map());
        if self
            .drag
            .as_ref()
            .is_some_and(|drag| project.clip(drag.grabbed).is_none())
        {
            self.drag = None;
        }
    }

    /// Where "Import audio" puts its clips from the File menu or the button,
    /// at the playhead: on the track that is highlighted (its header picked),
    /// else the selected clip's track, else the first.
    pub fn import_target(&self, project: &Project, playhead: Tick) -> Option<Target> {
        let track = self
            .selected_track
            .filter(|&input| tracks(project).contains(&input))
            .or_else(|| {
                self.selected
                    .iter()
                    .next()
                    .and_then(|&id| project.clip(id))
                    .map(|clip| clip.node)
            })
            .or_else(|| tracks(project).first().copied())?;
        Some(Target {
            track,
            at: playhead.max(Tick::ZERO),
        })
    }

    /// The clip that is selected, if exactly one is.
    pub fn selected_clip(&self) -> Option<ClipId> {
        let mut selected = self.selected.iter();
        match (selected.next(), selected.next()) {
            (Some(&id), None) => Some(id),
            _ => None,
        }
    }

    /// What is known about the audio file `name` (relative to `directory`),
    /// reading it in the background if this is the first time it's asked for.
    pub fn source(
        &mut self,
        ctx: &egui::Context,
        directory: Option<&std::path::Path>,
        name: &str,
    ) -> Option<Loaded> {
        self.sources.get(ctx, directory, name)
    }

    #[cfg(test)]
    pub fn select_only(&mut self, id: ClipId) {
        self.selected = BTreeSet::from([id]);
    }

    #[cfg(test)]
    pub fn selected(&self) -> &BTreeSet<ClipId> {
        &self.selected
    }

    /// Whether the tempo lane is shown under the ruler.
    pub fn tempo_lane(&self) -> bool {
        self.show_tempo
    }

    pub fn set_tempo_lane(&mut self, on: bool) {
        self.show_tempo = on;
    }

    #[cfg(test)]
    pub fn tempo(&self) -> &tempo_lane::State {
        &self.tempo
    }

    #[cfg(test)]
    pub fn automation(&self) -> &automation::State {
        &self.automation
    }

    #[cfg(test)]
    pub fn waveforms(&self) -> usize {
        self.waveforms_drawn
    }

    /// How many warped sections' renders have come back.
    #[cfg(test)]
    pub fn warped_renders(&self) -> usize {
        self.warped_waves.rendered()
    }

    #[cfg(test)]
    pub fn columns_computed(&self) -> usize {
        self.waveforms.computed()
    }

    #[cfg(test)]
    pub fn drawn_text(&self) -> &[String] {
        &self.drawn_text
    }

    #[cfg(test)]
    pub fn fade_handle(&self, id: ClipId, fade_in: bool) -> Option<Rect> {
        self.fade_handles.get(&(id, fade_in)).copied()
    }

    /// Zooms to `pps` screen points per second, as if the user had zoomed.
    #[cfg(test)]
    pub fn set_zoom(&mut self, pps: f32) {
        self.pps = pps.clamp(MIN_PPS, MAX_PPS);
    }

    /// Where the ruler is at `seconds` into the song, if it is on screen.
    #[cfg(test)]
    pub fn ruler_at(&self, seconds: f64) -> Option<Pos2> {
        let (rect, origin, pps) = self.ruler_seen?;
        let x = origin + (seconds * f64::from(pps)) as f32;
        (x > rect.left() && x < rect.right()).then(|| Pos2::new(x, rect.center().y - 3.0))
    }

    #[cfg(test)]
    pub fn clip_rect(&self, id: ClipId) -> Option<Rect> {
        self.clip_rects.get(&id).copied()
    }

    /// Where take `k` of the take folder `id` was drawn last frame.
    #[cfg(test)]
    pub fn take_lane(&self, id: ClipId, k: usize) -> Option<Rect> {
        self.take_lane_rects.get(&(id, k)).copied()
    }

    /// Where a crossfade's handle or edge was drawn last frame.
    #[cfg(test)]
    fn crossfade_widget(&self, later: ClipId, grab: crossfade::Grab) -> Option<Rect> {
        self.crossfade.widgets.get(&(later, grab)).copied()
    }

    /// Where the take being recorded was drawn on a track, if it was.
    #[cfg(test)]
    pub fn take_rect(&self, track: NodeId) -> Option<Rect> {
        self.take_rects.get(&track).copied()
    }
}

/// What the Edit menu, the keys and the clips' menus can do with clips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipAction {
    Copy,
    Cut,
    /// Puts the clipboard's clips at the playhead.
    Paste,
    /// Copies the selected clips to just after themselves.
    Duplicate,
    /// Opens empty time the length of the selected range at its start.
    InsertTime,
    /// Takes the selected range out and closes the gap.
    DeleteTime,
}

impl TimelineState {
    /// Asks for `action` to be done to the selection on the next frame.
    pub fn request(&mut self, action: ClipAction) {
        self.pending = Some(action);
    }

    /// Whether there are selected clips to copy, cut or duplicate.
    pub fn can_copy(&self) -> bool {
        !self.selected.is_empty() || self.automation.selected().is_some() || self.range.is_some()
    }

    /// Whether a range is selected, for insert time and delete time.
    pub fn has_range(&self) -> bool {
        self.range.is_some()
    }

    /// Whether there is anything to paste: clips, or a point where one is
    /// selected to paste onto its lane.
    pub fn can_paste(&self) -> bool {
        crate::clipboard::holds(crate::clipboard::CLIP_MIME)
            || crate::clipboard::holds(crate::clipboard::SECTION_MIME)
            || (self.automation.selected().is_some()
                && crate::clipboard::holds(crate::clipboard::POINT_MIME))
    }
}

/// A selection box, in the arrangement's own coordinates (points from the
/// top left of the lanes, scrolled with them) so it stays put as the view moves.
struct Band {
    from: Vec2,
    to: Vec2,
    /// What was selected before it, kept when Shift was held.
    base: BTreeSet<ClipId>,
}

/// A clip being moved or trimmed. Each frame's edit is worked out from the
/// clips as they were when the drag began, so snapping can't accumulate error.
struct Drag {
    mode: Mode,
    grabbed: ClipId,
    /// Every clip the drag moves, as it was at the start.
    originals: Vec<(ClipId, Clip)>,
    press_x: f32,
    /// Whether `originals` are copies the drag made, leaving the clips it
    /// started with where they were.
    copied: bool,
    /// What each audio clip in `originals` knows about its file, for the
    /// trims and slips that work out lengths.
    sources: BTreeMap<ClipId, Source>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Move,
    TrimStart,
    TrimEnd,
    /// Sliding an audio clip's contents under its window (Shift+Alt).
    Slip,
    /// Dragging a fade handle; worked out where the handle is drawn.
    FadeIn,
    FadeOut,
}

/// The track lanes, top to bottom: every track input node, and any other node
/// that has clips on the timeline (a Pattern Player's patterns are not).
pub(crate) fn tracks(project: &Project) -> Vec<NodeId> {
    let with_clips: BTreeSet<NodeId> = project
        .timeline_clips()
        .map(|(_, clip)| clip.node)
        .collect();
    let mut tracks: Vec<NodeId> = project
        .graph()
        .nodes()
        .filter(|(id, node)| node.type_id == TRACK_INPUT || with_clips.contains(id))
        .map(|(id, _)| id)
        .collect();
    // Track order is the order of the groups the inputs sit in; an input
    // with no group (or clips on another node) sorts by its own ID.
    tracks.sort_by_key(|&id| {
        let group = group_of(project, id);
        let rank = project.track_order().iter().position(|&g| g == group);
        (rank.unwrap_or(usize::MAX), id)
    });
    // A folder's tracks sit together, where its first one is.
    let org = project.organisation();
    if org.folders.is_empty() {
        return tracks;
    }
    let mut together = Vec::with_capacity(tracks.len());
    let mut placed = BTreeSet::new();
    for &track in &tracks {
        match org.folder_of(group_of(project, track)) {
            Some(folder) if org.folders.contains_key(&folder) => {
                if placed.insert(folder) {
                    together.extend(
                        tracks
                            .iter()
                            .copied()
                            .filter(|&t| org.folder_of(group_of(project, t)) == Some(folder)),
                    );
                }
            }
            _ => together.push(track),
        }
    }
    together
}

/// The group a track is ordered, coloured and foldered by: the one its input
/// sits in, or the node itself if it has none.
pub(crate) fn group_of(project: &Project, track: NodeId) -> NodeId {
    project
        .graph()
        .node(track)
        .and_then(|n| n.parent)
        .unwrap_or(track)
}

/// The folder a track is in, if it has one.
pub(crate) fn folder_of(project: &Project, track: NodeId) -> Option<FolderId> {
    let org = project.organisation();
    org.folder_of(group_of(project, track))
        .filter(|f| org.folders.contains_key(f))
}

/// [`tracks`] without the ones in folded folders: the lanes on screen.
pub(crate) fn visible_tracks(project: &Project) -> Vec<NodeId> {
    let org = project.organisation();
    let mut tracks = tracks(project);
    tracks.retain(|&t| folder_of(project, t).is_none_or(|f| !org.folders[&f].folded));
    tracks
}

/// A folder's header strip in the track list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Strip {
    pub folder: FolderId,
    /// How many of the visible tracks come before it.
    pub before: usize,
}

/// The folder strips for the visible lanes, top to bottom: one for each
/// folder, above its first visible track (or, folded, where it would have
/// been).
pub(crate) fn strips(project: &Project, all: &[NodeId], visible: &[NodeId]) -> Vec<Strip> {
    let mut strips: Vec<Strip> = Vec::new();
    let mut seen = 0;
    for &track in all {
        if let Some(folder) = folder_of(project, track)
            && !strips.iter().any(|s| s.folder == folder)
        {
            strips.push(Strip {
                folder,
                before: seen,
            });
        }
        if visible.contains(&track) {
            seen += 1;
        }
    }
    strips
}

/// How many strips come right before each visible track, and one more entry
/// for after the last, as [`automation::Rows::new`] takes them.
pub(crate) fn strip_counts(strips: &[Strip], tracks: usize) -> Vec<usize> {
    let mut counts = vec![0; tracks + 1];
    for strip in strips {
        counts[strip.before] += 1;
    }
    counts
}

/// The colour each of `shown` is drawn in: the palette slot it was given, or
/// its folder's, or else one by when it was made, so reordering never
/// recolours a track.
pub(crate) fn track_colours(project: &Project, shown: &[NodeId]) -> Vec<Color32> {
    let mut made: Vec<NodeId> = tracks(project);
    made.sort();
    let org = project.organisation();
    shown
        .iter()
        .map(|&track| {
            let slot = org
                .shown_colour(group_of(project, track))
                .map(usize::from)
                .or_else(|| made.iter().position(|&t| t == track))
                .unwrap_or(0);
            colors::track_colour(slot)
        })
        .collect()
}

/// One track's colour (see [`track_colours`]).
pub(crate) fn colour_of(project: &Project, track: NodeId) -> Color32 {
    track_colours(project, &[track])[0]
}

/// Where the screen's x axis is in time.
#[derive(Clone, Copy)]
struct Axis<'a> {
    /// The screen x of tick 0.
    origin: f32,
    /// Screen points per second.
    pps: f32,
    map: &'a TempoMap,
    /// The snap step in ticks (see [`crate::snap`]); 1 or less is off.
    snap: i64,
}

impl Axis<'_> {
    fn x(self, tick: Tick) -> f32 {
        self.origin + (self.map.seconds_at(tick) * f64::from(self.pps)) as f32
    }

    /// `tick` on the grid the snap setting asks for.
    fn snap(self, tick: Tick) -> Tick {
        grid::snap_to(self.map, tick, self.snap)
    }

    fn tick(self, x: f32) -> Tick {
        let seconds = f64::from(x - self.origin) / f64::from(self.pps);
        Tick(self.map.tick_at_seconds(seconds).round() as i64)
    }
}

/// What the user did in the arrangement.
#[derive(Default)]
pub struct Output {
    pub edits: Vec<Edit>,
    /// Where they asked the playhead to go, by clicking the ruler.
    pub seek: Option<Tick>,
    /// Something to tell the user, such as files that couldn't be imported.
    pub notice: Option<String>,
    /// They asked to import audio from a file: where it should go.
    pub pick: Option<Target>,
    /// They changed the transport's loop.
    pub set_loop: Option<crate::session::LoopRegion>,
    /// Tracks whose record-arm button was pressed, with the state asked for.
    pub arm: Vec<(NodeId, crate::session::ArmKind, bool)>,
    /// A MIDI clip they double-clicked, to open in the piano roll.
    pub open_midi: Option<ClipId>,
    /// An audio clip they double-clicked, to open in the clip editor.
    pub open_audio: Option<ClipId>,
    /// They clicked a take to audition (or the same one to stop): the take
    /// folder and take.
    pub audition: Option<Option<(ClipId, usize)>>,
}

/// Where imported audio goes when it isn't dropped on a lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub track: NodeId,
    pub at: Tick,
}

/// Draws the arrangement and returns what the user did. The ruler moves the
/// playhead, to the beat nearest the pointer unless Alt is held.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut TimelineState,
    session: &Session,
    playhead: Option<Tick>,
) -> Output {
    let project = session.project();
    state.retain_existing(project);
    let tracks = state.tracks(project);
    let heights: Vec<f32> = tracks.iter().map(|&t| state.lane_height(t)).collect();
    state.takes.retain(project);
    let take_lanes = state.takes.lanes(project, &tracks);
    let all_tracks = self::tracks(project);
    let strips = strips(project, &all_tracks, &tracks);
    let rows = automation::Rows::new(
        project,
        &tracks,
        &heights,
        &take_lanes,
        &strip_counts(&strips, tracks.len()),
    );
    let colours = track_colours(project, &tracks);
    let map = project.tempo_map();

    let (rect, background) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    // The tempo lane, when it is shown, sits between the ruler and the tracks.
    let lane_height = if state.show_tempo {
        tempo_lane::HEIGHT
    } else {
        0.0
    };
    let content = Rect::from_min_max(
        rect.min + vec2(colors::HEADER_WIDTH, colors::RULER_HEIGHT + lane_height),
        rect.max,
    );
    let hovered = ui.rect_contains_pointer(rect);
    let directory = session.directory().map(std::path::Path::to_path_buf);
    if hovered {
        // Room to scroll to: the end of the last clip, and some more to
        // place clips in.
        let extent = project
            .timeline_clips()
            .map(|(_, clip)| {
                let rate = clip.as_audio().map_or(FALLBACK_RATE, |audio| {
                    state
                        .sources
                        .get(ui.ctx(), directory.as_deref(), &audio.source)
                        .map_or(FALLBACK_RATE, |loaded| loaded.source.sample_rate)
                });
                map.seconds_at(clips::clip_end(map, clip, rate, warp_of(project, clip)))
            })
            .fold(0.0, f64::max) as f32
            + 8.0;
        navigate(ui, state, content, extent);
    }
    let axis = Axis {
        origin: content.left() - state.scroll_x,
        pps: state.pps,
        map,
        snap: session.snap(),
    };
    let lines = grid::lines(map, axis.tick(content.left()), axis.tick(content.right()));

    let mut edits = Vec::new();
    let mut open_midi = None;
    let mut open_audio = None;
    let mut audition = None;
    let mut notice: Option<String> = None;
    let mut arm = Vec::new();
    let mut jump = None;
    #[cfg(test)]
    {
        state.clip_rects.clear();
        state.take_rects.clear();
        state.take_lane_rects.clear();
        state.join_rects.clear();
        state.fade_handles.clear();
        state.drawn_text.clear();
        state.waveforms_drawn = 0;
    }

    // The lanes and headers scroll together in a vertical scroll area, below
    // the ruler and tempo lane, which are drawn afterwards, outside it. The
    // scroll area clips widgets scrolled out of view instead of letting them
    // grow the panel, and its virtual height (set inside) is the scroll range.
    let body = Rect::from_min_max(
        rect.min + vec2(0.0, colors::RULER_HEIGHT + lane_height),
        rect.max,
    );
    let mut body_ui = ui.new_child(egui::UiBuilder::new().max_rect(body));
    let scroll = egui::ScrollArea::vertical()
        .id_salt("arrangement body")
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .scroll_source(egui::containers::scroll_area::ScrollSource {
            drag: egui::containers::scroll_area::DragScroll::Never,
            ..Default::default()
        })
        .vertical_scroll_offset(state.scroll_y)
        .show_viewport(&mut body_ui, |ui, viewport| {
            let scroll_y = viewport.min.y;
            ui.set_min_height(rows.total() + colors::LANE_HEIGHT);

            let painter = ui.painter_at(content);
            painter.rect_filled(rect, 0.0, colors::BACKGROUND);
            let lane_top = |i: usize| content.top() - scroll_y + rows.top(i);
            for k in 0..strips.len() {
                let top = content.top() - scroll_y + rows.strip_top(k);
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(content.left(), top),
                        vec2(content.width(), automation::STRIP),
                    ),
                    0.0,
                    colors::HEADER,
                );
            }
            for i in 0..tracks.len() {
                let top = lane_top(i);
                let fill = if i % 2 == 0 {
                    colors::LANE_EVEN
                } else {
                    colors::LANE_ODD
                };
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(content.left(), top),
                        vec2(content.width(), rows.height(i)),
                    ),
                    0.0,
                    fill,
                );
            }
            for line in &lines {
                let (color, width) = match line.kind {
                    grid::Kind::Bar(_) => (colors::BAR_LINE, 1.0),
                    grid::Kind::Beat => (colors::BEAT_LINE, 1.0),
                };
                painter.vline(
                    axis.x(line.tick),
                    content.y_range(),
                    Stroke::new(width, color),
                );
            }

            if tracks.is_empty() {
                #[cfg(test)]
                state.drawn_text.push("No tracks yet".into());
                painter.text(
                    content.center(),
                    Align2::CENTER_CENTER,
                    "No tracks yet",
                    FontId::proportional(14.0),
                    colors::TEXT_WEAK,
                );
            }

            let mut hit_clip = false;
            // Where each clip was drawn, for the selection box.
            let mut drawn: Vec<(ClipId, Rect)> = Vec::new();
            let clips_before = state.selected.clone();
            let take = session.take_preview();
            // Every audio-armed track shows the same waveform: work it out once.
            let take_wave: std::cell::RefCell<Option<(usize, Vec<noodle_io::Peak>)>> = Default::default();
            for (index, &track) in tracks.iter().enumerate() {
                let top = lane_top(index);
                let lane_height = rows.lane_height(index);
                if top > content.bottom() || top + lane_height < content.top() {
                    continue;
                }
                let colour = colours[index];
                if let Some(take) = &take {
                    let drawn = take::draw(&painter, axis, top, lane_height, take, track, |n| {
                        let mut cached = take_wave.borrow_mut();
                        match &*cached {
                            Some((columns, peaks)) if *columns == n => peaks.clone(),
                            _ => {
                                let peaks = session.take_waveform(n);
                                *cached = Some((n, peaks.clone()));
                                peaks
                            }
                        }
                    });
                    #[cfg(test)]
                    if let Some((rect, label)) = drawn {
                        state.take_rects.insert(track, rect);
                        state.drawn_text.push(label);
                    }
                    #[cfg(not(test))]
                    let _ = drawn;
                }
                for (id, clip) in project.clips_on(track) {
                    let audio = clip.as_audio();
                    let loaded = audio.and_then(|audio| {
                        state
                            .sources
                            .get(ui.ctx(), directory.as_deref(), &audio.source)
                    });
                    let source = loaded.as_ref().map(|loaded| loaded.source);
                    let rate = source.map_or(FALLBACK_RATE, |s| s.sample_rate);
                    let end = clips::clip_end(map, clip, rate, warp_of(project, clip));
                    let full = Rect::from_min_max(
                        Pos2::new(axis.x(clip.start), top + 3.0),
                        Pos2::new(
                            axis.x(end).max(axis.x(clip.start) + 2.0),
                            top + lane_height - 3.0,
                        ),
                    );
                    let visible = full.intersect(content);
                    if visible.width() <= 0.0 || visible.height() <= 0.0 {
                        continue;
                    }
                    drawn.push((id, visible));
                    #[cfg(test)]
                    state.clip_rects.insert(id, visible);
                    let response =
                        ui.interact(visible, ui.id().with(("clip", id)), Sense::click_and_drag());
                    hit_clip |= response.contains_pointer() || response.is_pointer_button_down_on();
                    let mode_at = |x: f32| mode_for(full, x);
                    if let Some(pos) = response.hover_pos() {
                        ui.ctx().set_cursor_icon(match mode_at(pos.x) {
                            Mode::Move => CursorIcon::Grab,
                            _ => CursorIcon::ResizeHorizontal,
                        });
                    }
                    let selected = state.selected.contains(&id);
                    if response.drag_started() {
                        let press = ui
                            .input(|i| i.pointer.press_origin())
                            .or(response.interact_pointer_pos())
                            .unwrap_or(full.center());
                        if !selected && !ui.input(|i| i.modifiers.shift) {
                            state.selected = BTreeSet::new();
                        }
                        state.selected.insert(id);
                        // A link moves, trims and slips as one.
                        state.selected = project.with_linked(state.selected.iter().copied()).into_iter().collect();
                        let slipping =
                            clip.as_audio().is_some() && ui.input(|i| i.modifiers.shift && i.modifiers.alt);
                        let mode = if slipping {
                            Mode::Slip
                        } else {
                            mode_at(press.x)
                        };
                        let copying = mode == Mode::Move && ui.input(|i| i.modifiers.command);
                        let mut grabbed = id;
                        let mut originals: Vec<(ClipId, Clip)> = if mode == Mode::Move {
                            state
                                .selected
                                .iter()
                                .filter_map(|&sel| Some((sel, project.clip(sel)?.clone())))
                                .collect()
                        } else {
                            state.selected = project.linked_with(id).into_iter().collect();
                            state
                                .selected
                                .iter()
                                .filter_map(|&sel| Some((sel, project.clip(sel)?.clone())))
                                .collect()
                        };
                        if copying {
                            // The copies are what the drag moves; the originals stay.
                            // Added in the drag's own undo step.
                            let copies = copies_of(project, &originals, grabbed);
                            edits.extend(copies.adds);
                            originals = copies.copies;
                            grabbed = copies.grabbed;
                            state.selected = originals.iter().map(|(copy, _)| *copy).collect();
                        }
                        let sources = originals
                            .iter()
                            .filter_map(|(member, original)| {
                                let audio = original.as_audio()?;
                                let loaded = state.sources.get(
                                    ui.ctx(),
                                    directory.as_deref(),
                                    &audio.source,
                                )?;
                                Some((*member, loaded.source))
                            })
                            .collect();
                        state.drag = Some(Drag {
                            mode,
                            grabbed,
                            originals,
                            press_x: press.x,
                            copied: copying,
                            sources,
                        });
                    } else if response.double_clicked() && clip.as_midi().is_some() {
                        state.selected = project.linked_with(id).into_iter().collect();
                        open_midi = Some(id);
                    } else if response.double_clicked() && clip.as_audio().is_some() {
                        state.selected = project.linked_with(id).into_iter().collect();
                        open_audio = Some(id);
                    } else if response.clicked() {
                        let link: BTreeSet<ClipId> = project.linked_with(id).into_iter().collect();
                        if ui.input(|i| i.modifiers.shift) {
                            if state.selected.contains(&id) {
                                state.selected.retain(|member| !link.contains(member));
                            } else {
                                state.selected.extend(link);
                            }
                        } else {
                            state.selected = link;
                        }
                    }
                    // Pressing Ctrl/Cmd partway through a move drops copies where the
                    // clips began and carries on with those.
                    if response.dragged()
                        && ui.input(|i| i.modifiers.command)
                        && let Some(drag) = state.drag.as_mut()
                        && drag.mode == Mode::Move
                        && !drag.copied
                    {
                        let copies = copies_of(project, &drag.originals, drag.grabbed);
                        // The clips go back where they were, as the drag began.
                        edits.extend(drag.originals.iter().map(|(id, original)| {
                            Edit::Drag(Command::SetClip {
                                id: *id,
                                clip: original.clone(),
                            })
                        }));
                        edits.extend(copies.adds);
                        state.selected = copies.copies.iter().map(|(copy, _)| *copy).collect();
                        drag.originals = copies.copies;
                        drag.grabbed = copies.grabbed;
                        drag.copied = true;
                    }
                    if response.dragged()
                        && let (Some(drag), Some(pos)) = (&state.drag, response.interact_pointer_pos())
                    {
                        let free = ui.input(|i| i.modifiers.alt);
                        let lane = rows.track_at(pos.y - content.top() + scroll_y);
                        let target = tracks[lane];
                        // Dragged out of the timeline, an audio clip is carried to a
                        // Sampler or the graph, and stays where it was.
                        let pointer = ui.input(|i| i.pointer.latest_pos()).unwrap_or(pos);
                        let carried = drag.mode == Mode::Move && audio.is_some() && !rect.contains(pointer);
                        let (x, target) = if carried {
                            let home = drag
                                .originals
                                .iter()
                                .find(|(id, _)| *id == drag.grabbed)
                                .map_or(target, |(_, clip)| clip.node);
                            crate::carry::carrying(
                                ui.ctx(),
                                crate::carry::Item::Clip(drag.grabbed),
                                pointer,
                                "Sample",
                            );
                            (drag.press_x, home)
                        } else {
                            crate::carry::cancel(ui.ctx());
                            (pos.x, target)
                        };
                        let command = drag_command(
                            drag,
                            DragInput {
                                map,
                                axis,
                                x,
                                free,
                                target,
                                source,
                            },
                            project,
                        );
                        if let Some(command) = command {
                            edits.push(Edit::Drag(command));
                        }
                    }
                    if response.drag_stopped() {
                        state.drag = None;
                        edits.push(Edit::EndDrag);
                        if let Some((item, at)) = crate::carry::carried(ui.ctx()) {
                            crate::carry::release(ui.ctx(), item, at);
                        }
                    }

                    if response.secondary_clicked() && !selected {
                        state.selected = project.linked_with(id).into_iter().collect();
                    }
                    response.context_menu(|ui| {
                        if let Some(audio) = audio {
                            let mute = if audio.muted { "Unmute" } else { "Mute" };
                            if ui.button(mute).clicked() {
                                ui.close();
                                edits.push(Edit::Apply(toggle_mute(id, clip)));
                            }
                            if audio.takes.is_some() {
                                let open = state.takes.shown.contains(&id);
                                if ui
                                    .button(if open { "Hide takes" } else { "Show takes" })
                                    .on_hover_text("Show the takes under the track to comp them: drag across a take to play that part of it, click one to audition it")
                                    .clicked()
                                {
                                    ui.close();
                                    if open {
                                        state.takes.shown.remove(&id);
                                    } else {
                                        state.takes.shown.insert(id);
                                    }
                                }
                            }
                            if audio.takes.is_some()
                                && ui
                                    .button("Flatten takes")
                                    .on_hover_text(
                                        "Cut the comp at its joins into plain clips and drop the takes it doesn't play",
                                    )
                                    .clicked()
                            {
                                ui.close();
                                edits.extend(comp::flatten(project, map, id, rate).map(Edit::Apply));
                            }
                            if ui
                                .button("Split to stems")
                                .on_hover_text(
                                    "Make a track for the drums, bass, vocals and the rest, and mute this clip",
                                )
                                .clicked()
                            {
                                ui.close();
                                edits.push(Edit::SplitToStems(id));
                            }
                            if ui
                                .button("Pin transients")
                                .on_hover_text(
                                    "Put a warp marker on every hit of this clip and the clips linked to it (one for a hit the mics hear a few milliseconds apart), where it plays now. Quantise and the Slices mode work from them.",
                                )
                                .clicked()
                            {
                                ui.close();
                                edits.push(Edit::PinTransients(id, 50));
                            }
                            if ui
                                .button("Slice to Sampler")
                                .on_hover_text(
                                    "Cut this clip at its transients onto a new track that plays each slice from a key, starting at C2",
                                )
                                .clicked()
                            {
                                ui.close();
                                edits.push(Edit::SliceToSampler(id));
                            }
                            // A MIDI clip picked with this one is what the
                            // conversion replaces.
                            let replace = state
                                .selected
                                .iter()
                                .copied()
                                .find(|c| project.clip(*c).is_some_and(|c| c.as_midi().is_some()));
                            ui.menu_button("Convert to MIDI", |ui| {
                                ui.label(if replace.is_some() {
                                    "Replaces the selected MIDI clip"
                                } else {
                                    "Notes on a new track (select a MIDI clip too to replace it)"
                                });
                                for detect in crate::session::convert::Detect::ALL {
                                    if ui
                                        .button(detect.label())
                                        .on_hover_text(
                                            "Follows one voice or line. The notes are plain notes to edit; convert again to try another setting.",
                                        )
                                        .clicked()
                                    {
                                        ui.close();
                                        edits.push(Edit::ConvertToMidi(id, detect, replace));
                                    }
                                }
                            });
                            ui.separator();
                        }
                        if let Some(midi) = clip.as_midi()
                            && midi.takes.is_some()
                        {
                            let open = state.takes.shown.contains(&id);
                            if ui
                                .button(if open { "Hide takes" } else { "Show takes" })
                                .clicked()
                            {
                                ui.close();
                                if open {
                                    state.takes.shown.remove(&id);
                                } else {
                                    state.takes.shown.insert(id);
                                }
                            }
                            if ui
                                .button("Flatten takes")
                                .on_hover_text("Keep what the comp plays as a plain clip and drop the takes")
                                .clicked()
                            {
                                ui.close();
                                edits.extend(comp::flatten_midi(project, id).map(Edit::Apply));
                            }
                            ui.separator();
                        }
                        let players = crate::pattern_panel::players(project);
                        if clip.as_midi().is_some() && !players.is_empty() {
                            ui.menu_button("Copy to pattern", |ui| {
                                for (n, &player) in players.iter().enumerate() {
                                    if ui
                                        .button(format!("Pattern Player {}", n + 1))
                                        .on_hover_text("Add a copy of this clip as a slot of the player")
                                        .clicked()
                                    {
                                        ui.close();
                                        edits.extend(
                                            crate::pattern_panel::copy_to(project, clip, player)
                                                .map(Edit::Apply),
                                        );
                                    }
                                }
                            });
                            ui.separator();
                        }
                        let chosen: Vec<ClipId> = state.selected.iter().copied().collect();
                        let linked = project.group_of(id).map(|(_, members)| members);
                        let whole_link = linked.as_ref().is_some_and(|m| *m == chosen);
                        if chosen.len() >= 2
                            && !whole_link
                            && ui
                                .button("Link")
                                .on_hover_text(
                                    "Move, trim, cut, warp and delete these clips together",
                                )
                                .clicked()
                        {
                            ui.close();
                            edits.extend(project.link_command(&chosen).map(Edit::Apply));
                        }
                        if chosen.iter().any(|&c| project.group_of(c).is_some())
                            && ui.button("Unlink").clicked()
                        {
                            ui.close();
                            edits.extend(project.unlink_command(&chosen).map(Edit::Apply));
                        }
                        ui.separator();
                        for (action, label) in [
                            (ClipAction::Cut, "Cut"),
                            (ClipAction::Copy, "Copy"),
                            (ClipAction::Duplicate, "Duplicate"),
                        ] {
                            if ui.button(label).clicked() {
                                ui.close();
                                state.pending = Some(action);
                            }
                        }
                    });

                    let painter = ui.painter_at(visible);
                    // A muted section stays where it is but is drawn faded.
                    let fill = if audio.is_some_and(|audio| audio.muted) {
                        colour.gamma_multiply(0.35)
                    } else {
                        colour
                    };
                    painter.rect_filled(full, f32::from(crate::theme::RADIUS), fill);
                    let missing = audio.is_some() && source.is_none();
                    let label = match (audio, clip.as_midi()) {
                        (Some(audio), _) => {
                            if let Some(peaks) = loaded.as_ref().and_then(|loaded| loaded.peaks.as_ref()) {
                                let wave = waveform::Clip {
                                    id,
                                    peaks,
                                    audio,
                                    colour,
                                };
                                let runs = clips::passes(map, clip, rate, warp_of(project, clip));
                                let drawn = match runs {
                                    // A looping section shows its audio up to the
                                    // loop's end and then the loop, run by run.
                                    Some(runs) => {
                                        let mut any = false;
                                        let last = runs.len().saturating_sub(1);
                                        for (n, run) in runs.iter().enumerate() {
                                            let rect = Rect::from_min_max(
                                                Pos2::new(axis.x(run.from), full.top()),
                                                Pos2::new(axis.x(run.to), full.bottom()),
                                            );
                                            if rect.right() < visible.left()
                                                || rect.left() > visible.right()
                                            {
                                                continue;
                                            }
                                            // The first run, the whole runs and the
                                            // cut-off last have their own cache slots.
                                            let slot = if n == 0 {
                                                0
                                            } else if n == last {
                                                2
                                            } else {
                                                1
                                            };
                                            any |= waveform::draw_range(
                                                &painter,
                                                &mut state.waveforms,
                                                (id, slot),
                                                rect,
                                                rect.intersect(visible),
                                                &wave,
                                                run.frames,
                                            );
                                        }
                                        any
                                    }
                                    None => {
                                        // A warped section shows the audio as it
                                        // plays, once that has been rendered, and the
                                        // file's waveform, evenly stretched, till then.
                                        let rendered = warp_of(project, clip)
                                            .filter(|warp| {
                                                !warp.is_identity() && warp.loop_region.is_none()
                                            })
                                            .and_then(|warp| {
                                                let request = warped_waves::Request {
                                                    clip: id,
                                                    path: directory
                                                        .as_deref()
                                                        .unwrap_or(std::path::Path::new(""))
                                                        .join(&audio.source),
                                                    tempo: map.clone(),
                                                    start: clip.start,
                                                    warp: warp.clone(),
                                                    offset: audio.offset,
                                                    length: audio.length,
                                                    file: peaks.clone(),
                                                    span: (clip.start, end),
                                                };
                                                state.warped_waves.shown(ui.ctx(), &request)
                                            });
                                        match rendered {
                                            Some(rendered) => {
                                                let area = warped_area(axis, full, &rendered);
                                                let played = waveform::Clip {
                                                    peaks: &rendered.peaks,
                                                    ..wave
                                                };
                                                waveform::draw_range(
                                                    &painter,
                                                    &mut state.waveforms,
                                                    (id, 3),
                                                    area,
                                                    area.intersect(visible),
                                                    &played,
                                                    (0, rendered.peaks.frames()),
                                                )
                                            }
                                            None => waveform::draw(
                                                &painter,
                                                &mut state.waveforms,
                                                full,
                                                visible,
                                                &wave,
                                            ),
                                        }
                                    }
                                };
                                #[cfg(test)]
                                {
                                    state.waveforms_drawn += usize::from(drawn);
                                }
                                #[cfg(not(test))]
                                let _ = drawn;
                            }
                            let name = std::path::Path::new(&audio.source).file_name().map_or_else(
                                || audio.source.clone(),
                                |n| n.to_string_lossy().into_owned(),
                            );
                            if missing {
                                format!("{name} (missing)")
                            } else {
                                name
                            }
                        }
                        (None, Some(midi)) => {
                            midi_preview::draw(&painter, axis, full, clip.start, midi);
                            format!("MIDI ({} notes)", midi.notes.len())
                        }
                        (None, None) => String::new(),
                    };
                    #[cfg(test)]
                    state.drawn_text.push(label.clone());
                    painter.text(
                        full.left_top() + vec2(6.0, 4.0),
                        Align2::LEFT_TOP,
                        label,
                        FontId::proportional(11.0),
                        Color32::BLACK.gamma_multiply(0.75),
                    );
                    if missing {
                        painter.rect_stroke(
                            full,
                            f32::from(crate::theme::RADIUS),
                            Stroke::new(1.5, colors::MISSING),
                            StrokeKind::Inside,
                        );
                    } else if selected {
                        painter.rect_stroke(
                            full,
                            f32::from(crate::theme::RADIUS),
                            Stroke::new(1.5, colors::SELECTED),
                            StrokeKind::Inside,
                        );
                    }
                    if full.width() >= MIN_TRIMMABLE * 2.0 && audio.is_some() && source.is_some() {
                        let handles = Handles {
                            ui,
                            state,
                            map,
                            axis,
                            rate,
                            full,
                            visible,
                            project,
                        };
                        edits.extend(fade_handles(handles, id, clip));
                    }
                }
            }

            // The take lanes of the open take folders, track by track.
            for (index, &track) in tracks.iter().enumerate() {
                if take_lanes[index] == 0 {
                    continue;
                }
                let lane_height = rows.lane_height(index);
                let top = lane_top(index);
                if top > content.bottom() || top + rows.height(index) < content.top() {
                    continue;
                }
                let shown = takes::View {
                    project,
                    axis,
                    content,
                    directory: directory.as_deref(),
                    colour: colours[index],
                    lane_top: top,
                    lane_height,
                    audition: session.take_audition(),
                };
                let lanes = takes::show(ui, state, &shown, track);
                edits.extend(lanes.edits);
                if lanes.audition.is_some() {
                    audition = lanes.audition;
                }
            }

            // The handles and crossfades where audio clips are butted together.
            let infos: crossfade::Infos = project
                .clips()
                .filter_map(|(id, clip)| {
                    let audio = clip.as_audio()?;
                    let loaded = state
                        .sources
                        .get(ui.ctx(), directory.as_deref(), &audio.source)?;
                    Some((id, (loaded.source.sample_rate, loaded.source.frames)))
                })
                .collect();
            let tops: Vec<(NodeId, f32)> = tracks
                .iter()
                .enumerate()
                .map(|(i, &track)| (track, lane_top(i)))
                .collect();
            edits.extend(crossfade::show(
                ui,
                &mut state.crossfade,
                &crossfade::View {
                    project,
                    axis,
                    content,
                    tracks: &tops,
                    infos: &infos,
                    lane_height: colors::LANE_HEIGHT,
                },
            ));
            if let Some(range) = state.range {
                let (x0, x1) = (axis.x(range.start), axis.x(range.end));
                let band = Rect::from_x_y_ranges(
                    x0.max(content.left())..=x1.min(content.right()),
                    content.y_range(),
                );
                if band.width() > 0.0 {
                    ui.painter_at(content)
                        .rect_filled(band, 0.0, colors::SELECTED.gamma_multiply(0.12));
                }
            }
            let before_box = state.selected.clone();
            select_by_box(ui, state, &background, content, &drawn, state.scroll_x, scroll_y);
            // A clip picked takes its link with it.
            if state.selected != before_box {
                state.selected = project
                    .with_linked(state.selected.iter().copied())
                    .into_iter()
                    .collect();
            }

            // Clips and automation points share the Delete key, so picking one
            // drops the other from the selection.
            if state.selected != clips_before && !state.selected.is_empty() {
                state.range = None;
                state.selected_track = None;
                state.automation.deselect();
                state.tempo.deselect();
            }
            let point_before = state.automation.selected();
            edits.extend(automation::show(
                ui,
                &mut state.automation,
                &automation::View {
                    project,
                    rows: &rows,
                    content,
                    scroll_y,
                    axis,
                },
            ));

            if state.automation.selected().is_some() && state.automation.selected() != point_before {
                state.range = None;
                state.selected.clear();
                state.tempo.deselect();
            }

            // A drag whose clip has scrolled off screen or been removed never sees
            // its widget's `drag_stopped`, which would leave its undo group open.
            if state.drag.is_some() && !ui.input(|i| i.pointer.any_down()) {
                state.drag = None;
                crate::carry::cancel(ui.ctx());
                if !edits.contains(&Edit::EndDrag) {
                    edits.push(Edit::EndDrag);
                }
            }
            if background.clicked() && !hit_clip {
                state.selected.clear();
            }
            // Double-clicking an empty lane starts a MIDI clip there and
            // opens it in the piano roll.
            if background.double_clicked()
                && !hit_clip
                && !tracks.is_empty()
                && let Some(at) = background
                    .interact_pointer_pos()
                    .filter(|p| content.contains(*p))
            {
                let id = project.next_clip_id();
                state.selected = BTreeSet::from([id]);
                edits.push(Edit::Apply(Command::AddClip {
                    id,
                    clip: Clip::midi(
                        tracks[rows.track_at(at.y - content.top() + state.scroll_y)],
                        axis.snap(axis.tick(at.x).max(Tick::ZERO)),
                        Tick(NEW_MIDI_CLIP),
                    ),
                }));
                open_midi = Some(id);
            }
            // With no clip anywhere, say how to start one.
            if !tracks.is_empty() && project.clips().next().is_none() {
                ui.painter_at(content).text(
                    content.center_top() + vec2(0.0, 24.0),
                    egui::Align2::CENTER_CENTER,
                    "Double-click a lane to add a MIDI clip and open the piano roll",
                    egui::FontId::proportional(13.0),
                    colors::TEXT_WEAK,
                );
            }
            // A focused button (a clicked header) isn't a text field; typing is.
            let delete_pressed = hovered
                && !ui.ctx().egui_wants_keyboard_input()
                && ui.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace));
            // A selected track goes when no clip or point is selected to take it.
            if delete_pressed
                && state.selected.is_empty()
                && state.automation.selected().is_none()
                && state.tempo.selected().is_none()
                && let Some(group) = state
                    .selected_track
                    .and_then(|input| project.graph().node(input))
                    .and_then(|n| n.parent)
            {
                edits.push(Edit::Apply(header::delete_track(group)));
                state.selected_track = None;
            }
            if delete_pressed && !state.selected.is_empty() && state.tempo.selected().is_none() {
                let removals = project
                    .with_linked(state.selected.iter().copied())
                    .into_iter()
                    .map(|id| Command::RemoveClip { id })
                    .collect();
                edits.push(Edit::Apply(Command::Batch(removals)));
            }

            // S cuts the selected clips (every clip, if none is selected) at the
            // playhead; Cmd+D copies them to just after themselves.
            if hovered && !ui.ctx().egui_wants_keyboard_input() {
                let split_key = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::S));
                let copy_key = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::D));
                let rate_of = |state: &mut TimelineState, clip: &Clip| {
                    clip.as_audio().map_or(FALLBACK_RATE, |audio| {
                        state
                            .sources
                            .get(ui.ctx(), directory.as_deref(), &audio.source)
                            .map_or(FALLBACK_RATE, |loaded| loaded.source.sample_rate)
                    })
                };
                if split_key && let Some(at) = playhead {
                    // Cutting one linked clip cuts them all.
                    let wanted: Vec<ClipId> = if state.selected.is_empty() {
                        project.timeline_clips().map(|(id, _)| id).collect()
                    } else {
                        project.with_linked(state.selected.iter().copied())
                    };
                    let mut commands = Vec::new();
                    let mut right_halves = BTreeSet::new();
                    let mut next = project.next_clip_id();
                    // Which right half came from which clip, to link them as
                    // the clips they come from were.
                    let mut halves: BTreeMap<ClipId, ClipId> = BTreeMap::new();
                    for id in wanted {
                        let Some(clip) = project.clip(id) else {
                            continue;
                        };
                        let rate = rate_of(state, clip);
                        if let Some((first, second)) =
                            clips::split(map, clip, at, rate, warp_of(project, clip))
                        {
                            commands.push(Command::SetClip { id, clip: first });
                            commands.push(Command::AddClip {
                                id: next,
                                clip: second,
                            });
                            right_halves.insert(next);
                            halves.insert(id, next);
                            next.0 += 1;
                        }
                    }
                    // The left halves stay in their link; the right halves
                    // make one of their own, which shares the warp (a clip
                    // cut in two has one warp until a half is edited alone).
                    let mut group_id = project.next_clip_group_id();
                    let mut made = BTreeSet::new();
                    for &id in halves.keys() {
                        let Some((old, members)) = project.group_of(id) else {
                            continue;
                        };
                        if !made.insert(old) {
                            continue;
                        }
                        let rights: BTreeSet<ClipId> =
                            members.iter().filter_map(|m| halves.get(m).copied()).collect();
                        if rights.len() >= 2 {
                            commands.push(Command::SetClipGroup {
                                id: group_id,
                                group: Some(noodle_core::ClipGroup { members: rights }),
                            });
                            group_id.0 += 1;
                        }
                    }
                    if commands.is_empty() {
                        notice = Some("Move the playhead inside a clip to cut it".to_string());
                    } else {
                        state.selected = right_halves;
                        edits.push(Edit::Apply(Command::Batch(commands)));
                    }
                }
                let (key_copy, key_cut, key_paste) = ui.input_mut(|i| {
                    // Egui turns the chords into events, and the clipboard's text into
                    // a paste only when it has some, so a copy leaves a note there.
                    let event = |wanted: fn(&egui::Event) -> bool, i: &egui::InputState| {
                        i.events.iter().any(wanted)
                    };
                    let copy = event(|e| matches!(e, egui::Event::Copy), i);
                    let cut = event(|e| matches!(e, egui::Event::Cut), i);
                    let paste = event(|e| matches!(e, egui::Event::Paste(_)), i);
                    (
                        i.consume_key(egui::Modifiers::COMMAND, Key::C) | copy,
                        i.consume_key(egui::Modifiers::COMMAND, Key::X) | cut,
                        i.consume_key(egui::Modifiers::COMMAND, Key::V) | paste,
                    )
                });
                state.pending = state.pending.take().or(if key_cut {
                    Some(ClipAction::Cut)
                } else if key_copy {
                    Some(ClipAction::Copy)
                } else if key_paste {
                    Some(ClipAction::Paste)
                } else if copy_key {
                    Some(ClipAction::Duplicate)
                } else {
                    None
                });
            }
            // The Edit menu and the clips' menus ask for these too, with the pointer
            // elsewhere.
            if !ui.ctx().egui_wants_keyboard_input()
                && let Some(action) = state.pending.take()
            {
                let rate_of = |state: &mut TimelineState, clip: &Clip| {
                    clip.as_audio().map_or(FALLBACK_RATE, |audio| {
                        state
                            .sources
                            .get(ui.ctx(), directory.as_deref(), &audio.source)
                            .map_or(FALLBACK_RATE, |loaded| loaded.source.sample_rate)
                    })
                };
                let ranged = state
                    .range
                    .filter(|_| state.selected.is_empty() && state.automation.selected().is_none());
                let pasting_section = action == ClipAction::Paste
                    && crate::clipboard::holds(crate::clipboard::SECTION_MIME);
                if ranged.is_some() || pasting_section {
                    let rates = source_rates(project, &mut state.sources, ui.ctx(), directory.as_deref());
                    let rate = |clip: &Clip| {
                        clip.as_audio()
                            .and_then(|audio| rates.get(&audio.source).copied())
                            .unwrap_or(FALLBACK_RATE)
                    };
                    match (action, ranged) {
                        (ClipAction::Copy | ClipAction::Cut, Some(range)) => {
                            let section = sections::copy(project, range, &rate);
                            crate::clipboard::copy(
                                crate::clipboard::SECTION_MIME,
                                &section,
                                "a section",
                            );
                            if action == ClipAction::Cut {
                                edits.push(Edit::Apply(sections::clear(project, range, &rate)));
                            }
                        }
                        (ClipAction::InsertTime, Some(range)) => {
                            edits.extend(
                                sections::insert_time(project, range.start, range.len(), &rate)
                                    .map(Edit::Apply),
                            );
                        }
                        (ClipAction::DeleteTime, Some(range)) => {
                            edits.push(Edit::Apply(sections::delete_time(project, range, &rate)));
                            state.range = None;
                        }
                        (ClipAction::Paste, _) => {
                            if let Some(section) = crate::clipboard::paste::<sections::Section>(
                                crate::clipboard::SECTION_MIME,
                            ) {
                                let at = playhead.unwrap_or(Tick::ZERO);
                                let (command, _) = sections::paste_section(project, &section, at);
                                if let Some(command) = command {
                                    edits.push(Edit::Apply(command));
                                    state.range = sections::Range::new(
                                        at,
                                        Tick(at.0 + section.length.0),
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let handled = ranged.is_some() || pasting_section;
                if !handled {
                edits.extend(automation::act(
                    action,
                    &mut state.automation,
                    project,
                    playhead.unwrap_or(Tick::ZERO),
                ));
                let chosen: Vec<(ClipId, Clip)> = state
                    .selected
                    .iter()
                    .filter_map(|&id| Some((id, project.clip(id)?.clone())))
                    .collect();
                if matches!(action, ClipAction::Copy | ClipAction::Cut) && !chosen.is_empty() {
                    let copied = Copied::of(project, chosen.iter().map(|(_, clip)| clip));
                    crate::clipboard::copy(
                        crate::clipboard::CLIP_MIME,
                        &copied,
                        &format!("{} clips", copied.clips.len()),
                    );
                    if action == ClipAction::Cut {
                        let removals = chosen
                            .iter()
                            .map(|&(id, _)| Command::RemoveClip { id })
                            .collect();
                        state.selected.clear();
                        edits.push(Edit::Apply(Command::Batch(removals)));
                    }
                }
                if action == ClipAction::Paste
                    && let Some(clips) = crate::clipboard::paste::<Copied>(crate::clipboard::CLIP_MIME)
                {
                    // On the selected track if the clips come from one, else where
                    // they came from.
                    let track = state.selected_track.or_else(|| {
                        let mut tracks = chosen.iter().map(|(_, clip)| clip.node);
                        tracks.next().filter(|first| tracks.all(|t| t == *first))
                    });
                    let (command, pasted) = paste(project, &clips, playhead.unwrap_or(Tick::ZERO), track);
                    if let Some(command) = command {
                        state.selected = pasted;
                        edits.push(Edit::Apply(command));
                    }
                }
                if action == ClipAction::Duplicate && !chosen.is_empty() {
                    let last = chosen
                        .iter()
                        .map(|(_, c)| clips::clip_end(map, c, rate_of(state, c), warp_of(project, c)))
                        .max()
                        .unwrap_or_default();
                    let clipboard = Copied::of(project, chosen.iter().map(|(_, clip)| clip));
                    let (command, copies) = paste(project, &clipboard, last, None);
                    // The earliest copy goes at `last`: just after the others.
                    if let Some(command) = command {
                        state.selected = copies;
                        edits.push(Edit::Apply(command));
                    }
                }
                }
            }

            // Ctrl/Cmd+Left and Right take the playhead to the edges of the selected
            // clips, and scroll to keep it in view.
            if hovered && !state.selected.is_empty() && !ui.ctx().egui_wants_keyboard_input() {
                let key = ui.input_mut(|i| {
                    [Key::ArrowLeft, Key::ArrowRight]
                        .into_iter()
                        .find(|&key| i.consume_key(egui::Modifiers::COMMAND, key))
                });
                if let Some(key) = key {
                    let selected = state.selected.iter().filter_map(|&id| project.clip(id));
                    jump = match key {
                        Key::ArrowLeft => selected.map(|clip| clip.start).min(),
                        _ => selected
                            .map(|clip| {
                                let rate = clip.as_audio().map_or(FALLBACK_RATE, |audio| {
                                    state
                                        .sources
                                        .get(ui.ctx(), directory.as_deref(), &audio.source)
                                        .map_or(FALLBACK_RATE, |loaded| loaded.source.sample_rate)
                                });
                                clips::clip_end(map, clip, rate, warp_of(project, clip))
                            })
                            .max(),
                    };
                }
            }
            if let Some(tick) = jump {
                let x = axis.x(tick);
                let margin = 24.0;
                if x < content.left() + margin {
                    state.scroll_x = (state.scroll_x - (content.left() + margin - x)).max(0.0);
                } else if x > content.right() - margin {
                    state.scroll_x += x - (content.right() - margin);
                }
            }

            let (header_edits, header_arm) =
                draw_headers(
                    ui,
                    rect,
                    content,
                    Lanes {
                        tracks: &tracks,
                        all: &all_tracks,
                        strips: &strips,
                        colours: &colours,
                    },
                    &rows,
                    session,
                    state,
                    scroll_y,
                );
            edits.extend(header_edits);
            arm.extend(header_arm);
        });
    state.scroll_y = scroll.state.offset.y;

    let _markers = draw_ruler(ui, rect, axis, &lines);
    #[cfg(test)]
    state.drawn_text.extend(_markers);
    let ruler = Rect::from_min_max(
        Pos2::new(content.left(), rect.top()),
        Pos2::new(rect.right(), rect.top() + colors::RULER_HEIGHT),
    );
    #[cfg(test)]
    {
        state.ruler_seen = Some((ruler, axis.origin, axis.pps));
    }
    if state.show_tempo {
        let row = Rect::from_min_size(
            Pos2::new(content.left(), rect.top() + colors::RULER_HEIGHT),
            vec2(content.width(), lane_height),
        );
        let header = Rect::from_min_size(
            Pos2::new(rect.left(), row.top()),
            vec2(colors::HEADER_WIDTH, lane_height),
        );
        let painter = ui.painter_at(header);
        painter.rect_filled(header, 0.0, colors::AUTOMATION_HEADER);
        painter.text(
            header.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            "Tempo",
            FontId::proportional(11.0),
            colors::TEXT,
        );
        #[cfg(test)]
        state.drawn_text.push("Tempo".into());
        let visible = row.intersect(rect);
        let tempo_before = state.tempo.selected();
        edits.extend(tempo_lane::show(
            ui,
            &mut state.tempo,
            map,
            row,
            visible,
            axis,
        ));
        // A tempo point shares Delete with clips, automation points and
        // tracks, so picking one lets the others go.
        if state.tempo.selected().is_some() && state.tempo.selected() != tempo_before {
            state.selected.clear();
            state.selected_track = None;
            state.automation.deselect();
        }
    }
    let scrub = ui.interact(ruler, ui.id().with("ruler"), Sense::click_and_drag());
    let wanted = scrub
        .interact_pointer_pos()
        .filter(|_| {
            ui.input(|i| i.pointer.primary_down())
                && (scrub.clicked() || scrub.dragged() || scrub.is_pointer_button_down_on())
        })
        .map(|pos| {
            let tick = axis.tick(pos.x).max(Tick::ZERO);
            if ui.input(|i| i.modifiers.alt) {
                tick
            } else {
                axis.snap(tick)
            }
        });
    // Right-click on the ruler adds or edits a time signature change at the
    // bar under the pointer. The loop is a band along it, dimmed when off,
    // whose edges can be dragged.
    let mut ruler_loop = None;
    {
        let region = session.loop_region();
        let (x0, x1) = (axis.x(region.start), axis.x(region.end));
        let band = Rect::from_x_y_ranges(
            x0.max(ruler.left())..=x1.min(ruler.right()),
            ruler.y_range(),
        );
        let alpha = if region.on { 0.3 } else { 0.12 };
        if band.width() > 0.0 {
            ui.painter_at(ruler).rect_filled(
                band.shrink2(vec2(0.0, 1.0)),
                0.0,
                colors::SELECTED.gamma_multiply(alpha),
            );
        }
        for (is_start, x) in [(true, x0), (false, x1)] {
            if x < ruler.left() - 4.0 || x > ruler.right() + 4.0 {
                continue;
            }
            let handle = Rect::from_x_y_ranges(x - 3.0..=x + 3.0, ruler.y_range());
            let id = ui.id().with(("loop_edge", is_start));
            let response = ui
                .interact(handle, id, Sense::drag())
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            ui.painter_at(ruler).vline(
                x,
                ruler.y_range(),
                Stroke::new(
                    if response.hovered() || response.dragged() {
                        3.0
                    } else {
                        2.0
                    },
                    colors::SELECTED.gamma_multiply(if region.on { 1.0 } else { 0.5 }),
                ),
            );
            if response.dragged()
                && let Some(pos) = response.interact_pointer_pos()
            {
                let tick = axis.snap(axis.tick(pos.x).max(Tick::ZERO));
                let mut moved = region;
                if is_start {
                    moved.start = tick.min(Tick((region.end.0 - 1).max(0)));
                } else {
                    moved.end = tick.max(Tick(region.start.0 + 1));
                }
                if moved != region {
                    ruler_loop = Some(moved);
                }
            }
        }
    }
    // The punch range is a second band, along the bottom of the ruler: the
    // part of the timeline a recording keeps. Its edges can be dragged.
    {
        let punch = session.project().punch();
        let strip = Rect::from_x_y_ranges(ruler.x_range(), ruler.bottom() - 6.0..=ruler.bottom());
        let (x0, x1) = (axis.x(punch.start), axis.x(punch.end));
        let band = Rect::from_x_y_ranges(
            x0.max(ruler.left())..=x1.min(ruler.right()),
            strip.y_range(),
        );
        if band.width() > 0.0 {
            ui.painter_at(ruler).rect_filled(
                band,
                0.0,
                crate::theme::RECORD.gamma_multiply(if punch.on { 0.8 } else { 0.25 }),
            );
        }
        for (is_start, x) in [(true, x0), (false, x1)] {
            if x < ruler.left() - 4.0 || x > ruler.right() + 4.0 {
                continue;
            }
            let handle = Rect::from_x_y_ranges(x - 3.0..=x + 3.0, strip.y_range());
            let id = ui.id().with(("punch_edge", is_start));
            let response = ui
                .interact(handle, id, Sense::drag())
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                .on_hover_text(if is_start { "Punch in" } else { "Punch out" });
            if response.dragged()
                && let Some(pos) = response.interact_pointer_pos()
            {
                let tick = axis.snap(axis.tick(pos.x).max(Tick::ZERO));
                let mut moved = punch;
                if is_start {
                    moved.start = tick.min(Tick((punch.end.0 - 1).max(0)));
                } else {
                    moved.end = tick.max(Tick(punch.start.0 + 1));
                }
                if moved != punch {
                    edits.push(Edit::Drag(Command::SetPunch(moved)));
                }
            }
            if response.drag_stopped() {
                edits.push(Edit::EndDrag);
            }
        }
    }
    // Markers: a flag on the ruler for each, dragged to move and right-clicked
    // for its menu. The span from one to the next is a section.
    {
        let markers = project.markers();
        let painter = ui.painter_at(ruler);
        let flag_top = ruler.top();
        for (i, marker) in markers.iter().enumerate() {
            let x = axis.x(marker.at);
            let galley = painter.layout_no_wrap(
                match marker.key {
                    Some(key) => format!("{} · {key}", marker.name),
                    None => marker.name.clone(),
                },
                FontId::proportional(10.0),
                MARKER_COLOUR,
            );
            if x + galley.size().x + 8.0 < ruler.left() || x - 4.0 > ruler.right() {
                continue;
            }
            let hit = Rect::from_min_size(
                Pos2::new(x - 4.0, flag_top),
                vec2(galley.size().x + 12.0, 12.0),
            );
            let response = ui
                .interact(hit, ui.id().with(("marker", i)), Sense::click_and_drag())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            painter.vline(x, ruler.y_range(), Stroke::new(1.5, MARKER_COLOUR));
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(x, flag_top), vec2(galley.size().x + 6.0, 12.0)),
                2.0,
                MARKER_COLOUR.gamma_multiply(if response.hovered() { 0.5 } else { 0.3 }),
            );
            painter.galley(Pos2::new(x + 3.0, flag_top + 1.0), galley, MARKER_COLOUR);
            if response.dragged()
                && let Some(pos) = response.interact_pointer_pos()
            {
                // Between its neighbours, so it keeps its place in the list.
                let low = if i == 0 { 0 } else { markers[i - 1].at.0 + 1 };
                let high = markers.get(i + 1).map_or(i64::MAX, |next| next.at.0 - 1);
                let tick = axis.snap(axis.tick(pos.x).max(Tick::ZERO));
                let tick = Tick(tick.0.clamp(low, high.max(low)));
                if tick != marker.at {
                    let mut moved = markers.to_vec();
                    moved[i].at = tick;
                    edits.push(Edit::Drag(Command::SetMarkers(moved)));
                }
            }
            if response.drag_stopped() {
                edits.push(Edit::EndDrag);
            }
            if response.secondary_clicked() {
                state.marker_menu = Some(i);
                state.marker_name = marker.name.clone();
            }
            // A click in the name field must not close the menu, so its buttons
            // close it themselves.
            egui::Popup::context_menu(&response)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    if state.marker_menu != Some(i) {
                        return;
                    }
                    let name = ui.add(
                        egui::TextEdit::singleline(&mut state.marker_name)
                            .hint_text("Marker name")
                            .desired_width(120.0),
                    );
                    if name.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                        let mut renamed = markers.to_vec();
                        renamed[i].name = state.marker_name.trim().to_string();
                        if renamed[i].name.is_empty() {
                            renamed[i].name = marker.name.clone();
                        }
                        edits.push(Edit::Apply(Command::SetMarkers(renamed)));
                        ui.close();
                    }
                    ui.horizontal(|ui| {
                        ui.label("Key");
                        let shown = marker.key.map(noodle_core::KeyTarget::Fixed);
                        if let Some(picked) = crate::clip_panel::key_picker(
                            ui,
                            "marker key",
                            shown,
                            Some("Same as before"),
                            false,
                        ) {
                            let mut changed = markers.to_vec();
                            changed[i].key = match picked {
                                Some(noodle_core::KeyTarget::Fixed(key)) => Some(key),
                                _ => None,
                            };
                            edits.push(Edit::Apply(Command::SetMarkers(changed)));
                        }
                    })
                    .response
                    .on_hover_text(
                        "Clips that follow the timeline's key are carried to this key from here",
                    );
                    let next = markers
                        .get(i + 1)
                        .map_or("end of the song".to_string(), |next| {
                            format!("\"{}\"", next.name)
                        });
                    if ui
                        .button(format!("Select to {next}"))
                        .on_hover_text("Select every track between this marker and the next")
                        .clicked()
                    {
                        ui.close();
                        let rates = source_rates(
                            project,
                            &mut state.sources,
                            ui.ctx(),
                            directory.as_deref(),
                        );
                        let rate = |clip: &Clip| {
                            clip.as_audio()
                                .and_then(|audio| rates.get(&audio.source).copied())
                                .unwrap_or(FALLBACK_RATE)
                        };
                        if let Some(range) = sections::between_markers(project, i, &rate) {
                            select_range(state, range);
                        }
                    }
                    if ui.button("Delete marker").clicked() {
                        ui.close();
                        let mut rest = markers.to_vec();
                        rest.remove(i);
                        edits.push(Edit::Apply(Command::SetMarkers(rest)));
                    }
                });
        }
        // The selected range is a band along the top of the ruler.
        if let Some(range) = state.range {
            let (x0, x1) = (axis.x(range.start), axis.x(range.end));
            let band = Rect::from_x_y_ranges(
                x0.max(ruler.left())..=x1.min(ruler.right()),
                ruler.top()..=ruler.top() + 3.0,
            );
            if band.width() > 0.0 {
                painter.rect_filled(band, 0.0, colors::SELECTED);
            }
        }
    }
    if scrub.secondary_clicked() {
        state.ruler_menu_bar = scrub
            .interact_pointer_pos()
            .map(|pos| map.position(axis.tick(pos.x).max(Tick::ZERO)).bar);
    }
    scrub.context_menu(|ui| {
        let Some(bar) = state.ruler_menu_bar else {
            return;
        };
        ui.label(format!("Bar {}", bar + 1));
        let region = session.loop_region();
        let here = bar_tick(map, bar + 1);
        if ui
            .button(if region.on { "Loop off" } else { "Loop on" })
            .clicked()
        {
            ui.close();
            ruler_loop = Some(crate::session::LoopRegion {
                on: !region.on,
                ..region
            });
        }
        if ui.button("Set loop start here").clicked() {
            ui.close();
            let end = if region.end > here {
                region.end
            } else {
                bar_tick(map, bar + 2)
            };
            ruler_loop = Some(crate::session::LoopRegion {
                start: here,
                end,
                ..region
            });
        }
        if ui.button("Set loop end here").clicked() {
            ui.close();
            let start = if region.start < here {
                region.start
            } else {
                Tick::ZERO
            };
            let end = if here > start { here } else { bar_tick(map, 2) };
            ruler_loop = Some(crate::session::LoopRegion {
                start,
                end,
                ..region
            });
        }
        ui.separator();
        let punch = session.project().punch();
        ui.menu_button("Punch", |ui| {
            if ui
                .button(if punch.on { "Punch off" } else { "Punch on" })
                .clicked()
            {
                ui.close();
                edits.push(Edit::Apply(Command::SetPunch(noodle_core::PunchRange {
                    on: !punch.on,
                    ..punch
                })));
            }
            if ui.button("Set punch in here").clicked() {
                ui.close();
                let end = if punch.end > here {
                    punch.end
                } else {
                    bar_tick(map, bar + 2)
                };
                edits.push(Edit::Apply(Command::SetPunch(noodle_core::PunchRange {
                    start: here,
                    end,
                    ..punch
                })));
            }
            if ui.button("Set punch out here").clicked() {
                ui.close();
                let start = if punch.start < here {
                    punch.start
                } else {
                    Tick::ZERO
                };
                let end = if here > start { here } else { bar_tick(map, 2) };
                edits.push(Edit::Apply(Command::SetPunch(noodle_core::PunchRange {
                    start,
                    end,
                    ..punch
                })));
            }
        });
        ui.separator();
        ui.menu_button("Markers and selection", |ui| {
            ui.menu_button(format!("Key at the start: {}", project.key()), |ui| {
                for key in crate::clip_panel::all_keys() {
                    if ui
                        .selectable_label(key == project.key(), key.to_string())
                        .clicked()
                    {
                        ui.close();
                        edits.push(Edit::Apply(Command::SetKey(key)));
                    }
                }
            })
            .response
            .on_hover_text("The key of the timeline until a marker changes it");
            if ui
                .button(format!("Add marker at bar {}", bar + 1))
                .on_hover_text("Name a point of the song: a verse, the chorus")
                .clicked()
            {
                ui.close();
                let mut markers = session.project().markers().to_vec();
                if markers.iter().all(|m| m.at != here) {
                    markers.push(noodle_core::Marker {
                        name: sections::next_name(session.project()),
                        key: None,
                        at: here,
                    });
                    edits.push(Edit::Apply(Command::SetMarkers(markers)));
                }
            }
            if ui
                .button("Start selection here")
                .on_hover_text("Select from here across all tracks, to copy, cut or delete")
                .clicked()
            {
                ui.close();
                let end = state
                    .range
                    .map(|r| r.end)
                    .filter(|&end| end > here)
                    .unwrap_or_else(|| bar_tick(map, bar + 2));
                if let Some(range) = sections::Range::new(here, end) {
                    select_range(state, range);
                }
            }
            if ui.button("End selection here").clicked() {
                ui.close();
                let start = state
                    .range
                    .map(|r| r.start)
                    .filter(|&start| start < here)
                    .unwrap_or(Tick::ZERO);
                if let Some(range) = sections::Range::new(start, here) {
                    select_range(state, range);
                }
            }
            if state.range.is_some() && ui.button("Clear selection").clicked() {
                ui.close();
                state.range = None;
            }
        });
        ui.separator();
        if ui
            .button(format!("Add tempo change at bar {}", bar + 1))
            .clicked()
        {
            ui.close();
            let at = map.bar_start(bar);
            edits.extend(
                crate::tempo::with_change(map, at, map.bpm_at(at))
                    .filter(|(new, _)| new != map)
                    .map(|(new, _)| Edit::Apply(Command::SetTempoMap(new))),
            );
        }
        for (numerator, denominator) in crate::tempo::SIGNATURES {
            let signature = noodle_core::TimeSignature {
                numerator,
                denominator,
            };
            if ui
                .button(format!("Time signature {numerator}/{denominator}"))
                .clicked()
            {
                ui.close();
                edits.extend(
                    crate::tempo::with_signature_at(map, bar, signature)
                        .map(|new| Edit::Apply(Command::SetTempoMap(new))),
                );
            }
        }
        if bar > 0
            && map.signatures().iter().any(|s| s.bar == bar)
            && ui.button("Remove time signature change").clicked()
        {
            ui.close();
            edits.extend(
                crate::tempo::without_signature_at(map, bar)
                    .map(|new| Edit::Apply(Command::SetTempoMap(new))),
            );
        }
    });
    // Only a new tick is worth seeking to: a held button, or a drag that stays
    // within a beat, would otherwise restart the audio's fade every frame.
    let seek = wanted.filter(|&tick| state.last_seek != Some(tick));
    let seek = jump.or(seek);
    state.last_seek = wanted;
    if let Some(tick) = playhead {
        let x = axis.x(tick);
        if x >= content.left() && x <= content.right() {
            ui.painter_at(Rect::from_min_max(
                Pos2::new(content.left(), rect.top()),
                rect.max,
            ))
            .vline(x, rect.y_range(), Stroke::new(1.5, colors::PLAYHEAD));
        }
    }
    let dropped: Vec<std::path::PathBuf> = ui.input(|i| {
        let files = i.raw.dropped_files.iter();
        files.map(|file| file.path().to_path_buf()).collect()
    });
    if !dropped.is_empty()
        && let Some(pos) = ui
            .input(|i| i.pointer.latest_pos())
            .filter(|p| content.contains(*p))
    {
        let y = pos.y - content.top() + state.scroll_y;
        let tick = axis.tick(pos.x).max(Tick::ZERO);
        let at = if ui.input(|i| i.modifiers.alt) {
            tick
        } else {
            axis.snap(tick)
        };
        if tracks.is_empty() || y >= rows.total() {
            // Dropped on the empty space under the tracks: a new track for it.
            let (new_track, track) =
                add_track::with_input(project.graph(), || session.new_node_id());
            let added = import::import(session, &dropped, track, at);
            edits.extend(
                added
                    .command
                    .map(|clips| Edit::Apply(Command::Batch(vec![new_track, clips]))),
            );
            notice = added.notice;
        } else {
            let added = import::import(session, &dropped, tracks[rows.track_at(y)], at);
            edits.extend(added.command.map(Edit::Apply));
            notice = added.notice;
        }
    }

    let corner = Rect::from_min_size(
        rect.min + vec2(6.0, 3.0),
        vec2(colors::HEADER_WIDTH - 12.0, colors::RULER_HEIGHT - 6.0),
    );
    let mut pick = None;
    let set_loop = ruler_loop.or(loop_controls(ui, corner, map, session.loop_region()));
    let import = |state: &TimelineState, notice: &mut Option<String>| {
        let target = state.import_target(project, playhead.unwrap_or(Tick::ZERO));
        if target.is_none() {
            *notice = Some("Add a track before importing audio".to_string());
        }
        target
    };
    // Right-click on empty lane space offers it there.
    if background.secondary_clicked() {
        state.menu_at = background.interact_pointer_pos();
    }
    background.context_menu(|ui| {
        if ui.button("New MIDI clip").clicked() {
            ui.close();
            let at = state
                .menu_at
                .filter(|p| content.contains(*p) && !tracks.is_empty());
            let (track, tick) = match at {
                Some(at) => (
                    tracks[rows.track_at(at.y - content.top() + state.scroll_y)],
                    axis.snap(axis.tick(at.x).max(Tick::ZERO)),
                ),
                None => match state.import_target(project, playhead.unwrap_or(Tick::ZERO)) {
                    Some(target) => (target.track, target.at),
                    None => {
                        notice = Some("Add a track before adding a MIDI clip".to_string());
                        return;
                    }
                },
            };
            let id = project.next_clip_id();
            state.selected = BTreeSet::from([id]);
            edits.push(Edit::Apply(Command::AddClip {
                id,
                clip: Clip::midi(track, tick, Tick(NEW_MIDI_CLIP)),
            }));
            open_midi = Some(id);
        }
        let paste_here = ui.add_enabled(state.can_paste(), egui::Button::new("Paste"));
        if paste_here.clicked() {
            ui.close();
            let at = state.menu_at.filter(|p| content.contains(*p));
            let (tick, track) = match at {
                Some(at) if !tracks.is_empty() => (
                    axis.snap(axis.tick(at.x).max(Tick::ZERO)),
                    Some(tracks[rows.track_at(at.y - content.top() + state.scroll_y)]),
                ),
                _ => (playhead.unwrap_or(Tick::ZERO), None),
            };
            let clips = crate::clipboard::paste::<Copied>(crate::clipboard::CLIP_MIME);
            let (command, pasted) = paste(project, &clips.unwrap_or_default(), tick, track);
            if let Some(command) = command {
                state.selected = pasted;
                edits.push(Edit::Apply(command));
            }
        }
        if ui.button("Import audio…").clicked() {
            ui.close();
            let on_lane = state.menu_at.filter(|p| content.contains(*p));
            pick = match on_lane {
                Some(at) if !tracks.is_empty() => {
                    let tick = axis.tick(at.x).max(Tick::ZERO);
                    let tick = if ui.input(|i| i.modifiers.alt) {
                        tick
                    } else {
                        axis.snap(tick)
                    };
                    Some(Target {
                        track: tracks[rows.track_at(at.y - content.top() + state.scroll_y)],
                        at: tick,
                    })
                }
                _ => import(state, &mut notice),
            };
        }
    });
    state.warped_waves.end_frame();
    // An edit shows in the next frame, which nothing else would ask for once
    // the pointer stops: the clip would sit behind the mouse.
    if !edits.is_empty() || state.band.is_some() {
        ui.ctx().request_repaint();
    }
    Output {
        edits,
        seek,
        notice,
        pick,
        set_loop,
        arm,
        open_midi,
        open_audio,
        audition,
    }
}

/// The first tick of 1-based `bar`, through the signature changes.
/// The colour of markers.
const MARKER_COLOUR: egui::Color32 = egui::Color32::from_rgb(235, 180, 60);

/// Selects `range` across all tracks, dropping any clip or point selection.
fn select_range(state: &mut TimelineState, range: sections::Range) {
    state.selected.clear();
    state.selected_track = None;
    state.automation.deselect();
    state.tempo.deselect();
    state.range = Some(range);
}

fn bar_tick(map: &noodle_core::TempoMap, bar: u32) -> Tick {
    let target = bar.saturating_sub(1);
    let signatures = map.signatures();
    let mut tick = 0;
    for (i, change) in signatures.iter().enumerate() {
        let end = signatures.get(i + 1).map_or(u32::MAX, |next| next.bar);
        let bars = target.min(end).saturating_sub(change.bar);
        tick += i64::from(bars) * change.signature.ticks_per_bar();
        if target < end {
            break;
        }
    }
    Tick(tick)
}

/// The loop toggle and its start and end bars, in the ruler's corner.
/// Returns the new loop if they changed it.
fn loop_controls(
    ui: &mut egui::Ui,
    corner: Rect,
    map: &noodle_core::TempoMap,
    current: crate::session::LoopRegion,
) -> Option<crate::session::LoopRegion> {
    let mut region = current;
    let bar_of = |tick: Tick| map.position(tick).bar + 1;
    let (mut start, mut end) = (bar_of(current.start), bar_of(current.end));
    ui.scope_builder(egui::UiBuilder::new().max_rect(corner), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let toggle = egui::Button::new("⟲").small().selected(current.on);
            if ui
                .add(toggle)
                .on_hover_text("Loop the transport between the two bars")
                .clicked()
            {
                region.on = !current.on;
            }
            fn field(value: &mut u32, min: u32) -> egui::DragValue<'_> {
                egui::DragValue::new(value).range(min..=9999).speed(0.1)
            }
            if ui
                .add(field(&mut start, 1))
                .on_hover_text("Loop start (bar)")
                .changed()
            {
                end = end.max(start + 1);
            }
            ui.label("–");
            ui.add(field(&mut end, 2)).on_hover_text("Loop end (bar)");
        });
    });
    region.start = bar_tick(map, start);
    region.end = bar_tick(map, end.max(start + 1));
    (region != current).then_some(region)
}

/// Scrolling and zooming, while the pointer is over the arrangement.
/// `extent` is in seconds.
fn navigate(ui: &egui::Ui, state: &mut TimelineState, content: Rect, extent: f32) {
    let (scroll, zoom, pointer, alt) = ui.input(|i| {
        (
            i.smooth_scroll_delta,
            i.zoom_delta(),
            i.pointer.hover_pos(),
            i.modifiers.alt,
        )
    });
    if alt && scroll.y != 0.0 {
        // Vertical zoom: keep the row under the pointer where it is.
        let anchor_y = pointer.map_or(content.top(), |p| p.y.max(content.top()));
        let at = (anchor_y - content.top() + state.scroll_y) / state.v_zoom;
        state.v_zoom = (state.v_zoom * (scroll.y / 200.0).exp()).clamp(MIN_V_ZOOM, MAX_V_ZOOM);
        state.scroll_y = at * state.v_zoom - (anchor_y - content.top());
        // The scroll area owns vertical scrolling; don't let it also scroll.
        ui.input_mut(|i| i.smooth_scroll_delta.y = 0.0);
    } else if zoom != 1.0 {
        // Keep the time under the pointer where it is.
        let anchor_x = pointer.map_or(content.left(), |p| p.x.max(content.left()));
        let seconds = (anchor_x - content.left() + state.scroll_x) / state.pps;
        state.pps = (state.pps * zoom).clamp(MIN_PPS, MAX_PPS);
        state.scroll_x = seconds * state.pps - (anchor_x - content.left());
    } else {
        // Horizontal scrolling only; the scroll area scrolls vertically.
        state.scroll_x -= scroll.x;
    }
    state.scroll_x = state
        .scroll_x
        .clamp(0.0, (extent * state.pps - content.width() * 0.5).max(0.0));
}

struct Handles<'a> {
    ui: &'a mut egui::Ui,
    state: &'a mut TimelineState,
    map: &'a TempoMap,
    axis: Axis<'a>,
    rate: u32,
    full: Rect,
    visible: Rect,
    project: &'a Project,
}

/// Size of a fade handle, and how far in from the clip's edge it sits when
/// there is no fade.
const HANDLE: f32 = 10.0;

/// How tall the strip on a track header's bottom edge is that drags to resize.
const GRIP: f32 = 6.0;
/// The range of a track's own lane height, before vertical zoom.
/// The least is a lane of just the track's name, with no controls.
const MIN_LANE: f32 = 20.0;
const MAX_LANE: f32 = 400.0;
/// The range of the vertical zoom (Alt+scroll).
const MIN_V_ZOOM: f32 = 0.5;
const MAX_V_ZOOM: f32 = 4.0;

/// Draws a clip's fades and the handles that drag them. Dragging one is an
/// undo group like any other drag.
fn fade_handles(h: Handles<'_>, id: ClipId, clip: &Clip) -> Vec<Edit> {
    let Handles {
        ui,
        state,
        map,
        axis,
        rate,
        full,
        visible,
        project,
    } = h;
    let mut edits = Vec::new();
    let Some(audio) = clip.as_audio() else {
        return edits;
    };
    let Some((fade_in_tick, fade_out_tick)) =
        clips::fade_ticks(map, clip, rate, warp_of(project, clip))
    else {
        return edits;
    };
    let (fade_in_x, fade_out_x) = (axis.x(fade_in_tick), axis.x(fade_out_tick));
    let half = HANDLE / 2.0;
    let painter = ui.painter_at(visible);
    let shade = Color32::BLACK.gamma_multiply(0.35);
    if audio.fade_in > 0 {
        painter.add(egui::Shape::convex_polygon(
            vec![
                full.left_bottom(),
                full.left_top(),
                Pos2::new(fade_in_x, full.top()),
            ],
            shade,
            Stroke::NONE,
        ));
    }
    if audio.fade_out > 0 {
        painter.add(egui::Shape::convex_polygon(
            vec![
                full.right_bottom(),
                full.right_top(),
                Pos2::new(fade_out_x, full.top()),
            ],
            shade,
            Stroke::NONE,
        ));
    }
    let lo = full.left() + half + 1.0;
    let hi = full.right() - half - 1.0;
    for (mode, x) in [
        (Mode::FadeIn, fade_in_x.clamp(lo, hi)),
        (Mode::FadeOut, fade_out_x.clamp(lo, hi)),
    ] {
        let rect =
            Rect::from_center_size(Pos2::new(x, full.top() + half + 1.0), Vec2::splat(HANDLE));
        #[cfg(test)]
        state.fade_handles.insert((id, mode == Mode::FadeIn), rect);
        let response = ui.interact(
            rect.intersect(visible),
            ui.id().with(("fade", id, mode == Mode::FadeIn)),
            Sense::drag(),
        );
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if response.drag_started() {
            state.selected = BTreeSet::from([id]);
            state.drag = Some(Drag {
                mode,
                grabbed: id,
                originals: vec![(id, clip.clone())],
                press_x: x,
                copied: false,
                sources: BTreeMap::new(),
            });
        }
        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(current) = project.clip(id)
        {
            let at = axis.tick(pos.x);
            let changed = match mode {
                Mode::FadeIn => {
                    clips::set_fade_in(map, current, at, rate, warp_of(project, current))
                }
                _ => clips::set_fade_out(map, current, at, rate, warp_of(project, current)),
            };
            if let Some(changed) = changed
                && &changed != current
            {
                edits.push(Edit::Drag(Command::SetClip { id, clip: changed }));
            }
        }
        if response.drag_stopped() {
            state.drag = None;
            edits.push(Edit::EndDrag);
        }
        let fill = if response.hovered() || response.dragged() {
            Color32::WHITE
        } else {
            Color32::WHITE.gamma_multiply(0.7)
        };
        painter.rect_filled(rect, 2.0, fill);
    }
    edits
}

/// Where to draw a warped section's render within `full`, its rectangle. The
/// render of the section as it is fills it. While an edit is rendered, the
/// last render stays where it was worked out: a section resized meanwhile
/// shows more or less of it, not all of it squeezed into the new width.
fn warped_area(axis: Axis, full: Rect, shown: &warped_waves::Shown) -> Rect {
    if shown.current {
        return full;
    }
    Rect::from_min_max(
        Pos2::new(axis.x(shown.span.0), full.top()),
        Pos2::new(axis.x(shown.span.1), full.bottom()),
    )
}

/// Clips as copied: with the warps they use, so they paste into another
/// project too.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Copied {
    clips: Vec<Clip>,
    warps: Vec<(WarpId, ClipWarp)>,
}

impl Copied {
    fn of<'a>(project: &Project, clips: impl Iterator<Item = &'a Clip>) -> Self {
        let clips: Vec<Clip> = clips.cloned().collect();
        let mut warps: Vec<(WarpId, ClipWarp)> = Vec::new();
        for id in clips.iter().filter_map(|c| c.as_audio()?.warp) {
            if let Some(warp) = project.warp(id)
                && warps.iter().all(|(w, _)| *w != id)
            {
                warps.push((id, warp.clone()));
            }
        }
        Self { clips, warps }
    }
}

/// The command that puts copies of `clips` with the earliest at `at`, and the
/// IDs of the copies. They keep their tracks, unless they all came from one and
/// `track` says where to put them. A track that is gone drops its clips.
fn paste(
    project: &Project,
    copied: &Copied,
    at: Tick,
    track: Option<NodeId>,
) -> (Option<Command>, BTreeSet<ClipId>) {
    let clips = &copied.clips;
    let Some(first) = clips.iter().map(|clip| clip.start.0).min() else {
        return (None, BTreeSet::new());
    };
    let one_track = clips.iter().all(|clip| clip.node == clips[0].node);
    let mut next = project.next_clip_id();
    let mut next_warp = project.next_warp_id();
    // Warps the copied clips used that this project doesn't have (the clips
    // came from another one), added once each under new IDs.
    let mut brought: BTreeMap<WarpId, WarpId> = BTreeMap::new();
    let mut commands = Vec::new();
    let mut pasted = BTreeSet::new();
    for clip in clips {
        let node = track.filter(|_| one_track).unwrap_or(clip.node);
        // Another project's track ID may name something else here.
        if project
            .graph()
            .node(node)
            .is_none_or(|n| n.type_id != TRACK_INPUT && project.clips_on(node).next().is_none())
        {
            continue;
        }
        let mut copy = clips::moved(clip, Tick(at.0 + clip.start.0 - first));
        copy.node = node;
        if let noodle_core::ClipContent::Audio(audio) = &mut copy.content
            && let Some(id) = audio.warp
            && project.warp(id).is_none()
        {
            audio.warp = match (
                brought.get(&id),
                copied.warps.iter().find(|(w, _)| *w == id),
            ) {
                (Some(&known), _) => Some(known),
                (None, Some((_, warp))) => {
                    let fresh = next_warp;
                    next_warp.0 += 1;
                    brought.insert(id, fresh);
                    commands.push(Command::AddWarp {
                        id: fresh,
                        warp: warp.clone(),
                    });
                    Some(fresh)
                }
                (None, None) => None,
            };
        }
        commands.push(Command::AddClip {
            id: next,
            clip: copy,
        });
        pasted.insert(next);
        next.0 += 1;
    }
    let command = (!commands.is_empty()).then_some(Command::Batch(commands));
    (command, pasted)
}

/// Selects the clips a box dragged over empty space touches; Shift keeps what
/// was selected before.
fn select_by_box(
    ui: &egui::Ui,
    state: &mut TimelineState,
    background: &egui::Response,
    content: Rect,
    drawn: &[(ClipId, Rect)],
    scroll_x: f32,
    scroll_y: f32,
) {
    let local = |p: Pos2| {
        vec2(
            p.x - content.left() + scroll_x,
            p.y - content.top() + scroll_y,
        )
    };
    let screen = |v: Vec2| {
        Pos2::new(
            v.x + content.left() - scroll_x,
            v.y + content.top() - scroll_y,
        )
    };
    if background.drag_started()
        && let Some(press) = ui
            .input(|i| i.pointer.press_origin())
            .filter(|p| content.contains(*p))
    {
        let base = if ui.input(|i| i.modifiers.shift) {
            state.selected.clone()
        } else {
            BTreeSet::new()
        };
        state.band = Some(Band {
            from: local(press),
            to: local(press),
            base,
        });
    }
    let Some(band) = state.band.as_mut() else {
        return;
    };
    if background.drag_stopped() || !ui.input(|i| i.pointer.any_down()) {
        state.band = None;
        return;
    }
    if let Some(pos) = background.interact_pointer_pos() {
        band.to = local(pos);
    }
    let rect = Rect::from_two_pos(screen(band.from), screen(band.to));
    let mut chosen = band.base.clone();
    chosen.extend(
        drawn
            .iter()
            .filter(|(_, clip)| clip.intersects(rect))
            .map(|(id, _)| *id),
    );
    let painter = ui.painter_at(content);
    painter.rect_filled(rect, 0.0, colors::SELECTED.gamma_multiply(0.15));
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, colors::SELECTED),
        StrokeKind::Inside,
    );
    state.selected = chosen;
}

/// Copies of dragged clips, and the edits that add them.
struct Copies {
    adds: Vec<Edit>,
    copies: Vec<(ClipId, Clip)>,
    /// The copy of the grabbed clip.
    grabbed: ClipId,
}

fn copies_of(project: &Project, originals: &[(ClipId, Clip)], grabbed: ClipId) -> Copies {
    let mut next = project.next_clip_id();
    let mut copies = Copies {
        adds: Vec::new(),
        copies: Vec::new(),
        grabbed,
    };
    for (id, clip) in originals {
        if *id == grabbed {
            copies.grabbed = next;
        }
        copies.adds.push(Edit::Drag(Command::AddClip {
            id: next,
            clip: clip.clone(),
        }));
        copies.copies.push((next, clip.clone()));
        next.0 += 1;
    }
    copies
}

/// Whether a press at `x` on a clip spanning `full` moves it or trims an edge.
fn mode_for(full: Rect, x: f32) -> Mode {
    if full.width() < MIN_TRIMMABLE {
        Mode::Move
    } else if x - full.left() < EDGE {
        Mode::TrimStart
    } else if full.right() - x < EDGE {
        Mode::TrimEnd
    } else {
        Mode::Move
    }
}

/// Where the pointer is, and what it's over, mid-drag.
struct DragInput<'a> {
    map: &'a TempoMap,
    axis: Axis<'a>,
    /// The pointer's x.
    x: f32,
    /// Alt: don't snap to beats.
    free: bool,
    /// The track under the pointer.
    target: NodeId,
    /// The grabbed clip's file, if it could be read.
    source: Option<Source>,
}

/// The command that puts the dragged clips where the pointer says, if that's
/// different from where they are.
fn drag_command(drag: &Drag, input: DragInput<'_>, project: &Project) -> Option<Command> {
    let DragInput {
        map,
        axis,
        x,
        free,
        target,
        source,
    } = input;
    let snap = |tick: Tick| if free { tick } else { axis.snap(tick) };
    let delta = axis.tick(x).0 - axis.tick(drag.press_x).0;
    let (_, grabbed) = drag.originals.iter().find(|(id, _)| *id == drag.grabbed)?;
    let rate = source.map_or(FALLBACK_RATE, |s| s.sample_rate);
    // What a member knows about its file; the grabbed clip's came with the
    // drag, and a member that isn't known is taken to be like it.
    let member_source = |id: ClipId, grabbed_source: Option<Source>| {
        drag.sources.get(&id).copied().or(grabbed_source)
    };
    let commands: Vec<Command> = match drag.mode {
        Mode::Move => {
            let earliest = drag.originals.iter().map(|(_, c)| c.start.0).min()?;
            let wanted = snap(Tick(grabbed.start.0 + delta)).0 - grabbed.start.0;
            let shift = wanted.max(-earliest);
            // Dragging between lanes only makes sense for one clip.
            let single = drag.originals.len() == 1;
            drag.originals
                .iter()
                .map(|(id, original)| {
                    let mut clip = clips::moved(original, Tick(original.start.0 + shift));
                    if single {
                        clip.node = target;
                    }
                    Command::SetClip { id: *id, clip }
                })
                .collect()
        }
        // A trim or slip reaches every clip in the drag: the grabbed one and
        // those linked to it, each worked out against its own file and moved
        // by as much as the grabbed one is.
        Mode::TrimStart => {
            let by = snap(Tick(grabbed.start.0 + delta)).0 - grabbed.start.0;
            drag.originals
                .iter()
                .filter_map(|(id, original)| {
                    let rate = member_source(*id, source).map_or(FALLBACK_RATE, |s| s.sample_rate);
                    let edge = Tick(original.start.0 + by);
                    let warp = warp_of(project, original);
                    let clip = clips::trim_start(map, original, edge, rate, warp)?;
                    Some(Command::SetClip { id: *id, clip })
                })
                .collect()
        }
        Mode::FadeIn | Mode::FadeOut => return None,
        Mode::Slip => drag
            .originals
            .iter()
            .filter_map(|(id, original)| {
                let warp = warp_of(project, original);
                let found = member_source(*id, source);
                let rate = found.map_or(FALLBACK_RATE, |s| s.sample_rate);
                let frames = found.and_then(|s| s.frames);
                // Unwarped content plays in real time, so measure the
                // pointer's seconds from the clip start; warped content
                // follows beats.
                let slip_delta = if warp.is_some() {
                    delta
                } else {
                    axis.tick(axis.x(original.start) + x - drag.press_x).0 - original.start.0
                };
                let clip = clips::slip(map, original, slip_delta, rate, frames, warp)?;
                Some(Command::SetClip { id: *id, clip })
            })
            .collect(),
        Mode::TrimEnd => {
            let grabbed_end = clips::clip_end(map, grabbed, rate, warp_of(project, grabbed));
            let by = snap(Tick(grabbed_end.0 + delta)).0 - grabbed_end.0;
            drag.originals
                .iter()
                .filter_map(|(id, original)| {
                    let found = member_source(*id, source);
                    let rate = found.map_or(FALLBACK_RATE, |s| s.sample_rate);
                    let frames = found.and_then(|s| s.frames);
                    let warp = warp_of(project, original);
                    let end = clips::clip_end(map, original, rate, warp);
                    let clip =
                        clips::trim_end(map, original, Tick(end.0 + by), rate, frames, warp)?;
                    Some(Command::SetClip { id: *id, clip })
                })
                .collect()
        }
    };
    let changed: Vec<Command> = commands
        .into_iter()
        .filter(|command| match command {
            Command::SetClip { id, clip } => project.clip(*id) != Some(clip),
            _ => true,
        })
        .collect();
    match changed.len() {
        0 => None,
        1 => changed.into_iter().next(),
        _ => Some(Command::Batch(changed)),
    }
}

/// Moving `moved` to position `slot` among the visible `tracks` (counted
/// before it was taken out), as a new track order, and the folder it lands
/// in. `None` when that leaves both as they were.
///
/// It joins the folder both of its new neighbours are in, and keeps its own
/// while either is in it, so reordering inside a folder, even to its ends,
/// does not take a track out. Anything else leaves it in no folder; the
/// header's menu takes a track out of a folder as well.
fn reorder(project: &Project, tracks: &[NodeId], moved: NodeId, slot: usize) -> Option<Edit> {
    // A track is ordered by its group, or by itself if it has none (see
    // `tracks`). The order covers every track, folded away or not.
    let all = self::tracks(project);
    let key = |input: NodeId| group_of(project, input);
    let from = tracks.iter().position(|&t| t == moved)?;
    let slot = slot.saturating_sub(usize::from(slot > from));
    let others: Vec<NodeId> = tracks.iter().copied().filter(|&t| t != moved).collect();
    // The neighbours it lands between, among the visible ones.
    let above = slot.checked_sub(1).and_then(|i| others.get(i)).copied();
    let below = others.get(slot).copied();
    let before: Vec<NodeId> = all.iter().map(|&t| key(t)).collect();
    let mut order = before.clone();
    let at = order.iter().position(|&g| g == key(moved))?;
    let group = order.remove(at);
    // Just before the track now below it, or after the one above it.
    let place = match (below, above) {
        (Some(below), _) => order.iter().position(|&g| g == key(below)),
        (None, Some(above)) => order.iter().position(|&g| g == key(above)).map(|i| i + 1),
        (None, None) => Some(0),
    }
    .unwrap_or(order.len());
    order.insert(place, group);
    let org = project.organisation();
    let folder = |t: Option<NodeId>| t.and_then(|t| folder_of(project, t));
    let own = folder_of(project, moved);
    let landing = match (folder(above), folder(below)) {
        (Some(a), Some(b)) if a == b => Some(a),
        (a, b) if own.is_some() && (a == own || b == own) => own,
        _ => None,
    };
    let new_org = org.clone().with_folder(&[group], landing);
    let mut commands = Vec::new();
    if order != before {
        commands.push(Command::SetTrackOrder(order));
    }
    if &new_org != org {
        commands.push(Command::SetOrganisation(new_org));
    }
    match commands.len() {
        0 => None,
        1 => commands.pop().map(Edit::Apply),
        _ => Some(Edit::Apply(Command::Batch(commands))),
    }
}

/// The lanes on screen, for the header column.
#[derive(Clone, Copy)]
struct Lanes<'a> {
    /// The visible tracks, top to bottom.
    tracks: &'a [NodeId],
    /// Every track, folded away or not.
    all: &'a [NodeId],
    strips: &'a [Strip],
    colours: &'a [Color32],
}

#[allow(clippy::too_many_arguments)]
fn draw_headers(
    ui: &mut egui::Ui,
    rect: Rect,
    content: Rect,
    lanes: Lanes<'_>,
    rows: &automation::Rows,
    session: &Session,
    state: &mut TimelineState,
    scroll_y: f32,
) -> (Vec<Edit>, Vec<(NodeId, ArmKind, bool)>) {
    let Lanes {
        tracks,
        all,
        strips,
        colours,
    } = lanes;
    let graph = session.project().graph();
    let column = Rect::from_min_max(
        Pos2::new(rect.left(), content.top()),
        Pos2::new(content.left(), rect.bottom()),
    );
    ui.painter_at(column)
        .rect_filled(column, 0.0, colors::HEADER);
    let mut edits = Vec::new();
    let mut arm = Vec::new();
    // A name being typed belongs to a header; if that header isn't drawn this
    // frame (scrolled away, or its track is gone) the rename is dropped, so it
    // can't wait unseen and swallow a later click.
    let renaming_group = state.renaming.as_ref().map(|r| r.group);
    let mut renaming_seen = false;
    let muted_by_solo = graph.solo_muted();
    let clip = ui.clip_rect();
    ui.set_clip_rect(column.intersect(clip));
    // Each track's block (its lane and its automation rows) on screen, for
    // working out where a dragged header lands.
    let blocks: Vec<(f32, f32)> = (0..tracks.len())
        .map(|i| {
            let top = content.top() - scroll_y + rows.top(i);
            (top, top + rows.height(i))
        })
        .collect();
    let mut drop = None;
    let mut grips: Vec<(NodeId, Rect)> = Vec::new();
    for (index, &input) in tracks.iter().enumerate() {
        let top = content.top() - scroll_y + rows.top(index);
        let lane = Rect::from_min_size(
            Pos2::new(column.left(), top),
            vec2(colors::HEADER_WIDTH, rows.lane_height(index)),
        );
        let block_bottom = top + rows.height(index);
        if block_bottom < column.top() || lane.top() > column.bottom() {
            continue;
        }
        renaming_seen |=
            renaming_group.is_some() && graph.node(input).and_then(|n| n.parent) == renaming_group;
        let controls =
            header::controls_for(graph, input, &muted_by_solo).map(|t| header::TrackControls {
                armed_audio: session.is_armed(input, ArmKind::Audio),
                armed_midi: session.is_armed(input, ArmKind::Midi),
                ..t
            });
        let automated = controls
            .as_ref()
            .map_or_else(header::Automated::default, |t| {
                let driven = |(node, key): &(NodeId, String)| {
                    let input = Endpoint::new(*node, key.as_str());
                    session.project().lane_for(&input).is_some() || graph.source(&input).is_some()
                };
                header::Automated {
                    mute: driven(&t.mute_target),
                }
            });
        let changes = header::show(
            ui,
            graph,
            header::Lane {
                rect: lane,
                index: all.iter().position(|&t| t == input).unwrap_or(index),
                input,
                selected: state.selected_track == Some(input),
                colour: colours[index],
            },
            controls.clone(),
            automated,
            &mut state.renaming,
            session.project().organisation(),
        );
        grips.push((input, lane));
        edits.extend(changes.edits);
        arm.extend(changes.arm_audio.map(|on| (input, ArmKind::Audio, on)));
        arm.extend(changes.arm_midi.map(|on| (input, ArmKind::Midi, on)));
        if let Some(drag) = changes.drag {
            drop = Some((input, drag));
        }
        if changes.select {
            state.selected_track = Some(input);
            state.selected.clear();
            state.automation.deselect();
        }
        if let Some(controls) = controls
            .as_ref()
            .filter(|_| lane.height() >= header::CONTROLS_MIN)
        {
            let spot = Rect::from_min_size(
                Pos2::new(lane.right() - 30.0, lane.top() + 2.0),
                vec2(26.0, 18.0),
            );
            edits.extend(automation::add_menu(
                ui,
                spot,
                session.project(),
                &controls.mute_target,
                controls.controls,
            ));
        }
    }
    // Each lane's bottom edge drags to resize the track, and double-clicking
    // it shrinks the track to its name. The grip straddles the edge, and is
    // made after every header so the next header's top can't cover it.
    for (input, lane) in grips {
        let grip = Rect::from_min_max(
            Pos2::new(lane.left(), lane.bottom() - GRIP),
            Pos2::new(lane.right(), lane.bottom() + GRIP / 2.0),
        );
        let resize = ui.interact(
            grip,
            ui.id().with(("resize", input)),
            Sense::click_and_drag(),
        );
        if resize.hovered() || resize.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if resize.dragged() {
            state.resize_track(input, resize.drag_delta().y);
        }
        if resize
            .on_hover_text("Drag to resize. Double-click to shrink to just the name.")
            .double_clicked()
        {
            state.toggle_name_only(input);
        }
    }
    for (k, strip) in strips.iter().enumerate() {
        let top = content.top() - scroll_y + rows.strip_top(k);
        let row = Rect::from_min_size(
            Pos2::new(column.left(), top),
            vec2(colors::HEADER_WIDTH, automation::STRIP),
        );
        if row.bottom() < column.top() || row.top() > column.bottom() {
            continue;
        }
        edits.extend(folder_strip(ui, session.project(), *strip, row, state));
    }
    let project = session.project();
    let mut lane_rows: Vec<(LaneId, f32)> = Vec::new();
    for index in 0..tracks.len() {
        for (k, &id) in rows.lanes(index).iter().enumerate() {
            lane_rows.push((id, rows.lane_top(index, k)));
        }
    }
    for (k, &id) in rows.free_lanes().iter().enumerate() {
        lane_rows.push((id, rows.free_lane_top(k)));
    }
    for (id, lane_top) in lane_rows {
        let Some(lane) = project.lane(id) else {
            continue;
        };
        let row = Rect::from_min_size(
            Pos2::new(column.left(), content.top() - scroll_y + lane_top),
            vec2(colors::HEADER_WIDTH, automation::HEIGHT),
        );
        let refused = automation::refused(lane);
        let label = automation::label(project, lane);
        // Removing an Automation node's lane removes the node too, so the
        // graph and the sequencer don't disagree. Removing a node takes its
        // lanes with it.
        let source =
            automation::is_automation_node(project, lane.target.node).then_some(lane.target.node);
        let arm = source.map(|node| automation::Arm {
            node,
            armed: session.automation_armed(node),
            value: session.automation_value(node).unwrap_or(0.0),
        });
        edits.extend(
            automation::header(ui, row, id, &label, refused, arm)
                .into_iter()
                .map(|edit| match (edit, source) {
                    (Edit::Apply(Command::RemoveLane { .. }), Some(node)) => {
                        Edit::Apply(Command::RemoveNode { id: node })
                    }
                    (edit, _) => edit,
                }),
        );
    }
    if renaming_group.is_some() && !renaming_seen {
        state.renaming = None;
    }
    if let Some((input, drag)) = drop {
        let slot = blocks
            .iter()
            .filter(|(top, bottom)| (top + bottom) / 2.0 < drag.y)
            .count();
        let line_y = blocks
            .get(slot)
            .map_or_else(|| blocks.last().map_or(content.top(), |b| b.1), |b| b.0);
        let pointer = Pos2::new(drag.x, drag.y);
        let group = session.project().graph().node(input).and_then(|n| n.parent);
        if let Some(group) = group.filter(|_| !rect.contains(pointer)) {
            // Carried out of the timeline, to the graph.
            if drag.released {
                crate::carry::release(ui.ctx(), crate::carry::Item::Track(group), pointer);
            } else {
                crate::carry::carrying(
                    ui.ctx(),
                    crate::carry::Item::Track(group),
                    pointer,
                    "Track",
                );
            }
        } else if drag.released {
            edits.extend(reorder(session.project(), tracks, input, slot));
        } else {
            ui.painter_at(column).hline(
                column.x_range(),
                line_y,
                Stroke::new(2.0, colors::SELECTED),
            );
        }
    }
    // The button sits in the lane after the last track, so it scrolls with them.
    let top = content.top() - scroll_y + rows.total();
    let spot = Rect::from_min_size(
        Pos2::new(column.left(), top),
        vec2(colors::HEADER_WIDTH, colors::LANE_HEIGHT),
    );
    if spot.bottom() >= column.top() && spot.top() <= column.bottom() {
        let place = Rect::from_center_size(spot.center(), vec2(spot.width() - 24.0, 24.0));
        let (track, automation) = {
            let half = (place.width() - 4.0) / 2.0;
            let left = Rect::from_min_size(place.min, vec2(half, place.height()));
            let right = Rect::from_min_size(
                place.min + vec2(half + 4.0, 0.0),
                vec2(half, place.height()),
            );
            (left, right)
        };
        // In a child, so a button that is partly scrolled out of the panel
        // doesn't grow the panel to fit it.
        let mut buttons = ui.new_child(egui::UiBuilder::new().max_rect(spot));
        buttons.set_clip_rect(spot.intersect(column).intersect(ui.clip_rect()));
        if buttons.put(track, egui::Button::new("+ Track")).clicked() {
            let command = add_track::command(graph, || session.new_node_id());
            edits.push(Edit::Apply(command));
        }
        if buttons
            .put(automation, egui::Button::new("+ Auto"))
            .on_hover_text(
                "Adds an Automation node and a lane for it. Wire its output to any input.",
            )
            .clicked()
        {
            let project = session.project();
            let command =
                add_track::automation_command(graph, session.new_node_id(), project.next_lane_id());
            edits.push(Edit::Apply(command));
        }
    }
    ui.set_clip_rect(clip);
    (edits, arm)
}

/// Draws a folder's header strip: its fold arrow, colour and name.
fn folder_strip(
    ui: &mut egui::Ui,
    project: &Project,
    strip: Strip,
    row: Rect,
    state: &mut TimelineState,
) -> Vec<Edit> {
    let organisation = project.organisation();
    let Some(folder) = organisation.folders.get(&strip.folder) else {
        return Vec::new();
    };
    let id = strip.folder;
    let mut edits = Vec::new();
    let apply = |organisation: noodle_core::Organisation| {
        Edit::Apply(Command::SetOrganisation(organisation))
    };
    let colour = folder.colour.map_or(colors::TEXT_WEAK, |slot| {
        colors::track_colour(usize::from(slot))
    });
    let painter = ui.painter_at(row);
    painter.rect_filled(row, 0.0, colors::AUTOMATION_HEADER);
    painter.rect_filled(
        Rect::from_min_size(row.min, vec2(4.0, row.height())),
        0.0,
        colour,
    );
    painter.hline(
        row.x_range(),
        row.bottom() - 0.5,
        Stroke::new(1.0, colors::LANE_EVEN),
    );
    let background = ui.interact(row, ui.id().with(("folder", id)), Sense::click());
    background.context_menu(|ui| {
        if ui.button("Rename").clicked() {
            ui.close();
            state.renaming_folder = Some((id, folder.name.clone()));
        }
        ui.menu_button("Colour", |ui| {
            if let Some(slot) = header::colour_menu(ui, folder.colour) {
                edits.push(apply(organisation.clone().with_folder_colour(id, slot)));
            }
        });
        let all_folded = organisation.folders.values().all(|f| f.folded);
        let label = if all_folded {
            "Unfold all folders"
        } else {
            "Fold all folders"
        };
        if ui.button(label).clicked() {
            ui.close();
            edits.push(apply(organisation.clone().with_all_folded(!all_folded)));
        }
        if ui.button("Ungroup").clicked() {
            ui.close();
            edits.push(apply(organisation.clone().without_folder(id)));
        }
    });
    let arrow = if folder.folded { "▸" } else { "▾" };
    let arrow_rect = Rect::from_min_size(row.min + vec2(6.0, 0.0), vec2(18.0, row.height()));
    let toggle = ui.interact(arrow_rect, ui.id().with(("fold", id)), Sense::click());
    painter.text(
        arrow_rect.center(),
        Align2::CENTER_CENTER,
        arrow,
        FontId::proportional(13.0),
        colors::TEXT,
    );
    if toggle
        .on_hover_text(if folder.folded { "Unfold" } else { "Fold" })
        .clicked()
        || background.clicked() && !matches!(&state.renaming_folder, Some((f, _)) if *f == id)
    {
        edits.push(apply(organisation.clone().with_folded(id, !folder.folded)));
    }
    let name_rect = Rect::from_min_max(
        Pos2::new(row.left() + 26.0, row.top()),
        Pos2::new(row.right() - 4.0, row.bottom()),
    );
    if let Some((f, text)) = &mut state.renaming_folder
        && *f == id
    {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(name_rect));
        let field = child.add(
            egui::TextEdit::singleline(text)
                .desired_width(name_rect.width())
                .id(child.id().with(("rename folder", id))),
        );
        if !field.has_focus() && !field.lost_focus() {
            field.request_focus();
        }
        if field.lost_focus() {
            let cancelled = child.input(|i| i.key_pressed(Key::Escape));
            let name = text.trim().to_owned();
            state.renaming_folder = None;
            if !cancelled && !name.is_empty() && name != folder.name {
                edits.push(apply(organisation.clone().with_folder_name(id, &name)));
            }
        }
    } else {
        painter.text(
            name_rect.left_center(),
            Align2::LEFT_CENTER,
            &folder.name,
            FontId::proportional(12.0),
            colors::TEXT,
        );
    }
    edits
}

/// Draws the ruler, and returns the time signature markers it drew.
fn draw_ruler(ui: &egui::Ui, rect: Rect, axis: Axis<'_>, lines: &[grid::Line]) -> Vec<String> {
    let strip = Rect::from_min_size(rect.min, Vec2::new(rect.width(), colors::RULER_HEIGHT));
    let painter = ui.painter_at(strip);
    painter.rect_filled(strip, 0.0, colors::RULER);
    let left = rect.left() + colors::HEADER_WIDTH;
    let painter = ui.painter_at(Rect::from_min_max(Pos2::new(left, strip.top()), strip.max));
    let mut last_label = f32::NEG_INFINITY;
    let mut last_line = f32::NEG_INFINITY;
    for line in lines {
        let x = axis.x(line.tick);
        let spaced = x - last_line >= 24.0;
        last_line = x;
        match line.kind {
            grid::Kind::Bar(bar) => {
                painter.vline(
                    x,
                    strip.bottom() - 10.0..=strip.bottom(),
                    Stroke::new(1.0, colors::TEXT_WEAK),
                );
                if x - last_label >= 32.0 {
                    painter.text(
                        Pos2::new(x + 4.0, strip.top() + 2.0),
                        Align2::LEFT_TOP,
                        (bar + 1).to_string(),
                        FontId::proportional(11.0),
                        colors::TEXT,
                    );
                    last_label = x;
                }
            }
            grid::Kind::Beat if spaced => {
                painter.vline(
                    x,
                    strip.bottom() - 5.0..=strip.bottom(),
                    Stroke::new(1.0, colors::BAR_LINE),
                );
            }
            grid::Kind::Beat => {}
        }
    }
    // A marker at every time signature change after the first, which the
    // transport shows.
    let mut markers = Vec::new();
    for change in axis.map.signatures().iter().skip(1) {
        let x = axis.x(axis.map.bar_start(change.bar));
        painter.vline(
            x,
            strip.y_range(),
            Stroke::new(1.5, colors::AUTOMATION_LINE),
        );
        painter.text(
            Pos2::new(x + 4.0, strip.bottom() - 2.0),
            Align2::LEFT_BOTTOM,
            change.signature.to_string(),
            FontId::proportional(10.0),
            colors::AUTOMATION_LINE,
        );
        markers.push(change.signature.to_string());
    }
    painter.hline(
        strip.x_range(),
        strip.bottom() - 0.5,
        Stroke::new(1.0, colors::BACKGROUND),
    );
    markers
}

#[cfg(test)]
mod automation_tests;
#[cfg(test)]
mod tempo_lane_tests;
#[cfg(test)]
mod tests;
