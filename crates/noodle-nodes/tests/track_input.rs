//! The track input node plays clips where the project puts them.

mod common;

use common::{BEAT, BLOCK, RATE, Rig};

/// A ramp whose value says which frame it is, so any misplaced frame shows.
fn ramp(channel: usize, frame: usize) -> f32 {
    let x = frame as f32 / 100_000.0;
    if channel == 0 { x } else { -x }
}

fn assert_close(got: &[f32], want: impl Fn(usize) -> f32, what: &str) {
    for (i, &g) in got.iter().enumerate() {
        let w = want(i);
        assert!((g - w).abs() < 1e-6, "{what}: frame {i} is {g}, wanted {w}");
    }
}

#[test]
fn a_clip_plays_from_its_offset_at_its_start() {
    let mut rig = Rig::new("plays");
    rig.wav("ramp.wav", 2, 60_000, ramp);
    rig.add_clip(1, "ramp.wav", 2_000, 10_000);
    assert!(rig.problems.is_empty(), "{:?}", rig.problems);
    rig.wait_ready(BEAT - 1_000, 1);

    let (left, right) = rig.play(BEAT - 1_000, BEAT + 12_000);
    // Silence before the clip, then the file from its offset, then silence.
    assert!(left[..1_000].iter().all(|&s| s == 0.0));
    assert_close(&left[1_000..11_000], |i| ramp(0, 2_000 + i), "left");
    assert_close(&right[1_000..11_000], |i| ramp(1, 2_000 + i), "right");
    assert!(left[11_000..].iter().all(|&s| s == 0.0));
    assert!(right[11_000..].iter().all(|&s| s == 0.0));
    assert_eq!(rig.feeds.status(rig.id).underruns, 0);
    // With the clip over, the node hands its stream back.
    rig.run(BEAT + 12_000, BLOCK, true);
    assert_eq!(rig.feeds.status(rig.id).streams, 0);
}

#[test]
fn mono_files_play_in_both_channels() {
    let mut rig = Rig::new("mono");
    rig.wav("mono.wav", 1, 20_000, |_, i| i as f32 / 100_000.0);
    rig.add_clip(0, "mono.wav", 0, 5_000);
    rig.wait_ready(0, 1);
    let (left, right) = rig.play(0, 3_000);
    assert_eq!(left[1_000..], right[1_000..]);
    assert_close(&left[1_000..], |i| (1_000 + i) as f32 / 100_000.0, "mono");
}

#[test]
fn gain_and_fades_shape_the_clip() {
    let mut rig = Rig::new("fades");
    rig.wav("one.wav", 1, 30_000, |_, _| 1.0);
    let id = rig.add_clip(0, "one.wav", 0, 8_000);
    rig.edit_clip(id, |clip| {
        let noodle_core::ClipContent::Audio(audio) = &mut clip.content;
        audio.gain = 0.5;
        audio.fade_in = 1_000;
        audio.fade_out = 2_000;
    });
    rig.wait_ready(0, 1);
    let (left, _) = rig.play(0, 9_000);
    // The node's own start-up fade covers the first 240 frames, so look past it.
    assert_close(
        &left[300..1_000],
        |i| 0.5 * ((300 + i) as f32 + 0.5) / 1_000.0,
        "fade in",
    );
    assert_close(&left[1_000..6_000], |_| 0.5, "body");
    assert_close(
        &left[6_000..8_000],
        |i| 0.5 * ((2_000 - i) as f32 - 0.5) / 2_000.0,
        "fade out",
    );
    assert!(left[8_000..].iter().all(|&s| s == 0.0));
}

#[test]
fn a_later_clip_cuts_an_earlier_one_which_does_not_resume() {
    let mut rig = Rig::new("overlap");
    rig.wav("a.wav", 1, 60_000, |_, _| 0.25);
    rig.wav("b.wav", 1, 60_000, |_, _| 0.75);
    rig.add_clip(0, "a.wav", 0, 40_000);
    // Starts at beat 1 (frame 24 000) and ends before A would have.
    rig.add_clip(1, "b.wav", 0, 6_000);
    rig.wait_ready(0, 1);
    let (left, _) = rig.play(0, 40_000);
    assert_close(&left[500..24_000], |_| 0.25, "A before B");
    assert_close(&left[24_000..30_000], |_| 0.75, "B");
    assert!(
        left[30_000..].iter().all(|&s| s == 0.0),
        "A must not resume after B"
    );
}

#[test]
fn stopping_fades_out_and_starting_again_picks_up_in_place() {
    let mut rig = Rig::new("stop");
    rig.wav("ramp.wav", 1, 60_000, |_, i| 0.2 + i as f32 / 100_000.0);
    rig.add_clip(0, "ramp.wav", 0, 50_000);
    rig.wait_ready(0, 1);
    let (played, _) = rig.play(0, 5_000);
    let before = *played.last().unwrap();
    assert!(before > 0.2);

    // Stop: the playhead holds at 5000 and the output ramps to silence
    // without a jump.
    let mut stopped = Vec::new();
    for _ in 0..4 {
        stopped.extend(rig.run(5_000, BLOCK, false).0);
    }
    let biggest_step = std::iter::once(before)
        .chain(stopped.iter().copied())
        .collect::<Vec<_>>()
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(biggest_step < 0.01, "stopping jumped by {biggest_step}");
    assert!(stopped[300..].iter().all(|&s| s == 0.0), "stays silent");

    // Start again from the same place, once the stream has had time to line up.
    std::thread::sleep(std::time::Duration::from_millis(100));
    rig.run(5_000, BLOCK, false);
    let (resumed, _) = rig.play(5_000, 6_000);
    // Sound from the first frame, rising over the node's 240-frame fade.
    assert_close(
        &resumed,
        |i| (0.2 + (5_000 + i) as f32 / 100_000.0) * ((i + 1) as f32 / 240.0).min(1.0),
        "after restart",
    );
}

#[test]
fn a_seek_resumes_at_the_new_place() {
    let mut rig = Rig::new("seek");
    rig.wav("ramp.wav", 1, 60_000, |_, i| i as f32 / 100_000.0);
    rig.add_clip(0, "ramp.wav", 0, 50_000);
    rig.wait_ready(0, 1);
    rig.play(0, 3_000);
    // The engine fades out, jumps, and resets the node.
    rig.run(3_000, BLOCK, false);
    rig.node.reset();
    rig.run(20_000, BLOCK, false);
    std::thread::sleep(std::time::Duration::from_millis(150));
    rig.run(20_000, BLOCK, false);
    let (left, _) = rig.play(20_000, 21_000);
    assert_close(
        &left[300..],
        |i| (20_300 + i) as f32 / 100_000.0,
        "after seek",
    );
}

#[test]
fn an_edit_far_from_the_playhead_is_not_heard() {
    let mut rig = Rig::new("far-edit");
    rig.wav("ramp.wav", 1, 60_000, |_, i| 0.1 + i as f32 / 200_000.0);
    rig.add_clip(0, "ramp.wav", 0, 50_000);
    rig.wait_ready(0, 1);
    rig.play(0, 2_000);
    // A clip added well after the playhead, and one moved there.
    rig.wav("late.wav", 1, 10_000, |_, _| 0.5);
    rig.add_clip(8, "late.wav", 0, 5_000);
    rig.settle();
    let (left, _) = rig.play(2_000, 4_000);
    assert_close(&left, |i| 0.1 + (2_000 + i) as f32 / 200_000.0, "unbroken");
}

#[test]
fn an_edit_to_the_clip_that_is_playing_dips_and_returns() {
    let mut rig = Rig::new("dip");
    rig.wav("one.wav", 1, 60_000, |_, _| 1.0);
    let id = rig.add_clip(0, "one.wav", 0, 50_000);
    rig.wait_ready(0, 1);
    rig.play(0, 2_000);
    rig.edit_clip(id, |clip| {
        let noodle_core::ClipContent::Audio(audio) = &mut clip.content;
        audio.gain = 0.5;
    });
    rig.settle();
    let (left, _) = rig.play(2_000, 3_000);
    // Down to silence, then back up to the new level, with no jump.
    let min = left.iter().copied().fold(f32::MAX, f32::min);
    assert!(min < 0.01, "never dipped: {min}");
    assert!((left[left.len() - 1] - 0.5).abs() < 1e-6);
    let biggest_step = left
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(biggest_step < 0.01, "jumped by {biggest_step}");
}

#[test]
fn a_clip_whose_file_is_missing_is_reported_and_the_rest_play() {
    let mut rig = Rig::new("missing");
    rig.wav("here.wav", 1, 20_000, |_, _| 0.5);
    let missing = rig.add_clip(0, "gone.wav", 0, 5_000);
    let here = rig.add_clip(1, "here.wav", 0, 5_000);
    assert_eq!(rig.problems.len(), 1);
    assert_eq!(rig.problems[0].clip, missing);
    assert!(rig.problems[0].message.contains("gone.wav"));
    rig.wait_ready(BEAT, 1);
    let (left, _) = rig.play(BEAT, BEAT + 2_000);
    assert_close(&left[300..], |_| 0.5, "the clip that exists");
    let _ = (here, RATE);
}

#[test]
fn a_clip_past_the_end_of_its_file_is_left_out() {
    let mut rig = Rig::new("past-end");
    rig.wav("short.wav", 1, 1_000, |_, _| 0.5);
    rig.add_clip(0, "short.wav", 5_000, 100);
    assert_eq!(rig.problems.len(), 1);
}

/// Plays a looping `rig` for `laps` laps from the loop's start and returns
/// each lap's left channel.
fn laps(rig: &mut Rig, laps: usize) -> Vec<Vec<f32>> {
    let (start, end) = rig.looping.unwrap();
    let len = (end - start) as usize;
    let (left, _) = rig.play_looping(start, len * laps);
    left.chunks(len).map(<[f32]>::to_vec).collect()
}

#[test]
fn a_loop_repeats_without_a_gap() {
    let mut rig = Rig::new("loop-gapless");
    rig.wav("one.wav", 2, 200_000, |_, _| 1.0);
    rig.add_clip(0, "one.wav", 0, 150_000);
    rig.looping = Some((0, BEAT));
    rig.wait_ready(0, 1);
    let laps = laps(&mut rig, 9);
    // The first lap fades in; every later one is solid from its first frame.
    for (n, lap) in laps.iter().enumerate().skip(1) {
        let first_zero = lap.iter().position(|&s| s == 0.0);
        assert_eq!(first_zero, None, "lap {n} has a silent frame");
        assert!(lap.iter().all(|&s| s == 1.0), "lap {n} isn't solid");
    }
}

#[test]
fn a_loop_that_starts_inside_a_clip_repeats_the_same_audio() {
    let mut rig = Rig::new("loop-inside");
    rig.wav("ramp.wav", 2, 200_000, ramp);
    rig.add_clip(0, "ramp.wav", 0, 150_000);
    rig.looping = Some((12_000, 36_000));
    rig.wait_ready(12_000, 1);
    let laps = laps(&mut rig, 9);
    for (n, lap) in laps.iter().enumerate().skip(1) {
        assert_close(lap, |i| ramp(0, 12_000 + i), &format!("lap {n}"));
    }
}

#[test]
fn clips_that_start_inside_a_loop_are_ready_for_every_lap() {
    let mut rig = Rig::new("loop-clips");
    rig.wav("ramp.wav", 2, 200_000, ramp);
    // The loop is longer than the hub looks ahead, so each clip's stream is
    // opened when the playhead gets near, in time for the lap. One clip
    // starts at the loop's start, the other runs to its end.
    rig.add_clip(0, "ramp.wav", 0, 20_000);
    rig.add_clip(3, "ramp.wav", 30_000, 24_000);
    rig.looping = Some((0, 4 * BEAT));
    rig.wait_ready(0, 1);
    let laps = laps(&mut rig, 4);
    for (n, lap) in laps.iter().enumerate().skip(1) {
        assert_close(
            &lap[..20_000],
            |i| ramp(0, i),
            &format!("lap {n}, first clip"),
        );
        assert!(lap[20_000..3 * BEAT as usize].iter().all(|&s| s == 0.0));
        assert_close(
            &lap[3 * BEAT as usize..],
            |i| ramp(0, 30_000 + i),
            &format!("lap {n}, second clip"),
        );
    }
}

#[test]
fn a_clip_cut_off_by_the_wrap_starts_afresh_next_lap() {
    let mut rig = Rig::new("loop-cut");
    rig.wav("ramp.wav", 2, 200_000, ramp);
    // Starts half way round the loop and would run on past the end.
    rig.add_clip(2, "ramp.wav", 0, 100_000);
    rig.looping = Some((0, 4 * BEAT));
    rig.settle();
    let laps = laps(&mut rig, 4);
    for (n, lap) in laps.iter().enumerate().skip(1) {
        assert!(
            lap[..2 * BEAT as usize].iter().all(|&s| s == 0.0),
            "lap {n} isn't silent before the clip"
        );
        assert_close(
            &lap[2 * BEAT as usize..],
            |i| ramp(0, i),
            &format!("lap {n}, clip"),
        );
    }
}

#[test]
fn a_file_that_fails_to_open_is_tried_again_later() {
    let mut rig = Rig::new("retry");
    rig.wav("file.wav", 1, 20_000, |_, _| 0.5);
    rig.add_clip(0, "file.wav", 0, 5_000);
    // The file is damaged after the project looked at it, and mended later.
    std::fs::write(rig.dir.join("file.wav"), b"not audio").unwrap();
    let started = std::time::Instant::now();
    while rig.feeds.status(rig.id).errors.is_empty() {
        rig.run(0, BLOCK, false);
        assert!(started.elapsed().as_secs() < 5, "no error was reported");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    rig.wav("file.wav", 1, 20_000, |_, _| 0.5);
    rig.wait_ready(0, 1);
    assert!(rig.feeds.status(rig.id).errors.is_empty());
    let (left, _) = rig.play(0, 2_000);
    assert_close(&left[300..], |_| 0.5, "after the retry");
}

#[test]
fn the_feeds_forget_nodes_that_are_gone() {
    let rig = Rig::new("forget");
    let (feeds, dir) = (rig.feeds.clone(), rig.dir.clone());
    assert_eq!(feeds.tracked(), 1);
    drop(rig);
    let table =
        noodle_engine::TempoTable::new(noodle_core::Project::new().tempo_map(), RATE as f32);
    let started = std::time::Instant::now();
    while feeds.tracked() > 0 {
        feeds.update(&noodle_core::Project::new(), &table, RATE, &dir);
        assert!(
            started.elapsed().as_secs() < 5,
            "the node was never forgotten"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn an_offline_render_does_not_wait_on_a_file_that_cannot_be_opened() {
    let mut rig = Rig::with_feeds("blocking-gone", noodle_nodes::ClipFeeds::blocking());
    rig.wav("file.wav", 1, 20_000, |_, _| 0.5);
    rig.add_clip(0, "file.wav", 0, 5_000);
    // The file goes after the project was scheduled.
    std::fs::remove_file(rig.dir.join("file.wav")).unwrap();
    let started = std::time::Instant::now();
    for block in 0..20 {
        let (left, _) = rig.run(block * BLOCK as u64, BLOCK, true);
        assert!(left.iter().all(|&s| s == 0.0));
    }
    assert!(
        started.elapsed().as_secs() < 5,
        "20 blocks took {:?}",
        started.elapsed()
    );
    let errors = rig.feeds.status(rig.id).errors;
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, rig.dir.join("file.wav"));
    assert_eq!(rig.feeds.errors(), errors);
}

#[test]
fn each_bad_file_is_named_separately() {
    let mut rig = Rig::new("named-errors");
    rig.wav("a.wav", 1, 20_000, |_, _| 0.5);
    rig.wav("b.wav", 1, 20_000, |_, _| 0.5);
    rig.add_clip(0, "a.wav", 0, 5_000);
    rig.add_clip(0, "b.wav", 6_000, 5_000);
    std::fs::write(rig.dir.join("a.wav"), b"not audio").unwrap();
    std::fs::write(rig.dir.join("b.wav"), b"not audio").unwrap();
    let started = std::time::Instant::now();
    loop {
        rig.run(0, BLOCK, false);
        let errors = rig.feeds.status(rig.id).errors;
        if errors.len() == 2 {
            let mut names: Vec<_> = errors
                .iter()
                .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            names.sort();
            assert_eq!(names, ["a.wav", "b.wav"]);
            break;
        }
        assert!(started.elapsed().as_secs() < 5, "got {errors:?}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn an_error_stays_while_the_schedule_changes() {
    let mut rig = Rig::new("steady-errors");
    rig.wav("a.wav", 1, 20_000, |_, _| 0.5);
    let clip = rig.add_clip(0, "a.wav", 0, 5_000);
    std::fs::write(rig.dir.join("a.wav"), b"not audio").unwrap();
    let started = std::time::Instant::now();
    while rig.feeds.status(rig.id).errors.is_empty() {
        rig.run(0, BLOCK, false);
        assert!(started.elapsed().as_secs() < 5, "no error was reported");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // Move the clip about, as a drag does; the error must never vanish.
    for beat in 1..200u64 {
        rig.edit_clip(clip, |c| c.start = noodle_core::Tick(beat as i64 * 10));
        rig.run(0, BLOCK, false);
        for _ in 0..20 {
            assert!(
                !rig.feeds.status(rig.id).errors.is_empty(),
                "the error vanished at step {beat}"
            );
        }
    }
}
