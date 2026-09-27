//! Core C2 bit-bang transport.
//!
//! Implements the two-wire C2 protocol from Silicon Labs AN127 §3–4.
//! Higher-level flash operations live in [`crate::flash`].
//!
//! # Portability
//!
//! Generic over:
//! - `C2CK`: any [`OutputPin`]
//! - `C2D`:  any [`StatefulOutputPin`] + [`InputPin`]
//! - `D`:    any [`DelayNs`] (e.g. `embassy_time::Timer`)
//!
//! Clock-pulse timing uses [`platform::ClockDelay`] — by default that's
//! ARM `NOP`s via the `cortex-m` feature, but you can implement the trait
//! for any architecture.

use embedded_hal::digital::{InputPin, OutputPin, StatefulOutputPin};
use embedded_hal_async::delay::DelayNs;

use crate::error::{C2Error, C2Status, Result};
use crate::platform::ClockDelay;
use crate::regs::{AddrReg, FpCmd, Flags, fpctl, confirm};

// ── Protocol framing ───────────────────────────────────────────────────────

/// INS field values (2-bit instruction, LSB first on wire).
mod ins {
    pub const READ_AR:  [bool; 2] = [false, true];
    pub const WRITE_AR: [bool; 2] = [true, true];
    pub const READ_DR:  [bool; 2] = [false, false];
    pub const WRITE_DR: [bool; 2] = [true, false];
}

/// Iterations before a wait/poll gives up.
const WAIT_LIMIT: u16  = 0x400;
const POLL_LIMIT: u32  = 50_000;

// ── Driver ─────────────────────────────────────────────────────────────────

/// C2 interface driver.
///
/// Wraps a clock pin and a bidirectional data pin; speaks the C2 protocol
/// to connect to, query, erase, read, and write EFM8 microcontrollers.
pub struct C2Interface<C2CK, C2D, D> {
    c2ck: C2CK,
    c2d: C2D,
    delay: D,
    fpdat: u8,
    connected: bool,
}

impl<C2CK, C2D, D> C2Interface<C2CK, C2D, D>
where
    C2CK: OutputPin + InputPin,
    C2D:  StatefulOutputPin + InputPin,
    D:    DelayNs,
{
    // ==================================================================
    // Construction
    // ==================================================================

    /// Create a new C2 interface.
    pub fn new(c2ck: C2CK, c2d: C2D, delay: D) -> Self {
        Self {
            c2ck,
            c2d,
            delay,
            fpdat: AddrReg::Fpdat as u8,
            connected: false,
        }
    }

    /// Consume the driver, returning the pins.
    pub fn into_pins(self) -> (C2CK, C2D) {
        (self.c2ck, self.c2d)
    }

    /// Whether we hold a C2 connection to the target.
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    // ==================================================================
    // Low-level bit-bang primitives
    // ==================================================================

    /// Drive a pin low, ignoring the Result (GPIO errors are not possible
    /// for simple push-pull drive on any MCU we target).
    #[inline(always)]
    fn pin_low(pin: &mut impl OutputPin) {
        let _ = pin.set_low();
    }

    /// Drive a pin high.
    #[inline(always)]
    fn pin_high(pin: &mut impl OutputPin) {
        let _ = pin.set_high();
    }

    /// Single C2CK pulse: low → high.
    ///
    /// The pulse is wrapped in a critical section (via `ClockDelay::spin_cycles`)
    /// to prevent ISRs from stretching the clock low time.
    #[inline(always)]
    fn clock_pulse(&mut self) {
        Self::pin_low(&mut self.c2ck);
        // Compiler fence ensures GPIO write is visible before we spin.
        // The actual timing delay happens inside spin_cycles (with
        // interrupts disabled on ARM).
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        <Self as HasClockDelay>::spin(self);
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        Self::pin_high(&mut self.c2ck);
    }

    /// Drive one bit on C2D, then pulse the clock.
    #[inline(always)]
    fn write_bit(&mut self, bit: bool) {
        if bit { Self::pin_high(&mut self.c2d) } else { Self::pin_low(&mut self.c2d) };
        self.clock_pulse();
    }

    /// Pulse the clock, then read one bit from C2D.
    #[inline(always)]
    fn read_bit(&mut self) -> Result<bool> {
        self.clock_pulse();
        self.c2d.is_high().map_err(|_| C2Error::Failed)
    }

    /// Write `n` bits of `data` LSB-first.
    fn write_bits(&mut self, data: u8, n: u8) {
        for i in 0..n {
            self.write_bit(data & (1 << i) != 0);
        }
    }

    /// Read `n` bits LSB-first into a byte.
    fn read_bits(&mut self, n: u8) -> Result<u8> {
        let mut v = 0u8;
        for i in 0..n {
            if self.read_bit()? { v |= 1 << i; }
        }
        Ok(v)
    }

    /// Send the 2-bit INS field.
    #[inline(always)]
    fn send_ins(&mut self, ins: [bool; 2]) {
        self.write_bit(ins[0]);
        self.write_bit(ins[1]);
    }

    /// Wait for C2D to go high (host-ready signal from target).
    fn wait_drdy(&mut self) -> Result<()> {
        for _ in 0..WAIT_LIMIT {
            if self.c2d.is_high().map_err(|_| C2Error::Failed)? { return Ok(()); }
            self.clock_pulse();
        }
        Err(C2Error::Timeout)
    }

    /// Release C2D to input (drive low then float).
    #[inline(always)]
    fn release_c2d(&mut self) {
        Self::pin_low(&mut self.c2d);
    }

    // ==================================================================
    // C2 register operations (AN127 §3)
    // ==================================================================

    /// Read from a C2 address register.
    fn ar_read(&mut self) -> Result<u8> {
        self.clock_pulse();                             // START
        self.send_ins(ins::READ_AR);                    // INS = 10b
        self.release_c2d();                             // switch to input
        let val = self.read_bits(8)?;                   // ADDRESS
        self.clock_pulse();                             // STOP
        Ok(val)
    }

    /// Write to a C2 address register.
    fn ar_write(&mut self, addr: u8) {
        self.clock_pulse();                             // START
        self.send_ins(ins::WRITE_AR);                   // INS = 11b
        self.write_bits(addr, 8);                       // ADDRESS
        self.release_c2d();
        self.clock_pulse();                             // STOP
    }

    /// Read from a C2 data register (1 byte).
    fn dr_read(&mut self) -> Result<u8> {
        self.clock_pulse();                             // START
        self.send_ins(ins::READ_DR);                    // INS = 00b
        self.write_bits(0, 2);                          // LENGTH = 1
        self.release_c2d();
        self.wait_drdy()?;                              // WAIT
        let val = self.read_bits(8)?;                   // DATA
        self.clock_pulse();                             // STOP
        Ok(val)
    }

    /// Write to a C2 data register (1 byte).
    fn dr_write(&mut self, data: u8) -> Result<()> {
        self.clock_pulse();                             // START
        self.send_ins(ins::WRITE_DR);                   // INS = 01b
        self.write_bits(0, 2);                          // LENGTH = 1
        self.write_bits(data, 8);                       // DATA
        self.release_c2d();
        self.wait_drdy()?;                              // WAIT
        self.clock_pulse();                             // STOP
        Ok(())
    }

    // ==================================================================
    // FPDAT-level helpers (AN127 §5)
    // ==================================================================

    async fn fpdat_write(&mut self, data: u8) -> Result<()> {
        self.ar_write(self.fpdat);
        self.dr_write(data)?;
        self.poll_in_busy().await
    }

    async fn fpdat_read(&mut self) -> Result<u8> {
        self.poll_out_ready().await?;
        self.ar_write(self.fpdat);
        self.dr_read()
    }

    async fn fpdat_write_read(&mut self, data: u8) -> Result<u8> {
        self.fpdat_write(data).await?;
        self.fpdat_read().await
    }

    /// Read a status byte from FPDAT and check it's `0x0D` (success), per
    /// AN127's Block Read/Write sequences ("Read FPI Command Status").
    async fn fpdat_check_status(&mut self) -> Result<()> {
        let status = self.fpdat_read().await?;
        match C2Status::try_from(status) {
            Ok(s) if s.is_ok() => Ok(()),
            Ok(s) => Err(C2Error::from(s)),
            Err(_) => Err(C2Error::BadData),
        }
    }

    async fn poll_in_busy(&mut self) -> Result<()> {
        for _ in 0..POLL_LIMIT {
            let ar = self.ar_read()?;
            if !Flags::from_bits_truncate(ar).contains(Flags::IN_BUSY) {
                return Ok(());
            }
        }
        Err(C2Error::Timeout)
    }

    async fn poll_out_ready(&mut self) -> Result<()> {
        for _ in 0..POLL_LIMIT {
            let ar = self.ar_read()?;
            if Flags::from_bits_truncate(ar).contains(Flags::OUT_READY) {
                return Ok(());
            }
        }
        Err(C2Error::Timeout)
    }

    /// C2 reset: C2CK low ≥ 20 µs, high ≥ 2 µs.
    async fn c2_reset(&mut self) {
        Self::pin_low(&mut self.c2ck);
        self.delay.delay_ns(20_000).await;
        Self::pin_high(&mut self.c2ck);
        self.delay.delay_ns(2_000).await;
    }

    // ==================================================================
    // Public API — connection
    // ==================================================================

    /// Connect to the target (AN127 connection sequence).
    pub async fn connect(&mut self) -> Result<()> {
        self.c2_reset().await;

        // Unlock flash programming via FPCTL. Verified-on-hardware
        // sequence (bird-sanctuary/arduino-c2-interface): write
        // 0x02, 0x04, 0x01 in order.
        self.ar_write(AddrReg::Fpctl as u8);
        self.dr_write(fpctl::ENABLE_KEY_1)?;
        self.dr_write(fpctl::ENABLE_KEY_2)?;
        self.dr_write(fpctl::RELEASE)?;
        self.delay.delay_ns(80_000).await;
        self.delay.delay_ns(25_000_000).await;   // 25 ms

        // Verify communication by reading device ID.
        let (dev_id, _rev) = self.read_device_id()?;
        self.select_fpdat(dev_id);
        self.connected = true;
        Ok(())
    }

    /// Disconnect (power-down FPCTL, reset).
    pub async fn disconnect(&mut self) -> Result<()> {
        self.ar_write(AddrReg::Fpctl as u8);
        self.dr_write(fpctl::DISCONNECT)?;
        self.delay.delay_ns(40_000).await;
        self.c2_reset().await;
        self.connected = false;
        Ok(())
    }

    /// Release target to run user code.
    pub async fn go(&mut self) -> Result<()> {
        self.ensure_connected()?;
        self.ar_write(AddrReg::Fpctl as u8);
        self.dr_write(fpctl::GO)?;
        Self::pin_high(&mut self.c2ck);
        self.connected = false;
        Ok(())
    }

    /// Halt a running target back into debug mode.
    pub async fn halt(&mut self) -> Result<()> {
        if self.connected { return Ok(()); }
        if self.c2d.is_high().map_err(|_| C2Error::Failed)? {
            Self::pin_low(&mut self.c2ck);
            self.delay.delay_ns(1_000).await;
            Self::pin_high(&mut self.c2ck);
        }
        for _ in 0..1_000_000u32 {
            if self.c2ck.is_high().map_err(|_| C2Error::Failed)? {
                self.connected = true;
                return Ok(());
            }
        }
        Err(C2Error::Timeout)
    }

    // ==================================================================
    // Public API — device info
    // ==================================================================

    /// Read device ID and silicon revision.
    pub fn read_device_id(&mut self) -> Result<(u8, u8)> {
        self.ar_write(AddrReg::DeviceId as u8);
        let id = self.dr_read()?;
        self.ar_write(AddrReg::Revision as u8);
        let rev = self.dr_read()?;
        Ok((id, rev))
    }

    /// Read the 2-byte unique device ID.
    pub async fn read_unique_id(&mut self) -> Result<(u8, u8)> {
        self.ensure_connected()?;
        let a = self.fpdat_write_read(0x01).await?;
        let b = self.fpdat_write_read(0x02).await?;
        Ok((a, b))
    }

    // ====================================================================
    // Public API — flash / XRAM
    // ====================================================================

    /// Mass-erase entire flash (AN127 Figure 6: Device Erase Sequence).
    /// Status is checked after the command byte and again after the
    /// 3-byte arming sequence (`0xDE 0xAD 0xA5`), which is when the
    /// target actually starts erasing.
    pub async fn erase_flash(&mut self) -> Result<()> {
        self.ensure_connected()?;
        self.fpdat_write(FpCmd::EraseDevice as u8).await?;
        self.fpdat_check_status().await?;
        for &code in &[confirm::BYTE_1, confirm::BYTE_2, confirm::BYTE_3] {
            self.fpdat_write(code).await?;
        }
        self.fpdat_check_status().await
    }

    /// Erase a single 2048-byte flash page (AN127 Figure 5: FLASH Page
    /// Erase Sequence). Status is checked after the command byte, the
    /// page-number byte, and the dummy write that initiates the erase.
    pub async fn erase_sector(&mut self, sector: u8) -> Result<()> {
        self.ensure_connected()?;
        self.fpdat_write(FpCmd::EraseSector as u8).await?;
        self.fpdat_check_status().await?;
        self.fpdat_write(sector).await?;
        self.fpdat_check_status().await?;
        self.fpdat_write(0).await?; // dummy write initiates the erase
        self.fpdat_check_status().await
    }

    /// Read flash into `buf` starting at `addr` (AN127 Figure 4: FLASH
    /// Block Read Sequence). A `0x0D` status byte is read back after both
    /// the command byte and the length byte — the target holds off on
    /// data until each has been positively acknowledged.
    pub async fn read_flash(&mut self, addr: u16, buf: &mut [u8]) -> Result<()> {
        self.ensure_connected()?;
        self.fpdat_write(FpCmd::ReadFlash as u8).await?;
        self.fpdat_check_status().await?;
        self.fpdat_write((addr >> 8) as u8).await?;
        self.fpdat_write(addr as u8).await?;
        self.fpdat_write(buf.len() as u8).await?;
        self.fpdat_check_status().await?;
        for byte in buf.iter_mut() {
            *byte = self.fpdat_read().await?;
        }
        Ok(())
    }

    /// Write `data` to flash starting at `addr` (AN127 Figure 3: FLASH
    /// Block Write Sequence). A `0x0D` status byte is read back after both
    /// the command byte and the length byte, mirroring `read_flash`.
    pub async fn write_flash(&mut self, addr: u16, data: &[u8]) -> Result<()> {
        self.ensure_connected()?;
        self.fpdat_write(FpCmd::WriteFlash as u8).await?;
        self.fpdat_check_status().await?;
        self.fpdat_write((addr >> 8) as u8).await?;
        self.fpdat_write(addr as u8).await?;
        self.fpdat_write(data.len() as u8).await?;
        self.fpdat_check_status().await?;
        for &byte in data {
            self.fpdat_write(byte).await?;
        }
        Ok(())
    }

    /// Read XRAM into `buf` starting at `addr`.
    ///
    /// AN127 doesn't cover the XRAM commands directly (they're a BB51-family
    /// extension), but they follow the same FPI command/length framing as
    /// Block Read (Figure 4), so we apply the same status-check pattern.
    pub async fn read_xram(&mut self, addr: u16, buf: &mut [u8]) -> Result<()> {
        self.ensure_connected()?;
        self.fpdat_write(FpCmd::ReadXram as u8).await?;
        self.fpdat_check_status().await?;
        self.fpdat_write((addr >> 8) as u8).await?;
        self.fpdat_write(addr as u8).await?;
        self.fpdat_write(buf.len() as u8).await?;
        self.fpdat_check_status().await?;
        for byte in buf.iter_mut() {
            *byte = self.fpdat_read().await?;
        }
        Ok(())
    }

    /// Write `data` to XRAM starting at `addr` (see `read_xram` re: status
    /// checks not being directly documented in AN127 for this command).
    pub async fn write_xram(&mut self, addr: u16, data: &[u8]) -> Result<()> {
        self.ensure_connected()?;
        self.fpdat_write(FpCmd::WriteXram as u8).await?;
        self.fpdat_check_status().await?;
        self.fpdat_write((addr >> 8) as u8).await?;
        self.fpdat_write(addr as u8).await?;
        self.fpdat_write(data.len() as u8).await?;
        self.fpdat_check_status().await?;
        for &byte in data {
            self.fpdat_write(byte).await?;
        }
        Ok(())
    }

    // ==================================================================
    // Internal
    // ==================================================================

    fn ensure_connected(&self) -> Result<()> {
        if self.connected { Ok(()) } else { Err(C2Error::NotConnected) }
    }

    fn select_fpdat(&mut self, dev_id: u8) {
        // C8051F32x (0x09), SI100x (0x16), and EFM8BB51 (0x39, confirmed
        // against the EFM8BB51 reference manual §26.4.1: C2ADD lists
        // C2FPDAT at 0xB4, not 0xAD) use 0xB4; everything else 0xAD.
        self.fpdat = match dev_id {
            0x09 | 0x16 | 0x39 => AddrReg::FpdatAlt as u8,
            _                  => AddrReg::Fpdat as u8,
        };
    }
}

// ── ClockDelay helper trait ────────────────────────────────────────────────
//
// We can't add a `CD: ClockDelay` generic to `C2Interface` without polluting
// the public API, so we use a blanket impl that lets any `C2Interface` call
// `spin()` when `ClockDelay` is implemented for the *type* (not the *value*).

/// Extension trait that gives `C2Interface` access to `ClockDelay::spin_cycles`
/// without adding another generic parameter.
trait HasClockDelay {
    fn spin(&self);
}

impl<C2CK, C2D, D> HasClockDelay for C2Interface<C2CK, C2D, D>
where
    C2CK: OutputPin + InputPin,
    C2D:  StatefulOutputPin + InputPin,
    D:    DelayNs,
{
    #[inline(always)]
    fn spin(&self) {
        // 50 cycles gives a comfortable margin over the 20 ns C2 minimum
        // on every target we support. The previous value (2 cycles) only
        // met spec on `NopDelay` at ≤125 MHz; on `VolatileDelay` (used on
        // any non-cortex-m target, e.g. RISC-V/ESP32) each "cycle" is a
        // full volatile load whose duration isn't 1:1 with core clocks,
        // so 2 iterations could produce a pulse too narrow to register
        // on the wire or be reliably sampled by test equipment.
        <crate::platform::NopDelay as ClockDelay>::spin_cycles(50);
    }
}
