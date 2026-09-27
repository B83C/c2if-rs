# The Silicon Labs C2 Debug Interface

A technical reference for the C2 protocol and how `c2if` implements it.

---

## 1. What is C2?

C2 is a proprietary two-wire debug interface designed by Silicon Labs for their
8051-based microcontrollers. It is the *only* way to read and write flash on
EFM8 chips — there is no bootloader, no JTAG, no SWD. If you want to flash
BLHeli_S onto a BusyBee ESC, you must use C2.

The interface is defined in
[Silicon Labs AN127](https://www.silabs.com/documents/public/application-notes/an127.pdf)
and uses just two signals:

| Signal | Direction | Purpose |
|--------|-----------|---------|
| **C2CK** | Host → Target | Clock — driven entirely by the host |
| **C2D** | Bidirectional | Data — host drives for writes, target drives for reads |

Both lines are directly connected to the target's GPIO pads on the ESC
board. The target must be powered externally (LiPo battery or bench supply);
C2 carries no power.

---

## 2. Wire-level protocol

### 2.1 Clock

C2 is **host-clocked** — the target has no internal oscillator connected to
the debug port. Every data transition on C2D is synchronised to a falling
edge on C2CK.

The clock pulse timing requirements (from AN127 §3):

- **C2CK low time**: ≥ 20 ns minimum
- **C2CK high time**: no minimum (but must be long enough for the target to
  sample C2D)
- **Setup time**: C2D must be stable before the next falling C2CK edge

On a 125 MHz RP2040, one CPU cycle is 8 ns. Two NOP instructions give
16 ns of low time — just under the 20 ns minimum. The actual GPIO
write overhead adds ~3 cycles, bringing the total to ~40 ns, which
satisfies the requirement with margin.

### 2.2 Bit ordering

All multi-bit fields are transmitted **LSB first**. A byte `0xA5` is sent
as: `1, 0, 1, 0, 0, 1, 0, 1`.

### 2.3 Transaction structure

Every C2 transaction follows the same pattern:

```
START   INS(2 bits)   DATA/ADDRESS   WAIT    STOP
  ↓         ↓              ↓           ↓       ↓
 C2CK      C2D            C2D        C2D     C2CK
 pulse    driven         driven     polled   pulse
```

1. **START**: A single C2CK pulse (low → high).
2. **INS**: A 2-bit instruction code driven on C2D:
   - `10` — Read Address Register (AR)
   - `11` — Write Address Register
   - `00` — Read Data Register (DR)
   - `01` — Write Data Register
3. **DATA/ADDRESS**: 8 bits of payload, LSB first.
4. **WAIT** (reads only): Host pulses C2CK repeatedly until C2D goes high,
   indicating the target has data ready. This is a polling loop.
5. **STOP**: A single C2CK pulse.

### 2.4 Address register vs Data register

The C2 interface has two registers:

- **Address Register (AR)**: An 8-bit pointer that selects which peripheral
  register to access (device ID, revision, flash control, flash data, etc.).
- **Data Register (DR)**: An 8-bit data port for reading/writing the
  register currently selected via AR.

A typical read is a two-step process:
1. Write the target address to AR
2. Read one byte from DR

A typical write is the reverse:
1. Write the target address to AR
2. Write one byte to DR

### 2.5 Polling (wait_drdy)

After writing to DR, the target processes the command asynchronously. The
host must poll the AR status flags to know when the operation completes:

```
Bit 0 (OUT_READY): Target has data available for the host to read
Bit 1 (IN_BUSY):   Target is still processing the host's last write
```

The host reads AR in a tight loop. If `IN_BUSY` is clear, the write
completed. If `OUT_READY` is set, the host can read DR.

---

## 3. Flash programming via C2

The EFM8 flash programming interface is accessed through the **FPDAT**
register. Its address varies by device: `0xAD` on most C8051Fxxx parts,
but `0xB4` on C8051F32x, SI100x, and — confirmed against the EFM8BB51
Reference Manual §26.4.1 and real hardware (device ID `0x39`) — **EFM8BB51
too**. `select_fpdat()` in `c2.rs` picks the right one from the device ID
read during `connect()`.

### 3.1 Connection sequence

Before any flash operations, the host must unlock the flash programming
engine:

```
1. C2 Reset:
     C2CK low  ≥ 20 µs
     C2CK high ≥ 2 µs

2. Unlock FPCTL:
     Write AR = 0x02 (FPCTL)
     Write DR = 0x02 (enable key 1)
     Write DR = 0x04 (enable key 2)
     Write DR = 0x01 (release)
     Wait ≥ 80 µs
     Wait ≥ 25 ms  (target initializes flash controller)

3. Verify connection:
     Read Device ID (AR=0x00, read DR)
     Select FPDAT address based on device ID
```

The EFM8BB51 Reference Manual documents a 2-byte enable sequence
(`0x02`, `0x01`), but the verified-on-hardware sequence — matching
[bird-sanctuary/arduino-c2-interface](https://github.com/bird-sanctuary/arduino-c2-interface)'s
`C2::init()` — is the 3-byte one above. Real hardware responds to the
3-byte sequence; going with what's proven to work.

### 3.2 Flash operations

All flash operations go through FPDAT as a command/response protocol,
and every multi-step command (block read, block write, page erase,
device erase) gets a `0x0D` status-byte acknowledgment read back after
its command byte, and again after its final parameter byte — see AN127
Figures 3–6 for the exact per-command sequences.

**Write to FPDAT:**
```
1. Write AR = FPDAT address
2. Write DR = command byte
3. Poll until IN_BUSY is clear
4. Write DR = parameter/data bytes (one per poll cycle)
```

**Read from FPDAT:**
```
1. Poll until OUT_READY is set
2. Write AR = FPDAT address
3. Read DR = response/data byte
```

### 3.3 Command set

| Command | Code | Description |
|---------|------|-------------|
| Erase device | `0x03` | Mass-erase entire flash (followed by confirm bytes `DE AD A5`) |
| Read flash | `0x06` | Read N bytes from flash at a 16-bit address |
| Write flash | `0x07` | Write N bytes to flash at a 16-bit address |
| Erase sector | `0x08` | Erase a single 2048-byte page |
| Read XRAM | `0x0E` | Read N bytes from extended RAM |
| Write XRAM | `0x0F` | Write N bytes to extended RAM |

### 3.4 Flash layout (EFM8BB51)

```
Address range     Content
──────────────────────────────────────────
0x0000–0x3FFF   Firmware code, EEPROM/settings block (~0x3000+)
0xF000+         Bootloader block — NOT reachable via C2
```

`c2if` only reads/writes `0x0000-0x3FFF` (16 KiB), confirmed against
`bird-sanctuary/arduino-c2-interface`'s Python reference client, which
notes explicitly that the bootloader region above `0xF000` never
responds over the C2 interface on BB51 — it can only be reached by the
target's own UART software bootloader, not the silicon debug port.
Flash is organized in 2048-byte pages.

---

## 4. Implementation in c2if

### 4.1 Architecture

```
┌─────────────────────────────────────────────────┐
│  Application / C2 flasher (prober, etc.)        │
├──────────────┬──────────────────────────────────┤
│  UsbBridge   │  FlashProgrammer                 │
│  (bridge.rs) │  (flash.rs)                      │
├──────────────┴──────────────────────────────────┤
│  C2Interface  (c2.rs)                           │
│  ┌──────────────────────────────────────────┐   │
│  │  clock_pulse / write_bit / read_bit      │   │
│  │  ar_read / ar_write / dr_read / dr_write │   │
│  │  fpdat_write / fpdat_read / poll_*       │   │
│  └──────────────────────────────────────────┘   │
├─────────────────────────────────────────────────┤
│  Platform abstraction  (platform.rs)            │
│  ┌─────────────────┐  ┌────────────────────┐   │
│  │ NopDelay (ARM)  │  │ VolatileDelay      │   │
│  │ NOP + interrupt │  │ (any target)       │   │
│  │   free()        │  │                    │   │
│  └─────────────────┘  └────────────────────┘   │
├─────────────────────────────────────────────────┤
│  embedded-hal 1.0 traits                        │
│  OutputPin / InputPin / StatefulOutputPin       │
│  DelayNs (async)                                │
└─────────────────────────────────────────────────┘
```

### 4.2 Portability via embedded-hal

The library is generic over three types:

```rust
pub struct C2Interface<C2CK, C2D, D> {
    c2ck: C2CK,   // OutputPin + InputPin
    c2d: C2D,     // StatefulOutputPin + InputPin
    delay: D,     // DelayNs (async)
}
```

Any MCU that implements these traits works. On RP2040 you pass
`embassy_rp::gpio::FlexPin`; on STM32 you pass `embassy_stm32::gpio::Pin`;
on an ESP32 you pass any `embedded-hal` GPIO. The library doesn't care.

### 4.3 Timing-critical code

The C2 clock pulse is the most timing-sensitive operation. The original
Arduino implementation uses inline assembly:

```c
void Pulse_C2CLK(void) {
    C2CK_PORT &= ~C2CK_BIT;   // Direct port register write
    asm volatile("nop\n\t");   // Exact 1 cycle
    asm volatile("nop\n\t");   // Exact 1 cycle
    C2CK_PORT |= C2CK_BIT;    // Direct port register write
}
// Entire register transaction wrapped in cli()/sei()
```

Our Rust equivalent:

```rust
fn clock_pulse(&mut self) {
    Self::pin_low(&mut self.c2ck);
    core::sync::atomic::compiler_fence(SeqCst);
    <Self as HasClockDelay>::spin(self);   // 2x NOP in critical section
    core::sync::atomic::compiler_fence(SeqCst);
    Self::pin_high(&mut self.c2ck);
}
```

The `spin()` call runs inside `cortex_m::interrupt::free()`, which
disables interrupts for the duration of the pulse. This prevents an
ISR from firing between `pin_low` and `pin_high` and stretching the
clock low time beyond the C2 specification.

`compiler_fence(SeqCst)` ensures the compiler cannot reorder the GPIO
writes around the NOP spin — the low transition is visible on the pin
before the delay begins, and the delay completes before the high
transition.

### 4.4 async/await

The `DelayNs` trait is async because flash operations involve multi-
millisecond waits (25 ms after connection, 20 ms after erase). Using
async delays lets the MCU run other tasks during these waits — critical
on single-threaded embedded systems where you might want to handle USB
packets concurrently.

The bit-bang primitives (`clock_pulse`, `write_bit`, `read_bit`) are
synchronous because they must complete in sub-microsecond time. Making
them async would add executor overhead that violates timing.

### 4.5 Error handling

The crate defines a unified `C2Error` enum that maps from the target's
status bytes:

```
Target status byte     C2Error variant
───────────────────────────────────────
0x00 (Invalid)    →    InvalidCommand
0x01 (NoConnect)  →    NotConnected
0x02 (Failed)     →    Failed
0x04 (Timeout)    →    Timeout
0x05 (BadData)    →    BadData
0x0D (Ok)         →    (success, never an error)
```

Library-level errors (`FlashFailed`, `VerifyFailed`, `InvalidDeviceId`)
are added for higher-level operations.

---

## 5. The C2 USB bridge

### 5.1 Why a bridge?

Existing C2 flashing tools —
[`arduino-c2-flasher`](https://stylesuxx.github.io/arduino-c2-flasher/)
(browser, WebSerial) and [`prober`](https://github.com/hackrfstuff/prober)
(native CLI/GUI) — expect a serial device running a specific, simple
binary protocol on the other end. That protocol was originally
implemented by an Arduino sketch
([bird-sanctuary/arduino-c2-interface](https://github.com/bird-sanctuary/arduino-c2-interface)).
`c2if`'s `UsbBridge` implements the same protocol, so any MCU with USB
support (ESP32-C3, RP2040, RP2350, …) can act as a drop-in replacement
for that Arduino — no separate hardware needed.

Two earlier protocol designs were tried and abandoned before landing on
this one:

- A hand-rolled ASCII `^hex$`-framed protocol, invented from scratch —
  no existing client speaks it.
- The real BLHeli "4-way interface" protocol (as used by BLHeliSuite,
  betaflight, esc-configurator) — implemented faithfully (CRC16/XMODEM
  framing, `0x2F`/`0x2E` start bytes, full command set), but it turned
  out none of those clients' own native-C2 (`SiLC2`) backend is actually
  implemented server-side in the open-source references (betaflight's
  `serial_4way.c` only ever calls into a UART software bootloader
  client, never a raw C2 debug-interface one) — so a compliant native-C2
  bridge speaking this protocol has no compliant client to talk to
  through it.

### 5.2 Wire format

Every command is `[ACTION, LEN_OR_ARG, ...]`; every successful response
starts with `ACTION | 0x80`. A few commands carry extra bytes:

```
PC → MCU:   03 07 02 00 20 00 85 AA BB
            ── ── ── ── ───── ── ─────
            │  │  │  │   │    │    └─ data (2 bytes)
            │  │  │  │   │    └─ checksum (8-bit sum)
            │  │  │  │   └─ address (0x2000)
            │  │  │  └─ reserved (always 0)
            │  │  └─ data length
            │  └─ total length (data_len + 5)
            └─ action = WRITE

MCU → PC:   83
            ── success (ACTION_WRITE | 0x80)
```

### 5.3 Command set

| Action | Name  | Request                                                      | Response                          |
|--------|-------|---------------------------------------------------------------|------------------------------------|
| `0x00` | ACK   | `[0x00, 0x00]`                                                | `[0x80]`                          |
| `0x01` | INIT  | `[0x01, 0x00]`                                                | `[0x81]`                          |
| `0x02` | RESET | `[0x02, 0x00]`                                                | `[0x82]`                          |
| `0x03` | WRITE | `[0x03, total_len, data_len, 0, addr_hi, addr_lo, crc8, data…]` | `[0x83]` (or `[0x43]` on bad CRC) |
| `0x04` | ERASE | `[0x04, 0x00]`                                                | `[0x84]`                          |
| `0x05` | READ  | `[0x05, 0x05, amount, addr_hi16, addr_mid8, addr_lo8, 0]`      | `[0x85, data…]`                   |
| `0x08` | INFO  | `[0x08, 0x00]`                                                | `[0x88, device_id, revision, 0]`  |
| `0x0F` | PING  | `[0x0F, 0x00]`                                                | `[0x8F]`                          |

Notes:

- `WRITE`'s checksum is a plain 8-bit sum: `addr_hi + addr_lo +
  sum(data)`, truncated to `u8` — no two's-complement negation.
- `READ`'s address is transmitted as 24 bits, but `c2if` only uses the
  low 16 bits (the underlying `C2Interface` address space is `u16`).
- `INFO`'s response is 4 bytes even though the original Arduino firmware
  only ever sends 3 (`0x88, id, revision`) — every client we checked
  (`C2.js`, `prober`'s `C2Flasher.cpp`) waits for 4 bytes regardless, so
  `c2if` sends a reserved 4th byte to avoid clients stalling on their
  read timeout.
- `PING` exists purely so clients can check the interface firmware is
  present (`hasInterface()` in `C2.js`) before doing anything else — it
  isn't documented in the reference Arduino source's command list but is
  used by every client.

### 5.4 Typical flashing session

```
Client          c2if bridge
──────          ───────────
0F00            → ping
  ← 8F          ← interface present
0200            → reset
  ← 82          ← OK
0100            → init (C2 connect)
  ← 81          ← OK
0800            → device info
  ← 88 39 02 00 ← device_id=0x39, revision=0x02
0400            → mass erase
  ← 84          ← OK
03 07 02 00 20 00 85 AA BB
                → write [0xAA,0xBB] @ 0x2000
  ← 83          ← OK
05 05 02 00 20 00 00
                → read 2 bytes @ 0x2000
  ← 85 AA BB    ← readback matches
```

### 5.5 Implementation

The `UsbBridge` struct owns both a `C2Interface` and separate reader/
writer halves of a serial port:

```rust
pub struct UsbBridge<C2CK, C2D, D, RD, WR> {
    c2: C2Interface<C2CK, C2D, D>,
    reader: RD,                // PacketRead
    writer: WR,                // PacketWrite
    rx_buf: [u8; MAX_FRAME],
    pending: [u8; 64],         // staged bytes from an over-full USB packet
    pending_pos: usize,
    pending_len: usize,
}
```

`run()` reads one command's header, dispatches to the matching
`c2.rs` method, and writes the response back as a single packet. The
`pending` staging buffer matters: a single USB packet can contain more
bytes than the current read needs (e.g. a 7-byte `READ` request arriving
whole while the dispatcher only wants its 2-byte header first) — without
staging the leftover bytes, they'd be silently dropped and the next read
would hang forever waiting for a byte that already arrived. On serial
read error (USB disconnect), `run()` returns so the caller can restart
USB enumeration.

---

## 6. Building for a new MCU

To port `c2if` to a new target:

1. **GPIO**: Ensure your MCU's GPIO pins implement
   `embedded_hal::digital::OutputPin + InputPin` for C2CK, and
   `StatefulOutputPin + InputPin` for C2D.

2. **Delay**: Pass any `embedded_hal_async::delay::DelayNs` impl
   (e.g. `embassy_time::Timer`).

3. **Clock timing**: Implement `ClockDelay` for your architecture
   if it's not Cortex-M:

   ```rust
   impl ClockDelay for MyPlatform {
       fn spin_cycles(cycles: u32) {
           // Platform-specific NOP or busy-wait
       }
   }
   ```

4. **USB bridge** (optional): If you want `arduino-c2-flasher` /
   `prober` compatibility, implement `PacketRead` + `PacketWrite` for
   your serial port (see the ESP32-C3 / RP2040 examples for
   embassy-usb / USB-Serial-JTAG adapters) and pass it to `UsbBridge`.

5. **Cargo features**:
   - `cortex-m` — enables ARM NOP + interrupt-free critical section
   - `embassy` — re-exports `embassy_time`
   - `bridge` — enables the USB bridge module
   - `mock` — enables mock GPIO/delay for testing (requires `std`)

---

## References

- [Silicon Labs AN127](https://www.silabs.com/documents/public/application-notes/an127.pdf) — C2 Debug Interface / FPDAT flash programming specification
- [EFM8BB51 Reference Manual](https://www.silabs.com/documents/public/reference-manuals/efm8bb51-rm.pdf) — BusyBee family register map, including the `0xB4` FPDAT address
- [bird-sanctuary/arduino-c2-interface](https://github.com/bird-sanctuary/arduino-c2-interface) — reference Arduino C2 firmware and Python client; the bridge protocol this crate implements
- [stylesuxx/arduino-c2-flasher](https://stylesuxx.github.io/arduino-c2-flasher/) — browser-based client for the above protocol
- [hackrfstuff/prober](https://github.com/hackrfstuff/prober) — native CLI/GUI client, independently verified against this crate's bridge
- [bird-sanctuary/bluejay](https://github.com/bird-sanctuary/bluejay) — Bluejay ESC firmware releases
