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
//! Every timed window is also read with the core's counters (`esp32p4-perf`): instructions,
//! conditional branches, mispredictions, stores and the L1 cache miss events. Three sections
//! follow the cases:
//! - row cost: all-dark rows through `begin_frame` + `push_rows` at several widths, warm (a
//!   few KiB of rows pushed again and again as consecutive rows, so they stay in the L1
//!   cache) and cold (an 80 KiB buffer flushed before each pass), with a least-squares fit
//!   `cost = a per row + b per pixel`;
//! - busy rows: the same at 1280 wide with one bright pixel in the same column of every row
//!   (one run per row, linked to the row above) and of every 8th row (a one-pixel blob
//!   between dark rows), so the cost of rows near blobs stays visible once dark rows are
//!   skipped in whole spans;
//! - blob cost: the 1280x800 strip path with 64 discs of radius 2, 4, 8 and 12, drawn here
//!   over the `synth` background (`synth` draws radius 3..5 only), per blob from the
//!   difference to the same background without discs.
//!
//! Before the cases, `pie_self_tests` runs the self-tests of the PIE kernels
//! (`esp32p4-pie`) against their scalar references.
//!
//! No CSI DMA runs at the same time, so the PSRAM bus is idle apart from the CPU: these are
//! lower bounds for the live case. Nothing is logged inside a timed window.

use core::fmt;

use esp32p4_perf::{Delta, Sample};
use esp32p4_pie::Pie;
use espion_vision::{Blob, DefaultDetector, Skip, Stats, raw10, synth};
use log::{error, info};
use static_cell::ConstStaticCell;

use crate::{DarkSkip, flush_cache, mcycle};

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

/// Widths of the row-cost section.
const ROW_WIDTHS: [usize; 5] = [32, 64, 256, 640, 1280];
/// Warm rows: 7680 B (6 rows of 1280) pushed again and again as consecutive rows.
const WARM_BYTES: usize = 6 * 1280;
/// Rows per warm pass at every width (a multiple of `WARM_BYTES / width` for each).
const WARM_ROWS: usize = 1200;
/// Cold rows: one 80 KiB buffer (a 1280-wide strip), flushed before every pass.
const COLD_BYTES: usize = STRIP_ROWS * 1280;
/// Timed passes per row case, after one untimed pass.
const ROW_PASSES: u32 = 5;
/// Width of the busy-row section.
const BUSY_WIDTH: usize = 1280;
/// Column of the bright pixel in the one-column busy rows (not on a word boundary).
const BUSY_COLUMN: usize = BUSY_WIDTH / 2 + 3;
/// Busy-row cases: name, the row period of the bright pixels and how many per row. Period 1
/// makes runs in every row, each linked to the run above, so every row goes through the
/// row-wise path; period 8 makes one blob of one pixel every 8 rows, so each blob row is
/// entered from dark rows and left again. With `n > 1` columns the pixels sit at
/// `3 + i * width / n`: `n` one-pixel runs per row, the skip asked again after each.
const BUSY_CASES: [(&str, usize, usize); 5] = [
    ("column", 1, 1),
    ("every 8th", 8, 1),
    ("4 columns", 1, 4),
    ("16 columns", 1, 16),
    ("64 columns", 1, 64),
];
/// Warm rows per push in the busy-row section: a whole period of every case.
const BUSY_PUSH_ROWS: usize = 8;
const _: () = assert!(WARM_ROWS.is_multiple_of(BUSY_PUSH_ROWS));

/// Disc radii of the blob-cost section; 0 stands for the frame without discs.
const RADII: [u32; 5] = [0, 2, 4, 8, 12];
/// Discs per frame in the blob-cost section.
const SIZE_BLOBS: usize = 64;
/// Frame size of the blob-cost section.
const SIZE_W: u16 = 1280;
const SIZE_H: u16 = 800;
/// Placement attempts per disc (as `synth`).
const ATTEMPTS: u32 = 1000;

/// Cases per PIE self-test at boot: 48 rounds of the 256 source and destination alignment
/// pairs that `self_test_copy` walks through.
const PIE_CASES: u32 = 48 * 256;
/// Seed of the PIE self-tests.
const PIE_SEED: u32 = 0x5EED_0001;
/// A PIE self-test: `(seed, cases) -> failures`.
type PieTest = fn(&Pie, u32, u32) -> u32;
const PIE_TESTS: [(&str, PieTest); 4] = [
    ("first_bright", Pie::self_test),
    ("first_bright10", Pie::self_test10),
    ("copy", Pie::self_test_copy),
    ("sum16_be", Pie::self_test_sum),
];

/// Frames per pixel format in the boot equivalence check of the two skips.
const EQ_FRAMES: u32 = 320;
/// Seed of the equivalence check.
const EQ_SEED: u32 = 0x0E0A_1234;
/// Widest row of the equivalence check, in pixels.
const EQ_MAX_WIDTH: u32 = 1300;
/// Most rows per frame of the equivalence check.
const EQ_MAX_HEIGHT: u32 = 64;
/// Most bright rectangles per frame of the equivalence check.
const EQ_RECTS: usize = 12;
const _: () = assert!(
    16 + (EQ_MAX_WIDTH as usize / 4 * raw10::GROUP_BYTES) * EQ_MAX_HEIGHT as usize <= INTERNAL_LEN
);

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
/// Blob output of the second skip in the equivalence check.
static EQ_BLOBS: ConstStaticCell<[Blob; 256]> = ConstStaticCell::new(
    [Blob {
        x: 0,
        y: 0,
        area: 0,
        peak: 0,
    }; 256],
);

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
    /// Name of the dark skip ([`DarkSkip::name`]).
    skip: &'static str,
    min_us: u32,
    mean_us: u32,
    max_us: u32,
    found: u32,
    overflow: u32,
    ctr: Counters,
}

/// Every mode, blob count and path, with each of the two skips.
const MAX_CASES: usize = MODES.len() * BLOB_COUNTS.len() * 3 * 2;

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

/// Counter sums over the windows of one case, and the passes they cover.
#[derive(Clone, Copy, Default)]
struct Counters {
    passes: u32,
    cycles: u64,
    instructions: u64,
    branches: u64,
    mispredicts: u64,
    stores: u64,
    l1i_miss: u64,
    l1d_miss: u64,
}

impl Counters {
    fn add(&mut self, d: &Delta) {
        self.cycles += u64::from(d.cpu.cycles);
        self.instructions += u64::from(d.cpu.instructions);
        self.branches += u64::from(d.cpu.branches);
        self.mispredicts += u64::from(d.cpu.mispredicts);
        self.stores += u64::from(d.cpu.stores);
        self.l1i_miss += u64::from(d.cache.l1i_miss);
        self.l1d_miss += u64::from(d.cache.l1d_miss);
    }

    /// Mean of `sum` per pass.
    fn mean(&self, sum: u64) -> u64 {
        sum / u64::from(self.passes.max(1))
    }

    /// Mean of `sum` per pass and per `rows`, in thousandths.
    fn per_row_milli(&self, sum: u64, rows: usize) -> i64 {
        (sum * 1000 / (u64::from(self.passes.max(1)) * rows as u64)) as i64
    }

    /// Instructions per cycle, in thousandths.
    fn ipc_milli(&self) -> i64 {
        (self.instructions * 1000)
            .checked_div(self.cycles)
            .unwrap_or(0) as i64
    }
}

/// Runs `f` inside one counter window and adds the window to `ctr`. Only the closing
/// sample's five CSR reads fall inside it besides `f` (see `esp32p4_perf::Sample`). Not
/// inlined, so the samples' locals stay out of the callers' stack frames; the call itself is
/// outside the window.
#[inline(never)]
fn window<R>(ctr: &mut Counters, f: impl FnOnce() -> R) -> R {
    let start = Sample::now();
    let r = f();
    let d = start.elapsed();
    ctr.add(&d);
    r
}

/// A value in thousandths, printed rounded to the given number of decimals (1 to 3).
#[derive(Clone, Copy)]
struct Dec(i64, u32);

impl fmt::Display for Dec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Dec(v, digits) = *self;
        let unit = 10i64.pow(3 - digits);
        let r = (v.abs() + unit / 2) / unit;
        let scale = 10i64.pow(digits);
        let sign = if v < 0 && r != 0 { "-" } else { "" };
        write!(
            f,
            "{sign}{}.{:0width$}",
            r / scale,
            r % scale,
            width = digits as usize
        )
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
/// line per case, then the time and counter tables, then the row-cost and blob-cost tables.
pub fn run(psram: &mut [u8], pie: Option<&Pie>) {
    esp32p4_perf::init();
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
    let both;
    let skips: &[DarkSkip] = match pie {
        Some(p) => {
            both = [DarkSkip::Scalar, DarkSkip::Pie(p)];
            &both
        }
        None => &[DarkSkip::Scalar],
    };

    log_header(&bench);
    if let Some(p) = pie {
        equivalence(&mut bench, p);
    }
    run_cases(&mut bench, psram, synth, cases, skips);
    log_table(cases);
    log_counters(cases);
    for &skip in skips {
        row_cost(&mut bench, skip);
    }
    for &skip in skips {
        busy_rows(&mut bench, skip);
    }
    for &skip in skips {
        blob_cost(&mut bench, psram, synth, skip);
    }
    info!("detector bench done");
}

/// Runs the four PIE kernel self-tests (each against its scalar reference, `PIE_CASES`
/// randomised cases) and logs the failures and the time of each.
#[inline(never)]
pub fn pie_self_tests(pie: &Pie) {
    let mhz = esp_hal::clock::cpu_clock().as_mhz();
    for (name, test) in PIE_TESTS {
        let start = mcycle();
        let failures = test(pie, PIE_SEED, PIE_CASES);
        let us = mcycle().wrapping_sub(start) / mhz;
        if failures == 0 {
            info!("PIE self-test {name}: 0 failures of {PIE_CASES} cases, {us} us");
        } else {
            error!("PIE self-test {name}: {failures} failures of {PIE_CASES} cases, {us} us");
        }
    }
}

#[inline(never)]
fn log_header(b: &Bench) {
    info!(
        "detector bench: threshold {THRESHOLD}, {PASSES} passes, {STRIP_ROWS}-line strips, \
         detector {} B and strip/frame buffer {} KiB in internal RAM at {:#010x}",
        core::mem::size_of::<DefaultDetector>(),
        INTERNAL_LEN / 1024,
        b.internal.as_ptr() as usize
    );
    info!(
        "detector bench statics: detector {:#010x}, blob output {:#010x}, buffer {:#010x}",
        core::ptr::from_ref::<DefaultDetector>(b.det) as usize,
        b.out.as_ptr() as usize,
        b.internal.as_ptr() as usize
    );
}

/// Every mode, blob count and path, each with every skip of `skips`; one line per case, the
/// results into `cases`.
#[inline(never)]
fn run_cases(
    b: &mut Bench,
    psram: &mut [u8],
    synth: &mut synth::Synth,
    cases: &mut [Option<Case>],
    skips: &[DarkSkip],
) {
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
                for &skip in skips {
                    let case = b.measure(skip, frame, width, height, blobs, path);
                    log_case(&case);
                    if let Some(slot) = slots.next() {
                        *slot = Some(case);
                    }
                }
            }
        }
    }
}

impl Bench {
    /// Times `PASSES` passes of one path over `frame` (in PSRAM) with the dark skip `skip`.
    fn measure(
        &mut self,
        skip: DarkSkip,
        frame: &[u8],
        width: u16,
        height: u16,
        blobs: u16,
        path: Path,
    ) -> Case {
        let mut case = with_skip!(skip, s => self.measure_with(s, frame, width, path));
        case.height = height;
        case.blobs = blobs;
        case.skip = skip.name();
        case
    }

    /// [`Self::measure`] with the skip as a type: the detector calls are monomorphised.
    #[inline(never)]
    fn measure_with<S: Skip>(&mut self, s: &S, frame: &[u8], width: u16, path: Path) -> Case {
        let len = frame.len();
        let mut t = Timing::new(self.mhz);
        let mut ctr = Counters::default();
        let mut found = 0;
        let mut overflow = 0;
        if path == Path::Sram {
            self.internal[..len].copy_from_slice(frame);
        }
        for _ in 0..PASSES {
            let before = ctr.cycles;
            match path {
                Path::Psram => {
                    flush_cache(frame);
                    found = window(&mut ctr, || {
                        self.det
                            .process_frame_with(s, frame, width, THRESHOLD, self.out)
                    });
                }
                Path::Sram => {
                    let src = &self.internal[..len];
                    flush_cache(src);
                    found = window(&mut ctr, || {
                        self.det
                            .process_frame_with(s, src, width, THRESHOLD, self.out)
                    });
                }
                Path::Strip => {
                    found =
                        strip_pass(s, self.det, self.out, frame, width, self.internal, &mut ctr);
                }
            }
            ctr.passes += 1;
            t.add((ctr.cycles - before) as u32);
            overflow = overflow.max(self.det.stats().overflow);
        }
        let (min_us, mean_us, max_us) = t.us();
        Case {
            width,
            height: 0,
            blobs: 0,
            path,
            skip: "",
            min_us,
            mean_us,
            max_us,
            found: found as u32,
            overflow,
            ctr,
        }
    }
}

fn log_table(cases: &[Option<Case>]) {
    info!("detector bench table (us, frame period {FRAME_PERIOD_US} us at 120 fps):");
    info!("| mode | blobs | path | skip | min | mean | max | found | overflow | 120 fps |");
    for c in cases.iter().flatten() {
        info!(
            "| {}x{} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c.width,
            c.height,
            c.blobs,
            c.path.name(),
            c.skip,
            c.min_us,
            c.mean_us,
            c.max_us,
            c.found,
            c.overflow,
            verdict(c.max_us)
        );
    }
}

fn log_counters(cases: &[Option<Case>]) {
    info!(
        "detector bench counters (per-frame means over {PASSES} passes; misses are miss events, \
         not lines):"
    );
    info!(
        "| mode | blobs | path | skip | cycles | instructions | IPC | branches | mispredicts | \
         stores | l1i_miss | l1d_miss |"
    );
    for c in cases.iter().flatten() {
        log_counter_row(c);
    }
}

#[inline(never)]
fn log_counter_row(c: &Case) {
    let k = &c.ctr;
    info!(
        "| {}x{} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        c.width,
        c.height,
        c.blobs,
        c.path.name(),
        c.skip,
        k.mean(k.cycles),
        k.mean(k.instructions),
        Dec(k.ipc_milli(), 3),
        k.mean(k.branches),
        k.mean(k.mispredicts),
        k.mean(k.stores),
        k.mean(k.l1i_miss),
        k.mean(k.l1d_miss),
    );
}

/// One frame through the strip path with the dark skip `skip`: adds the counter windows of
/// the detector calls only to `ctr` and returns the blobs found.
fn strip_pass<S: Skip>(
    skip: &S,
    det: &mut DefaultDetector,
    out: &mut [Blob],
    frame: &[u8],
    width: u16,
    strip_buf: &mut [u8],
    ctr: &mut Counters,
) -> usize {
    let strip_len = STRIP_ROWS * usize::from(width);
    window(ctr, || det.begin_frame(width, THRESHOLD));
    for (i, src) in frame.chunks(strip_len).enumerate() {
        let strip = &mut strip_buf[..src.len()];
        strip.copy_from_slice(src);
        flush_cache(strip);
        window(ctr, || {
            det.push_rows_with(skip, strip, (i * STRIP_ROWS) as u16)
        });
    }
    window(ctr, || det.finish(out))
}

/// Row cost of one width: warm and cold counters and their row counts.
struct RowCase {
    width: usize,
    warm: Counters,
    warm_rows: usize,
    cold: Counters,
    cold_rows: usize,
}

/// Picks one counter sum.
type Pick = fn(&Counters) -> u64;

/// The fits of the row-cost section: name, warm (else cold), the counter fitted.
const FITS: [(&str, bool, Pick); 4] = [
    ("warm cycles", true, |c| c.cycles),
    ("warm instructions", true, |c| c.instructions),
    ("cold cycles", false, |c| c.cycles),
    ("cold instructions", false, |c| c.instructions),
];

/// All-dark rows through `begin_frame` + `push_rows_with` at each of [`ROW_WIDTHS`]; logs
/// the per-row table and the linear fits.
#[inline(never)]
fn row_cost(b: &mut Bench, skip: DarkSkip) {
    dark_fill(&mut b.internal[..COLD_BYTES]);
    log_row_header(skip);
    // Per-row means in thousandths, one array per fit, one entry per width.
    let mut points = [[0i64; ROW_WIDTHS.len()]; FITS.len()];
    for (i, &w) in ROW_WIDTHS.iter().enumerate() {
        let r = row_case(b, skip, w, WARM_BYTES / w);
        log_row_case("dark", &r);
        for (k, &(_, warm, pick)) in FITS.iter().enumerate() {
            let (c, n) = if warm {
                (&r.warm, r.warm_rows)
            } else {
                (&r.cold, r.cold_rows)
            };
            points[k][i] = c.per_row_milli(pick(c), n);
        }
    }
    log_fit_header(skip);
    for (&(name, _, _), p) in FITS.iter().zip(&points) {
        log_fit(name, p);
    }
}

/// Rows with bright pixels at [`BUSY_WIDTH`], through `begin_frame` + `push_rows_with`, warm
/// and cold as the dark rows; logs one line per case of [`BUSY_CASES`].
#[inline(never)]
fn busy_rows(b: &mut Bench, skip: DarkSkip) {
    info!(
        "detector busy rows, skip {} ({BUSY_WIDTH} wide, dark rows with pixels of 255 every \
         `period` rows: column {BUSY_COLUMN}, or `n` columns at 3 + i x {BUSY_WIDTH} / n; \
         begin_frame + push_rows, per row, means over {ROW_PASSES} passes; warm = \
         {BUSY_PUSH_ROWS} rows pushed again and again, cold = {COLD_BYTES} B flushed before \
         each pass):",
        skip.name()
    );
    log_row_columns();
    for (name, period, n) in BUSY_CASES {
        busy_fill(&mut b.internal[..COLD_BYTES], period, n);
        let r = row_case(b, skip, BUSY_WIDTH, BUSY_PUSH_ROWS);
        log_row_case(name, &r);
    }
    dark_fill(&mut b.internal[..COLD_BYTES]);
}

/// Dark rows of [`BUSY_WIDTH`] with pixels of 255 every `period` rows, `n` per row.
#[inline(never)]
fn busy_fill(buf: &mut [u8], period: usize, n: usize) {
    dark_fill(buf);
    for row in buf
        .as_chunks_mut::<BUSY_WIDTH>()
        .0
        .iter_mut()
        .step_by(period)
    {
        if n == 1 {
            row[BUSY_COLUMN] = 255;
        } else {
            for i in 0..n {
                row[3 + i * BUSY_WIDTH / n] = 255;
            }
        }
    }
}

#[inline(never)]
fn log_fit_header(skip: DarkSkip) {
    info!(
        "detector row cost fit, skip {} (least squares over the {} widths: per row + per \
         pixel x width):",
        skip.name(),
        ROW_WIDTHS.len()
    );
    info!("| fit | per row | per pixel | largest residual |");
}

/// Fits per-row means (thousandths, one per width of [`ROW_WIDTHS`]) and logs the line.
#[inline(never)]
fn log_fit(name: &str, per_row: &[i64; ROW_WIDTHS.len()]) {
    let points =
        core::array::from_fn::<_, { ROW_WIDTHS.len() }, _>(|i| (ROW_WIDTHS[i] as i64, per_row[i]));
    let f = fit(&points);
    info!(
        "| {name} | {} | {} | {} |",
        Dec(f.a, 1),
        Dec(f.b, 3),
        Dec(f.worst, 1)
    );
}

/// Warm and cold row passes at width `w` with the dark skip `skip`, the first pass of each
/// discarded. Warm pushes the first `push_rows` rows of the buffer again and again.
#[inline(never)]
fn row_case(b: &mut Bench, skip: DarkSkip, w: usize, push_rows: usize) -> RowCase {
    let det = &mut *b.det;
    let width = w as u16;
    let mut discard = Counters::default();
    // Warm: the same rows again and again, as consecutive frame rows.
    let per_push = push_rows;
    let buf = &b.internal[..per_push * w];
    let mut warm = Counters::default();
    for pass in 0..=ROW_PASSES {
        let ctr = if pass == 0 { &mut discard } else { &mut warm };
        with_skip!(skip, s => window(ctr, || {
            det.begin_frame(width, THRESHOLD);
            for k in 0..WARM_ROWS / per_push {
                det.push_rows_with(s, buf, (k * per_push) as u16);
            }
        }));
        ctr.passes += 1;
    }
    // Cold: one buffer, flushed before each pass.
    let cold_rows = COLD_BYTES / w;
    let buf = &b.internal[..cold_rows * w];
    let mut cold = Counters::default();
    for pass in 0..=ROW_PASSES {
        let ctr = if pass == 0 { &mut discard } else { &mut cold };
        flush_cache(buf);
        with_skip!(skip, s => window(ctr, || {
            det.begin_frame(width, THRESHOLD);
            det.push_rows_with(s, buf, 0);
        }));
        ctr.passes += 1;
    }
    RowCase {
        width: w,
        warm,
        warm_rows: WARM_ROWS,
        cold,
        cold_rows,
    }
}

#[inline(never)]
fn log_row_header(skip: DarkSkip) {
    info!(
        "detector row cost, skip {} (all-dark rows, begin_frame + push_rows, per row, means \
         over {ROW_PASSES} passes; warm = {WARM_BYTES} B of rows pushed again and again, cold \
         = {COLD_BYTES} B flushed before each pass):",
        skip.name()
    );
    log_row_columns();
}

#[inline(never)]
fn log_row_columns() {
    info!(
        "| rows | width | warm rows | warm cycles | warm instructions | warm branches | warm \
         mispredicts | warm stores | cold rows | cold cycles | cold instructions | cold l1d_miss |"
    );
}

#[inline(never)]
fn log_row_case(name: &str, r: &RowCase) {
    let (w, c) = (&r.warm, &r.cold);
    info!(
        "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        r.width,
        r.warm_rows,
        Dec(w.per_row_milli(w.cycles, r.warm_rows), 1),
        Dec(w.per_row_milli(w.instructions, r.warm_rows), 1),
        Dec(w.per_row_milli(w.branches, r.warm_rows), 1),
        Dec(w.per_row_milli(w.mispredicts, r.warm_rows), 1),
        Dec(w.per_row_milli(w.stores, r.warm_rows), 1),
        r.cold_rows,
        Dec(c.per_row_milli(c.cycles, r.cold_rows), 1),
        Dec(c.per_row_milli(c.instructions, r.cold_rows), 1),
        Dec(c.per_row_milli(c.l1d_miss, r.cold_rows), 1),
    );
}

/// A least-squares line `y = a + b x`.
struct Fit {
    /// Intercept, in thousandths.
    a: i64,
    /// Slope, in thousandths per unit of `x`.
    b: i64,
    /// Largest distance of a point from the line, in thousandths.
    worst: i64,
}

/// Fits `y = a + b x` to `(x, y)` points with `y` in thousandths.
fn fit(points: &[(i64, i64)]) -> Fit {
    let n = points.len() as i64;
    let (sx, sy, sxx, sxy) = points
        .iter()
        .fold((0, 0, 0, 0), |(sx, sy, sxx, sxy), &(x, y)| {
            (sx + x, sy + y, sxx + x * x, sxy + x * y)
        });
    let den = (n * sxx - sx * sx).max(1);
    // The slope in millionths per unit of x, so three decimals survive the division.
    let b_micro = (n * sxy - sx * sy) * 1000 / den;
    let a = (sy * 1000 - b_micro * sx) / (n * 1000);
    let worst = points
        .iter()
        .map(|&(x, y)| (y - a - b_micro * x / 1000).abs())
        .max()
        .unwrap_or(0);
    Fit {
        a,
        b: (b_micro + 500).div_euclid(1000),
        worst,
    }
}

/// Fills `buf` with dark bytes in 0..64: the OR of any of them stays below the threshold,
/// so the dark skip never stops.
fn dark_fill(buf: &mut [u8]) {
    let mut x = 0x2545_F491u32;
    for b in buf.iter_mut() {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *b = (x >> 24) as u8 & 0x3F;
    }
}

/// The 1280x800 strip path with [`SIZE_BLOBS`] discs of each radius of [`RADII`] and the
/// dark skip `skip`; logs the table with the cost per blob against the frame without discs.
#[inline(never)]
fn blob_cost(b: &mut Bench, psram: &mut [u8], synth: &mut synth::Synth, skip: DarkSkip) {
    let frame = &mut psram[..usize::from(SIZE_W) * usize::from(SIZE_H)];
    info!(
        "detector blob cost, skip {}: {SIZE_W}x{SIZE_H} strip path, {SIZE_BLOBS} discs per \
         frame, per blob = (frame - frame without discs) / found:",
        skip.name()
    );
    info!(
        "| radius | found | mean area | mean us | cycles | instructions | cycles/blob | \
         instructions/blob | branches/blob | mispredicts/blob | stores/blob |"
    );
    let mut dark = Counters::default();
    for r in RADII {
        synth.fill(frame, SIZE_W.into(), SIZE_H.into(), 0, SEED);
        let drawn = draw_discs(frame, SIZE_W.into(), SIZE_H.into(), r, SEED);
        let c = b.measure(skip, frame, SIZE_W, SIZE_H, drawn as u16, Path::Strip);
        let found = (c.found as usize).min(b.out.len());
        let area = b.out[..found]
            .iter()
            .map(|blob| u32::from(blob.area))
            .sum::<u32>()
            .checked_div(found as u32)
            .unwrap_or(0);
        if r == 0 {
            dark = c.ctr;
        }
        log_blob_case(r, &c, area, &dark);
    }
}

/// One row of the blob-cost table; `dark` is the frame without discs.
#[inline(never)]
fn log_blob_case(r: u32, c: &Case, area: u32, dark: &Counters) {
    let k = &c.ctr;
    if r == 0 {
        info!(
            "| none | {} | - | {} | {} | {} | - | - | - | - | - |",
            c.found,
            c.mean_us,
            k.mean(k.cycles),
            k.mean(k.instructions)
        );
        return;
    }
    let per_blob = |pick: Pick| -> i64 {
        let diff = k.mean(pick(k)) as i64 - dark.mean(pick(dark)) as i64;
        diff / i64::from(c.found.max(1))
    };
    info!(
        "| {r} | {} | {area} | {} | {} | {} | {} | {} | {} | {} | {} |",
        c.found,
        c.mean_us,
        k.mean(k.cycles),
        k.mean(k.instructions),
        per_blob(|x| x.cycles),
        per_blob(|x| x.instructions),
        per_blob(|x| x.branches),
        per_blob(|x| x.mispredicts),
        per_blob(|x| x.stores),
    );
}

/// Draws [`SIZE_BLOBS`] discs of radius `r` (fewer if the frame has no room left) into a
/// `width` x `height` RAW8 frame with the `synth` spacing: centres at least `r + 2` from the
/// edges, bounding squares at least 2 pixels apart, so each disc is one blob with its centroid
/// on its centre. Centres come from a seeded xorshift. Returns how many were drawn; none for
/// `r == 0`.
#[inline(never)]
fn draw_discs(frame: &mut [u8], width: usize, height: usize, r: u32, seed: u32) -> usize {
    let (w, h) = (width as u32, height as u32);
    let margin = r + 2;
    if r == 0 || w <= 2 * margin || h <= 2 * margin || w > u32::from(u16::MAX) {
        return 0;
    }
    let mut placed = [(0u16, 0u16); SIZE_BLOBS];
    let mut state = seed | 1;
    let mut below = |n: u32| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state % n
    };
    let gap = 2 * r + 4;
    let mut n = 0;
    while n < placed.len() {
        let mut centre = None;
        for _ in 0..ATTEMPTS {
            let cx = margin + below(w - 2 * margin);
            let cy = margin + below(h - 2 * margin);
            let clear = placed[..n]
                .iter()
                .all(|&(px, py)| cx.abs_diff(px.into()) > gap || cy.abs_diff(py.into()) > gap);
            if clear {
                centre = Some((cx, cy));
                break;
            }
        }
        let Some((cx, cy)) = centre else {
            break;
        };
        placed[n] = (cx as u16, cy as u16);
        n += 1;
        draw_disc(frame, width, cx as usize, cy as usize, r);
    }
    n
}

/// The `synth` disc profile: `synth::PEAK * (rr - d²) / rr` with `rr = (r + 1)²`, drawn over
/// the background where brighter. The disc must lie inside the frame.
fn draw_disc(frame: &mut [u8], width: usize, cx: usize, cy: usize, r: u32) {
    let reach = r as usize + 1;
    let rr = (reach * reach) as u32;
    for y in cy - reach..=cy + reach {
        for x in cx - reach..=cx + reach {
            let d2 = (x.abs_diff(cx).pow(2) + y.abs_diff(cy).pow(2)) as u32;
            if d2 < rr {
                let v = (u32::from(synth::PEAK) * (rr - d2) / rr) as u8;
                let p = &mut frame[y * width + x];
                *p = (*p).max(v);
            }
        }
    }
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
        "bench {}x{} blobs {:>2} {:<5} {:<6}: min {} mean {} max {} us, found {}, overflow {}, {} ({} us budget)",
        c.width,
        c.height,
        c.blobs,
        c.path.name(),
        c.skip,
        c.min_us,
        c.mean_us,
        c.max_us,
        c.found,
        c.overflow,
        verdict(c.max_us),
        FRAME_PERIOD_US
    );
}

/// Xorshift generator of the equivalence check.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// A value in `0..n` (`n > 0`).
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// One frame of the equivalence check, at `start` in the buffer.
struct EqFrame {
    raw10: bool,
    width: u16,
    height: usize,
    /// In the units of the format (8 or 10 bits).
    threshold: u16,
    start: usize,
    /// Bytes per row.
    stride: usize,
}

/// How the pixels of one equivalence frame are drawn.
struct EqScene {
    threshold: u32,
    /// Largest pixel value of the format.
    max: u32,
    /// Background just under the threshold (by 1 to `spread`), else dark (below 16).
    near: bool,
    spread: u32,
    /// Isolated bright pixels, one in `2^sparse` on average; none if `None`.
    sparse: Option<u32>,
    /// Bright rectangles `(x0, y0, x1, y1)`, ends exclusive.
    rects: [(usize, usize, usize, usize); EQ_RECTS],
    nrects: usize,
}

impl EqScene {
    fn value(&self, rng: &mut Rng, x: usize, y: usize) -> u32 {
        let in_rect = self.rects[..self.nrects]
            .iter()
            .any(|&(x0, y0, x1, y1)| (x0..x1).contains(&x) && (y0..y1).contains(&y));
        let isolated = self.sparse.is_some_and(|s| rng.next() >> (32 - s) == 0);
        if in_rect || isolated {
            self.threshold + rng.below(self.max - self.threshold + 1)
        } else if self.near {
            self.threshold.saturating_sub(1 + rng.below(self.spread))
        } else {
            rng.below(self.threshold.min(16))
        }
    }
}

/// Draws one random frame of the equivalence check into `buf`: RAW8 widths 1..=1300, RAW10
/// widths multiples of 4 up to 1300, 1 to 64 rows, any start alignment mod 16, a threshold
/// anywhere in the range, background dark or just under the threshold, isolated bright
/// pixels at one of four densities and up to [`EQ_RECTS`] bright rectangles of 1 to 8 pixels
/// a side.
#[inline(never)]
fn eq_frame(rng: &mut Rng, buf: &mut [u8], raw10: bool) -> EqFrame {
    let width = if raw10 {
        4 * (1 + rng.below(EQ_MAX_WIDTH / 4))
    } else {
        1 + rng.below(EQ_MAX_WIDTH)
    } as usize;
    let height = 1 + rng.below(EQ_MAX_HEIGHT) as usize;
    let max = if raw10 { 1023 } else { 255 };
    let threshold = 1 + rng.below(max);
    let stride = if raw10 {
        width / 4 * raw10::GROUP_BYTES
    } else {
        width
    };
    let start = rng.below(16) as usize;
    let mut scene = EqScene {
        threshold,
        max,
        near: rng.below(4) != 0,
        spread: [1, 4, 16, 64, max][rng.below(5) as usize],
        sparse: [None, Some(12), Some(8), Some(5)][rng.below(4) as usize],
        rects: [(0, 0, 0, 0); EQ_RECTS],
        nrects: rng.below(EQ_RECTS as u32 + 1) as usize,
    };
    for r in &mut scene.rects[..scene.nrects] {
        let (x0, y0) = (
            rng.below(width as u32) as usize,
            rng.below(height as u32) as usize,
        );
        *r = (
            x0,
            y0,
            x0 + 1 + rng.below(8) as usize,
            y0 + 1 + rng.below(8) as usize,
        );
    }
    eq_fill(
        rng,
        &mut buf[start..start + stride * height],
        stride,
        raw10,
        &scene,
    );
    EqFrame {
        raw10,
        width: width as u16,
        height,
        threshold: threshold as u16,
        start,
        stride,
    }
}

/// Writes the pixels of `scene` into `frame`, rows of `stride` bytes, RAW8 or RAW10.
#[inline(never)]
fn eq_fill(rng: &mut Rng, frame: &mut [u8], stride: usize, raw10: bool, scene: &EqScene) {
    for (y, row) in frame.chunks_exact_mut(stride).enumerate() {
        if raw10 {
            let groups = row.as_chunks_mut::<{ raw10::GROUP_BYTES }>().0;
            for (g, group) in groups.iter_mut().enumerate() {
                let px = core::array::from_fn(|k| scene.value(rng, 4 * g + k, y) as u16);
                *group = raw10::pack(px);
            }
        } else {
            for (x, p) in row.iter_mut().enumerate() {
                *p = scene.value(rng, x, y) as u8;
            }
        }
    }
}

/// Runs frame `f` of `buf` through the detector with `skip`, in strips of random heights
/// drawn from `split_seed`; returns the blob count and the `Stats`.
#[inline(never)]
fn eq_detect<S: Skip>(
    skip: &S,
    det: &mut DefaultDetector,
    out: &mut [Blob],
    buf: &[u8],
    f: &EqFrame,
    split_seed: u32,
) -> (usize, Stats) {
    let frame = &buf[f.start..f.start + f.stride * f.height];
    if f.raw10 {
        det.begin_frame_raw10(f.width, f.threshold);
    } else {
        det.begin_frame(f.width, f.threshold as u8);
    }
    let mut rng = Rng(split_seed | 1);
    let mut row = 0;
    while row < f.height {
        let n = (1 + rng.below(f.height as u32) as usize).min(f.height - row);
        let strip = &frame[row * f.stride..(row + n) * f.stride];
        det.push_rows_with(skip, strip, row as u16);
        row += n;
    }
    let n = det.finish(out);
    (n, det.stats())
}

/// Boot check of the PIE skip against [`espion_vision::Scalar`]: [`EQ_FRAMES`] random RAW8
/// frames and as many RAW10 ones ([`eq_frame`]), each through both skips with the same
/// random strip split. The blob count, the blobs in order and the `Stats` must be identical;
/// logs the number of frames where they are not (and the first few of them).
#[inline(never)]
fn equivalence(b: &mut Bench, pie: &Pie) {
    let other = EQ_BLOBS.take();
    let mut rng = Rng(EQ_SEED);
    let start = mcycle();
    let mut mismatches = 0u32;
    let mut blobs = 0u32;
    let mut overflowed = 0u32;
    for i in 0..2 * EQ_FRAMES {
        let f = eq_frame(&mut rng, b.internal, i >= EQ_FRAMES);
        let split = rng.next();
        let scalar = &espion_vision::Scalar;
        let (n0, s0) = eq_detect(scalar, b.det, b.out, b.internal, &f, split);
        let (n1, s1) = eq_detect(pie, b.det, other, b.internal, &f, split);
        if n0 != n1 || s0 != s1 || b.out[..n0] != other[..n1] {
            if mismatches < 4 {
                log_mismatch(i, &f, (n0, &s0), (n1, &s1));
            }
            mismatches += 1;
        }
        blobs += n0 as u32;
        overflowed += u32::from(s0.overflow != 0);
    }
    let ms = mcycle().wrapping_sub(start) / b.mhz / 1000;
    info!(
        "detector skip equivalence (scalar against pie, {EQ_FRAMES} RAW8 and {EQ_FRAMES} RAW10 \
         random frames, random strip splits): {mismatches} mismatches of {} frames; {blobs} \
         blobs, {overflowed} frames with overflow, {ms} ms",
        2 * EQ_FRAMES
    );
}

#[inline(never)]
fn log_mismatch(i: u32, f: &EqFrame, scalar: (usize, &Stats), pie: (usize, &Stats)) {
    error!(
        "skip equivalence: frame {i} (raw10 {}, {}x{}, threshold {}) differs: scalar {} blobs \
         {:?}, pie {} blobs {:?}",
        f.raw10, f.width, f.height, f.threshold, scalar.0, scalar.1, pie.0, pie.1
    );
}
