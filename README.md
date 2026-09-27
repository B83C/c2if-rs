# c2if

[![Crates.io](https://img.shields.io/crates/v/c2if.svg)](https://crates.io/crates/c2if)
[![docs.rs](https://img.shields.io/docsrs/c2if)](https://docs.rs/c2if)

Portable `no_std` Rust library for flashing EFM8-based BLHeli_S / Bluejay
ESCs (Silicon Labs BusyBee family: BB1, BB21, BB51) via the **C2 debug
interface** — the same two-wire protocol Silicon Labs' AN127 describes,
and the only way to program these MCUs when they have no bootloader (or
a corrupted one).

Turns any MCU that can bit-bang two GPIO pins and provide an async delay
into a USB "C2 flasher" — compatible with existing tools
([`arduino-c2-flasher`](https://stylesuxx.github.io/arduino-c2-flasher/),
[`prober`](https://github.com/hackrfstuff/prober)) that already speak this
wire protocol, so you don't need a dedicated Arduino to flash a bare or
bricked BB51 ESC.

## Status

Hardware-verified end-to-end on an **ESP32-C3** against a real EFM8BB51
(device ID `0x39`): connect, read, erase, write, and a full Bluejay
firmware flash — confirmed via signature readback and cross-checked with
an independent native client ([`prober`](https://github.com/hackrfstuff/prober)).

The RP2040 and RP2350 (Pico / Pico 2) examples build and use the same
code path, but haven't yet been verified against real hardware — please
[open an issue](../../issues) if you try one.

## Quick start

```rust,ignore
use embassy_executor::Spawner;
use c2if::{C2Interface, C2Error};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let mut c2 = C2Interface::new(p.PIN_2, p.PIN_3, embassy_time::Delay);
    c2.connect().await.unwrap();
    let (dev, rev) = c2.read_device_id().unwrap();
    c2.erase_flash().await.unwrap();
    c2.go().await.unwrap();
}
```

See `examples/` for complete, flashable firmware images (ESP32-C3,
RP2040, RP2350) that expose the interface over USB serial.

## Wiring

```text
 Programmer          ESC
 ────────────        ──────
 C2CK (out)  ────▶  C2CK pad
 C2D  (bidir) ◀──▶  C2D  pad
 GND          ────▶  GND  pad
```

The ESC must be powered separately (LiPo or bench supply) — the
programmer only drives the two C2 signal lines, not the target's power
rail.

## Flashing a bare/bricked ESC

1. Flash one of the `examples/` firmware images onto your MCU (ESP32-C3
   recommended — it's the hardware-verified target).
2. Wire it to the ESC's C2 pads per the diagram above.
3. Point [`prober`](https://github.com/hackrfstuff/prober) (native CLI/GUI,
   recommended) or [`arduino-c2-flasher`](https://stylesuxx.github.io/arduino-c2-flasher/)
   (browser-based) at the serial port:
   ```sh
   prober --c2 --c2-port /dev/ttyACM0 --c2-detect
   prober --c2 --c2-port /dev/ttyACM0 --c2-read-info
   prober --c2 --c2-port /dev/ttyACM0 --c2-write-hex path/to/bluejay.hex
   ```

## Feature flags

| Feature   | Default | Description                                         |
|-----------|---------|------------------------------------------------------|
| `embassy` | ✓       | `embassy-time`-based `DelayNs` support               |
| `cortex-m`| ✓       | ARM Cortex-M `NOP`-based clock timing                |
| `bridge`  |         | USB serial bridge (`UsbBridge`), needs `embedded-io-async` |
| `mock`    |         | In-memory GPIO/delay mocks for testing (needs `std`) |
| `defmt-impl` |      | `defmt::Format` on error types                       |

Non-ARM targets (RISC-V, Xtensa, …) fall back to a portable
volatile-read busy-loop for clock timing — see [`platform::ClockDelay`]
if you need cycle-accurate timing on your architecture.

## Protocol

See [PROTOCOL.md](PROTOCOL.md) for the full wire protocol this crate's
`bridge` module implements, and the low-level C2/FPDAT sequences
`C2Interface` speaks to the target.

## Acknowledgments

The C2 bridge protocol and much of the FPDAT command sequencing here
were cross-checked against two independent, real-hardware-verified
implementations:

- [bird-sanctuary/arduino-c2-interface](https://github.com/bird-sanctuary/arduino-c2-interface)
  (GPL-3.0) — the reference Arduino firmware and Python client this
  crate's bridge protocol is compatible with.
- [hackrfstuff/prober](https://github.com/hackrfstuff/prober) — a native
  CLI/GUI flashing tool with an independent C++ implementation of the
  same protocol, used to verify this crate's bridge against real hardware.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
