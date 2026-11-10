//! Live 120 fps soak: blob detection on every frame the CSI receiver hands out, on the same
//! executor (and core) that receives the frames.
//!
//! Per mode, with the sensor free-running and the three capture buffers in PSRAM:
//! - a live run: `process_frame` over every delivered frame, then the frame is given back;
//! - synthetic runs: frames are still waited for and given back at the sensor's rate, but the
//!   detector processes a `synth` frame of the mode's size, held in PSRAM outside the capture
//!   buffers and written back and invalidated from the caches before every pass, so each pass
//!   reads cold PSRAM while the CSI DMA writes the next frame.
//!
//! Nothing is logged while a run measures (`esp-println` masks interrupts per line, which can
//! delay the DMA buffer swap): per-frame figures go into counters, printed after the run.

use embassy_time::{Duration, Instant, with_timeout};
use esp32p4_csi::{Csi, Frame, Stats};
use log::info;
use static_cell::ConstStaticCell;

use crate::modes::CamMode;
use crate::{flush_cache, mcycle, synth, vision};

/// Foreground threshold.
pub(crate) const THRESHOLD: u8 = 128;
/// Frame period at 120 fps.
pub(crate) const BUDGET_US: u32 = 8333;
/// A run ends early if no frame arrives for this long.
const FRAME_TIMEOUT: Duration = Duration::from_millis(500);
/// Processing-time histogram: 1 ms bins, the last one holds everything from 12 ms up.
const HIST_BINS: usize = 13;
/// Synthetic frame seed (as `bench`).
const SEED: u32 = 0x1234_5678;
/// Result slots: 4 modes x (live + 3 synthetic).
const MAX_RUNS: usize = 16;

/// What the soak runs per mode.
#[derive(Clone, Copy)]
pub struct Plan {
    /// Length of the live run.
    pub live: Duration,
    /// Length of each synthetic run.
    pub synthetic: Duration,
    /// Disc counts of the synthetic runs.
    pub blob_counts: &'static [u16],
}

static DETECTOR: ConstStaticCell<vision::DefaultDetector> =
    ConstStaticCell::new(vision::DefaultDetector::new());
static BLOBS: ConstStaticCell<[vision::Blob; 256]> = ConstStaticCell::new(
    [vision::Blob {
        x: 0,
        y: 0,
        area: 0,
        peak: 0,
    }; 256],
);
static SYNTH: ConstStaticCell<synth::Synth> = ConstStaticCell::new(synth::Synth::NEW);
static RESULTS: ConstStaticCell<[Option<RunResult>; MAX_RUNS]> =
    ConstStaticCell::new([None; MAX_RUNS]);
static ACC: ConstStaticCell<Acc> = ConstStaticCell::new(Acc::NEW);

#[derive(Clone, Copy)]
enum Workload {
    Live,
    Synthetic(u16),
}

/// One run's counters, cycle figures already converted to microseconds.
#[derive(Clone, Copy)]
struct RunResult {
    mode: &'static str,
    workload: Workload,
    /// Frames handed out by `wait_frame` and processed.
    frames: u32,
    /// Driver counters over the run.
    stats: Stats,
    seq_gaps: u32,
    seq_skipped: u32,
    wrong_size: u32,
    timed_out: bool,
    proc_min_us: u32,
    proc_mean_us: u32,
    proc_max_us: u32,
    over_budget: u32,
    interval_min_us: u32,
    interval_mean_us: u32,
    interval_max_us: u32,
    /// Processed frames per second, in millihertz.
    rate_mhz: u64,
    blobs_min: u32,
    blobs_max: u32,
    /// Sum of the detector's overflow counter over all frames.
    overflow: u32,
    hist: [u32; HIST_BINS],
}

/// Per-frame accumulation in CPU cycles.
struct Acc {
    frames: u32,
    proc_min: u32,
    proc_max: u32,
    proc_total: u64,
    interval_min: u32,
    interval_max: u32,
    interval_total: u64,
    over_budget: u32,
    prev_got: Option<u32>,
    prev_seq: u32,
    seq_gaps: u32,
    seq_skipped: u32,
    wrong_size: u32,
    blobs_min: u32,
    blobs_max: u32,
    overflow: u32,
    hist: [u32; HIST_BINS],
}

impl Acc {
    const NEW: Self = Self {
        frames: 0,
        proc_min: u32::MAX,
        proc_max: 0,
        proc_total: 0,
        interval_min: u32::MAX,
        interval_max: 0,
        interval_total: 0,
        over_budget: 0,
        prev_got: None,
        prev_seq: 0,
        seq_gaps: 0,
        seq_skipped: 0,
        wrong_size: 0,
        blobs_min: u32::MAX,
        blobs_max: 0,
        overflow: 0,
        hist: [0; HIST_BINS],
    };
}

/// What one frame contributes to a run.
struct Sample {
    /// Cycle count when `wait_frame` returned the frame.
    got: u32,
    /// Processing time in cycles.
    cycles: u32,
    blobs: u32,
    overflow: u32,
    seq: u32,
    size_ok: bool,
}

/// The detector, its buffers and the results, all in internal RAM except `scratch` (PSRAM).
pub struct Soak {
    plan: Plan,
    det: &'static mut vision::DefaultDetector,
    out: &'static mut [vision::Blob; 256],
    synth: &'static mut synth::Synth,
    /// Synthetic frame area in PSRAM, outside the capture buffers, cache-line aligned.
    scratch: &'static mut [u8],
    results: &'static mut [Option<RunResult>; MAX_RUNS],
    /// The current run's counters.
    acc: &'static mut Acc,
    n: usize,
    mhz: u32,
}

/// The detector, its blob output and the synthetic-frame generator, in internal RAM. Shared
/// by the frame soak and the strip soak (`strip_soak`); only one of them runs per boot, so
/// this may be called once.
pub(crate) fn take_detector() -> (
    &'static mut vision::DefaultDetector,
    &'static mut [vision::Blob; 256],
    &'static mut synth::Synth,
) {
    (DETECTOR.take(), BLOBS.take(), SYNTH.take())
}

impl Soak {
    /// `scratch` must hold a 1280x800 frame and start on a cache line.
    pub fn new(plan: Plan, scratch: &'static mut [u8]) -> Self {
        let (det, out, synth) = take_detector();
        Self {
            plan,
            det,
            out,
            synth,
            scratch,
            results: RESULTS.take(),
            acc: ACC.take(),
            n: 0,
            mhz: esp_hal::clock::cpu_clock().as_mhz(),
        }
    }

    /// Live run, then one synthetic run per blob count, for the streaming `mode`.
    pub async fn run_mode(&mut self, csi: &mut Csi<'static>, mode: &CamMode) {
        self.run(csi, mode, Workload::Live, self.plan.live).await;
        for &blobs in self.plan.blob_counts {
            self.run(csi, mode, Workload::Synthetic(blobs), self.plan.synthetic)
                .await;
        }
    }

    async fn run(
        &mut self,
        csi: &mut Csi<'static>,
        mode: &CamMode,
        work: Workload,
        window: Duration,
    ) {
        let (width, height) = (mode.mode.width(), mode.mode.height());
        let len = usize::from(width) * usize::from(height);
        if let Workload::Synthetic(blobs) = work {
            self.prepare_synthetic(mode, blobs);
        }
        // Drop what completed while the frame was built or the last lines were logged.
        let _ = with_timeout(FRAME_TIMEOUT, csi.wait_frame(None)).await;
        csi.reset_stats();
        *self.acc = Acc::NEW;

        let mut timed_out = false;
        let mut frame: Option<Frame> = None;
        let end = Instant::now() + window;
        while Instant::now() < end {
            let f = match with_timeout(FRAME_TIMEOUT, csi.wait_frame(frame.take())).await {
                Ok(Ok(f)) => f,
                _ => {
                    timed_out = true;
                    break;
                }
            };
            self.frame(&f, len, width, work);
            frame = Some(f);
        }
        drop(frame);
        self.record(csi.stats(), mode, work, timed_out);
    }

    /// Processes one delivered frame (or the synthetic one) and adds it to the counters.
    #[inline(never)]
    fn frame(&mut self, f: &Frame, len: usize, width: u16, work: Workload) {
        let got = mcycle();
        let src: &[u8] = match work {
            Workload::Live => f,
            Workload::Synthetic(_) => {
                let s = &self.scratch[..len];
                flush_cache(s);
                s
            }
        };
        let (blobs, cycles) = detect(self.det, self.out, src, width);
        let sample = Sample {
            got,
            cycles,
            blobs,
            overflow: self.det.stats().overflow,
            seq: f.seq(),
            size_ok: f.received_len() == len,
        };
        account(self.acc, &sample, self.mhz);
    }

    /// Stores the run's result and logs it.
    #[inline(never)]
    fn record(&mut self, stats: Stats, mode: &CamMode, work: Workload, timed_out: bool) {
        let Some(slot) = self.results.get_mut(self.n) else {
            return;
        };
        *slot = Some(finish(self.acc, stats, mode, work, timed_out, self.mhz));
        self.n += 1;
        if let Some(r) = slot {
            log_run(r);
        }
    }

    #[inline(never)]
    fn prepare_synthetic(&mut self, mode: &CamMode, blobs: u16) {
        let (w, h) = (
            usize::from(mode.mode.width()),
            usize::from(mode.mode.height()),
        );
        let frame = &mut self.scratch[..w * h];
        self.synth.fill(frame, w, h, blobs, SEED);
        flush_cache(frame);
    }

    /// Prints every run as a table row.
    #[inline(never)]
    pub fn log_table(&self) {
        info!(
            "soak table (us; budget {BUDGET_US} us; PASS = nothing dropped/overwritten/skipped, no errors, max < budget):"
        );
        info!(
            "| mode | run | frames | done | dropped | overwritten | seq gaps | wrong size | errors | proc min | mean | max | > budget | fps | blobs | overflow | verdict |"
        );
        for r in self.results.iter().flatten() {
            info!("{}", Row(r));
        }
        info!("soak done");
    }
}

/// Runs the detector over `src`; returns the blobs found and the cycles it took.
#[inline(never)]
fn detect(
    det: &mut vision::DefaultDetector,
    out: &mut [vision::Blob],
    src: &[u8],
    width: u16,
) -> (u32, u32) {
    let t0 = mcycle();
    let n = det.process_frame(src, width, THRESHOLD, out);
    (n as u32, mcycle().wrapping_sub(t0))
}

#[inline(never)]
fn account(acc: &mut Acc, s: &Sample, mhz: u32) {
    acc.frames += 1;
    acc.proc_min = acc.proc_min.min(s.cycles);
    acc.proc_max = acc.proc_max.max(s.cycles);
    acc.proc_total += u64::from(s.cycles);
    let us = s.cycles / mhz;
    if us >= BUDGET_US {
        acc.over_budget += 1;
    }
    acc.hist[((us / 1000) as usize).min(HIST_BINS - 1)] += 1;
    if let Some(prev) = acc.prev_got {
        let interval = s.got.wrapping_sub(prev);
        acc.interval_min = acc.interval_min.min(interval);
        acc.interval_max = acc.interval_max.max(interval);
        acc.interval_total += u64::from(interval);
    }
    acc.prev_got = Some(s.got);
    // Sequence numbers restart at 1 after `reset_stats`.
    if s.seq != acc.prev_seq.wrapping_add(1) {
        acc.seq_gaps += 1;
        acc.seq_skipped = acc
            .seq_skipped
            .wrapping_add(s.seq.wrapping_sub(acc.prev_seq).wrapping_sub(1));
    }
    acc.prev_seq = s.seq;
    if !s.size_ok {
        acc.wrong_size += 1;
    }
    acc.blobs_min = acc.blobs_min.min(s.blobs);
    acc.blobs_max = acc.blobs_max.max(s.blobs);
    acc.overflow = acc.overflow.wrapping_add(s.overflow);
}

#[inline(never)]
fn finish(
    acc: &Acc,
    stats: Stats,
    mode: &CamMode,
    workload: Workload,
    timed_out: bool,
    mhz: u32,
) -> RunResult {
    let frames = acc.frames.max(1);
    let intervals = acc.frames.saturating_sub(1).max(1);
    let to_us = |c: u32| if c == u32::MAX { 0 } else { c / mhz };
    RunResult {
        mode: mode.name,
        workload,
        frames: acc.frames,
        stats,
        seq_gaps: acc.seq_gaps,
        seq_skipped: acc.seq_skipped,
        wrong_size: acc.wrong_size,
        timed_out,
        proc_min_us: to_us(acc.proc_min),
        proc_mean_us: (acc.proc_total / u64::from(frames)) as u32 / mhz,
        proc_max_us: acc.proc_max / mhz,
        over_budget: acc.over_budget,
        interval_min_us: to_us(acc.interval_min),
        interval_mean_us: (acc.interval_total / u64::from(intervals)) as u32 / mhz,
        interval_max_us: acc.interval_max / mhz,
        rate_mhz: u64::from(acc.frames.saturating_sub(1)) * u64::from(mhz) * 1_000_000_000
            / acc.interval_total.max(1),
        blobs_min: if acc.frames == 0 { 0 } else { acc.blobs_min },
        blobs_max: acc.blobs_max,
        overflow: acc.overflow,
        hist: acc.hist,
    }
}

fn errors(s: &Stats) -> u32 {
    s.dma_errors + s.host_errors + s.bridge_errors
}

/// PASS, or FAIL with the first reason.
fn verdict(r: &RunResult) -> (bool, &'static str) {
    let reason = if r.timed_out {
        "FAIL: frames stopped"
    } else if r.frames == 0 {
        "FAIL: no frames"
    } else if r.stats.frames_dropped != 0 {
        "FAIL: dropped"
    } else if r.stats.frames_overwritten != 0 {
        "FAIL: overwritten"
    } else if r.seq_gaps != 0 {
        "FAIL: seq gaps"
    } else if r.wrong_size != 0 {
        "FAIL: wrong size"
    } else if errors(&r.stats) != 0 {
        "FAIL: errors"
    } else if r.proc_max_us >= BUDGET_US {
        "FAIL: over budget"
    } else {
        return (true, "PASS");
    };
    (false, reason)
}

/// Millihertz as `x.yyy`.
pub(crate) struct Fps(pub(crate) u64);

impl core::fmt::Display for Fps {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{:03}", self.0 / 1000, self.0 % 1000)
    }
}

struct Work(Workload);

impl core::fmt::Display for Work {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Workload::Live => f.write_str("live"),
            Workload::Synthetic(n) => write!(f, "synth {n}"),
        }
    }
}

/// One run as a table row.
struct Row<'a>(&'a RunResult);

impl core::fmt::Display for Row<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let r = self.0;
        write!(
            f,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} ",
            r.mode,
            Work(r.workload),
            r.frames,
            r.stats.frames_done,
            r.stats.frames_dropped,
            r.stats.frames_overwritten,
            r.seq_gaps,
            r.wrong_size,
            errors(&r.stats)
        )?;
        write!(
            f,
            "| {} | {} | {} | {} | {} ",
            r.proc_min_us,
            r.proc_mean_us,
            r.proc_max_us,
            r.over_budget,
            Fps(r.rate_mhz)
        )?;
        write!(
            f,
            "| {}..{} | {} | {} |",
            r.blobs_min,
            r.blobs_max,
            r.overflow,
            Verdict(r)
        )
    }
}

/// One run as a log line.
struct Line<'a>(&'a RunResult);

impl core::fmt::Display for Line<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let r = self.0;
        write!(
            f,
            "soak {} {}: frames {} (done {}), dropped {}, overwritten {}, seq gaps {} (skipped {}), wrong size {}, ",
            r.mode,
            Work(r.workload),
            r.frames,
            r.stats.frames_done,
            r.stats.frames_dropped,
            r.stats.frames_overwritten,
            r.seq_gaps,
            r.seq_skipped,
            r.wrong_size
        )?;
        write!(
            f,
            "errors dma {} host {} bridge {}, proc min/mean/max {}/{}/{} us, over budget {}, ",
            r.stats.dma_errors,
            r.stats.host_errors,
            r.stats.bridge_errors,
            r.proc_min_us,
            r.proc_mean_us,
            r.proc_max_us,
            r.over_budget
        )?;
        write!(
            f,
            "interval min/mean/max {}/{}/{} us, {} fps processed, ",
            r.interval_min_us,
            r.interval_mean_us,
            r.interval_max_us,
            Fps(r.rate_mhz)
        )?;
        write!(
            f,
            "blobs {}..{}, overflow {}: {}",
            r.blobs_min,
            r.blobs_max,
            r.overflow,
            Verdict(r)
        )
    }
}

/// PASS, or FAIL with the first reason.
struct Verdict<'a>(&'a RunResult);

impl core::fmt::Display for Verdict<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(verdict(self.0).1)
    }
}

#[inline(never)]
fn log_run(r: &RunResult) {
    info!("{}", Line(r));
    info!(
        "soak {} {}: processing histogram, 1 ms bins from 0 (last >= 12 ms): {:?}",
        r.mode,
        Work(r.workload),
        r.hist
    );
}
