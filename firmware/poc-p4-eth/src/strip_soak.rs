//! Strip soak: blob detection on strips of lines that the CSI DMA writes into a ring of
//! internal-RAM buffers (`esp32p4_csi::CsiStrips`), on the executor that receives them. RAW8
//! and RAW10 (CSI-2 packed) modes; the strips are 64 lines, or 128 where the format needs it
//! (RAW10 at 640 wide, [`lines`]).
//!
//! Per mode, with the sensor free-running:
//! - a live run: every strip from `wait_strip` goes through `begin_frame` or
//!   `begin_frame_raw10` (row 0), `push_rows` and `finish` (last strip), then is given back;
//! - synthetic runs: strips are still waited for and given back at the sensor's rate, but
//!   `push_rows` reads a synthetic strip held in internal RAM instead, written back and
//!   invalidated from the cache before every strip (as `wait_strip` does for a real one).
//!
//! The synthetic strip is made of 16-row bands, each filled by `synth::Synth` (`fill` or
//! `fill_raw10`) with its own disc count: four distinct bands, repeated in a strip of more than
//! 64 rows. Every frame height of the modes is a multiple of 16 and every last strip is 16 or
//! 32 rows, so each strip of a frame (including the short last one, which reads the first
//! bands only) holds whole discs and nothing else. The band counts are searched so the blobs
//! per frame come as close to the target as the frame geometry allows; the exact figure is
//! logged before each run. The budget per frame is the mode's frame period, 1 s / fps.
//!
//! [`StripCheck`] (instead of the soak) checks that the strips carry the bytes of whole
//! frames: per mode, row checksums of the sensor's random test pattern from a whole-frame
//! capture, then every row of every strip, and the strip sequence, against them.
//!
//! Nothing is logged while a run measures. The receiver runs only while a run measures: the
//! caller starts it (and the sensor) for each run and stops both before the results are
//! logged or the next run is prepared. While the firmware does anything else the DMA would
//! fill the ring and halt (a stall). In RAW8 that costs about 4 frames; in RAW10 a halt of
//! about 30 ms (right after stream-on, ring of 2 or 3 strips) corrupted memory and crashed the
//! board (load access faults in the driver's DMA handler and the executor, the faulting words
//! image data; how the memory is hit is not known, see the `esp32p4_csi::strips` docs), so a
//! RAW10 stall must be avoided, not just counted.

use embassy_time::{Duration, Instant, with_timeout};
use esp32p4_csi::{Config, CsiStrips, Strip, StripLink, StripStats};
use log::info;
use static_cell::ConstStaticCell;

use crate::ov9281::{self, Mode};
use crate::soak::{Fps, THRESHOLD, take_detector};
use crate::{flush_cache, mcycle, synth, vision};

/// Rows per strip unless the format needs more ([`lines`]). 64 is not a divisor of any mode
/// height here (the driver needs a short last strip).
const MIN_LINES: u16 = 64;
/// Bytes of the largest strip: 64 lines of 1280 RAW10 (1600 B per line), or 128 of 640 RAW10.
const MAX_STRIP_BYTES: usize = 1600 * MIN_LINES as usize;
/// The ring's memory (320 KiB): 4 strips of 1280 RAW8, 8 of 640 RAW8, 3 of RAW10.
const RING_BYTES: usize = 4 * 1280 * MIN_LINES as usize;
/// Most strips in the ring (list items).
const MAX_STRIPS: usize = 8;
/// Rows per synthetic band.
const BAND_ROWS: usize = 16;
/// Distinct bands of the synthetic strip (64 rows); a longer strip repeats them.
const BANDS: usize = 4;
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
/// Foreground threshold of RAW10 runs in 10-bit units: the RAW8 [`THRESHOLD`] times 4.
const THRESHOLD_RAW10: u16 = 512;

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

/// Rows per strip for `config`: [`MIN_LINES`], or the smallest valid strip height if that is
/// more (whole 4096-byte bursts: 128 for RAW10 at 640 wide).
pub const fn lines(config: &Config) -> u16 {
    let step = CsiStrips::line_step(config);
    if step > MIN_LINES { step } else { MIN_LINES }
}

/// Bytes of one strip of [`lines`] rows for `config`. Modes with the same figure can share a
/// receiver and its ring split.
pub const fn strip_bytes(config: &Config) -> usize {
    CsiStrips::strip_bytes(config, lines(config))
}

/// Strips in the ring for `config`: as many as fit, at most 8 (8 at 640 RAW8, 4 at 1280
/// RAW8, 3 in the RAW10 modes).
pub const fn ring_strips(config: &Config) -> usize {
    let n = RING_BYTES / strip_bytes(config);
    if n < MAX_STRIPS { n } else { MAX_STRIPS }
}

/// The frame period of `mode` in us, 1 s / fps (8333 at 120 fps, 4762 at 210): the
/// processing budget per frame.
pub fn budget_us(mode: &Mode) -> u32 {
    let fps = u32::from(mode.fps.max(1));
    (1_000_000 + fps / 2) / fps
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
        let bytes = strip_bytes(config);
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
    /// Instead of the soak: the strip check ([`StripCheck`]), this long per mode, with the
    /// sensor's random test pattern. `live`, `synthetic` and `blob_targets` are then unused.
    pub check: Option<Duration>,
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
    /// The mode's frame period ([`budget_us`]).
    budget_us: u32,
    /// Rows per strip.
    lines: u16,
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
    /// Frame period of the mode ([`budget_us`]) and rows per strip, set when the run starts.
    budget_us: u32,
    lines: u16,
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
        budget_us: 0,
        lines: 0,
    };
}

/// The detector, the synthetic strip and the results, all in internal RAM.
pub struct StripSoak {
    plan: Plan,
    det: &'static mut vision::DefaultDetector,
    out: &'static mut [vision::Blob; 256],
    synth: &'static mut synth::Synth,
    /// Synthetic strip: one strip's bytes used, cache-line aligned.
    scratch: &'static mut [u8],
    results: &'static mut [Option<RunResult>; MAX_RUNS],
    acc: &'static mut Acc,
    n: usize,
    /// The run [`Self::prepare`] set up.
    work: Workload,
    mhz: u32,
}

impl StripSoak {
    /// `scratch` (internal RAM, cache-line aligned) must hold the largest strip (100 KiB).
    pub fn new(plan: Plan, scratch: &'static mut [u8]) -> Option<Self> {
        if scratch.len() < MAX_STRIP_BYTES {
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
            work: Workload::Live,
            mhz: esp_hal::clock::cpu_clock().as_mhz(),
        })
    }

    /// Runs per mode: the live run, then one per blob target.
    pub fn runs(&self) -> usize {
        1 + self.plan.blob_targets.len()
    }

    /// Sets up run `k` of `mode` (0 live, then the blob targets in order) for strips of
    /// `lines` rows, building the synthetic strip. Call it with the receiver stopped (see the
    /// module docs).
    pub fn prepare(&mut self, mode: &Mode, k: usize, lines: u16) {
        self.work = match k.checked_sub(1).map(|i| self.plan.blob_targets.get(i)) {
            Some(Some(&target)) => self.prepare_synthetic(mode, target, lines),
            _ => Workload::Live,
        };
    }

    /// The prepared run on the streaming `mode`. `false` if no whole frame arrived (the
    /// caller prints the receiver state). Logs nothing: [`Self::log_last`] prints the result.
    pub async fn run_mode(&mut self, cs: &mut CsiStrips<'_>, mode: &Mode) -> bool {
        let work = self.work;
        let window = match work {
            Workload::Live => self.plan.live,
            Workload::Synthetic { .. } => self.plan.synthetic,
        };
        if !sync(cs, self.mhz).await {
            return false;
        }
        cs.reset_stats();
        *self.acc = Acc::NEW;
        self.acc.budget_us = budget_us(mode);
        self.acc.lines = cs.strip_lines();
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
            self.strip(&s, mode, work);
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
    fn strip(&mut self, s: &Strip, mode: &Mode, work: Workload) {
        let got = mcycle();
        let acc = &mut *self.acc;
        acc.held_since = Some((got, s.first_row()));
        acc.strips += 1;
        if s.first_row() == 0 {
            if acc.open.is_some() {
                acc.abandoned += 1;
            }
            let t0 = mcycle();
            if mode.data_type == ov9281::DT_RAW10 {
                self.det.begin_frame_raw10(mode.width, THRESHOLD_RAW10);
            } else {
                self.det.begin_frame(mode.width, THRESHOLD);
            }
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

    /// Builds the synthetic strip of `lines` rows for `target` blobs per frame of `mode`.
    #[inline(never)]
    fn prepare_synthetic(&mut self, mode: &Mode, target: u16, lines: u16) -> Workload {
        let w = usize::from(mode.width);
        let raw10 = mode.data_type == ov9281::DT_RAW10;
        let (bands, per_frame) = band_counts(target, mode.height, lines);
        let band_len = mode.line_bytes() as usize * BAND_ROWS;
        for j in 0..usize::from(lines) / BAND_ROWS {
            let (discs, seed) = (bands[j % BANDS], SEED.wrapping_add((j % BANDS) as u32));
            let band = &mut self.scratch[j * band_len..(j + 1) * band_len];
            if raw10 {
                self.synth.fill_raw10(band, w, BAND_ROWS, discs, seed);
            } else {
                self.synth.fill(band, w, BAND_ROWS, discs, seed);
            }
        }
        flush_cache(&self.scratch[..band_len * usize::from(lines) / BAND_ROWS]);
        let work = Workload::Synthetic {
            target,
            bands,
            per_frame,
        };
        log_workload(mode, &work, lines);
        work
    }

    #[inline(never)]
    fn record(&mut self, stats: StripStats, mode: &Mode, work: Workload, timed_out: bool) {
        let Some(slot) = self.results.get_mut(self.n) else {
            return;
        };
        *slot = Some(finish(self.acc, stats, mode, work, timed_out, self.mhz));
        self.n += 1;
    }

    /// Prints the last run's result. Call it with the receiver stopped.
    #[inline(never)]
    pub fn log_last(&self) {
        if let Some(Some(r)) = self.n.checked_sub(1).and_then(|i| self.results.get(i)) {
            log_line(r);
        }
    }

    /// Prints every run as a table row.
    #[inline(never)]
    pub fn log_table(&self) {
        info!(
            "strip soak table (us; budget = the mode's frame period, 1 s / fps; PASS = no stall, overrun, truncated or skipped frame, no errors, max processing < budget):"
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

/// Disc counts of the four distinct 16-row bands, and the blobs per frame they give: the
/// frame has `height / lines` full strips and a last strip of `height % lines` rows (the
/// first bands only); band `j` of a strip has the counts of band `j % 4`. The closest total
/// wins, then the most even split.
fn band_counts(target: u16, height: u16, lines: u16) -> ([u16; BANDS], u32) {
    let full = u32::from(height / lines);
    let last_bands = usize::from(height % lines) / BAND_ROWS;
    // How often each distinct band occurs in a frame.
    let mut occurs = [0u32; BANDS];
    for j in 0..usize::from(lines) / BAND_ROWS {
        occurs[j % BANDS] += full + u32::from(j < last_bands);
    }
    let mut best = ([0u16; BANDS], 0u32, u32::MAX, u16::MAX);
    let n = u32::from(MAX_PER_BAND) + 1;
    for code in 0..n.pow(BANDS as u32) {
        let mut c = [0u16; BANDS];
        let mut k = code;
        for v in &mut c {
            *v = (k % n) as u16;
            k /= n;
        }
        let total: u32 = c.iter().zip(occurs).map(|(&v, o)| u32::from(v) * o).sum();
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
    if cycles / mhz >= acc.budget_us {
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
        budget_us: acc.budget_us,
        lines: acc.lines,
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
    } else if r.proc_max_us >= r.budget_us {
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
fn log_workload(mode: &Mode, work: &Workload, lines: u16) {
    if let Workload::Synthetic {
        target,
        bands,
        per_frame,
    } = *work
    {
        info!(
            "strip soak {} synth {target}: discs per 16-row band {bands:?} (repeated over a {lines}-row strip), {per_frame} blobs per frame ({} full strips + {} rows)",
            mode.name,
            mode.height / lines,
            mode.height % lines
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
            "{}-row strips, proc min/mean/max {}/{}/{} us, over budget ({} us) {}, latency min/mean/max {}/{}/{} us, ",
            r.lines,
            r.proc_min_us,
            r.proc_mean_us,
            r.proc_max_us,
            r.budget_us,
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

// ── Strip check ─────────────────────────────────────────────────────────────────────────

/// Modes per check plan.
const CHECK_MODES: usize = 4;
/// Rows of the tallest mode.
const CHECK_ROWS: usize = 800;
/// Words the check needs for its reference row checksums: [`CHECK_ROWS`] per mode.
pub const CHECK_WORDS: usize = CHECK_MODES * CHECK_ROWS;
/// Whole frames per mode for the reference: the first one is it, the others are compared
/// with it (the test pattern must be the same in every frame).
pub const REFERENCE_FRAMES: u32 = 8;
/// Start value of the frame checksum ([`fold_row`]).
const FOLD_SEED: u32 = 0x811C_9DC5;

/// Checksum of one row: two running sums over its little-endian 32-bit words (Fletcher
/// style, so the order of the words counts), then a tail of up to 3 bytes. Rows of both paths
/// start on a word boundary (frame buffers 4 KiB aligned, strip buffers 64 B, rows of a
/// multiple of 4 bytes); another start takes a slower path with the same result.
fn row_sum(row: &[u8]) -> u32 {
    let (mut a, mut b) = (0u32, 0u32);
    let mut add = |w: u32| {
        a = a.wrapping_add(w);
        b = b.wrapping_add(a);
    };
    // SAFETY: every bit pattern is a valid `u32`.
    let (pre, words, post) = unsafe { row.align_to::<u32>() };
    let tail = if pre.is_empty() {
        words.iter().for_each(|&w| add(u32::from_le(w)));
        post
    } else {
        let (words, tail) = row.as_chunks::<4>();
        words.iter().for_each(|w| add(u32::from_le_bytes(*w)));
        tail
    };
    tail.iter().for_each(|&x| add(u32::from(x)));
    a ^ b.rotate_left(16)
}

/// Frame checksum: the row checksums folded in row order.
fn fold_row(acc: u32, row: u32) -> u32 {
    (acc ^ row).wrapping_mul(0x0100_0193)
}

/// The whole-frame reference of one mode.
#[derive(Clone, Copy)]
struct Reference {
    name: &'static str,
    height: u16,
    line_bytes: usize,
    frame_bytes: usize,
    /// Whole frames checked (right size), and those whose every row equals the first one's.
    frames: u32,
    identical: u32,
    wrong_size: u32,
    /// Most rows of one frame that differ from the first frame.
    rows_differing_max: u32,
    /// Rows of the first frame whose checksum another of its rows also has.
    duplicate_rows: u32,
    frame_sum: u32,
    seq_first: u32,
    seq_last: u32,
    /// First bytes of rows 0 and 1 of the first frame.
    head: [[u8; 10]; 2],
}

/// A frame being checked: its number, the row its next strip must start at, and the
/// checksum so far.
#[derive(Clone, Copy)]
struct Open {
    frame: u32,
    next_row: u16,
    fold: u32,
}

/// One strip run's counters (cycles where not said otherwise).
#[derive(Clone, Copy)]
struct CheckAcc {
    strips: u32,
    /// Strips whose row count or length does not fit their position.
    size_errors: u32,
    /// Strips out of order: not at the row the frame is at, a frame begun before the last
    /// one ended, or `is_last` where the frame does not end (or not where it does).
    seq_errors: u32,
    /// Frame numbers missing between consecutive frames.
    frame_gaps: u32,
    /// Frames whose last strip arrived after all others in order.
    frames: u32,
    /// Those whose frame checksum equals the reference.
    matching: u32,
    rows_checked: u32,
    rows_bad: u32,
    /// Frame number and row of the first bad row.
    first_bad: Option<(u32, u16)>,
    open: Option<Open>,
    last_frame: Option<u32>,
    check_max: u32,
    held_since: Option<u32>,
    hold_max: u32,
    /// First and last frame end, in us of the 64-bit time base.
    first_finish: Option<u64>,
    last_finish: u64,
}

impl CheckAcc {
    const NEW: Self = Self {
        strips: 0,
        size_errors: 0,
        seq_errors: 0,
        frame_gaps: 0,
        frames: 0,
        matching: 0,
        rows_checked: 0,
        rows_bad: 0,
        first_bad: None,
        open: None,
        last_frame: None,
        check_max: 0,
        held_since: None,
        hold_max: 0,
        first_finish: None,
        last_finish: 0,
    };
}

#[derive(Clone, Copy)]
struct CheckResult {
    name: &'static str,
    height: u16,
    /// The reference's frame checksum.
    frame_sum: u32,
    lines: u16,
    stats: StripStats,
    timed_out: bool,
    acc: CheckAcc,
    mhz: u32,
}

/// Strips against whole frames, with the sensor's random test pattern (the same in every
/// frame, every row different). Per mode: [`Self::reference_frame`] on whole frames from a
/// `Csi` keeps the first frame's row checksums and compares the others with them; then,
/// with a `CsiStrips` (the two cannot exist at the same time), [`Self::run_mode`] checks
/// every strip for its size and place in the sequence and every row against the reference.
pub struct StripCheck {
    window: Duration,
    /// Reference row checksums, [`CHECK_ROWS`] per mode slot.
    rows: &'static mut [u32],
    refs: &'static mut [Option<Reference>; CHECK_MODES],
    results: &'static mut [Option<CheckResult>; CHECK_MODES],
    acc: &'static mut CheckAcc,
    mhz: u32,
}

static CHECK_REFS: ConstStaticCell<[Option<Reference>; CHECK_MODES]> =
    ConstStaticCell::new([None; CHECK_MODES]);
static CHECK_RESULTS: ConstStaticCell<[Option<CheckResult>; CHECK_MODES]> =
    ConstStaticCell::new([None; CHECK_MODES]);
static CHECK_ACC: ConstStaticCell<CheckAcc> = ConstStaticCell::new(CheckAcc::NEW);

impl StripCheck {
    /// `rows` (any memory, PSRAM is fine) must hold [`CHECK_WORDS`] words. A strip run lasts
    /// `window`. Call once.
    pub fn new(window: Duration, rows: &'static mut [u32]) -> Option<Self> {
        if rows.len() < CHECK_WORDS {
            return None;
        }
        Some(Self {
            window,
            rows,
            refs: CHECK_REFS.take(),
            results: CHECK_RESULTS.take(),
            acc: CHECK_ACC.take(),
            mhz: esp_hal::clock::cpu_clock().as_mhz(),
        })
    }

    /// Starts the reference of plan slot `slot` (at most [`CHECK_MODES`]) for `mode`.
    pub fn begin_reference(&mut self, slot: usize, mode: &Mode) {
        if let Some(r) = self.refs.get_mut(slot) {
            *r = Some(Reference {
                name: mode.name,
                height: mode.height,
                line_bytes: mode.line_bytes() as usize,
                frame_bytes: mode.frame_bytes() as usize,
                frames: 0,
                identical: 0,
                wrong_size: 0,
                rows_differing_max: 0,
                duplicate_rows: 0,
                frame_sum: 0,
                seq_first: 0,
                seq_last: 0,
                head: [[0; 10]; 2],
            });
        }
    }

    /// One whole frame of slot `slot`'s mode (`received` bytes, driver sequence `seq`): the
    /// first right-sized one becomes the reference, later ones are compared with it.
    #[inline(never)]
    pub fn reference_frame(&mut self, slot: usize, frame: &[u8], received: usize, seq: u32) {
        let Some(Some(r)) = self.refs.get_mut(slot) else {
            return;
        };
        let height = usize::from(r.height);
        if received != r.frame_bytes || frame.len() < r.frame_bytes || height > CHECK_ROWS {
            r.wrong_size += 1;
            return;
        }
        let want = &mut self.rows[slot * CHECK_ROWS..][..height];
        let lines = frame[..r.frame_bytes].chunks_exact(r.line_bytes);
        if r.frames == 0 {
            let mut fold = FOLD_SEED;
            for (w, row) in want.iter_mut().zip(lines) {
                *w = row_sum(row);
                fold = fold_row(fold, *w);
            }
            r.frame_sum = fold;
            r.seq_first = seq;
            for (h, row) in r.head.iter_mut().zip(frame.chunks_exact(r.line_bytes)) {
                h.copy_from_slice(&row[..10]);
            }
            r.duplicate_rows = want
                .iter()
                .enumerate()
                .filter(|&(i, w)| want.iter().enumerate().any(|(j, v)| j != i && v == w))
                .count() as u32;
            r.identical = 1;
        } else {
            let differing = want
                .iter()
                .zip(lines)
                .filter(|&(&w, row)| row_sum(row) != w)
                .count() as u32;
            r.rows_differing_max = r.rows_differing_max.max(differing);
            if differing == 0 {
                r.identical += 1;
            }
        }
        r.seq_last = seq;
        r.frames += 1;
    }

    /// Prints slot `slot`'s reference.
    #[inline(never)]
    pub fn log_reference(&self, slot: usize) {
        if let Some(Some(r)) = self.refs.get(slot) {
            info!("{}", RefLine(r));
        }
    }

    /// The strip run for the streaming `mode` of plan slot `slot` (nothing without a
    /// reference). `false` if no whole frame arrived (the caller prints the receiver state).
    /// Logs nothing: [`Self::log_run`] prints the result.
    pub async fn run_mode(&mut self, cs: &mut CsiStrips<'_>, slot: usize) -> bool {
        if !matches!(self.refs.get(slot), Some(Some(r)) if r.frames > 0) {
            return true;
        }
        if !sync(cs, self.mhz).await {
            return false;
        }
        let lines = cs.strip_lines();
        cs.reset_stats();
        *self.acc = CheckAcc::NEW;
        let mut timed_out = false;
        let mut strip: Option<Strip> = None;
        let end = Instant::now() + self.window;
        while Instant::now() < end {
            gave_back(self.acc);
            let s = match with_timeout(STRIP_TIMEOUT, cs.wait_strip(strip.take())).await {
                Ok(Ok(s)) => s,
                _ => {
                    timed_out = true;
                    break;
                }
            };
            self.check(&s, slot, lines);
            strip = Some(s);
        }
        gave_back(self.acc);
        drop(strip);
        self.record(slot, lines, cs.stats(), timed_out);
        true
    }

    #[inline(never)]
    fn check(&mut self, s: &Strip, slot: usize, lines: u16) {
        if let Some(Some(r)) = self.refs.get(slot) {
            let want = &self.rows[slot * CHECK_ROWS..][..usize::from(r.height)];
            check_strip(self.acc, s, r, want, lines);
        }
    }

    #[inline(never)]
    fn record(&mut self, slot: usize, lines: u16, stats: StripStats, timed_out: bool) {
        let (Some(Some(r)), Some(out)) = (self.refs.get(slot), self.results.get_mut(slot)) else {
            return;
        };
        let _ = out.insert(CheckResult {
            name: r.name,
            height: r.height,
            frame_sum: r.frame_sum,
            lines,
            stats,
            timed_out,
            acc: *self.acc,
            mhz: self.mhz,
        });
    }

    /// Prints the strip run of slot `slot` (for `mode`). Call it with the receiver stopped.
    #[inline(never)]
    pub fn log_run(&self, slot: usize, mode: &Mode) {
        match self.results.get(slot) {
            Some(Some(r)) => info!("strip check {}", CheckLine(r)),
            _ => info!(
                "strip check {}: no reference frame, strips not checked",
                mode.name
            ),
        }
    }

    /// Prints the references and the strip runs as tables.
    pub fn log_table(&self) {
        self.log_references();
        self.log_runs();
    }

    #[inline(never)]
    fn log_references(&self) {
        info!(
            "strip check references (whole frames, random test pattern 0x5E00 = 0x81; PASS = every frame right-sized and identical to the first):"
        );
        info!(
            "| mode | bytes/frame | frames | wrong size | identical to the first | most rows differing | rows sharing a checksum | frame checksum | verdict |"
        );
        for r in self.refs.iter().flatten() {
            info!("{}", RefRow(r));
        }
    }

    #[inline(never)]
    fn log_runs(&self) {
        info!(
            "strip check table (us; PASS = right sizes and sequence, every row and frame equal to the reference, no stall, overrun, truncated or skipped frame, gap or error):"
        );
        info!(
            "| mode | lines/strip | strips/frame | strips checked | size errors | sequence errors | frames complete | frames matching | rows checked | bad rows (first frame/row) | frame gaps | stalls | overruns | truncated | skipped | errors | check max/strip | hold max | fps | verdict |"
        );
        for r in self.results.iter().flatten() {
            info!("{}", CheckRow(r));
        }
        info!("strip check done");
    }
}

/// The held strip is about to go back: its hold time ends now.
fn gave_back(acc: &mut CheckAcc) {
    if let Some(since) = acc.held_since.take() {
        acc.hold_max = acc.hold_max.max(mcycle().wrapping_sub(since));
    }
}

/// Checks one strip's size, its place in the sequence and every row against `want` (the
/// reference row checksums).
#[inline(never)]
fn check_strip(acc: &mut CheckAcc, s: &Strip, r: &Reference, want: &[u32], lines: u16) {
    let t0 = mcycle();
    acc.held_since = Some(t0);
    acc.strips += 1;
    let (first, rows) = (s.first_row(), s.rows());
    let end = first.saturating_add(rows);
    let expect = r.height.saturating_sub(first).min(lines);
    let size_ok = rows == expect && rows > 0 && s.len() == usize::from(rows) * r.line_bytes;
    if !size_ok {
        acc.size_errors += 1;
    }
    if s.is_last() != (end == r.height) {
        acc.seq_errors += 1;
    }
    if first == 0 {
        if acc.open.is_some() {
            acc.seq_errors += 1;
        }
        if let Some(prev) = acc.last_frame {
            acc.frame_gaps += s.frame().wrapping_sub(prev).wrapping_sub(1);
        }
        acc.last_frame = Some(s.frame());
        acc.open = Some(Open {
            frame: s.frame(),
            next_row: 0,
            fold: FOLD_SEED,
        });
    }
    match acc.open {
        Some(mut o) if o.frame == s.frame() && o.next_row == first => {
            if size_ok {
                for (i, row) in s.chunks_exact(r.line_bytes).enumerate() {
                    let sum = row_sum(row);
                    let at = usize::from(first) + i;
                    if want.get(at) != Some(&sum) {
                        acc.rows_bad += 1;
                        acc.first_bad.get_or_insert((s.frame(), at as u16));
                    }
                    o.fold = fold_row(o.fold, sum);
                }
                acc.rows_checked += u32::from(rows);
            }
            o.next_row = end;
            if s.is_last() {
                acc.frames += 1;
                if o.fold == r.frame_sum {
                    acc.matching += 1;
                }
                let at = Instant::now().as_micros();
                acc.first_finish.get_or_insert(at);
                acc.last_finish = at;
                acc.open = None;
            } else {
                acc.open = Some(o);
            }
        }
        _ => {
            acc.seq_errors += 1;
            acc.open = None;
        }
    }
    acc.check_max = acc.check_max.max(mcycle().wrapping_sub(t0));
}

fn ref_verdict(r: &Reference) -> &'static str {
    if r.frames != REFERENCE_FRAMES || r.wrong_size != 0 {
        "FAIL: frames missing or wrong size"
    } else if r.identical != r.frames {
        "FAIL: frames differ"
    } else {
        "PASS"
    }
}

fn check_verdict(c: &CheckResult) -> &'static str {
    let (a, s) = (&c.acc, &c.stats);
    if c.timed_out {
        "FAIL: strips stopped"
    } else if a.frames == 0 {
        "FAIL: no frames"
    } else if a.size_errors != 0 {
        "FAIL: strip size"
    } else if a.seq_errors != 0 {
        "FAIL: sequence"
    } else if a.rows_bad != 0 || a.matching != a.frames {
        "FAIL: content"
    } else if s.dma_stalls != 0 {
        "FAIL: DMA stall"
    } else if s.bridge_overruns != 0 {
        "FAIL: bridge overrun"
    } else if s.frames_truncated != 0 {
        "FAIL: truncated frame"
    } else if s.frames_skipped != 0 || a.frame_gaps != 0 {
        "FAIL: skipped frame"
    } else if errors(s) != 0 {
        "FAIL: errors"
    } else {
        "PASS"
    }
}

/// The first bad row as `frame/row`, or `-`.
struct FirstBad(Option<(u32, u16)>);

impl core::fmt::Display for FirstBad {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some((frame, row)) => write!(f, "{frame}/{row}"),
            None => f.write_str("-"),
        }
    }
}

struct RefLine<'a>(&'a Reference);

impl core::fmt::Display for RefLine<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let r = self.0;
        write!(
            f,
            "strip check {}: reference: {} whole frames of {} B (wrong size {}), seq {}..{}, identical to the first {}/{}, most rows differing {}, ",
            r.name,
            r.frames,
            r.frame_bytes,
            r.wrong_size,
            r.seq_first,
            r.seq_last,
            r.identical,
            r.frames,
            r.rows_differing_max
        )?;
        write!(
            f,
            "rows sharing a checksum {} of {}, frame checksum {:#010x}, row 0 head {:02x?}, row 1 head {:02x?}: {}",
            r.duplicate_rows,
            r.height,
            r.frame_sum,
            r.head[0],
            r.head[1],
            ref_verdict(r)
        )
    }
}

struct RefRow<'a>(&'a Reference);

impl core::fmt::Display for RefRow<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let r = self.0;
        write!(
            f,
            "| {} | {} | {} | {} | {}/{} | {} | {} | {:#010x} | {} |",
            r.name,
            r.frame_bytes,
            r.frames,
            r.wrong_size,
            r.identical,
            r.frames,
            r.rows_differing_max,
            r.duplicate_rows,
            r.frame_sum,
            ref_verdict(r)
        )
    }
}

/// Finished frames per second in millihertz.
fn check_rate(a: &CheckAcc) -> u64 {
    let span = a.first_finish.map_or(0, |first| a.last_finish - first);
    u64::from(a.frames.saturating_sub(1)) * 1_000_000_000 / span.max(1)
}

/// One strip run as a log line (formatted in parts to keep each `fmt` frame small).
struct CheckLine<'a>(&'a CheckResult);

impl core::fmt::Display for CheckLine<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}{}{}",
            CheckLineCounts(self.0),
            CheckLineDriver(self.0),
            CheckLineTimes(self.0)
        )
    }
}

struct CheckLineCounts<'a>(&'a CheckResult);

impl core::fmt::Display for CheckLineCounts<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let c = self.0;
        let a = &c.acc;
        write!(
            f,
            "{}: {}-row strips, {} strips: size errors {}, sequence errors {}, frames complete {} matching the reference {} (checksum {:#010x}), rows checked {} bad {} (first {}), frame gaps {}, ",
            c.name,
            c.lines,
            a.strips,
            a.size_errors,
            a.seq_errors,
            a.frames,
            a.matching,
            c.frame_sum,
            a.rows_checked,
            a.rows_bad,
            FirstBad(a.first_bad),
            a.frame_gaps
        )
    }
}

struct CheckLineDriver<'a>(&'a CheckResult);

impl core::fmt::Display for CheckLineDriver<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = &self.0.stats;
        write!(
            f,
            "driver: frames complete {} truncated {} skipped {}, strips delivered {} discarded {}, stalls {}, overruns {}, errors {} (dma {:#x} host {:#x} bridge {:#x}), ",
            s.frames_complete,
            s.frames_truncated,
            s.frames_skipped,
            s.strips_delivered,
            s.strips_discarded,
            s.dma_stalls,
            s.bridge_overruns,
            errors(s),
            s.dma_error_bits,
            s.host_error_main,
            s.bridge_error_bits
        )
    }
}

struct CheckLineTimes<'a>(&'a CheckResult);

impl core::fmt::Display for CheckLineTimes<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let c = self.0;
        let a = &c.acc;
        write!(
            f,
            "check max {} us per strip, hold max {} us, {} fps: {}",
            a.check_max / c.mhz,
            a.hold_max / c.mhz,
            Fps(check_rate(a)),
            check_verdict(c)
        )
    }
}

/// One strip run as a table row (formatted in two parts to keep each `fmt` frame small).
struct CheckRow<'a>(&'a CheckResult);

impl core::fmt::Display for CheckRow<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}{}", CheckCounts(self.0), CheckTimes(self.0))
    }
}

struct CheckCounts<'a>(&'a CheckResult);

impl core::fmt::Display for CheckCounts<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let c = self.0;
        let a = &c.acc;
        write!(
            f,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} ({}) ",
            c.name,
            c.lines,
            c.height.div_ceil(c.lines.max(1)),
            a.strips,
            a.size_errors,
            a.seq_errors,
            a.frames,
            a.matching,
            a.rows_checked,
            a.rows_bad,
            FirstBad(a.first_bad)
        )
    }
}

struct CheckTimes<'a>(&'a CheckResult);

impl core::fmt::Display for CheckTimes<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let c = self.0;
        let (a, s) = (&c.acc, &c.stats);
        write!(
            f,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            a.frame_gaps,
            s.dma_stalls,
            s.bridge_overruns,
            s.frames_truncated,
            s.frames_skipped,
            errors(s),
            a.check_max / c.mhz,
            a.hold_max / c.mhz,
            Fps(check_rate(a)),
            check_verdict(c)
        )
    }
}
