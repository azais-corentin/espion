//! The capture task: owns the sensor and the receiver, takes every frame as it completes,
//! counts it, and hands it to the send task or drops it; runs the mode changes and restarts the
//! camera when frames stop.

use core::sync::atomic::Ordering;

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp32p4_csi::{Frame, Stats};
use espion_proto::cam::{CamError, Encoding};
use log::{error, info, warn};
use ov9281::Mode;

use super::{CMDS, CameraResources, Cmd, DONE, FAULT, JOB, Job, Shared, shared};
use crate::camera::{self, Camera, Controls, Fault, StartError};
use crate::cpu;

/// How long to wait before the next start when one failed.
const RETRY: Duration = Duration::from_secs(1);

/// Sets up the camera, then captures for good.
#[embassy_executor::task]
pub(super) async fn task(res: CameraResources) {
    cpu::name_current("capture");
    let (cam_mem, _) = res.psram.split_at_mut(camera::BUFFERS * camera::BUFFER_LEN);
    let Some(cam) = camera::init(res.i2c0, res.vdma, cam_mem).await else {
        error!("stream: no camera, no frames");
        return;
    };
    let last = cam.stats();
    let mut c = Capture {
        cam,
        mode: res.mode,
        controls: res.controls,
        last,
    };
    c.run().await;
}

struct Capture {
    cam: &'static mut Camera,
    /// The mode and controls wanted (running unless a start failed).
    mode: Mode,
    controls: Controls,
    /// The receiver's counters when last looked at.
    last: Stats,
}

/// The wire error of a failed start.
fn cam_error(e: StartError) -> CamError {
    match e {
        StartError::Sensor(_) => CamError::SensorBus,
        StartError::Receiver(_) | StartError::LanesNotStopped | StartError::NoFirstFrame => {
            CamError::Receiver
        }
    }
}

fn take_fault() -> Option<Fault> {
    match FAULT.swap(0, Ordering::Relaxed) {
        1 => Some(Fault::SensorBus),
        2 => Some(Fault::NoStream),
        _ => None,
    }
}

impl Capture {
    async fn run(&mut self) -> ! {
        loop {
            if self.cam.running().is_none() && self.start(None).await.is_err() {
                if let Either::First(cmd) = select(CMDS.receive(), Timer::after(RETRY)).await {
                    self.command(cmd).await;
                }
                continue;
            }
            let timeout = camera::frame_timeout(&self.mode);
            match select(CMDS.receive(), with_timeout(timeout, self.cam.wait_frame())).await {
                Either::First(cmd) => self.command(cmd).await,
                Either::Second(Ok(Ok(frame))) => self.frame(frame),
                Either::Second(_) => {
                    // No frame within two periods (or the receiver stopped): count it and
                    // restart the camera on the next pass.
                    shared(|s| {
                        s.counters.capture_errors = s.counters.capture_errors.wrapping_add(1);
                    });
                    let _ = self.cam.stop().await;
                    log_restart(&self.mode);
                }
            }
        }
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

    /// Starts the wanted mode, aborting the frame in flight first. The frames and errors of
    /// the start count as captured and as errors, not as skipped.
    async fn start(&mut self, fault: Option<Fault>) -> Result<(), StartError> {
        shared(Shared::abort);
        let r = self.cam.start(&self.mode, &self.controls, fault).await;
        let (frames, errors) = self.account();
        shared(|s| {
            s.counters.frames_captured = s.counters.frames_captured.wrapping_add(frames);
            s.counters.capture_errors = s.counters.capture_errors.wrapping_add(errors);
        });
        if let Err(e) = r {
            log_start_failed(&e);
        }
        r
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
                DONE.signal(r.map_err(|_| CamError::SensorBus));
            }
            Cmd::Stall => {
                let _ = self.cam.stall().await;
            }
        }
    }

    /// Counts `f` and hands it to the send task, or drops it: short frame, no host, a frame in
    /// flight (skipped), or held back by `max_fps`.
    fn frame(&mut self, f: Frame) {
        let now = Instant::now();
        let (frames, errors) = self.account();
        let short = f.received_len() != f.len();
        let period_us = u64::from(self.mode.frame_ns()) / 1000;
        let job = shared(|s| {
            let active = s.active(now);
            let c = &mut s.counters;
            c.frames_captured = c.frames_captured.wrapping_add(frames);
            c.capture_errors = c.capture_errors.wrapping_add(errors + u32::from(short));
            let number = c.frames_captured.wrapping_sub(1);
            if short || !active {
                return None;
            }
            if s.sending {
                c.frames_skipped = c.frames_skipped.wrapping_add(frames);
                return None;
            }
            // Frames overwritten before this task took them were not sent either.
            c.frames_skipped = c.frames_skipped.wrapping_add(frames.saturating_sub(1));
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
            s.sending = true;
            Some((number, s.generation, s.epoch, s.stream.encoding))
        });
        let Some((number, generation, epoch, encoding)) = job else {
            return;
        };
        let (width, height) = (self.mode.width(), self.mode.height());
        JOB.signal(Job {
            frame: f,
            high_bits: self.mode.format() == ov9281::Format::Raw10 && encoding == Encoding::Raw8,
            number,
            generation,
            epoch,
            encoding,
            len: encoding.frame_bytes(width, height) as u32,
            width,
            height,
            timestamp_us: now.as_micros(),
        });
    }
}

/// Logged with the camera stopped (a log line masks interrupts; never while frames flow).
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
fn log_start_failed(e: &StartError) {
    match e {
        StartError::Sensor(b) => info!("stream: camera start failed: sensor {b:?}"),
        StartError::Receiver(r) => info!("stream: camera start failed: receiver {r:?}"),
        StartError::LanesNotStopped => {
            info!("stream: camera start failed: lanes not in stop state")
        }
        StartError::NoFirstFrame => info!("stream: camera start failed: no first frame"),
    }
}
