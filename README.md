# BLE Quiz Display

Firmware for an nRF52840 microcontroller that receives quiz vote results over Bluetooth Low Energy and renders them as a bar chart on a physical LED matrix -- in real time, with async Rust on bare metal.

The audience votes on quiz questions from their phones; results appear instantly on the LED display across the room. The firmware itself is the subject — async BLE + SPI on a microcontroller with no OS.

## How It Works

```
Audience phones        Quiz server         Mac BLE bridge        nRF52840 SuperMini
(browser)              (Bun + TS)          (Rust + btleplug)     (this firmware)
    |                       |                     |                       |
    |--- POST /vote ------->|                     |                       |
    |                       |<--- GET /results ---|                       |
    |                       |--- JSON ----------->|                       |
    |                       |                     |--- BLE write -------->|
    |                       |                     |   [70, 20, 8, 2]      |
    |                       |                     |                       |--- SPI --> MAX7219
    |                       |                     |                       |           LED matrix
    |                       |                     |                       |
    |                       |                     |                       | [#####] [##..] [#...] [....]
    |                       |                     |                       |  A=70%  B=20%  C=8%   D=2%
```

The board advertises as **"RustQuiz"** over BLE. A connected central (laptop or phone) writes 4 bytes -- one per answer option (A/B/C/D), each representing a percentage 0-100 -- and the LED matrix updates immediately.

## Project Structure

```
src/                       -- firmware (nRF52840, no_std)
  main.rs                  -- entry point, BLE stack setup, GATT server, main event loop
  display.rs               -- MAX7219 SPI driver: bar chart rendering, XOR letter cutout
  blink.rs                 -- reveal-animation helper
bridge/                    -- Mac BLE bridge (Rust + btleplug)
  src/main.rs              -- scans for "RustQuiz", polls quiz-server, writes BLE
quiz-server/               -- audience voting server (Bun + TypeScript)
  server.ts                -- HTTP endpoints + in-memory question/vote state
  public/                  -- audience.html and admin.html
Cargo.toml                 -- firmware deps (Embassy, trouble, nrf-sdc, defmt)
memory.x                   -- linker memory layout for nRF52840 (1MB flash, 256KB RAM)
build.rs                   -- tells the linker where to find memory.x
.cargo/config.toml         -- build target (thumbv7em-none-eabihf) and probe-rs runner
```

## Key Concepts

### `#![no_std]` + `#![no_main]`

No standard library, no OS. The firmware runs directly on the Cortex-M4 core. Embassy provides an async executor that schedules tasks cooperatively.

### BLE without a proprietary blob

The BLE stack is composed of two Rust crates:
- **`trouble-host`** -- pure Rust BLE host (GATT/GAP layer)
- **`nrf-sdc`** -- Nordic's SoftDevice Controller linked as a library, not a pre-flashed blob

Everything compiles into a single binary. No SoftDevice pre-flash step needed.

### GATT service definition via proc macros

```rust
#[gatt_service(uuid = "00001000-b0cd-11ec-871f-d45ddf138840")]
struct QuizService {
    #[characteristic(uuid = "00001001-...", write)]
    votes: [u8; 4],       // [A%, B%, C%, D%]

    #[characteristic(uuid = "00001002-...", write)]
    control: [u8; 2],     // [command, param]
}
```

The `#[gatt_service]` and `#[gatt_server]` macros generate the GATT table, handle serialization, and produce strongly-typed event enums -- no manual byte parsing.

### Async event loop

The main loop awaits GATT events and updates the display directly -- no channels, no shared state, no separate tasks for display updates:

```rust
loop {
    match server.next(&conn, &mut rx).await {
        Ok(ServerEvent::Quiz(QuizServiceEvent::VotesWrite(votes))) => {
            display.show_bars(votes).await;
        }
        // ...
    }
}
```

This is possible because `trouble` delivers GATT events as async values (unlike `nrf-softdevice`, which uses sync callbacks).

### SPI display driver

`display.rs` is a ~160-line driver for a chain of 4 MAX7219 LED matrix modules. Each module represents one answer option (A/B/C/D) and displays a vertical bar proportional to the vote percentage. The driver handles:
- Bar chart rendering (percentage to pixel height)
- Startup sweep animation
- Correct answer reveal (blinking)

## Hardware

| Part | Description | Price |
|---|---|---|
| **nRF52840 SuperMini** | Nice!Nano V2 compatible, ARM Cortex-M4F, BLE 5.0, Pro Micro form factor | ~$6 |
| **MAX7219 8x32 LED matrix** | 4 chained 8x8 red LED modules on a single PCB, SPI interface | ~$5 |

### Wiring

```
nRF52840 SuperMini          MAX7219 module
------------------          --------------
P0.20 (D3)           ---->  CLK
P0.17 (D2)           ---->  DIN
P0.22 (D4)           ---->  CS
VCC (3.3V)           ---->  VCC
GND                  ---->  GND
```

> **Note:** nRF52840 GPIO is 3.3V. MAX7219 officially needs 3.5V HIGH input. In practice 3.3V works on most modules. Add a level shifter if you get flickering.

## BLE Protocol

| | UUID | Type | Description |
|---|---|---|---|
| **Service** | `00001000-b0cd-11ec-871f-d45ddf138840` | -- | Quiz display service |
| **Votes** | `00001001-b0cd-11ec-871f-d45ddf138840` | Write | 4 bytes: `[A%, B%, C%, D%]`, each 0-100 |
| **Control** | `00001002-b0cd-11ec-871f-d45ddf138840` | Write | 2 bytes: `[command, param]` |

**Control commands:**
- `[0x00, 0x00]` -- clear display
- `[0x01, N]` -- reveal correct answer (N = 0-3 for A-D), blinks the bar 3 times

### Manual testing with nRF Connect

1. Scan for "RustQuiz" and connect
2. Find the custom service (UUID `...1000...`)
3. Write to Votes: `46 1E 14 0A` (hex for 70%, 30%, 20%, 10%)
4. The LED matrix shows 4 bars of different heights
5. Write to Control: `01 00` to blink bar A as the correct answer

## Tech Stack

| Layer | Crate | Role |
|---|---|---|
| Async runtime | `embassy-executor`, `embassy-time`, `embassy-nrf` | Async/await on bare metal, hardware peripheral access |
| BLE host | `trouble-host` | Pure Rust GATT/GAP stack |
| BLE controller | `nrf-sdc` | Nordic SoftDevice Controller (linked, not pre-flashed) |
| Display | custom `display.rs` | MAX7219 SPI driver, bar chart rendering |
| Logging | `defmt` + `defmt-rtt` | Structured logging over RTT, viewable with probe-rs |

## Building

```bash
# Prerequisites
rustup target add thumbv7em-none-eabihf
cargo install probe-rs-tools   # only if using an SWD debug probe

# Build
cargo build --release

# Flash via SWD probe
cargo run --release

# Flash via UF2 bootloader (no probe needed):
# 1. Double-tap RST to enter bootloader (USB drive "NRF52BOOT" appears)
# 2. ELF -> raw binary -> UF2. Converting straight from ELF produces a 6+ MB
#    file because the ELF describes both flash (0x26000) and RAM (0x20000000)
#    segments, and uf2conv fills the gap with empty blocks. Strip to a raw
#    binary first:
#    cargo install cargo-binutils uf2conv
#    rustup component add llvm-tools-preview
#    cargo objcopy --release -- -O binary firmware.bin
#    uf2conv firmware.bin --base 0x26000 --family 0xADA52840 -o firmware.uf2
#    # --base must match FLASH ORIGIN in memory.x.
#    # --family 0xADA52840 is the nRF52840-with-Adafruit-bootloader UF2 family ID.
# 3. Copy firmware.uf2 to the NRF52BOOT drive
```

## Why These Choices

| Decision | Reason |
|---|---|
| `trouble` over `nrf-softdevice` | No SoftDevice pre-flash, single binary, pure Rust BLE host, async GATT events |
| MAX7219 LED matrix over OLED/TFT | Bright red LEDs visible across a room (5-10m). 4 matrices = 4 answer bars |
| nRF52840 over ESP32 | First-class Embassy + trouble support. Pure Rust BLE, no C bindings |
| Single-task design | trouble's async events make channels/signals unnecessary |

