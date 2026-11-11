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
//! conditional branches, mispredictions, stores and the L1 cache miss events. Two sections
//! follow the cases:
//! - row cost: all-dark rows through `begin_frame` + `push_rows` at several widths, warm (a
//!   few KiB of rows pushed again and again as consecutive rows, so they stay in the L1
//!   cache) and cold (an 80 KiB buffer flushed before each pass), with a least-squares fit
//!   `cost = a per row + b per pixel`;
//! - blob cost: the 1280x800 strip path with 64 discs of radius 2, 4, 8 and 12, drawn here
//!   over the `synth` background (`synth` draws radius 3..5 only), per blob from the
//!   difference to the same background without discs.
//!
//! No CSI DMA runs at the same time, so the PSRAM bus is idle apart from the CPU: these are
//! lower bounds for the live case. Nothing is logged inside a timed window.

use core::fmt;

use esp32p4_perf::{Delta, Sample};
use espion_vision::{Blob, DefaultDetector, synth};
use log::info;
use static_cell::ConstStaticCell;

use crate::flush_cache;

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

/// Disc radii of the blob-cost section; 0 stands for the frame without discs.
const RADII: [u32; 5] = [0, 2, 4, 8, 12];
/// Discs per frame in the blob-cost section.
const SIZE_BLOBS: usize = 64;
/// Frame size of the blob-cost section.
const SIZE_W: u16 = 1280;
const SIZE_H: u16 = 800;
/// Placement attempts per disc (as `synth`).
const ATTEMPTS: u32 = 1000;

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
    ctr: Counters,
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
pub fn run(psram: &mut [u8]) {
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

    log_header(&bench);
    run_cases(&mut bench, psram, synth, cases);
    log_table(cases);
    log_counters(cases);
    row_cost(&mut bench);
    blob_cost(&mut bench, psram, synth);
    info!("detector bench done");
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

/// Every mode, blob count and path; one line per case, the results into `cases`.
#[inline(never)]
fn run_cases(
    b: &mut Bench,
    psram: &mut [u8],
    synth: &mut synth::Synth,
    cases: &mut [Option<Case>],
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
                let case = b.measure(frame, width, height, blobs, path);
                log_case(&case);
                if let Some(slot) = slots.next() {
                    *slot = Some(case);
                }
            }
        }
    }
}

impl Bench {
    /// Times `PASSES` passes of one path over `frame` (in PSRAM).
    #[inline(never)]
    fn measure(&mut self, frame: &[u8], width: u16, height: u16, blobs: u16, path: Path) -> Case {
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
                        self.det.process_frame(frame, width, THRESHOLD, self.out)
                    });
                }
                Path::Sram => {
                    let src = &self.internal[..len];
                    flush_cache(src);
                    found = window(&mut ctr, || {
                        self.det.process_frame(src, width, THRESHOLD, self.out)
                    });
                }
                Path::Strip => {
                    found = strip_pass(self.det, self.out, frame, width, self.internal, &mut ctr);
                }
            }
            ctr.passes += 1;
            t.add((ctr.cycles - before) as u32);
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
            ctr,
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
}

fn log_counters(cases: &[Option<Case>]) {
    info!(
        "detector bench counters (per-frame means over {PASSES} passes; misses are miss events, \
         not lines):"
    );
    info!(
        "| mode | blobs | path | cycles | instructions | IPC | branches | mispredicts | stores | \
         l1i_miss | l1d_miss |"
    );
    for c in cases.iter().flatten() {
        log_counter_row(c);
    }
}

#[inline(never)]
fn log_counter_row(c: &Case) {
    let k = &c.ctr;
    info!(
        "| {}x{} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        c.width,
        c.height,
        c.blobs,
        c.path.name(),
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

/// One frame through the strip path: adds the counter windows of the detector calls only
/// to `ctr` and returns the blobs found.
fn strip_pass(
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
        window(ctr, || det.push_rows(strip, (i * STRIP_ROWS) as u16));
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

/// All-dark rows through `begin_frame` + `push_rows` at each of [`ROW_WIDTHS`]; logs the
/// per-row table and the linear fits.
#[inline(never)]
fn row_cost(b: &mut Bench) {
    dark_fill(&mut b.internal[..COLD_BYTES]);
    log_row_header();
    // Per-row means in thousandths, one array per fit, one entry per width.
    let mut points = [[0i64; ROW_WIDTHS.len()]; FITS.len()];
    for (i, &w) in ROW_WIDTHS.iter().enumerate() {
        let r = row_case(b, w);
        log_row_case(&r);
        for (k, &(_, warm, pick)) in FITS.iter().enumerate() {
            let (c, n) = if warm {
                (&r.warm, r.warm_rows)
            } else {
                (&r.cold, r.cold_rows)
            };
            points[k][i] = c.per_row_milli(pick(c), n);
        }
    }
    log_fit_header();
    for (&(name, _, _), p) in FITS.iter().zip(&points) {
        log_fit(name, p);
    }
}

#[inline(never)]
fn log_fit_header() {
    info!(
        "detector row cost fit (least squares over the {} widths: per row + per pixel x width):",
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

/// Warm and cold row passes at width `w`, the first pass of each discarded.
#[inline(never)]
fn row_case(b: &mut Bench, w: usize) -> RowCase {
    let det = &mut *b.det;
    let width = w as u16;
    let mut discard = Counters::default();
    // Warm: the same rows again and again, as consecutive frame rows.
    let per_push = WARM_BYTES / w;
    let buf = &b.internal[..per_push * w];
    let mut warm = Counters::default();
    for pass in 0..=ROW_PASSES {
        let ctr = if pass == 0 { &mut discard } else { &mut warm };
        window(ctr, || {
            det.begin_frame(width, THRESHOLD);
            for k in 0..WARM_ROWS / per_push {
                det.push_rows(buf, (k * per_push) as u16);
            }
        });
        ctr.passes += 1;
    }
    // Cold: one buffer, flushed before each pass.
    let cold_rows = COLD_BYTES / w;
    let buf = &b.internal[..cold_rows * w];
    let mut cold = Counters::default();
    for pass in 0..=ROW_PASSES {
        let ctr = if pass == 0 { &mut discard } else { &mut cold };
        flush_cache(buf);
        window(ctr, || {
            det.begin_frame(width, THRESHOLD);
            det.push_rows(buf, 0);
        });
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
fn log_row_header() {
    info!(
        "detector row cost (all-dark rows, begin_frame + push_rows, per row, means over \
         {ROW_PASSES} passes; warm = {WARM_BYTES} B of rows pushed again and again, cold = \
         {COLD_BYTES} B flushed before each pass):"
    );
    info!(
        "| width | warm rows | warm cycles | warm instructions | warm branches | warm mispredicts \
         | warm stores | cold rows | cold cycles | cold instructions | cold l1d_miss |"
    );
}

#[inline(never)]
fn log_row_case(r: &RowCase) {
    let (w, c) = (&r.warm, &r.cold);
    info!(
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
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

/// The 1280x800 strip path with [`SIZE_BLOBS`] discs of each radius of [`RADII`]; logs the
/// table with the cost per blob against the frame without discs.
#[inline(never)]
fn blob_cost(b: &mut Bench, psram: &mut [u8], synth: &mut synth::Synth) {
    let frame = &mut psram[..usize::from(SIZE_W) * usize::from(SIZE_H)];
    info!(
        "detector blob cost: {SIZE_W}x{SIZE_H} strip path, {SIZE_BLOBS} discs per frame, per \
         blob = (frame - frame without discs) / found:"
    );
    info!(
        "| radius | found | mean area | mean us | cycles | instructions | cycles/blob | \
         instructions/blob | branches/blob | mispredicts/blob | stores/blob |"
    );
    let mut dark = Counters::default();
    for r in RADII {
        synth.fill(frame, SIZE_W.into(), SIZE_H.into(), 0, SEED);
        let drawn = draw_discs(frame, SIZE_W.into(), SIZE_H.into(), r, SEED);
        let c = b.measure(frame, SIZE_W, SIZE_H, drawn as u16, Path::Strip);
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
