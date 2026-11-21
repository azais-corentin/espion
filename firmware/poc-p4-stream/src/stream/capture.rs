//! The capture task: owns the sensor, the receiver and core 1's results.
//!
//! - **Strip path** (`camera::strip_plan`'s modes, core 1 healthy): every millisecond it
//!   services the strip receiver, counts its frame ends and errors, gives core 1 the detection
//!   settings when they change, and takes core 1's results: each an assembled frame in a
//!   buffer of the pool (or none: core 1 found no free buffer) with its detection, which goes to
//!   [`detect::Work`]. Frames stop (no frame end within `camera::frame_timeout`): a capture
//!   error and a restart.
//! - **Whole frames** (the other modes): it waits for each frame from the receiver.
//!
//! Either way each frame is counted and handed on or dropped by the stream rules
//! ([`Capture::deliver`]). The task also runs the mode changes.

use core::sync::atomic::Ordering;

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp32p4_csi::{Frame, Stats};
use espion_core1::{NO_BUFFER, StreamDetect, give_frame};
use espion_proto::cam::{self, CamError, Encoding, Exposure};
use log::{error, info, warn};
use ov9281::{Gain, Mode, limits};

use super::detect::{self, Meta};
use super::frames::{self, FrameBuf, Pool};
use super::{CMDS, CameraResources, Cmd, DONE, FAULT, JOB, Job, Payload, jpeg, shared};
use crate::camera::{self, Camera, Controls, Fault, StartError};
use crate::cpu;

/// How long to wait before the next start when one failed.
const RETRY: Duration = Duration::from_secs(1);
/// The strip path's service period.
const TICK: Duration = Duration::from_millis(1);
/// How long a switch from whole frames to strips waits for the whole frames still held (by the
/// send or encode task, both dropping theirs at the abort) to come back; after that the mode
/// runs on whole frames.
const WHOLE_BACK: Duration = Duration::from_millis(100);
/// Ticks between two looks at core 1's stack.
const STACK_TICKS: u32 = 1000;

/// Sets up the camera, then captures for good.
#[embassy_executor::task]
pub(super) async fn task(res: CameraResources) {
    cpu::name_current("capture");
    let Some(cam) = camera::init(res.i2c0, res.vdma, res.psram).await else {
        error!("stream: no camera, no frames");
        return;
    };
    if let Some(core1) = res.core1 {
        cam.enable_strips(core1, res.pool.buffers());
    }
    let mut c = Capture {
        cam,
        mode: res.mode,
        controls: res.controls,
        last: Stats::default(),
        pool: res.pool,
        work: res.work,
        strips: None,
        pushed: None,
        ticks: 0,
        mhz: esp_hal::clock::cpu_clock().as_mhz().max(1),
    };
    c.run().await;
}

/// The strip run going on.
struct StripRun {
    /// Its number (core 1's results carry it).
    id: u32,
    /// Frames captured before it: a frame's number is this plus the frame ends between the
    /// start and the frame ([`espion_core1::FrameResult::frame`]).
    base: u32,
    /// Frame (ends since the start) of the last result.
    last_frame: Option<u32>,
    /// The last tick that counted a frame end.
    progress: Instant,
    /// Bytes per frame.
    frame_bytes: usize,
    /// Rows of a full strip, and the hold a strip is allowed on core 1 (µs).
    strip_lines: u16,
    hold_budget_us: u16,
}

struct Capture {
    cam: &'static mut Camera,
    /// The mode and controls wanted (running unless a start failed).
    mode: Mode,
    controls: Controls,
    /// The receiver's counters when last looked at.
    last: Stats,
    pool: Pool,
    work: detect::Work,
    strips: Option<StripRun>,
    /// The detection settings core 1 has.
    pushed: Option<StreamDetect>,
    ticks: u32,
    mhz: u32,
}

/// The wire error of a failed start.
fn cam_error(e: StartError) -> CamError {
    match e {
        StartError::Sensor(_) => CamError::SensorBus,
        StartError::Receiver(_)
        | StartError::Strips(_)
        | StartError::LanesNotStopped
        | StartError::NoFirstFrame => CamError::Receiver,
    }
}

fn take_fault() -> Option<Fault> {
    match FAULT.swap(0, Ordering::Relaxed) {
        1 => Some(Fault::SensorBus),
        2 => Some(Fault::NoStream),
        _ => None,
    }
}

/// One frame to hand on.
struct Delivery {
    number: u32,
    generation: u32,
    timestamp_us: u64,
    /// Frames lost before it (not handed on either).
    lost: u32,
}

impl Capture {
    #[allow(
        clippy::large_stack_frames,
        reason = "the capture task's loop: its future lives in the task's static storage, and its \
        poll frame (about 1.3 KiB: a camera start's) is on the main stack"
    )]
    async fn run(&mut self) -> ! {
        loop {
            if self.cam.running().is_none() && self.start(None).await.is_err() {
                if let Either::First(cmd) = select(CMDS.receive(), Timer::after(RETRY)).await {
                    self.command(cmd).await;
                }
                continue;
            }
            if self.strips.is_some() {
                match select(CMDS.receive(), Timer::after(TICK)).await {
                    Either::First(cmd) => self.command(cmd).await,
                    Either::Second(()) => {
                        if !self.tick() {
                            self.lost_frames().await;
                        }
                    }
                }
                continue;
            }
            let timeout = camera::frame_timeout(&self.mode);
            match select(CMDS.receive(), with_timeout(timeout, self.cam.wait_frame())).await {
                Either::First(cmd) => self.command(cmd).await,
                Either::Second(Ok(Ok(frame))) => self.frame(frame),
                Either::Second(_) => self.lost_frames().await,
            }
        }
    }

    /// No frame within two periods (or the receiver stopped): count it and restart the camera
    /// on the next pass.
    async fn lost_frames(&mut self) {
        shared(|s| {
            s.counters.capture_errors = s.counters.capture_errors.wrapping_add(1);
        });
        let _ = self.cam.stop().await;
        self.strips = None;
        log_restart(&self.mode);
    }

    /// Frames and error events the receiver counted since the last look.
    fn account(&mut self) -> (u32, u32) {
        let st = self.cam.stats();
        let d = |a: u32, b: u32| a.wrapping_sub(b);
        let frames = d(st.frames_done, self.last.frames_done);
        let errors = d(st.dma_errors, self.last.dma_errors)
            .wrapping_add(d(st.host_errors, self.last.host_errors))
            .wrapping_add(d(st.bridge_errors, self.last.bridge_errors))
            .wrapping_add(d(st.frames_dropped, self.last.frames_dropped));
        self.last = st;
        critical_section::with(|cs| super::diag::CSI_STATS.borrow(cs).set(st));
        (frames, errors)
    }

    /// [`Capture::account`] into the stream's counters; the frames counted.
    fn count(&mut self) -> u32 {
        let (frames, errors) = self.account();
        shared(|s| {
            let c = &mut s.counters;
            c.frames_captured = c.frames_captured.wrapping_add(frames);
            c.capture_errors = c.capture_errors.wrapping_add(errors);
        });
        frames
    }

    /// Gives core 1 the detection settings if they changed.
    fn push_detect(&mut self) {
        let d = detect::core1_settings();
        if self.pushed != Some(d)
            && let Some(core1) = self.cam.core1()
        {
            core1.set_detect(&d);
            self.pushed = Some(d);
        }
    }

    /// Gives back the buffers of every result waiting (core 1 is idle: between runs).
    fn drop_results(&mut self) {
        let Some(core1) = self.cam.core1() else {
            return;
        };
        while let Some(buffer) = core1.pop_with(|r| r.buffer) {
            if buffer != NO_BUFFER {
                give_frame(buffer);
            }
        }
    }

    /// Starts the wanted mode, aborting the frame in flight first; core 1 drops what it holds
    /// before the receiver stops, and its results of the frames before are not used. The
    /// frames and errors of the start count as captured and as errors, not as skipped.
    #[allow(
        clippy::large_stack_frames,
        reason = "between runs, on the main stack: the camera start's future"
    )]
    async fn start(&mut self, fault: Option<Fault>) -> Result<(), StartError> {
        shared(|s| {
            s.abort();
            s.detect_serial = s.detect_serial.wrapping_add(1) & !(1 << 31);
        });
        if self.cam.running().is_some() {
            self.count();
            let _ = self.cam.stop().await;
        }
        self.strips = None;
        self.drop_results();
        let faulty = shared(|s| s.detect.fault);
        let autoload = !faulty && detect::autoload_for_run();
        self.cam.set_strips_allowed(!faulty, autoload);
        if self.cam.plan_for(&self.mode).is_some() && self.cam.has_frame_receiver() {
            // The strip receiver may not run while a whole frame is read: the whole-frame
            // receiver made again later would write into it.
            let end = Instant::now() + WHOLE_BACK;
            while frames::whole_out() > 0 && Instant::now() < end {
                Timer::after_millis(1).await;
            }
            if frames::whole_out() > 0 {
                self.cam.set_strips_allowed(false, autoload);
                warn!("stream: whole frames still held, this mode runs on whole frames");
            }
        }
        let base = shared(|s| s.counters.frames_captured);
        self.last = Stats::default();
        let r = self.cam.start(&self.mode, &self.controls, fault).await;
        self.count();
        let format = if self.mode.format() == ov9281::Format::Raw10 {
            cam::Format::Raw10
        } else {
            cam::Format::Raw8
        };
        let cap = cam::auto_exposure_cap(self.mode.line_ns(), self.mode.max_exposure_lines());
        let strips = if r.is_ok() {
            self.cam.strips_running()
        } else {
            None
        };
        shared(|s| {
            if r.is_ok() {
                // Settings and threshold follow the format of the mode now running.
                if s.detect_format != format {
                    let d = &mut s.detect;
                    d.settings = d.settings.for_format(s.detect_format, format);
                    d.threshold = d.settings.threshold;
                    s.detect_format = format;
                }
                s.ae_cap = cap;
            }
            s.detect.available = strips.is_some();
        });
        let strip_us = |lines: u16| u32::from(lines) * self.mode.line_ns() / 1000;
        critical_section::with(|cs| {
            detect::PLAN
                .borrow(cs)
                .set(strips.map_or((0, 0, 0), |(p, _)| (p.lines, p.count, strip_us(p.lines))));
        });
        if let Some((plan, id)) = strips {
            let budget = camera::hold_budget_cycles(&self.mode, plan, self.mhz) / self.mhz;
            self.strips = Some(StripRun {
                id,
                base,
                last_frame: None,
                progress: Instant::now(),
                frame_bytes: camera::csi_config(&self.mode).frame_bytes(),
                strip_lines: plan.lines,
                hold_budget_us: u16::try_from(budget).unwrap_or(u16::MAX),
            });
            self.pushed = None;
            self.push_detect();
        }
        if r.is_ok() {
            self.publish_exposure();
        }
        if let Err(e) = r {
            log_start_failed(&e);
        } else {
            log_started(&self.mode, strips.map(|(p, _)| p));
        }
        r
    }

    /// The exposure (clamped to the mode's range, as the sensor got it) and gain in use, for
    /// the state and the automatic exposure.
    fn publish_exposure(&self) {
        let e = Exposure {
            lines: self
                .controls
                .exposure_lines
                .clamp(limits::EXPOSURE_MIN, self.mode.max_exposure_lines()),
            gain_sixteenths: self.controls.gain.as_sixteenths(),
        };
        shared(|s| s.exposure = e);
    }

    async fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Apply {
                mode,
                controls,
                layout_changed,
            } => {
                let old = (self.mode, self.controls);
                (self.mode, self.controls) = (mode, controls);
                let r = self.start(take_fault()).await;
                match r {
                    Ok(()) if layout_changed => shared(|s| {
                        s.generation = s.generation.wrapping_add(1);
                    }),
                    Ok(()) => {}
                    Err(_) => {
                        // Back to the mode that ran; if that fails too, the loop retries.
                        (self.mode, self.controls) = old;
                        let _ = self.start(None).await;
                    }
                }
                DONE.signal(r.map_err(cam_error));
            }
            Cmd::Controls(c) => {
                let r = self.cam.set_controls(&self.mode, &c).await;
                if r.is_ok() {
                    self.controls = c;
                } else {
                    let _ = self.cam.set_controls(&self.mode, &self.controls).await;
                }
                self.publish_exposure();
                DONE.signal(r.map_err(|_| CamError::SensorBus));
            }
            Cmd::Stall => {
                let _ = self.cam.stall().await;
            }
            Cmd::Exposure(e) if shared(|s| s.auto_exposure) => {
                let c = Controls {
                    exposure_lines: e.lines,
                    gain: Gain::from_sixteenths(e.gain_sixteenths),
                    ..self.controls
                };
                if self.cam.running().is_some()
                    && self.cam.set_controls(&self.mode, &c).await.is_ok()
                {
                    self.controls = c;
                    self.publish_exposure();
                }
            }
            // A step queued before the automatic exposure was turned off (or a request
            // began): dropped.
            Cmd::Exposure(_) => {}
        }
    }

    /// The strip path's millisecond: services the receiver, counts, pushes the detection
    /// settings, takes core 1's results. `false` once frames stopped.
    fn tick(&mut self) -> bool {
        let now = Instant::now();
        if let Some(c) = self.cam.strip_control() {
            c.service();
            detect::LAPPED.store(c.strips_lapped(), Ordering::Relaxed);
            detect::LAPPED_COPIED.store(c.strips_lapped_copied(), Ordering::Relaxed);
        }
        let frames = self.count();
        self.push_detect();
        let timeout = camera::frame_timeout(&self.mode);
        let Some(run) = &mut self.strips else {
            return true;
        };
        if frames > 0 {
            run.progress = now;
        } else if now - run.progress > timeout {
            return false;
        }
        while let Some(taken) = self.take_result(now) {
            if let Some((f, d)) = taken {
                self.deliver(f, &d, now);
            }
        }
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(STACK_TICKS)
            && let Some(core1) = self.cam.core1()
        {
            detect::DROPPED.store(core1.dropped(), Ordering::Relaxed);
            detect::CORE1_STACK_UNUSED.store(core1.stack_unused() as u32, Ordering::Relaxed);
        }
        true
    }

    /// Core 1's oldest result: `None` if there is none; else its frame to hand on, if it has
    /// one (its detection went to [`detect::Work`]).
    fn take_result(&mut self, now: Instant) -> Option<Option<(FrameBuf, Delivery)>> {
        let core1 = self.cam.core1()?;
        let (work, pool, mhz) = (&mut self.work, self.pool, self.mhz);
        let run = self.strips.as_mut()?;
        let (width, height) = (self.mode.width(), self.mode.height());
        core1.pop_with(|r| {
            if r.run != run.id {
                // A result of a run before (its frame no longer counted).
                if r.buffer != NO_BUFFER {
                    give_frame(r.buffer);
                }
                return None;
            }
            work.strips(r);
            let lost = run
                .last_frame
                .map_or(0, |l| r.frame.wrapping_sub(l).saturating_sub(1));
            run.last_frame = Some(r.frame);
            let age_us = u64::from(espion_core1::age(r.queued_at) / mhz);
            let d = Delivery {
                number: run.base.wrapping_add(r.frame),
                generation: shared(|s| s.generation),
                timestamp_us: now.as_micros().saturating_sub(age_us),
                lost,
            };
            work.result(
                r,
                &Meta {
                    number: d.number,
                    generation: d.generation,
                    timestamp_us: d.timestamp_us,
                    lost,
                    width,
                    height,
                    strip_lines: run.strip_lines,
                    hold_budget_us: run.hold_budget_us,
                },
                now,
            );
            if r.buffer == NO_BUFFER {
                // Detected, not copied: lost for the image path too.
                skip_lost(1);
                return None;
            }
            pool.frame(r.buffer, run.frame_bytes).map(|f| (f, d))
        })
    }

    /// A whole frame: counts it and hands it on (a short one is a capture error, dropped).
    fn frame(&mut self, f: Frame) {
        let now = Instant::now();
        let frames = self.count();
        let short = f.received_len() != f.len();
        let (number, generation) = shared(|s| {
            let c = &mut s.counters;
            c.capture_errors = c.capture_errors.wrapping_add(u32::from(short));
            (c.frames_captured.wrapping_sub(1), s.generation)
        });
        if short {
            return;
        }
        let d = Delivery {
            number,
            generation,
            timestamp_us: now.as_micros(),
            // Frames overwritten before this task took them.
            lost: frames.saturating_sub(1),
        };
        self.deliver(FrameBuf::whole(f), &d, now);
    }

    /// Hands `f` on, or drops it: no host, held back by `max_fps`, or (raw) a frame in flight
    /// (skipped). JPEG frames go to the encode task, which keeps the newest one waiting
    /// ([`jpeg::offer`]); raw frames to the send task. The frames lost before it count as
    /// skipped.
    fn deliver(&mut self, f: FrameBuf, d: &Delivery, now: Instant) {
        let period_us = u64::from(self.mode.frame_ns()) / 1000;
        let job = shared(|s| {
            if !s.active(now) {
                return None;
            }
            let jpeg = s.stream.encoding == Encoding::Jpeg;
            let c = &mut s.counters;
            if !jpeg && s.sending {
                c.frames_skipped = c.frames_skipped.wrapping_add(d.lost + 1);
                return None;
            }
            // Frames lost before this one were not sent either: the link's (raw) or the
            // encoder's (JPEG: they never reached it).
            if jpeg {
                s.encoder.frames_skipped = s.encoder.frames_skipped.wrapping_add(d.lost);
            } else {
                c.frames_skipped = c.frames_skipped.wrapping_add(d.lost);
            }
            if s.stream.max_fps > 0 {
                let at = now.as_micros();
                let slack = period_us / 2;
                let next = s.next_send.as_micros();
                if at + slack < next {
                    return None;
                }
                let interval = 1_000_000 / u64::from(s.stream.max_fps);
                let next = (next + interval).max((at + interval).saturating_sub(slack));
                s.next_send = Instant::from_micros(next);
            }
            if !jpeg {
                s.sending = true;
            }
            Some((s.epoch, s.stream.encoding))
        });
        let Some((epoch, encoding)) = job else {
            return;
        };
        let (width, height) = (self.mode.width(), self.mode.height());
        let Some(len) = encoding.frame_bytes(width, height) else {
            jpeg::offer(jpeg::Input {
                frame: f,
                number: d.number,
                generation: d.generation,
                epoch,
                width,
                height,
                timestamp_us: d.timestamp_us,
            });
            return;
        };
        JOB.signal(Job {
            data: Payload::Raw(f),
            high_bits: self.mode.format() == ov9281::Format::Raw10 && encoding == Encoding::Raw8,
            number: d.number,
            generation: d.generation,
            epoch,
            encoding,
            len: len as u32,
            width,
            height,
            timestamp_us: d.timestamp_us,
            encode: None,
        });
    }
}

/// A frame core 1 could not copy, for the image path: skipped by the link (raw) or the
/// encoder (JPEG), while frames go out.
fn skip_lost(n: u32) {
    let now = Instant::now();
    shared(|s| {
        if !s.active(now) {
            return;
        }
        if s.stream.encoding == Encoding::Jpeg {
            s.encoder.frames_skipped = s.encoder.frames_skipped.wrapping_add(n);
        } else {
            s.counters.frames_skipped = s.counters.frames_skipped.wrapping_add(n);
        }
    });
}

/// Logged with the camera stopped (a log line masks interrupts; never while whole frames
/// flow).
#[inline(never)]
fn log_restart(mode: &Mode) {
    warn!(
        "stream: no frame within {} ms, restarting {}x{}",
        camera::frame_timeout(mode).as_millis(),
        mode.width(),
        mode.height()
    );
}

#[inline(never)]
fn log_started(mode: &Mode, plan: Option<camera::StripPlan>) {
    match plan {
        Some(p) => info!(
            "stream: {}x{} {:?} on strips of {} rows x {} ({} B each)",
            mode.width(),
            mode.height(),
            mode.format(),
            p.lines,
            p.count,
            p.bytes
        ),
        None => info!(
            "stream: {}x{} {:?} on whole frames, no detection",
            mode.width(),
            mode.height(),
            mode.format()
        ),
    }
}

#[inline(never)]
fn log_start_failed(e: &StartError) {
    match e {
        StartError::Sensor(b) => info!("stream: camera start failed: sensor {b:?}"),
        StartError::Receiver(r) => info!("stream: camera start failed: receiver {r:?}"),
        StartError::Strips(r) => info!("stream: camera start failed: strips {r:?}"),
        StartError::LanesNotStopped => {
            info!("stream: camera start failed: lanes not in stop state")
        }
        StartError::NoFirstFrame => info!("stream: camera start failed: no first frame"),
    }
}
