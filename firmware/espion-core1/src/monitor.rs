//! Core 0's side of a run on core 1: the hand-over of a strip reader, and the monitor that
//! calls `StripControl::service()`, drains the frame results, and once a second logs what core
//! 1 did and checks that it is alive. Core 0 only: everything here may log, wait and use
//! embassy-time; none of it is reachable from core 1's entry.

use embassy_time::{Duration, Instant, Timer};
#[cfg(feature = "stats")]
use esp32p4_cache::stats::{self as lock_stats, LockStats};
use esp32p4_csi::{CsiStrips, StripControl, StripStats};
use esp32p4_perf::cycles;
use log::{error, info, warn};

use crate::runner::{
    CHECK, CHECK_MAX, CheckSnap, Core1, FrameResult, HOLD_TIMES, Job, Trap, abandoned, heartbeat,
    trap,
};

impl Core1 {
    /// Waits up to `timeout` for core 1 to have taken the last reader posted, checking every
    /// millisecond. `false` (logged) if it did not.
    pub async fn wait_mailbox(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while !self.can_post() {
            if Instant::now() >= end {
                report_dead("did not take the last reader");
                return false;
            }
            Timer::after_millis(1).await;
        }
        true
    }

    /// Splits `cs` and hands the reader to core 1 (the mailbox must be free:
    /// [`Core1::wait_mailbox`]); returns the control half. `None` (logged) if the split or the
    /// post failed.
    #[inline(never)]
    pub fn hand_over<'d>(&mut self, cs: CsiStrips<'d>) -> Option<StripControl<'d>> {
        match cs.split() {
            // The reader is copied into the mailbox and dropped here unused (`Core1::post`).
            Ok((control, ref reader)) => {
                if self.post(reader) {
                    Some(control)
                } else {
                    error!("core 1: the mailbox is not free");
                    None
                }
            }
            Err((_, e)) => {
                error!("core 1: split: {e:?}");
                None
            }
        }
    }
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
    start_latency: u32,
    ready_total: u64,
    ready_max: u32,
    pop_total: u64,
    pop_max: u32,
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
        start_latency: 0,
        ready_total: 0,
        ready_max: 0,
        pop_total: 0,
        pop_max: 0,
        blobs_min: u32::MAX,
        blobs_max: 0,
        overflow: 0,
    };

    /// `r`, popped on core 0 at core 1's time `now1`.
    fn add(&mut self, r: &FrameResult, now1: u32) {
        self.frames += 1;
        self.cycles_total += u64::from(r.cycles);
        self.cycles_max = self.cycles_max.max(r.cycles);
        self.instructions_total += u64::from(r.instructions);
        self.late_strips += u32::from(r.late_strips);
        self.worst_latency = self.worst_latency.max(r.worst_latency);
        self.start_latency = self.start_latency.max(r.start_latency);
        self.ready_total += u64::from(r.ready);
        self.ready_max = self.ready_max.max(r.ready);
        // A pop right after the push can come out a few cycles negative (the clock offset's
        // uncertainty): counted as 0.
        let pop = match now1.wrapping_sub(r.pushed_at) {
            p if p > u32::MAX / 2 => 0,
            p => p,
        };
        self.pop_total += u64::from(pop);
        self.pop_max = self.pop_max.max(pop);
        self.blobs_min = self.blobs_min.min(u32::from(r.blob_count));
        self.blobs_max = self.blobs_max.max(u32::from(r.blob_count));
        self.overflow = self.overflow.wrapping_add(r.stats.overflow);
    }

    fn merge(&mut self, o: &Acc) {
        self.frames += o.frames;
        self.cycles_total += o.cycles_total;
        self.cycles_max = self.cycles_max.max(o.cycles_max);
        self.instructions_total += o.instructions_total;
        self.late_strips += o.late_strips;
        self.worst_latency = self.worst_latency.max(o.worst_latency);
        self.start_latency = self.start_latency.max(o.start_latency);
        self.ready_total += o.ready_total;
        self.ready_max = self.ready_max.max(o.ready_max);
        self.pop_total += o.pop_total;
        self.pop_max = self.pop_max.max(o.pop_max);
        self.blobs_min = self.blobs_min.min(o.blobs_min);
        self.blobs_max = self.blobs_max.max(o.blobs_max);
        self.overflow = self.overflow.wrapping_add(o.overflow);
    }
}

/// Counters read at the start of a second, for the per-second differences.
#[derive(Clone, Copy)]
struct Marks {
    heartbeat: u32,
    lapped: u32,
    abandoned: u32,
    dropped: u32,
    check: CheckSnap,
    #[cfg(feature = "stats")]
    lock: [LockStats; 2],
}

impl Marks {
    fn now(control: &StripControl<'_>, core1: &Core1) -> Self {
        Self {
            heartbeat: heartbeat(),
            lapped: control.strips_lapped(),
            abandoned: abandoned(),
            dropped: core1.dropped(),
            check: CHECK.snap(),
            #[cfg(feature = "stats")]
            lock: [lock_stats::get(0), lock_stats::get(1)],
        }
    }
}

/// One run on core 1 as core 0 watches it. Every millisecond [`Monitor::poll`], once a second
/// [`Monitor::second`], at the end [`Monitor::end`].
pub struct Monitor {
    job: Job,
    offset: u32,
    clock_err: u32,
    mhz: u32,
    second: Acc,
    total: Acc,
    marks: Marks,
    busy: u64,
    busy_max: u32,
    seconds: u32,
    #[cfg(feature = "stats")]
    lock_max: [LockStats; 2],
}

impl Monitor {
    /// Starts watching a run of `job` (call right after the strip path started): measures the
    /// offset of core 1's clock (a few microseconds of spinning), zeroes the lock maxima and
    /// takes the first marks.
    #[inline(never)]
    pub fn new(core1: &mut Core1, control: &StripControl<'_>, job: Job) -> Self {
        let mhz = esp_hal::clock::cpu_clock().as_mhz();
        let (offset, clock_err) = core1.clock_offset(10_000 * mhz).unwrap_or((0, u32::MAX));
        #[cfg(feature = "stats")]
        lock_stats::reset_max();
        Self {
            job,
            offset,
            clock_err,
            mhz,
            second: Acc::NEW,
            total: Acc::NEW,
            marks: Marks::now(control, core1),
            busy: 0,
            busy_max: 0,
            seconds: 0,
            #[cfg(feature = "stats")]
            lock_max: [LockStats::default(); 2],
        }
    }

    /// `service()` and the drain of the frame results; call about every millisecond. Its own
    /// time is counted (core 0's `service+drain` in the log).
    #[inline(never)]
    pub fn poll(&mut self, control: &mut StripControl<'_>, core1: &mut Core1) {
        let t0 = cycles();
        control.service();
        let (offset, second) = (self.offset, &mut self.second);
        while core1
            .pop_with(|r| second.add(r, cycles().wrapping_add(offset)))
            .is_some()
        {}
        let dt = cycles().wrapping_sub(t0);
        self.busy += u64::from(dt);
        self.busy_max = self.busy_max.max(dt);
    }

    /// Once a second: core 1's trap record and heartbeat, then the second's log lines (the
    /// executor gets a turn after each line: a line at 2 Mbaud holds core 0 for about a
    /// millisecond). `false` if core 1 trapped or stopped (logged); the caller ends the run.
    pub async fn second(&mut self, control: &StripControl<'_>, core1: &Core1) -> bool {
        let m = Marks::now(control, core1);
        if let Some(t) = trap() {
            report_trap(&t);
            return false;
        }
        if m.heartbeat == self.marks.heartbeat {
            report_dead("heartbeat stopped for 1 s");
            return false;
        }
        let first = self.seconds == 0;
        self.seconds += 1;
        #[cfg(feature = "stats")]
        for (mx, l) in self.lock_max.iter_mut().zip(m.lock) {
            mx.hold_max = mx.hold_max.max(l.hold_max);
            mx.wait_max = mx.wait_max.max(l.wait_max);
        }
        if self.job == Job::Check {
            log_check_second(&self.marks, &m);
            embassy_futures::yield_now().await;
            log_check_second2(&m.check.since(&self.marks.check), self.mhz);
        } else {
            log_second(&self.second, self.mhz);
            embassy_futures::yield_now().await;
            log_second2(
                &self.second,
                &self.marks,
                &m,
                self.busy / u64::from(self.mhz.max(1)),
            );
            if !first {
                self.total.merge(&self.second);
            }
        }
        #[cfg(feature = "stats")]
        {
            embassy_futures::yield_now().await;
            log_lock_second(&self.marks, &m);
            lock_stats::reset_max();
        }
        self.marks = m;
        self.second = Acc::NEW;
        self.busy = 0;
        true
    }

    /// The run's totals (detector: after the first second) under `name`.
    #[inline(never)]
    pub fn end(&self, name: &str) {
        if self.job == Job::Detect {
            log_detect_total(name, &self.total, self.mhz);
            log_detect_total2(&self.total, self.mhz, self.busy_max, self.clock_err);
        }
        #[cfg(feature = "stats")]
        log_lock_total(&self.lock_max, self.mhz);
    }
}

// ── Log lines ───────────────────────────────────────────────────────────────────────────

#[inline(never)]
fn log_second(a: &Acc, mhz: u32) {
    let mhz = mhz.max(1);
    let frames = u64::from(a.frames.max(1));
    let m = u64::from(mhz);
    info!(
        "core 1: {} fps, detect mean {} max {} us ({} ins/frame), late strips {}, latency to start max {} us, to end max {} us, last strip to result mean {} max {} us, result to core 0 mean {} max {} us",
        a.frames,
        a.cycles_total / frames / m,
        a.cycles_max / mhz,
        a.instructions_total / frames,
        a.late_strips,
        a.start_latency / mhz,
        a.worst_latency / mhz,
        a.ready_total / frames / m,
        a.ready_max / mhz,
        a.pop_total / frames / m,
        a.pop_max / mhz,
    );
}

#[inline(never)]
fn log_second2(a: &Acc, before: &Marks, now: &Marks, busy_us: u64) {
    info!(
        "core 1:   lapped {}, abandoned {}, blobs {}..{}, overflow {}, results dropped {}, core 0 in service+drain {busy_us} us/s, heartbeat {}/s",
        now.lapped.wrapping_sub(before.lapped),
        now.abandoned.wrapping_sub(before.abandoned),
        if a.frames == 0 { 0 } else { a.blobs_min },
        a.blobs_max,
        a.overflow,
        now.dropped.wrapping_sub(before.dropped),
        now.heartbeat.wrapping_sub(before.heartbeat),
    );
}

#[cfg(feature = "stats")]
#[inline(never)]
fn log_lock_second(before: &Marks, now: &Marks) {
    info!(
        "core 1:   cache lock ops {}/{}, hold max {}/{} cycles, wait max {}/{} cycles (core 0/core 1)",
        now.lock[0].ops.wrapping_sub(before.lock[0].ops),
        now.lock[1].ops.wrapping_sub(before.lock[1].ops),
        now.lock[0].hold_max,
        now.lock[1].hold_max,
        now.lock[0].wait_max,
        now.lock[1].wait_max,
    );
}

#[inline(never)]
fn log_detect_total(name: &str, a: &Acc, mhz: u32) {
    let mhz = mhz.max(1);
    let m = u64::from(mhz);
    let frames = u64::from(a.frames.max(1));
    info!(
        "core 1: {name} total after the first second: frames {}, detect mean {} max {} us, {} ins/frame, late strips {}, latency to start max {} us, to end max {} us",
        a.frames,
        a.cycles_total / frames / m,
        a.cycles_max / mhz,
        a.instructions_total / frames,
        a.late_strips,
        a.start_latency / mhz,
        a.worst_latency / mhz,
    );
}

#[inline(never)]
fn log_detect_total2(a: &Acc, mhz: u32, busy_max: u32, clock_err: u32) {
    let mhz = mhz.max(1);
    let m = u64::from(mhz);
    let frames = u64::from(a.frames.max(1));
    info!(
        "core 1:   last strip to result mean {} max {} us, result to core 0 mean {} max {} us, blobs {}..{}, overflow {}, longest service+drain {} us, clock offset uncertainty {} cycles",
        a.ready_total / frames / m,
        a.ready_max / mhz,
        a.pop_total / frames / m,
        a.pop_max / mhz,
        if a.frames == 0 { 0 } else { a.blobs_min },
        a.blobs_max,
        a.overflow,
        busy_max / mhz,
        clock_err,
    );
}

#[inline(never)]
fn log_check_second(before: &Marks, now: &Marks) {
    let d = now.check.since(&before.check);
    info!(
        "core 1: check: strips {} (dropped {}), rows {} bad {} (in dropped strips {}), seq errors {}, size errors {}, frames {} good {}, lapped {}, heartbeat {}/s",
        d.kept,
        d.dropped,
        d.rows,
        d.rows_bad,
        d.rows_bad_dropped,
        d.seq_errors,
        d.size_errors,
        d.frames,
        d.frames_good,
        now.lapped.wrapping_sub(before.lapped),
        now.heartbeat.wrapping_sub(before.heartbeat),
    );
}

/// The check's holds and recoveries in `d`, and the run maxima so far.
#[inline(never)]
pub fn log_check_second2(d: &CheckSnap, mhz: u32) {
    let mx = CHECK_MAX.snap();
    let mhz = mhz.max(1);
    let (bad_frame, bad_row) = match mx.first_bad {
        0 => (0, 0),
        v => ((v - 1) >> 12, (v - 1) & 0xFFF),
    };
    info!(
        "core 1:   holds {} (changed+kept {}, changed+dropped {}, same+dropped {}, same+kept {}, order errors {}), recoveries {}; run maxima: recovery {} us, start latency {} us, check {} us, first bad frame/row {bad_frame}/{bad_row}",
        d.held,
        d.held_changed_kept,
        d.held_changed_dropped,
        d.held_same_dropped,
        d.held_same_kept,
        d.order_errors,
        d.recoveries,
        mx.recovery / mhz,
        mx.start_latency / mhz,
        mx.check / mhz,
    );
}

#[cfg(feature = "stats")]
#[inline(never)]
fn log_lock_total(m: &[LockStats; 2], mhz: u32) {
    let mhz = mhz.max(1);
    info!(
        "core 1: cache lock over the run: hold max core 0 {} us core 1 {} us, wait max core 0 {} us core 1 {} us (cycles {}/{}, {}/{})",
        m[0].hold_max / mhz,
        m[1].hold_max / mhz,
        m[0].wait_max / mhz,
        m[1].wait_max / mhz,
        m[0].hold_max,
        m[1].hold_max,
        m[0].wait_max,
        m[1].wait_max,
    );
}

/// The end of a run under `name`: the strip path's counters, and with `check` (the check's
/// counters over the run) the check's verdict.
#[inline(never)]
pub fn log_summary(name: &str, control: &StripControl<'_>, check: Option<&CheckSnap>) {
    let s: StripStats = control.stats();
    info!(
        "core 1: {name} done: frames complete {} truncated {} skipped {}, strips delivered {} discarded {} lapped {}, invalidate max {} cycles",
        s.frames_complete,
        s.frames_truncated,
        s.frames_skipped,
        s.strips_delivered,
        s.strips_discarded,
        control.strips_lapped(),
        s.invalidate_cycles_max,
    );
    info!(
        "core 1:   stalls {}, length errors {}, DMA errors {}, bridge errors {}, host errors {}, ISR runs {}",
        s.dma_stalls, s.length_errors, s.dma_errors, s.bridge_errors, s.host_errors, s.isr_runs,
    );
    if let Some(d) = check {
        log_check_summary(name, d);
    }
}

#[inline(never)]
fn log_check_summary(name: &str, d: &CheckSnap) {
    let pass = d.rows_bad == 0
        && d.seq_errors == 0
        && d.size_errors == 0
        && d.held_changed_kept == 0
        && d.order_errors == 0
        && d.old_kept == 0
        && d.frames_good == d.frames;
    info!(
        "core 1: {name} check done: strips {} (dropped {}), rows {} bad {} (in dropped strips {}), seq errors {}, size errors {}, frames {} good {}, old-run strips kept {} dropped {}: {}",
        d.kept,
        d.dropped,
        d.rows,
        d.rows_bad,
        d.rows_bad_dropped,
        d.seq_errors,
        d.size_errors,
        d.frames,
        d.frames_good,
        d.old_kept,
        d.old_dropped,
        if pass { "PASS" } else { "FAIL" }
    );
    let mhz = esp_hal::clock::cpu_clock().as_mhz();
    log_check_second2(d, mhz);
    if d.held != 0 {
        log_hold_times(mhz);
    }
}

#[inline(never)]
fn log_hold_times(mhz: u32) {
    let t = HOLD_TIMES.snap();
    let us = |c: u32| c / mhz.max(1);
    info!(
        "core 1:   hold timeline (us after the take, min..max; 0: not seen): item k+1 done {}..{}, k+2 {}..{}, k+3 {}..{}, k again {}..{}; first bytes of k changed {}..{}",
        us(t.done1_min),
        us(t.done1_max),
        us(t.done2_min),
        us(t.done2_max),
        us(t.done3_min),
        us(t.done3_max),
        us(t.done0_min),
        us(t.done0_max),
        us(t.change_min),
        us(t.change_max),
    );
}

/// Logs core 1's trap record.
#[inline(never)]
pub fn report_trap(t: &Trap) {
    error!(
        "core 1 trapped: mcause {:#x} mepc {:#010x} mtval {:#010x}; it waits for ever",
        t.mcause, t.mepc, t.mtval
    );
}

/// Logs that core 1 stopped: `what`, its heartbeat.
#[inline(never)]
pub fn report_dead(what: &str) {
    warn!(
        "core 1 stalled: {what} (heartbeat {}, no trap recorded)",
        heartbeat()
    );
}
