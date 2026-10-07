//! The audio-thread side of a clip stream never allocates or frees memory,
//! whether it is reading, running dry, or seeking.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Duration, Instant};

use noodle_io::{StreamSpec, open_stream, write_wav};

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

/// Runs `f` as if on the audio thread, returning how many times it allocated
/// or freed memory.
fn realtime(f: impl FnOnce()) -> usize {
    REALTIME.set(true);
    f();
    REALTIME.set(false);
    VIOLATIONS.replace(0)
}

#[test]
fn reading_seeking_and_running_dry_never_touch_the_allocator() {
    let dir = std::env::temp_dir().join("noodle-io-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stream-realtime.wav");
    let samples: Vec<f32> = (0..2 * 100_000).map(|i| (i as f32 * 0.001).sin()).collect();
    write_wav(&path, &samples, 2, 44_100).unwrap();
    // Resampled, so the worker does the most it can.
    let (mut stream, worker) = open_stream(StreamSpec {
        path,
        rate: 48_000,
        offset: 0,
        length: 100_000,
        chunks: 4,
    })
    .unwrap();

    let mut block = vec![0.0f32; 2 * 256];
    let mut violations = 0;
    let started = Instant::now();
    // Play through the clip with seeks in between, long enough for the
    // worker to fall behind after each one.
    for round in 0..40 {
        violations += realtime(|| {
            if round % 5 == 0 {
                stream.seek(round * 1_000);
            }
            for _ in 0..20 {
                stream.read(&mut block);
            }
        });
        std::thread::sleep(Duration::from_millis(3));
        assert!(started.elapsed() < Duration::from_secs(20));
    }
    assert!(stream.underruns() > 0, "the test never ran dry");

    // And with the worker gone, so every read is an underrun.
    drop(worker);
    violations += realtime(|| {
        stream.seek(0);
        for _ in 0..50 {
            stream.read(&mut block);
        }
    });
    assert_eq!(violations, 0, "the audio thread allocated or freed memory");
}
