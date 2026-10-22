//! The editor for audio that isn't warped (a clip, or a Sampler's sample): the
//! shared audio pane (see [`super::pane`]) on the file's seconds, with the
//! gain envelope on top. The part that plays is lit and the rest dimmed;
//! dragging the waveform slips the audio under the lit part (a clip's offset
//! into the file, a Sampler's `start` and `end`), and pulling either edge of
//! the lit part trims it. The loop's ends can be pulled too. The Warp button
//! in the toolbar turns the beat editing on.

use egui::Rect;
use noodle_core::{AudioClip, Command, GainPoint, MIN_LOOP_BEATS, Tick};

use super::pane::{self, CurveSource, Edge, Gesture, MIN_SECONDS};
use super::subject::{ENVELOPE_LENGTH, Section, Subject};
use super::{ClipEditor, gain_line};
use crate::clip_panel::Span;
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

/// The loop checkbox of the toolbar: turn the loop on over the lit part, or
/// off.
pub(super) fn loop_toggle(
    ui: &mut egui::Ui,
    project: &noodle_core::Project,
    subject: Subject,
    section: &Section,
    span: Span,
    edits: &mut Vec<Edit>,
) {
    let rate = f64::from(span.rate.max(1));
    let mut on = section.looping.is_some();
    if ui
        .checkbox(&mut on, "Loop")
        .on_hover_text(match subject {
            Subject::Clip(_) => "Repeat the lit part when the clip is dragged out past its audio",
            Subject::Sampler(_) => "Repeat a stretch of the sample while a note is held",
        })
        .changed()
    {
        let lit = (
            section.audio.offset as f64 / rate,
            (section.audio.offset + section.audio.length) as f64 / rate,
        );
        let command = match subject {
            Subject::Clip(id) => crate::clip_panel::set_loop(project, id, on, span.rate),
            Subject::Sampler(_) => subject.set_loop(project, span, on.then_some(lit)),
        };
        edits.extend(command.map(Edit::Apply));
    }
}

/// Draws the editor for `subject` (shown as `section`, with the plain axis)
/// into `body` (everything under the toolbar).
#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    ui: &mut egui::Ui,
    state: &mut ClipEditor,
    session: &Session,
    subject: Subject,
    section: &Section,
    loaded: &Loaded,
    body: Rect,
    playhead: Option<Tick>,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    let project = session.project();
    let audio = &section.audio;
    let rate = f64::from(loaded.source.sample_rate.max(1));
    let Some(file_frames) = loaded
        .source
        .frames
        .or_else(|| loaded.peaks.as_ref().map(|p| p.frames()))
    else {
        return edits;
    };
    let span = Span {
        rate: loaded.source.sample_rate,
        frames: file_frames,
    };
    let total = file_frames as f64 / rate;
    let (offset_secs, length_secs) = (audio.offset as f64 / rate, audio.length as f64 / rate);
    let map = project.tempo_map();
    // Time 0 is the lit part's start for a clip (the timeline's view of it),
    // the file's start for a sample.
    let zero = match subject {
        Subject::Clip(_) => offset_secs,
        Subject::Sampler(_) => 0.0,
    };
    let playheads: Vec<f64> = match subject {
        Subject::Clip(_) => playhead
            .filter(|at| *at >= section.start)
            .and_then(|at| {
                let seconds = map.seconds_at(at) - map.seconds_at(section.start);
                (seconds <= length_secs).then_some(offset_secs + seconds)
            })
            .into_iter()
            .collect(),
        Subject::Sampler(node) => session
            .telemetry()
            .read_positions(node)
            .into_iter()
            .map(|fraction| f64::from(fraction) * total)
            .collect(),
    };
    #[cfg(test)]
    {
        state.playheads = playheads.clone();
    }
    if matches!(subject, Subject::Sampler(_)) && !playheads.is_empty() {
        ui.ctx().request_repaint();
    }
    let out = pane::show(
        ui,
        &mut state.view,
        body,
        &pane::Spec {
            peaks: loaded.peaks.as_deref(),
            curve: match &loaded.memory {
                Some(sample) => CurveSource::Memory(sample),
                None => CurveSource::File(&loaded.path),
            },
            rate,
            frames: file_frames,
            zero,
            lit: (offset_secs, offset_secs + length_secs),
            looping: section.looping,
            gain: audio.gain,
            playheads: &playheads,
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
    let loop_before = section.looping;
    for gesture in out.gestures {
        match gesture {
            Gesture::Begin => state.slip = Some((audio.clone(), loop_before)),
            Gesture::Slip(seconds) => {
                if let Some((original, original_loop)) = &state.slip {
                    let shifted = slipped(original, seconds, rate, file_frames, *original_loop);
                    let moved = shifted.0 != *audio;
                    let command = subject.slide(project, span, shifted.0, shifted.1);
                    edits.extend(command.filter(|_| moved).map(Edit::Drag));
                }
            }
            Gesture::Edge(edge @ (Edge::LoopStart | Edge::LoopEnd), at) => {
                if let Some(looped) = loop_before
                    && let Some(moved) = pull_loop(subject, project, section, edge, at, rate)
                    && moved != looped
                {
                    edits.extend(subject.set_loop(project, span, Some(moved)).map(Edit::Drag));
                }
            }
            Gesture::Edge(edge, file_seconds) => {
                match subject {
                    Subject::Clip(id) => {
                        let Some(clip) = project.clip(id) else {
                            continue;
                        };
                        let start_secs = map.seconds_at(clip.start);
                        let tick = Tick(
                            map.tick_at_seconds(start_secs + file_seconds - offset_secs)
                                .round() as i64,
                        );
                        let sample_rate = loaded.source.sample_rate;
                        let changed = if edge == Edge::Start {
                            clips::trim_start(map, clip, tick, sample_rate, None)
                        } else {
                            clips::trim_end(map, clip, tick, sample_rate, Some(file_frames), None)
                        };
                        if let Some(changed) = changed.filter(|c| c != clip) {
                            if edge == Edge::Start
                                && let Some(new) = changed.as_audio()
                            {
                                // Time 0 is the window's start: keep the audio
                                // where it is on screen while the window moves
                                // over it.
                                let moved = (new.offset as f64 - audio.offset as f64) / rate;
                                state.view.scroll_x -= moved as f32 * axis.pps;
                            }
                            edits.push(Edit::Drag(Command::SetClip { id, clip: changed }));
                        }
                    }
                    Subject::Sampler(_) => {
                        let (new, looping) = pulled(audio, edge, file_seconds, rate, loop_before);
                        if new != *audio {
                            edits
                                .extend(subject.slide(project, span, new, looping).map(Edit::Drag));
                        }
                    }
                }
            }
            Gesture::Release => {
                state.slip = None;
                edits.push(Edit::EndDrag);
            }
        }
    }
    if state.slip.is_some() && subject.read(project, span).is_none() {
        state.slip = None;
    }
    // The gain envelope.
    {
        let painter = ui.painter_at(body);
        let set = |points: Vec<GainPoint>| subject.set_gain_points(project, points);
        match subject {
            Subject::Clip(_) => {
                // In timeline time from the clip's start.
                let start = map.seconds_at(section.start);
                let x_of =
                    |tick: Tick| axis.x(map.seconds_at(Tick(section.start.0 + tick.0)) - start);
                let tick_at = |x: f32| {
                    let seconds = start + f64::from((x - axis.origin) / axis.pps);
                    Tick((map.tick_at_seconds(seconds).round() as i64 - section.start.0).max(0))
                };
                let end = map.sample_at(section.start, rate) + audio.length;
                let length = Tick(map.tick_at_sample(end, rate).round() as i64 - section.start.0);
                edits.extend(gain_line::show(
                    ui,
                    &painter,
                    &gain_line::Frame {
                        wave,
                        length,
                        x_of: &x_of,
                        tick_at: &tick_at,
                    },
                    subject,
                    &audio.gain_points,
                    &set,
                    &response,
                ));
            }
            Subject::Sampler(_) => {
                // Millionths of the sample.
                let scale = ENVELOPE_LENGTH.0 as f64;
                let x_of = |tick: Tick| axis.x(tick.0 as f64 / scale * total - zero);
                let tick_at = |x: f32| {
                    let seconds = f64::from((x - axis.origin) / axis.pps) + zero;
                    Tick(
                        (seconds / total.max(1e-9) * scale)
                            .round()
                            .clamp(0.0, scale) as i64,
                    )
                };
                edits.extend(gain_line::show(
                    ui,
                    &painter,
                    &gain_line::Frame {
                        wave,
                        length: ENVELOPE_LENGTH,
                        x_of: &x_of,
                        tick_at: &tick_at,
                    },
                    subject,
                    &audio.gain_points,
                    &set,
                    &response,
                ));
            }
        }
    }
    edits
}

/// `original` with its audio slid `seconds` along the lit part, by changing
/// the offset the other way: the audio follows a drag to the right, so the
/// part moves left in the file. It stops at the ends of the file, and the
/// loop, if there is one, goes with it (and stops it first if it reaches an
/// end).
pub(super) fn slipped(
    original: &AudioClip,
    seconds: f32,
    rate: f64,
    file_frames: u64,
    looping: Option<(f64, f64)>,
) -> (AudioClip, Option<(f64, f64)>) {
    let offset = original.offset as f64;
    let last = file_frames.saturating_sub(original.length) as f64;
    // How far the offset may move either way.
    let (mut back, mut forward) = (offset, last - offset);
    if let Some((a, b)) = looping {
        back = back.min(a * rate);
        forward = forward.min(file_frames as f64 - b * rate);
    }
    let by = (-f64::from(seconds) * rate)
        .clamp(-back.max(0.0), forward.max(0.0))
        .round();
    let shift = by / rate;
    (
        AudioClip {
            offset: (offset + by) as u64,
            ..original.clone()
        },
        looping.map(|(a, b)| (a + shift, b + shift)),
    )
}

/// The lit part after its edge `edge` is pulled to `seconds` of the file, and
/// the loop kept inside it.
fn pulled(
    audio: &AudioClip,
    edge: Edge,
    seconds: f64,
    rate: f64,
    looping: Option<(f64, f64)>,
) -> (AudioClip, Option<(f64, f64)>) {
    let min = (MIN_SECONDS * rate).ceil() as u64;
    let end = audio.offset + audio.length;
    let frame = (seconds * rate).max(0.0).round() as u64;
    let (offset, end) = match edge {
        Edge::Start => (frame.min(end.saturating_sub(min)), end),
        _ => (audio.offset, frame.max(audio.offset + min)),
    };
    let new = AudioClip {
        offset,
        length: end - offset,
        ..audio.clone()
    };
    let (lo, hi) = (offset as f64 / rate, end as f64 / rate);
    (
        new,
        looping.map(|(a, b)| (a.clamp(lo, hi), b.clamp(lo, hi))),
    )
}

/// The loop with its end `edge` pulled to `at`, kept inside the lit part, and
/// at least as long as the subject allows.
fn pull_loop(
    subject: Subject,
    project: &noodle_core::Project,
    section: &Section,
    edge: Edge,
    at: f64,
    rate: f64,
) -> Option<(f64, f64)> {
    let (from, to) = section.looping?;
    let lit_from = section.audio.offset as f64 / rate;
    let lit_to = (section.audio.offset + section.audio.length) as f64 / rate;
    let min = match subject {
        // A clip's loop is counted in the beats of its warp.
        Subject::Clip(id) => {
            let tempo = &project.warp_of(id)?.1.tempo;
            tempo.seconds_at(tempo.beats_at(from) + MIN_LOOP_BEATS) - from
        }
        Subject::Sampler(_) => MIN_SECONDS,
    };
    Some(match edge {
        Edge::LoopStart => (at.clamp(lit_from, (to - min).max(lit_from)), to),
        _ => (from, at.clamp((from + min).min(lit_to), lit_to)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(offset: u64, length: u64) -> AudioClip {
        let mut audio = AudioClip::new("a.wav", length);
        audio.offset = offset;
        audio
    }

    #[test]
    fn the_audio_follows_the_drag_so_the_offset_goes_the_other_way() {
        let original = audio(48_000, 24_000);
        // A second to the right: the file's earlier audio comes under the window.
        let (moved, _) = slipped(&original, 0.5, 48_000.0, 200_000, None);
        assert_eq!(moved.offset, 24_000);
        let (moved, _) = slipped(&original, -0.5, 48_000.0, 200_000, None);
        assert_eq!(moved.offset, 72_000);
    }

    #[test]
    fn the_window_stays_inside_the_file() {
        let original = audio(48_000, 24_000);
        assert_eq!(
            slipped(&original, 10.0, 48_000.0, 200_000, None).0.offset,
            0
        );
        // The window's end can't pass the file's.
        assert_eq!(
            slipped(&original, -10.0, 48_000.0, 200_000, None).0.offset,
            200_000 - 24_000
        );
    }

    #[test]
    fn the_loop_goes_along_and_stops_the_slip_where_it_reaches_the_file() {
        let original = audio(48_000, 48_000);
        let looping = Some((1.2, 1.8));
        let (moved, shifted) = slipped(&original, -0.25, 48_000.0, 480_000, looping);
        assert_eq!(moved.offset, 60_000);
        let (a, b) = shifted.unwrap();
        assert!((a - 1.45).abs() < 1e-9 && (b - 2.05).abs() < 1e-9);
        // Pulled all the way back, the part reaches the file's start with
        // the loop still 0.2 s into it.
        let (moved, shifted) = slipped(&original, 10.0, 48_000.0, 480_000, looping);
        assert_eq!(moved.offset, 0);
        let (a, b) = shifted.unwrap();
        assert!((a - 0.2).abs() < 1e-9 && (b - 0.8).abs() < 1e-9);
        // A loop nearer the part's end than the file's stops it at the file's.
        let (moved, shifted) = slipped(&original, -100.0, 48_000.0, 480_000, Some((1.2, 9.5)));
        assert_eq!(moved.offset, 48_000 + 24_000);
        assert!((shifted.unwrap().1 - 10.0).abs() < 1e-9);
    }

    #[test]
    fn pulling_an_edge_keeps_the_loop_inside_the_part() {
        let original = audio(0, 96_000);
        let (new, looping) = pulled(&original, Edge::Start, 0.5, 48_000.0, Some((0.25, 1.5)));
        assert_eq!((new.offset, new.length), (24_000, 72_000));
        assert_eq!(looping, Some((0.5, 1.5)));
        let (new, looping) = pulled(&original, Edge::End, 1.0, 48_000.0, Some((0.25, 1.5)));
        assert_eq!(new.length, 48_000);
        assert_eq!(looping, Some((0.25, 1.0)));
    }
}
