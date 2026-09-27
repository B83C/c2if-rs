//! High-level firmware flashing workflow.
//!
//! [`FlashProgrammer`] wraps a [`C2Interface`] and provides a convenient
//! API for the full erase → write → verify → go cycle.
//!
//! The actual firmware data comes from the caller (e.g. a `&[u8]` embedded
//! in the binary, or read from an SD card / USB mass-storage device).

use embedded_hal::digital::{InputPin, OutputPin, StatefulOutputPin};
use embedded_hal_async::delay::DelayNs;

use crate::c2::C2Interface;
use crate::error::{C2Error, Result};
use crate::regs::layout;

/// High-level flash programmer.
///
/// Wraps a [`C2Interface`] and adds page-aligned erase, chunked writes
/// and read-back verification on top.
pub struct FlashProgrammer<'a, C2CK, C2D, D> {
    c2: &'a mut C2Interface<C2CK, C2D, D>,
}

impl<'a, C2CK, C2D, D> FlashProgrammer<'a, C2CK, C2D, D>
where
    C2CK: OutputPin + InputPin,
    C2D:  StatefulOutputPin + InputPin,
    D:    DelayNs,
{
    /// Wrap an already-connected [`C2Interface`].
    pub fn new(c2: &'a mut C2Interface<C2CK, C2D, D>) -> Self {
        Self { c2 }
    }

    /// Access the underlying `C2Interface`.
    pub fn inner(&mut self) -> &mut C2Interface<C2CK, C2D, D> {
        self.c2
    }

    // ==================================================================
    // Erase
    // ==================================================================

    /// Mass-erase the entire flash.
    pub async fn erase_flash(&mut self) -> Result<()> {
        self.c2.erase_flash().await
    }

    /// Erase every page that overlaps with the address range `[addr .. addr+len)`.
    ///
    /// Pages are 2048 bytes on BB51.  This rounds down to the containing
    /// page boundary and iterates.
    pub async fn erase_range(&mut self, addr: u16, len: u16) -> Result<()> {
        let first_sector = addr / layout::PAGE_SIZE;
        let last_sector  = (addr + len - 1) / layout::PAGE_SIZE;
        for s in first_sector..=last_sector {
            self.c2.erase_sector(s as u8).await?;
        }
        Ok(())
    }

    // ==================================================================
    // Write
    // ==================================================================

    /// Write a firmware image to flash.
    ///
    /// The image is written starting at `base_addr`.  Sectors overlapping
    /// the range are erased first.  Writes are chunked to stay within
    /// the target's flash page buffer.
    pub async fn write_flash(
        &mut self,
        base_addr: u16,
        image: &[u8],
    ) -> Result<()> {
        if image.is_empty() {
            return Ok(());
        }

        self.erase_range(base_addr, image.len() as u16).await?;

        // The EFM8 FP write command accepts at most 128 bytes per transaction.
        const CHUNK: usize = 128;
        let mut offset = 0usize;
        while offset < image.len() {
            let end = (offset + CHUNK).min(image.len());
            let addr = base_addr + offset as u16;
            self.c2.write_flash(addr, &image[offset..end]).await?;
            offset = end;
        }
        Ok(())
    }

    // ==================================================================
    // Read / verify
    // ==================================================================

    /// Read back flash content starting at `addr`.
    pub async fn read_flash(&mut self, addr: u16, buf: &mut [u8]) -> Result<()> {
        self.c2.read_flash(addr, buf).await
    }

    /// Read back flash and compare it byte-for-byte against `expected`.
    ///
    /// Returns `Ok(())` on match, `Err(C2Error::VerifyFailed)` on the
    /// first mismatch (including the address where it occurred, encoded
    /// in the error).
    pub async fn verify(
        &mut self,
        addr: u16,
        expected: &[u8],
    ) -> Result<()> {
        // Read in chunks to avoid allocating a full-size buffer.
        const CHUNK: usize = 128;
        let mut rbuf = [0u8; CHUNK];
        let mut offset = 0usize;
        while offset < expected.len() {
            let end = (offset + CHUNK).min(expected.len());
            let a = addr + offset as u16;
            self.c2.read_flash(a, &mut rbuf[..end - offset]).await?;
            if rbuf[..end - offset] != expected[offset..end] {
                return Err(C2Error::VerifyFailed);
            }
            offset = end;
        }
        Ok(())
    }

    /// Write-then-verify in one call.
    pub async fn write_and_verify(
        &mut self,
        base_addr: u16,
        image: &[u8],
    ) -> Result<()> {
        self.write_flash(base_addr, image).await?;
        self.verify(base_addr, image).await
    }

    // ==================================================================
    // Execute
    // ==================================================================

    /// Release the target to run the newly flashed firmware.
    pub async fn go(&mut self) -> Result<()> {
        self.c2.go().await
    }
}
