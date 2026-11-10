//! Strip soak: blob detection on 64-line strips that the CSI DMA writes into a ring of
//! internal-RAM buffers (`esp32p4_csi::CsiStrips`), on the executor that receives them.
//!
//! Per mode, with the sensor free-running:
//! - a live run: every strip from `wait_strip` goes through `begin_frame` (row 0),
//!   `push_rows` and `finish` (last strip), then is given back;
//! - synthetic runs: strips are still waited for and given back at the sensor's rate, but
//!   `push_rows` reads a synthetic strip held in internal RAM instead, written back and
//!   invalidated from the cache before every strip (as `wait_strip` does for a real one).
//!
//! The synthetic strip is 64 rows of four 16-row bands, each filled by `synth::Synth` with
//! its own disc count. Every frame height of the four modes is a multiple of 16 and every
//! last strip is 16 or 32 rows, so each strip of a frame (including the short last one, which
//! reads the first bands only) holds whole discs and nothing else. The band counts are
//! searched so the blobs per frame come as close to the target as the frame geometry allows;
//! the exact figure is logged before each run.
//!
//! Nothing is logged while a run measures.

use embassy_time::{Duration, Instant, with_timeout};
use esp32p4_csi::{Config, CsiStrips, Strip, StripLink, StripStats};
use log::info;
use static_cell::ConstStaticCell;

use crate::ov9281::Mode;
use crate::soak::{BUDGET_US, Fps, THRESHOLD, take_detector};
use crate::{flush_cache, mcycle, synth, vision};

/// Rows per strip. Not a divisor of any of the four mode heights (the driver needs a short
/// last strip), and `width * LINES` is a whole number of 4096-byte bursts at 640 and 1280.
pub const LINES: u16 = 64;
/// Bytes of one 1280-wide strip, the widest.
const STRIP_BYTES: usize = 1280 * LINES as usize;
/// The ring's memory: 4 strips at 1280 wide, or 8 at 640 (320 KiB).
const RING_BYTES: usize = 4 * STRIP_BYTES;
/// Most strips in the ring (list items).
const MAX_STRIPS: usize = 8;
/// Rows per synthetic band.
const BAND_ROWS: usize = 16;
/// Bands per synthetic strip.
const BANDS: usize = LINES as usize / BAND_ROWS;
/// Most discs per band the search tries.
const MAX_PER_BAND: u16 = 9;
/// A wait ends the run if no strip arrives for this long.
const STRIP_TIMEOUT: Duration = Duration::from_millis(500);
/// A strip queued longer ago than this (us) is backlog, not the current frame.
const FRESH_US: u32 = 200;
/// How long to wait for the first whole frame before a run.
const SYNC_TIMEOUT: Duration = Duration::from_secs(2);
/// Synthetic band seed (band `j` uses `SEED + j`).
const SEED: u32 = 0x1234_5678;
/// Result slots: 4 modes x (live + 3 synthetic).
const MAX_RUNS: usize = 16;

/// The ring's memory, cache-line aligned.
#[repr(C, align(64))]
struct RingMem([u8; RING_BYTES]);

static RING: ConstStaticCell<RingMem> = ConstStaticCell::new(RingMem([0; RING_BYTES]));
static LINKS: ConstStaticCell<[StripLink; MAX_STRIPS]> =
    ConstStaticCell::new([StripLink::NEW; MAX_STRIPS]);
static RESULTS: ConstStaticCell<[Option<RunResult>; MAX_RUNS]> =
    ConstStaticCell::new([None; MAX_RUNS]);
static ACC: ConstStaticCell<Acc> = ConstStaticCell::new(Acc::NEW);

/// `N` strip buffers and their list items, as `CsiStrips::new` takes them.
pub type RingSplit<const N: usize> = ([&'static mut [u8]; N], &'static mut [StripLink; N]);

/// The ring memory and list items (internal RAM, statics), split per receiver: the driver
/// keeps what it is given for `'static`, so the same memory is handed out again once that
/// receiver is dropped.
pub struct Ring {
    mem: *mut u8,
    links: *mut StripLink,
}

/// Strips in the ring for `config`: 8 if 8 fit (640 wide and less), else 4.
pub const fn ring_strips(config: &Config) -> usize {
    if MAX_STRIPS * CsiStrips::strip_bytes(config, LINES) <= RING_BYTES {
        MAX_STRIPS
    } else {
        RING_BYTES / STRIP_BYTES
    }
}

impl Ring {
    /// Takes the statics. Call once.
    pub fn take() -> Self {
        Self {
            mem: RING.take().0.as_mut_ptr(),
            links: LINKS.take().as_mut_ptr(),
        }
    }

    /// `N` strips sized for `config`, from the start of the ring memory, and `N` list items.
    ///
    /// # Safety
    ///
    /// No receiver built from an earlier split may still be alive (it would share the
    /// memory with the DMA running).
    pub unsafe fn split<const N: usize>(&mut self, config: &Config) -> RingSplit<N> {
        let bytes = CsiStrips::strip_bytes(config, LINES);
        assert!(N <= MAX_STRIPS && N * bytes <= RING_BYTES);
        let mem = self.mem;
        // SAFETY: `N` disjoint ranges inside the ring memory (checked above), which `self`
        // owns; the caller guarantees that nothing else uses it any more.
        let strips = core::array::from_fn(|i| unsafe {
            core::slice::from_raw_parts_mut(mem.add(i * bytes), bytes)
        });
        // SAFETY: the first `N` of the `MAX_STRIPS` items, owned by `self` as above.
        let links = unsafe { &mut *self.links.cast::<[StripLink; N]>() };
        (strips, links)
    }
}

/// What the strip soak runs per mode.
#[derive(Clone, Copy)]
pub struct Plan {
    /// Length of the live run.
    pub live: Duration,
    /// Length of each synthetic run.
    pub synthetic: Duration,
    /// Blobs per frame the synthetic runs aim at.
    pub blob_targets: &'static [u16],
}

#[derive(Clone, Copy)]
enum Workload {
    Live,
    Synthetic {
        target: u16,
        bands: [u16; BANDS],
        per_frame: u32,
    },
}

/// One run's counters, cycle figures already converted to microseconds.
#[derive(Clone, Copy)]
struct RunResult {
    mode: &'static str,
    workload: Workload,
    stats: StripStats,
    timed_out: bool,
    /// Strips handed out during the run.
    strips: u32,
    /// Frames the detector finished (row 0 to the last strip).
    frames: u32,
    /// Frames begun but not finished (a row-0 strip came first).
    abandoned: u32,
    /// Strips not used: not part of a frame that began during the run.
    orphans: u32,
    proc_min_us: u32,
    proc_mean_us: u32,
    proc_max_us: u32,
    over_budget: u32,
    latency_min_us: u32,
    latency_mean_us: u32,
    latency_max_us: u32,
    hold_max_us: u32,
    /// First row of the strip held longest.
    hold_max_row: u16,
    /// Strip queued by the DMA handler until the detector has consumed it (`push_rows`, and
    /// `finish` for the last strip), over every strip fed to the detector.
    consumed_mean_us: u32,
    consumed_max_us: u32,
    /// Last strip of the frame queued until `finish` returns: centroids ready after the end
    /// of the frame.
    ready_min_us: u32,
    ready_mean_us: u32,
    ready_max_us: u32,
    blobs_min: u32,
    blobs_max: u32,
    overflow: u32,
    /// Finished frames per second, in millihertz.
    rate_mhz: u64,
    mhz: u32,
}

/// Per-run accumulation in CPU cycles.
struct Acc {
    strips: u32,
    frames: u32,
    abandoned: u32,
    orphans: u32,
    /// Frame being assembled: its number, the cycle count its first strip was handed out,
    /// and the detector cycles so far.
    open: Option<(u32, u32, u32)>,
    proc_min: u32,
    proc_max: u32,
    proc_total: u64,
    over_budget: u32,
    latency_min: u32,
    latency_max: u32,
    latency_total: u64,
    /// When the strip now held was handed out, and its first row.
    held_since: Option<(u32, u16)>,
    hold_max: u32,
    hold_max_row: u16,
    consumed_max: u32,
    consumed_total: u64,
    consumed_n: u32,
    ready_min: u32,
    ready_max: u32,
    ready_total: u64,
    /// First and last `finish`, in us of the 64-bit time base (`mcycle` wraps after 10.7 s).
    first_finish: Option<u64>,
    last_finish: u64,
    blobs_min: u32,
    blobs_max: u32,
    overflow: u32,
}

impl Acc {
    const NEW: Self = Self {
        strips: 0,
        frames: 0,
        abandoned: 0,
        orphans: 0,
        open: None,
        proc_min: u32::MAX,
        proc_max: 0,
        proc_total: 0,
        over_budget: 0,
        latency_min: u32::MAX,
        latency_max: 0,
        latency_total: 0,
        held_since: None,
        hold_max: 0,
        hold_max_row: 0,
        consumed_max: 0,
        consumed_total: 0,
        consumed_n: 0,
        ready_min: u32::MAX,
        ready_max: 0,
        ready_total: 0,
        first_finish: None,
        last_finish: 0,
        blobs_min: u32::MAX,
        blobs_max: 0,
        overflow: 0,
    };
}

/// The detector, the synthetic strip and the results, all in internal RAM.
pub struct StripSoak {
    plan: Plan,
    det: &'static mut vision::DefaultDetector,
    out: &'static mut [vision::Blob; 256],
    synth: &'static mut synth::Synth,
    /// Synthetic strip: `width * LINES` bytes used, cache-line aligned.
    scratch: &'static mut [u8],
    results: &'static mut [Option<RunResult>; MAX_RUNS],
    acc: &'static mut Acc,
    n: usize,
    mhz: u32,
}

impl StripSoak {
    /// `scratch` (internal RAM, cache-line aligned) must hold a 1280-wide strip.
    pub fn new(plan: Plan, scratch: &'static mut [u8]) -> Option<Self> {
        if scratch.len() < STRIP_BYTES {
            return None;
        }
        let (det, out, synth) = take_detector();
        Some(Self {
            plan,
            det,
            out,
            synth,
            scratch,
            results: RESULTS.take(),
            acc: ACC.take(),
            n: 0,
            mhz: esp_hal::clock::cpu_clock().as_mhz(),
        })
    }

    /// Live run, then one synthetic run per blob target, for the streaming `mode`. `false` if
    /// no whole frame arrived (the caller prints the receiver state).
    pub async fn run_mode(&mut self, cs: &mut CsiStrips<'_>, mode: &Mode) -> bool {
        if !self.run(cs, mode, Workload::Live, self.plan.live).await {
            return false;
        }
        for &target in self.plan.blob_targets {
            let work = self.prepare_synthetic(mode, target);
            if !self.run(cs, mode, work, self.plan.synthetic).await {
                return false;
            }
        }
        true
    }

    /// One run. `false` if no whole frame arrived before it.
    async fn run(
        &mut self,
        cs: &mut CsiStrips<'_>,
        mode: &Mode,
        work: Workload,
        window: Duration,
    ) -> bool {
        if !sync(cs, self.mhz).await {
            return false;
        }
        cs.reset_stats();
        *self.acc = Acc::NEW;
        let mut timed_out = false;
        let mut strip: Option<Strip> = None;
        let end = Instant::now() + window;
        while Instant::now() < end {
            self.giving_back();
            let s = match with_timeout(STRIP_TIMEOUT, cs.wait_strip(strip.take())).await {
                Ok(Ok(s)) => s,
                _ => {
                    timed_out = true;
                    break;
                }
            };
            self.strip(&s, mode.width, work);
            strip = Some(s);
        }
        self.giving_back();
        drop(strip);
        self.record(cs.stats(), mode, work, timed_out);
        true
    }

    /// The held strip is about to go back: its hold time ends now.
    fn giving_back(&mut self) {
        if let Some((since, row)) = self.acc.held_since.take() {
            let held = mcycle().wrapping_sub(since);
            if held > self.acc.hold_max {
                self.acc.hold_max = held;
                self.acc.hold_max_row = row;
            }
        }
    }

    /// Feeds one handed-out strip to the detector (or the synthetic strip in its place).
    #[inline(never)]
    fn strip(&mut self, s: &Strip, width: u16, work: Workload) {
        let got = mcycle();
        let acc = &mut *self.acc;
        acc.held_since = Some((got, s.first_row()));
        acc.strips += 1;
        if s.first_row() == 0 {
            if acc.open.is_some() {
                acc.abandoned += 1;
            }
            let t0 = mcycle();
            self.det.begin_frame(width, THRESHOLD);
            acc.open = Some((s.frame(), got, mcycle().wrapping_sub(t0)));
        }
        let Some((frame, start, mut cycles)) = acc.open else {
            acc.orphans += 1;
            return;
        };
        if frame != s.frame() {
            acc.orphans += 1;
            return;
        }
        let src: &[u8] = match work {
            Workload::Live => s,
            Workload::Synthetic { .. } => {
                let rows = &self.scratch[..s.len()];
                flush_cache(rows);
                rows
            }
        };
        let t0 = mcycle();
        self.det.push_rows(src, s.first_row());
        let pushed = mcycle();
        cycles = cycles.wrapping_add(pushed.wrapping_sub(t0));
        if !s.is_last() {
            consumed(acc, pushed.wrapping_sub(s.queued_at_cycles()));
            acc.open = Some((frame, start, cycles));
            return;
        }
        let t0 = mcycle();
        let blobs = self.det.finish(self.out) as u32;
        let done = mcycle();
        cycles = cycles.wrapping_add(done.wrapping_sub(t0));
        let ready = done.wrapping_sub(s.queued_at_cycles());
        consumed(acc, ready);
        acc.ready_min = acc.ready_min.min(ready);
        acc.ready_max = acc.ready_max.max(ready);
        acc.ready_total += u64::from(ready);
        acc.open = None;
        let at = Instant::now().as_micros();
        frame_done(acc, cycles, done.wrapping_sub(start), at, blobs, self.mhz);
        acc.overflow = acc.overflow.wrapping_add(self.det.stats().overflow);
    }

    /// Builds the synthetic strip for `target` blobs per frame of `mode`.
    #[inline(never)]
    fn prepare_synthetic(&mut self, mode: &Mode, target: u16) -> Workload {
        let w = usize::from(mode.width);
        let (bands, per_frame) = band_counts(target, mode.height);
        let band_len = w * BAND_ROWS;
        for (j, &discs) in bands.iter().enumerate() {
            let band = &mut self.scratch[j * band_len..(j + 1) * band_len];
            self.synth
                .fill(band, w, BAND_ROWS, discs, SEED.wrapping_add(j as u32));
        }
        flush_cache(&self.scratch[..w * usize::from(LINES)]);
        let work = Workload::Synthetic {
            target,
            bands,
            per_frame,
        };
        log_workload(mode, &work);
        work
    }

    #[inline(never)]
    fn record(&mut self, stats: StripStats, mode: &Mode, work: Workload, timed_out: bool) {
        let Some(slot) = self.results.get_mut(self.n) else {
            return;
        };
        *slot = Some(finish(self.acc, stats, mode, work, timed_out, self.mhz));
        self.n += 1;
        if let Some(r) = slot {
            log_line(r);
        }
    }

    /// Prints every run as a table row.
    #[inline(never)]
    pub fn log_table(&self) {
        info!(
            "strip soak table (us; budget {BUDGET_US} us; PASS = no stall, overrun, truncated or skipped frame, no errors, max processing < budget):"
        );
        info!(
            "| mode | run | blobs/frame fed | frames complete | truncated | skipped | strips delivered | discarded | stalls | overruns | errors | proc min | mean | max | > budget | latency min | mean | max | hold max (row) | queued->consumed mean/max | frame end->centroids min/mean/max | ISR mean/max | invalidate mean/max | fps | blobs | overflow | verdict |"
        );
        for r in self.results.iter().flatten() {
            info!("{}", Row(r));
        }
        info!("strip soak done");
    }
}

/// Waits for the end of a whole frame, so a run starts at row 0 of the next one (and the
/// driver's start-up frames and any frame damaged between runs stay outside the run). The
/// last strip must have been queued less than [`FRESH_US`] ago: strips queued while the
/// previous run's results were logged (a ring of 8 holds a whole 640-wide frame) are drained
/// first, so the run does not start on a backlog.
async fn sync(cs: &mut CsiStrips<'_>, mhz: u32) -> bool {
    let end = Instant::now() + SYNC_TIMEOUT;
    let mut strip = None;
    while Instant::now() < end {
        match with_timeout(STRIP_TIMEOUT, cs.wait_strip(strip.take())).await {
            Ok(Ok(s))
                if s.is_last() && mcycle().wrapping_sub(s.queued_at_cycles()) < FRESH_US * mhz =>
            {
                return true;
            }
            Ok(Ok(s)) => strip = Some(s),
            _ => return false,
        }
    }
    false
}

/// Disc counts per 16-row band, and the blobs per frame they give: the frame has
/// `height / 64` full strips (all four bands) and a last strip of `height % 64` rows (the
/// first bands only). The closest total wins, then the most even split.
fn band_counts(target: u16, height: u16) -> ([u16; BANDS], u32) {
    let full = u32::from(height / LINES);
    let last_bands = usize::from(height % LINES) / BAND_ROWS;
    let mut best = ([0u16; BANDS], 0u32, u32::MAX, u16::MAX);
    let n = u32::from(MAX_PER_BAND) + 1;
    for code in 0..n.pow(BANDS as u32) {
        let mut c = [0u16; BANDS];
        let mut k = code;
        for v in &mut c {
            *v = (k % n) as u16;
            k /= n;
        }
        let sum: u32 = c.iter().map(|&v| u32::from(v)).sum();
        let tail: u32 = c[..last_bands].iter().map(|&v| u32::from(v)).sum();
        let total = full * sum + tail;
        let err = total.abs_diff(u32::from(target));
        let spread = c.iter().max().unwrap_or(&0) - c.iter().min().unwrap_or(&0);
        if (err, spread) < (best.2, best.3) {
            best = (c, total, err, spread);
        }
    }
    (best.0, best.1)
}

fn consumed(acc: &mut Acc, cycles: u32) {
    acc.consumed_max = acc.consumed_max.max(cycles);
    acc.consumed_total += u64::from(cycles);
    acc.consumed_n += 1;
}

fn frame_done(acc: &mut Acc, cycles: u32, latency: u32, done_us: u64, blobs: u32, mhz: u32) {
    acc.frames += 1;
    acc.proc_min = acc.proc_min.min(cycles);
    acc.proc_max = acc.proc_max.max(cycles);
    acc.proc_total += u64::from(cycles);
    if cycles / mhz >= BUDGET_US {
        acc.over_budget += 1;
    }
    acc.latency_min = acc.latency_min.min(latency);
    acc.latency_max = acc.latency_max.max(latency);
    acc.latency_total += u64::from(latency);
    if acc.first_finish.is_none() {
        acc.first_finish = Some(done_us);
    }
    acc.last_finish = done_us;
    acc.blobs_min = acc.blobs_min.min(blobs);
    acc.blobs_max = acc.blobs_max.max(blobs);
}

#[inline(never)]
fn finish(
    acc: &Acc,
    stats: StripStats,
    mode: &Mode,
    workload: Workload,
    timed_out: bool,
    mhz: u32,
) -> RunResult {
    let frames = u64::from(acc.frames.max(1));
    let to_us = |c: u32| if c == u32::MAX { 0 } else { c / mhz };
    let span = acc.first_finish.map_or(0, |first| acc.last_finish - first);
    RunResult {
        mode: mode.name,
        workload,
        stats,
        timed_out,
        strips: acc.strips,
        frames: acc.frames,
        abandoned: acc.abandoned,
        orphans: acc.orphans,
        proc_min_us: to_us(acc.proc_min),
        proc_mean_us: (acc.proc_total / frames) as u32 / mhz,
        proc_max_us: acc.proc_max / mhz,
        over_budget: acc.over_budget,
        latency_min_us: to_us(acc.latency_min),
        latency_mean_us: (acc.latency_total / frames) as u32 / mhz,
        latency_max_us: acc.latency_max / mhz,
        hold_max_us: acc.hold_max / mhz,
        hold_max_row: acc.hold_max_row,
        consumed_mean_us: (acc.consumed_total / u64::from(acc.consumed_n.max(1))) as u32 / mhz,
        consumed_max_us: acc.consumed_max / mhz,
        ready_min_us: to_us(acc.ready_min),
        ready_mean_us: (acc.ready_total / frames) as u32 / mhz,
        ready_max_us: acc.ready_max / mhz,
        blobs_min: if acc.frames == 0 { 0 } else { acc.blobs_min },
        blobs_max: acc.blobs_max,
        overflow: acc.overflow,
        rate_mhz: u64::from(acc.frames.saturating_sub(1)) * 1_000_000_000 / span.max(1),
        mhz,
    }
}

/// The driver's DMA handler and invalidation times: mean and max in us (one decimal).
struct DriverTimes<'a>(&'a RunResult);

impl core::fmt::Display for DriverTimes<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = &self.0.stats;
        let tenths = |c: u64| c * 10 / u64::from(self.0.mhz);
        let isr = tenths(s.isr_cycles_total / u64::from(s.isr_runs.max(1)));
        let isr_max = tenths(u64::from(s.isr_cycles_max));
        let inv = tenths(s.invalidate_cycles_total / u64::from(s.strips_delivered.max(1)));
        let inv_max = tenths(u64::from(s.invalidate_cycles_max));
        write!(
            f,
            "{}.{}/{}.{} | {}.{}/{}.{}",
            isr / 10,
            isr % 10,
            isr_max / 10,
            isr_max % 10,
            inv / 10,
            inv % 10,
            inv_max / 10,
            inv_max % 10
        )
    }
}

fn errors(s: &StripStats) -> u32 {
    s.dma_errors + s.host_errors + s.bridge_errors + s.length_errors + s.writeback_missing
}

/// PASS, or FAIL with the first reason.
fn verdict(r: &RunResult) -> &'static str {
    let s = &r.stats;
    if r.timed_out {
        "FAIL: strips stopped"
    } else if r.frames == 0 {
        "FAIL: no frames"
    } else if s.dma_stalls != 0 {
        "FAIL: DMA stall"
    } else if s.bridge_overruns != 0 {
        "FAIL: bridge overrun"
    } else if s.frames_truncated != 0 || r.abandoned != 0 {
        "FAIL: truncated frame"
    } else if s.frames_skipped != 0 {
        "FAIL: skipped frame"
    } else if errors(s) != 0 {
        "FAIL: errors"
    } else if r.proc_max_us >= BUDGET_US {
        "FAIL: over budget"
    } else {
        "PASS"
    }
}

struct Work(Workload);

impl core::fmt::Display for Work {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Workload::Live => f.write_str("live"),
            Workload::Synthetic { target, .. } => write!(f, "synth {target}"),
        }
    }
}

/// Blobs per frame fed to the detector: the synthetic total, or "scene".
struct Fed(Workload);

impl core::fmt::Display for Fed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Workload::Live => f.write_str("scene"),
            Workload::Synthetic { per_frame, .. } => write!(f, "{per_frame}"),
        }
    }
}

#[inline(never)]
fn log_workload(mode: &Mode, work: &Workload) {
    if let Workload::Synthetic {
        target,
        bands,
        per_frame,
    } = *work
    {
        info!(
            "strip soak {} synth {target}: discs per 16-row band {bands:?}, {per_frame} blobs per frame ({} full strips + {} rows)",
            mode.name,
            mode.height / LINES,
            mode.height % LINES
        );
    }
}

#[inline(never)]
fn log_line(r: &RunResult) {
    info!("{}", Line(r));
}

/// One run as a table row (formatted in two parts to keep each `fmt` frame small).
struct Row<'a>(&'a RunResult);

impl core::fmt::Display for Row<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}{}", RowCounts(self.0), RowTimes(self.0))
    }
}

struct RowCounts<'a>(&'a RunResult);

impl core::fmt::Display for RowCounts<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let r = self.0;
        let s = &r.stats;
        write!(
            f,
            "| {} | {} | {} | {} | {} | {} | {} | {} ",
            r.mode,
            Work(r.workload),
            Fed(r.workload),
            s.frames_complete,
            s.frames_truncated,
            s.frames_skipped,
            s.strips_delivered,
            s.strips_discarded
        )?;
        write!(
            f,
            "| {} | {} | {} | {} | {} | {} | {} ",
            s.dma_stalls,
            s.bridge_overruns,
            errors(s),
            r.proc_min_us,
            r.proc_mean_us,
            r.proc_max_us,
            r.over_budget
        )
    }
}

struct RowTimes<'a>(&'a RunResult);

impl core::fmt::Display for RowTimes<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let r = self.0;
        write!(
            f,
            "| {} | {} | {} | {} ({}) | {}/{} | {}/{}/{} ",
            r.latency_min_us,
            r.latency_mean_us,
            r.latency_max_us,
            r.hold_max_us,
            r.hold_max_row,
            r.consumed_mean_us,
            r.consumed_max_us,
            r.ready_min_us,
            r.ready_mean_us,
            r.ready_max_us
        )?;
        write!(f, "| {} | {} ", DriverTimes(r), Fps(r.rate_mhz))?;
        write!(
            f,
            "| {}..{} | {} | {} |",
            r.blobs_min,
            r.blobs_max,
            r.overflow,
            verdict(r)
        )
    }
}

/// One run as a log line (formatted in parts to keep each `fmt` frame small).
struct Line<'a>(&'a RunResult);

impl core::fmt::Display for Line<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}{}{}", Counts(self.0), Faults(self.0), Times(self.0))
    }
}

struct Counts<'a>(&'a RunResult);

impl core::fmt::Display for Counts<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let r = self.0;
        let s = &r.stats;
        write!(
            f,
            "strip soak {} {}: frames complete {} truncated {} skipped {} (done {}), finished {}, abandoned {}, orphan strips {}, ",
            r.mode,
            Work(r.workload),
            s.frames_complete,
            s.frames_truncated,
            s.frames_skipped,
            s.frames_done,
            r.frames,
            r.abandoned,
            r.orphans
        )?;
        write!(
            f,
            "strips done {} delivered {} discarded {} handed out {}, ",
            s.strips_done, s.strips_delivered, s.strips_discarded, r.strips
        )
    }
}

struct Faults<'a>(&'a RunResult);

impl core::fmt::Display for Faults<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = &self.0.stats;
        write!(
            f,
            "stalls {}, overruns {}, errors length {} writeback {} dma {} ({:#x}) host {} ({:#x}) bridge {} ({:#x}), ",
            s.dma_stalls,
            s.bridge_overruns,
            s.length_errors,
            s.writeback_missing,
            s.dma_errors,
            s.dma_error_bits,
            s.host_errors,
            s.host_error_main,
            s.bridge_errors,
            s.bridge_error_bits
        )
    }
}

struct Times<'a>(&'a RunResult);

impl core::fmt::Display for Times<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let r = self.0;
        write!(
            f,
            "proc min/mean/max {}/{}/{} us, over budget {}, latency min/mean/max {}/{}/{} us, ",
            r.proc_min_us,
            r.proc_mean_us,
            r.proc_max_us,
            r.over_budget,
            r.latency_min_us,
            r.latency_mean_us,
            r.latency_max_us
        )?;
        write!(
            f,
            "hold max {} us (row {}), queued->consumed mean/max {}/{} us, frame end->centroids min/mean/max {}/{}/{} us, ",
            r.hold_max_us,
            r.hold_max_row,
            r.consumed_mean_us,
            r.consumed_max_us,
            r.ready_min_us,
            r.ready_mean_us,
            r.ready_max_us
        )?;
        write!(
            f,
            "ISR runs {} mean/max | invalidate mean/max (us) {}, {} fps, blobs {}..{} (fed {}), overflow {}, last block items {}: {}",
            r.stats.isr_runs,
            DriverTimes(r),
            Fps(r.rate_mhz),
            r.blobs_min,
            r.blobs_max,
            Fed(r.workload),
            r.overflow,
            r.stats.last_block_items,
            verdict(r)
        )
    }
}
