//! Audio clips with a warp play back at the speed the warp gives them (Repitch:
//! the pitch follows), at the same samples whatever the block size, with no
//! allocation on the audio thread.

use std::path::{Path, PathBuf};

use noodle_core::{
    ClipContent, ClipWarp, Command, Connection, Endpoint, History, Node, NodeId, Project,
    SignatureChange, TempoChange, TempoMap, Tick, TimeSignature, WarpMarker,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};
use noodle_io::write_wav;
use noodle_nodes::{ClipRender, TRACK_INPUT_ID, render_project_with_clips};

const RATE: u32 = 48_000;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: 512,
    channels: 2,
};
/// Frames the node takes to fade in at the start.
const FADE: usize = 300;

/// A ramp whose value says which frame it is, so a warp's work shows as the
/// straight line it makes.
fn ramp(frame: f64) -> f32 {
    (frame / 10_000.0) as f32
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("noodle-warped-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    // Four seconds of it, mono-ish stereo.
    let samples: Vec<f32> = (0..192_000)
        .flat_map(|i| [ramp(i as f64), -ramp(i as f64)])
        .collect();
    write_wav(&dir.join("ramp.wav"), &samples, 2, RATE).unwrap();
    dir
}

/// A track input wired to the output, with one clip of `length` frames of
/// `ramp.wav` from `offset`, warped by `warp`.
fn project(warp: Option<ClipWarp>, offset: u64, length: u64) -> Project {
    section(warp, offset, length, None)
}

/// Like [`project`], with the section dragged out to `extent` ticks.
fn section(warp: Option<ClipWarp>, offset: u64, length: u64, extent: Option<Tick>) -> Project {
    let mut project = Project::new();
    let mut history = History::new();
    let (track, out) = (NodeId(1), NodeId(2));
    for (id, kind) in [(track, TRACK_INPUT_ID), (out, OUTPUT_ID)] {
        history
            .apply(
                &mut project,
                Command::AddNode {
                    id,
                    node: Node::new(kind),
                },
            )
            .unwrap();
    }
    let connection = Connection {
        from: Endpoint::new(track, "audio"),
        to: Endpoint::new(out, "in"),
    };
    history
        .apply(&mut project, Command::Connect(connection))
        .unwrap();
    let id = project.new_clip_id();
    let mut clip = noodle_core::Clip::audio(track, Tick(0), "ramp.wav", length);
    let ClipContent::Audio(audio) = &mut clip.content else {
        unreachable!("not an audio clip")
    };
    audio.offset = offset;
    audio.extent = extent;
    let mut commands = Vec::new();
    if let Some(warp) = warp {
        let warp_id = project.next_warp_id();
        audio.warp = Some(warp_id);
        commands.push(Command::AddWarp { id: warp_id, warp });
    }
    commands.push(Command::AddClip { id, clip });
    history
        .apply(&mut project, Command::Batch(commands))
        .unwrap();
    project
}

fn render(project: &Project, dir: &Path, settings: Settings, frames: usize) -> ClipRender {
    let mut registry = Registry::with_builtins();
    let rendered =
        render_project_with_clips(project, &mut registry, dir, settings, frames).unwrap();
    assert!(
        rendered.render.diagnostics.is_empty(),
        "{:?}",
        rendered.render.diagnostics
    );
    assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
    assert_eq!(rendered.underruns, 0);
    rendered
}

/// Checks that the left channel over `from..to` follows the source: output
/// frame `f` plays source frame `source(f)`.
fn follows(samples: &[f32], from: usize, to: usize, source: impl Fn(f64) -> f64) {
    for f in from..to {
        let got = samples[f * 2];
        let want = ramp(source(f as f64));
        assert!(
            (got - want).abs() < 2e-5,
            "frame {f} is {got}, wanted {want}"
        );
    }
}

fn silent(samples: &[f32], from: usize, to: usize) {
    for f in from..to {
        assert_eq!(samples[f * 2], 0.0, "frame {f} isn't silent");
    }
}

#[test]
fn a_clip_at_the_timeline_tempo_plays_as_recorded() {
    let dir = folder("same");
    let project = project(Some(ClipWarp::at_tempo(120.0)), 0, 96_000);
    let samples = render(&project, &dir, SETTINGS, 120_000).render.samples;
    follows(&samples, FADE, 96_000, |f| f);
    silent(&samples, 96_000, 120_000);
}

#[test]
fn a_clip_recorded_slower_than_the_timeline_plays_faster_and_ends_sooner() {
    let dir = folder("faster");
    // Recorded at 60 bpm on a 120 bpm timeline: twice as fast, half as long.
    let project = project(Some(ClipWarp::at_tempo(60.0)), 0, 96_000);
    let samples = render(&project, &dir, SETTINGS, 120_000).render.samples;
    follows(&samples, FADE, 48_000, |f| f * 2.0);
    silent(&samples, 48_000 + 8, 120_000);
}

#[test]
fn a_clip_recorded_faster_than_the_timeline_plays_slower_and_ends_later() {
    let dir = folder("slower");
    let project = project(Some(ClipWarp::at_tempo(240.0)), 0, 48_000);
    let samples = render(&project, &dir, SETTINGS, 120_000).render.samples;
    follows(&samples, FADE, 96_000 - 2, |f| f * 0.5);
    silent(&samples, 96_000 + 8, 120_000);
}

#[test]
fn with_the_conversion_off_the_timeline_tempo_does_not_matter() {
    let dir = folder("off");
    let mut warp = ClipWarp::at_tempo(60.0);
    warp.tempo_conversion = false;
    let project = project(Some(warp), 0, 48_000);
    let samples = render(&project, &dir, SETTINGS, 80_000).render.samples;
    follows(&samples, FADE, 48_000, |f| f);
    silent(&samples, 48_000 + 8, 80_000);
}

#[test]
fn markers_stretch_the_audio_between_them() {
    let dir = folder("markers");
    // 120 bpm on a 120 timeline. The moment at 1 s stays on beat 2; the one
    // at 2 s is pulled back to beat 5 (2.5 s), so the second between them
    // takes a second and a half.
    let mut warp = ClipWarp::at_tempo(120.0);
    warp.set_marker(WarpMarker {
        seconds: 1.0,
        beat: 2.0,
    });
    warp.set_marker(WarpMarker {
        seconds: 2.0,
        beat: 5.0,
    });
    let project = project(Some(warp), 0, 4 * 48_000);
    let rendered = render(&project, &dir, SETTINGS, 5 * 48_000);
    let samples = &rendered.render.samples;
    follows(samples, FADE, 48_000, |f| f);
    follows(samples, 48_000, 120_000, |f| {
        48_000.0 + (f - 48_000.0) * 2.0 / 3.0
    });
    // After the second marker it plays at the clip's own tempo again, and the
    // clip is half a second longer than its audio.
    follows(samples, 120_000, 216_000 - 2, |f| {
        96_000.0 + (f - 120_000.0)
    });
    silent(samples, 216_000 + 8, 5 * 48_000);
}

#[test]
fn markers_that_are_switched_off_do_nothing() {
    let dir = folder("markers-off");
    let mut warp = ClipWarp::at_tempo(120.0);
    warp.set_marker(WarpMarker {
        seconds: 1.0,
        beat: 2.0,
    });
    warp.set_marker(WarpMarker {
        seconds: 2.0,
        beat: 5.0,
    });
    warp.use_markers = false;
    let project = project(Some(warp), 0, 96_000);
    let samples = render(&project, &dir, SETTINGS, 100_000).render.samples;
    follows(&samples, FADE, 96_000, |f| f);
}

#[test]
fn a_section_showing_part_of_a_warped_clip_plays_that_part() {
    let dir = folder("part");
    let mut warp = ClipWarp::at_tempo(120.0);
    warp.set_marker(WarpMarker {
        seconds: 1.0,
        beat: 2.0,
    });
    warp.set_marker(WarpMarker {
        seconds: 2.0,
        beat: 5.0,
    });
    // From 1.5 s to 3 s of the file: half a second of the slow part, then the
    // rest at the clip's tempo.
    let project = project(Some(warp), 72_000, 72_000);
    let samples = render(&project, &dir, SETTINGS, 120_000).render.samples;
    // 0.5 s of the file takes 0.75 s.
    follows(&samples, FADE, 36_000, |f| 72_000.0 + f * 2.0 / 3.0);
    follows(&samples, 36_000, 36_000 + 48_000 - 2, |f| {
        96_000.0 + (f - 36_000.0)
    });
    silent(&samples, 36_000 + 48_000 + 8, 120_000);
}

#[test]
fn the_timeline_tempo_changing_changes_the_speed_there() {
    let dir = folder("tempo-change");
    let mut project = project(Some(ClipWarp::at_tempo(120.0)), 0, 4 * 48_000);
    // 120 bpm for four beats (2 s), then 60.
    let map = TempoMap::new(
        vec![
            TempoChange {
                tick: Tick(0),
                bpm: 120.0,
            },
            TempoChange {
                tick: Tick(4 * 960),
                bpm: 60.0,
            },
        ],
        vec![SignatureChange {
            bar: 0,
            signature: TimeSignature::COMMON,
        }],
    )
    .unwrap();
    History::new()
        .apply(&mut project, Command::SetTempoMap(map))
        .unwrap();
    let samples = render(&project, &dir, SETTINGS, 8 * 48_000).render.samples;
    follows(&samples, FADE, 96_000, |f| f);
    // Past the tempo change the clip's beats are twice as long.
    follows(&samples, 96_000 + 4, 6 * 48_000 - 2, |f| {
        96_000.0 + (f - 96_000.0) * 0.5
    });
}

#[test]
fn the_block_size_does_not_change_the_sound() {
    let dir = folder("blocks");
    let mut warp = ClipWarp::at_tempo(97.0);
    warp.set_marker(WarpMarker {
        seconds: 0.3,
        beat: 0.2,
    });
    warp.set_marker(WarpMarker {
        seconds: 2.1,
        beat: 4.9,
    });
    let project = project(Some(warp), 10_000, 150_000);
    let reference = render(&project, &dir, SETTINGS, 200_000).render.samples;
    for max_frames in [64, 100, 333, 1024] {
        let settings = Settings {
            max_frames,
            ..SETTINGS
        };
        let other = render(&project, &dir, settings, 200_000).render.samples;
        assert_eq!(other[FADE * 2..], reference[FADE * 2..], "{max_frames}");
    }
    // And twice with the same settings.
    assert_eq!(
        render(&project, &dir, SETTINGS, 200_000).render.samples,
        reference
    );
}

#[test]
fn a_warp_that_changes_nothing_sounds_like_no_warp() {
    let dir = folder("identity");
    let mut warp = ClipWarp::at_tempo(80.0);
    warp.tempo_conversion = false;
    let warped = render(&project(Some(warp), 5_000, 60_000), &dir, SETTINGS, 70_000);
    let plain = render(&project(None, 5_000, 60_000), &dir, SETTINGS, 70_000);
    assert_eq!(warped.render.samples, plain.render.samples);
}

#[test]
fn a_stereo_pair_stays_a_pair() {
    let dir = folder("stereo");
    let project = project(Some(ClipWarp::at_tempo(90.0)), 0, 96_000);
    let samples = render(&project, &dir, SETTINGS, 130_000).render.samples;
    // Recorded at 90 on a timeline at 120: three quarters as long.
    for f in FADE..72_000 {
        assert!(
            (samples[f * 2] + samples[f * 2 + 1]).abs() < 1e-6,
            "frame {f}"
        );
    }
    assert!(samples[50_000 * 2].abs() > 0.1);
    silent(&samples, 72_000 + 8, 130_000);
}

#[test]
fn fades_are_stretched_with_the_audio() {
    let dir = folder("fades");
    // Twice as fast: a fade over 4800 frames of the file lasts 2400 frames, and
    // a fade out over 9600 lasts 4800.
    let mut project = project(Some(ClipWarp::at_tempo(60.0)), 0, 96_000);
    let id = project.clips().next().unwrap().0;
    let mut clip = project.clip(id).unwrap().clone();
    let ClipContent::Audio(audio) = &mut clip.content else {
        unreachable!("not an audio clip")
    };
    audio.fade_in = 4_800;
    audio.fade_out = 9_600;
    History::new()
        .apply(&mut project, Command::SetClip { id, clip })
        .unwrap();
    let samples = render(&project, &dir, SETTINGS, 60_000).render.samples;
    // The node's own 5 ms fade-in is over by 600 frames.
    for f in 600..2_400 {
        let gain = (f as f32 + 0.5) / 2_400.0;
        let want = ramp(f as f64 * 2.0) * gain;
        assert!((samples[f * 2] - want).abs() < 2e-5, "frame {f}");
    }
    follows(&samples, 2_400, 48_000 - 4_800, |f| f * 2.0);
    for f in (48_000 - 4_800 + 2)..48_000 - 2 {
        let gain = (48_000 - f) as f32 - 0.5;
        let want = ramp(f as f64 * 2.0) * gain / 4_800.0;
        assert!((samples[f * 2] - want).abs() < 2e-5, "frame {f}");
    }
}

fn looping(from: f64, to: f64) -> ClipWarp {
    ClipWarp {
        loop_region: Some(noodle_core::LoopRegion {
            start: from,
            end: to,
        }),
        ..ClipWarp::at_tempo(120.0)
    }
}

/// Where a section that plays up to 72000, then repeats frames 24000..72000,
/// reads at output frame `f`.
fn two_beat_loop(f: f64) -> f64 {
    if f < 72_000.0 {
        f
    } else {
        24_000.0 + (f - 72_000.0) % 48_000.0
    }
}

#[test]
fn a_looping_section_repeats_the_loop_until_it_ends() {
    let dir = folder("loop");
    // Beats 1 to 3 repeat; the section is dragged out to 12 beats (6 s) though
    // its audio lasts 4 s.
    let project = section(Some(looping(1.0, 3.0)), 0, 4 * 48_000, Some(Tick(12 * 960)));
    let samples = render(&project, &dir, SETTINGS, 7 * 48_000).render.samples;
    follows(&samples, FADE, 6 * 48_000 - 4, two_beat_loop);
    silent(&samples, 6 * 48_000 + 8, 7 * 48_000);
}

#[test]
fn a_looping_section_is_the_same_whatever_the_block_size() {
    let dir = folder("loop-blocks");
    let project = section(Some(looping(1.0, 3.0)), 0, 4 * 48_000, Some(Tick(12 * 960)));
    let small = Settings {
        max_frames: 64,
        ..SETTINGS
    };
    let a = render(&project, &dir, SETTINGS, 7 * 48_000).render.samples;
    let b = render(&project, &dir, small, 7 * 48_000).render.samples;
    assert_eq!(a, b);
}

#[test]
fn a_section_starting_inside_the_loop_joins_it_at_the_end() {
    let dir = folder("loop-inside");
    // The section shows the file from beat 2 (1 s); it plays on to the loop's
    // end at beat 3 and goes round from beat 1.
    let project = section(
        Some(looping(1.0, 3.0)),
        48_000,
        3 * 48_000,
        Some(Tick(8 * 960)),
    );
    let samples = render(&project, &dir, SETTINGS, 5 * 48_000).render.samples;
    follows(&samples, FADE, 4 * 48_000 - 4, |f| {
        if f < 24_000.0 {
            48_000.0 + f
        } else {
            24_000.0 + (f - 24_000.0) % 48_000.0
        }
    });
    silent(&samples, 4 * 48_000 + 8, 5 * 48_000);
}

#[test]
fn a_looping_clip_follows_the_timeline_tempo() {
    let dir = folder("loop-tempo");
    // Recorded at 60 bpm on a 120 timeline: twice as fast, so the 4 s of
    // audio last 2 s and the 2-beat loop (2 s of audio) lasts 1 s.
    let mut warp = looping(1.0, 3.0);
    warp.tempo = noodle_core::TempoCurve::constant(60.0);
    let project = section(Some(warp), 0, 4 * 48_000, Some(Tick(8 * 960)));
    let samples = render(&project, &dir, SETTINGS, 5 * 48_000).render.samples;
    // 1 s of audio from beat 1 (1 s in) to beat 3 (3 s in).
    follows(&samples, FADE, 4 * 48_000 - 4, |f| {
        let out = f * 2.0;
        if out < 3.0 * 48_000.0 {
            out
        } else {
            48_000.0 + (out - 3.0 * 48_000.0) % (2.0 * 48_000.0)
        }
    });
    silent(&samples, 4 * 48_000 + 8, 5 * 48_000);
}

#[test]
fn a_section_whose_window_passes_the_loop_end_goes_round_the_loop() {
    let dir = folder("loop-unextended");
    // A 2 s window with beats 1 to 3 (0.5 s to 1.5 s) as the loop: it plays to
    // the loop's end and goes round for the last half second.
    let project = section(Some(looping(1.0, 3.0)), 0, 96_000, None);
    let samples = render(&project, &dir, SETTINGS, 120_000).render.samples;
    follows(&samples, FADE, 96_000 - 4, two_beat_loop);
    silent(&samples, 96_000 + 8, 120_000);
}

#[test]
fn dragging_the_edge_out_never_changes_what_plays_before_it() {
    let dir = folder("loop-continuous");
    // The window ends at 4 beats. Whether the edge sits at the window's end,
    // a tick past it or well past it, the first 4 beats sound the same.
    let reference = render(
        &section(Some(looping(1.0, 3.0)), 0, 96_000, None),
        &dir,
        SETTINGS,
        96_000,
    )
    .render
    .samples;
    for ticks in [4 * 960 + 1, 6 * 960] {
        let project = section(Some(looping(1.0, 3.0)), 0, 96_000, Some(Tick(ticks)));
        let samples = render(&project, &dir, SETTINGS, 96_000).render.samples;
        let worst = (0..2 * 95_990)
            .map(|i| (samples[i] - reference[i]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-6, "{ticks}: {worst}");
    }
}

#[test]
fn a_looping_section_dragged_shorter_than_its_window_ends_there() {
    let dir = folder("loop-shorter");
    // 3 beats of a 4 s window, past the loop's end at beat 3? No: 2 beats,
    // before it, so nothing repeats.
    let project = section(Some(looping(1.0, 3.0)), 0, 4 * 48_000, Some(Tick(2 * 960)));
    let samples = render(&project, &dir, SETTINGS, 4 * 48_000).render.samples;
    follows(&samples, FADE, 48_000 - 4, |f| f);
    silent(&samples, 48_000 + 8, 4 * 48_000);
}

#[test]
fn a_loop_too_short_to_keep_up_with_is_reported_and_not_repeated() {
    let dir = folder("loop-short");
    // 480 bpm, not following the timeline: a beat is 0.125 s, so a one-beat
    // loop would be over faster than a stream can be opened for each round.
    let mut warp = looping(1.0, 2.0);
    warp.tempo = noodle_core::TempoCurve::constant(480.0);
    warp.tempo_conversion = false;
    let project = section(Some(warp), 0, 96_000, Some(Tick(12 * 960)));
    let mut registry = Registry::with_builtins();
    let rendered =
        render_project_with_clips(&project, &mut registry, &dir, SETTINGS, 6 * 48_000).unwrap();
    assert_eq!(rendered.problems.len(), 1, "{:?}", rendered.problems);
    assert!(
        rendered.problems[0].message.contains("loop"),
        "{:?}",
        rendered.problems
    );
    let samples = rendered.render.samples;
    follows(&samples, FADE, 96_000 - 4, |f| f);
    silent(&samples, 96_000 + 8, 6 * 48_000);
}

#[test]
fn a_fade_out_longer_than_the_last_pass_is_held_to_it() {
    let dir = folder("loop-fade");
    // 8 beats (4 s): 1.5 s to the loop's end, two rounds (1 s each) and half
    // a second of the next. A 1 s fade can only fit in that last pass.
    let mut project = section(Some(looping(1.0, 3.0)), 0, 4 * 48_000, Some(Tick(8 * 960)));
    let id = project.clips().next().unwrap().0;
    let mut clip = project.clip(id).unwrap().clone();
    let ClipContent::Audio(audio) = &mut clip.content else {
        unreachable!()
    };
    audio.fade_out = 48_000;
    let mut history = History::new();
    history
        .apply(&mut project, Command::SetClip { id, clip })
        .unwrap();
    let samples = render(&project, &dir, SETTINGS, 5 * 48_000).render.samples;
    // Before the last pass (it starts at 3.5 s) the gain is 1.
    for f in 6_000..168_000 - 4 {
        let want = ramp(two_beat_loop(f as f64));
        assert!((samples[f * 2] - want).abs() < 2e-5, "frame {f}");
    }
    // Inside it the gain falls to 0 by the end at 4 s.
    let end = 4 * 48_000;
    let early = samples[(168_000 + 1_000) * 2] / ramp(two_beat_loop(169_000.0));
    let late = samples[(end - 1_000) * 2] / ramp(two_beat_loop((end - 1_000) as f64));
    assert!(early > 0.9 && late < 0.2, "{early} {late}");
}

#[test]
fn a_looping_section_ending_part_way_through_a_repeat_stops_there() {
    let dir = folder("loop-cut");
    // 9 beats: the 4 s of audio, then 1 s of the loop.
    let project = section(Some(looping(1.0, 3.0)), 0, 4 * 48_000, Some(Tick(9 * 960)));
    let samples = render(&project, &dir, SETTINGS, 6 * 48_000).render.samples;
    follows(&samples, FADE, 4 * 48_000 + 24_000 - 4, two_beat_loop);
    silent(&samples, 4 * 48_000 + 24_000 + 8, 6 * 48_000);
}
