//! espion ESP32-C5 firmware: Wi-Fi station (5 GHz), UDP to the host, master election, FTM
//! clock sync (responder on the master, initiator elsewhere), 10 Hz SYNC output to the P4,
//! SPI slave for the P4 link.
//!
//! Stub: boots esp-hal, the heap and the embassy executor, then idles. Wi-Fi is not started.

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    log::info!("radio-c5 stub running");
    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}
