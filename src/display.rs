//! MAX7219 4-chain LED matrix driver for quiz bar chart display.
//!
//! Wiring (Nice!Nano → MAX7219 chain):
//!   P0.20 (D3) → CLK  (all modules share CLK)
//!   P0.17 (D2) → DIN  (first module's DIN)
//!   P0.22 (D4) → CS   (all modules share CS)
//!   VCC        → 5V   (Nice!Nano has 5V output when USB-powered)
//!   GND        → GND
//!
//! Chain: DIN→[Module 0: D]→DOUT→DIN→[Module 1: C]→...→[Module 3: A]
//! Letters run right-to-left in chain order so the visible layout is
//! D C B A from left to right. Change `OPTION_FOR_MODULE` if rewired.
//!
//! Each module shows a letter label (A/B/C/D) with a full-width (8 px)
//! vertical bar XOR-overlaid on top. The letter (4 px wide, cols 2–5)
//! becomes negative space inside the bar.
//!
//!   ░░░██░░░   0% (A)   ███░░███   100% (A)
//!   ░░█░░█░░             ██░██░██
//!   ░░█░░█░░             ██░██░██
//!   ░░█░░█░░             ██░██░██
//!   ░░████░░             ██░░░░██
//!   ░░█░░█░░             ██░██░██
//!   ░░█░░█░░             ██░██░██
//!   ░░░░░░░░             ████████

use embassy_nrf::gpio::Output;
use embassy_nrf::spim::{Instance, Spim};
use embassy_time::Timer;

const NUM_DEVICES: usize = 4;

// MAX7219 registers
const REG_DECODE_MODE: u8 = 0x09;
const REG_INTENSITY: u8 = 0x0A;
const REG_SCAN_LIMIT: u8 = 0x0B;
const REG_SHUTDOWN: u8 = 0x0C;
const REG_DISPLAY_TEST: u8 = 0x0F;

// Full-width bar (all 8 columns lit when a row is filled).
const BAR_FILLED: u8 = 0b1111_1111;

/// Maps physical chain position → quiz option (A=0, B=1, C=2, D=3).
/// Default `[3, 2, 1, 0]` means module 0 (first in chain) shows D and the
/// chain ends on A — so the audience sees D C B A from left to right.
/// Flip to `[0, 1, 2, 3]` for A-on-left.
const OPTION_FOR_MODULE: [usize; NUM_DEVICES] = [3, 2, 1, 0];

/// 4-wide letter glyphs (A/B/C/D), occupying cols 2–5 of an 8-col module.
/// Rendered as a backdrop and XORed with the bar — wherever the bar is
/// lit, the letter pixels invert (negative-space cutout).
const LETTERS: [[u8; 8]; 4] = [
    // A
    [
        0b0001_1000,
        0b0010_0100,
        0b0010_0100,
        0b0010_0100,
        0b0011_1100,
        0b0010_0100,
        0b0010_0100,
        0b0000_0000,
    ],
    // B
    [
        0b0011_1000,
        0b0010_0100,
        0b0010_0100,
        0b0011_1000,
        0b0010_0100,
        0b0010_0100,
        0b0011_1000,
        0b0000_0000,
    ],
    // C
    [
        0b0001_1100,
        0b0010_0000,
        0b0010_0000,
        0b0010_0000,
        0b0010_0000,
        0b0010_0000,
        0b0001_1100,
        0b0000_0000,
    ],
    // D
    [
        0b0011_1000,
        0b0010_0100,
        0b0010_0100,
        0b0010_0100,
        0b0010_0100,
        0b0010_0100,
        0b0011_1000,
        0b0000_0000,
    ],
];

/// Convert vote percentages to per-module bar bitmaps (bit `8 - row` = lit).
/// Indexes `pct` by option, but writes into the bitmap by module position.
fn bars_from_pct(pct: [u8; 4]) -> [u8; NUM_DEVICES] {
    let mut bar_lit = [0u8; NUM_DEVICES];
    for i in 0..NUM_DEVICES {
        let p = pct[OPTION_FOR_MODULE[i]].min(100) as u16;
        let h = ((p * 8 + 50) / 100) as u8; // 0..=8 rows lit from bottom
        bar_lit[i] = if h >= 8 { 0xFF } else { (1u8 << h) - 1 };
    }
    bar_lit
}

/// Module position that displays the given quiz option.
fn module_for_option(option: usize) -> Option<usize> {
    let mut i = 0;
    while i < NUM_DEVICES {
        if OPTION_FOR_MODULE[i] == option {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub struct QuizDisplay<'d, T: Instance> {
    spi: Spim<'d, T>,
    cs: Output<'d>,
}

impl<'d, T: Instance> QuizDisplay<'d, T> {
    pub fn new(spi: Spim<'d, T>, cs: Output<'d>) -> Self {
        Self { spi, cs }
    }

    /// Initialize all 4 MAX7219 modules.
    pub async fn init(&mut self) {
        // Exit shutdown, configure all modules identically
        self.command_all(REG_DISPLAY_TEST, 0x00).await; // normal operation
        self.command_all(REG_DECODE_MODE, 0x00).await; // no BCD decode — raw pixels
        self.command_all(REG_SCAN_LIMIT, 0x07).await; // display all 8 rows
        self.command_all(REG_INTENSITY, 0x07).await; // brightness 7/15
        self.command_all(REG_SHUTDOWN, 0x01).await; // power on
        self.clear().await;
    }

    /// Show letter labels with no bars (ready state between questions).
    pub async fn clear(&mut self) {
        self.render([0u8; NUM_DEVICES]).await;
    }

    /// Render the display from per-module bar bitmaps.
    ///
    /// `bar_lit[i]` is a vertical bitmap for module `i`: bit `8 - row`
    /// indicates whether the bar is lit at that row (bit 7 = row 1 / top,
    /// bit 0 = row 8 / bottom). The letter glyph is XORed on top, so any
    /// bar-lit pixel that overlaps a letter pixel inverts.
    async fn render(&mut self, bar_lit: [u8; NUM_DEVICES]) {
        for row in 1..=8u8 {
            let row_bit = 1u8 << (8 - row);
            let mut data = [0u8; NUM_DEVICES];
            for i in 0..NUM_DEVICES {
                let bar = if bar_lit[i] & row_bit != 0 { BAR_FILLED } else { 0x00 };
                let letter = LETTERS[OPTION_FOR_MODULE[i]][(row - 1) as usize];
                data[i] = bar ^ letter;
            }
            self.send_raw(&data, row).await;
        }
    }

    /// Display bar chart. Each value in `pct` is 0–100.
    ///
    /// Layout per module (example 75%):
    /// ```text
    ///   ░ ░ ░ ░ ░ ░ ░ ░   row 1 (top)
    ///   ░ ░ ░ ░ ░ ░ ░ ░   row 2
    ///   ░ ■ ■ ■ ■ ■ ■ ░   row 3  ← top of 75% bar
    ///   ░ ■ ■ ■ ■ ■ ■ ░   row 4
    ///   ░ ■ ■ ■ ■ ■ ■ ░   row 5
    ///   ░ ■ ■ ■ ■ ■ ■ ░   row 6
    ///   ░ ■ ■ ■ ■ ■ ■ ░   row 7
    ///   ░ ■ ■ ■ ■ ■ ■ ░   row 8 (bottom)
    /// ```
    pub async fn show_bars(&mut self, pct: [u8; 4]) {
        self.render(bars_from_pct(pct)).await;
    }

    /// Render a single frame of the "blink" animation.
    ///
    /// Alternates the blinked option's module between:
    ///   phase 0: actual vote bar (same as normal)
    ///   phase 1: vertically inverted bar (lit rows ↔ dark rows)
    /// At 0% / 100% this becomes a clean letter ↔ full-lit flash; at
    /// intermediate percentages it oscillates the bar top↔bottom.
    pub async fn show_with_blink(&mut self, pct: [u8; 4], blink_option: usize, phase: u8) {
        let mut bar_lit = bars_from_pct(pct);
        if let Some(module) = module_for_option(blink_option) {
            if phase % 2 == 1 {
                bar_lit[module] = !bar_lit[module];
            }
        }
        self.render(bar_lit).await;
    }

    /// Startup animation — sweep bars left to right, then drain.
    pub async fn startup_animation(&mut self) {
        for module in 0..NUM_DEVICES {
            let mut bar_lit = [0u8; NUM_DEVICES];
            for m in 0..=module {
                bar_lit[m] = 0xFF;
            }
            self.render(bar_lit).await;
            Timer::after_millis(150).await;
        }
        Timer::after_millis(300).await;
        for module in 0..NUM_DEVICES {
            let mut bar_lit = [0xFFu8; NUM_DEVICES];
            for m in 0..=module {
                bar_lit[m] = 0;
            }
            self.render(bar_lit).await;
            Timer::after_millis(150).await;
        }
    }

    // --- private ---

    /// Send the same register+value to all 4 modules.
    async fn command_all(&mut self, register: u8, value: u8) {
        self.send_raw(&[value; NUM_DEVICES], register).await;
    }

    /// Send one register write to each module in the chain.
    ///
    /// `data[0]` goes to the first module (closest to DIN),
    /// `data[3]` goes to the last module.
    ///
    /// MAX7219 chain protocol: data shifts through. The last 16 bits
    /// received by a module are latched on CS rising edge. So we send
    /// the last module's data first.
    async fn send_raw(&mut self, data: &[u8; NUM_DEVICES], register: u8) {
        let mut buf = [0u8; NUM_DEVICES * 2];
        for i in 0..NUM_DEVICES {
            let dev = NUM_DEVICES - 1 - i; // last device first
            buf[i * 2] = register;
            buf[i * 2 + 1] = data[dev];
        }
        self.cs.set_low();
        // Ignoring error: SPI write to a display is best-effort
        let _ = self.spi.write(&buf).await;
        self.cs.set_high();
    }
}
