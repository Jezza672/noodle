//! The clip editor for a clip that isn't warped: the shared audio pane (see
//! [`super::pane`]) on the clip's seconds, with the clip's gain envelope on
//! top. The part the clip plays is lit and the rest dimmed; dragging the
//! waveform slips the audio under the clip's window (it changes the clip's
//! offset into the file, not where the clip is), and pulling either edge of
//! the window trims the clip like the timeline does. The Warp button in the
//! toolbar turns the beat editing on.

use egui::Rect;
use noodle_core::{AudioClip, Clip, ClipContent, ClipId, Command, Tick};

use super::ClipEditor;
use super::pane::{self, CurveSource, Edge, Gesture};
use crate::session::{Edit, Session};
use crate::timeline::Loaded;
use crate::timeline::clips;

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
    let (offset_secs, length_secs) = (audio.offset as f64 / rate, audio.length as f64 / rate);
    let map = project.tempo_map();
    let playhead = playhead.filter(|at| *at >= clip.start).and_then(|at| {
        let seconds = map.seconds_at(at) - map.seconds_at(clip.start);
        (seconds <= length_secs).then_some(offset_secs + seconds)
    });
    let out = pane::show(
        ui,
        &mut state.view,
        body,
        &pane::Spec {
            peaks: loaded.peaks.as_deref(),
            curve: CurveSource::File(&loaded.path),
            rate,
            frames: file_frames,
            zero: offset_secs,
            lit: (offset_secs, offset_secs + length_secs),
            looping: None,
            gain: audio.gain,
            playhead,
        },
    );
    let (wave, axis, response) = (out.wave, out.axis, out.response);
    #[cfg(test)]
    {
        state.plain_geometry = Some(Geometry {
            wave,
            window: axis.x(0.0),
            pps: axis.pps,
        });
    }
    for gesture in out.gestures {
        match gesture {
            Gesture::Begin => state.slip = Some(clip.clone()),
            Gesture::Slip(seconds) => {
                if let Some(original) = &state.slip
                    && let Some(changed) = slipped(original, seconds, rate, file_frames)
                    && changed != *clip
                {
                    edits.push(Edit::Drag(Command::SetClip { id, clip: changed }));
                }
            }
            Gesture::Edge(edge, file_seconds) => {
                let start_secs = map.seconds_at(clip.start);
                let tick = Tick(
                    map.tick_at_seconds(start_secs + file_seconds - offset_secs)
                        .round() as i64,
                );
                let sample_rate = loaded.source.sample_rate;
                let changed = match edge {
                    Edge::Start => clips::trim_start(map, clip, tick, sample_rate, None),
                    Edge::End => {
                        clips::trim_end(map, clip, tick, sample_rate, Some(file_frames), None)
                    }
                    Edge::LoopStart | Edge::LoopEnd => None,
                };
                if let Some(changed) = changed.filter(|c| c != clip) {
                    if edge == Edge::Start
                        && let Some(new) = changed.as_audio()
                    {
                        // Time 0 is the window's start: keep the audio where
                        // it is on screen while the window moves over it.
                        let moved = (new.offset as f64 - audio.offset as f64) / rate;
                        state.view.scroll_x -= moved as f32 * axis.pps;
                    }
                    edits.push(Edit::Drag(Command::SetClip { id, clip: changed }));
                }
            }
            Gesture::Release => {
                state.slip = None;
                edits.push(Edit::EndDrag);
            }
        }
    }
    if state.slip.is_some() && project.clip(id).is_none() {
        state.slip = None;
    }
    // The gain envelope, in timeline time from the clip's start.
    {
        let start = map.seconds_at(clip.start);
        let x_of = |tick: Tick| axis.x(map.seconds_at(Tick(clip.start.0 + tick.0)) - start);
        let tick_at = |x: f32| {
            let seconds = start + f64::from((x - axis.origin) / axis.pps);
            Tick((map.tick_at_seconds(seconds).round() as i64 - clip.start.0).max(0))
        };
        let end = map.sample_at(clip.start, rate) + audio.length;
        let length = Tick(map.tick_at_sample(end, rate).round() as i64 - clip.start.0);
        let painter = ui.painter_at(body);
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
