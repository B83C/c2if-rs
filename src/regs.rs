//! C2 register addresses, FP commands, and bitfield flags.
//!
//! Everything in this module is pure data — no I/O — so it compiles away on
//! targets that don't use a given constant.

use bitflags::bitflags;
use num_enum::{IntoPrimitive, TryFromPrimitive};

// ── C2 address register addresses ──────────────────────────────────────────

/// Addresses accessible through the C2 address register (AN127 §3.1).
///
/// `TryFrom<u8>` is derived automatically by `num_enum`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum AddrReg {
    /// Device ID register
    DeviceId  = 0x00,
    /// Revision ID register
    Revision  = 0x01,
    /// Flash Programming Control register
    Fpctl     = 0x02,
    /// Flash Programming Data register (default address)
    Fpdat     = 0xAD,
    /// Flash Programming Data register (C8051F32x / SI100x variant)
    FpdatAlt  = 0xB4,
}

// ── Flash-programming commands sent through FPDAT ──────────────────────────

/// Flash-programming commands written to the FPDAT register (AN127 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum FpCmd {
    /// Erase entire device (mass erase)
    EraseDevice = 0x03,
    /// Erase sector
    EraseSector = 0x08,
    /// Read flash
    ReadFlash   = 0x06,
    /// Write flash
    WriteFlash  = 0x07,
    /// Read XRAM
    ReadXram    = 0x0E,
    /// Write XRAM
    WriteXram   = 0x0F,
}

// ── FPCTL key codes ────────────────────────────────────────────────────────

/// Key codes written to FPCTL to control flash programming mode.
///
/// The EFM8BB51 Reference Manual §26.4.4 documents a 2-byte sequence
/// (`0x02`, `0x01`), but the proven-working reference implementation
/// (bird-sanctuary/arduino-c2-interface, `C2::init()`) writes 3 bytes —
/// `0x02, 0x04, 0x01` — and that's what real hardware responds to. Going
/// with the verified-on-hardware sequence over the manual's.
pub mod fpctl {
    /// Enable flash programming (first key)
    pub const ENABLE_KEY_1: u8 = 0x02;
    /// Enable flash programming (second key)
    pub const ENABLE_KEY_2: u8 = 0x04;
    /// Release from reset / confirm (third key)
    pub const RELEASE:      u8 = 0x01;
    /// Start user code (go)
    pub const GO:           u8 = 0x08;
    /// Disconnect / power-down
    pub const DISCONNECT:   u8 = 0x00;
}

// ── Flash-programming confirmation codes ───────────────────────────────────

/// Magic bytes sent after mass-erase commands.
pub mod confirm {
    /// Confirmation byte 1
    pub const BYTE_1: u8 = 0xDE;
    /// Confirmation byte 2
    pub const BYTE_2: u8 = 0xAD;
    /// Confirmation byte 3
    pub const BYTE_3: u8 = 0xA5;
}

// ── Address-register status flags ──────────────────────────────────────────

bitflags! {
    /// Flags read from the C2 address register.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Flags: u8 {
        /// Output data ready (target → host)
        const OUT_READY = 0x01;
        /// Input buffer busy (host → target, still processing)
        const IN_BUSY   = 0x02;
    }
}

// ── Known device IDs ───────────────────────────────────────────────────────

/// Known EFM8 BusyBee device IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum DeviceId {
    /// EFM8BB10 / BB1
    Bb1  = 0x05,
    /// EFM8BB21 / BB21
    Bb21 = 0x15,
    /// EFM8BB51 / BB51
    Bb51 = 0x35,
}

// ── Flash layout constants for EFM8BB51 ────────────────────────────────────

/// Flash geometry for the EFM8BB51 (BLHeli_S target).
#[allow(dead_code)]  // public API constants — users reference these
pub mod layout {
    /// Addressable flash size in bytes reachable via the native C2
    /// interface: 0x0000-0x3FFF (16 KiB), confirmed against
    /// bird-sanctuary/arduino-c2-interface's Python reference client
    /// (`efm8.py`), which only ever reads/writes this range and notes
    /// explicitly: "Reading the bootloader on BB51 does not seem to be
    /// possible since we are not getting a response from this address
    /// space" — the 0xF000+ bootloader block esc-configurator's Silabs.js
    /// table implies (flash_size: 63485) is NOT reachable over C2 at all.
    pub const FLASH_SIZE: u16 = 0x4000;
    /// Flash page size in bytes (EFM8BB51 Reference Manual §1: all flash
    /// memory maps show 2048-byte pages, not 512).
    pub const PAGE_SIZE:  u16 = 2048;
    /// Number of flash pages.
    pub const PAGE_COUNT: u16 = FLASH_SIZE / PAGE_SIZE;

    /// BLHeli_S signature byte address.
    ///
    /// NOTE: this is the BB1/BB21 convention. A real BB51 firmware dump
    /// (16 KiB variant) shows no data at all near 0x1FFE/0x1FFF — its
    /// signature strings ("#BLHELI$EFM8B51#", "Bluejay", etc.) instead
    /// live around 0x3040-0x306F. This constant needs the real BB51
    /// address before it's trustworthy; don't rely on it yet.
    pub const SIGNATURE_ADDR: u16 = 0x1FFF;
    /// BLHeli_S version byte address. See `SIGNATURE_ADDR` note.
    pub const VERSION_ADDR:   u16 = 0x1FFE;
}
