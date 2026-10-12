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
    ClipContent, ClipId, ClipWarp, Command, LoopRegion, MAX_CLIP_BPM, MAX_GRID_OFFSET,
    MIN_CLIP_BPM, MIN_LOOP_BEATS, Project, QUANTISE_GRIDS, TempoCurve, WarpMarker, WarpMode,
    WarpQuantise,
};

use noodle_io::{CLEAR_TEMPO, TempoEstimate};

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

/// The command that gives clip `id` the tempo and beat `found` in its audio:
/// the clip tempo, and a grid slid so its lines fall on the transients.
pub fn detect_tempo(project: &Project, id: ClipId, found: &TempoEstimate) -> Option<Command> {
    if !(MIN_CLIP_BPM..=MAX_CLIP_BPM).contains(&found.bpm) {
        return None;
    }
    let (bpm, phase) = (found.bpm, found.phase);
    project
        .warp_edit(id, move |warp| {
            warp.tempo = TempoCurve::constant(bpm);
            warp.grid_offset = phase;
        })
        .ok()
}

/// The command that slides the grid of clip `id` to `offset` beats.
pub fn set_grid_offset(project: &Project, id: ClipId, offset: f64) -> Option<Command> {
    if !offset.is_finite() || offset.abs() > MAX_GRID_OFFSET {
        return None;
    }
    project
        .warp_edit(id, move |warp| warp.grid_offset = offset)
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

/// The most markers "Pin transients" puts on a clip.
const MAX_PINNED: usize = 256;
/// Transients closer than this (seconds) share a marker.
const MIN_PIN_GAP: f64 = 0.04;

/// The command that pins every transient of clip `id` that has no marker yet,
/// at the beat it plays at with the markers as they are stored. With quantise
/// off the sound does not change; with it on the new markers are quantised
/// like the rest, so the clip is re-timed. Quantise has markers to move, and
/// the clip's groove has something to be read from. `None` if there is
/// nothing to pin.
pub fn pin_transients(project: &Project, id: ClipId, onsets: &[f64]) -> Option<Command> {
    let (_, warp) = project.warp_of(id)?;
    // Where things play with the markers as they are stored.
    let plain = ClipWarp {
        quantise: None,
        ..warp.clone()
    };
    let mut pinned = plain.clone();
    let mut last = f64::NEG_INFINITY;
    for &seconds in onsets {
        if pinned.markers.len() >= MAX_PINNED {
            break;
        }
        let near_last = seconds - last < MIN_PIN_GAP;
        last = seconds;
        if near_last || seconds < 0.0 || pinned.marker_at(seconds).is_some() {
            continue;
        }
        // A marker with another close beside it is pointless.
        if pinned
            .markers
            .iter()
            .any(|m| (m.seconds - seconds).abs() < MIN_PIN_GAP)
        {
            continue;
        }
        pinned.set_marker(WarpMarker {
            seconds,
            beat: plain.beat_at(seconds),
        });
    }
    if pinned.markers.len() == warp.markers.len() || pinned.problem().is_some() {
        return None;
    }
    let markers = pinned.markers;
    project
        .warp_edit(id, move |warp| warp.markers = markers)
        .ok()
}

/// The command that changes warp quantise of clip `id`: `change` is given the
/// current settings (or the defaults, for a clip that has none).
pub fn change_quantise(
    project: &Project,
    id: ClipId,
    change: impl FnOnce(&mut WarpQuantise),
) -> Option<Command> {
    project
        .warp_edit(id, |warp| {
            let quantise = warp.quantise.get_or_insert_with(WarpQuantise::default);
            change(quantise);
        })
        .ok()
}

/// The command that turns warp quantise of clip `id` off.
pub fn quantise_off(project: &Project, id: ClipId) -> Option<Command> {
    project.warp_edit(id, |warp| warp.quantise = None).ok()
}

/// The command that makes the quantised markers of clip `id` its markers.
pub fn bake_quantise(project: &Project, id: ClipId) -> Option<Command> {
    project.warp_edit(id, |warp| *warp = warp.baked()).ok()
}

/// The clips clip `id` can take a groove from, by file name: the other audio
/// clips whose warp has markers to read one from. Clips that share `id`'s own
/// warp are left out.
pub fn groove_sources(project: &Project, id: ClipId) -> Vec<(ClipId, String)> {
    let own = project.warp_of(id).map(|(warp, _)| warp);
    project
        .clips()
        .filter(|&(other, _)| other != id)
        .filter_map(|(other, clip)| {
            let audio = clip.as_audio()?;
            let warp = audio.warp?;
            if Some(warp) == own || project.warp(warp)?.groove().is_none() {
                return None;
            }
            let name = std::path::Path::new(&audio.source).file_name().map_or_else(
                || audio.source.clone(),
                |n| n.to_string_lossy().into_owned(),
            );
            Some((other, name))
        })
        .collect()
}

/// The command that gives clip `id` the groove of clip `source`: copied, or
/// followed as it changes if `follow`.
pub fn take_groove(project: &Project, id: ClipId, source: ClipId, follow: bool) -> Option<Command> {
    let groove = project.warp_of(source)?.1.groove();
    change_quantise(project, id, move |quantise| {
        quantise.groove = groove.or(quantise.groove.take());
        quantise.groove_from = follow.then_some(source);
    })
}

/// The command that drops the groove of clip `id`'s quantise.
pub fn drop_groove(project: &Project, id: ClipId) -> Option<Command> {
    change_quantise(project, id, |quantise| {
        quantise.groove = None;
        quantise.groove_from = None;
    })
}

/// The name of a grid, for the menu.
fn grid_name(grid: f64) -> String {
    match grid {
        g if (g - 4.0).abs() < 1e-9 => "Bar".into(),
        g if (g - 1.0).abs() < 1e-9 => "1/4 note".into(),
        g if g > 0.0 && (1.0 / g).fract().abs() < 1e-9 => format!("1/{} note", (4.0 / g).round()),
        g => format!("{g} beats"),
    }
}

/// The quantise section of the panel.
fn quantise_section(
    ui: &mut Ui,
    project: &Project,
    id: ClipId,
    warp: &ClipWarp,
    analysis: Option<&Analysis>,
    edits: &mut Vec<Edit>,
) {
    ui.separator();
    ui.horizontal(|ui| {
        let mut on = warp.quantise.is_some();
        if ui
            .checkbox(&mut on, "Quantise")
            .on_hover_text("Pull the warp markers toward a grid. The markers you set stay as they are: turn it off and the original timing is back")
            .changed()
        {
            let command = if on {
                change_quantise(project, id, |_| {})
            } else {
                quantise_off(project, id)
            };
            edits.extend(command.map(Edit::Apply));
        }
        if ui
            .button("Pin transients")
            .on_hover_text("Put a marker on every transient that has none, where it plays now (with quantise on, they are quantised too), so there is something to quantise")
            .clicked()
        {
            edits.extend(
                pin_transients(project, id, analysis.map_or(&[], |a| &a.onsets)).map(Edit::Apply),
            );
        }
    });
    let Some(quantise) = &warp.quantise else {
        return;
    };

    ui.horizontal(|ui| {
        ui.label("Grid");
        let mut grid = quantise.grid;
        egui::ComboBox::from_id_salt("clip quantise grid")
            .selected_text(grid_name(grid))
            .show_ui(ui, |ui| {
                for option in QUANTISE_GRIDS {
                    ui.selectable_value(&mut grid, option, grid_name(option));
                }
            });
        if grid != quantise.grid {
            edits.extend(change_quantise(project, id, |q| q.grid = grid).map(Edit::Apply));
        }
    });
    ui.horizontal(|ui| {
        ui.label("Amount");
        let mut percent = quantise.amount * 100.0;
        let response = ui.add(
            egui::DragValue::new(&mut percent)
                .range(0.0..=100.0)
                .speed(0.5)
                .max_decimals(0)
                .suffix(" %"),
        );
        let command = response
            .changed()
            .then(|| change_quantise(project, id, |q| q.amount = percent / 100.0))
            .flatten();
        commit(&response, command, edits);
    });

    // The groove: a feel lent by another clip.
    let sources = groove_sources(project, id);
    ui.horizontal(|ui| {
        ui.label("Groove");
        let current = match (&quantise.groove, quantise.groove_from) {
            (None, _) => "None".to_string(),
            (Some(_), None) => "Copied".to_string(),
            (Some(_), Some(from)) => sources
                .iter()
                .find(|(warp, _)| *warp == from)
                .map_or("Linked".to_string(), |(_, name)| format!("Linked: {name}")),
        };
        let mut chosen: Option<Option<(ClipId, bool)>> = None;
        egui::ComboBox::from_id_salt("clip groove")
            .selected_text(current)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(quantise.groove.is_none(), "None")
                    .clicked()
                {
                    chosen = Some(None);
                }
                for (warp, name) in &sources {
                    if ui
                        .button(format!("Link: {name}"))
                        .on_hover_text("Follow this clip's groove as it changes")
                        .clicked()
                    {
                        chosen = Some(Some((*warp, true)));
                    }
                    if ui
                        .button(format!("Copy: {name}"))
                        .on_hover_text("Take this clip's groove as it is now")
                        .clicked()
                    {
                        chosen = Some(Some((*warp, false)));
                    }
                }
            });
        match chosen {
            Some(None) => edits.extend(drop_groove(project, id).map(Edit::Apply)),
            Some(Some((source, follow))) => {
                edits.extend(take_groove(project, id, source, follow).map(Edit::Apply));
            }
            None => {}
        }
    });
    if quantise.groove.is_some() {
        ui.horizontal(|ui| {
            ui.label("Groove amount");
            let mut percent = quantise.groove_amount * 100.0;
            let response = ui.add(
                egui::DragValue::new(&mut percent)
                    .range(0.0..=100.0)
                    .speed(0.5)
                    .max_decimals(0)
                    .suffix(" %"),
            );
            let command = response
                .changed()
                .then(|| change_quantise(project, id, |q| q.groove_amount = percent / 100.0))
                .flatten();
            commit(&response, command, edits);
        });
    }
    if ui
        .button("Bake")
        .on_hover_text("Make the quantised markers the markers, and turn quantise off")
        .clicked()
    {
        edits.extend(bake_quantise(project, id).map(Edit::Apply));
    }
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
        let found = analysis.and_then(|a| a.grid);
        let detect = ui.add_enabled(found.is_some(), egui::Button::new("Detect"));
        let hint = match (analysis, found) {
            (None, _) => "Still reading the file".to_owned(),
            (Some(_), None) => {
                "Too few clear transients to find a tempo: tap it or type it".to_owned()
            }
            (Some(_), Some(f)) if f.confidence < CLEAR_TEMPO => format!(
                "Find the tempo and where the beat falls from the transients (a weak guess: {:.0}% sure)",
                f.confidence * 100.0
            ),
            (Some(_), Some(_)) => {
                "Find the tempo and where the beat falls from the transients".to_owned()
            }
        };
        if detect.on_hover_text(hint).clicked()
            && let Some(found) = found
        {
            edits.extend(detect_tempo(project, id, &found).map(Edit::Apply));
        }
        let tap = ui.button("Tap").on_hover_text("Tap along to set the tempo");
        if tap.clicked()
            && let Some(bpm) = state.tap(Instant::now())
        {
            edits.extend(set_tempo(project, id, bpm).map(Edit::Apply));
        }
    });
    ui.horizontal(|ui| {
        ui.label("Offset");
        let mut offset = warp.grid_offset;
        let response = ui
            .add(
                egui::DragValue::new(&mut offset)
                    .range(-MAX_GRID_OFFSET..=MAX_GRID_OFFSET)
                    .speed(0.005)
                    .max_decimals(3)
                    .suffix(" beats"),
            )
            .on_hover_text(
                "Slide the clip's beat lines against its audio, so they line up with its transients (or drag the waveform in the clip editor)",
            );
        if response.changed() {
            commit(&response, set_grid_offset(project, id, offset), &mut edits);
        } else {
            commit(&response, None, &mut edits);
        }
        if warp.grid_offset != 0.0
            && ui
                .small_button("Reset")
                .on_hover_text("Put the beat lines back on the whole beats")
                .clicked()
        {
            edits.extend(set_grid_offset(project, id, 0.0).map(Edit::Apply));
        }
    });

    quantise_section(ui, project, id, warp, analysis, &mut edits);
    ui.separator();

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
            grid: None,
            percussive: false,
        };
        let drums = Analysis {
            onsets: vec![0.0, 0.5, 1.0],
            tempo: Some(120.0),
            grid: None,
            percussive: true,
        };
        assert_eq!(suggested_mode(Some(&tonal)), WarpMode::Grain);
        assert_eq!(suggested_mode(Some(&drums)), WarpMode::Beats);
        assert_eq!(suggested_mode(None), WarpMode::Grain);
    }
}
