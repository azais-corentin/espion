//! Throwaway proof of concept on the Waveshare ESP32-P4-ETH: Ethernet and IPv6 (embassy-net)
//! and how fast the board can send, the base of the camera streaming firmware.
//!
//! On boot it switches the log UART to [`LOG_BAUD`], prints the chip and memory facts, brings
//! up PSRAM, then the network (`net`: EMAC, IP101 PHY, embassy-net on the link-local address
//! `fe80::4553:9281`), and starts the service [`SERVICE`] selects.
//!
//! Board facts (Waveshare `ESP32-P4-ETH-datasheet.pdf`, the schematic, sheet 1):
//! - UART0 to the CH343P USB-UART bridge (U4): P4 GPIO37 = U0TXD -> CH343 RXD, CH343 TXD ->
//!   GPIO38 = U0RXD. CH343 RTS/DTR drive EN and GPIO35 (BOOT) through U5 for auto-reset.
//! - Ethernet: IP101GRI PHY U10 on RMII; pins in `net`.
//! - Camera: CSI connector J1, SCCB on GPIO7/GPIO8; set-up in `camera`.

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
use esp_hal::psram::{Psram, PsramConfig, PsramTimingParams};
use esp_hal::timer::timg::TimerGroup;
use log::{error, info};
use static_cell::StaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

mod bench;
mod camera;
mod cpu;
mod irq;
mod net;
mod stream;

/// UART0 log baud rate the app switches to right at boot (the ROM and the bootloader print
/// at 115200). Capture with `espflash monitor --monitor-baud <LOG_BAUD>`. `None` keeps 115200.
const LOG_BAUD: Option<u32> = Some(2_000_000);

/// What runs on top of the network.
#[allow(dead_code, reason = "alternatives, selected by editing `SERVICE`")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Service {
    /// The network only: ping and neighbour discovery.
    Network,
    /// The test service (`bench`): board-to-host throughput from internal RAM, PSRAM or the
    /// camera, and the host-to-board RX check, driven by `tools/netbench.py`.
    Bench,
    /// The camera stream (`stream`): the `espion_proto::cam` protocol on UDP 5009 for
    /// `poc-viewer`, test hooks on UDP 5010.
    Stream,
}

/// The service this build runs.
const SERVICE: Service = Service::Stream;

/// PSRAM bus timing: esp-hal's default 250 MHz (MPLL 500 MHz / 2), as in `poc-p4-eth`.
const PSRAM_TIMING: PsramTimingParams = PsramTimingParams::MHZ_250;
/// Internal RAM for the app (L2MEM after the 128 KiB L2 cache, esp-hal's `RAM` region).
const RAM_START: usize = 0x4FF2_0000;

/// Keeps the PSRAM driver (and its peripheral) alive for the whole run.
static PSRAM_DRIVER: StaticCell<Psram> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let baud_divider = LOG_BAUD.map(set_log_baud);

    let timg0 = TimerGroup::new(p.TIMG0);
    #[cfg(not(feature = "idle-counter"))]
    esp_rtos::start(timg0.timer0, p.FROM_CPU_INTR0);
    #[cfg(feature = "idle-counter")]
    esp_rtos::start_with_idle_hook(timg0.timer0, p.FROM_CPU_INTR0, cpu::idle_hook);
    irq::install(&[irq::Source::Tick]);
    cpu::name_current("main");

    if let Some((old, new)) = baud_divider {
        info!(
            "log UART: 115200 -> {} baud (clkdiv x16 {old} -> {new})",
            LOG_BAUD.unwrap_or(115_200)
        );
    }
    log_chip();
    let psram = psram_init(p.PSRAM);

    let pins = net::Pins {
        eth: p.ETH,
        ref_clk: p.GPIO50,
        rxd0: p.GPIO29,
        rxd1: p.GPIO30,
        crs_dv: p.GPIO28,
        txd0: p.GPIO34,
        txd1: p.GPIO35,
        tx_en: p.GPIO49,
        mdc: p.GPIO31,
        mdio: p.GPIO52,
    };
    let stack = net::start(spawner, pins, p.GPIO51).await;
    match (SERVICE, stack, psram) {
        (Service::Bench, Some(stack), Some(psram)) => bench::start(
            spawner,
            stack,
            bench::Resources {
                psram,
                i2c0: p.I2C0,
                vdma: p.VDMA_CH0,
            },
        ),
        (Service::Stream, Some(stack), Some(psram)) => stream::start(
            spawner,
            stack,
            stream::Resources {
                psram,
                i2c0: p.I2C0,
                vdma: p.VDMA_CH0,
            },
        ),
        (Service::Bench | Service::Stream, _, _) => {
            error!("service: needs the network and PSRAM, not started")
        }
        (Service::Network, _, _) => {}
    }
    log_ram();
    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}

fn log_chip() {
    let rev = esp_hal::efuse::chip_revision();
    info!(
        "chip: ESP32-P4 rev v{}.{}, CPU {} MHz",
        rev.major,
        rev.minor,
        esp_hal::clock::cpu_clock().as_mhz(),
    );
}

/// Brings up PSRAM. `None` (logged) if it is not mapped.
fn psram_init(psram: esp_hal::peripherals::PSRAM<'static>) -> Option<&'static mut [u8]> {
    let config = PsramConfig {
        timing: PSRAM_TIMING,
        ..PsramConfig::default()
    };
    let psram = PSRAM_DRIVER.init(Psram::new(psram, config));
    let (ptr, len) = psram.raw_parts();
    info!(
        "PSRAM: {} MiB mapped at {:#010x}, bus {} MHz",
        len / (1024 * 1024),
        ptr as usize,
        config.timing.clock,
    );
    if len == 0 {
        error!("PSRAM: not mapped");
        return None;
    }
    // SAFETY: esp-hal mapped `len` bytes at `ptr`, and nothing else uses PSRAM.
    Some(unsafe { core::slice::from_raw_parts_mut(ptr, len) })
}

/// Internal RAM: static data (`.data`, `.bss`, RAM code) and what is left for the main stack
/// (esp-hal gives it all of the rest of the `RAM` region).
fn log_ram() {
    unsafe extern "C" {
        static _stack_end_cpu0: u8;
        static _stack_start_cpu0: u8;
    }
    let end = &raw const _stack_end_cpu0 as usize;
    let top = &raw const _stack_start_cpu0 as usize;
    info!(
        "RAM: {} B static ({:#010x}..{end:#010x}), {} B left for the main stack (..{top:#010x}); network DMA {} B, stack resources {} B, service buffers {} B",
        end - RAM_START,
        RAM_START,
        top - end,
        core::mem::size_of::<net::DmaStorage>(),
        core::mem::size_of::<embassy_net::StackResources<{ net::SOCKETS }>>(),
        match SERVICE {
            Service::Network => 0,
            Service::Bench => bench::RAM_BYTES,
            Service::Stream => stream::RAM_BYTES,
        },
    );
}

/// Switches UART0 (the log console) from the bootloader's 115200 baud to `baud`, keeping the
/// UART's clock source: the new divider is the current one scaled by 115200 / `baud`.
/// Returns the old and new divider in 1/16 steps (`CLKDIV * 16 + CLKDIV_FRAG`).
fn set_log_baud(baud: u32) -> (u32, u32) {
    // SAFETY: esp-println writes UART0's FIFO; only the divider changes here, after the FIFO and
    // the transmitter are idle, inside a critical section so no log line starts meanwhile.
    let uart = unsafe { &*esp32p4::UART0::ptr() };
    critical_section::with(|_| {
        while uart.status().read().txfifo_cnt().bits() != 0 {}
        while uart.fsm_status().read().st_utx_out().bits() != 0 {}
        let r = uart.clkdiv().read();
        let old = r.clkdiv().bits() as u32 * 16 + r.clkdiv_frag().bits() as u32;
        let new = (old as u64 * 115_200 / baud as u64) as u32;
        uart.clkdiv().write(|w| unsafe {
            w.clkdiv().bits((new / 16) as u16);
            w.clkdiv_frag().bits((new % 16) as u8)
        });
        uart.reg_update().write(|w| w.reg_update().set_bit());
        while uart.reg_update().read().reg_update().bit_is_set() {}
        (old, new)
    })
}
