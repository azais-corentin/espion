//! Throwaway proof of concept on the Waveshare ESP32-P4-ETH dev board.
//!
//! On boot it switches the log UART to [`LOG_BAUD`], prints the chip info, brings up PSRAM and
//! checks it, measures one core's sequential read speed from PSRAM (cold) and from internal
//! RAM, scans the camera's I2C bus and reads the OV9281 chip ID. Then, as the constants below
//! select, it benchmarks the blob detector on synthetic frames (`bench`) and runs the camera
//! section (`camera`): every mode of [`CAMERA_PLAN`] over MIPI CSI-2, frame rate and error
//! counters, cache checks and an optional still dump.
//!
//! Board facts (Waveshare `ESP32-P4-ETH-datasheet.pdf`, the schematic, sheet 1):
//! - UART0 to the CH343P USB-UART bridge (U4): P4 GPIO37 = U0TXD -> CH343 RXD, CH343 TXD ->
//!   GPIO38 = U0RXD. CH343 RTS/DTR drive EN and GPIO35 (BOOT) through U5 for auto-reset.
//! - CSI connector J1 (22 pin, Pi 5 pinout): pin 2 ESP_I2C_SDA, pin 3 ESP_I2C_SCL (2.2 k
//!   pull-ups R46/R44 to 3.3 V; the bus is shared with the ES8311 codec), pin 6 CSI_IO0
//!   (10 k R42 to 3.3 V, no GPIO), pin 5 CSI_IO1 (pull-up R43 not fitted, no GPIO).
//! - MIPI D-PHY supply VDD_MIPI_DPHY (U6 pin 41) = ESP_LDO_VO3, the P4's internal LDO channel 3.

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use core::hint::black_box;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::AnyPin;
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::psram::{Psram, PsramConfig, PsramTimingParams};
use esp_hal::time::{Instant, Rate};
use esp_hal::timer::timg::TimerGroup;
use log::{error, info, warn};
use static_cell::{ConstStaticCell, StaticCell};

esp_bootloader_esp_idf::esp_app_desc!();

mod bench;
mod camera;
#[allow(
    dead_code,
    reason = "complete OV9281 register table; the camera section uses part of it"
)]
mod ov9281;
mod synth;
mod vision;

/// UART0 log baud rate the app switches to right at boot (the ROM and the bootloader print
/// at 115200). Capture with `espflash monitor --monitor-baud <LOG_BAUD>`. `None` keeps 115200.
const LOG_BAUD: Option<u32> = Some(2_000_000);
/// Run the detector benchmark on synthetic frames at boot (about 90 s).
const RUN_BENCH: bool = false;
/// Run the camera section after the boot report (and the benchmark, if on).
const RUN_CAMERA: bool = true;
/// Camera section: modes (indexes into `ov9281::MODES`: 0 1280x800, 1 1280x720, 2 640x480,
/// 3 640x400), sensor settings, checks and still dump (decode with `tools/still.py`).
const CAMERA_PLAN: camera::Plan = camera::Plan {
    modes: &[0, 1, 2, 3],
    test_pattern: false,
    clock_gated: false,
    // Indoor scene without the IR ring: 800 of 890 possible lines (7.3 ms) at 2x gain.
    exposure_lines: 800,
    gain: 0x20,
    measure: Duration::from_secs(5),
    cache_check: true,
    still_dump: false,
};

/// SCCB data: net ESP_I2C_SDA, U6 pin 7 (GPIO7) to CSI connector J1 pin 2.
const SCCB_SDA_GPIO: u8 = 7;
/// SCCB clock: net ESP_I2C_SCL, U6 pin 8 (GPIO8) to CSI connector J1 pin 3.
const SCCB_SCL_GPIO: u8 = 8;
/// SCCB clock rate.
const SCCB_FREQ_KHZ: u32 = 100;
/// Camera enable (J1 pin 6, CSI_IO0, the module's PWDN pin on the 15-pin side): not wired to
/// a GPIO on this board. R42 (10 k) holds it at 3.3 V.
const CAM_EN_GPIO: Option<u8> = None;
/// OV9281 7-bit SCCB addresses: datasheet write addresses 0xC0 and 0x20. The module on the
/// bench answers at 0x60 (and also at 0x70 with the same chip ID).
const OV9281_ADDRS: [u8; 2] = [0x60, 0x10];
/// OV9281 chip ID registers (0x300A high byte, 0x300B low byte) and the expected value.
const OV9281_CHIP_ID_REG: u16 = 0x300A;
const OV9281_CHIP_ID: u16 = 0x9281;

/// PSRAM bus timing: esp-hal's default 250 MHz (MPLL 500 MHz / 2). `MHZ_200` fails at init
/// with esp-hal 1.2.2 (500 MHz is not a multiple of 200; PSRAM stays unmapped).
const PSRAM_TIMING: PsramTimingParams = PsramTimingParams::MHZ_250;

/// Size of each read-speed buffer in PSRAM.
const PSRAM_BENCH_LEN: usize = 1024 * 1024;
/// Size of the read-speed buffer in internal RAM.
const SRAM_BENCH_LEN: usize = 128 * 1024;
/// Size of the pattern test at the far end of the PSRAM mapping.
const PSRAM_TEST_LEN: usize = 4 * 1024 * 1024;
/// Timed passes per buffer.
const BENCH_PASSES: u32 = 4;

/// ROM cache maps (`cache_map_t` in ESP-IDF `rom/cache.h`).
const CACHE_MAP_L1_DCACHE: u32 = 1 << 4;
const CACHE_MAP_L2_CACHE: u32 = 1 << 5;
/// Internal L2MEM (cached by L1 only); everything else here is PSRAM (L1 and L2).
const INTERNAL_RAM: core::ops::Range<usize> = 0x4FF0_0000..0x4FFC_0000;

unsafe extern "C" {
    /// ROM: write back then invalidate the cache lines covering `addr..addr + size`.
    fn Cache_WriteBack_Invalidate_Addr(map: u32, addr: u32, size: u32) -> i32;
}

static SRAM_BENCH: ConstStaticCell<[u8; SRAM_BENCH_LEN]> =
    ConstStaticCell::new([0; SRAM_BENCH_LEN]);
/// Keeps the PSRAM driver (and its peripheral) alive for the whole run.
static PSRAM_DRIVER: StaticCell<Psram> = StaticCell::new();

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let baud_divider = LOG_BAUD.map(set_log_baud);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    if let Some((old, new)) = baud_divider {
        info!(
            "log UART: 115200 -> {} baud (clkdiv x16 {old} -> {new})",
            LOG_BAUD.unwrap_or(115_200)
        );
    }
    log_chip();
    let psram_mem = memory_report(peripherals.PSRAM);
    let mut i2c = sccb_report(peripherals.I2C0).await;

    info!("boot report done");
    match psram_mem {
        Some(mem) => {
            if RUN_BENCH {
                bench::run(mem);
            }
            if RUN_CAMERA {
                camera::run(&mut i2c, peripherals.VDMA_CH0, mem, &CAMERA_PLAN).await;
            }
        }
        None => error!("detector benchmark and camera need PSRAM, skipped"),
    }
    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}

fn log_chip() {
    let rev = esp_hal::efuse::chip_revision();
    let mac = esp_hal::efuse::base_mac_address();
    info!(
        "chip: ESP32-P4 rev v{}.{}, CPU {} MHz, MAC {:02x?}",
        rev.major,
        rev.minor,
        esp_hal::clock::cpu_clock().as_mhz(),
        mac.as_bytes()
    );
    log_cache_config();
}

/// Brings up PSRAM, checks it and measures the read speeds. Returns the mapped PSRAM, if
/// it is large enough for the tests.
fn memory_report(psram: esp_hal::peripherals::PSRAM<'static>) -> Option<&'static mut [u8]> {
    let psram_config = PsramConfig {
        timing: PSRAM_TIMING,
        ..PsramConfig::default()
    };
    let psram = PSRAM_DRIVER.init(Psram::new(psram, psram_config));
    let (psram_ptr, psram_len) = psram.raw_parts();
    info!(
        "PSRAM: {} MiB mapped at {:#010x}, bus {} MHz (source {:?})",
        psram_len / (1024 * 1024),
        psram_ptr as usize,
        psram_config.timing.clock,
        psram_config.timing.clock_source
    );
    let psram_mem = if psram_len >= PSRAM_TEST_LEN {
        // SAFETY: esp-hal mapped `psram_len` bytes at `psram_ptr`, and nothing else uses PSRAM.
        let mem: &'static mut [u8] =
            unsafe { core::slice::from_raw_parts_mut(psram_ptr, psram_len) };
        psram_pattern_test(mem);
        bench("PSRAM cold", &mut mem[..PSRAM_BENCH_LEN], true);
        Some(mem)
    } else {
        error!("PSRAM: not mapped or too small, skipping its tests");
        None
    };
    bench("internal RAM", SRAM_BENCH.take(), false);
    psram_mem
}

/// Scans the camera's I2C bus and reads the OV9281 chip ID. Returns the bus for the camera.
async fn sccb_report(i2c0: esp_hal::peripherals::I2C0<'static>) -> I2c<'static, esp_hal::Async> {
    let mut i2c = I2c::new(
        i2c0,
        I2cConfig::default().with_frequency(Rate::from_khz(SCCB_FREQ_KHZ)),
    )
    .expect("I2C0 config")
    // SAFETY: GPIO7/GPIO8 are used only by this bus.
    .with_sda(unsafe { AnyPin::steal(SCCB_SDA_GPIO) })
    .with_scl(unsafe { AnyPin::steal(SCCB_SCL_GPIO) })
    .into_async();
    info!(
        "SCCB: I2C0 SDA GPIO{SCCB_SDA_GPIO} SCL GPIO{SCCB_SCL_GPIO} {SCCB_FREQ_KHZ} kHz, camera enable {}",
        match CAM_EN_GPIO {
            Some(_) => "on a GPIO",
            None => "hard-wired high (R42)",
        }
    );
    i2c_scan(&mut i2c).await;
    read_ov9281_id(&mut i2c).await;
    i2c
}

/// Prints the L1 data cache and L2 cache size and line size the bootloader configured.
fn log_cache_config() {
    // SAFETY: read-only access to cache configuration registers.
    let cache = unsafe { &*esp32p4::CACHE::ptr() };
    let one_hot = |bits: u32, unit: u32| unit << bits.trailing_zeros();
    info!(
        "cache: L1 D {} KiB, {} B lines; L2 {} KiB, {} B lines",
        one_hot(cache.l1_dcache_cachesize_conf().read().bits(), 256) / 1024,
        one_hot(cache.l1_dcache_blocksize_conf().read().bits(), 8),
        one_hot(cache.l2_cache_cachesize_conf().read().bits(), 256) / 1024,
        one_hot(cache.l2_cache_blocksize_conf().read().bits(), 8),
    );
}

/// Writes back and drops every cached line of `buf`, so the next read comes from memory
/// (as after a DMA write).
fn flush_cache(buf: &[u8]) {
    let addr = buf.as_ptr() as usize;
    let map = if INTERNAL_RAM.contains(&addr) {
        CACHE_MAP_L1_DCACHE
    } else {
        CACHE_MAP_L1_DCACHE | CACHE_MAP_L2_CACHE
    };
    // SAFETY: ROM call on a range we own; dirty lines are written back before invalidation.
    unsafe {
        Cache_WriteBack_Invalidate_Addr(map, addr as u32, buf.len() as u32);
    }
}

fn pattern(addr: usize, seed: u32) -> u32 {
    (addr as u32).wrapping_mul(0x9E37_79B9) ^ seed
}

/// Writes an address-dependent pattern and its complement over the last
/// `PSRAM_TEST_LEN` bytes of the mapping, reads them back from memory, then checks the
/// mapping does not alias (start, middle and end hold different words).
fn psram_pattern_test(mem: &mut [u8]) {
    let len = mem.len();
    let (_, words, _) = unsafe { mem[len - PSRAM_TEST_LEN..].align_to_mut::<u32>() };
    let start = words.as_ptr() as usize;
    let t0 = Instant::now();
    let first = pattern_pass(words, 0xA5A5_5A5A, 0);
    let errors = first + pattern_pass(words, !0xA5A5_5A5A, first);
    let us = t0.elapsed().as_micros();
    let aliased = psram_aliases(mem);

    if errors == 0 && !aliased {
        info!(
            "PSRAM: pattern test OK over the last {} MiB ({:#010x}..{:#010x}), 2 passes, {} ms; no aliasing",
            PSRAM_TEST_LEN / (1024 * 1024),
            start,
            start + PSRAM_TEST_LEN,
            us / 1000
        );
    } else {
        error!("PSRAM: pattern test FAILED: {errors} bad words, aliasing {aliased}");
    }
}

/// Writes `pattern(addr, seed)` to every word, writes back and invalidates the cache, then
/// reads the words back from memory. Returns the number of bad words; logs the first ones
/// while fewer than four errors were seen before (`errors_before`).
fn pattern_pass(words: &mut [u32], seed: u32, errors_before: u32) -> u32 {
    let start = words.as_ptr() as usize;
    for (i, w) in words.iter_mut().enumerate() {
        *w = pattern(start + 4 * i, seed);
    }
    flush_cache(bytes_of(words));
    let mut errors = 0u32;
    for (i, w) in words.iter().enumerate() {
        let got = unsafe { core::ptr::read_volatile(w) };
        if got != pattern(start + 4 * i, seed) {
            if errors_before + errors < 4 {
                error!("PSRAM: {:#010x}: got {got:#010x}", start + 4 * i);
            }
            errors += 1;
        }
    }
    errors
}

/// Tags the first, middle and last word of the mapping and checks they read back distinct
/// (a wrongly detected size would alias the far end onto the start).
fn psram_aliases(mem: &mut [u8]) -> bool {
    let (_, all, _) = unsafe { mem.align_to_mut::<u32>() };
    let n = all.len();
    let probes = [0, n / 2, n - 1];
    for (k, &i) in probes.iter().enumerate() {
        all[i] = 0x5000_0000 + k as u32;
    }
    flush_cache(bytes_of(all));
    probes
        .iter()
        .enumerate()
        .any(|(k, &i)| unsafe { core::ptr::read_volatile(&all[i]) } != 0x5000_0000 + k as u32)
}

fn bytes_of(words: &[u32]) -> &[u8] {
    // SAFETY: any u32 slice is a valid byte slice of four times the length.
    unsafe { core::slice::from_raw_parts(words.as_ptr().cast(), words.len() * 4) }
}

/// Sums every byte: the per-pixel read cost of one pass over a RAW8 frame.
#[inline(never)]
fn byte_sum(buf: &[u8]) -> u32 {
    buf.iter()
        .fold(0u32, |acc, &b| acc.wrapping_add(u32::from(b)))
}

/// Sums 32-bit words, one 64-byte cache line (16 independent loads) per iteration: the same
/// bytes with a quarter of the loads and no load-use stall per load.
#[inline(never)]
fn word_sum(buf: &[u8]) -> u32 {
    let (_, words, _) = unsafe { buf.align_to::<u32>() };
    let (lines, _) = words.as_chunks::<16>();
    lines.iter().fold(0u32, |acc, line| {
        acc.wrapping_add(line.iter().fold(0u32, |a, &w| a.wrapping_add(w)))
    })
}

/// Loads one word per 64-byte cache line: the cache-fill ceiling, nearly no CPU work.
#[inline(never)]
fn line_touch(buf: &[u8]) -> u32 {
    let (_, words, _) = unsafe { buf.align_to::<u32>() };
    words
        .iter()
        .step_by(16)
        .fold(0u32, |acc, &w| acc.wrapping_add(w))
}

/// A read kernel: sums its input so the loads cannot be optimised away.
type Kernel = fn(&[u8]) -> u32;

const KERNELS: [(&str, Kernel); 3] = [
    ("byte-sum", byte_sum),
    ("word-sum", word_sum),
    ("line-touch", line_touch),
];

fn mcycle() -> u32 {
    let c: u32;
    // SAFETY: reads the machine cycle counter CSR; no side effects.
    unsafe { core::arch::asm!("csrr {0}, mcycle", out(reg) c) };
    c
}

/// Times `BENCH_PASSES` passes of each kernel over `buf` and logs MB/s (10^6 bytes per
/// second), microseconds and CPU cycles per pass. `cold` writes back and invalidates the
/// buffer's cache lines before each pass.
fn bench(label: &str, buf: &mut [u8], cold: bool) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (i as u8) ^ (i >> 8) as u8;
    }
    for (name, f) in KERNELS {
        let mut total_us = 0u64;
        let mut total_cycles = 0u64;
        let mut sum = 0u32;
        for _ in 0..BENCH_PASSES {
            if cold {
                flush_cache(buf);
            }
            let t0 = Instant::now();
            let c0 = mcycle();
            sum = sum.wrapping_add(black_box(f(black_box(buf))));
            total_cycles += u64::from(mcycle().wrapping_sub(c0));
            total_us += t0.elapsed().as_micros();
        }
        black_box(sum);
        let us = (total_us / u64::from(BENCH_PASSES)).max(1);
        let cycles = total_cycles / u64::from(BENCH_PASSES);
        info!(
            "read {label}, {} KiB at {:#010x}: {name} {} MB/s ({us} us, {cycles} cycles per pass)",
            buf.len() / 1024,
            buf.as_ptr() as usize,
            buf.len() as u64 / us,
        );
    }
}

async fn i2c_scan(i2c: &mut I2c<'_, esp_hal::Async>) {
    let mut found = [0u8; 16];
    let mut n = 0;
    for addr in 0x08..=0x77u8 {
        if i2c.write_async(addr, &[]).await.is_ok() && n < found.len() {
            found[n] = addr;
            n += 1;
        }
    }
    info!("SCCB scan: {} device(s) at {:02x?}", n, &found[..n]);
}

async fn read_ov9281_id(i2c: &mut I2c<'_, esp_hal::Async>) {
    for addr in OV9281_ADDRS {
        let mut id = [0u8; 2];
        match i2c
            .write_read_async(addr, &OV9281_CHIP_ID_REG.to_be_bytes(), &mut id)
            .await
        {
            Ok(()) => {
                let id = u16::from_be_bytes(id);
                if id == OV9281_CHIP_ID {
                    info!("OV9281: chip ID {id:#06x} at {addr:#04x}");
                } else {
                    warn!("OV9281: {addr:#04x} answered with chip ID {id:#06x}");
                }
                return;
            }
            Err(e) => warn!("OV9281: no answer at {addr:#04x}: {e:?}"),
        }
    }
    error!("OV9281: not found");
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
