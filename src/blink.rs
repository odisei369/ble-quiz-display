//! Diagnostic: blink with EARLY watchdog feeding.

#![no_std]
#![no_main]

use defmt_rtt as _;
use panic_probe as _;

use embassy_executor::Spawner;
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::{bind_interrupts, peripherals};
use embassy_time::Timer;

bind_interrupts!(struct Irqs {
    RNG => embassy_nrf::rng::InterruptHandler<peripherals::RNG>;
    EGU0_SWI0 => nrf_sdc::mpsl::LowPrioInterruptHandler;
    CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler;
    RADIO => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TIMER0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    RTC0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
});

const WDT_BASE: u32 = 0x4001_0000;
const WDT_RUNSTATUS: u32 = WDT_BASE + 0x400;
const WDT_REQSTATUS: u32 = WDT_BASE + 0x404;
const WDT_RR_BASE: u32 = WDT_BASE + 0x600;
const WDT_RR_VALUE: u32 = 0x6E52_4635;

/// Feed watchdog -- raw register writes, safe to call anytime.
fn feed_watchdog() {
    unsafe {
        let running = core::ptr::read_volatile(WDT_RUNSTATUS as *const u32) & 1;
        if running == 0 {
            return;
        }
        let req = core::ptr::read_volatile(WDT_REQSTATUS as *const u32);
        for i in 0..8u32 {
            if req & (1 << i) != 0 {
                core::ptr::write_volatile((WDT_RR_BASE + i * 4) as *mut u32, WDT_RR_VALUE);
            }
        }
    }
}

/// Runs before .bss/.data init -- earliest possible code execution.
#[cortex_m_rt::pre_init]
unsafe fn pre_init() {
    feed_watchdog();
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    defmt::info!("blink: main entered");
    feed_watchdog();
    let p = embassy_nrf::init(Default::default());
    defmt::info!("blink: embassy_nrf::init returned");
    feed_watchdog();
    let mut led = Output::new(p.P0_24, Level::Low, OutputDrive::Standard);
    defmt::info!("blink: GPIO P0_24 configured, entering loop");

    // 3 sec ON, 1 sec OFF -- unmistakable
    let mut tick: u32 = 0;
    loop {
        feed_watchdog();
        led.set_low();
        defmt::info!("blink: tick {} LED LOW", tick);
        Timer::after_millis(3000).await;
        feed_watchdog();
        led.set_high();
        defmt::info!("blink: tick {} LED HIGH", tick);
        Timer::after_millis(1000).await;
        tick = tick.wrapping_add(1);
    }
}
