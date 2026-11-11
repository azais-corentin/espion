//! Strip runs with core 1 reading the strips (`RUN_CORE1` in `main.rs`; core 1's side and
//! core 0's monitor are the crate `espion-core1`). Board results: `README.md`, "Detector on
//! core 1".
//!
//! Core 1 is started once. [`PLAN`] is a list of [`Step`]s; consecutive steps with the same
//! strip size share one `CsiStrips` on the strip soak's ring, split, with the reader handed
//! to core 1. If a step checks pixels, core 0 first captures reference row checksums of the
//! sensor's random test pattern from whole frames (`Csi` on PSRAM frame buffers, before any
//! strip receiver exists). Per step core 0 sets core 1's run parameters, sets up the sensor,
//! starts the control half and the stream, and then, every millisecond, calls
//! `StripControl::service()` (restarts the channel after a stall or a DMA error once the
//! reader has caught up) and drains core 1's frame results; once a second it logs a line,
//! checks core 1's heartbeat and trap record, and ends the runs if core 1 stopped.
//!
//! What a step does ([`Work`]):
//! - `Detect`: the blob detector on core 1, live or on a synthetic strip, with or without the
//!   L1 data autoload; per second fps, detect time, instructions, latencies (strip taken to
//!   detector start, last strip taken to centroids published, publication to the pop on core
//!   0), dropped results, lapped strips, and core 0's busy share in `service()` and the drain.
//! - `Check`: every row of every strip against the reference (core 1), plus the strip
//!   sequence; with `contention`, core 0 meanwhile runs cache operations on buffers of its own
//!   without pause, in thread mode and from a 10 kHz timer interrupt.
//! - `Holds`: the lap test, holds from the reader's take stepped over the ring period.
//! - `Stall`: holds longer than the ring, with a guard over the memory around the list items
//!   and after the ring.
//! - `Cycles`: stop/start cycles of the control half while the sensor streams and core 1
//!   polls.
//!
//! At the end of each group (the control half dropped), core 0 checks that the reader writes
//! no list item any more (`Ring::links_sum` 50 ms apart) and that core 1 takes no strip.
//! Core 0 never waits on core 1 except for the mailbox and the clock request (both bounded).

use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use critical_section::Mutex;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::Blocking;
use esp_hal::handler;
use esp_hal::peripherals::{CPU_CTRL, TIMG1, VDMA_CH0};
use esp_hal::timer::PeriodicTimer;
use esp_hal::timer::timg::TimerGroup;
use esp32p4_csi::{Config, Csi, CsiStrips, StripControl, StripError};
use esp32p4_perf::cycles;
use espion_core1::{
    self as core1, CHECK, CheckSnap, Core1, Job, Monitor, Run, SkipChoice, log_summary,
    report_dead, report_trap,
};
use espion_vision::synth;
use log::{error, info};
use ov9281::TestPattern;
use static_cell::StaticCell;

use crate::camera::{self, BUFFER_LEN, BUFFERS, Cam, Sensor};
use crate::modes::{self, CamMode};
use crate::{soak, strip_soak};

/// The modes, by reference: indexing the `const` array itself would copy all of it.
const MODES: &[CamMode] = &modes::MODES;

/// RAW10 threshold in 10-bit units: the RAW8 one times 4, as the strip soak uses.
const THRESHOLD_RAW10: u16 = soak::THRESHOLD as u16 * 4;
/// How long core 0 waits for core 1 to take a reader from the mailbox.
const MAILBOX_TIMEOUT: Duration = Duration::from_millis(100);
/// The dark skip of the detector on core 1: PIE (enabled on core 1 by its entry; the scalar
/// skip if `Pie::enable` refuses) or `Scalar`.
const CORE1_SKIP: SkipChoice = SkipChoice::Pie;
/// How long a stop/start cycle waits for its first good frame.
const CYCLE_TIMEOUT: Duration = Duration::from_millis(300);
/// Stop-to-start pause of `Work::Cycles`: the rest of the sensor's frame (8.3 ms at 120 fps)
/// and the lanes back in LP-11, as `stop_mode` waits.
const CYCLE_LP11: Duration = Duration::from_millis(20);
/// The strip the lap and stall tests hold: the one starting at this row (mid-frame in every
/// mode: strips of 64 or 128 rows).
const HOLD_ROW: u16 = 256;

/// What a step runs.
#[derive(Clone, Copy)]
pub enum Work {
    /// The detector; `blobs`: a synthetic strip aiming at that many blobs per frame (`None`:
    /// the live strips).
    Detect { autoload: bool, blobs: Option<u16> },
    /// Every row against the reference.
    Check,
    /// The lap test: one run of the step's window per hold, from `from_us` to `to_us` in
    /// steps of `step_us`, holding one strip in `every` frames.
    Holds {
        from_us: u32,
        to_us: u32,
        step_us: u32,
        every: u32,
    },
    /// The stall test: one run of the step's window per hold, in milliseconds.
    Stall {
        holds_ms: &'static [u32],
        every: u32,
    },
    /// Stop/start cycles of the control half.
    Cycles(u32),
}

/// One step of a plan.
#[derive(Clone, Copy)]
pub struct Step {
    /// Index into `modes::MODES`.
    pub mode: usize,
    pub work: Work,
    pub window: Duration,
    /// Core 0 runs cache operations without pause meanwhile.
    pub contention: bool,
}

const fn step(mode: usize, work: Work, secs: u64) -> Step {
    Step {
        mode,
        work,
        window: Duration::from_secs(secs),
        contention: false,
    }
}

const LIVE: Work = Work::Detect {
    autoload: true,
    blobs: None,
};

/// Bring-up: the four 120 fps RAW8 modes, 30 s each, the detector live with the autoload.
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const BRINGUP: &[Step] = &[
    step(0, LIVE, 30),
    step(1, LIVE, 30),
    step(2, LIVE, 30),
    step(3, LIVE, 30),
];

/// Measurement on 1280x800 RAW8: 120 s live with the autoload, 30 s without, then synthetic
/// strips with 0 and 64 blobs per frame, 30 s each (the skip is [`CORE1_SKIP`]).
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const MEASURE: &[Step] = &[
    step(0, LIVE, 120),
    step(
        0,
        Work::Detect {
            autoload: false,
            blobs: None,
        },
        30,
    ),
    step(
        0,
        Work::Detect {
            autoload: true,
            blobs: Some(0),
        },
        30,
    ),
    step(
        0,
        Work::Detect {
            autoload: true,
            blobs: Some(64),
        },
        30,
    ),
];

/// Pixel check of the four 120 fps modes, 30 s each.
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const CHECK_PLAN: &[Step] = &[
    step(0, Work::Check, 30),
    step(1, Work::Check, 30),
    step(2, Work::Check, 30),
    step(3, Work::Check, 30),
];

/// The fault tests on 1280x800 RAW8: the lap sweep (1.5 to 2.5 ms in 50 us steps, 2 s
/// each), then the stall cases (5 s each, a hold every 20 frames).
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const FAULTS: &[Step] = &[
    step(
        0,
        Work::Holds {
            from_us: 1500,
            to_us: 2500,
            step_us: 50,
            every: 3,
        },
        2,
    ),
    step(
        0,
        Work::Stall {
            holds_ms: &[1, 5, 30, 100],
            every: 20,
        },
        5,
    ),
];

/// 1000 stop/start cycles on 1280x800 RAW8, then a 10 s check.
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const CYCLES: &[Step] = &[step(0, Work::Cycles(1000), 0), step(0, Work::Check, 10)];

/// The contention run on 1280x800 RAW8, with core 0 running cache operations without pause
/// (thread mode and a 10 kHz timer interrupt): the detector live for 2 minutes, then the
/// pixel check for 10 minutes.
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const CONTENTION: &[Step] = &[
    Step {
        contention: true,
        ..step(0, LIVE, 120)
    },
    Step {
        contention: true,
        ..step(0, Work::Check, 600)
    },
];

/// The long run: the pixel check on 1280x800 RAW8 for 10 minutes, core 0 otherwise idle.
#[allow(dead_code, reason = "alternative plan, selected by editing `PLAN`")]
const LONG: &[Step] = &[step(0, Work::Check, 600)];

/// The plan `run` executes.
const PLAN: &[Step] = BRINGUP;

/// The sensor settings: those of the camera section's plan; the random test pattern for the
/// steps that check pixels.
const SENSOR: &Sensor = &crate::CAMERA_PLAN.sensor;
const PATTERN_SENSOR: Sensor = Sensor {
    pattern: TestPattern::Random,
    ..crate::CAMERA_PLAN.sensor
};

const fn checks(work: &Work) -> bool {
    !matches!(work, Work::Detect { .. })
}

/// The plan has a step that holds strips past the ring (it needs the guard).
const fn needs_guard(plan: &[Step]) -> bool {
    let mut i = 0;
    while i < plan.len() {
        if matches!(plan[i].work, Work::Stall { .. }) {
            return true;
        }
        i += 1;
    }
    false
}

/// The synthetic strip, the stall test's guard, or neither, in the internal buffer.
enum Internal {
    Synth(&'static mut [u8]),
    Guard(strip_soak::Guard),
}

/// The references of the steps that check pixels, by mode index.
struct Refs {
    check: &'static strip_soak::StripCheck,
    modes: [Option<usize>; 4],
}

impl Refs {
    fn rows(&self, mode: usize) -> Option<&'static [u32]> {
        let slot = self.modes.iter().position(|&m| m == Some(mode))?;
        self.check.reference_rows(slot)
    }
}

static STRIP_CHECK: StaticCell<strip_soak::StripCheck> = StaticCell::new();

/// Runs [`PLAN`]. Does nothing unless `ON` (`RUN_CORE1`), a constant checked before the first
/// await. Everything that names the big statics (core 1's stack and start, the result queue,
/// the detector, the ring) runs before that first await too, so with `ON` false the compiler
/// drops it (the later states of this future cannot be pruned, the first one can).
pub async fn run<const ON: bool>(
    cam: &mut Cam,
    mut vdma: VDMA_CH0<'static>,
    cpu: CPU_CTRL<'static>,
    timg1: TIMG1<'static>,
    psram: &'static mut [u8],
    internal: &'static mut [u8],
) {
    if !ON {
        return;
    }
    let Some((mut ctx, ring, frames, words)) = setup(cpu, timg1, psram, internal) else {
        return;
    };
    if !camera::power_up().await {
        return;
    }
    if PLAN.iter().any(|s| checks(&s.work)) {
        let Some(refs) = references(cam, vdma.reborrow(), frames, words).await else {
            return;
        };
        ctx.refs = Some(refs);
    }
    steps(cam, vdma, ring, &mut ctx).await;
    note("core 1 runs done");
}

/// What `run` sets up before its first await: core 1 started, the ring, the internal buffer
/// (the synthetic strip or the stall test's guard, and the contention's 8 KiB), the PSRAM
/// split into the reference frame buffers, the reference words and the contention buffer.
#[inline(never)]
fn setup(
    cpu: CPU_CTRL<'static>,
    timg1: TIMG1<'static>,
    psram: &'static mut [u8],
    internal: &'static mut [u8],
) -> Option<(Ctx, strip_soak::Ring, &'static mut [u8], &'static mut [u8])> {
    let (det, scratch, synth) = soak::take_detector();
    let (internal, links, hammer_int) = split_internal(internal)?;
    let ring = strip_soak::Ring::take(links);
    let Some(core1) = core1::start(cpu, det, scratch, CORE1_SKIP) else {
        fail("could not start", &());
        return None;
    };
    note("started (bare: no scheduler, no interrupts)");
    let (frames, rest) = psram.split_at_mut(BUFFERS * BUFFER_LEN);
    let (words, rest) = rest.split_at_mut(4 * strip_soak::CHECK_WORDS);
    hammer_init(timg1, hammer_int, rest);
    let ctx = Ctx {
        core1,
        ring: ring.addrs(),
        refs: None,
        internal,
        synth,
        mhz: esp_hal::clock::cpu_clock().as_mhz(),
    };
    Some((ctx, ring, frames, words))
}

/// The internal buffer split: the synthetic strip or the stall test's guard (with the list
/// items inside it), and the contention's 8 KiB at the end.
type InternalSplit = (Internal, Option<&'static mut [u8]>, &'static mut [u8]);

#[inline(never)]
fn split_internal(internal: &'static mut [u8]) -> Option<InternalSplit> {
    let Some(at) = internal.len().checked_sub(HAMMER_INT_BYTES) else {
        fail("internal buffer too small", &internal.len());
        return None;
    };
    let (internal, hammer_int) = internal.split_at_mut(at);
    if !needs_guard(PLAN) {
        return Some((Internal::Synth(internal), None, hammer_int));
    }
    let Some((guard, links)) = strip_soak::guarded_links(internal) else {
        fail("internal buffer too small for the guard", &at);
        return None;
    };
    Some((Internal::Guard(guard), Some(links), hammer_int))
}

/// The steps of [`PLAN`], one receiver per group of steps with the same strip size.
async fn steps(
    cam: &mut Cam,
    mut vdma: VDMA_CH0<'static>,
    mut ring: strip_soak::Ring,
    ctx: &mut Ctx,
) {
    let mut rest = PLAN;
    while !rest.is_empty() {
        let (group, next) = next_group(rest);
        rest = next;
        if !ctx.core1.wait_mailbox(MAILBOX_TIMEOUT).await {
            break;
        }
        let Some(control) = group_receiver(vdma.reborrow(), &mut ring, ctx, group) else {
            break;
        };
        if !group_run(cam, control, ctx, group).await {
            break;
        }
        detached(&ring).await;
    }
}

/// The first steps of `plan` with the same strip size, and the rest.
fn next_group(plan: &[Step]) -> (&[Step], &[Step]) {
    let bytes = |s: &Step| strip_soak::strip_bytes(&camera::csi_config(&MODES[s.mode].mode));
    let first = plan.first().map(bytes);
    let n = plan.iter().take_while(|s| Some(bytes(s)) == first).count();
    plan.split_at(n)
}

/// The split receiver of `group` on `ring`, its reader posted to core 1 (the guard's ring
/// tail set first).
#[inline(never)]
fn group_receiver<'d>(
    vdma: VDMA_CH0<'d>,
    ring: &mut strip_soak::Ring,
    ctx: &mut Ctx,
    group: &[Step],
) -> Option<StripControl<'d>> {
    let config = camera::csi_config(&MODES[group.first()?.mode].mode);
    if let Internal::Guard(g) = &mut ctx.internal {
        let (addr, len) = ring.tail(&config);
        g.set_tail(addr, len);
    }
    // SAFETY: the previous group's control half, and with it its receiver, was dropped at
    // the end of `group_run`; its reader on core 1 is inert from then on.
    unsafe { receiver(vdma, ring, &config, ctx.core1) }
}

/// What the steps share.
struct Ctx {
    core1: &'static mut Core1,
    /// The ring's strip buffers and list items ([`strip_soak::Ring::addrs`]).
    ring: (usize, usize),
    refs: Option<Refs>,
    internal: Internal,
    synth: &'static mut synth::Synth,
    mhz: u32,
}

/// Whole frames of the random test pattern for every mode a step checks: the reference row
/// checksums. `None` if a reference failed (logged).
async fn references(
    cam: &mut Cam,
    vdma: VDMA_CH0<'_>,
    frames: &'static mut [u8],
    words: &'static mut [u8],
) -> Option<Refs> {
    let (mut csi, check, modes) = reference_receiver(vdma, frames, words)?;
    for (slot, mode) in modes.iter().enumerate() {
        let Some(m) = *mode else { break };
        if !reference(cam, &mut csi, check, slot, &MODES[m]).await {
            return None;
        }
    }
    drop(csi);
    Some(Refs { check, modes })
}

/// The check modes of [`PLAN`] (at most 4), the check's state and the whole-frame receiver.
#[inline(never)]
fn reference_receiver<'d>(
    vdma: VDMA_CH0<'d>,
    frames: &'static mut [u8],
    words: &'static mut [u8],
) -> Option<(
    Csi<'d>,
    &'static mut strip_soak::StripCheck,
    [Option<usize>; 4],
)> {
    let mut modes = [None; 4];
    for s in PLAN.iter().filter(|s| checks(&s.work)) {
        if !modes.contains(&Some(s.mode))
            && let Some(free) = modes.iter_mut().find(|m| m.is_none())
        {
            *free = Some(s.mode);
        }
    }
    // SAFETY: every bit pattern is a valid `u32`.
    let (_, words, _) = unsafe { words.align_to_mut::<u32>() };
    let check = STRIP_CHECK.init(strip_soak::StripCheck::new(
        Duration::from_secs(1),
        words,
        None,
    )?);
    let (b0, rest) = frames.split_at_mut(BUFFER_LEN);
    let (b1, b2) = rest.split_at_mut(BUFFER_LEN);
    let first = &MODES[modes[0]?].mode;
    match Csi::new(vdma, [b0, b1, b2], camera::csi_config(first)) {
        Ok(c) => Some((c, check, modes)),
        Err(e) => {
            fail("Csi::new", &e);
            None
        }
    }
}

/// The reference of `mode` in slot `slot`; `false` if it failed (logged).
async fn reference(
    cam: &mut Cam,
    csi: &mut Csi<'_>,
    check: &mut strip_soak::StripCheck,
    slot: usize,
    mode: &CamMode,
) -> bool {
    check.begin_reference(slot, mode);
    if !reference_stream(cam, csi, mode).await {
        return false;
    }
    let ok = camera::reference_frames(csi, check, slot).await;
    if let Err(e) = cam.stop().await {
        fail("stream off", &e);
    }
    Timer::after_millis(20).await;
    csi.stop();
    check.log_reference(slot);
    if !ok || check.reference_rows(slot).is_none() {
        fail("no reference", &mode.name);
        return false;
    }
    true
}

/// Sensor set-up with the random test pattern, receiver start and stream-on.
async fn reference_stream(cam: &mut Cam, csi: &mut Csi<'_>, mode: &CamMode) -> bool {
    if let Err(e) = camera::sensor_setup(cam, &mode.mode, &PATTERN_SENSOR).await {
        fail("sensor set-up", &e);
        return false;
    }
    if let Err(e) = csi
        .set_config(camera::csi_config(&mode.mode))
        .and_then(|()| csi.start())
    {
        fail("Csi start", &e);
        return false;
    }
    Timer::after_millis(1).await;
    if let Err(e) = cam.start().await {
        fail("stream on", &e);
        csi.stop();
        return false;
    }
    true
}

#[inline(never)]
fn fail(what: &str, detail: &dyn core::fmt::Debug) {
    error!("core 1: {what}: {detail:?}");
}

#[inline(never)]
fn note(what: &str) {
    info!("core 1: {what}");
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
    core1.hand_over(cs)
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

/// After a group: the control half is dropped, so the reader must write no list item and
/// hand out no strip any more while core 1 keeps polling.
async fn detached(ring: &strip_soak::Ring) {
    let (sum, kept, beat) = (ring.links_sum(), CHECK.snap().kept, core1::heartbeat());
    Timer::after_millis(50).await;
    let (sum2, kept2, beat2) = (ring.links_sum(), CHECK.snap().kept, core1::heartbeat());
    info!(
        "core 1: control half dropped: list items {} over 50 ms ({sum:#010x} -> {sum2:#010x}), strips checked meanwhile {}, heartbeat +{}",
        if sum == sum2 { "unchanged" } else { "CHANGED" },
        kept2.wrapping_sub(kept),
        beat2.wrapping_sub(beat)
    );
}

/// Every step of `group` on `control`, dropped at the end. `false` if core 1 stopped.
async fn group_run(
    cam: &mut Cam,
    mut control: StripControl<'_>,
    ctx: &mut Ctx,
    group: &[Step],
) -> bool {
    for s in group {
        let alive = match s.work {
            Work::Detect { .. } | Work::Check => {
                let hold = None;
                sub_run(cam, &mut control, ctx, s, hold).await
            }
            Work::Holds {
                from_us,
                to_us,
                step_us,
                every,
            } => {
                let mut us = from_us;
                let mut alive = true;
                while alive && us <= to_us {
                    alive = sub_run(cam, &mut control, ctx, s, Some((us, every))).await;
                    us += step_us.max(1);
                }
                alive
            }
            Work::Stall { holds_ms, every } => {
                let mut alive = true;
                for &ms in holds_ms {
                    if let Internal::Guard(g) = &ctx.internal {
                        g.fill();
                    }
                    alive = sub_run(cam, &mut control, ctx, s, Some((ms * 1000, every))).await;
                    if let Internal::Guard(g) = &ctx.internal {
                        log_guard(ms, &g.check());
                    }
                    if !alive {
                        break;
                    }
                }
                alive
            }
            Work::Cycles(n) => cycles_run(cam, &mut control, ctx, s, n).await,
        };
        if !alive {
            return false;
        }
    }
    true
}

/// The run parameters of `step` (with the test hold `hold`: microseconds after the take, one
/// frame in `every`).
#[inline(never)]
fn set_run(ctx: &mut Ctx, s: &Step, hold: Option<(u32, u32)>) -> bool {
    let mode = &MODES[s.mode];
    let lines = strip_soak::lines(&camera::csi_config(&mode.mode));
    let strip_us = strip_soak::budget_us(mode) * u32::from(lines) / u32::from(mode.mode.height());
    let threshold = if mode.raw10() {
        THRESHOLD_RAW10
    } else {
        u16::from(soak::THRESHOLD)
    };
    let (job, autoload, blobs) = match s.work {
        Work::Detect { autoload, blobs } => (Job::Detect, autoload, blobs),
        _ => (Job::Check, false, None),
    };
    let synth = match (blobs, &mut ctx.internal) {
        (Some(target), Internal::Synth(buf)) => {
            let (_, per_frame) = strip_soak::fill_synthetic(buf, ctx.synth, mode, target, lines);
            info!("core 1: synthetic strip, {per_frame} blobs per frame");
            // SAFETY: core 1 reads the buffer only during this run; core 0 writes it again
            // only for a later run, while the strip path is stopped.
            Some(unsafe { core::slice::from_raw_parts(buf.as_ptr(), buf.len()) })
        }
        (Some(_), Internal::Guard(_)) => {
            error!("core 1: no buffer for a synthetic strip in a plan with a stall step");
            return false;
        }
        (None, _) => None,
    };
    let reference = match (job, &ctx.refs) {
        (Job::Check, Some(r)) => match r.rows(s.mode) {
            Some(rows) => rows,
            None => {
                error!("core 1: no reference for {}", mode.name);
                return false;
            }
        },
        _ => &[],
    };
    ctx.core1.set_run(&Run {
        job,
        width: mode.mode.width(),
        raw10: mode.raw10(),
        threshold,
        late_cycles: strip_us * ctx.mhz,
        autoload,
        synth,
        reference,
        line_bytes: mode.mode.bytes_per_line(),
        hold: hold.map(|(us, every)| (us * ctx.mhz, HOLD_ROW, every)),
        ring: ring_info(ctx.ring, mode),
    });
    log_step(mode, s, lines, strip_us, hold);
    true
}

/// The ring as core 1's test holds see it: strip buffers, list items, bytes per strip and
/// strips for `mode`.
#[inline(never)]
fn ring_info((strips, links): (usize, usize), mode: &CamMode) -> (usize, usize, u32, u32) {
    let config = camera::csi_config(&mode.mode);
    (
        strips,
        links,
        strip_soak::strip_bytes(&config) as u32,
        strip_soak::ring_strips(&config) as u32,
    )
}

/// One run of `step`: parameters, start, the monitored window, stop, summary. `false` if core
/// 1 stopped.
async fn sub_run(
    cam: &mut Cam,
    control: &mut StripControl<'_>,
    ctx: &mut Ctx,
    s: &Step,
    hold: Option<(u32, u32)>,
) -> bool {
    if !set_run(ctx, s, hold) {
        return true;
    }
    let mode = &MODES[s.mode];
    if !start_mode(cam, control, mode, s).await {
        return true;
    }
    let check0 = CHECK.snap();
    let alive = monitor(control, ctx, s).await;
    stop_mode(cam, control, ctx.core1).await;
    let check = CHECK.snap().since(&check0);
    log_summary(mode.name, control, checks(&s.work).then_some(&check));
    alive
}

/// Sensor set-up, receiver start, stream-on. `false` if something failed (logged).
async fn start_mode(
    cam: &mut Cam,
    control: &mut StripControl<'_>,
    mode: &CamMode,
    s: &Step,
) -> bool {
    let sensor = if checks(&s.work) {
        &PATTERN_SENSOR
    } else {
        SENSOR
    };
    if let Err(e) = camera::sensor_setup(cam, &mode.mode, sensor).await {
        fail("sensor set-up", &e);
        return false;
    }
    // `sensor_setup` sets a test pattern but never clears one an earlier step left on.
    if sensor.pattern == TestPattern::Off
        && let Err(e) = cam.set_test_pattern(TestPattern::Off).await
    {
        fail("test pattern off", &e);
        return false;
    }
    if !start_receiver(control, mode) {
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
fn start_receiver(control: &mut StripControl<'_>, mode: &CamMode) -> bool {
    let config = camera::csi_config(&mode.mode);
    match control
        .set_config(config, strip_soak::lines(&config))
        .and_then(|()| {
            control.reset_stats();
            control.start()
        }) {
        Ok(()) => true,
        Err(e) => {
            error!("core 1: receiver start: {e:?}");
            false
        }
    }
}

/// Stream-off, then the receiver stopped once the current frame has ended.
async fn stop_mode(cam: &mut Cam, control: &mut StripControl<'_>, core1: &mut Core1) {
    if let Err(e) = cam.stop().await {
        error!("core 1: stream off: {e:?}");
    }
    // Let the current frame finish so the lanes go back to LP-11.
    Timer::after_millis(20).await;
    control.stop();
    core1.note_stop();
}

/// Counters of `Work::Cycles`.
#[derive(Default)]
struct Cycles {
    n: u32,
    busy: u32,
    others: u32,
    late: u32,
    first_max: u64,
    first_total: u64,
    beat: u32,
    /// Longest `StripControl::start` call and longest whole cycle, us.
    start_max: u64,
    cycle_max: u64,
    /// Cycles finished.
    done: u32,
}

impl Cycles {
    /// Starts the control half for cycle `k`: `false` if it refused (counted).
    #[inline(never)]
    fn start(&mut self, control: &mut StripControl<'_>, k: u32) -> bool {
        let t0 = Instant::now();
        let r = control.start();
        self.start_max = self.start_max.max(t0.elapsed().as_micros());
        match r {
            Ok(()) => true,
            Err(StripError::ReaderBusy) => {
                self.busy += 1;
                false
            }
            Err(e) => {
                self.others += 1;
                if self.others < 4 {
                    fail("cycle start", &(k, e));
                }
                false
            }
        }
    }

    /// The first good frame of a cycle came after `took` us, or not (`None`).
    fn first_frame(&mut self, took: Option<u64>) {
        match took {
            Some(us) => {
                self.first_max = self.first_max.max(us);
                self.first_total += us;
            }
            None => self.late += 1,
        }
    }

    /// Once a second: a progress line, core 1's trap record and heartbeat. `false` if it
    /// stopped (logged).
    #[inline(never)]
    fn alive(&mut self) -> bool {
        let ok = self
            .done
            .saturating_sub(self.late + self.busy + self.others);
        info!(
            "core 1: cycles {}/{}: ReaderBusy {}, other errors {}, no good frame {}, first good frame mean {} max {} us, start max {} us, cycle max {} us, frames good {}, heartbeat {}",
            self.done,
            self.n,
            self.busy,
            self.others,
            self.late,
            self.first_total / u64::from(ok.max(1)),
            self.first_max,
            self.start_max,
            self.cycle_max,
            CHECK.snap().frames_good,
            core1::heartbeat().wrapping_sub(self.beat),
        );
        if let Some(t) = core1::trap() {
            report_trap(&t);
            return false;
        }
        let b = core1::heartbeat();
        if b == self.beat {
            report_dead("heartbeat stopped for 1 s");
            return false;
        }
        self.beat = b;
        true
    }

    #[inline(never)]
    fn log(&self, mode: &CamMode, d: &CheckSnap) {
        let ok = self.n.saturating_sub(self.late + self.busy + self.others);
        info!(
            "core 1: {} stop/start cycles: {}, ReaderBusy {}, other start errors {}, without a good frame within {} ms {}, first good frame mean {} max {} us",
            mode.name,
            self.n,
            self.busy,
            self.others,
            CYCLE_TIMEOUT.as_millis(),
            self.late,
            self.first_total / u64::from(ok.max(1)),
            self.first_max,
        );
        let pass = self.busy == 0
            && self.others == 0
            && self.late == 0
            && d.old_kept == 0
            && d.rows_bad == 0;
        info!(
            "core 1: {} cycles: strips checked {} (dropped {}), rows bad {}, strips of a stopped run released true {} (false {}), frames good {}/{}: {}",
            mode.name,
            d.kept,
            d.dropped,
            d.rows_bad,
            d.old_kept,
            d.old_dropped,
            d.frames_good,
            d.frames,
            if pass { "PASS" } else { "FAIL" }
        );
    }
}

/// `Work::Cycles`: `n` stop/start cycles of the control half while core 1 polls. Each cycle:
/// `start`, stream on, until the first good frame (at most [`CYCLE_TIMEOUT`]) plus 0 to 9
/// ms, stream off and at once `stop` (the sensor still sends the rest of its frame), then
/// [`CYCLE_LP11`] for the lanes to go back to LP-11. `stop` holds the D-PHY in reset, and it
/// syncs again only from LP-11 (with the stream on during `start`, no data at all).
async fn cycles_run(
    cam: &mut Cam,
    control: &mut StripControl<'_>,
    ctx: &mut Ctx,
    s: &Step,
    n: u32,
) -> bool {
    if !set_run(ctx, s, None) {
        return true;
    }
    let mode = &MODES[s.mode];
    if !start_mode(cam, control, mode, s).await {
        return true;
    }
    let c0 = CHECK.snap();
    let Some(c) = cycle_loop(cam, control, ctx.core1, n).await else {
        return false;
    };
    c.log(mode, &CHECK.snap().since(&c0));
    true
}

/// The `n` cycles (the first already started by `start_mode`); `None` if core 1 stopped
/// (logged).
async fn cycle_loop(
    cam: &mut Cam,
    control: &mut StripControl<'_>,
    core1: &mut Core1,
    n: u32,
) -> Option<Cycles> {
    let mut c = Cycles {
        n,
        beat: core1::heartbeat(),
        ..Cycles::default()
    };
    let mut next_beat = Instant::now() + Duration::from_secs(1);
    for k in 0..n {
        let beat_due = one_cycle(cam, control, &mut c, k, next_beat).await;
        core1.note_stop();
        if beat_due {
            next_beat += Duration::from_secs(1);
            if !c.alive() {
                return None;
            }
        }
        Timer::after(CYCLE_LP11).await;
    }
    Some(c)
}

/// Cycle `k` of `Work::Cycles` up to and with the stop (the caller notes it to core 1).
/// Returns whether the once-a-second check is due (the link counters are then logged).
async fn one_cycle(
    cam: &mut Cam,
    control: &mut StripControl<'_>,
    c: &mut Cycles,
    k: u32,
    next_beat: Instant,
) -> bool {
    let tc = Instant::now();
    if k > 0 {
        if !c.start(control, k) {
            c.done += 1;
            return Instant::now() >= next_beat;
        }
        Timer::after_millis(1).await;
        if let Err(e) = cam.start().await {
            fail("cycle stream on", &(k, e));
            c.others += 1;
        }
    }
    let t0 = Instant::now();
    let good0 = frames_good();
    while frames_good() == good0 && t0.elapsed() < CYCLE_TIMEOUT {
        Timer::after_millis(1).await;
        control.service();
    }
    let took = t0.elapsed().as_micros();
    c.first_frame((frames_good() != good0).then_some(took));
    Timer::after_millis(u64::from(k.wrapping_mul(7) % 10)).await;
    let beat_due = Instant::now() >= next_beat;
    if beat_due {
        log_cycle_link(control);
    }
    if let Err(e) = cam.stop().await {
        fail("cycle stream off", &(k, e));
    }
    control.stop();
    c.done += 1;
    c.cycle_max = c.cycle_max.max(tc.elapsed().as_micros());
    beat_due
}

/// Good frames of the pixel check so far.
#[inline(never)]
fn frames_good() -> u32 {
    CHECK.snap().frames_good
}

/// The receiver's counters and link state, while running (cycle diagnostics).
#[inline(never)]
fn log_cycle_link(control: &StripControl<'_>) {
    let st = control.stats();
    info!(
        "core 1:   link: strips done {} delivered {} discarded {}, frames done {} complete {} truncated {} skipped {}, stalls {} overruns {} length errors {} last block {} DMA errors {} ISR runs {}",
        st.strips_done,
        st.strips_delivered,
        st.strips_discarded,
        st.frames_done,
        st.frames_complete,
        st.frames_truncated,
        st.frames_skipped,
        st.dma_stalls,
        st.bridge_overruns,
        st.length_errors,
        st.last_block_items,
        st.dma_errors,
        st.isr_runs,
    );
}

/// The step's monitor loop (see the module docs). `false` if core 1 stopped or trapped.
async fn monitor(control: &mut StripControl<'_>, ctx: &mut Ctx, s: &Step) -> bool {
    let job = if checks(&s.work) {
        Job::Check
    } else {
        Job::Detect
    };
    let mut mon = Monitor::new(ctx.core1, control, job);
    let start = Instant::now();
    let mut next = start + Duration::from_secs(1);
    if s.contention {
        hammer_isr(true);
    }
    let budget = 1000 * ctx.mhz;
    let alive = loop {
        if s.contention {
            hammer_for(budget);
            embassy_futures::yield_now().await;
        } else {
            Timer::after_millis(1).await;
        }
        mon.poll(control, ctx.core1);
        let now = Instant::now();
        if now < next {
            continue;
        }
        if !mon.second(control, ctx.core1).await {
            break false;
        }
        next += Duration::from_secs(1);
        if now >= start + s.window {
            break true;
        }
    };
    if s.contention {
        hammer_isr(false);
    }
    mon.end(MODES[s.mode].name);
    alive
}

// ── Cache contention on core 0 ──────────────────────────────────────────────────────────

/// Core 0's buffers for the contention: 8 KiB at the end of the internal buffer (thread mode
/// and the interrupt handler use one half each) and one in PSRAM of two whole 1280x800 RAW8
/// frames (the handler uses the first, thread mode the second).
const HAMMER_INT_BYTES: usize = 8 * 1024;
static HAMMER_INT: AtomicUsize = AtomicUsize::new(0);
static HAMMER_PSRAM: AtomicUsize = AtomicUsize::new(0);
/// Bytes of the PSRAM buffer per user (one whole frame).
const HAMMER_FRAME: usize = 1280 * 800;
/// Cache operations core 0 ran for the contention (thread mode and handler).
static HAMMER_OPS: AtomicU32 = AtomicU32::new(0);
static HAMMER_K: AtomicU32 = AtomicU32::new(0);
static HAMMER_ISR_K: AtomicU32 = AtomicU32::new(0);
static HAMMER_TIMER: Mutex<RefCell<Option<PeriodicTimer<'static, Blocking>>>> =
    Mutex::new(RefCell::new(None));

/// Takes the buffers and sets up TIMG1's timer 0 (10 kHz, interrupt not yet listened to).
#[inline(never)]
fn hammer_init(timg1: TIMG1<'static>, internal: &'static mut [u8], psram: &'static mut [u8]) {
    let line = esp32p4_cache::line_size(psram.as_ptr() as usize);
    let at = psram.as_ptr().align_offset(line);
    if psram.len() < at + 2 * HAMMER_FRAME.next_multiple_of(line) {
        error!("core 1: PSRAM too small for the contention buffers");
        return;
    }
    HAMMER_PSRAM.store(psram.as_ptr() as usize + at, Ordering::Relaxed);
    HAMMER_INT.store(internal.as_ptr() as usize, Ordering::Relaxed);
    let mut t = PeriodicTimer::new(TimerGroup::new(timg1).timer0);
    t.set_interrupt_handler(hammer_handler);
    if t.start(esp_hal::time::Duration::from_micros(100)).is_err() {
        error!("core 1: contention timer did not start");
        return;
    }
    critical_section::with(|cs| HAMMER_TIMER.borrow_ref_mut(cs).replace(t));
}

fn hammer_isr(on: bool) {
    critical_section::with(|cs| {
        if let Some(t) = HAMMER_TIMER.borrow_ref_mut(cs).as_mut() {
            if on {
                t.listen();
            } else {
                t.unlisten();
            }
        }
    });
}

/// One cache operation of the sequence `k` on the buffers of `half` (0 handler, 1 thread):
/// every size from one line to a whole frame, invalidate, write-back and both, with a few
/// lines dirtied first.
fn hammer_op(k: u32, half: usize) {
    let int = HAMMER_INT.load(Ordering::Relaxed);
    let ps = HAMMER_PSRAM.load(Ordering::Relaxed);
    if int == 0 || ps == 0 {
        return;
    }
    let pl = esp32p4_cache::line_size(ps);
    let int = int + half * HAMMER_INT_BYTES / 2;
    let ps = ps + half * HAMMER_FRAME.next_multiple_of(pl);
    // (base, len): internal one line, 512 B, 4 KiB; PSRAM one line, 4 KiB, 64 KiB, a frame.
    let (base, len) = match (k / 3) % 7 {
        0 => (int, 64),
        1 => (int, 512),
        2 => (int, HAMMER_INT_BYTES / 2),
        3 => (ps, pl),
        4 => (ps, 4096),
        5 => (ps, 64 * 1024),
        // The whole frame: one in 21 operations in thread mode, rarer in the handler.
        _ if half == 0 && !(k / 21).is_multiple_of(8) => (ps, 4096),
        _ => (ps, HAMMER_FRAME.next_multiple_of(pl)),
    };
    // SAFETY: the buffers belong to the contention alone; a few bytes dirtied per operation.
    unsafe {
        for i in 0..4 {
            let p = (base + (i * 64) % len) as *mut u8;
            p.write_volatile(p.read_volatile().wrapping_add(1));
        }
    }
    match k % 3 {
        0 => esp32p4_cache::invalidate(base, len),
        1 => esp32p4_cache::write_back(base, len),
        _ => esp32p4_cache::writeback_invalidate(base, len),
    }
    HAMMER_OPS.fetch_add(1, Ordering::Relaxed);
}

/// Cache operations in thread mode for `budget` cycles.
fn hammer_for(budget: u32) {
    let t0 = cycles();
    while cycles().wrapping_sub(t0) < budget {
        hammer_op(HAMMER_K.fetch_add(1, Ordering::Relaxed), 1);
    }
}

#[handler]
fn hammer_handler() {
    critical_section::with(|cs| {
        if let Some(t) = HAMMER_TIMER.borrow_ref_mut(cs).as_mut() {
            t.clear_interrupt();
        }
    });
    hammer_op(HAMMER_ISR_K.fetch_add(1, Ordering::Relaxed), 0);
}

// ── Log lines ───────────────────────────────────────────────────────────────────────────

#[inline(never)]
fn log_step(mode: &CamMode, s: &Step, lines: u16, strip_us: u32, hold: Option<(u32, u32)>) {
    let what = match s.work {
        Work::Detect {
            autoload,
            blobs: None,
        } => {
            if autoload {
                "detect live, autoload"
            } else {
                "detect live, no autoload"
            }
        }
        Work::Detect { .. } => "detect synthetic, autoload",
        Work::Check => "pixel check",
        Work::Holds { .. } => "lap test",
        Work::Stall { .. } => "stall test",
        Work::Cycles(_) => "stop/start cycles",
    };
    let (hold_us, every) = hold.unwrap_or((0, 0));
    info!(
        "core 1: {}: {what} for {} s{}, {lines}-row strips (one every {strip_us} us), hold {hold_us} us of row {HOLD_ROW} every {every} frames, skip {:?}",
        mode.name,
        s.window.as_secs(),
        if s.contention {
            ", core 0 cache contention"
        } else {
            ""
        },
        core1::skip_used()
    );
}

#[inline(never)]
fn log_guard(ms: u32, g: &strip_soak::GuardReport) {
    info!(
        "core 1: guard after the {ms} ms holds: {g}: {}",
        if g.written() == 0 { "PASS" } else { "FAIL" }
    );
}
