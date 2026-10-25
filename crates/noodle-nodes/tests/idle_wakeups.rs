//! A track input that has nothing to do doesn't wake any thread: the tasks
//! that open its streams and stretch its clips sleep until rung.

mod common;

use std::time::Duration;

use common::{BEAT, BLOCK, Rig};

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Runs blocks that go nowhere for about `ms`, as a stopped transport does.
fn idle_for(rig: &mut Rig, position: u64, ms: u64) {
    for _ in 0..ms / 5 {
        rig.run_quiet(position, BLOCK, false);
        sleep(5);
    }
}

#[test]
fn a_stopped_track_with_no_clip_in_reach_wakes_nothing() {
    let mut rig = Rig::new("idle-far");
    rig.wav("far.wav", 1, 50_000, |_, i| (i as f32 * 0.01).sin());
    // Many seconds ahead of the playhead, beyond the lookahead.
    rig.add_clip(100, "far.wav", 0, 40_000);
    // Let the first schedule arrive and be taken in.
    idle_for(&mut rig, 0, 400);
    let before = rig.feeds.background_runs(rig.id);
    idle_for(&mut rig, 0, 200);
    assert_eq!(rig.feeds.background_runs(rig.id), before);
}

#[test]
fn a_stopped_track_with_open_streams_wakes_nothing() {
    let mut rig = Rig::new("idle-open");
    rig.wav("a.wav", 2, 100_000, |c, i| {
        (i as f32 * 0.01 + c as f32).sin()
    });
    rig.add_clip(0, "a.wav", 0, 90_000);
    rig.wait_ready(0, 1);
    idle_for(&mut rig, 0, 300);
    let before = rig.feeds.background_runs(rig.id);
    idle_for(&mut rig, 0, 200);
    assert_eq!(rig.feeds.background_runs(rig.id), before);
}

#[test]
fn moving_the_playhead_wakes_the_hub_to_open_what_comes_next() {
    let mut rig = Rig::new("idle-move");
    rig.wav("a.wav", 1, 200_000, |_, i| (i as f32 * 0.01).sin());
    rig.wav("b.wav", 1, 60_000, |_, i| (i as f32 * 0.02).sin());
    rig.add_clip(0, "a.wav", 0, 200_000);
    // Four beats is two seconds: beyond the one second the hub looks ahead.
    rig.add_clip(4, "b.wav", 0, 50_000);
    rig.wait_ready(0, 1);
    idle_for(&mut rig, 0, 200);
    assert_eq!(rig.feeds.status(rig.id).streams, 1);
    let mut at = 0;
    // Play on until b's start is within reach, then wait for its stream.
    while at < 3 * BEAT {
        rig.run_quiet(at, BLOCK, true);
        at += BLOCK as u64;
        sleep(1);
    }
    rig.wait_ready(at, 2);
}
