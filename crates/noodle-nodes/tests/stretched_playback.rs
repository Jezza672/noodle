//! Stretched clips (Grain, Beats) played live, the way the audio callback
//! does: a worker renders ahead into a ring, and the playhead jumping,
//! looping or stopping makes the worker start over there.

mod common;

use std::time::{Duration, Instant};

use common::{BLOCK, Rig};
use noodle_core::WarpMode;

/// 440 Hz for the first 120 000 frames, then an octave up (phase carried).
fn tone(_: usize, i: usize) -> f32 {
    let phase = if i < 120_000 {
        i as f32 * 440.0
    } else {
        120_000.0 * 440.0 + (i - 120_000) as f32 * 880.0
    };
    (phase / 48_000.0 * std::f32::consts::TAU).sin() * 0.5
}

fn crossings(signal: &[f32]) -> usize {
    signal
        .windows(2)
        .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
        .count()
}

fn peak(signal: &[f32]) -> f32 {
    signal.iter().fold(0.0, |m, x| m.max(x.abs()))
}

/// A rig playing 200 000 frames of a 440 Hz tone recorded at 60 bpm in `mode`
/// on the 120 bpm timeline: twice as fast, 100 000 frames long.
fn rig(name: &str, mode: WarpMode) -> Rig {
    let mut rig = Rig::new(name);
    rig.wav("tone.wav", 2, 200_000, tone);
    let id = rig.add_clip(0, "tone.wav", 0, 200_000);
    rig.warp_clip(id, |warp| {
        warp.tempo = noodle_core::TempoCurve::constant(60.0);
        warp.tempo_conversion = true;
        warp.mode = mode;
    });
    rig.wait_ready(0, 1);
    rig
}

#[test]
fn it_plays_from_the_start_at_the_original_pitch() {
    for mode in [WarpMode::Grain, WarpMode::Beats] {
        let mut rig = rig("play", mode);
        let (left, _) = rig.play(0, 40_000);
        let c = crossings(&left[2_000..38_000]) as i64;
        // 36 000 frames of 440 Hz.
        assert!((c - 330).abs() <= 4, "{mode:?}: {c} cycles");
        assert_eq!(rig.feeds.status(rig.id).underruns, 0, "{mode:?}");
    }
}

#[test]
fn it_picks_up_after_a_jump() {
    let mut rig = rig("jump", WarpMode::Grain);
    rig.play(0, 10_000);
    // Stop, and jump a long way into the clip.
    for _ in 0..4 {
        rig.run(10_000, BLOCK, false);
    }
    rig.node.reset();
    // Output frame 70 000 is source frame 140 000, in the octave up. Silent
    // for a moment while the worker restarts there.
    let mut at = 70_000;
    let started = Instant::now();
    loop {
        let (left, _) = rig.run(at, BLOCK, true);
        at += BLOCK as u64;
        if let Some(first) = left.iter().position(|x| x.abs() > 1e-6) {
            // The declick ramps in from the first real frame, not from
            // somewhere behind the gap: a click otherwise.
            let start = &left[first..(first + 48).min(left.len())];
            assert!(peak(start) < 0.12, "clicked in at {}", peak(start));
        }
        if peak(&left) > 0.1 {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "never sounded");
        std::thread::sleep(Duration::from_millis(1));
    }
    // And from the first sound it is the new tone, with nothing left over from
    // the old place.
    let (left, _) = rig.play(at, at + 10_000);
    let c = crossings(&left) as i64;
    assert!((c - 183).abs() <= 6, "{c} cycles");
    assert!(peak(&left) > 0.4);
}

#[test]
fn a_loop_wraps_without_a_gap() {
    let mut rig = rig("loop", WarpMode::Grain);
    rig.looping = Some((20_000, 60_000));
    rig.node.reset();
    let (left, _) = rig.play_looping(20_000, 5 * 40_000);
    // The tone is steady, so every block of it is loud, including the ones
    // just after each wrap.
    let quiet: Vec<usize> = left
        .chunks(BLOCK)
        .enumerate()
        .filter(|(_, block)| peak(block) < 0.3)
        .map(|(i, _)| i)
        .collect();
    // The first wait for the worker to restart at the loop's start.
    assert!(quiet.iter().all(|i| *i < 40), "quiet blocks: {quiet:?}");
}

#[test]
fn stopping_and_starting_again_resumes_the_clip() {
    let mut rig = rig("stop", WarpMode::Beats);
    rig.play(0, 30_000);
    for _ in 0..8 {
        rig.run(30_000, BLOCK, false);
    }
    rig.node.reset();
    let (left, _) = rig.play(30_000, 50_000);
    assert!(peak(&left[8_000..]) > 0.4);
}
