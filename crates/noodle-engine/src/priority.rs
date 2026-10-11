//! Raising a thread to real-time priority.
//!
//! The audio device's callback thread is already real-time on most systems,
//! but the worker threads that help it run a plan (see `parallel.rs`) are
//! ordinary threads, and a block waits for the slowest of them: a worker that
//! the scheduler leaves off its core for a millisecond makes the whole block
//! late. [`raise_current_thread`] asks the system to treat the calling thread
//! like an audio thread:
//!
//! - **macOS:** a time-constraint policy (`THREAD_TIME_CONSTRAINT_POLICY`),
//!   which is what Core Audio gives its own I/O threads, sized from the
//!   block's period. (Joining the device's audio workgroup would also let
//!   the scheduler place helpers with the I/O thread; that needs the
//!   workgroup handle from Core Audio, which the output stream doesn't give
//!   us yet.)
//! - **Windows:** MMCSS, the "Pro Audio" task, falling back to the highest
//!   ordinary thread priority.
//! - **Linux and other Unix:** `SCHED_FIFO` at a low priority, which
//!   needs `RLIMIT_RTPRIO` or `CAP_SYS_NICE` (rtkit isn't used), falling back
//!   to nothing: the thread stays ordinary.
//!
//! It is best effort. A refusal is not an error: [`Priority`] says what the
//! thread got, and the caller carries on.

use std::time::Duration;

/// What [`raise_current_thread`] managed to get.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    /// The system's real-time class for audio.
    Realtime,
    /// Better than ordinary, but not real-time.
    Raised,
    /// The system refused, or this platform has nothing to ask for.
    Normal,
}

impl Priority {
    pub fn name(self) -> &'static str {
        match self {
            Self::Realtime => "real-time",
            Self::Raised => "raised",
            Self::Normal => "normal",
        }
    }
}

/// Asks the system to schedule the calling thread as an audio thread.
/// `period` is how often it has work, i.e. a block's duration.
pub fn raise_current_thread(period: Duration) -> Priority {
    imp::raise(period)
}

// libc marks its mach bindings deprecated in favour of a crate we would add
// for four calls.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod imp {
    use super::Priority;
    use std::time::Duration;

    pub fn raise(period: Duration) -> Priority {
        let mut timebase = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: plain mach calls on the calling thread with valid pointers.
        unsafe {
            if libc::mach_timebase_info(&mut timebase) != 0 || timebase.numer == 0 {
                return Priority::Normal;
            }
            let to_ticks = |d: Duration| -> u32 {
                let nanos = d.as_nanos() as u64;
                (nanos * u64::from(timebase.denom) / u64::from(timebase.numer))
                    .min(u64::from(u32::MAX)) as u32
            };
            let period_ticks = to_ticks(period.max(Duration::from_micros(100)));
            // A worker is busy for a good part of the block at most; the
            // scheduler may preempt it if it uses more than `computation`
            // inside `constraint`.
            let policy = libc::thread_time_constraint_policy {
                period: period_ticks,
                computation: period_ticks / 2,
                constraint: period_ticks,
                preemptible: 1,
            };
            let result = libc::thread_policy_set(
                libc::mach_thread_self(),
                libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
                &policy as *const _ as libc::thread_policy_t,
                libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            );
            if result == 0 {
                Priority::Realtime
            } else {
                Priority::Normal
            }
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::Priority;
    use std::time::Duration;

    pub fn raise(_period: Duration) -> Priority {
        // Below the audio callback's usual 80 to 90 or so, so a worker never
        // starves the thread it works for.
        let param = libc::sched_param { sched_priority: 20 };
        // SAFETY: sets the scheduling policy of the calling thread.
        let result =
            unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) };
        if result == 0 {
            Priority::Realtime
        } else {
            Priority::Normal
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::Priority;
    use std::time::Duration;

    #[link(name = "avrt")]
    unsafe extern "system" {
        fn AvSetMmThreadCharacteristicsW(
            task: *const u16,
            index: *mut u32,
        ) -> *mut core::ffi::c_void;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThread() -> *mut core::ffi::c_void;
        fn SetThreadPriority(thread: *mut core::ffi::c_void, priority: i32) -> i32;
    }
    const THREAD_PRIORITY_HIGHEST: i32 = 2;

    pub fn raise(_period: Duration) -> Priority {
        let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut index = 0u32;
        // SAFETY: `task` is NUL-terminated and outlives the call.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut index) };
        if !handle.is_null() {
            return Priority::Realtime;
        }
        // SAFETY: the pseudo-handle of the calling thread.
        if unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) } != 0 {
            Priority::Raised
        } else {
            Priority::Normal
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::Priority;
    use std::time::Duration;

    pub fn raise(_period: Duration) -> Priority {
        Priority::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_never_fails_and_says_what_it_got() {
        // A sandbox usually refuses, a desktop usually agrees; either is fine.
        let got = std::thread::spawn(|| raise_current_thread(Duration::from_millis(5)))
            .join()
            .unwrap();
        assert!(matches!(
            got,
            Priority::Realtime | Priority::Raised | Priority::Normal
        ));
        assert!(!got.name().is_empty());
    }
}
