//! The track input's audio-thread code never allocates or frees memory:
//! playing, stopping, seeking, and taking in new schedules and streams
//! while it does.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Duration;

use common::{BEAT, BLOCK, Rig};

struct Guarded;

thread_local! {
    static REALTIME: Cell<bool> = const { Cell::new(false) };
    static VIOLATIONS: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    let _ = REALTIME.try_with(|realtime| {
        if realtime.get() {
            VIOLATIONS.with(|v| v.set(v.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for Guarded {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Guarded = Guarded;

fn realtime(f: impl FnOnce()) -> usize {
    REALTIME.set(true);
    f();
    REALTIME.set(false);
    VIOLATIONS.replace(0)
}

#[test]
fn playing_stopping_seeking_and_edits_never_touch_the_allocator() {
    let mut rig = Rig::new("realtime");
    rig.wav("a.wav", 2, 200_000, |c, i| {
        (i as f32 * 0.01 + c as f32).sin()
    });
    rig.wav("b.wav", 1, 200_000, |_, i| (i as f32 * 0.02).sin());
    let a = rig.add_clip(0, "a.wav", 0, 150_000);
    rig.add_clip(3, "b.wav", 0, 60_000);
    rig.wait_ready(0, 1);

    let mut violations = 0;
    let mut at = 0u64;
    let pause = || std::thread::sleep(Duration::from_millis(2));

    // Play across the start of the second clip, so a stream is taken in and
    // the first is eventually handed back.
    while at < 3 * BEAT + 10_000 {
        violations += realtime(|| rig.run_quiet(at, BLOCK, true));
        at += BLOCK as u64;
        pause();
    }
    // Edits arrive while it plays: one that dips, one that doesn't.
    rig.edit_clip(a, |clip| {
        let noodle_core::ClipContent::Audio(audio) = &mut clip.content else {
            unreachable!("not an audio clip")
        };
        audio.gain = 0.4;
    });
    rig.wav("c.wav", 1, 10_000, |_, _| 0.1);
    rig.add_clip(20, "c.wav", 0, 5_000);
    for _ in 0..40 {
        violations += realtime(|| rig.run_quiet(at, BLOCK, true));
        at += BLOCK as u64;
        pause();
    }
    // Stop, seek back (reset), start again.
    for _ in 0..8 {
        violations += realtime(|| rig.run_quiet(at, BLOCK, false));
        pause();
    }
    violations += realtime(|| rig.node.reset());
    at = 5_000;
    for _ in 0..20 {
        violations += realtime(|| rig.run_quiet(at, BLOCK, false));
        pause();
    }
    for _ in 0..60 {
        violations += realtime(|| rig.run_quiet(at, BLOCK, true));
        at += BLOCK as u64;
        pause();
    }
    // Loop over a stretch with both clips' audio, several times round, so
    // streams opened for the loop's start are taken in and promoted, and the
    // ones a lap leaves behind are handed back.
    let (start, end) = (12_000u64, 4 * BEAT);
    rig.looping = Some((start, end));
    rig.node.reset();
    at = start;
    for _ in 0..4 * ((end - start) as usize / BLOCK + 1) {
        let n = BLOCK.min((end - at) as usize);
        violations += realtime(|| rig.run_quiet(at, n, true));
        at += n as u64;
        if at == end {
            at = start;
        }
        pause();
    }
    assert_eq!(violations, 0, "the audio thread allocated or freed memory");
}

#[test]
fn playing_midi_clips_and_taking_in_edits_never_touches_the_allocator() {
    let mut rig = Rig::new("realtime-midi");
    let notes: Vec<(i64, i64, u8)> = (0..32).map(|i| (i * 240, 700, 40 + i as u8)).collect();
    let clip = rig.add_midi_clip(0, 8, &notes);
    rig.settle();
    let mut violations = 0;
    let mut at = 0u64;
    let pause = || std::thread::sleep(Duration::from_millis(2));
    for _ in 0..40 {
        violations += realtime(|| rig.run_quiet(at, BLOCK, true));
        at += BLOCK as u64;
        pause();
    }
    // An edit while notes sound, then a stop and a jump.
    rig.edit_clip(clip, |clip| {
        let noodle_core::ClipContent::Midi(midi) = &mut clip.content else {
            unreachable!()
        };
        midi.notes.truncate(8);
    });
    for _ in 0..40 {
        violations += realtime(|| rig.run_quiet(at, BLOCK, true));
        at += BLOCK as u64;
        pause();
    }
    for _ in 0..4 {
        violations += realtime(|| rig.run_quiet(at, BLOCK, false));
        pause();
    }
    violations += realtime(|| rig.node.reset());
    at = BEAT / 3;
    for _ in 0..40 {
        violations += realtime(|| rig.run_quiet(at, BLOCK, true));
        at += BLOCK as u64;
        pause();
    }
    assert_eq!(violations, 0, "the audio thread allocated or freed memory");
}
