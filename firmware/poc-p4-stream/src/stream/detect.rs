//! Blob detection in the stream (protocol version 6, `docs/protocol.md`, "Detection"): the
//! blob detector of `espion-vision` on a bare core 1 (`espion-core1`'s stream job) over the
//! strips of each frame, and core 0's side of it.
//!
//! - **Start** ([`start`], at boot): core 1 with the detector and its blob scratch in internal
//!   RAM, PIE on its own token, and DW-GDMA channel 1 for its strip copies.
//! - **Per frame on core 1** (the strip path runs the mode, `camera::strip_plan`): the
//!   detector and the background sample on each strip while the strip is copied into a frame
//!   buffer; after the last strip the kept blobs (area filter, at most `FRAME_BLOBS_MAX`)
//!   measured on the assembled frame (`espion_vision::refine`). Detection does not depend on a
//!   host, on "Send frames" or on `max_fps`. A frame core 1 could not copy (no free frame
//!   buffer) is detected but not measured: counted as skipped, no blobs sent. The modes the
//!   strip path cannot run stream whole frames without detection (`DetectState::available`
//!   false; the settings are kept).
//! - **Settings to core 1** ([`core1_settings`], the capture task, every millisecond): on or
//!   off, the threshold of the next frame, the area bounds, the detection serial.
//! - **Results** ([`Work::result`], the capture task, as it drains core 1's results): the
//!   area filter's counts, the threshold of the next frame (automatic: `espion_vision::auto`
//!   from the frame's background and the brightest blob kept; manual: the set one),
//!   `DetectState`, and the `FrameBlobs` datagram queued for the send task while a host polls
//!   ([`BLOBS_READY`]; the send task sends it after the answers and before the next frame
//!   chunk). Results of settings changed since (another serial) are not used.
//! - **Automatic exposure** (the detect task, every `AUTO_EXPOSURE_PERIOD_MS`):
//!   `cam::auto_exposure` on the highest peak of the blobs sent in the period; a change goes
//!   to the capture task as [`Cmd::Exposure`] (no restart).
//! - **Watch** (the detect task, once a second): core 1's trap record and heartbeat; on a
//!   fault, `DetectState::fault`, and the next start streams whole frames (core 1 also copies
//!   the strips, so a dead core 1 stops the strip path); the log says why. Then, while the
//!   strip path runs, three log lines with the second's figures (a log line masks interrupts
//!   on core 0, which the strip path does not mind: core 1 polls the strips), and every 10 s
//!   the stacks' unused bytes.
//! - **L1 data autoload** over each strip core 1's detector reads: on from boot, switched by
//!   the test hooks (`L0`/`L1`) for the next start ([`autoload_for_run`]); while it is on core 1
//!   owns the autoload registers (`crate::prefetch::lend_l1_autoload`).

use core::cell::{Cell, RefCell, UnsafeCell};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::peripherals::{CPU_CTRL, VDMA_CH1};
use esp32p4_csi::DmaCopy;
use espion_core1::spsc::{Consumer, Producer, Spsc};
use espion_core1::{
    self as core1, Core1, FrameResult, Health, MAX_BLOBS, NO_BUFFER, SkipChoice, StreamDetect,
};
use espion_proto::cam::{
    self, AUTO_EXPOSURE_PERIOD_MS, BLOB_BYTES, DetectTiming, FRAME_BLOBS_MAX, Format, FrameBlobs,
    MAX_DATAGRAM,
};
use espion_vision::{Blob, DefaultDetector, auto};
use log::{error, info, warn};
use static_cell::ConstStaticCell;

use super::{CMDS, Cmd, shared};
use crate::{cpu, prefetch};

/// `FrameBlobs` datagrams waiting for the send task.
const BLOBS_QUEUE: usize = 4;
/// Bytes of the blobs of one `FrameBlobs`.
const PACK_BYTES: usize = FRAME_BLOBS_MAX * BLOB_BYTES;
const _: () = assert!(FRAME_BLOBS_MAX <= espion_core1::MAX_SPOTS);

static DETECTOR: ConstStaticCell<DefaultDetector> = ConstStaticCell::new(DefaultDetector::new());
static SCRATCH: ConstStaticCell<[Blob; MAX_BLOBS]> = ConstStaticCell::new(
    [Blob {
        x: 0,
        y: 0,
        area: 0,
        peak: 0,
    }; MAX_BLOBS],
);
static PACK: ConstStaticCell<[u8; PACK_BYTES]> = ConstStaticCell::new([0; PACK_BYTES]);

/// One encoded `FrameBlobs` datagram for the send task.
pub(super) struct BlobsOut {
    pub len: usize,
    pub datagram: [u8; MAX_DATAGRAM],
}

/// The detect side's `FrameBlobs` datagrams, to the send task.
pub(super) type BlobsRx = Consumer<BlobsOut, BLOBS_QUEUE>;
pub(super) type BlobsTx = Producer<BlobsOut, BLOBS_QUEUE>;

static BLOBS: Spsc<BlobsOut, BLOBS_QUEUE> = Spsc::new(
    [const {
        UnsafeCell::new(BlobsOut {
            len: 0,
            datagram: [0; MAX_DATAGRAM],
        })
    }; BLOBS_QUEUE],
);

/// Raised after a datagram was queued in [`BLOBS`].
pub(super) static BLOBS_READY: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Bytes of internal RAM this side takes: core 1's detector and blob scratch, the packed
/// blobs, the datagram queue (core 1's stack and result queue are in `espion-core1`).
pub const RAM_BYTES: usize = core::mem::size_of::<DefaultDetector>()
    + core::mem::size_of::<[Blob; MAX_BLOBS]>()
    + PACK_BYTES
    + core::mem::size_of::<Spsc<BlobsOut, BLOBS_QUEUE>>();

/// The L1 data autoload over each strip core 1's detector reads: wanted (the test hooks'
/// `L0`/`L1`), applied at the next start.
pub(super) static AUTOLOAD_WANTED: AtomicBool = AtomicBool::new(true);

/// Highest peak of the blobs sent in the automatic exposure's period.
static AE_PEAK: Mutex<Cell<Option<u8>>> = Mutex::new(Cell::new(None));

/// The strip path's figures the capture task keeps for the log: its plan (rows, strips) and
/// strip period (µs), strips lapped (frame lost; and after an intact copy, frame kept),
/// results core 1 dropped (full queue); 0 rows: whole frames.
pub(super) static PLAN: Mutex<Cell<(u16, u8, u32)>> = Mutex::new(Cell::new((0, 0, 0)));
pub(super) static LAPPED: AtomicU32 = AtomicU32::new(0);
pub(super) static LAPPED_COPIED: AtomicU32 = AtomicU32::new(0);
pub(super) static DROPPED: AtomicU32 = AtomicU32::new(0);
/// Core 1's stack never used, bytes (the capture task reads it from the `Core1` handle).
pub(super) static CORE1_STACK_UNUSED: AtomicU32 = AtomicU32::new(0);

/// Starts core 1 with DW-GDMA channel `vdma1` for the strip copies. `None` (logged) if it did
/// not start.
pub(super) fn start(
    cpu: CPU_CTRL<'static>,
    vdma1: VDMA_CH1<'static>,
) -> Option<&'static mut Core1> {
    let copy = DmaCopy::new(vdma1);
    let Some(core1) = core1::start(
        cpu,
        DETECTOR.take(),
        SCRATCH.take(),
        SkipChoice::Pie,
        Some(copy),
    ) else {
        error!("detect: core 1 did not start: no detection, whole frames only");
        return None;
    };
    info!(
        "detect: core 1 started (bare: no scheduler, no interrupts), {} B of detector state in internal RAM, strip copies on DW-GDMA channel 1",
        RAM_BYTES
    );
    Some(core1)
}

/// The two ends of the `FrameBlobs` queue, once.
pub(super) fn blobs_queue() -> Option<(BlobsTx, BlobsRx)> {
    BLOBS.split()
}

/// Whether the next strip run has core 1's L1 data autoload: wanted and lent (the registers
/// are taken back when not wanted). Only while core 1 holds no strip (between runs).
pub(super) fn autoload_for_run() -> bool {
    let want = AUTOLOAD_WANTED.load(Relaxed);
    match prefetch::lend_l1_autoload(want) {
        Ok(()) => want,
        Err(e) => {
            AUTOLOAD_WANTED.store(false, Relaxed);
            let _ = prefetch::lend_l1_autoload(false);
            warn!("detect: {}", e.text());
            false
        }
    }
}

/// The detection settings core 1 takes at its next frame's first strip.
pub(super) fn core1_settings() -> StreamDetect {
    shared(|s| {
        let d = &s.detect;
        let (min_area, max_area) = d.settings.area_bounds();
        StreamDetect {
            enabled: d.settings.enabled && !d.fault,
            threshold: d.threshold,
            min_area,
            max_area,
            spots: FRAME_BLOBS_MAX as u16,
            serial: s.detect_serial,
        }
    })
}

/// What a frame's `FrameBlobs` carries besides the blobs.
pub(super) struct Meta {
    pub number: u32,
    pub generation: u32,
    pub timestamp_us: u64,
    /// Frames lost since the frame before (not published by core 1).
    pub lost: u32,
    pub width: u16,
    pub height: u16,
    /// The running strip plan's rows of a full strip, and the run's hold budget (µs,
    /// `DetectTiming`).
    pub strip_lines: u16,
    pub hold_budget_us: u16,
}

/// `cycles` of core 1 in µs at `mhz`, saturating at `u16::MAX`.
fn us16(cycles: u32, mhz: u32) -> u16 {
    u16::try_from(cycles / mhz).unwrap_or(u16::MAX)
}

/// Where the detection of result `r` spent its time, relative to the frame's capture time
/// (`m.timestamp_us`); `queued_us` is now, just before its datagram is built and queued.
/// Out of line: inlined, it takes [`Work::result`]'s frame past the stack-size threshold.
#[inline(never)]
fn timing(r: &FrameResult, m: &Meta, mhz: u32) -> DetectTiming {
    DetectTiming {
        strips_lead_us: r.lead / mhz,
        strips_end_us: r.last_hold / mhz,
        published_us: r.ready / mhz,
        queued_us: Instant::now()
            .as_micros()
            .saturating_sub(m.timestamp_us)
            .min(u64::from(u32::MAX)) as u32,
        measure_us: us16(r.refine_cycles, mhz),
        strips: r.strips,
        strip_lines: m.strip_lines,
        late_strips: r.late_strips,
        start_max_us: us16(r.start_latency, mhz),
        detect_max_us: us16(r.detect_max, mhz),
        copy_wait_max_us: us16(r.copy_wait_max, mhz),
        hold_max_us: us16(r.hold_max, mhz),
        hold_budget_us: m.hold_budget_us,
    }
}

/// The result side of detection (the capture task owns it).
pub(super) struct Work {
    out: BlobsTx,
    pack: &'static mut [u8; PACK_BYTES],
    mhz: u32,
}

/// The second's figures for the log.
#[derive(Clone, Copy)]
struct Second {
    frames: u32,
    cycles_total: u64,
    cycles_max: u32,
    instructions_total: u64,
    refine_total: u64,
    refine_max: u32,
    latency_total: u64,
    latency_max: u32,
    blobs_min: u32,
    blobs_max: u32,
    kept: u32,
    sent: u32,
    /// Results of a frame detected with settings changed since (not used).
    stale: u32,
    /// Results of frames core 1 did not copy (detected, not measured, not sent).
    unmeasured: u32,
    /// Frames whose detection core 1 cut short to keep up with the strips (copied, not
    /// detected).
    cut: u32,
    /// Every result of the second (detection on or off): frames, strips, and per strip the
    /// hold (take to release), the detector and the wait for the copy (cycles).
    results: u32,
    copied: u32,
    strips: u32,
    hold_total: u64,
    hold_max: u32,
    detect_strip_max: u32,
    copy_wait_total: u64,
    copy_wait_max: u32,
}

impl Second {
    const NEW: Self = Self {
        frames: 0,
        cycles_total: 0,
        cycles_max: 0,
        instructions_total: 0,
        refine_total: 0,
        refine_max: 0,
        latency_total: 0,
        latency_max: 0,
        blobs_min: u32::MAX,
        blobs_max: 0,
        kept: 0,
        sent: 0,
        stale: 0,
        unmeasured: 0,
        cut: 0,
        results: 0,
        copied: 0,
        strips: 0,
        hold_total: 0,
        hold_max: 0,
        detect_strip_max: 0,
        copy_wait_total: 0,
        copy_wait_max: 0,
    };
}

static SECOND: Mutex<RefCell<Second>> = Mutex::new(RefCell::new(Second::NEW));

fn second<R>(f: impl FnOnce(&mut Second) -> R) -> R {
    critical_section::with(|cs| f(&mut SECOND.borrow_ref_mut(cs)))
}

impl Work {
    pub fn new(out: BlobsTx) -> Self {
        Self {
            out,
            pack: PACK.take(),
            mhz: esp_hal::clock::cpu_clock().as_mhz().max(1),
        }
    }

    /// The strip and copy figures of one result (every result of the strip path).
    pub fn strips(&mut self, r: &FrameResult) {
        second(|s| {
            s.results += 1;
            s.copied += u32::from(r.buffer != NO_BUFFER);
            s.strips += u32::from(r.strips);
            s.hold_total += u64::from(r.hold_total);
            s.hold_max = s.hold_max.max(r.hold_max);
            s.detect_strip_max = s.detect_strip_max.max(r.detect_max);
            s.copy_wait_total += u64::from(r.copy_wait_total);
            s.copy_wait_max = s.copy_wait_max.max(r.copy_wait_max);
        });
    }

    /// One frame's detection: the area filter's counts, the next threshold, the state, the
    /// datagram. `now`: when the capture task took the result.
    #[inline(never)]
    pub fn result(&mut self, r: &FrameResult, m: &Meta, now: Instant) {
        let (settings, serial, send, on) = shared(|s| {
            (
                s.detect.settings,
                s.detect_serial,
                s.host_active(now),
                s.detect.settings.enabled && !s.detect.fault,
            )
        });
        if on && m.lost > 0 {
            shared(|s| s.detect.frames_skipped = s.detect.frames_skipped.wrapping_add(m.lost));
        }
        if !r.detected {
            if r.cut && on {
                // Core 1 fell behind the strips on this frame and copied it without
                // detecting the rest: not detected.
                shared(|s| {
                    let d = &mut s.detect;
                    d.frames_skipped = d.frames_skipped.wrapping_add(1);
                    d.frames_cut = d.frames_cut.wrapping_add(1);
                });
                second(|s| s.cut += 1);
            }
            return;
        }
        if r.serial != serial {
            second(|s| s.stale += 1);
            return;
        }
        let n = usize::from(r.blob_count).min(MAX_BLOBS);
        let found = &r.blobs[..n];
        let measured = r.buffer != NO_BUFFER;
        let (kept, peak) = found
            .iter()
            .filter(|b| settings.keeps(b.area))
            .fold((0u32, None), |(k, p), b| (k + 1, p.max(Some(b.peak))));
        // The centre, width and flux from the refined measure; area and peak from the
        // thresholded blob.
        let spots = usize::from(r.spot_count).min(r.spots.len());
        let (bytes, _) = cam::pack_blobs(
            r.spots[..spots]
                .iter()
                .zip(&r.spot_blob[..spots])
                .filter_map(|(s, &i)| {
                    found.get(usize::from(i)).map(|b| cam::Blob {
                        x: s.x,
                        y: s.y,
                        width: s.width,
                        flux: s.flux,
                        area: b.area,
                        peak: b.peak,
                    })
                }),
            &mut self.pack[..],
        );
        let packed = (bytes / BLOB_BYTES) as u32;
        let dropped = if measured {
            r.stats.overflow.saturating_add(kept.saturating_sub(packed))
        } else {
            r.stats.overflow
        };
        let filtered = found.len() as u32 - kept;
        let detect_us = r.cycles / self.mhz;
        let next = if settings.auto_threshold {
            r.background.map(|bg| {
                if r.raw10 {
                    auto::threshold_raw10(bg, peak)
                } else {
                    u16::from(auto::threshold_raw8(bg, peak))
                }
            })
        } else {
            Some(settings.threshold)
        };
        shared(|s| {
            let d = &mut s.detect;
            if measured {
                d.frames_detected = d.frames_detected.wrapping_add(1);
            } else {
                d.frames_skipped = d.frames_skipped.wrapping_add(1);
            }
            d.blobs_dropped = d.blobs_dropped.wrapping_add(dropped);
            d.blobs_filtered = d.blobs_filtered.wrapping_add(filtered);
            if let Some(th) = next {
                d.threshold = th;
            }
            if let Some(bg) = r.background {
                d.background_median = bg.median;
                d.background_sigma = bg.sigma;
            }
            if measured {
                let a = &mut s.detect_us;
                a.0 += u64::from(detect_us);
                a.1 += 1;
                a.2 = a.2.max(detect_us);
            }
        });
        let latency = now
            .as_micros()
            .saturating_sub(m.timestamp_us)
            .min(u64::from(u32::MAX)) as u32;
        second(|s| {
            if !measured {
                s.unmeasured += 1;
                return;
            }
            s.frames += 1;
            s.cycles_total += u64::from(r.cycles);
            s.cycles_max = s.cycles_max.max(r.cycles);
            s.instructions_total += u64::from(r.instructions);
            s.refine_total += u64::from(r.refine_cycles);
            s.refine_max = s.refine_max.max(r.refine_cycles);
            s.latency_total += u64::from(latency);
            s.latency_max = s.latency_max.max(latency);
            s.blobs_min = s.blobs_min.min(found.len() as u32);
            s.blobs_max = s.blobs_max.max(found.len() as u32);
            s.kept += kept;
        });
        if !send || !measured {
            return;
        }
        let mut flags = 0;
        if dropped > 0 {
            flags |= FrameBlobs::OVERFLOW;
        }
        if settings.auto_threshold {
            flags |= FrameBlobs::AUTO_THRESHOLD;
        }
        let blobs = FrameBlobs {
            frame: m.number,
            generation: m.generation,
            timestamp_us: m.timestamp_us,
            format: if r.raw10 { Format::Raw10 } else { Format::Raw8 },
            width: m.width,
            height: m.height,
            threshold: r.threshold,
            detect_us,
            timing: timing(r, m, self.mhz),
            flags,
            blobs: &self.pack[..bytes],
        };
        let queued = self.out.push_with(|o| {
            o.len = blobs.encode(&mut o.datagram).map_or(0, <[u8]>::len);
        });
        if queued {
            let sent_peak = blobs.blobs().map(|b| b.peak).max();
            critical_section::with(|cs| {
                let p = AE_PEAK.borrow(cs);
                p.set(p.get().max(sent_peak));
            });
            second(|s| s.sent += 1);
            BLOBS_READY.signal(());
        } else {
            shared(|s| s.detect.blobs_dropped = s.detect.blobs_dropped.wrapping_add(packed));
        }
    }
}

/// The automatic exposure's step at the end of a period.
fn auto_exposure() {
    let peak = critical_section::with(|cs| AE_PEAK.borrow(cs).take());
    let step = shared(|s| {
        if !s.auto_exposure || s.ae_cap == 0 {
            return None;
        }
        let next = cam::auto_exposure(s.exposure, peak, s.ae_cap);
        (next != s.exposure).then_some(next)
    });
    if let Some(e) = step {
        // A full command queue (a request in progress) skips this step.
        let _ = CMDS.try_send(Cmd::Exposure(e));
    }
}

/// Counters at the start of a second.
#[derive(Clone, Copy)]
struct Marks {
    captured: u32,
    /// Capture errors (stream counters).
    errors: u32,
    skipped: u32,
    dropped: u32,
    lapped: u32,
    lapped_copied: u32,
    not_copied: u32,
    copy_errors: u32,
    heartbeat: u32,
}

impl Marks {
    fn now() -> Self {
        let (captured, errors, skipped) = shared(|s| {
            let c = &s.counters;
            (c.frames_captured, c.capture_errors, s.detect.frames_skipped)
        });
        let (not_copied, copy_errors) = core1::copy_counts();
        Self {
            captured,
            errors,
            skipped,
            dropped: DROPPED.load(Relaxed),
            lapped: LAPPED.load(Relaxed),
            lapped_copied: LAPPED_COPIED.load(Relaxed),
            not_copied,
            copy_errors,
            heartbeat: core1::heartbeat(),
        }
    }
}

/// Steps the automatic exposure every period, and once a second checks core 1 and (strip
/// path running) logs the second's figures; every 10 s the stacks' unused bytes.
#[allow(
    clippy::large_stack_frames,
    reason = "a spawned task: its future lives in the task's static storage, and its poll frame \
    (the inlined log lines' arguments) is on the main stack, which has most of internal RAM"
)]
#[embassy_executor::task]
pub(super) async fn task(core1_up: bool) {
    cpu::name_current("detect");
    let period = Duration::from_millis(u64::from(AUTO_EXPOSURE_PERIOD_MS));
    let per_second = (1000 / AUTO_EXPOSURE_PERIOD_MS).max(1);
    let mut heartbeat = core1::heartbeat();
    let mut marks = Marks::now();
    let mut alive = core1_up;
    let mut first = true;
    let mut tick = 0u32;
    let mut next = Instant::now() + period;
    loop {
        Timer::at(next).await;
        next += period;
        auto_exposure();
        tick = tick.wrapping_add(1);
        if !tick.is_multiple_of(per_second) {
            continue;
        }
        if alive {
            let h = core1::health(&mut heartbeat);
            if h != Health::Alive {
                alive = false;
                fault(h);
            }
        }
        let m = Marks::now();
        let sec = second(|s| core::mem::replace(s, Second::NEW));
        let plan = critical_section::with(|cs| PLAN.borrow(cs).get());
        if plan.0 != 0 && alive {
            if first {
                first = false;
                info!("detect: core 1's dark skip {:?}", core1::skip_used());
                embassy_futures::yield_now().await;
            }
            log_second(&sec, &marks, &m, mhz());
            embassy_futures::yield_now().await;
            log_strips(&sec, &marks, &m, plan, mhz());
            embassy_futures::yield_now().await;
            log_second2(&sec, &marks, &m);
            embassy_futures::yield_now().await;
        }
        if tick.is_multiple_of(10 * per_second) {
            log_stacks();
        }
        marks = m;
    }
}

/// The CPU clock, MHz.
fn mhz() -> u32 {
    esp_hal::clock::cpu_clock().as_mhz().max(1)
}

/// Core 1 trapped or stopped: no more detection, and no more strips (core 1 copies them).
#[inline(never)]
fn fault(h: Health) {
    shared(|s| s.detect.fault = true);
    error!(
        "detect: core 1 {h:?}: detection stopped, the stream goes on with whole frames from its next start"
    );
}

#[inline(never)]
fn log_second(a: &Second, before: &Marks, now: &Marks, mhz: u32) {
    let captured = now.captured.wrapping_sub(before.captured);
    let n = u64::from(a.frames.max(1));
    info!(
        "detect: {} of {} frames ({}%), skipped {}, detect mean {} max {} us ({} kins), refine mean {} max {} us, capture to result mean {} max {} us",
        a.frames,
        captured,
        u64::from(a.frames) * 100 / u64::from(captured.max(1)),
        now.skipped.wrapping_sub(before.skipped),
        a.cycles_total / n / u64::from(mhz),
        a.cycles_max / mhz,
        a.instructions_total / n / 1000,
        a.refine_total / n / u64::from(mhz),
        a.refine_max / mhz,
        a.latency_total / n,
        a.latency_max,
    );
}

#[inline(never)]
fn log_strips(a: &Second, before: &Marks, now: &Marks, plan: (u16, u8, u32), mhz: u32) {
    let strips = u64::from(a.strips.max(1));
    info!(
        "strips: {} rows x {} in the ring (one every {} us), frames {} copied {} not copied {}, copy errors {}, lapped {} (after the copy {}), dropped results {}, per strip hold mean {} max {} us (budget {} us), detect max {} us, copy wait mean {} max {} us",
        plan.0,
        plan.1,
        plan.2,
        a.results,
        a.copied,
        now.not_copied.wrapping_sub(before.not_copied),
        now.copy_errors.wrapping_sub(before.copy_errors),
        now.lapped.wrapping_sub(before.lapped),
        now.lapped_copied.wrapping_sub(before.lapped_copied),
        now.dropped.wrapping_sub(before.dropped),
        a.hold_total / strips / u64::from(mhz),
        a.hold_max / mhz,
        u32::from(plan.1.saturating_sub(1)) * plan.2,
        a.detect_strip_max / mhz,
        a.copy_wait_total / strips / u64::from(mhz),
        a.copy_wait_max / mhz,
    );
}

#[inline(never)]
fn log_second2(a: &Second, before: &Marks, now: &Marks) {
    let (threshold, median, sigma, auto_threshold, exposure, fault) = shared(|s| {
        let d = &s.detect;
        (
            d.threshold,
            d.background_median,
            d.background_sigma,
            d.settings.auto_threshold,
            s.exposure,
            d.fault,
        )
    });
    info!(
        "detect:   blobs {}..{} kept {} sent {}, threshold {}{}, background {} sigma {}, exposure {} gain {}, stale {} unmeasured {} cut {}, capture errors {}, heartbeat {}/s{}",
        if a.frames == 0 { 0 } else { a.blobs_min },
        a.blobs_max,
        a.kept,
        a.sent,
        threshold,
        if auto_threshold { " auto" } else { "" },
        median,
        sigma,
        exposure.lines,
        exposure.gain_sixteenths,
        a.stale,
        a.unmeasured,
        a.cut,
        now.errors.wrapping_sub(before.errors),
        now.heartbeat.wrapping_sub(before.heartbeat),
        if fault { ", FAULT" } else { "" },
    );
}

#[inline(never)]
fn log_stacks() {
    let (main_unused, main_size) = crate::stack::main_unused();
    info!(
        "stack: main {} B of {} never used, core 1 {} B of 16384 never used",
        main_unused,
        main_size,
        CORE1_STACK_UNUSED.load(Relaxed),
    );
}
