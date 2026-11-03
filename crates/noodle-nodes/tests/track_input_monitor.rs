//! The track input plays the device input, by its `monitor` setting.

mod common;

use std::sync::Arc;

use common::{BEAT, BLOCK, Rig};
use noodle_engine::InputTap;
use noodle_nodes::{ClipFeeds, parse_input_channels};

const CLIP: f32 = 0.5;
const LIVE: f32 = 0.25;

/// A rig with a clip of a constant `CLIP` from the start, a device input of a
/// constant `LIVE` on its first channel, and the monitor set to `mode`.
fn monitored(name: &str, mode: u8) -> (Rig, Arc<InputTap>) {
    let feeds = ClipFeeds::blocking();
    let tap = Arc::new(InputTap::new(BLOCK));
    feeds.set_input_tap(Some(tap.clone()));
    let mut rig = Rig::with_feeds(name, feeds);
    rig.wav("tone.wav", 1, 4 * BEAT as usize, |_, _| CLIP);
    rig.add_clip(0, "tone.wav", 0, 4 * BEAT);
    rig.settle();
    rig.set_monitor(mode);
    (rig, tap)
}

/// Runs blocks from `at` (long enough for a crossfade to finish) and returns
/// the last sample of the left channel.
fn settle_to(rig: &mut Rig, tap: &InputTap, at: u64, playing: bool) -> f32 {
    let mut last = 0.0;
    for k in 0..8 {
        tap.write(&vec![LIVE; BLOCK], 1);
        let (left, _) = rig.run(at + k * BLOCK as u64, BLOCK, playing);
        last = *left.last().unwrap();
    }
    last
}

#[test]
fn in_plays_the_input_and_off_the_clips() {
    let (mut rig, tap) = monitored("monitor-in", 2);
    assert!((settle_to(&mut rig, &tap, 0, true) - LIVE).abs() < 1e-4);
    let (mut rig, tap) = monitored("monitor-off", 0);
    assert!((settle_to(&mut rig, &tap, 0, true) - CLIP).abs() < 1e-4);
}

#[test]
fn auto_hears_the_input_while_armed_and_stopped_and_the_clips_while_playing() {
    let (mut rig, tap) = monitored("monitor-auto", 1);
    // Not armed: the clips, whatever the transport does.
    assert!((settle_to(&mut rig, &tap, 0, true) - CLIP).abs() < 1e-4);
    rig.feeds.set_audio_armed(rig.id, true);
    // Armed and playing, but not recording: still the old take.
    assert!((settle_to(&mut rig, &tap, 8 * BLOCK as u64, true) - CLIP).abs() < 1e-4);
    // Stopped: the player hears themselves.
    let stopped = settle_to(&mut rig, &tap, 16 * BLOCK as u64, false);
    assert!((stopped - LIVE).abs() < 1e-4, "{stopped}");
    // Playing again goes back to the clips.
    assert!((settle_to(&mut rig, &tap, 24 * BLOCK as u64, true) - CLIP).abs() < 1e-4);
}

#[test]
fn auto_switches_at_the_punch_in_and_out_while_recording() {
    let (mut rig, tap) = monitored("monitor-punch", 1);
    rig.feeds.set_audio_armed(rig.id, true);
    let (punch_in, punch_out) = (BEAT, 2 * BEAT);
    rig.feeds
        .set_recording(rig.id, Some((punch_in, Some(punch_out))));
    let before = settle_to(&mut rig, &tap, 0, true);
    assert!(
        (before - CLIP).abs() < 1e-4,
        "pre-roll plays the old take: {before}"
    );
    let inside = settle_to(&mut rig, &tap, punch_in + 100, true);
    assert!((inside - LIVE).abs() < 1e-4, "punched in: {inside}");
    let after = settle_to(&mut rig, &tap, punch_out + 100, true);
    assert!((after - CLIP).abs() < 1e-4, "punched out: {after}");
}

#[test]
fn a_node_with_no_tap_plays_only_its_clips() {
    let mut rig = Rig::blocking("monitor-no-tap");
    rig.wav("tone.wav", 1, BLOCK * 8, |_, _| CLIP);
    rig.add_clip(0, "tone.wav", 0, BLOCK as u64 * 8);
    rig.settle();
    rig.set_monitor(2);
    let (left, _) = rig.run(0, BLOCK, true);
    assert!((left[BLOCK - 1] - CLIP).abs() < 1e-4);
}

#[test]
fn input_channels_are_numbered_from_one() {
    assert_eq!(parse_input_channels("").unwrap(), [0, 1]);
    assert_eq!(parse_input_channels("3").unwrap(), [2, 2]);
    assert_eq!(parse_input_channels("3 4").unwrap(), [2, 3]);
    assert_eq!(parse_input_channels("3,4").unwrap(), [2, 3]);
    assert!(parse_input_channels("0").is_err());
    assert!(parse_input_channels("1 2 3").is_err());
    assert!(parse_input_channels("left").is_err());
}
