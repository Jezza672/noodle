//! The clip editor for a clip that isn't warped: its audio on a seconds ruler,
//! the part the clip plays lit and the rest dimmed. Dragging the waveform
//! slips the audio under the clip's window (it changes the clip's offset into
//! the file, not where the clip is). The Warp button in the toolbar turns the
//! beat editing on.

use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, vec2};
use noodle_core::{AudioClip, Clip, ClipContent, ClipId, Command, Tick};
use noodle_io::Peaks;

use super::{ClipEditor, MAX_COLUMNS, MAX_PPB, MIN_PPB, RULER};
use crate::session::{Edit, Session};
use crate::theme::{self, timeline as colors};
use crate::timeline::Loaded;

/// A drag of the waveform: where the pointer was, and the clip then.
pub(super) struct Slip {
    press_x: f32,
    original: Clip,
}

/// Where the seconds are on the screen: second 0 is the start of the clip's
/// window, so the audio before it (the file's first `offset` frames) is at a
/// negative time.
#[derive(Clone, Copy)]
struct Axis {
    origin: f32,
    pps: f32,
}

impl Axis {
    fn x(self, seconds: f64) -> f32 {
        self.origin + seconds as f32 * self.pps
    }
}

/// Where things were drawn last frame, for tests to point at.
#[cfg(test)]
#[derive(Clone, Copy)]
pub struct Geometry {
    pub wave: Rect,
    /// The window's left edge.
    pub window: f32,
    pub pps: f32,
}

/// Draws the editor for the unwarped audio clip `id` into `body` (everything
/// under the toolbar).
pub(super) fn show(
    ui: &mut egui::Ui,
    state: &mut ClipEditor,
    session: &Session,
    id: ClipId,
    loaded: &Loaded,
    body: Rect,
    playhead: Option<Tick>,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    let project = session.project();
    let Some(clip) = project.clip(id) else {
        return edits;
    };
    let Some(audio) = clip.as_audio() else {
        return edits;
    };
    let rate = f64::from(loaded.source.sample_rate.max(1));
    let Some(file_frames) = loaded
        .source
        .frames
        .or_else(|| loaded.peaks.as_ref().map(|p| p.frames()))
    else {
        return edits;
    };
    let (offset_secs, length_secs, file_secs) = (
        audio.offset as f64 / rate,
        audio.length as f64 / rate,
        file_frames as f64 / rate,
    );
    let ruler = Rect::from_min_size(body.min, vec2(body.width(), RULER));
    let wave = Rect::from_min_max(Pos2::new(body.left(), ruler.bottom()), body.max);
    let area = Rect::from_min_max(ruler.min, body.max);

    // The file runs from -offset to (file - offset) around the window.
    let extent = (-offset_secs, file_secs - offset_secs);
    if state.focus {
        state.focus = false;
        let span = (extent.1 - extent.0).max(0.1) as f32;
        state.ppb = ((area.width() - 24.0) / span).clamp(MIN_PPB, MAX_PPB);
        state.scroll_x = extent.0 as f32 * state.ppb - 12.0;
    }
    if ui.rect_contains_pointer(area) {
        // The slip changes the extent while it runs; scrolling follows the
        // clip's start instead, so the view doesn't slide under the pointer.
        super::navigate(
            ui,
            state,
            area,
            (extent.0.min(0.0), extent.1.max(length_secs)),
        );
    }
    let axis = Axis {
        origin: area.left() - state.scroll_x,
        pps: state.ppb,
    };
    #[cfg(test)]
    {
        state.plain_geometry = Some(Geometry {
            wave,
            window: axis.x(0.0),
            pps: axis.pps,
        });
    }

    let painter = ui.painter_at(body);
    painter.rect_filled(wave, 0.0, colors::BACKGROUND);
    draw_waveform(
        &painter,
        wave,
        axis,
        loaded.peaks.as_deref(),
        rate,
        offset_secs,
        audio.gain,
    );
    // The audio outside the window is dimmed.
    let dim = Color32::BLACK.gamma_multiply(0.45);
    let (from, to) = (axis.x(0.0), axis.x(length_secs));
    for (a, b) in [(wave.left(), from), (to, wave.right())] {
        let (a, b) = (a.max(wave.left()), b.min(wave.right()));
        if b > a {
            painter.rect_filled(Rect::from_x_y_ranges(a..=b, wave.y_range()), 0.0, dim);
        }
    }
    for x in [from, to] {
        if (wave.left()..=wave.right()).contains(&x) {
            painter.vline(x, wave.y_range(), Stroke::new(1.0, colors::TEXT_WEAK));
        }
    }
    draw_ruler(&painter, ruler, wave, axis);
    if let Some(at) = playhead.filter(|at| *at >= clip.start) {
        let map = project.tempo_map();
        let seconds = map.seconds_at(at) - map.seconds_at(clip.start);
        let x = axis.x(seconds);
        if seconds <= length_secs && (wave.left()..=wave.right()).contains(&x) {
            painter.vline(x, area.y_range(), Stroke::new(1.0, colors::PLAYHEAD));
        }
    }

    // Dragging the waveform slips the audio under the window.
    let response = ui.interact(
        wave,
        ui.id().with("clip editor audio"),
        Sense::click_and_drag(),
    );
    if response.hovered() || state.slip.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    if response.drag_started()
        && let Some(press) = ui.input(|i| i.pointer.press_origin())
    {
        state.slip = Some(Slip {
            press_x: press.x,
            original: clip.clone(),
        });
    }
    if response.dragged()
        && let (Some(slip), Some(pos)) = (&state.slip, response.interact_pointer_pos())
        && let Some(changed) = slipped(
            &slip.original,
            (pos.x - slip.press_x) / axis.pps,
            rate,
            file_frames,
        )
        && changed != *clip
    {
        edits.push(Edit::Drag(Command::SetClip { id, clip: changed }));
    }
    if response.drag_stopped() && state.slip.take().is_some() {
        edits.push(Edit::EndDrag);
    }
    if state.slip.is_some() && project.clip(id).is_none() {
        state.slip = None;
    }
    // The gain envelope, in timeline time from the clip's start.
    {
        let map = project.tempo_map();
        let start = map.seconds_at(clip.start);
        let x_of = |tick: Tick| axis.x(map.seconds_at(Tick(clip.start.0 + tick.0)) - start);
        let tick_at = |x: f32| {
            let seconds = start + f64::from((x - axis.origin) / axis.pps);
            Tick((map.tick_at_seconds(seconds).round() as i64 - clip.start.0).max(0))
        };
        let end = map.sample_at(clip.start, rate) + audio.length;
        let length = Tick(map.tick_at_sample(end, rate).round() as i64 - clip.start.0);
        edits.extend(super::gain_line::show(
            ui,
            &painter,
            &super::gain_line::Frame {
                wave,
                length,
                x_of: &x_of,
                tick_at: &tick_at,
            },
            id,
            clip,
            audio,
            &response,
        ));
    }
    if loaded.peaks.is_none() {
        painter.text(
            wave.center(),
            Align2::CENTER_CENTER,
            "Reading the waveform…",
            FontId::proportional(12.0),
            colors::TEXT_WEAK,
        );
    }
    edits
}

/// `original` with its audio slid `seconds` along the clip's window, by
/// changing the offset the other way: the audio follows a drag to the right,
/// so the window moves left in the file. It stops at the ends of the file.
pub(super) fn slipped(original: &Clip, seconds: f32, rate: f64, file_frames: u64) -> Option<Clip> {
    let ClipContent::Audio(audio) = &original.content else {
        return None;
    };
    let last = file_frames.saturating_sub(audio.length) as f64;
    let offset = (audio.offset as f64 - f64::from(seconds) * rate).clamp(0.0, last);
    Some(Clip {
        content: ClipContent::Audio(AudioClip {
            offset: offset.round() as u64,
            ..audio.clone()
        }),
        ..original.clone()
    })
}

/// The file's peaks across the window, one column per pixel.
fn draw_waveform(
    painter: &egui::Painter,
    wave: Rect,
    axis: Axis,
    peaks: Option<&Peaks>,
    rate: f64,
    offset_secs: f64,
    gain: f32,
) {
    let Some(peaks) = peaks else {
        return;
    };
    let frames = peaks.frames();
    // The file's frames, on screen.
    let (x0, x1) = (
        axis.x(-offset_secs),
        axis.x(frames as f64 / rate - offset_secs),
    );
    let (v0, v1) = (x0.max(wave.left()), x1.min(wave.right()));
    if v1 - v0 < 0.5 || x1 <= x0 {
        return;
    }
    let across =
        |x: f32| (f64::from((x - x0) / (x1 - x0)) * frames as f64).clamp(0.0, frames as f64);
    let columns = ((v1 - v0).ceil() as usize).clamp(1, MAX_COLUMNS);
    let (start, end) = (across(v0).round() as u64, across(v1).round() as u64);
    let middle = wave.center().y;
    let half = wave.height() / 2.0 - 2.0;
    let stroke = Stroke::new(
        (v1 - v0) / columns as f32,
        theme::ACCENT.gamma_multiply(0.55),
    );
    for (i, peak) in peaks.columns(None, start, end, columns).iter().enumerate() {
        let x = v0 + (i as f32 + 0.5) * (v1 - v0) / columns as f32;
        let (min, max) = (
            (peak.min * gain).clamp(-1.0, 1.0),
            (peak.max * gain).clamp(-1.0, 1.0),
        );
        painter.line_segment(
            [
                Pos2::new(x, middle - max * half - 0.5),
                Pos2::new(x, middle - min * half + 0.5),
            ],
            stroke,
        );
    }
}

/// A seconds ruler: a line at a round number of seconds, labelled when there
/// is room.
fn draw_ruler(painter: &egui::Painter, ruler: Rect, wave: Rect, axis: Axis) {
    painter.rect_filled(ruler, 0.0, colors::RULER);
    let every: f64 = [
        0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0,
    ]
    .into_iter()
    .find(|s| *s * f64::from(axis.pps) >= 60.0)
    .unwrap_or(120.0);
    let first =
        ((f64::from(ruler.left() - axis.origin) / f64::from(axis.pps)) / every).floor() as i64;
    let last =
        ((f64::from(ruler.right() - axis.origin) / f64::from(axis.pps)) / every).ceil() as i64;
    for n in first..=last {
        let seconds = n as f64 * every;
        let x = axis.x(seconds);
        if !(ruler.left()..=ruler.right()).contains(&x) {
            continue;
        }
        painter.vline(
            x,
            ruler.bottom() - 6.0..=wave.bottom(),
            Stroke::new(1.0, colors::BEAT_LINE),
        );
        painter.text(
            Pos2::new(x + 3.0, ruler.top() + 2.0),
            Align2::LEFT_TOP,
            seconds_label(seconds, every),
            FontId::proportional(10.0),
            colors::TEXT_WEAK,
        );
    }
}

fn seconds_label(seconds: f64, every: f64) -> String {
    if every >= 1.0 {
        format!("{seconds:.0} s")
    } else if every >= 0.1 {
        format!("{seconds:.2} s")
    } else {
        format!("{:.0} ms", seconds * 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::NodeId;

    fn clip(offset: u64, length: u64) -> Clip {
        let mut clip = Clip::audio(NodeId(1), Tick(0), "a.wav", length);
        if let ClipContent::Audio(audio) = &mut clip.content {
            audio.offset = offset;
        }
        clip
    }

    fn offset_of(clip: &Clip) -> u64 {
        clip.as_audio().unwrap().offset
    }

    #[test]
    fn the_audio_follows_the_drag_so_the_offset_goes_the_other_way() {
        let original = clip(48_000, 24_000);
        // A second to the right: the file's earlier audio comes under the window.
        let moved = slipped(&original, 0.5, 48_000.0, 200_000).unwrap();
        assert_eq!(offset_of(&moved), 24_000);
        let moved = slipped(&original, -0.5, 48_000.0, 200_000).unwrap();
        assert_eq!(offset_of(&moved), 72_000);
    }

    #[test]
    fn the_window_stays_inside_the_file() {
        let original = clip(48_000, 24_000);
        assert_eq!(
            offset_of(&slipped(&original, 10.0, 48_000.0, 200_000).unwrap()),
            0
        );
        // The window's end can't pass the file's.
        assert_eq!(
            offset_of(&slipped(&original, -10.0, 48_000.0, 200_000).unwrap()),
            200_000 - 24_000
        );
    }
}
