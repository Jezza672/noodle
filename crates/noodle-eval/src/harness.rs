//! Warps a probe from one tempo to another and scores what comes out.
//!
//! For each probe and target tempo the harness makes three renders:
//!
//! - the *source*: the probe natively at the source tempo, which is the
//!   "recording" handed to the warp;
//! - the *native* render: the probe natively at the target tempo, which is the
//!   answer key (known truth);
//! - the *warped* audio: the source on a clip in a Noodle project whose
//!   timeline has the target tempo, rendered offline by the real engine.
//!
//! The warp under test is tempo conversion only: no warp markers, no
//! quantise. It is scored three ways, each through a time map built from the
//! known tempo maps and never from Noodle's own warp map:
//!
//! - [`View::Native`]: against the native render, on one timeline;
//! - [`View::Source`]: against the source, as the listening-test dataset does,
//!   through the map from target time to source time;
//! - [`View::RoundTrip`]: warped to the target and back to the source tempo,
//!   against the source.
//!
//! Methods are the headline [`Method::Auto`] (tempo and mode as the app
//! would choose them from the bare audio; with no tempo detected the clip is
//! left at the timeline's, as in the app), the three modes at the true source
//! tempo, and Repitch as the floor, which cannot keep pitch.

use crate::clip::Clip;
use crate::probe::{BEATS, Probe};
use crate::report::evaluate;
use crate::song::{Range, Song};
use crate::tempo::{TempoCase, steady};
use crate::timemap::TimeMap;
use noodle_core::{
    ClipContent, ClipWarp, Command, Connection, Endpoint, History, Node, NodeId, Project,
    TempoCurve, TempoMap, Tick, WarpMode,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};
use noodle_io::{ANALYSIS_SECONDS, Analysis, write_wav};
use noodle_nodes::{TRACK_INPUT_ID, render_project_with_clips};
use std::cell::Cell;
use std::path::PathBuf;

/// How a clip is warped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Method {
    /// The headline: the clip tempo and the mode (Beats for close transients,
    /// Grain for the rest) the app picks from the bare audio. Wrong detection
    /// is part of what is scored.
    Auto,
    Grain,
    Beats,
    /// The floor: plain resampling, so the pitch follows the speed.
    Repitch,
}

impl Method {
    pub const ALL: [Method; 4] = [Method::Auto, Method::Grain, Method::Beats, Method::Repitch];

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Grain => "grain",
            Self::Beats => "beats",
            Self::Repitch => "repitch",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    /// Whether this is a contender; Repitch is the floor.
    pub fn is_contender(self) -> bool {
        self != Self::Repitch
    }
}

/// What a warp was scored against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum View {
    Native,
    Source,
    RoundTrip,
}

impl View {
    pub const ALL: [View; 3] = [View::Native, View::Source, View::RoundTrip];

    pub fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Source => "source",
            Self::RoundTrip => "round-trip",
        }
    }
}

/// One scored warp.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub probe: String,
    pub case: String,
    pub method: Method,
    pub view: View,
    /// [`Report::features`](crate::Report::features), plus `detected_bpm` on
    /// the headline method.
    pub metrics: Vec<(String, f64)>,
}

impl Row {
    /// The row's name in a baseline.
    pub fn key(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.probe,
            self.case,
            self.method.name(),
            self.view.name()
        )
    }

    pub fn metric(&self, name: &str) -> Option<f64> {
        self.metrics
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
    }
}

pub struct Harness {
    pub rate: u32,
    pub source_bpm: f64,
    /// How long the piece is, in quarter notes: what a ramp, a step and a
    /// wobble stretch over, and where the source map ends.
    pub beats: f64,
    dir: tempfile::TempDir,
    files: Cell<usize>,
    /// When set, [`run`](Self::run) also writes every render here as a WAV
    /// to listen to: `<probe>_source.wav`, `<probe>_<case>_native.wav`, and per
    /// method `_<method>_warped.wav` and `_<method>_roundtrip.wav`.
    pub export: Option<PathBuf>,
}

impl Harness {
    pub fn new(rate: u32, source_bpm: f64) -> std::io::Result<Self> {
        Ok(Self {
            rate,
            source_bpm,
            beats: BEATS,
            dir: tempfile::tempdir()?,
            files: Cell::new(0),
            export: None,
        })
    }

    /// The same harness for a piece of `beats` quarter notes (a probe's
    /// [`BEATS`] by default).
    pub fn with_beats(mut self, beats: f64) -> Self {
        self.beats = beats;
        self
    }

    /// The probe at the source tempo.
    pub fn source(&self, probe: Probe) -> Vec<f32> {
        probe.render(&steady(self.source_bpm), self.rate)
    }

    /// The target tempo map of `case`.
    pub fn target(&self, case: TempoCase) -> TempoMap {
        case.map(self.source_bpm, self.beats)
    }

    /// Where target time falls in the source, for [`View::Source`].
    pub fn source_map(&self, case: TempoCase, target: &TempoMap) -> TimeMap {
        if let Some(ratio) = case.ratio() {
            return TimeMap::Speed(ratio);
        }
        let seconds_per_beat = 60.0 / self.source_bpm;
        let end = Tick::from_quarters(self.beats);
        let mut points: Vec<(f64, f64)> = target
            .tempos()
            .iter()
            .map(|c| c.tick)
            .chain([end])
            .map(|tick| (target.seconds_at(tick), tick.quarters() * seconds_per_beat))
            .collect();
        points.dedup_by(|b, a| b.0 <= a.0);
        TimeMap::Points(points)
    }

    /// Scores `methods` on one probe at one target tempo, in every view.
    pub fn run(
        &self,
        probe: Probe,
        case: TempoCase,
        methods: &[Method],
    ) -> Result<Vec<Row>, String> {
        let target = self.target(case);
        let native = probe.render(&target, self.rate);
        self.score(probe.name(), self.source(probe), native, case, methods)
    }

    /// A harness for `range` of `song`: at the song's own tempo, as long as
    /// the range.
    pub fn for_song(song: &Song, range: Range, rate: u32) -> std::io::Result<Self> {
        Ok(Self::new(rate, song.bpm)?.with_beats(range.beats()))
    }

    /// Scores `methods` on `range` of `song` at one target tempo, in every
    /// view, under `name`. The source is the song rendered at its own tempo;
    /// the native render is the same notes rendered at the target tempo map
    /// (known truth).
    pub fn run_song(
        &self,
        name: &str,
        song: &Song,
        range: Range,
        case: TempoCase,
        methods: &[Method],
    ) -> Result<Vec<Row>, String> {
        let (source, native) = self.song_renders(song, range, case)?;
        self.score(name, source, native, case, methods)
    }

    /// Scores `methods` on each part of `song` alone (its stems), as pieces
    /// named `name/part`. A part with no notes in `range` is skipped. Each
    /// stem is analysed on its own, so its detected tempo is what the app
    /// would find on that stem exported as a file.
    pub fn run_stems(
        &self,
        name: &str,
        song: &Song,
        range: Range,
        case: TempoCase,
        methods: &[Method],
    ) -> Result<Vec<Row>, String> {
        let source_map = steady(song.bpm);
        let target = self.target(case);
        let mut rows = Vec::new();
        for part in &song.parts {
            let only = |p: &crate::song::Part| p.name == part.name;
            let source = song.render_of(&source_map, range, self.rate, only)?;
            if source.iter().all(|x| x.abs() < 1e-6) {
                continue;
            }
            let native = song.render_of(&target, range, self.rate, only)?;
            rows.extend(self.score(
                &format!("{name}/{}", part.name),
                source,
                native,
                case,
                methods,
            )?);
        }
        Ok(rows)
    }

    /// The source and the native render of `range` of `song` for `case`.
    pub fn song_renders(
        &self,
        song: &Song,
        range: Range,
        case: TempoCase,
    ) -> Result<(Vec<f32>, Vec<f32>), String> {
        let source = song.render(&steady(song.bpm), range, self.rate)?;
        let native = song.render(&self.target(case), range, self.rate)?;
        Ok((source, native))
    }

    /// Scores `methods` on a piece whose source (at the source tempo) and
    /// native render (at the `case`'s target tempo) the caller has made:
    /// both stereo, `self.beats` long.
    pub fn score(
        &self,
        name: &str,
        source: Vec<f32>,
        native: Vec<f32>,
        case: TempoCase,
        methods: &[Method],
    ) -> Result<Vec<Row>, String> {
        let rate = self.rate;
        let target = self.target(case);
        let frames = native.len() / 2;
        let source_map = self.source_map(case, &target);
        let analysis = analyse(&source, rate);
        let seconds = frames as f64 / f64::from(rate);
        // Export files are named for the piece and the tempo case, so a full
        // run keeps one set per case.
        let tag = format!("{name}_{}", case.name());
        self.export_wav(&format!("{name}_source"), &source)?;
        self.export_wav(&format!("{tag}_native"), &native)?;
        let mut rows = Vec::new();
        for &method in methods {
            let (tempo, mode) = self.choice(method, &analysis);
            let warped = self
                .warp(&source, tempo, mode, &target, frames)
                .map_err(|e| format!("{name} {} {}: {e}", case.name(), method.name()))?;
            self.export_wav(&format!("{tag}_{}_warped", method.name()), &warped)?;
            // Back to the source tempo: the clip's tempo is the target's.
            let back_tempo = TempoCurve::from_tempo_map(&target, Tick::ZERO, 0.0, seconds);
            let back = self
                .warp(
                    &warped,
                    back_tempo,
                    mode,
                    &steady(self.source_bpm),
                    source.len() / 2,
                )
                .map_err(|e| {
                    format!("{} {} {} round trip: {e}", name, case.name(), method.name())
                })?;
            self.export_wav(&format!("{tag}_{}_roundtrip", method.name()), &back)?;
            for view in View::ALL {
                let (reference, test, map) = match view {
                    View::Native => (&native, &warped, TimeMap::Identity),
                    View::Source => (&source, &warped, source_map.clone()),
                    View::RoundTrip => (&source, &back, TimeMap::Identity),
                };
                let report = evaluate(
                    Clip::new(reference, 2, rate),
                    Clip::new(test, 2, rate),
                    &map,
                );
                let mut metrics: Vec<(String, f64)> = report
                    .features()
                    .into_iter()
                    .map(|(n, v)| (n.to_string(), v))
                    .collect();
                if method == Method::Auto {
                    metrics.push(("detected_bpm".into(), analysis.tempo.unwrap_or(f64::NAN)));
                }
                rows.push(Row {
                    probe: name.into(),
                    case: case.name(),
                    method,
                    view,
                    metrics,
                });
            }
        }
        Ok(rows)
    }

    /// The clip tempo and mode `method` gives a clip: the true source tempo
    /// for the fixed modes, and for [`Method::Auto`] what `analysis` finds.
    ///
    /// Auto is warped the way a user does it: the clip goes into a project
    /// at the song's own tempo, where a failed detection leaves the clip at
    /// the timeline's tempo, and the project's tempo is changed afterwards.
    /// A clip with no tempo to find (a lone kick, a sweep) is then still
    /// stretched by the tempo change, in the pitch-keeping mode it was given.
    /// `detected_bpm` in the rows says when detection failed.
    pub fn choice(&self, method: Method, analysis: &Analysis) -> (TempoCurve, WarpMode) {
        let fixed = |mode| (TempoCurve::constant(self.source_bpm), mode);
        match method {
            Method::Auto => auto_choice(analysis, &steady(self.source_bpm)),
            Method::Grain => fixed(WarpMode::Grain),
            Method::Beats => fixed(WarpMode::Beats),
            Method::Repitch => fixed(WarpMode::Repitch),
        }
    }

    fn export_wav(&self, name: &str, samples: &[f32]) -> Result<(), String> {
        let Some(dir) = &self.export else {
            return Ok(());
        };
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        write_wav(&dir.join(format!("{name}.wav")), samples, 2, self.rate)
            .map_err(|e| format!("{name}.wav: {e}"))
    }

    /// Plays `samples` (stereo) as a warped clip on a timeline with the
    /// `timeline` tempo map, and renders `frames` of it offline through the
    /// engine. The warp has no markers: it is tempo conversion alone.
    pub fn warp(
        &self,
        samples: &[f32],
        tempo: TempoCurve,
        mode: WarpMode,
        timeline: &TempoMap,
        frames: usize,
    ) -> Result<Vec<f32>, String> {
        let n = self.files.get();
        self.files.set(n + 1);
        let file = format!("clip-{n}.wav");
        write_wav(&self.dir.path().join(&file), samples, 2, self.rate)
            .map_err(|e| e.to_string())?;
        let warp = ClipWarp {
            tempo,
            mode,
            ..ClipWarp::at_tempo(self.source_bpm)
        };
        let project = project(&file, warp, (samples.len() / 2) as u64, timeline)?;
        let settings = Settings {
            sample_rate: self.rate as f32,
            max_frames: 512,
            channels: 2,
        };
        let mut registry = Registry::with_builtins();
        let rendered =
            render_project_with_clips(&project, &mut registry, self.dir.path(), settings, frames)
                .map_err(|e| e.to_string())?;
        if !rendered.render.diagnostics.is_empty() {
            return Err(format!("{:?}", rendered.render.diagnostics));
        }
        if !rendered.problems.is_empty() {
            return Err(format!("{:?}", rendered.problems));
        }
        if rendered.underruns != 0 {
            return Err(format!("{} underruns", rendered.underruns));
        }
        Ok(rendered.render.samples)
    }
}

/// The clip tempo and mode the app gives a freshly warped clip
/// (`clip_panel::start_warp`): the detected tempo, or when there is none the
/// tempo the timeline has as the clip is warped, so the clip is not stretched
/// until the timeline's tempo changes;
/// Beats for close transients and Grain for the rest.
pub fn auto_choice(analysis: &Analysis, timeline: &TempoMap) -> (TempoCurve, WarpMode) {
    let tempo = TempoCurve::constant(
        analysis
            .tempo
            .unwrap_or_else(|| timeline.bpm_at(Tick::ZERO)),
    );
    let mode = if analysis.percussive {
        WarpMode::Beats
    } else {
        WarpMode::Grain
    };
    (tempo, mode)
}

/// What the app learns from a file's audio: the same analysis it runs on
/// import, over the same stretch of the file.
pub fn analyse(stereo: &[f32], rate: u32) -> Analysis {
    let mono: Vec<f32> = stereo
        .as_chunks::<2>()
        .0
        .iter()
        .map(|f| (f[0] + f[1]) / 2.0)
        .take((f64::from(rate) * ANALYSIS_SECONDS) as usize)
        .collect();
    Analysis::of(&mono, rate)
}

fn project(
    file: &str,
    warp: ClipWarp,
    length: u64,
    timeline: &TempoMap,
) -> Result<Project, String> {
    let mut project = Project::new();
    let mut history = History::new();
    let (track, out) = (NodeId(1), NodeId(2));
    let apply = |history: &mut History, project: &mut Project, command| {
        history
            .apply(project, command)
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    for (id, kind) in [(track, TRACK_INPUT_ID), (out, OUTPUT_ID)] {
        apply(
            &mut history,
            &mut project,
            Command::AddNode {
                id,
                node: Node::new(kind),
            },
        )?;
    }
    apply(
        &mut history,
        &mut project,
        Command::Connect(Connection {
            from: Endpoint::new(track, "audio"),
            to: Endpoint::new(out, "in"),
        }),
    )?;
    apply(
        &mut history,
        &mut project,
        Command::SetTempoMap(timeline.clone()),
    )?;
    let id = project.new_clip_id();
    let mut clip = noodle_core::Clip::audio(track, Tick(0), file, length);
    let warp_id = project.next_warp_id();
    let ClipContent::Audio(audio) = &mut clip.content else {
        unreachable!("not an audio clip")
    };
    audio.warp = Some(warp_id);
    apply(
        &mut history,
        &mut project,
        Command::Batch(vec![
            Command::AddWarp { id: warp_id, warp },
            Command::AddClip { id, clip },
        ]),
    )?;
    Ok(project)
}
