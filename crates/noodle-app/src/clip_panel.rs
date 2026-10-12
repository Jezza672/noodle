//! The properties of the selected audio clip, in the properties panel: how it
//! is warped (mode, tempo, markers), its loop and its gain. The warp itself is
//! edited in the clip editor.
//!
//! Every setting is one [`Edit`]: the commands come from
//! [`Project::warp_edit`], which forks a warp that other sections share, so
//! changing this section leaves its copies as they were.

use std::time::{Duration, Instant};

use egui::{Response, RichText, Ui};
use noodle_core::{
    ClipContent, ClipId, ClipWarp, Command, LoopRegion, MAX_CLIP_BPM, MIN_CLIP_BPM, MIN_LOOP_BEATS,
    Project, TempoCurve, WarpMode,
};

use crate::session::{Edit, Session};
use crate::timeline::{Analysis, Loaded};

/// Taps further apart than this start a new count.
const TAP_RESET: Duration = Duration::from_secs(2);
/// The most taps the tempo is averaged over.
const TAPS: usize = 8;

/// What the panel remembers between frames.
#[derive(Default)]
pub struct ClipPanel {
    taps: Vec<Instant>,
}

/// The tempo the taps at `taps` keep, in beats per minute, once there are
/// enough of them (three).
pub fn tap_tempo(taps: &[Instant]) -> Option<f64> {
    let taps = &taps[taps.len().saturating_sub(TAPS)..];
    if taps.len() < 3 {
        return None;
    }
    let span = taps[taps.len() - 1].duration_since(taps[0]).as_secs_f64();
    let bpm = 60.0 * (taps.len() - 1) as f64 / span;
    (MIN_CLIP_BPM..=MAX_CLIP_BPM).contains(&bpm).then_some(bpm)
}

impl ClipPanel {
    /// Counts a tap at `now`, and says the tempo they make if there is one.
    pub fn tap(&mut self, now: Instant) -> Option<f64> {
        if self
            .taps
            .last()
            .is_some_and(|last| now.duration_since(*last) > TAP_RESET)
        {
            self.taps.clear();
        }
        self.taps.push(now);
        if self.taps.len() > TAPS {
            self.taps.remove(0);
        }
        tap_tempo(&self.taps)
    }
}

/// The mode a clip is first warped in, from what was heard in it: Beats for
/// material with close transients, Grain for the rest.
pub fn suggested_mode(analysis: Option<&Analysis>) -> WarpMode {
    match analysis {
        Some(analysis) if analysis.percussive => WarpMode::Beats,
        _ => WarpMode::Grain,
    }
}

/// The command that warps clip `id`: it follows the timeline's tempo, from
/// the tempo found in the audio if there was one.
pub fn start_warp(project: &Project, id: ClipId, analysis: Option<&Analysis>) -> Option<Command> {
    let tempo = analysis.and_then(|a| a.tempo);
    let mode = suggested_mode(analysis);
    project
        .warp_edit(id, |warp| {
            warp.tempo_conversion = true;
            warp.mode = mode;
            if let Some(bpm) = tempo {
                warp.tempo = TempoCurve::constant(bpm);
            }
        })
        .ok()
}

/// The command that sets the tempo of clip `id` to `bpm` all along.
pub fn set_tempo(project: &Project, id: ClipId, bpm: f64) -> Option<Command> {
    if !(MIN_CLIP_BPM..=MAX_CLIP_BPM).contains(&bpm) {
        return None;
    }
    project
        .warp_edit(id, |warp| warp.tempo = TempoCurve::constant(bpm))
        .ok()
}

/// A loop for the part of the file section `id` shows: from the first whole
/// beat in it to the last, or the whole of it if that is shorter than a beat
/// of the clip but longer than the minimum.
pub fn default_loop(warp: &ClipWarp, offset: u64, length: u64, rate: u32) -> Option<LoopRegion> {
    let rate = f64::from(rate);
    let from = warp.tempo.beats_at(offset as f64 / rate).max(0.0);
    let to = warp.tempo.beats_at((offset + length) as f64 / rate);
    let whole = LoopRegion {
        start: from.ceil(),
        end: to.floor(),
    };
    if whole.beats() >= MIN_LOOP_BEATS {
        return Some(whole);
    }
    let region = LoopRegion {
        start: from,
        end: to,
    };
    (region.beats() >= MIN_LOOP_BEATS).then_some(region)
}

/// The command that turns clip `id`'s loop on or off. Turning it on needs the
/// file's `rate`.
pub fn set_loop(project: &Project, id: ClipId, on: bool, rate: u32) -> Option<Command> {
    let audio = project.clip(id)?.as_audio()?;
    let region = if on {
        let (_, warp) = project.warp_of(id)?;
        Some(default_loop(warp, audio.offset, audio.length, rate)?)
    } else {
        None
    };
    project.warp_edit(id, |warp| warp.loop_region = region).ok()
}

/// One edit for a widget that may be dragged: a drag's steps are one undo
/// step, closed when it is let go.
fn commit(response: &Response, command: Option<Command>, edits: &mut Vec<Edit>) {
    if let Some(command) = command {
        edits.push(if response.dragged() {
            Edit::Drag(command)
        } else {
            Edit::Apply(command)
        });
    }
    if response.drag_stopped() {
        edits.push(Edit::EndDrag);
    }
}

/// Shows the properties of the audio clip `id`, if it is one. `loaded` is
/// what is known about its file.
pub fn show(
    ui: &mut Ui,
    state: &mut ClipPanel,
    session: &Session,
    id: ClipId,
    loaded: Option<&Loaded>,
) -> Vec<Edit> {
    let project = session.project();
    let mut edits = Vec::new();
    let Some(audio) = project.clip(id).and_then(|clip| clip.as_audio()) else {
        return edits;
    };
    let rate = loaded.map_or(48_000, |l| l.source.sample_rate);
    let analysis = loaded.and_then(|l| l.analysis.as_deref());
    let name = std::path::Path::new(&audio.source).file_name().map_or_else(
        || audio.source.clone(),
        |n| n.to_string_lossy().into_owned(),
    );
    ui.heading("Clip");
    ui.label(RichText::new(name).weak().small());

    let Some((_, warp)) = project.warp_of(id) else {
        ui.weak("Not warped: plays as recorded, whatever the tempo.");
        let button = ui
            .button("Warp")
            .on_hover_text("Follow the timeline's tempo, from the tempo found in the audio");
        if button.clicked() {
            edits.extend(start_warp(project, id, analysis).map(Edit::Apply));
        }
        gain(ui, project, id, &mut edits);
        return edits;
    };

    ui.horizontal(|ui| {
        ui.label("Mode");
        let mut mode = warp.mode;
        egui::ComboBox::from_id_salt("clip warp mode")
            .selected_text(match mode {
                WarpMode::Repitch => "Repitch",
                WarpMode::Grain => "Grain",
                WarpMode::Beats => "Beats",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut mode, WarpMode::Repitch, "Repitch")
                    .on_hover_text("Plain resampling: pitch follows speed");
                ui.selectable_value(&mut mode, WarpMode::Grain, "Grain")
                    .on_hover_text("Keeps the pitch: for tonal and sustained sounds");
                ui.selectable_value(&mut mode, WarpMode::Beats, "Beats")
                    .on_hover_text("Keeps each transient whole: for drums and plucks");
            });
        if mode != warp.mode {
            edits.extend(
                project
                    .warp_edit(id, |warp| warp.mode = mode)
                    .ok()
                    .map(Edit::Apply),
            );
        }
    });

    let mut follow = warp.tempo_conversion;
    if ui
        .checkbox(&mut follow, "Follow tempo")
        .on_hover_text("Play the clip's beats on the timeline's tempo")
        .changed()
    {
        edits.extend(
            project
                .warp_edit(id, |warp| warp.tempo_conversion = follow)
                .ok()
                .map(Edit::Apply),
        );
    }
    let mut markers = warp.use_markers;
    if ui
        .checkbox(&mut markers, "Warp markers")
        .on_hover_text("Pin moments of the clip to beats (in the clip editor)")
        .changed()
    {
        edits.extend(
            project
                .warp_edit(id, |warp| warp.use_markers = markers)
                .ok()
                .map(Edit::Apply),
        );
    }

    ui.horizontal(|ui| {
        ui.label("Clip tempo");
        match warp.tempo.points() {
            [only] => {
                let mut bpm = only.bpm;
                let response = ui.add(
                    egui::DragValue::new(&mut bpm)
                        .range(MIN_CLIP_BPM..=MAX_CLIP_BPM)
                        .speed(0.1)
                        .max_decimals(2)
                        .suffix(" bpm"),
                );
                if response.changed() {
                    commit(&response, set_tempo(project, id, bpm), &mut edits);
                } else {
                    commit(&response, None, &mut edits);
                }
            }
            points => {
                ui.weak(format!("{} tempos", points.len()));
            }
        }
    });
    ui.horizontal(|ui| {
        let detect = ui.add_enabled(
            analysis.and_then(|a| a.tempo).is_some(),
            egui::Button::new("Detect"),
        );
        if detect
            .on_hover_text("Find the tempo from the transients")
            .clicked()
            && let Some(bpm) = analysis.and_then(|a| a.tempo)
        {
            edits.extend(set_tempo(project, id, bpm).map(Edit::Apply));
        }
        let tap = ui.button("Tap").on_hover_text("Tap along to set the tempo");
        if tap.clicked()
            && let Some(bpm) = state.tap(Instant::now())
        {
            edits.extend(set_tempo(project, id, bpm).map(Edit::Apply));
        }
    });

    let mut looped = warp.loop_region.is_some();
    ui.horizontal(|ui| {
        if ui
            .checkbox(&mut looped, "Loop")
            .on_hover_text("Repeat the loop when the section is dragged out past its audio")
            .changed()
        {
            edits.extend(set_loop(project, id, looped, rate).map(Edit::Apply));
        }
        if let Some(region) = warp.loop_region {
            ui.weak(format!("{:.2} beats", region.beats()));
        }
    });

    gain(ui, project, id, &mut edits);
    edits
}

/// The clip's gain, in decibels.
fn gain(ui: &mut Ui, project: &Project, id: ClipId, edits: &mut Vec<Edit>) {
    let Some(clip) = project.clip(id) else { return };
    let Some(audio) = clip.as_audio() else { return };
    ui.horizontal(|ui| {
        ui.label("Gain");
        let mut db = 20.0 * f64::from(audio.gain.max(1e-4)).log10();
        let response = ui.add(
            egui::DragValue::new(&mut db)
                .range(-60.0..=24.0)
                .speed(0.1)
                .max_decimals(1)
                .suffix(" dB"),
        );
        let mut command = None;
        if response.changed() {
            let mut changed = clip.clone();
            if let ClipContent::Audio(audio) = &mut changed.content {
                audio.gain = 10f32.powf(db as f32 / 20.0);
            }
            command = Some(Command::SetClip { id, clip: changed });
        }
        commit(&response, command, edits);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_make_a_tempo_from_the_third_on() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut panel = ClipPanel::default();
        assert_eq!(panel.tap(at(0)), None);
        assert_eq!(panel.tap(at(500)), None);
        let bpm = panel.tap(at(1000)).unwrap();
        assert!((bpm - 120.0).abs() < 1e-9, "{bpm}");
        // Slowing down moves the average.
        let bpm = panel.tap(at(1600)).unwrap();
        assert!(bpm < 120.0 && bpm > 100.0, "{bpm}");
    }

    #[test]
    fn a_long_pause_starts_the_count_again() {
        let start = Instant::now();
        let mut panel = ClipPanel::default();
        for ms in [0, 500, 1000] {
            panel.tap(start + Duration::from_millis(ms));
        }
        assert_eq!(panel.tap(start + Duration::from_millis(10_000)), None);
        assert_eq!(panel.tap(start + Duration::from_millis(10_500)), None);
        let bpm = panel.tap(start + Duration::from_millis(11_000)).unwrap();
        assert!((bpm - 120.0).abs() < 1e-9);
    }

    #[test]
    fn a_default_loop_is_the_whole_beats_of_the_window() {
        let warp = ClipWarp::at_tempo(120.0);
        // Four seconds are eight beats.
        let region = default_loop(&warp, 0, 192_000, 48_000).unwrap();
        assert_eq!((region.start, region.end), (0.0, 8.0));
        // A window from 0.3 s to 3.7 s holds beats 1 to 7.
        let region = default_loop(&warp, 14_400, 163_200, 48_000).unwrap();
        assert_eq!((region.start, region.end), (1.0, 7.0));
        // Too short for a loop.
        assert_eq!(default_loop(&warp, 0, 12_000, 48_000), None);
        // Between a beat and two whole ones, the window itself.
        let region = default_loop(&warp, 12_000, 30_000, 48_000).unwrap();
        assert_eq!((region.start, region.end), (0.5, 1.75));
    }

    #[test]
    fn material_with_close_transients_is_warped_in_beats_mode() {
        let tonal = Analysis {
            onsets: vec![0.0],
            tempo: None,
            percussive: false,
        };
        let drums = Analysis {
            onsets: vec![0.0, 0.5, 1.0],
            tempo: Some(120.0),
            percussive: true,
        };
        assert_eq!(suggested_mode(Some(&tonal)), WarpMode::Grain);
        assert_eq!(suggested_mode(Some(&drums)), WarpMode::Beats);
        assert_eq!(suggested_mode(None), WarpMode::Grain);
    }
}
