//! Track inputs share a few threads for their background work instead of
//! starting their own.

#![cfg(target_os = "linux")]

mod common;

use common::Rig;

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").unwrap().count()
}

#[test]
fn sixteen_track_inputs_start_a_pool_not_a_thread_each() {
    let before = threads();
    let mut rigs = Vec::new();
    for i in 0..16 {
        let mut rig = Rig::new(&format!("threads-{i}"));
        rig.wav("a.wav", 2, 50_000, |c, i| {
            (i as f32 * 0.01 + c as f32).sin()
        });
        rig.add_clip(0, "a.wav", 0, 40_000);
        rig.wait_ready(0, 1);
        rigs.push(rig);
    }
    let (io, control) = noodle_io::pool::thread_counts();
    let started = threads() - before;
    assert!(
        started <= io + control + 2,
        "{started} threads for 16 track inputs, with pools of {io} and {control}"
    );
}
