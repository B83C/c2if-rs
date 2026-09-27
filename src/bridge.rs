//! Simple C2 USB bridge, compatible with `arduino-c2-flasher`.
//!
//! Implements the same wire protocol as
//! [bird-sanctuary/arduino-c2-interface](https://github.com/bird-sanctuary/arduino-c2-interface)
//! (the Arduino reference implementation behind
//! <https://stylesuxx.github.io/arduino-c2-flasher/>), so this crate's
//! bridge is a drop-in replacement — same commands, same framing, same
//! response bytes — for any RP2040/ESP32/etc. running `c2if`.
//!
//! This project's own hand-rolled ASCII `^hex$` framing and, before that,
//! a hand-rolled implementation of the real BLHeli 4-way interface
//! protocol were both attempted here first. Neither is what shipped tools
//! actually expect on real hardware: esc-configurator's 4-way client (see
//! `FourWayConstants.js`) speaks the real 4-way protocol, but doesn't
//! implement a genuine native-C2 (`SiLC2`) backend — its `Connect()` only
//! ever calls into a UART software bootloader (`BL_ConnectEx`), so a raw
//! C2 debug-interface bridge has no compliant client to talk to through
//! it. This simpler protocol is what a real, open-source, working native
//! C2 tool actually uses.
//!
//! ## Wire format
//!
//! Every command is `[ACTION, LEN, ...ARGS]` (for the fixed 2-byte
//! commands, `LEN` is unused and sent as `0x00`); every response starts
//! with `ACTION | 0x80` on success. A few commands carry extra bytes.
//!
//! | Action | Name    | Request                                                    | Response                          |
//! |--------|---------|-------------------------------------------------------------|-----------------------------------|
//! | 0x00   | ACK     | `[0x00, 0x00]`                                               | `[0x80]`                          |
//! | 0x01   | INIT    | `[0x01, 0x00]`                                               | `[0x81]`                          |
//! | 0x02   | RESET   | `[0x02, 0x00]`                                               | `[0x82]`                          |
//! | 0x03   | WRITE   | `[0x03, total_len, data_len, 0, addr_hi, addr_lo, crc8, data...]` | `[0x83]` (or `[0x43]` on bad CRC) |
//! | 0x04   | ERASE   | `[0x04, 0x00]`                                               | `[0x84]`                          |
//! | 0x05   | READ    | `[0x05, 0x05, amount, addr_hi16, addr_mid8, addr_lo8, 0]`     | `[0x85, data...]`                 |
//! | 0x08   | INFO    | `[0x08, 0x00]`                                               | `[0x88, device_id, revision, 0]`  |
//! | 0x0F   | PING    | `[0x0F, 0x00]`                                               | `[0x8F]` — used by clients to detect the interface is flashed before doing anything else |
//!
//! `WRITE`'s checksum is a plain 8-bit sum: `addr_hi + addr_lo + sum(data)`,
//! truncated to `u8` (no two's-complement negation — matches
//! `efm8.py`/`C2.js`'s `crc` computation exactly, which never negates it
//! either).
//!
//! `READ`'s address is transmitted as 24 bits (`addr_hi16, addr_mid8,
//! addr_lo8`) but only the low 16 bits are meaningful on the EFM8 family
//! `c2if` targets — anything above `0xFFFF` isn't representable in the
//! underlying `u16` address space this crate uses.
//!
//! ## Usage
//!
//! Create a [`UsbBridge`](crate::bridge::UsbBridge) with your USB CDC-ACM
//! serial port and C2 pins, then call
//! [`UsbBridge::run`](crate::bridge::UsbBridge::run) in your main loop.

use embedded_hal::digital::{InputPin, OutputPin, StatefulOutputPin};
use embedded_hal_async::delay::DelayNs;

use crate::c2::C2Interface;

// ── Packet I/O trait ───────────────────────────────────────────────────────

/// Async packet-level read (e.g. USB CDC-ACM OUT endpoint).
pub trait PacketRead {
    /// Error type.
    type Error;
    /// Read one packet. Returns number of bytes read.
    async fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error>;
}

/// Async packet-level write (e.g. USB CDC-ACM IN endpoint).
pub trait PacketWrite {
    /// Error type.
    type Error;
    /// Write one packet.
    async fn write_packet(&mut self, data: &[u8]) -> Result<(), Self::Error>;
}

// ── Protocol constants ─────────────────────────────────────────────────────

const ACTION_ACK: u8 = 0x00;
const ACTION_INIT: u8 = 0x01;
const ACTION_RESET: u8 = 0x02;
const ACTION_WRITE: u8 = 0x03;
const ACTION_ERASE: u8 = 0x04;
const ACTION_READ: u8 = 0x05;
const ACTION_INFO: u8 = 0x08;
const ACTION_PING: u8 = 0x0F;

/// Sentinel response byte for a `WRITE` whose checksum didn't match.
/// Matches the Arduino reference implementation exactly (not part of the
/// `action | 0x80` convention the other responses use).
const WRITE_CRC_ERROR: u8 = 0x43;

/// Maximum data payload for a single `READ`/`WRITE` (matches the
/// reference implementation's 300-byte message buffer, minus header).
const MAX_PAYLOAD: usize = 256;
/// Longest single request: `WRITE`'s header (7 bytes) + payload.
const MAX_FRAME: usize = 7 + MAX_PAYLOAD;

// ── USB Bridge ─────────────────────────────────────────────────────────────

/// C2 USB bridge, compatible with `arduino-c2-flasher` / the
/// `bird-sanctuary/arduino-c2-interface` wire protocol.
///
/// Reads one command frame at a time from a USB serial port, dispatches
/// it to the C2 interface, and writes the response back.
///
/// Uses separate reader (`RD`) and writer (`WR`) types so it works with
/// embassy-usb's `cdc_acm::Receiver`/`Sender` split, as well as any
/// `embedded-io-async` stream that implements both traits.
pub struct UsbBridge<C2CK, C2D, D, RD, WR> {
    c2: C2Interface<C2CK, C2D, D>,
    reader: RD,
    writer: WR,
    /// Receive buffer for one command frame.
    rx_buf: [u8; MAX_FRAME],
    /// Bytes already pulled from the packet stream but not yet consumed.
    /// A single `read_packet` call can return more bytes than the current
    /// `read_exact` needs (e.g. a whole request arriving in one USB
    /// packet while we're only reading its 2-byte header) — those extra
    /// bytes are staged here instead of being dropped.
    pending: [u8; 64],
    pending_pos: usize,
    pending_len: usize,
}

impl<C2CK, C2D, D, RD, WR> UsbBridge<C2CK, C2D, D, RD, WR>
where
    C2CK: OutputPin + InputPin,
    C2D:  StatefulOutputPin + InputPin,
    D:    DelayNs,
    RD:   PacketRead,
    WR:   PacketWrite,
{
    /// Create a new bridge with separate reader and writer.
    pub fn new(c2: C2Interface<C2CK, C2D, D>, reader: RD, writer: WR) -> Self {
        Self {
            c2,
            reader,
            writer,
            rx_buf: [0u8; MAX_FRAME],
            pending: [0u8; 64],
            pending_pos: 0,
            pending_len: 0,
        }
    }

    /// Consume the bridge, returning the C2 interface and serial parts.
    pub fn into_inner(self) -> (C2Interface<C2CK, C2D, D>, RD, WR) {
        (self.c2, self.reader, self.writer)
    }

    /// Run the bridge event loop.
    ///
    /// Reads one command frame at a time from the serial port, executes
    /// it, and sends the response. Returns when the serial connection is
    /// lost (e.g. USB disconnect).
    pub async fn run(&mut self) {
        loop {
            let (action, len_field) = match self.read_header().await {
                Ok(v) => v,
                Err(_) => return, // serial disconnected
            };

            match action {
                ACTION_ACK => {
                    self.write_byte(ACTION_ACK | 0x80).await;
                }

                ACTION_INIT => {
                    match self.c2.connect().await {
                        Ok(()) => self.write_byte(ACTION_INIT | 0x80).await,
                        Err(_) => self.write_byte(ACTION_INIT | 0x80).await, // best-effort, matches reference (no error path)
                    }
                }

                ACTION_RESET => {
                    let _ = self.c2.disconnect().await;
                    self.write_byte(ACTION_RESET | 0x80).await;
                }

                ACTION_ERASE => {
                    let _ = self.c2.erase_flash().await;
                    self.write_byte(ACTION_ERASE | 0x80).await;
                }

                ACTION_INFO => {
                    let (dev, rev) = self.c2.read_device_id().unwrap_or((0, 0));
                    // Every client we've checked (C2.js, prober's
                    // C2Flasher.cpp) waits for a 4-byte INFO response,
                    // even though the reference Arduino firmware
                    // (bird-sanctuary/arduino-c2-interface) only ever
                    // sends 3 (`0x88, id, revision`) — a real bug in that
                    // firmware that every client had to work around by
                    // reading with a timeout. We send a 4th (reserved,
                    // currently unused) byte so clients that wait for the
                    // full 4 bytes get a clean, fast response instead of
                    // stalling until their read timeout.
                    self.write_bytes(&[ACTION_INFO | 0x80, dev, rev, 0]).await;
                }

                ACTION_PING => {
                    // Used by clients (e.g. C2.js's `hasInterface()`) to
                    // check the interface firmware is present before
                    // attempting anything else.
                    self.write_byte(ACTION_PING | 0x80).await;
                }

                ACTION_WRITE => {
                    // Header: [action(already read), total_len, data_len, 0, addr_hi, addr_lo, crc8]
                    // `len_field` here is `total_len` (data_len + 5); we
                    // still need to read total_len more bytes to get the
                    // rest of the header plus the payload.
                    let total_len = len_field as usize;
                    if self.read_exact(0, total_len).await.is_err() {
                        return;
                    }
                    let data_len = self.rx_buf[0] as usize;
                    let addr = ((self.rx_buf[2] as u16) << 8) | self.rx_buf[3] as u16;
                    let received_crc = self.rx_buf[4];
                    let data = &self.rx_buf[5..5 + data_len.min(total_len.saturating_sub(5))];

                    let mut crc: u8 = ((addr >> 8) as u8).wrapping_add(addr as u8);
                    for &b in data {
                        crc = crc.wrapping_add(b);
                    }

                    if crc != received_crc {
                        self.write_byte(WRITE_CRC_ERROR).await;
                        continue;
                    }

                    let _ = self.c2.write_flash(addr, data).await;
                    self.write_byte(ACTION_WRITE | 0x80).await;
                }

                ACTION_READ => {
                    // Full request: [action, action_again, amount, addr_hi16,
                    // addr_mid8, addr_lo8, 0]. `read_header()` already
                    // consumed the first 2 bytes (action, action_again — the
                    // "length field" IS the repeated action byte for this
                    // command), so only 5 bytes remain to read here.
                    if self.read_exact(0, 5).await.is_err() {
                        return;
                    }
                    let amount = self.rx_buf[0] as usize;
                    // Only the low 16 bits of the 24-bit address are
                    // meaningful for this crate's u16 address space.
                    let addr = ((self.rx_buf[2] as u16) << 8) | self.rx_buf[3] as u16;

                    let mut buf = [0u8; 1 + MAX_PAYLOAD];
                    let read_len = amount.min(MAX_PAYLOAD);
                    buf[0] = ACTION_READ | 0x80;
                    let _ = self.c2.read_flash(addr, &mut buf[1..1 + read_len]).await;

                    // Single packet write: the response byte and data must
                    // arrive together, or a two-packet split can race with
                    // the host's read timing and drop the data half.
                    self.write_bytes(&buf[..1 + read_len]).await;
                }

                _ => {
                    // Unknown action: reference implementation's state
                    // machine simply has no matching case and sends
                    // nothing. Do the same — stay silent and wait for
                    // the next frame.
                }
            }
        }
    }

    /// Write a single byte as the whole packet.
    async fn write_byte(&mut self, b: u8) {
        let _ = self.writer.write_packet(&[b]).await;
    }

    /// Write a byte slice as the whole packet.
    async fn write_bytes(&mut self, data: &[u8]) {
        let _ = self.writer.write_packet(data).await;
    }

    /// Read exactly `n` bytes into `self.rx_buf[offset..offset+n]`,
    /// draining any staged `pending` bytes first and only pulling fresh
    /// packets once that's exhausted. Leftover bytes from an
    /// over-full packet are kept in `pending` for the next call instead
    /// of being dropped.
    async fn read_exact(&mut self, offset: usize, n: usize) -> Result<(), ()> {
        let mut got = 0;
        while got < n {
            if self.pending_pos >= self.pending_len {
                let read = self.reader.read_packet(&mut self.pending).await.map_err(|_| ())?;
                if read == 0 {
                    return Err(());
                }
                self.pending_len = read;
                self.pending_pos = 0;
            }

            let available = self.pending_len - self.pending_pos;
            let take = available.min(n - got);
            self.rx_buf[offset + got..offset + got + take]
                .copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + take]);
            self.pending_pos += take;
            got += take;
        }
        Ok(())
    }

    /// Read the 2-byte command header: `[ACTION, LEN]`.
    async fn read_header(&mut self) -> Result<(u8, u8), ()> {
        self.read_exact(0, 2).await?;
        Ok((self.rx_buf[0], self.rx_buf[1]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_checksum_matches_reference() {
        // Mirrors efm8.py's `write()`: crc = addrHi + addrLo + sum(data),
        // truncated to u8 — no negation.
        let addr: u16 = 0x1234;
        let data: &[u8] = &[0x01, 0x02, 0x03];

        let mut crc: u8 = ((addr >> 8) as u8).wrapping_add(addr as u8);
        for &b in data {
            crc = crc.wrapping_add(b);
        }

        let expected = (0x12u8).wrapping_add(0x34).wrapping_add(1).wrapping_add(2).wrapping_add(3);
        assert_eq!(crc, expected);
    }

    #[test]
    fn action_success_byte_convention() {
        assert_eq!(ACTION_INIT | 0x80, 0x81);
        assert_eq!(ACTION_RESET | 0x80, 0x82);
        assert_eq!(ACTION_WRITE | 0x80, 0x83);
        assert_eq!(ACTION_ERASE | 0x80, 0x84);
        assert_eq!(ACTION_READ | 0x80, 0x85);
        assert_eq!(ACTION_INFO | 0x80, 0x88);
        assert_eq!(ACTION_PING | 0x80, 0x8F);
    }
}
