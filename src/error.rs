//! Error types.

use core::fmt;
use num_enum::{IntoPrimitive, TryFromPrimitive};

/// Result alias for C2 operations.
pub type Result<T> = core::result::Result<T, C2Error>;

// ── C2 status byte (returned by the target after every transaction) ────────

/// C2 status codes returned by the target MCU (AN127 §4).
///
/// `num_enum` gives us `TryFrom<u8>` for free — no hand-written `from_byte`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum C2Status {
    /// Command invalid / not recognised
    Invalid       = 0x00,
    /// No connection to target
    NoConnect     = 0x01,
    /// Command failed
    Failed        = 0x02,
    /// Timed out
    Timeout       = 0x04,
    /// Bad data
    BadData       = 0x05,
    /// Success
    Ok            = 0x0D,
}

impl C2Status {
    /// Is this a success status?
    pub fn is_ok(self) -> bool {
        self == C2Status::Ok
    }
}

// ── Library-level error type ───────────────────────────────────────────────

/// Unified error type for every fallible operation in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt-impl", derive(defmt::Format))]
pub enum C2Error {
    /// Target did not respond within the expected time.
    Timeout,
    /// Operation requires a connection but we are not connected.
    NotConnected,
    /// The target reported an invalid-command status.
    InvalidCommand,
    /// The target reported a generic failure.
    Failed,
    /// Bad data received from target.
    BadData,
    /// Flash programming failed.
    FlashFailed,
    /// Flash erase failed.
    EraseFailed,
    /// Read-back verification failed.
    VerifyFailed,
    /// Device returned an unexpected device ID.
    InvalidDeviceId(u8),
}

impl fmt::Display for C2Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout           => write!(f, "timeout"),
            Self::NotConnected      => write!(f, "not connected"),
            Self::InvalidCommand    => write!(f, "invalid command"),
            Self::Failed            => write!(f, "command failed"),
            Self::BadData           => write!(f, "bad data"),
            Self::FlashFailed       => write!(f, "flash failed"),
            Self::EraseFailed       => write!(f, "erase failed"),
            Self::VerifyFailed      => write!(f, "verify failed"),
            Self::InvalidDeviceId(id) => write!(f, "invalid device ID 0x{id:02X}"),
        }
    }
}

impl From<C2Status> for C2Error {
    fn from(s: C2Status) -> Self {
        match s {
            C2Status::Ok        => panic!("C2Status::Ok converted to error — this is a bug"),
            C2Status::Invalid   => Self::InvalidCommand,
            C2Status::NoConnect => Self::NotConnected,
            C2Status::Failed    => Self::Failed,
            C2Status::Timeout   => Self::Timeout,
            C2Status::BadData   => Self::BadData,
        }
    }
}
