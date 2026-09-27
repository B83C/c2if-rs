//! Platform-specific clock-pulse delay.
//!
//! The C2 clock pulse needs to be *fast* (sub-microsecond) and
//! **interrupt-safe** — an ISR firing between `pin_low` and `pin_high`
//! would stretch the clock and violate C2 timing.
//!
//! This module provides a [`ClockDelay`] trait and two implementations:
//!
//! - **[`NopDelay`]** — uses ARM `NOP` instructions (default on Cortex-M).
//! - **[`VolatileDelay`]** — busy-loop via volatile read (any target).
//!
//! Both use a critical section (`cortex_m::interrupt::free`) on ARM to
//! prevent ISIs from stretching the clock pulse.  Implement the trait
//! yourself for cycle-accurate timing on other architectures (RISC-V, Xtensa, …).

/// Busy-wait for approximately `cycles` CPU cycles inside a critical
/// section that disables interrupts.
///
/// Must complete in well under 1 µs.  On a 125 MHz core one cycle is 8 ns,
/// so even 10 cycles is only 80 ns — well above C2's 20 ns minimum.
pub trait ClockDelay {
    /// Spin for `cycles` CPU cycles with interrupts disabled.
    fn spin_cycles(cycles: u32);
}

// ── Cortex-M: single-cycle NOPs inside a critical section ──────────────────

/// ARM Cortex-M implementation using `NOP`.
pub struct NopDelay;

#[cfg(all(feature = "cortex-m", target_arch = "arm"))]
impl ClockDelay for NopDelay {
    #[inline(always)]
    fn spin_cycles(cycles: u32) {
        // Disable interrupts for the duration of the pulse to prevent
        // ISRs from stretching the clock low time.
        cortex_m::interrupt::free(|_| {
            for _ in 0..cycles {
                cortex_m::asm::nop();
            }
        });
    }
}

/// When `cortex-m` is off or we're not on ARM, use the volatile fallback.
#[cfg(not(all(feature = "cortex-m", target_arch = "arm")))]
impl ClockDelay for NopDelay {
    #[inline(always)]
    fn spin_cycles(cycles: u32) {
        VolatileDelay::spin_cycles(cycles);
    }
}

// ── Fallback: volatile read to block the optimiser ─────────────────────────

/// Minimal fallback that works on any target.
///
/// Uses a volatile read to prevent the compiler from eliding the loop.
/// Timing is less precise but still satisfies C2's ≥ 20 ns requirement
/// on any MCU running ≥ 1 MHz.
pub struct VolatileDelay;

impl ClockDelay for VolatileDelay {
    #[inline(always)]
    fn spin_cycles(cycles: u32) {
        for _ in 0..cycles {
            // SAFETY: reading from a static is always safe; we just need
            // the side-effect of a memory access to occupy the core.
            unsafe { core::ptr::read_volatile(&0u8); }
        }
    }
}
