//! C2IF USB Bridge — ESP32-C3 example.
//!
//! Exposes the C2 debug interface over the native USB-Serial-JTAG
//! peripheral, compatible with `arduino-c2-flasher` / `prober`'s C2 mode,
//! so BLHeli_S / Bluejay firmware can be flashed onto bare EFM8 ESC MCUs.
//!
//! Hardware-verified end-to-end against a real EFM8BB51 (device ID
//! `0x39`) ESC: connect, read, erase, and write all confirmed working,
//! including a full Bluejay firmware flash verified via signature
//! readback.
//!
//! ## Wiring
//!
//! ```text
//!  ESP32-C3         ESC
//!  ──────           ──────
//!  GPIO3 (C2CK) ──▶ C2CK pad
//!  GPIO2 (C2D)  ◀──▶ C2D  pad
//!  GND          ──▶ GND  pad
//! ```
//!
//! The ESC must be powered separately (LiPo or bench supply).
//!
//! ## Flashing
//!
//! ```sh
//! cargo build --release
//! espflash flash target/riscv32imc-unknown-none-elf/release/c2if-esp32c3-bridge
//! ```

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_println::println;

use embassy_executor::Spawner;
use embassy_time::Delay;

use esp_hal::gpio::{Flex, Pull};
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx, UsbSerialJtagTx};
use esp_hal::Async;

use embedded_hal::digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin};
use embedded_io_async::{Read, Write};

use c2if::bridge::{PacketRead, PacketWrite, UsbBridge};
use c2if::C2Interface;

/// Adapts an `esp_hal::gpio::Flex` to `embedded-hal`'s push-pull / input
/// traits, switching the pin's direction on every drive/read call.
///
/// C2CK is push-pull output only; C2D is bidirectional (driven by the
/// host during WRITE, released to input during READ), so both need this.
struct OpenPin<'d> {
    pin: Flex<'d>,
}

impl<'d> OpenPin<'d> {
    fn new(pin: Flex<'d>) -> Self {
        Self { pin }
    }
}

impl<'d> ErrorType for OpenPin<'d> {
    type Error = core::convert::Infallible;
}

impl<'d> OutputPin for OpenPin<'d> {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.pin.set_as_output();
        self.pin.set_low();
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.pin.set_as_output();
        self.pin.set_high();
        Ok(())
    }
}

impl<'d> StatefulOutputPin for OpenPin<'d> {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.pin.is_set_high())
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        Ok(self.pin.is_set_low())
    }
}

impl<'d> InputPin for OpenPin<'d> {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        // C2D is pulled up externally by the target; float (input, no
        // internal pull) so we read the real bus level.
        self.pin.set_as_input(Pull::None);
        Ok(self.pin.is_high())
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        self.pin.set_as_input(Pull::None);
        Ok(self.pin.is_low())
    }
}

// ── Packet I/O adapters for USB-Serial-JTAG ─────────────────────────────────

struct JtagReader<'d> {
    port: UsbSerialJtagRx<'d, Async>,
}

impl<'d> PacketRead for JtagReader<'d> {
    type Error = core::convert::Infallible;

    async fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.port.read(buf).await
    }
}

struct JtagWriter<'d> {
    port: UsbSerialJtagTx<'d, Async>,
}

impl<'d> PacketWrite for JtagWriter<'d> {
    type Error = core::convert::Infallible;

    async fn write_packet(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        // `embedded_io_async::Write::write` may do a partial write (like
        // POSIX write()) — loop until every byte is actually sent, or a
        // short write here silently truncates the response.
        let mut sent = 0;
        while sent < data.len() {
            sent += self.port.write(&data[sent..]).await?;
        }
        self.port.flush().await
    }
}

// ── Main ───────────────────────────────────────────────────────────────────

#[esp_hal_embassy::main]
async fn main(_spawner: Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default());

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_hal_embassy::init(timg0.timer0);

    println!("C2IF Bridge starting on ESP32-C3");

    // ── C2 pins ──────────────────────────────────────────────────────
    let mut c2ck_raw = Flex::new(peripherals.GPIO3);
    let mut c2d_raw = Flex::new(peripherals.GPIO2);
    c2ck_raw.set_as_output();
    c2d_raw.set_as_output();
    // Idle both lines high before handing off, matching the target's
    // expected reset state (C2CK/C2D are open-drain, pulled up).
    c2ck_raw.set_high();
    c2d_raw.set_high();
    let c2ck = OpenPin::new(c2ck_raw);
    let c2d = OpenPin::new(c2d_raw);
    let c2 = C2Interface::new(c2ck, c2d, Delay);

    // ── USB Serial/JTAG ──────────────────────────────────────────────
    let usb_serial = UsbSerialJtag::new(peripherals.USB_DEVICE).into_async();
    let (rx, tx) = usb_serial.split();

    let reader = JtagReader { port: rx };
    let writer = JtagWriter { port: tx };

    println!("Waiting for C2 flasher connection...");

    let mut bridge = UsbBridge::new(c2, reader, writer);
    bridge.run().await;

    println!("Bridge disconnected");
    loop {}
}
