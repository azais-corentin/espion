//! `Service::BenchVision`, the camera side: the blob detector (or the pixel check) on a bare
//! core 1 (crate `espion-core1`) on live 64-row strips of 1280x800 RAW8 at 120 fps, while the
//! bench service (`bench`) sends and receives on this core (performance plan step 5.4, and
//! the check of step 5.1). README, "Co-load".
//!
//! The host drives it through the bench's TX port: a `V` request ([`request`]) stops the run
//! in progress and starts the one asked for (the detector or the pixel check, the L1 data
//! autoload over each strip on or off), or only stops. The first request that starts a run
//! also takes the pixel check's reference and starts core 1:
//!
//! 1. the reference: a whole-frame receiver (`Csi`) on three of the camera's PSRAM buffers,
//!    the sensor's random test pattern (the same in every frame, every row different), the
//!    row checksums of the first whole frame after two, compared with seven more
//!    (`espion_core1::row_sum`); then that receiver is dropped;
//! 2. core 1 started (`espion_core1::start`, PIE on core 1, its own token);
//! 3. the strip receiver on a ring of [`STRIPS`] strips in internal RAM, split, its reader
//!    handed to core 1, the CSI interrupt wrappers of `irq` installed.
//!
//! While a run goes on, this task calls `StripControl::service()` and drains core 1's frame
//! results every millisecond and logs core 1's figures once a second (`espion_core1::Monitor`,
//! the lines `poc-p4-eth` logs); a trap record or a stopped heartbeat ends the run and refuses
//! the later requests. Ending a run logs its totals and the strip path's counters.
//!
//! Prefetch: core 1 owns the L1 data autoload registers while its run has the autoload on
//! (`crate::prefetch::lend_l1_autoload`); the bench's prefetch modes that write them are
//! refused meanwhile (see `crate::prefetch`).

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::peripherals::{CPU_CTRL, I2C0, VDMA_CH0};
use esp32p4_csi::{Config, Csi, CsiStrips, StripControl, StripLink};
use espion_core1::{
    self as core1, CHECK, CheckSnap, Core1, Job, MAX_BLOBS, Monitor, Run, SkipChoice, log_summary,
    row_sum,
};
use espion_vision::{Blob, DefaultDetector};
use log::{error, info, warn};
use ov9281::TestPattern;
use static_cell::ConstStaticCell;

use crate::camera::{self, BUFFER_LEN, Controls, MODE, Sensor};
use crate::{cpu, irq, prefetch};

/// Rows per strip: `poc-p4-eth`'s strip height for 1280x800 RAW8 (`strip_soak::lines`), a
/// strip of 80 KiB every 0.67 ms at 120 fps.
const LINES: u16 = 64;
/// Strips in the ring. `poc-p4-eth` uses 4; 3 leave room in internal RAM for the bench's
/// buffers and the main stack. Core 1 has (STRIPS - 1) strip periods, 1.33 ms, to take a
/// strip before the DMA writes it again (`esp32p4-csi` README, "A reader that falls behind").
pub const STRIPS: usize = 3;
/// The receiver's configuration: [`MODE`], 1280x800 RAW8 at 120 fps.
const CONFIG: Config = camera::csi_config(&MODE);
const STRIP_BYTES: usize = CsiStrips::strip_bytes(&CONFIG, LINES);
const HEIGHT: usize = MODE.height() as usize;
const _: () = assert!(LINES.is_multiple_of(CsiStrips::line_step(&CONFIG)));
/// Foreground threshold of the detector, as `poc-p4-eth` (`soak::THRESHOLD`).
const THRESHOLD: u16 = 128;
/// Whole frames of the reference after the two skipped: the first one is it, the others must
/// equal it.
const REFERENCE_FRAMES: u32 = 8;
/// The run's name in the log lines.
const NAME: &str = "1280x800 RAW8";

#[repr(C, align(64))]
struct RingMem([[u8; STRIP_BYTES]; STRIPS]);

static RING: ConstStaticCell<RingMem> = ConstStaticCell::new(RingMem([[0; STRIP_BYTES]; STRIPS]));
static LINKS: ConstStaticCell<[StripLink; STRIPS]> = ConstStaticCell::new([StripLink::NEW; STRIPS]);
static DETECTOR: ConstStaticCell<DefaultDetector> = ConstStaticCell::new(DefaultDetector::new());
static BLOBS: ConstStaticCell<[Blob; MAX_BLOBS]> = ConstStaticCell::new(
    [Blob {
        x: 0,
        y: 0,
        area: 0,
        peak: 0,
    }; MAX_BLOBS],
);
static REFERENCE: ConstStaticCell<[u32; HEIGHT]> = ConstStaticCell::new([0; HEIGHT]);

/// Bytes of internal RAM this side takes: the ring, its list items, the detector, the blob
/// scratch and the reference (core 1's stack and result queue are in `espion-core1`).
pub const RAM_BYTES: usize = core::mem::size_of::<RingMem>()
    + core::mem::size_of::<[StripLink; STRIPS]>()
    + core::mem::size_of::<DefaultDetector>()
    + core::mem::size_of::<[Blob; MAX_BLOBS]>()
    + core::mem::size_of::<[u32; HEIGHT]>();

/// What the camera side needs: the camera's peripherals, PSRAM for the reference's frame
/// buffers (three of `camera::BUFFER_LEN`), and core 1.
pub struct Resources {
    pub i2c0: I2C0<'static>,
    pub vdma: VDMA_CH0<'static>,
    pub frames: &'static mut [u8],
    pub cpu: CPU_CTRL<'static>,
}

/// A run asked for: the job (`None`: stop) and the L1 data autoload over each strip.
#[derive(Clone, Copy)]
pub struct Request {
    pub job: Option<Job>,
    pub autoload: bool,
}

static REQUEST: Signal<CriticalSectionRawMutex, Request> = Signal::new();
static ANSWER: Signal<CriticalSectionRawMutex, Result<(), &'static str>> = Signal::new();

/// Spawns the camera side's task.
pub fn start(spawner: Spawner, res: Resources) {
    spawner.spawn(vision_task(res).unwrap());
}

/// Stops the run in progress and starts `r` (the bench's TX task, between its runs). `Err`
/// with the reason if the run was refused or did not start (then nothing runs).
pub async fn request(r: Request) -> Result<(), &'static str> {
    ANSWER.reset();
    REQUEST.signal(r);
    with_timeout(Duration::from_secs(10), ANSWER.wait())
        .await
        .unwrap_or(Err("no answer from the camera side within 10 s"))
}

#[allow(
    clippy::large_stack_frames,
    reason = "a spawned task: its future lives in the task's static storage, and its poll frame \
    (about 2 KiB) is on the main stack (55 KiB in BenchVision)"
)]
#[embassy_executor::task]
async fn vision_task(res: Resources) {
    cpu::name_current("vision");
    let Some((mut sensor, _)) = camera::sensor(res.i2c0).await else {
        refuse_all("no camera").await
    };
    info!("vision: ready; the first V request takes the reference and starts core 1");
    let first = loop {
        let r = REQUEST.wait().await;
        if r.job.is_some() {
            break r;
        }
        ANSWER.signal(Ok(()));
    };
    let mut vdma = res.vdma;
    let reference = take_reference(&mut sensor, vdma.reborrow(), res.frames).await;
    log_rom_cache_hook();
    let Some((core1, control, ring)) = start_core1(res.cpu, vdma.reborrow()) else {
        refuse_all("core 1 or the strip receiver did not start (see the log)").await
    };
    let mut v = Vision {
        sensor,
        core1,
        control,
        ring,
        reference,
        run: None,
        dead: false,
    };
    let a = v.apply(first).await;
    ANSWER.signal(a);
    v.serve().await
}

/// Answers every request with `why`, for ever.
async fn refuse_all(why: &'static str) -> ! {
    loop {
        let _ = REQUEST.wait().await;
        ANSWER.signal(Err(why));
    }
}

/// The run in progress.
struct Running {
    job: Job,
    mon: Monitor,
    next: Instant,
    check0: CheckSnap,
}

/// The camera side once core 1 runs.
struct Vision<'d> {
    sensor: Sensor,
    core1: &'static mut Core1,
    control: StripControl<'d>,
    /// The strip buffers' and the list items' addresses (core 1's test-hold diagnostics).
    ring: (usize, usize),
    /// The pixel check's reference row checksums, if the reference passed.
    reference: Option<&'static [u32]>,
    run: Option<Running>,
    /// Core 1 trapped or stopped: nothing is started any more.
    dead: bool,
}

impl Vision<'_> {
    /// Every millisecond: a request, or the monitor.
    async fn serve(&mut self) -> ! {
        loop {
            Timer::after_millis(1).await;
            if let Some(r) = REQUEST.try_take() {
                let a = self.apply(r).await;
                ANSWER.signal(a);
                continue;
            }
            let Self {
                run,
                control,
                core1,
                ..
            } = self;
            let Some(r) = run.as_mut() else { continue };
            r.mon.poll(control, core1);
            if Instant::now() < r.next {
                continue;
            }
            r.next += Duration::from_secs(1);
            if !r.mon.second(control, core1).await {
                self.dead = true;
                self.stop().await;
            }
        }
    }

    /// Stops the run in progress, then starts `r`.
    async fn apply(&mut self, r: Request) -> Result<(), &'static str> {
        if self.dead {
            return Err("core 1 stopped (see the log)");
        }
        self.stop().await;
        let Some(job) = r.job else {
            info!("vision: stopped");
            return Ok(());
        };
        let reference = match (job, self.reference) {
            (Job::Check, Some(rows)) => rows,
            (Job::Check, None) => return Err("no reference of the test pattern (see the log)"),
            (Job::Detect, _) => &[],
        };
        if r.autoload {
            prefetch::lend_l1_autoload(true).map_err(prefetch::Refused::text)?;
        }
        let started = self.start(job, r.autoload, reference).await;
        if started.is_err() {
            let _ = prefetch::lend_l1_autoload(false);
        }
        started
    }

    /// Parameters, sensor, receiver, stream on, the monitor. The strip path is stopped.
    #[allow(
        clippy::large_stack_frames,
        reason = "between runs, on the main stack (55 KiB in BenchVision): the run parameters, \
        the sensor's set-up future and the monitor are built here"
    )]
    async fn start(
        &mut self,
        job: Job,
        autoload: bool,
        reference: &'static [u32],
    ) -> Result<(), &'static str> {
        let mhz = esp_hal::clock::cpu_clock().as_mhz();
        let strip_ns = u64::from(MODE.frame_ns()) * u64::from(LINES) / HEIGHT as u64;
        self.core1.set_run(&Run {
            job,
            width: MODE.width(),
            raw10: false,
            threshold: THRESHOLD,
            late_cycles: (strip_ns * u64::from(mhz) / 1000) as u32,
            autoload,
            synth: None,
            reference,
            line_bytes: MODE.bytes_per_line(),
            hold: None,
            ring: (self.ring.0, self.ring.1, STRIP_BYTES as u32, STRIPS as u32),
        });
        let controls = Controls {
            pattern: if job == Job::Check {
                TestPattern::Random
            } else {
                TestPattern::Off
            },
            ..camera::BENCH_CONTROLS
        };
        if let Err(e) = camera::setup(&mut self.sensor, &MODE, &controls).await {
            error!("vision: sensor set-up: {e:?}");
            return Err("sensor set-up failed (see the log)");
        }
        let c = &mut self.control;
        if let Err(e) = c.set_config(CONFIG, LINES).and_then(|()| {
            c.reset_stats();
            c.start()
        }) {
            error!("vision: receiver start: {e:?}");
            return Err("receiver start failed (see the log)");
        }
        Timer::after_millis(1).await;
        if let Err(e) = self.sensor.start().await {
            error!("vision: stream on: {e:?}");
            self.control.stop();
            self.core1.note_stop();
            return Err("stream on failed (see the log)");
        }
        log_start(job, autoload, strip_ns / 1000);
        self.run = Some(Running {
            job,
            mon: Monitor::new(self.core1, &self.control, job),
            next: Instant::now() + Duration::from_secs(1),
            check0: CHECK.snap(),
        });
        Ok(())
    }

    /// Stream off, the receiver stopped once the frame is out, core 1 past the strip it had,
    /// the L1 data autoload back to this side, the run's totals logged.
    #[allow(
        clippy::large_stack_frames,
        reason = "between runs, on the main stack (55 KiB in BenchVision): the run's monitor is \
        moved out and its totals logged here"
    )]
    async fn stop(&mut self) {
        let Some(r) = self.run.take() else { return };
        if let Err(e) = self.sensor.stop().await {
            warn!("vision: stream off: {e:?}");
        }
        Timer::after_millis(20).await;
        self.control.stop();
        self.core1.note_stop();
        core1_turned().await;
        let _ = prefetch::lend_l1_autoload(false);
        r.mon.end(NAME);
        let check = CHECK.snap().since(&r.check0);
        log_summary(NAME, &self.control, (r.job == Job::Check).then_some(&check));
    }
}

/// Waits (up to 10 ms) until core 1's loop turned twice: it then holds no strip, so its
/// autoload is off.
async fn core1_turned() {
    let b = core1::heartbeat();
    for _ in 0..10 {
        Timer::after_millis(1).await;
        if core1::heartbeat().wrapping_sub(b) >= 2 {
            return;
        }
    }
}

/// Starts core 1 and the strip receiver, and hands the reader over. `None` (logged) if
/// something failed.
#[allow(
    clippy::large_stack_frames,
    reason = "once, on the main stack (55 KiB in BenchVision): the strip receiver is built here \
    by value and split"
)]
#[inline(never)]
fn start_core1<'d>(
    cpu: CPU_CTRL<'static>,
    vdma: VDMA_CH0<'d>,
) -> Option<(&'static mut Core1, StripControl<'d>, (usize, usize))> {
    let Some(core1) = core1::start(cpu, DETECTOR.take(), BLOBS.take(), SkipChoice::Pie) else {
        error!("vision: core 1 did not start");
        return None;
    };
    info!("vision: core 1 started (bare: no scheduler, no interrupts)");
    let ring = RING.take();
    let links = LINKS.take();
    let addrs = (ring.0.as_ptr() as usize, links.as_ptr() as usize);
    let strips = ring.0.each_mut().map(|s| s.as_mut_slice());
    let cs = match CsiStrips::new(vdma, strips, links, CONFIG, LINES) {
        Ok(c) => c,
        Err(e) => {
            error!("vision: CsiStrips::new: {e:?}");
            return None;
        }
    };
    irq::install(&[
        irq::Source::CsiDma,
        irq::Source::CsiHost,
        irq::Source::CsiBridge,
    ]);
    if !core1.can_post() {
        error!("vision: core 1's mailbox is not free");
        return None;
    }
    let control = core1.hand_over(cs)?;
    info!(
        "vision: strip receiver with {STRIPS} strips of {STRIP_BYTES} B ({LINES} rows), reader on core 1"
    );
    Some((core1, control, addrs))
}

/// The pixel check's reference (module docs, step 1). `None` (logged) if it failed.
#[allow(
    clippy::large_stack_frames,
    reason = "once, on the main stack (55 KiB in BenchVision): the whole-frame receiver lives here \
    for the reference"
)]
async fn take_reference(
    sensor: &mut Sensor,
    vdma: VDMA_CH0<'_>,
    frames: &'static mut [u8],
) -> Option<&'static [u32]> {
    let (b0, rest) = frames.split_at_mut(BUFFER_LEN);
    let (b1, rest) = rest.split_at_mut(BUFFER_LEN);
    let (b2, _) = rest.split_at_mut(BUFFER_LEN);
    let mut csi = match Csi::new(vdma, [b0, b1, b2], CONFIG) {
        Ok(c) => c,
        Err(e) => {
            error!("vision: reference: Csi::new: {e:?}");
            return None;
        }
    };
    let controls = Controls {
        pattern: TestPattern::Random,
        ..camera::BENCH_CONTROLS
    };
    if let Err(e) = camera::setup(sensor, &MODE, &controls).await {
        error!("vision: reference: sensor set-up: {e:?}");
        return None;
    }
    if let Err(e) = csi.start() {
        error!("vision: reference: receiver start: {e:?}");
        return None;
    }
    Timer::after_millis(1).await;
    let rows = REFERENCE.take();
    let tally = match sensor.start().await {
        Ok(()) => reference_frames(&mut csi, rows).await,
        Err(e) => {
            error!("vision: reference: stream on: {e:?}");
            RefTally::default()
        }
    };
    if let Err(e) = sensor.stop().await {
        warn!("vision: reference: stream off: {e:?}");
    }
    Timer::after_millis(20).await;
    csi.stop();
    let pass = tally.frames == REFERENCE_FRAMES && tally.identical == REFERENCE_FRAMES;
    info!(
        "vision: reference of the random test pattern: frames {} (wrong size {}), identical to the first {}, most rows differing {}: {}",
        tally.frames,
        tally.wrong_size,
        tally.identical,
        tally.differing_max,
        if pass { "PASS" } else { "FAIL" }
    );
    pass.then_some(&rows[..])
}

#[derive(Default)]
struct RefTally {
    frames: u32,
    wrong_size: u32,
    identical: u32,
    differing_max: u32,
}

/// Two frames skipped, then [`REFERENCE_FRAMES`] whole frames: the first one's row checksums
/// into `rows`, the others compared with them.
async fn reference_frames(csi: &mut Csi<'_>, rows: &mut [u32; HEIGHT]) -> RefTally {
    let mut t = RefTally::default();
    let line = MODE.bytes_per_line() as usize;
    let mut frame = None;
    for k in 0..2 + REFERENCE_FRAMES {
        let Ok(Ok(f)) =
            with_timeout(camera::frame_timeout(&MODE), csi.wait_frame(frame.take())).await
        else {
            error!("vision: reference: no frame");
            break;
        };
        if k >= 2 {
            if f.received_len() != HEIGHT * line {
                t.wrong_size += 1;
            } else if t.frames == 0 {
                for (w, row) in rows.iter_mut().zip(f.chunks_exact(line)) {
                    *w = row_sum(row);
                }
                t.frames = 1;
                t.identical = 1;
            } else {
                let differing = rows
                    .iter()
                    .zip(f.chunks_exact(line))
                    .filter(|&(&w, row)| row_sum(row) != w)
                    .count() as u32;
                t.differing_max = t.differing_max.max(differing);
                t.frames += 1;
                t.identical += u32::from(differing == 0);
            }
        }
        frame = Some(f);
    }
    t
}

/// The ROM's cache-operation hooks: `Cache_Invalidate_Addr` and `Cache_WriteBack_Addr` go
/// through `Cache_Op_Addr`, which calls the callbacks `rom_cache_op_cb` points to (if any)
/// around `Cache_Sync_Items`, which writes the cache-sync registers only (disassembly of
/// `esp32p4_rev300_rom.elf`). A null hook means the ROM's maintenance never touches the
/// prefetch registers that core 1 owns.
#[inline(never)]
fn log_rom_cache_hook() {
    unsafe extern "C" {
        static rom_cache_op_cb: usize;
        static rom_cache_internal_table_ptr: usize;
    }
    // SAFETY: two words of the ROM's data in internal RAM (addresses from the ROM linker
    // script esp-hal links), read once.
    let (hook, table) = unsafe {
        (
            (&raw const rom_cache_op_cb).read_volatile(),
            (&raw const rom_cache_internal_table_ptr).read_volatile(),
        )
    };
    info!(
        "vision: ROM cache-operation hook (rom_cache_op_cb) {hook:#010x}, internal table {table:#010x}"
    );
}

#[inline(never)]
fn log_start(job: Job, autoload: bool, strip_us: u64) {
    info!(
        "vision: {NAME} 120 fps: {} on core 1, L1 data autoload {}, {LINES}-row strips (one every {strip_us} us), skip {:?}",
        match job {
            Job::Detect => "detector live",
            Job::Check => "pixel check (random test pattern)",
        },
        if autoload { "on" } else { "off" },
        core1::skip_used()
    );
}
