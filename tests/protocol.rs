//! Unit tests for the C2 protocol using mock GPIO/delay.

use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::delay::DelayNs;

use c2if::mock::{MockDelay, MockPin, PinEvent};
use c2if::C2Interface;

// ═══════════════════════════════════════════════════════════════════════════
// Basic pin behaviour
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn mock_pin_starts_low() {
    let pin = MockPin::new();
    assert!(!pin.level());
    assert_eq!(pin.queue_len(), 0);
}

#[test]
fn mock_pin_set_high_records_event() {
    let mut pin = MockPin::new();
    pin.set_high().unwrap();
    assert!(pin.level());
    assert_eq!(pin.take_events(), vec![PinEvent::SetHigh]);
}

#[test]
fn mock_pin_set_low_records_event() {
    let mut pin = MockPin::new();
    pin.set_high().unwrap();
    pin.set_low().unwrap();
    assert_eq!(
        pin.take_events(),
        vec![PinEvent::SetHigh, PinEvent::SetLow]
    );
}

#[test]
fn mock_pin_is_high_uses_queue_first() {
    let mut pin = MockPin::new();
    pin.set_low().unwrap();

    // Queue says "high" even though pin is driven low
    pin.push_is_high(true);
    assert!(pin.is_high().unwrap());
    // Queue empty → returns actual level
    assert!(!pin.is_high().unwrap());
}

#[test]
fn mock_pin_is_high_fifo_order() {
    let mut pin = MockPin::new();
    pin.push_is_high(true);
    pin.push_is_high(false);
    pin.push_is_high(true);

    assert!(pin.is_high().unwrap());   // first queued
    assert!(!pin.is_high().unwrap());   // second
    assert!(pin.is_high().unwrap());    // third
    // Queue empty → falls back to level
    assert!(!pin.is_high().unwrap());
}

// ═══════════════════════════════════════════════════════════════════════════
// MockDelay
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn mock_delay_tracks_elapsed() {
    let mut delay = MockDelay::new();
    assert_eq!(delay.elapsed_ns(), 0);

    futures::executor::block_on(delay.delay_ns(1_000));
    assert_eq!(delay.elapsed_ns(), 1_000);

    futures::executor::block_on(delay.delay_us(500));
    assert_eq!(delay.elapsed_ns(), 1_000 + 500_000);

    futures::executor::block_on(delay.delay_ms(2));
    assert_eq!(delay.elapsed_ns(), 1_000 + 500_000 + 2_000_000);
}

#[test]
fn mock_delay_reset() {
    let mut delay = MockDelay::new();
    futures::executor::block_on(delay.delay_ms(10));
    assert_eq!(delay.elapsed_ns(), 10_000_000);
    delay.reset();
    assert_eq!(delay.elapsed_ns(), 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// C2Interface construction
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn new_c2_interface_starts_disconnected() {
    let c2d = MockPin::new();
    let c2ck = MockPin::new();
    let delay = MockDelay::new();
    let c2 = C2Interface::new(c2ck, c2d, delay);
    assert!(!c2.is_connected());
}

#[test]
fn into_pins_returns_original_pins() {
    let c2d = MockPin::new();
    let c2ck = MockPin::new();
    let delay = MockDelay::new();
    let c2 = C2Interface::new(c2ck, c2d, delay);
    let (ck, d): (MockPin, MockPin) = c2.into_pins();
    assert!(!ck.level());
    assert!(!d.level());
}

// ═══════════════════════════════════════════════════════════════════════════
// C2 reset timing via connect()
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn connect_issues_reset_and_advances_delay() {
    let c2d = MockPin::new();
    let c2ck = MockPin::new();
    let delay = MockDelay::new();

    // Pre-queue enough is_high responses so the connect sequence doesn't
    // stall in wait_drdy.
    for _ in 0..50 {
        c2d.push_is_high(true);
    }

    let mut c2 = C2Interface::new(c2ck, c2d, delay);

    // connect() will likely fail at the read_device_id step because
    // the mock doesn't return real device-ID bytes, but it should
    // at least issue the reset sequence.
    let _ = futures::executor::block_on(c2.connect());

    // The C2CK pin should have recorded clock pulses (SetLow→SetHigh pairs).
    let (_, c2ck): (MockPin, MockPin) = c2.into_pins();
    let events = c2ck.take_events();
    assert!(
        events.len() >= 2,
        "Expected at least 2 clock events (reset), got {}",
        events.len()
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// C2 pin activity during reset
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn connect_drives_clock_low_then_high_for_reset() {
    let c2d = MockPin::new();
    let c2ck = MockPin::new();
    let delay = MockDelay::new();

    for _ in 0..50 {
        c2d.push_is_high(true);
    }

    let mut c2 = C2Interface::new(c2ck, c2d, delay);
    let _ = futures::executor::block_on(c2.connect());

    // The C2CK pin should show at least one SetLow→SetHigh pair (the reset).
    let (_, c2ck): (MockPin, MockPin) = c2.into_pins();
    let events = c2ck.take_events();

    // Find the first SetLow→SetHigh pair in the event log.
    let has_low_high = events
        .windows(2)
        .any(|w| w[0] == PinEvent::SetLow && w[1] == PinEvent::SetHigh);
    assert!(
        has_low_high,
        "Expected at least one SetLow→SetHigh pair (C2 reset), got {:?}",
        &events[..events.len().min(10)]
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Registers / enums — TryFrom via num_enum
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn addr_reg_try_from_u8() {
    use core::convert::TryFrom;
    use c2if::AddrReg;

    assert_eq!(AddrReg::try_from(0x00), Ok(AddrReg::DeviceId));
    assert_eq!(AddrReg::try_from(0x01), Ok(AddrReg::Revision));
    assert_eq!(AddrReg::try_from(0x02), Ok(AddrReg::Fpctl));
    assert_eq!(AddrReg::try_from(0xAD), Ok(AddrReg::Fpdat));
    assert_eq!(AddrReg::try_from(0xB4), Ok(AddrReg::FpdatAlt));
    assert!(AddrReg::try_from(0xFF).is_err());
}

#[test]
fn fp_cmd_try_from_u8() {
    use core::convert::TryFrom;
    use c2if::FpCmd;

    assert_eq!(FpCmd::try_from(0x03), Ok(FpCmd::EraseDevice));
    assert_eq!(FpCmd::try_from(0x06), Ok(FpCmd::ReadFlash));
    assert_eq!(FpCmd::try_from(0x07), Ok(FpCmd::WriteFlash));
    assert_eq!(FpCmd::try_from(0x08), Ok(FpCmd::EraseSector));
    assert_eq!(FpCmd::try_from(0x0E), Ok(FpCmd::ReadXram));
    assert_eq!(FpCmd::try_from(0x0F), Ok(FpCmd::WriteXram));
    assert!(FpCmd::try_from(0xFF).is_err());
}

#[test]
fn flags_contains() {
    use c2if::Flags;

    let f = Flags::from_bits_truncate(0x03);
    assert!(f.contains(Flags::OUT_READY));
    assert!(f.contains(Flags::IN_BUSY));

    let f2 = Flags::from_bits_truncate(0x01);
    assert!(f2.contains(Flags::OUT_READY));
    assert!(!f2.contains(Flags::IN_BUSY));

    let f3 = Flags::from_bits_truncate(0x00);
    assert!(!f3.contains(Flags::OUT_READY));
    assert!(!f3.contains(Flags::IN_BUSY));
}

#[test]
fn c2_status_try_from() {
    use core::convert::TryFrom;
    use c2if::C2Status;

    assert_eq!(C2Status::try_from(0x0D), Ok(C2Status::Ok));
    assert_eq!(C2Status::try_from(0x00), Ok(C2Status::Invalid));
    assert_eq!(C2Status::try_from(0x01), Ok(C2Status::NoConnect));
    assert_eq!(C2Status::try_from(0x02), Ok(C2Status::Failed));
    assert_eq!(C2Status::try_from(0x04), Ok(C2Status::Timeout));
    assert_eq!(C2Status::try_from(0x05), Ok(C2Status::BadData));
    assert!(C2Status::try_from(0xFF).is_err());
}

#[test]
fn c2_status_is_ok() {
    use c2if::C2Status;

    assert!(C2Status::Ok.is_ok());
    assert!(!C2Status::Failed.is_ok());
    assert!(!C2Status::Timeout.is_ok());
    assert!(!C2Status::Invalid.is_ok());
}

#[test]
fn layout_constants() {
    use c2if::layout;

    assert_eq!(layout::FLASH_SIZE, 0x4000);
    assert_eq!(layout::PAGE_SIZE, 2048);
    assert_eq!(layout::PAGE_COUNT, 8);
    assert_eq!(layout::SIGNATURE_ADDR, 0x1FFF);
    assert_eq!(layout::VERSION_ADDR, 0x1FFE);
}

// ═══════════════════════════════════════════════════════════════════════════
// Error display / conversion
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn error_display() {
    use c2if::C2Error;

    assert_eq!(format!("{}", C2Error::Timeout), "timeout");
    assert_eq!(format!("{}", C2Error::NotConnected), "not connected");
    assert_eq!(format!("{}", C2Error::InvalidCommand), "invalid command");
    assert_eq!(format!("{}", C2Error::Failed), "command failed");
    assert_eq!(format!("{}", C2Error::BadData), "bad data");
    assert_eq!(format!("{}", C2Error::FlashFailed), "flash failed");
    assert_eq!(format!("{}", C2Error::EraseFailed), "erase failed");
    assert_eq!(format!("{}", C2Error::VerifyFailed), "verify failed");
    assert_eq!(
        format!("{}", C2Error::InvalidDeviceId(0x35)),
        "invalid device ID 0x35"
    );
}

#[test]
fn c2_status_to_error_roundtrip() {
    use c2if::{C2Error, C2Status};

    let cases = [
        (C2Status::Invalid,   C2Error::InvalidCommand),
        (C2Status::NoConnect, C2Error::NotConnected),
        (C2Status::Failed,    C2Error::Failed),
        (C2Status::Timeout,   C2Error::Timeout),
        (C2Status::BadData,   C2Error::BadData),
    ];

    for (status, expected_err) in cases {
        let err: C2Error = status.into();
        assert_eq!(err, expected_err, "status {status:?} mismatch");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// FlashProgrammer
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn flash_write_empty_is_noop() {
    use c2if::FlashProgrammer;

    let c2d = MockPin::new();
    let c2ck = MockPin::new();
    let delay = MockDelay::new();
    let mut c2 = C2Interface::new(c2ck, c2d, delay);
    let mut prog = FlashProgrammer::new(&mut c2);

    let result = futures::executor::block_on(prog.write_flash(0x0000, &[]));
    assert!(result.is_ok());
}

#[test]
fn flash_read_disconnected_returns_error() {
    use c2if::{C2Error, FlashProgrammer};

    let c2d = MockPin::new();
    let c2ck = MockPin::new();
    let delay = MockDelay::new();
    let mut c2 = C2Interface::new(c2ck, c2d, delay);
    let mut prog = FlashProgrammer::new(&mut c2);
    let mut buf: [u8; 0] = [];

    // Reading from a disconnected C2 should return NotConnected.
    let result = futures::executor::block_on(prog.read_flash(0x0000, &mut buf));
    assert_eq!(result.unwrap_err(), C2Error::NotConnected);
}

// ═══════════════════════════════════════════════════════════════════════════
// Platform / ClockDelay
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn nop_delay_spin_cycles_does_not_panic() {
    use c2if::platform::{ClockDelay, NopDelay};

    NopDelay::spin_cycles(0);
    NopDelay::spin_cycles(1);
    NopDelay::spin_cycles(100);
}

#[test]
fn volatile_delay_spin_cycles_does_not_panic() {
    use c2if::platform::{ClockDelay, VolatileDelay};

    VolatileDelay::spin_cycles(0);
    VolatileDelay::spin_cycles(1);
    VolatileDelay::spin_cycles(100);
}
