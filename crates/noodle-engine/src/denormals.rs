//! Flushing subnormal floats to zero while the audio thread renders.
//!
//! A filter or envelope whose input goes silent decays towards zero, and its
//! state can end up stuck among the subnormals, the tiny floats below
//! `f32::MIN_POSITIVE`. Most CPUs handle those far more slowly than normal
//! floats, so a silent patch could cost many times the CPU of a sounding one.
//! With flush-to-zero set, the CPU treats them as zero instead.

/// The mode bits that flush subnormals on this CPU. Found once, off the audio
/// thread, by [`Flush::detect`].
#[derive(Clone, Copy)]
pub(crate) struct Flush(arch::Mode);

impl Flush {
    pub(crate) fn detect() -> Self {
        Self(arch::flush_bits())
    }

    /// Sets flush-to-zero (and denormals-are-zero, on x86, where the CPU has
    /// it) until the guard is dropped, then restores the previous mode.
    /// Real-time safe: it's only a register write. Does nothing on other
    /// architectures.
    pub(crate) fn enable(self) -> FlushDenormals {
        let saved = arch::get();
        arch::set(arch::with(saved, self.0));
        FlushDenormals { saved }
    }
}

pub(crate) struct FlushDenormals {
    saved: arch::Mode,
}

impl Drop for FlushDenormals {
    fn drop(&mut self) {
        arch::set(self.saved);
    }
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use std::arch::asm;
    use std::arch::x86_64::_fxsave;

    pub(super) type Mode = u32;

    const FTZ: Mode = 1 << 15;
    const DAZ: Mode = 1 << 6;

    /// FTZ, plus DAZ if this CPU supports it. Setting an unsupported MXCSR
    /// bit faults, and some early x86_64 CPUs and virtual CPUs lack DAZ, so
    /// this asks the CPU: `fxsave` reports the writable bits as MXCSR_MASK.
    pub(super) fn flush_bits() -> Mode {
        #[repr(align(16))]
        struct Area([u8; 512]);
        let mut area = Area([0; 512]);
        // SAFETY: fxsave writes 512 bytes to a 16-byte-aligned buffer, which
        // `area` is. FXSR is part of the x86_64 baseline.
        unsafe { _fxsave(area.0.as_mut_ptr()) };
        let mask = Mode::from_le_bytes(area.0[28..32].try_into().unwrap());
        // A zero mask means the CPU predates the field: no DAZ.
        let mask = if mask == 0 { 0xffbf } else { mask };
        FTZ | (DAZ & mask)
    }

    pub(super) fn with(csr: Mode, bits: Mode) -> Mode {
        csr | bits
    }

    pub(super) fn get() -> Mode {
        let mut csr: Mode = 0;
        // SAFETY: stores MXCSR into a local.
        unsafe { asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack, preserves_flags)) };
        csr
    }

    pub(super) fn set(csr: Mode) {
        // SAFETY: loads MXCSR from a local. Only bits that `flush_bits` found
        // writable ever differ from the mode Rust code was already running
        // with.
        unsafe { asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, readonly, preserves_flags)) };
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use std::arch::asm;

    pub(super) type Mode = u64;

    /// FPCR's flush-to-zero bit (24), which flushes inputs and results for
    /// f32 and f64. Half precision has its own bit (FZ16, 19), which would
    /// need setting too if f16 processing ever arrives.
    pub(super) fn flush_bits() -> Mode {
        1 << 24
    }

    pub(super) fn with(fpcr: Mode, bits: Mode) -> Mode {
        fpcr | bits
    }

    // No `nomem` on these: it would let the compiler move the block's loads
    // and stores across the mode switch.

    pub(super) fn get() -> Mode {
        let fpcr: Mode;
        // SAFETY: reads FPCR.
        unsafe { asm!("mrs {}, fpcr", out(reg) fpcr, options(nostack, preserves_flags)) };
        fpcr
    }

    pub(super) fn set(fpcr: Mode) {
        // SAFETY: writes FPCR. Only the flush bit ever differs from the mode
        // Rust code was already running with.
        unsafe { asm!("msr fpcr, {}", in(reg) fpcr, options(nostack, preserves_flags)) };
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod arch {
    pub(super) type Mode = ();

    pub(super) fn flush_bits() -> Mode {}

    pub(super) fn with(_: Mode, _: Mode) -> Mode {}

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
            let _flush = Flush::detect().enable();
            assert_eq!(subnormal(), 0.0);
        }
        assert!(subnormal().is_subnormal());
    }
}
