//! Mock GPIO and delay for testing without hardware.
//!
//! `MockPin` records every `set_low()`/`set_high()` call and can be
//! pre-loaded with return values for `is_high()`.  `MockDelay` counts
//! elapsed nanoseconds.
//!
//! This module requires `std` (for `Vec`).  Available when the `mock`
//! feature is enabled (it's on by default in dev-dependencies).

extern crate alloc;

use alloc::vec::Vec;
use core::cell::RefCell;
use embedded_hal::digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin};
use embedded_hal_async::delay::DelayNs;

// ── PinEvent ───────────────────────────────────────────────────────────────

/// Recorded GPIO event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinEvent {
    /// Pin driven low.
    SetLow,
    /// Pin driven high.
    SetHigh,
}

// ── MockPin ────────────────────────────────────────────────────────────────

/// Mock GPIO pin that records events and can return preset values.
///
/// Uses `RefCell` internally so the `&mut self` methods on `InputPin`
/// (embedded-hal 1.0) can pop from the queue while still being
/// inspectable from test code via `level()` / `take_events()`.
pub struct MockPin {
    state: RefCell<MockPinState>,
}

struct MockPinState {
    level: bool,
    is_high_queue: Vec<bool>,
    events: Vec<PinEvent>,
}

impl MockPin {
    /// Create a new mock pin starting LOW.
    pub fn new() -> Self {
        Self::with_level(false)
    }

    /// Create a mock pin starting at the given level.
    pub fn with_level(level: bool) -> Self {
        Self {
            state: RefCell::new(MockPinState {
                level,
                is_high_queue: Vec::new(),
                events: Vec::new(),
            }),
        }
    }

    /// Push a value that the next `is_high()` call will return.
    ///
    /// Values are consumed FIFO.  When the queue is empty, `is_high()`
    /// returns the last driven level instead.
    pub fn push_is_high(&self, val: bool) {
        self.state.borrow_mut().is_high_queue.push(val);
    }

    /// Push many values for successive `is_high()` calls.
    pub fn push_is_high_iter(&self, iter: impl Iterator<Item = bool>) {
        self.state.borrow_mut().is_high_queue.extend(iter);
    }

    /// Return the recorded event log and clear it.
    pub fn take_events(&self) -> Vec<PinEvent> {
        core::mem::take(&mut self.state.borrow_mut().events)
    }

    /// Peek at the current output level.
    pub fn level(&self) -> bool {
        self.state.borrow().level
    }

    /// Number of queued `is_high` values remaining.
    pub fn queue_len(&self) -> usize {
        self.state.borrow().is_high_queue.len()
    }
}

impl Default for MockPin {
    fn default() -> Self {
        Self::new()
    }
}

impl ErrorType for MockPin {
    type Error = core::convert::Infallible;
}

impl OutputPin for MockPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        let mut s = self.state.borrow_mut();
        s.level = false;
        s.events.push(PinEvent::SetLow);
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        let mut s = self.state.borrow_mut();
        s.level = true;
        s.events.push(PinEvent::SetHigh);
        Ok(())
    }
}

impl InputPin for MockPin {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        let mut s = self.state.borrow_mut();
        if s.is_high_queue.is_empty() {
            Ok(s.level)
        } else {
            Ok(s.is_high_queue.remove(0))
        }
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        Ok(!self.is_high()?)
    }
}

impl StatefulOutputPin for MockPin {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.state.borrow().level)
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        Ok(!self.state.borrow().level)
    }
}

// ── MockDelay ──────────────────────────────────────────────────────────────

/// Mock delay provider that counts elapsed nanoseconds.
pub struct MockDelay {
    elapsed_ns: RefCell<u64>,
}

impl MockDelay {
    /// Create a new mock delay with zero elapsed time.
    pub fn new() -> Self {
        Self {
            elapsed_ns: RefCell::new(0),
        }
    }

    /// Total nanoseconds elapsed across all `delay_*` calls.
    pub fn elapsed_ns(&self) -> u64 {
        *self.elapsed_ns.borrow()
    }

    /// Reset the elapsed counter to zero.
    pub fn reset(&self) {
        *self.elapsed_ns.borrow_mut() = 0;
    }
}

impl Default for MockDelay {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for MockDelay {
    fn clone(&self) -> Self {
        Self {
            elapsed_ns: RefCell::new(*self.elapsed_ns.borrow()),
        }
    }
}

impl DelayNs for MockDelay {
    async fn delay_ns(&mut self, ns: u32) {
        *self.elapsed_ns.borrow_mut() += ns as u64;
    }

    async fn delay_us(&mut self, us: u32) {
        *self.elapsed_ns.borrow_mut() += us as u64 * 1_000;
    }

    async fn delay_ms(&mut self, ms: u32) {
        *self.elapsed_ns.borrow_mut() += ms as u64 * 1_000_000;
    }
}
