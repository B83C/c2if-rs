//! C2IF USB Bridge — RP2350 example.
//!
//! Exposes the C2 debug interface over USB CDC-ACM serial, compatible
//! with `arduino-c2-flasher` / `prober`'s C2 mode, so BLHeli_S / Bluejay
//! firmware can be flashed onto bare EFM8 ESC MCUs.
//!
//! Builds and runs the same code path as the ESP32-C3 example, but has
//! only been hardware-verified there; this target hasn't yet been tested
//! end-to-end against a real ESC. Please report back if you try it.
//!
//! ## Wiring
//!
//! ```text
//!  Pico 2          ESC
//!  ──────          ──────
//!  GP3  (C2CK) ──▶ C2CK pad
//!  GP2  (C2D)  ◀──▶ C2D  pad
//!  GND          ──▶ GND  pad
//! ```
//!
//! The ESC must be powered separately (LiPo or bench supply).
//!
//! ## Flashing
//!
//! ```sh
//! cargo build --release
//! picotool load -x -t elf target/thumbv8m.main-none-eabihf/release/c2if-rp2040-bridge
//! ```

#![no_std]
#![no_main]

use defmt_rtt as _;
use panic_probe as _;

use defmt::unwrap;
use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::InterruptHandler;
use embassy_time::Delay;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::driver::EndpointError;
use static_cell::StaticCell;

use embassy_rp::gpio::Flex;
use embedded_hal::digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin};

use c2if::bridge::{PacketRead, PacketWrite, UsbBridge};
use c2if::C2Interface;

/// Adapts an `embassy_rp::gpio::Flex` to `embedded-hal`'s push-pull /
/// input traits, switching the pin's direction on every drive/read call.
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
        self.pin.set_as_input();
        Ok(self.pin.is_high())
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        self.pin.set_as_input();
        Ok(self.pin.is_low())
    }
}

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

// ── Packet I/O adapters for embassy-usb CDC-ACM ────────────────────────────

struct CdcReader<'d> {
    class: embassy_usb::class::cdc_acm::Receiver<'d, embassy_rp::usb::Driver<'d, USB>>,
}

impl<'d> PacketRead for CdcReader<'d> {
    type Error = EndpointError;

    async fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let n = self.class.read_packet(buf).await?;
        Ok(n)
    }
}

struct CdcWriter<'d> {
    class: embassy_usb::class::cdc_acm::Sender<'d, embassy_rp::usb::Driver<'d, USB>>,
}

impl<'d> PacketWrite for CdcWriter<'d> {
    type Error = EndpointError;

    async fn write_packet(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.class.write_packet(data).await
    }
}

// ── Main ───────────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    defmt::info!("C2IF Bridge starting on RP2350");

    // ── USB device setup ──────────────────────────────────────────────
    let driver = embassy_rp::usb::Driver::new(p.USB, Irqs);

    let config = {
        let mut config = embassy_usb::Config::new(0x2e8a, 0x000a);
        config.manufacturer = Some("c2if");
        config.product = Some("C2IF Bridge");
        config.serial_number = Some("c2if-001");
        config.max_power = 100;
        config.max_packet_size_0 = 64;
        config
    };

    let mut builder = {
        static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
        static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
        static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();

        embassy_usb::Builder::new(
            driver,
            config,
            CONFIG_DESCRIPTOR.init([0; 256]),
            BOS_DESCRIPTOR.init([0; 256]),
            &mut [],
            CONTROL_BUF.init([0; 64]),
        )
    };

    // Create CDC-ACM class
    let mut class = {
        static STATE: StaticCell<State> = StaticCell::new();
        let state = STATE.init(State::new());
        CdcAcmClass::new(&mut builder, state, 64)
    };

    // Build USB device
    let usb = builder.build();

    // Spawn USB driver task
    _spawner.spawn(unwrap!(usb_task(usb)));

    // ── C2 pins ──────────────────────────────────────────────────────
    let mut c2d_raw = embassy_rp::gpio::Flex::new(p.PIN_2);
    let mut c2ck_raw = embassy_rp::gpio::Flex::new(p.PIN_3);
    // Idle both lines high before handing off, matching the target's
    // expected reset state (C2CK/C2D are open-drain, pulled up).
    c2ck_raw.set_as_output();
    c2ck_raw.set_high();
    c2d_raw.set_as_output();
    c2d_raw.set_high();
    let c2d = OpenPin::new(c2d_raw);
    let c2ck = OpenPin::new(c2ck_raw);
    let c2 = C2Interface::new(c2ck, c2d, Delay);

    defmt::info!("Waiting for C2 flasher connection...");

    // Wait for USB host to connect
    class.wait_connection().await;
    defmt::info!("USB connected, running bridge");

    // Split CDC-ACM into reader + writer
    let (sender, receiver) = class.split();
    let reader = CdcReader { class: receiver };
    let writer = CdcWriter { class: sender };

    // Run the bridge
    let mut bridge = UsbBridge::new(c2, reader, writer);
    bridge.run().await;

    defmt::warn!("Bridge disconnected");
}

type MyUsbDriver = embassy_rp::usb::Driver<'static, USB>;
type MyUsbDevice = embassy_usb::UsbDevice<'static, MyUsbDriver>;

#[embassy_executor::task]
async fn usb_task(mut usb: MyUsbDevice) -> ! {
    usb.run().await
}
