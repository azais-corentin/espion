//! Onboard benchmark of the blob detector (`espion_vision`) on synthetic frames (`synth`), no
//! camera.
//!
//! For each mode and blob count, three paths over the same frame content:
//! - `psram`: the whole frame in PSRAM through `process_frame`, its cache lines written back
//!   and invalidated before every pass (cold, as after a CSI DMA write);
//! - `strip`: 64-line strips copied from the PSRAM frame into one internal strip buffer and
//!   flushed from the cache (untimed, standing in for DMA into internal RAM); only
//!   `begin_frame`, the `push_rows` calls and `finish` are timed and summed per frame;
//! - `sram`: the whole frame in internal RAM (only the modes that fit), flushed before every
//!   pass.
//!
//! No CSI DMA runs at the same time, so the PSRAM bus is idle apart from the CPU: these are
//! lower bounds for the live case.

use espion_vision::{Blob, DefaultDetector, synth};
use log::info;
use static_cell::ConstStaticCell;

use crate::{flush_cache, mcycle};

/// Foreground threshold.
const THRESHOLD: u8 = 128;
/// Timed passes per case.
const PASSES: u32 = 50;
/// Rows per strip in the `strip` path.
const STRIP_ROWS: usize = 64;
/// Frame period at 120 fps.
const FRAME_PERIOD_US: u32 = 8333;
/// Synthetic frame seed.
const SEED: u32 = 0x1234_5678;

const MODES: [(u16, u16); 4] = [(1280, 800), (1280, 720), (640, 480), (640, 400)];
const BLOB_COUNTS: [u16; 3] = [0, 16, 64];

/// Largest frame that fits in internal RAM next to everything else (640x480, 300 KiB). The
/// `strip` path reuses its first `STRIP_ROWS` rows of 1280 pixels (80 KiB).
const INTERNAL_LEN: usize = 640 * 480;
const _: () = assert!(INTERNAL_LEN >= STRIP_ROWS * 1280);

static DETECTOR: ConstStaticCell<DefaultDetector> = ConstStaticCell::new(DefaultDetector::new());
static BLOBS: ConstStaticCell<[Blob; 256]> = ConstStaticCell::new(
    [Blob {
        x: 0,
        y: 0,
        area: 0,
        peak: 0,
    }; 256],
);
static INTERNAL: ConstStaticCell<[u8; INTERNAL_LEN]> = ConstStaticCell::new([0; INTERNAL_LEN]);
static SYNTH: ConstStaticCell<synth::Synth> = ConstStaticCell::new(synth::Synth::NEW);
static CASES: ConstStaticCell<[Option<Case>; MAX_CASES]> = ConstStaticCell::new([None; MAX_CASES]);

#[derive(Clone, Copy, PartialEq)]
enum Path {
    Psram,
    Strip,
    Sram,
}

impl Path {
    fn name(self) -> &'static str {
        match self {
            Path::Psram => "psram",
            Path::Strip => "strip",
            Path::Sram => "sram",
        }
    }
}

#[derive(Clone, Copy)]
struct Case {
    width: u16,
    height: u16,
    blobs: u16,
    path: Path,
    min_us: u32,
    mean_us: u32,
    max_us: u32,
    found: u32,
    overflow: u32,
}

const MAX_CASES: usize = MODES.len() * BLOB_COUNTS.len() * 3;

/// Per-pass cycle counts, reduced to min / mean / max microseconds.
struct Timing {
    mhz: u32,
    min: u32,
    max: u32,
    total: u64,
}

impl Timing {
    fn new(mhz: u32) -> Self {
        Self {
            mhz,
            min: u32::MAX,
            max: 0,
            total: 0,
        }
    }

    fn add(&mut self, cycles: u32) {
        self.min = self.min.min(cycles);
        self.max = self.max.max(cycles);
        self.total += u64::from(cycles);
    }

    fn us(&self) -> (u32, u32, u32) {
        let mean = (self.total / u64::from(PASSES)) as u32;
        (self.min / self.mhz, mean / self.mhz, self.max / self.mhz)
    }
}

/// The detector and its buffers, all in internal RAM.
struct Bench {
    det: &'static mut DefaultDetector,
    out: &'static mut [Blob; 256],
    internal: &'static mut [u8; INTERNAL_LEN],
    mhz: u32,
}

/// Runs every case over frames built at the start of `psram` (at least 1 MB) and logs one
/// line per case, then a table.
pub fn run(psram: &mut [u8]) {
    let mut bench = Bench {
        det: DETECTOR.take(),
        out: BLOBS.take(),
        internal: INTERNAL.take(),
        mhz: esp_hal::clock::cpu_clock().as_mhz(),
    };
    let synth = SYNTH.take();
    let cases = CASES.take();
    // Keep every blob of one pixel or more (the detector's default), so `found` counts all
    // synthetic discs.
    bench.det.set_area_filter(1, u32::MAX);

    info!(
        "detector bench: threshold {THRESHOLD}, {PASSES} passes, {STRIP_ROWS}-line strips, \
         detector {} B and strip/frame buffer {} KiB in internal RAM at {:#010x}",
        core::mem::size_of::<DefaultDetector>(),
        INTERNAL_LEN / 1024,
        bench.internal.as_ptr() as usize
    );
    let mut slots = cases.iter_mut();
    for (width, height) in MODES {
        let len = usize::from(width) * usize::from(height);
        let frame = &mut psram[..len];
        for blobs in BLOB_COUNTS {
            synth.fill(frame, width.into(), height.into(), blobs, SEED);
            let paths: &[Path] = if len <= INTERNAL_LEN {
                &[Path::Psram, Path::Strip, Path::Sram]
            } else {
                &[Path::Psram, Path::Strip]
            };
            for &path in paths {
                let case = bench.measure(frame, width, height, blobs, path);
                log_case(&case);
                if let Some(slot) = slots.next() {
                    *slot = Some(case);
                }
            }
        }
    }
    log_table(cases);
}

impl Bench {
    /// Times `PASSES` passes of one path over `frame` (in PSRAM).
    fn measure(&mut self, frame: &[u8], width: u16, height: u16, blobs: u16, path: Path) -> Case {
        let len = frame.len();
        let mut t = Timing::new(self.mhz);
        let mut found = 0;
        let mut overflow = 0;
        if path == Path::Sram {
            self.internal[..len].copy_from_slice(frame);
        }
        for _ in 0..PASSES {
            let cycles = match path {
                Path::Psram => {
                    flush_cache(frame);
                    let c0 = mcycle();
                    found = self.det.process_frame(frame, width, THRESHOLD, self.out);
                    mcycle().wrapping_sub(c0)
                }
                Path::Sram => {
                    let src = &self.internal[..len];
                    flush_cache(src);
                    let c0 = mcycle();
                    found = self.det.process_frame(src, width, THRESHOLD, self.out);
                    mcycle().wrapping_sub(c0)
                }
                Path::Strip => {
                    strip_pass(self.det, self.out, frame, width, self.internal, &mut found)
                }
            };
            t.add(cycles);
            overflow = overflow.max(self.det.stats().overflow);
        }
        let (min_us, mean_us, max_us) = t.us();
        Case {
            width,
            height,
            blobs,
            path,
            min_us,
            mean_us,
            max_us,
            found: found as u32,
            overflow,
        }
    }
}

fn log_table(cases: &[Option<Case>]) {
    info!("detector bench table (us, frame period {FRAME_PERIOD_US} us at 120 fps):");
    info!("| mode | blobs | path | min | mean | max | found | overflow | 120 fps |");
    for c in cases.iter().flatten() {
        info!(
            "| {}x{} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c.width,
            c.height,
            c.blobs,
            c.path.name(),
            c.min_us,
            c.mean_us,
            c.max_us,
            c.found,
            c.overflow,
            verdict(c.max_us)
        );
    }
    info!("detector bench done");
}

/// One frame through the strip path; returns the summed cycles of the detector calls only.
fn strip_pass(
    det: &mut DefaultDetector,
    out: &mut [Blob],
    frame: &[u8],
    width: u16,
    strip_buf: &mut [u8],
    found: &mut usize,
) -> u32 {
    let strip_len = STRIP_ROWS * usize::from(width);
    let c0 = mcycle();
    det.begin_frame(width, THRESHOLD);
    let mut cycles = mcycle().wrapping_sub(c0);
    for (i, src) in frame.chunks(strip_len).enumerate() {
        let strip = &mut strip_buf[..src.len()];
        strip.copy_from_slice(src);
        flush_cache(strip);
        let c0 = mcycle();
        det.push_rows(strip, (i * STRIP_ROWS) as u16);
        cycles = cycles.wrapping_add(mcycle().wrapping_sub(c0));
    }
    let c0 = mcycle();
    *found = det.finish(out);
    cycles.wrapping_add(mcycle().wrapping_sub(c0))
}

fn verdict(max_us: u32) -> &'static str {
    if max_us < FRAME_PERIOD_US {
        "OK"
    } else {
        "OVER"
    }
}

fn log_case(c: &Case) {
    info!(
        "bench {}x{} blobs {:>2} {:<5}: min {} mean {} max {} us, found {}, overflow {}, {} ({} us budget)",
        c.width,
        c.height,
        c.blobs,
        c.path.name(),
        c.min_us,
        c.mean_us,
        c.max_us,
        c.found,
        c.overflow,
        verdict(c.max_us),
        FRAME_PERIOD_US
    );
}
