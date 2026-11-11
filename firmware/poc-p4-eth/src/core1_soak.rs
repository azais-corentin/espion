//! Strip runs with the blob detector on core 1 (`RUN_CORE1` in `main.rs`; see [`crate::core1`]
//! for core 1's side). **Not run on the board yet.**
//!
//! Core 1 is started once. Per group of modes with the same strip size, core 0 builds a
//! `CsiStrips` on the strip soak's ring, splits it, and hands the reader to core 1. Per mode it
//! sets the detector parameters, sets up the sensor, starts the control half and the stream,
//! and then, for [`WINDOW`], every millisecond:
//! - calls `StripControl::service()` (restarts the channel after a stall or a DMA error once
//!   the reader has caught up);
//! - drains core 1's frame results;
//! - once a second logs frames, fps, detector time per frame (mean and max), late strips,
//!   lapped strips, abandoned frames, blobs, dropped results and the worst strip latency,
//!   checks core 1's heartbeat and trap record, and ends the run if core 1 stopped.
//!
//! After the run: stream off, the control half stopped, one summary line with the strip
//! path's counters. Core 0 never waits on core 1 except for the mailbox (bounded).

use embassy_time::{Duration, Instant, Timer};
use esp_hal::peripherals::{CPU_CTRL, VDMA_CH0};
use esp32p4_csi::{Config, CsiStrips, StripControl};
use log::{error, info, warn};

use crate::camera::{self, Cam, Plan, Sensor};
use crate::core1::{self, Core1, FrameResult, Trap};
use crate::modes::{self, CamMode};
use crate::{soak, strip_soak};

/// The modes, by reference: indexing the `const` array itself would copy all of it.
const MODES: &[CamMode] = &modes::MODES;

/// Length of each mode's run.
const WINDOW: Duration = Duration::from_secs(30);
/// RAW10 threshold in 10-bit units: the RAW8 one times 4, as the strip soak uses.
const THRESHOLD_RAW10: u16 = soak::THRESHOLD as u16 * 4;
/// How long core 0 waits for core 1 to take a reader from the mailbox.
const MAILBOX_TIMEOUT: Duration = Duration::from_millis(100);

/// The plan whose modes and sensor settings the runs use. A reference to the `const`, so it is
/// not copied onto a stack.
const PLAN: &Plan = &crate::CAMERA_PLAN;

/// Runs the modes of [`PLAN`] with its sensor settings, the detector on core 1. Does nothing
/// unless `ON` (`RUN_CORE1`), a constant checked before the first await. Everything that
/// names the big statics (core 1's stack and start, the result queue, the detector, the ring)
/// runs before that first await too, so with `ON` false the compiler drops it (the later
/// states of this future cannot be pruned, the first one can).
pub async fn run<const ON: bool>(
    cam: &mut Cam,
    mut vdma: VDMA_CH0<'static>,
    cpu: CPU_CTRL<'static>,
) {
    if !ON {
        return;
    }
    let plan = PLAN;
    let (det, scratch, _) = soak::take_detector();
    let mut ring = strip_soak::Ring::take(None);
    let Some(core1) = core1::start(cpu, det, scratch) else {
        error!("core 1: could not start");
        return;
    };
    info!("core 1: started (bare: no scheduler, no interrupts)");
    if !camera::power_up().await {
        return;
    }
    let mut rest = plan.modes;
    while let Some(&first) = rest.first() {
        let config = camera::csi_config(&MODES[first].mode);
        let bytes = strip_soak::strip_bytes(&config);
        let n = rest
            .iter()
            .take_while(|&&i| strip_soak::strip_bytes(&camera::csi_config(&MODES[i].mode)) == bytes)
            .count();
        let (group, next) = rest.split_at(n);
        rest = next;
        if !mailbox_free(core1).await {
            break;
        }
        // SAFETY: the previous group's control half, and with it its receiver, was dropped at
        // the end of `group_run`; its reader on core 1 is inert from then on.
        let Some(control) = (unsafe { receiver(vdma.reborrow(), &mut ring, &config, core1) })
        else {
            break;
        };
        if !group_run(cam, control, core1, group, &plan.sensor).await {
            break;
        }
    }
    info!("core 1 runs done");
}

/// Waits up to [`MAILBOX_TIMEOUT`] for core 1 to have taken the last reader posted.
async fn mailbox_free(core1: &Core1) -> bool {
    let end = Instant::now() + MAILBOX_TIMEOUT;
    while !core1.can_post() {
        if Instant::now() >= end {
            report_dead("did not take the last reader");
            return false;
        }
        Timer::after_millis(1).await;
    }
    true
}

/// The strip receiver for `config` on `ring`, split: the reader goes to core 1 (the mailbox
/// must be free), the control half is returned.
///
/// # Safety
///
/// No receiver built on an earlier split of `ring` may still be alive.
#[inline(never)]
unsafe fn receiver<'d>(
    vdma: VDMA_CH0<'d>,
    ring: &mut strip_soak::Ring,
    config: &Config,
    core1: &mut Core1,
) -> Option<StripControl<'d>> {
    // SAFETY (every arm): the caller's guarantee.
    let cs = match strip_soak::ring_strips(config) {
        8 => new_receiver(vdma, unsafe { ring.split::<8>(config) }, config),
        4 => new_receiver(vdma, unsafe { ring.split::<4>(config) }, config),
        3 => new_receiver(vdma, unsafe { ring.split::<3>(config) }, config),
        other => {
            error!("core 1: no ring split for {other} strips");
            None
        }
    }?;
    split_to(cs, core1)
}

/// Splits `cs`, posts the reader to core 1 and returns the control half.
#[inline(never)]
fn split_to<'d>(cs: CsiStrips<'d>, core1: &mut Core1) -> Option<StripControl<'d>> {
    match cs.split() {
        // The reader is copied into the mailbox and dropped here unused (`Core1::post`).
        Ok((control, ref reader)) => core1.post(reader).then_some(control),
        Err((_, e)) => {
            error!("core 1: split: {e:?}");
            None
        }
    }
}

fn new_receiver<'d, const N: usize>(
    vdma: VDMA_CH0<'d>,
    (strips, links): strip_soak::RingSplit<N>,
    config: &Config,
) -> Option<CsiStrips<'d>> {
    match CsiStrips::new(vdma, strips, links, *config, strip_soak::lines(config)) {
        Ok(c) => {
            info!("core 1: receiver with {N} strips");
            Some(c)
        }
        Err(e) => {
            error!("core 1: CsiStrips::new: {e:?}");
            None
        }
    }
}

/// Every mode of `group` on `control`, dropped at the end. `false` if core 1 stopped.
async fn group_run(
    cam: &mut Cam,
    mut control: StripControl<'_>,
    core1: &mut Core1,
    group: &[usize],
    sensor: &Sensor,
) -> bool {
    for &index in group {
        if !mode_run(cam, &mut control, core1, &MODES[index], sensor).await {
            return false;
        }
    }
    true
}

/// One mode: parameters, start, the monitored run, stop, summary. `false` if core 1 stopped.
/// Each step is a function of its own, so no poll function builds several futures at once
/// (`clippy::large_stack_frames`).
async fn mode_run(
    cam: &mut Cam,
    control: &mut StripControl<'_>,
    core1: &mut Core1,
    mode: &CamMode,
    sensor: &Sensor,
) -> bool {
    let mhz = esp_hal::clock::cpu_clock().as_mhz();
    let lines = set_params(core1, mode, mhz);
    if !start_mode(cam, control, mode, sensor, lines).await {
        return true;
    }
    let alive = monitor(control, core1, mode, mhz).await;
    stop_mode(cam, control).await;
    log_summary(mode, control);
    alive
}

/// The detector parameters of `mode` for core 1. Returns the rows per strip.
#[inline(never)]
fn set_params(core1: &mut Core1, mode: &CamMode, mhz: u32) -> u16 {
    let lines = strip_soak::lines(&camera::csi_config(&mode.mode));
    // One strip period: the frame period times the strip's share of the rows.
    let strip_us = strip_soak::budget_us(mode) * u32::from(lines) / u32::from(mode.mode.height());
    let threshold = if mode.raw10() {
        THRESHOLD_RAW10
    } else {
        u16::from(soak::THRESHOLD)
    };
    core1.set_params(mode.mode.width(), mode.raw10(), threshold, strip_us * mhz);
    log_mode_start(mode, lines, strip_us);
    lines
}

/// Sensor set-up, receiver start, stream-on. `false` if something failed (logged).
async fn start_mode(
    cam: &mut Cam,
    control: &mut StripControl<'_>,
    mode: &CamMode,
    sensor: &Sensor,
    lines: u16,
) -> bool {
    if let Err(e) = camera::sensor_setup(cam, &mode.mode, sensor).await {
        error!("core 1: sensor set-up: {e:?}");
        return false;
    }
    if !start_receiver(control, mode, lines) {
        return false;
    }
    Timer::after_millis(1).await;
    if let Err(e) = cam.start().await {
        error!("core 1: stream on: {e:?}");
        control.stop();
        return false;
    }
    true
}

#[inline(never)]
fn start_receiver(control: &mut StripControl<'_>, mode: &CamMode, lines: u16) -> bool {
    let config = camera::csi_config(&mode.mode);
    match control
        .set_config(config, lines)
        .and_then(|()| control.start())
    {
        Ok(()) => true,
        Err(e) => {
            error!("core 1: receiver start: {e:?}");
            false
        }
    }
}

/// Stream-off, then the receiver stopped once the current frame has ended.
async fn stop_mode(cam: &mut Cam, control: &mut StripControl<'_>) {
    if let Err(e) = cam.stop().await {
        error!("core 1: stream off: {e:?}");
    }
    // Let the current frame finish so the lanes go back to LP-11.
    Timer::after_millis(20).await;
    control.stop();
}

/// Totals over a stretch of frame results.
#[derive(Clone, Copy)]
struct Acc {
    frames: u32,
    cycles_total: u64,
    cycles_max: u32,
    instructions_total: u64,
    late_strips: u32,
    worst_latency: u32,
    blobs_min: u32,
    blobs_max: u32,
    overflow: u32,
}

impl Acc {
    const NEW: Self = Self {
        frames: 0,
        cycles_total: 0,
        cycles_max: 0,
        instructions_total: 0,
        late_strips: 0,
        worst_latency: 0,
        blobs_min: u32::MAX,
        blobs_max: 0,
        overflow: 0,
    };

    fn add(&mut self, r: &FrameResult) {
        self.frames += 1;
        self.cycles_total += u64::from(r.cycles);
        self.cycles_max = self.cycles_max.max(r.cycles);
        self.instructions_total += u64::from(r.instructions);
        self.late_strips += u32::from(r.late_strips);
        self.worst_latency = self.worst_latency.max(r.worst_latency);
        self.blobs_min = self.blobs_min.min(u32::from(r.blob_count));
        self.blobs_max = self.blobs_max.max(u32::from(r.blob_count));
        self.overflow = self.overflow.wrapping_add(r.stats.overflow);
    }
}

/// Counters read at the start of a second, for the per-second differences.
#[derive(Clone, Copy)]
struct Marks {
    heartbeat: u32,
    lapped: u32,
    abandoned: u32,
    dropped: u32,
}

impl Marks {
    fn now(control: &StripControl<'_>, core1: &Core1) -> Self {
        Self {
            heartbeat: core1::heartbeat(),
            lapped: control.strips_lapped(),
            abandoned: core1::abandoned(),
            dropped: core1.dropped(),
        }
    }
}

/// The run's monitor loop (see the module docs). `false` if core 1 stopped or trapped.
async fn monitor(
    control: &mut StripControl<'_>,
    core1: &mut Core1,
    mode: &CamMode,
    mhz: u32,
) -> bool {
    let start = Instant::now();
    let mut next = start + Duration::from_secs(1);
    let mut second = Acc::NEW;
    let mut marks = Marks::now(control, core1);
    loop {
        Timer::after_millis(1).await;
        control.service();
        while core1.pop_with(|r| second.add(r)).is_some() {}
        let now = Instant::now();
        if now < next {
            continue;
        }
        let m = Marks::now(control, core1);
        if let Some(t) = core1::trap() {
            report_trap(&t);
            return false;
        }
        if m.heartbeat == marks.heartbeat {
            report_dead("heartbeat stopped for 1 s");
            return false;
        }
        log_second(mode, &second, &marks, &m, mhz);
        marks = m;
        second = Acc::NEW;
        next += Duration::from_secs(1);
        if now >= start + WINDOW {
            return true;
        }
    }
}

// ── Log lines ───────────────────────────────────────────────────────────────────────────

#[inline(never)]
fn log_mode_start(mode: &CamMode, lines: u16, strip_us: u32) {
    info!(
        "core 1: {} for {} s, {lines}-row strips, late above {strip_us} us, skip {:?}",
        mode.name,
        WINDOW.as_secs(),
        core1::skip_used()
    );
}

#[inline(never)]
fn log_second(mode: &CamMode, a: &Acc, before: &Marks, now: &Marks, mhz: u32) {
    let mhz = mhz.max(1);
    let frames = u64::from(a.frames.max(1));
    let blobs_min = if a.frames == 0 { 0 } else { a.blobs_min };
    info!(
        "core 1: {}: {} fps, detect mean {} max {} us ({} ins/frame), late strips {}, worst latency {} us, lapped {}, abandoned {}, blobs {}..{}, overflow {}, results dropped {}, heartbeat {}/s",
        mode.name,
        a.frames,
        a.cycles_total / frames / u64::from(mhz),
        a.cycles_max / mhz,
        a.instructions_total / frames,
        a.late_strips,
        a.worst_latency / mhz,
        now.lapped.wrapping_sub(before.lapped),
        now.abandoned.wrapping_sub(before.abandoned),
        blobs_min,
        a.blobs_max,
        a.overflow,
        now.dropped.wrapping_sub(before.dropped),
        now.heartbeat.wrapping_sub(before.heartbeat),
    );
}

#[inline(never)]
fn log_summary(mode: &CamMode, control: &StripControl<'_>) {
    let s = control.stats();
    let lapped = control.strips_lapped();
    info!(
        "core 1: {} done: frames complete {} truncated {} skipped {}, strips delivered {} discarded {} lapped {}, stalls {}, length errors {}, DMA errors {}, bridge errors {}, host errors {}, invalidate max {} cycles",
        mode.name,
        s.frames_complete,
        s.frames_truncated,
        s.frames_skipped,
        s.strips_delivered,
        s.strips_discarded,
        lapped,
        s.dma_stalls,
        s.length_errors,
        s.dma_errors,
        s.bridge_errors,
        s.host_errors,
        s.invalidate_cycles_max,
    );
}

#[inline(never)]
fn report_trap(t: &Trap) {
    error!(
        "core 1 trapped: mcause {:#x} mepc {:#010x} mtval {:#010x}; it waits for ever",
        t.mcause, t.mepc, t.mtval
    );
}

#[inline(never)]
fn report_dead(what: &str) {
    warn!(
        "core 1 stalled: {what} (heartbeat {}, no trap recorded)",
        core1::heartbeat()
    );
}
