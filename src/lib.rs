//! # C2IF — Silicon Labs C2 Flash Programmer
//!
//! Portable `no_std` library for flashing EFM8-based BLHeli_S ESCs
//! (BusyBee family: BB1, BB21, BB51) via the Silicon Labs C2 debug interface.
//!
//! Works on any MCU that can bit-bang two GPIO pins and provide an async
//! delay.  The clock-pulse timing uses platform-specific NOPs (Cortex-M by
//! default) via the [`platform::ClockDelay`] trait — implement it for your
//! target if you're not on ARM.
//!
//! ## Wiring
//!
//! ```text
//!  Programmer          ESC
//!  ────────────        ──────
//!  C2CK (out)  ────▶  C2CK pad
//!  C2D  (bidir) ◀──▶  C2D  pad
//!  GND          ────▶  GND  pad
//! ```
//!
//! The ESC must be powered separately (LiPo or bench supply).
//!
//! ## Quick start
//!
//! ```rust,ignore
//! use embassy_executor::Spawner;
//! use c2if::{C2Interface, C2Error};
//!
//! #[embassy_executor::main]
//! async fn main(_spawner: Spawner) {
//!     let p = embassy_rp::init(Default::default());
//!     let mut c2 = C2Interface::new(p.PIN_2, p.PIN_3, embassy_time::Delay);
//!     c2.connect().await.unwrap();
//!     let (dev, rev) = c2.read_device_id().unwrap();
//!     defmt::info!("Device 0x{:02X} rev 0x{:02X}", dev, rev);
//!     c2.erase_flash().await.unwrap();
//!     c2.go().await.unwrap();
//! }
//! ```

#![no_std]
#![warn(missing_docs)]

mod error;
mod regs;
mod c2;
mod flash;
pub mod platform;

/// C2 USB bridge, compatible with `arduino-c2-flasher` / `prober`'s C2
/// mode.  Requires `embedded-io-async`.
#[cfg(feature = "bridge")]
pub mod bridge;

/// Mock GPIO/delay for testing.  Requires `std` (uses `Vec`).
#[cfg(feature = "mock")]
pub mod mock;

pub use error::{C2Error, C2Status, Result};
pub use regs::{AddrReg, FpCmd, Flags};
/// Flash layout constants (page size, addresses, …).
pub use regs::layout;
pub use c2::C2Interface;
pub use flash::FlashProgrammer;

/// Re-export of `embassy_time` for convenience.
#[cfg(feature = "embassy")]
pub use embassy_time;
