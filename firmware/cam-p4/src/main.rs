//! espion ESP32-P4 firmware: OV9281 capture over MIPI CSI (`esp32p4-csi`), blob detection
//! with subpixel centroids, XVS frame trigger, LED gating with periodic dark frames, SYNC
//! edge capture through ETM, centroid batches to the C5 over SPI.
//!
//! Stub: boots esp-hal and the embassy executor, then idles.

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

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    log::info!("cam-p4 stub running");
    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}
