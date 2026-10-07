//! Flushing subnormal floats to zero while the audio thread renders.
//!
//! A filter or envelope whose input goes silent decays towards zero, and its
//! state can end up stuck among the subnormals, the tiny floats below
//! `f32::MIN_POSITIVE`. Most CPUs handle those far more slowly than normal
//! floats, so a silent patch could cost many times the CPU of a sounding one.
//! With flush-to-zero set, the CPU treats them as zero instead.

/// Sets flush-to-zero (and denormals-are-zero, on x86) until dropped, then
/// restores the previous mode. Real-time safe: it's only a register write.
/// Does nothing on other architectures.
pub(crate) struct FlushDenormals {
    saved: arch::Mode,
}

impl FlushDenormals {
    pub(crate) fn new() -> Self {
        let saved = arch::get();
        arch::set(arch::flushing(saved));
        Self { saved }
    }
}

impl Drop for FlushDenormals {
    fn drop(&mut self) {
        arch::set(self.saved);
    }
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use std::arch::asm;

    pub(super) type Mode = u32;

    /// `csr` with flush-to-zero (bit 15) and denormals-are-zero (bit 6) set.
    pub(super) fn flushing(csr: Mode) -> Mode {
        csr | 1 << 15 | 1 << 6
    }

    pub(super) fn get() -> Mode {
        let mut csr: Mode = 0;
        // SAFETY: stores MXCSR into a local.
        unsafe { asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack, preserves_flags)) };
        csr
    }

    pub(super) fn set(csr: Mode) {
        // SAFETY: loads MXCSR from a local. Only the flush bits ever differ
        // from the mode Rust code was already running with.
        unsafe { asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, readonly, preserves_flags)) };
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use std::arch::asm;

    pub(super) type Mode = u64;

    /// `fpcr` with flush-to-zero set (bit 24), which flushes inputs and
    /// results.
    pub(super) fn flushing(fpcr: Mode) -> Mode {
        fpcr | 1 << 24
    }

    pub(super) fn get() -> Mode {
        let fpcr: Mode;
        // SAFETY: reads FPCR.
        unsafe { asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags)) };
        fpcr
    }

    pub(super) fn set(fpcr: Mode) {
        // SAFETY: writes FPCR. Only the flush bit ever differs from the mode
        // Rust code was already running with.
        unsafe { asm!("msr fpcr, {}", in(reg) fpcr, options(nomem, nostack, preserves_flags)) };
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod arch {
    pub(super) type Mode = ();

    pub(super) fn flushing(_: Mode) -> Mode {}

    pub(super) fn get() -> Mode {}

    pub(super) fn set(_: Mode) {}
}

#[cfg(test)]
mod tests {
    use std::hint::black_box;

    use super::*;

    /// A product that's subnormal, computed at run time.
    fn subnormal() -> f32 {
        black_box(f32::MIN_POSITIVE) * black_box(0.25)
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn flushes_while_held_and_restores_after() {
        assert!(subnormal().is_subnormal());
        {
            let _flush = FlushDenormals::new();
            assert_eq!(subnormal(), 0.0);
        }
        assert!(subnormal().is_subnormal());
    }
}
