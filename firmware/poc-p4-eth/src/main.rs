//! Throwaway proof of concept on the Waveshare ESP32-P4-ETH dev board.
//!
//! On boot it switches the log UART to [`LOG_BAUD`], prints the chip info, brings up PSRAM and
//! checks it, measures one core's sequential read speed from PSRAM (cold) and from internal
//! RAM, scans the camera's I2C bus and identifies the OV9281 (`ov9281` crate). Then, as the
//! constants below select, it benchmarks the blob detector on synthetic frames (`bench`) and
//! runs the camera section (`camera`): every mode of [`CAMERA_PLAN`] over MIPI CSI-2, frame
//! rate and error counters, cache checks and an optional still dump.
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
use espion_core1::SkipChoice;
use log::{error, info, warn};
use ov9281::{Address, CHIP_ID, Gain, Ov9281, TestPattern};
use static_cell::{ConstStaticCell, StaticCell};

esp_bootloader_esp_idf::esp_app_desc!();

/// Runs `$body` with `$s` bound to the skip of a [`DarkSkip`] as its concrete type, so the
/// detector calls in `$body` are monomorphised per skip: one branch per use, none per skip
/// call inside the detector.
macro_rules! with_skip {
    ($skip:expr, $s:ident => $body:expr) => {
        match $skip {
            $crate::DarkSkip::Scalar => {
                let $s = &espion_vision::Scalar;
                $body
            }
            $crate::DarkSkip::Pie(pie) => {
                let $s = pie;
                $body
            }
        }
    };
}

mod bench;
mod camera;
mod core1_soak;
mod jpeg_bench;
mod modes;
mod requests;
mod soak;
mod strip_soak;

/// UART0 log baud rate the app switches to right at boot (the ROM and the bootloader print
/// at 115200). Capture with `espflash monitor --monitor-baud <LOG_BAUD>`. `None` keeps 115200.
const LOG_BAUD: Option<u32> = Some(2_000_000);
/// Run the detector benchmark on synthetic frames at boot (about 15 s).
const RUN_BENCH: bool = false;
/// Run the camera section after the boot report (and the benchmark, if on).
const RUN_CAMERA: bool = true;
/// The dark skip of the frame soak and the strip soak: [`SkipChoice::Pie`] (PIE on core 0,
/// `Scalar` if `Pie::enable` refuses) or [`SkipChoice::Scalar`]. The benchmark runs both.
const SOAK_SKIP: SkipChoice = SkipChoice::Pie;
/// The dark skip a detector call runs with (`espion_vision::Skip`), passed down to the
/// benchmark and the soaks. The PIE token is created once, in [`app`]; see the rule there.
#[derive(Clone, Copy)]
enum DarkSkip<'a> {
    /// `espion_vision::Scalar`: SWAR tests on 32-bit words.
    Scalar,
    /// `esp32p4_pie::Pie`: 16-byte vector loads.
    Pie(&'a esp32p4_pie::Pie),
}

impl DarkSkip<'_> {
    fn name(self) -> &'static str {
        match self {
            DarkSkip::Scalar => "scalar",
            DarkSkip::Pie(_) => "pie",
        }
    }
}
/// Instead of [`PLAN`]: strip runs on the modes of [`CAMERA_PLAN`] with the blob detector on
/// the second core, started bare (crate `espion-core1`, `core1_soak`; plan
/// `core1_soak::PLAN`). Board results: README, "Detector on core 1".
const RUN_CORE1: bool = false;
/// What the camera section runs after capture starts in each mode.
#[allow(
    dead_code,
    reason = "the variants not selected by `SOAK` are the documented alternatives"
)]
enum Soak {
    /// Capture only: frame rate and error counters (plus `cache_check`/`still_dump`).
    None,
    /// Frame soak: blob detection on whole frames in PSRAM ([`SOAK_PLAN`]).
    Frames,
    /// Strip soak: blob detection on 64-line strips in internal RAM ([`STRIP_SOAK_PLAN`]). The
    /// whole-frame receiver is then never created, and `measure`, `cache_check` and
    /// `still_dump` are ignored. Needs `RUN_BENCH = false` (checked at compile time).
    Strips,
}
/// The soak [`CAMERA_PLAN`] runs.
const SOAK: Soak = Soak::Frames;
/// The plan the camera section runs: [`CAMERA_PLAN`] (default), [`FAST_CAPTURE_PLAN`],
/// [`MODE_STILLS_PLAN`], [`RAW10_PATTERNS_PLAN`], [`REQUESTS_PLAN`], [`RAW10_STRIP_SOAK_PLAN`],
/// [`STRIP_CHECK_PLAN`], [`STRIP_STALL_PLAN`] or [`JPEG_PLAN`].
const PLAN: &camera::Plan = &CAMERA_PLAN;
/// Camera section on the 120 fps RAW8 modes (indexes into `modes::MODES`: 0 1280x800,
/// 1 1280x720, 2 640x480, 3 640x400): sensor settings, checks, still dump (decode with
/// `tools/still.py`) and the soak selected by [`SOAK`]. The frame soak runs on RAW8 modes only.
const CAMERA_PLAN: camera::Plan = camera::Plan {
    modes: &[0, 1, 2, 3],
    requests: &[],
    sensor: camera::Sensor {
        pattern: TestPattern::Off,
        clock_gated: false,
        // Indoor scene without the IR ring: 800 of 889 possible lines (7.3 ms) at 2x gain.
        exposure_lines: 800,
        gain: Gain::from_sixteenths(0x20),
        psv_off: false,
    },
    measure: Duration::from_secs(5),
    cache_check: false,
    still_dump: false,
    still_patterns: &[],
    soak: match SOAK {
        Soak::Frames => Some(SOAK_PLAN),
        _ => None,
    },
    strips: match SOAK {
        Soak::Strips => Some(STRIP_SOAK_PLAN),
        _ => None,
    },
    jpeg: None,
};
/// Frame soak per mode: a live run, then synthetic frames with these disc counts, while
/// capture keeps running (about 60 s per mode).
const SOAK_PLAN: soak::Plan = soak::Plan {
    live: Duration::from_secs(30),
    synthetic: Duration::from_secs(10),
    blob_counts: &[0, 16, 64],
};
/// Strip soak per mode: a live run, then synthetic strips aiming at these blob counts per
/// frame, while capture keeps running (about 60 s per mode).
const STRIP_SOAK_PLAN: strip_soak::Plan = strip_soak::Plan {
    live: Duration::from_secs(30),
    synthetic: Duration::from_secs(10),
    blob_targets: &[0, 16, 64],
    check: None,
    stall: false,
};
/// Strip soak on the RAW10 modes 9..=11 (1280x800, 1280x720, 640x400 binned at 210 fps):
/// select with `PLAN`. Strips of 64 lines at 1280 wide and 128 at 640, 3 in the ring.
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const RAW10_STRIP_SOAK_PLAN: camera::Plan = camera::Plan {
    modes: &[9, 10, 11],
    soak: None,
    strips: Some(STRIP_SOAK_PLAN),
    ..CAMERA_PLAN
};
/// Strip check on mode 0 (1280x800 RAW8, as in the strip bring-up) and the RAW10 modes 9..=11
/// (at most 4 modes): whole frames of the sensor's random test pattern (same seed every frame)
/// as the reference, then 10 s of strips per mode checked row by row against it. Select with
/// `PLAN`.
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const STRIP_CHECK_PLAN: camera::Plan = camera::Plan {
    modes: &[0, 9, 10, 11],
    soak: None,
    strips: Some(strip_soak::Plan {
        check: Some(Duration::from_secs(10)),
        ..STRIP_SOAK_PLAN
    }),
    ..CAMERA_PLAN
};
/// Stall test on the modes of [`STRIP_CHECK_PLAN`]: the same references, then per mode and
/// case (`strip_soak::STALL_CASES`: single DMA halts of 1, 5, 30 and 100 ms, bursts of 20 x
/// 2 ms and 8 x 30 ms on a strip the test holds) the strips up to the first whole frame and
/// 1 s of strips after it, every row checked against the reference, and a memory guard around
/// the list items and after the ring. Select with `PLAN`.
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const STRIP_STALL_PLAN: camera::Plan = camera::Plan {
    strips: Some(strip_soak::Plan {
        check: Some(Duration::from_secs(1)),
        stall: true,
        ..STRIP_SOAK_PLAN
    }),
    ..STRIP_CHECK_PLAN
};
/// Capture only over the extended modes 4..=11 (4..=8 faster RAW8 up to 453 fps, 9..=11
/// RAW10): measured against the predicted frame rate, 5 s each. The exposure is clamped per
/// mode to its longest (below 800 lines in modes 6, 7 and 8).
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const FAST_CAPTURE_PLAN: camera::Plan = camera::Plan {
    modes: &[4, 5, 6, 7, 8, 9, 10, 11],
    still_dump: false,
    soak: None,
    strips: None,
    ..CAMERA_PLAN
};
/// All 12 modes with a scene still and a bar-pattern still per mode, capture only. Each RAW10
/// mode comes right after the RAW8 mode of the same window (0 before 9, 1 before 10, 6 before
/// 11), and 8 (the centre of the binned field of view) after 7, so the stills of a boot show
/// the same scene; 2 and 3 come last. Gain 2x, for the bright bench scene. About 5 minutes.
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const MODE_STILLS_PLAN: camera::Plan = camera::Plan {
    modes: &[0, 9, 1, 10, 6, 11, 4, 5, 7, 8, 2, 3],
    still_dump: true,
    still_patterns: &[TestPattern::Bars],
    ..FAST_CAPTURE_PLAN
};
/// The RAW10 byte layout against RAW8 on 640x400 at 210 fps (modes 6 and 11): a scene still,
/// then the random-data, grey-squares and rolling-bar test patterns.
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const RAW10_PATTERNS_PLAN: camera::Plan = camera::Plan {
    modes: &[6, 11],
    still_patterns: &[
        TestPattern::Random,
        TestPattern::GreySquares,
        TestPattern::RollingBars,
    ],
    ..MODE_STILLS_PLAN
};
/// Request verification (`requests`): every request of `requests::REQUESTS` built on the
/// P4 (timed); each accepted one captured for 3 s after a reference of the same window at a
/// long VTS, with a scene and a bar-pattern still. Gain 8x, as the characterisation's sweeps
/// (the dim bench scene at exposures clamped to a fast mode's frame).
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const REQUESTS_PLAN: camera::Plan = camera::Plan {
    modes: &[],
    requests: requests::REQUESTS,
    sensor: camera::Sensor {
        gain: Gain::from_sixteenths(0x80),
        ..CAMERA_PLAN.sensor
    },
    measure: Duration::from_secs(3),
    ..FAST_CAPTURE_PLAN
};
/// JPEG encoder test and benchmark (`jpeg_bench`, `esp32p4-jpeg`): correctness on a synthetic
/// picture and on camera frames (dumped for `tools/jpeg_check.py`), speed alone, speed while
/// the camera captures, robustness. About 8 min with the dumps. Select with `PLAN`.
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const JPEG_PLAN: camera::Plan = camera::Plan {
    modes: &[],
    soak: None,
    strips: None,
    jpeg: Some(jpeg_bench::Plan {
        synthetic: true,
        camera_stills: jpeg_bench::CAMERA_STILLS,
        speed_frames: 200,
        variants: true,
        baseline: Duration::from_secs(5),
        capture: jpeg_bench::CAPTURE_RUNS,
        robustness: true,
        soak: Duration::from_secs(60),
    }),
    ..CAMERA_PLAN
};
// The benchmark's 300 KiB frame buffer and the strip ring do not both fit in internal RAM.
const _: () = assert!(!(RUN_BENCH && (PLAN.strips.is_some() || RUN_CORE1)));

/// SCCB data: net ESP_I2C_SDA, U6 pin 7 (GPIO7) to CSI connector J1 pin 2.
const SCCB_SDA_GPIO: u8 = 7;
/// SCCB clock: net ESP_I2C_SCL, U6 pin 8 (GPIO8) to CSI connector J1 pin 3.
const SCCB_SCL_GPIO: u8 = 8;
/// SCCB clock rate.
const SCCB_FREQ_KHZ: u32 = 100;
/// Camera enable (J1 pin 6, CSI_IO0, the module's PWDN pin on the 15-pin side): not wired to
/// a GPIO on this board. R42 (10 k) holds it at 3.3 V.
const CAM_EN_GPIO: Option<u8> = None;
/// OV9281 SCCB addresses tried, in order: datasheet write addresses 0xC0 and 0x20 (7-bit 0x60
/// and 0x10). The module on the bench answers at 0x60 (and also at 0x70 with the same chip ID).
const OV9281_ADDRESSES: [Address; 2] = [Address::Primary, Address::Alternate];

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

/// Internal-RAM buffer of the boot read-speed test, then the strip soak's synthetic strip.
#[repr(C, align(64))]
struct SramBench([u8; SRAM_BENCH_LEN]);
static SRAM_BENCH: ConstStaticCell<SramBench> =
    ConstStaticCell::new(SramBench([0; SRAM_BENCH_LEN]));
/// Keeps the PSRAM driver (and its peripheral) alive for the whole run.
static PSRAM_DRIVER: StaticCell<Psram> = StaticCell::new();
/// The sensor and its bus, for the camera section.
static CAMERA: StaticCell<camera::Cam> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
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
    let (psram_mem, internal) = memory_report(peripherals.PSRAM);
    spawner.spawn(
        app(
            peripherals.I2C0,
            peripherals.VDMA_CH0,
            peripherals.CPU_CTRL,
            peripherals.TIMG1,
            psram_mem,
            internal,
        )
        .unwrap(),
    );
    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}

/// The rest of the boot: the SCCB report, then the benchmark and the camera section. A task
/// of its own: inside `main` the SCCB futures make `main`'s future, which its macro moves on
/// the stack, exceed the stack-frame limit (`clippy::large_stack_frames`).
#[embassy_executor::task]
async fn app(
    i2c0: esp_hal::peripherals::I2C0<'static>,
    vdma: esp_hal::peripherals::VDMA_CH0<'static>,
    cpu: esp_hal::peripherals::CPU_CTRL<'static>,
    timg1: esp_hal::peripherals::TIMG1<'static>,
    psram_mem: Option<&'static mut [u8]>,
    internal: &'static mut [u8],
) {
    let (i2c, address) = sccb_report(i2c0).await;
    info!("boot report done");
    let Some(mem) = psram_mem else {
        error!("detector benchmark and camera need PSRAM, skipped");
        return;
    };
    // PIE on this core (core 0) for the detector. Rule for the token: one PIE-using thread
    // per core: this task's executor thread, the only code of this firmware that uses PIE;
    // never in an interrupt handler; no vector state across an await (the kernels keep
    // none between calls). Held until `app` returns.
    let pie = esp32p4_pie::Pie::enable()
        .inspect_err(|e| error!("PIE: {e}; the detector uses the scalar skip"))
        .ok();
    if RUN_BENCH {
        run_bench(mem, pie.as_ref());
    }
    let skip = match (&pie, SOAK_SKIP) {
        (Some(p), SkipChoice::Pie) => DarkSkip::Pie(p),
        _ => DarkSkip::Scalar,
    };
    match address {
        Some(a) if RUN_CAMERA => {
            const STRIPS: bool = PLAN.strips.is_some();
            let cam = CAMERA.init(Ov9281::new(i2c, a));
            if RUN_CORE1 {
                core1_soak::run::<RUN_CORE1>(cam, vdma, cpu, timg1, mem, internal).await;
            } else {
                camera::run::<STRIPS>(cam, vdma, mem, internal, PLAN, skip).await;
            }
        }
        None if RUN_CAMERA => error!("camera section skipped: no OV9281"),
        _ => {}
    }
}

/// The bench build: the PIE kernel self-tests, then the detector benchmark (with both
/// skips if `pie` is there). Not inlined, so its locals stay out of the `app` future.
#[inline(never)]
fn run_bench(mem: &mut [u8], pie: Option<&esp32p4_pie::Pie>) {
    match pie {
        Some(p) => bench::pie_self_tests(p),
        None => error!("PIE: off; self-tests skipped"),
    }
    bench::run(mem, pie);
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
/// it is large enough for the tests, and the internal-RAM read-speed buffer (reused by the
/// strip soak for its synthetic strip).
fn memory_report(
    psram: esp_hal::peripherals::PSRAM<'static>,
) -> (Option<&'static mut [u8]>, &'static mut [u8]) {
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
    let internal = &mut SRAM_BENCH.take().0[..];
    bench("internal RAM", internal, false);
    (psram_mem, internal)
}

/// Scans the camera's I2C bus and identifies the OV9281. Returns the bus and the sensor's
/// address, `None` if no address answered with its chip ID.
async fn sccb_report(
    i2c0: esp_hal::peripherals::I2C0<'static>,
) -> (I2c<'static, esp_hal::Async>, Option<Address>) {
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
    let address = find_ov9281(&mut i2c).await;
    (i2c, address)
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
    esp32p4_cache::writeback_invalidate(buf.as_ptr() as usize, buf.len());
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

/// The first of [`OV9281_ADDRESSES`] whose chip ID is the OV9281's.
async fn find_ov9281(i2c: &mut I2c<'static, esp_hal::Async>) -> Option<Address> {
    for address in OV9281_ADDRESSES {
        match Ov9281::new(&mut *i2c, address).identify().await {
            Ok(()) => {
                log_found(address);
                return Some(address);
            }
            Err(e) => log_not_at(address, &e),
        }
    }
    error!("OV9281: not found");
    None
}

#[inline(never)]
fn log_found(address: Address) {
    info!("OV9281: chip ID {CHIP_ID:#06x} at {:#04x}", address.bits());
}

#[inline(never)]
fn log_not_at(address: Address, e: &camera::SensorError) {
    warn!("OV9281: not at {:#04x}: {e}", address.bits());
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
