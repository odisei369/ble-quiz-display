//! BLE Quiz Display firmware for nRF52840 SuperMini (Nice!Nano compatible).
//!
//! Uses `trouble` (pure Rust BLE host stack) + `nrf-sdc` (Nordic controller).
//! No SoftDevice blob needed — everything is compiled into one binary.
//!
//! Advertises as "RustQuiz" over BLE. A connected central writes vote
//! percentages to update the 4x MAX7219 LED matrix bar chart in real time.
//!
//! GATT Service UUID: 00001000-b0cd-11ec-871f-d45ddf138840
//!   - Votes char (write):   00001001-...  → [A%, B%, C%, D%]
//!   - Control char (write): 00001002-...  → [cmd, param]
//!     Commands: 0 = clear, 1 = blink option (param = 0–3), 2 = stop blinking
//!
//! NOTE: the `trouble` API is evolving rapidly. If this doesn't compile,
//! check https://github.com/embassy-rs/trouble/tree/main/examples/nrf52
//! for up-to-date examples and adjust accordingly.

#![no_std]
#![no_main]

use defmt::*;
use defmt_rtt as _;
use panic_probe as _;

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::{bind_interrupts, peripherals};
use embassy_time::Timer;
use static_cell::StaticCell;

use trouble_host::prelude::*;

mod display;
use display::QuizDisplay;

// ── Interrupt bindings ────────────────────────────────────────────

bind_interrupts!(struct Irqs {
    TWISPI0 => spim::InterruptHandler<peripherals::TWISPI0>;
    RNG => embassy_nrf::rng::InterruptHandler<peripherals::RNG>;
    EGU0_SWI0 => nrf_sdc::mpsl::LowPrioInterruptHandler;
    CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler;
    RADIO => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TIMER0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    RTC0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
});

#[embassy_executor::task]
async fn mpsl_task(mpsl: &'static nrf_sdc::mpsl::MultiprotocolServiceLayer<'static>) -> ! {
    mpsl.run().await
}

// ── BLE GATT service definition ───────────────────────────────────
// trouble uses similar proc macros to nrf-softdevice but with async event handling

#[gatt_service(uuid = "00001000-b0cd-11ec-871f-d45ddf138840")]
struct QuizService {
    /// Vote percentages: [A%, B%, C%, D%], each 0–100.
    #[characteristic(uuid = "00001001-b0cd-11ec-871f-d45ddf138840", write)]
    votes: [u8; 4],

    /// Control commands: [cmd, param].
    /// cmd=0: clear display. cmd=1: blink option (param = 0–3). cmd=2: stop blinking.
    #[characteristic(uuid = "00001002-b0cd-11ec-871f-d45ddf138840", write)]
    control: [u8; 2],
}

#[gatt_server]
struct Server {
    quiz: QuizService,
}

// ── BLE advertising data ──────────────────────────────────────────

#[rustfmt::skip]
const ADV_DATA: &[u8] = &[
    0x02, 0x01, 0x06,                                                   // Flags
    0x09, 0x09, b'R', b'u', b's', b't', b'Q', b'u', b'i', b'z',       // Name: "RustQuiz"
];

#[rustfmt::skip]
const SCAN_DATA: &[u8] = &[
    // 128-bit service UUID (little-endian)
    0x11, 0x07,
    0x40, 0x88, 0x13, 0xdf, 0x5d, 0xd4, 0x1f, 0x87,
    0xec, 0x11, 0xcd, 0xb0, 0x00, 0x10, 0x00, 0x00,
];

// ── BLE runner task ───────────────────────────────────────────────

/// Runs the BLE controller + host stack in the background.
async fn ble_task(mut runner: Runner<'_, nrf_sdc::SoftdeviceController<'_>, DefaultPacketPool>) {
    loop {
        if let Err(e) = runner.run().await {
            warn!("BLE runner failed: {:?}", e);
        }
    }
}

// ── Pin assignments ───────────────────────────────────────────────
//
// nRF52840 SuperMini (Pro Micro pinout):
//
//   Pro Micro pin   nRF52840 pin   Function
//   ─────────────   ────────────   ────────
//   D2              P0.17          MAX7219 DIN (MOSI)
//   D3              P0.20          MAX7219 CLK (SCK)
//   D4              P0.22          MAX7219 CS
//
// Adjust the pin assignments below if your wiring differs.
// SuperMini pinout matches Nice!Nano V2.

// ── Entry point ───────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("ble: main entered");
    let p = embassy_nrf::init(Default::default());
    info!("ble: embassy_nrf::init returned");

    // ── SPI for MAX7219 display ──────────────────────────────────

    let mut spi_config = spim::Config::default();
    spi_config.frequency = spim::Frequency::M1;
    let spi = Spim::new_txonly(
        p.TWISPI0, Irqs,
        p.P0_20, // SCK  → MAX7219 CLK
        p.P0_17, // MOSI → MAX7219 DIN
        spi_config,
    );
    info!("ble: SPI configured");
    let cs = Output::new(p.P0_22, Level::High, OutputDrive::Standard);
    info!("ble: CS pin configured");

    let mut display = QuizDisplay::new(spi, cs);
    info!("ble: display created, calling init()");
    display.init().await;
    info!("ble: display.init() done, calling startup_animation()");
    display.startup_animation().await;
    info!("Display ready");

    // ── BLE setup ────────────────────────────────────────────────
    // No SoftDevice pre-flash needed — nrf-sdc controller is linked
    // into our binary. trouble-host handles the GATT/GAP layer in Rust.

    let mpsl_p = nrf_sdc::mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.PPI_CH30, p.PPI_CH31);
    let lfclk_cfg = nrf_sdc::mpsl::raw::mpsl_clock_lfclk_cfg_t {
        source: nrf_sdc::mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
        rc_ctiv: nrf_sdc::mpsl::raw::MPSL_RECOMMENDED_RC_CTIV as u8,
        rc_temp_ctiv: nrf_sdc::mpsl::raw::MPSL_RECOMMENDED_RC_TEMP_CTIV as u8,
        accuracy_ppm: nrf_sdc::mpsl::raw::MPSL_DEFAULT_CLOCK_ACCURACY_PPM as u16,
        skip_wait_lfclk_started: nrf_sdc::mpsl::raw::MPSL_DEFAULT_SKIP_WAIT_LFCLK_STARTED != 0,
    };
    static MPSL: StaticCell<nrf_sdc::mpsl::MultiprotocolServiceLayer> = StaticCell::new();
    let mpsl = MPSL.init(nrf_sdc::mpsl::MultiprotocolServiceLayer::new(mpsl_p, Irqs, lfclk_cfg).unwrap());
    spawner.spawn(mpsl_task(&*mpsl)).unwrap();

    let sdc_p = nrf_sdc::Peripherals::new(
        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24, p.PPI_CH25, p.PPI_CH26,
        p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
    );

    let mut rng = embassy_nrf::rng::Rng::new(p.RNG, Irqs);
    let mut sdc_mem = nrf_sdc::Mem::<8192>::new();

    // The nRF52840 has no factory-assigned public BLE address — only a
    // random-static address. Generate one before the SDC builder takes a
    // mutable borrow on `rng` for the rest of its lifetime; otherwise
    // LeSetAdvParams sends own_addr_kind=PUBLIC and the controller rejects
    // with "Invalid HCI Command Parameters".
    let mut bd_addr = [0u8; 6];
    rng.blocking_fill_bytes(&mut bd_addr);
    bd_addr[5] |= 0xC0; // top 2 bits = 0b11 → static random per BT spec
    info!("ble: random static address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
          bd_addr[5], bd_addr[4], bd_addr[3], bd_addr[2], bd_addr[1], bd_addr[0]);

    // Build the BLE controller (Nordic SoftDevice Controller as a library)
    let sdc = nrf_sdc::Builder::new()
        .expect("SDC builder")
        .support_adv()
        .expect("adv support")
        .support_peripheral()
        .expect("peripheral support")
        .peripheral_count(1)
        .expect("peripheral count")
        .buffer_cfg(27, 27, 3, 3)
        .expect("buffer config")
        .build(sdc_p, &mut rng, mpsl, &mut sdc_mem)
        .expect("SDC build");

    // Allocate host resources (connections, L2CAP channels, packet buffers)
    static HOST_RESOURCES: StaticCell<HostResources<DefaultPacketPool, 1, 2>> = StaticCell::new();
    let resources = HOST_RESOURCES.init(HostResources::new());

    // Create the BLE host stack
    let stack = trouble_host::new(sdc, resources)
        .set_random_address(Address::random(bd_addr));
    let Host { mut peripheral, runner, .. } = stack.build();
    let server = Server::new_with_config(
        GapConfig::Peripheral(PeripheralConfig {
            name: "RustQuiz",
            appearance: &appearance::computer::GENERIC_COMPUTER,
        }),
    )
    .expect("GATT server");

    info!("BLE Quiz Display started — advertising as \"RustQuiz\"");

    let _ = embassy_futures::join::join(ble_task(runner), async {
        // ── Main loop: advertise → connect → serve GATT → repeat ────

        // 500 ms per phase → 1 s cycle between the bar and its inverse.
        const BLINK_PHASE_MS: u64 = 500;

        let mut current_votes = [0u8; 4];

        loop {
            // Start advertising
            let adv = Advertisement::ConnectableScannableUndirected {
                adv_data: ADV_DATA,
                scan_data: SCAN_DATA,
            };
            let advertiser = match peripheral.advertise(&Default::default(), adv).await {
                Ok(a) => a,
                Err(e) => {
                    warn!("Advertise error: {:?}", e);
                    continue;
                }
            };

            // Wait for a central to connect
            let acceptor = match advertiser.accept().await {
                Ok(c) => c,
                Err(e) => {
                    warn!("Accept error: {:?}", e);
                    continue;
                }
            };
            let conn = match acceptor.with_attribute_server(&server) {
                Ok(c) => c,
                Err(e) => {
                    warn!("Attribute server error: {:?}", e);
                    continue;
                }
            };
            info!("Central connected");

            // Per-connection animation state.
            let mut blink_target: Option<u8> = None; // quiz option (0..=3)
            let mut blink_phase: u8 = 0;

            // Handle GATT events until disconnect, advancing the blink
            // animation on a timer whenever a target is set.
            'session: loop {
                let event = if blink_target.is_some() {
                    match select(conn.next(), Timer::after_millis(BLINK_PHASE_MS)).await {
                        Either::First(e) => e,
                        Either::Second(_) => {
                            blink_phase = (blink_phase + 1) % 2;
                            if let Some(target) = blink_target {
                                display
                                    .show_with_blink(current_votes, target as usize, blink_phase)
                                    .await;
                            }
                            continue 'session;
                        }
                    }
                } else {
                    conn.next().await
                };

                match event {
                    GattConnectionEvent::Disconnected { reason } => {
                        info!("Disconnected: {:?}", reason);
                        break 'session;
                    }
                    GattConnectionEvent::Gatt { event } => {
                        match &event {
                            GattEvent::Write(evt) => {
                                if evt.handle() == server.quiz.votes.handle {
                                    let data = evt.data();
                                    if data.len() == 4 {
                                        let mut votes = [0u8; 4];
                                        votes.copy_from_slice(data);
                                        info!("Votes: A={}% B={}% C={}% D={}%",
                                              votes[0], votes[1], votes[2], votes[3]);
                                        current_votes = votes;
                                        if let Some(target) = blink_target {
                                            display
                                                .show_with_blink(current_votes, target as usize, blink_phase)
                                                .await;
                                        } else {
                                            display.show_bars(current_votes).await;
                                        }
                                    }
                                } else if evt.handle() == server.quiz.control.handle {
                                    let data = evt.data();
                                    if data.len() == 2 {
                                        info!("Control: cmd={} param={}", data[0], data[1]);
                                        match data[0] {
                                            0 => {
                                                // clear
                                                blink_target = None;
                                                current_votes = [0; 4];
                                                display.clear().await;
                                            }
                                            1 => {
                                                // start continuous blink on option N
                                                let target = data[1];
                                                if target < 4 {
                                                    blink_target = Some(target);
                                                    blink_phase = 1;
                                                    display
                                                        .show_with_blink(current_votes, target as usize, blink_phase)
                                                        .await;
                                                }
                                            }
                                            2 => {
                                                // stop blinking
                                                blink_target = None;
                                                display.show_bars(current_votes).await;
                                            }
                                            _ => warn!("Unknown command: {}", data[0]),
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                        // Acknowledge the event
                        match event.accept() {
                            Ok(reply) => reply.send().await,
                            Err(e) => warn!("Error sending response: {:?}", e),
                        };
                    }
                    _ => {} // Ignore other events
                }
            }

            info!("Disconnected, re-advertising...");
        }
    }).await;
}
