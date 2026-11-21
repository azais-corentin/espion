//! The camera stream service: the protocol of `espion_proto::cam` on UDP [`CAM_PORT`], as the
//! viewer's fake camera (`host/poc-viewer/src/fake.rs`) behaves, with the real OV9281.
//!
//! The network, the stream's tasks and every interrupt run on core 0's executor (the README
//! has the measurements behind that, and the two-core attempt that hung); core 1, started
//! bare at boot, reads the camera's strips: it detects blobs on them and copies them into
//! frame buffers in PSRAM ([`detect`]). Tasks:
//!
//! - **control** ([`control_task`]): receives the requests, checks them ([`check`]), applies
//!   them (a mode or controls through the capture task), and queues the answer, the full
//!   [`State`] or the error, for the send task.
//! - **capture** ([`capture::task`]): owns the sensor, the receiver and core 1's results.
//!   Strip path (`camera::strip_plan`'s modes): services the strip receiver every millisecond,
//!   takes core 1's results (each an assembled frame with its detection), passes the detection
//!   on ([`detect::Work`]) and keeps the settings core 1 detects with up to date. Other modes:
//!   whole frames from the receiver, no detection. For each frame it counts, applies the
//!   stream rules (destination and timeout, one frame in flight for the raw encodings,
//!   `max_fps`) and hands it to the send task, to the encode task ([`Encoding::Jpeg`]) or drops
//!   it ([`frames`]: one holder per frame). It runs the mode changes (core 1 drops what it holds
//!   in a bounded wait), and restarts the camera when frames stop.
//! - **encode** ([`jpeg::task`]): the JPEG stage: the hardware encoder, from the frame's
//!   buffer into one of three output buffers, the newest frame at each stage, the automatic
//!   quality ([`jpeg`] has the pipeline).
//! - **detect** ([`detect::task`]): the automatic exposure; core 1's heartbeat and trap
//!   record once a second, and the detector's log lines.
//! - **send** ([`send::task`]): the only task that sends on the socket: the answers
//!   first, then the `FrameBlobs` datagrams, then the frame in flight, chunk by chunk on the
//!   protocol's grid, to the address and port of the last valid request; each chunk is
//!   encoded straight into the socket's ring, the one CPU copy of the pixels or of the JPEG
//!   file (a RAW10 frame sent as RAW8 is reduced to its high bits in that same copy).
//! - **diag** ([`diag::task`], UDP [`DIAG_PORT`]): the test hooks: the CPU and counter
//!   report, a log marker, fault injection, and a sensor stall.
//!
//! The socket is shared by control (receive only) and send (send only) through a `RefCell`
//! borrowed only inside a poll, so each of smoltcp's two wakers has one owner.

mod capture;
mod check;
mod detect;
mod diag;
mod frames;
mod jpeg;
mod send;

use core::cell::RefCell;
use core::future::poll_fn;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpEndpoint, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant};
use esp_hal::peripherals::{CPU_CTRL, I2C0, VDMA_CH0, VDMA_CH1};
use espion_proto::cam::{
    CAM_PORT, CamError, CameraSettings, Counters, DetectSettings, DetectState, EncoderState,
    Encoding, Exposure, Format, FrameTiming, MAX_DATAGRAM, Message, Op, Readout, Reply, Request,
    Response, STREAM_TIMEOUT_MS, Span, State, StreamSettings, TestPattern,
};
use log::{error, info};
use ov9281::Mode;
use static_cell::{ConstStaticCell, StaticCell};

use crate::camera::{self, Controls};
use crate::cpu;

/// Port of the test hooks ([`diag`]).
pub const DIAG_PORT: u16 = 5010;

/// The camera settings at boot: the fake camera's (`poc_viewer::fake::default_camera`).
pub const DEFAULT_CAMERA: CameraSettings = CameraSettings {
    format: Format::Raw8,
    width: 1280,
    height: 800,
    readout: Readout::Crop,
    origin: None,
    rate_mhz: 120_000,
    tolerance_mhz: 500,
    exposure_lines: 200,
    gain_sixteenths: 32,
    test_pattern: TestPattern::Off,
    auto_exposure: true,
};

/// The stream settings at boot (the fake camera's): on, JPEG at quality 90 with the automatic
/// quality, no rate limit.
pub const DEFAULT_STREAM: StreamSettings = StreamSettings {
    enabled: true,
    encoding: Encoding::Jpeg,
    max_fps: 0,
    jpeg_quality: 90,
    auto_quality: true,
};

/// Datagrams the TX ring holds (the app copies into it, the stack drains it). At least two:
/// smoltcp asks for [`MAX_DATAGRAM`] contiguous bytes, which a ring of less than twice that
/// cannot always offer.
const TX_PACKETS: usize = 8;
/// Bytes of the RX ring: requests are about 40 bytes; a burst of bad datagrams overflows it
/// and smoltcp drops the rest.
const RX_BYTES: usize = 4096;
/// Metadata slots of a ring: one per 300 B, plus two. On the TX side they must outnumber the
/// datagrams the ring holds, with one to spare for the wrap padding (smoltcp 0.13.1, see the
/// README, "The Ethernet driver", item 7).
const fn meta_slots(payload: usize) -> usize {
    payload / 300 + 2
}
const TX_META: usize = meta_slots(TX_PACKETS * MAX_DATAGRAM);
const RX_META: usize = meta_slots(RX_BYTES);

static RX_META_BUF: ConstStaticCell<[PacketMetadata; RX_META]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; RX_META]);
static RX_BUF: ConstStaticCell<[u8; RX_BYTES]> = ConstStaticCell::new([0; RX_BYTES]);
static TX_META_BUF: ConstStaticCell<[PacketMetadata; TX_META]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; TX_META]);
static TX_BUF: ConstStaticCell<[u8; TX_PACKETS * MAX_DATAGRAM]> =
    ConstStaticCell::new([0; TX_PACKETS * MAX_DATAGRAM]);
static REQUEST_BUF: ConstStaticCell<[u8; MAX_DATAGRAM]> = ConstStaticCell::new([0; MAX_DATAGRAM]);
static SOCKET: StaticCell<RefCell<UdpSocket<'static>>> = StaticCell::new();

/// Bytes of internal RAM the service's sockets and buffers take, the detector's and the strip
/// ring included.
pub const RAM_BYTES: usize = RX_BYTES
    + TX_PACKETS * MAX_DATAGRAM
    + MAX_DATAGRAM
    + (RX_META + TX_META) * core::mem::size_of::<PacketMetadata>()
    + diag::RAM_BYTES
    + detect::RAM_BYTES
    + send::RAM_BYTES
    + camera::RING_RAM_BYTES;

/// What the service needs besides the stack: PSRAM for the frame buffers, the camera's
/// peripherals, core 1 (the detector) with the DMA channel of its strip copies, and core 0's
/// PIE token for the chunk copy.
pub struct Resources {
    pub psram: &'static mut [u8],
    pub i2c0: I2C0<'static>,
    pub vdma: VDMA_CH0<'static>,
    pub vdma1: VDMA_CH1<'static>,
    pub cpu: CPU_CTRL<'static>,
    pub pie: &'static esp32p4_pie::Pie,
}

/// What the capture task gets.
pub(crate) struct CameraResources {
    /// The whole-frame receiver's buffers ([`camera::BUFFERS`] x [`camera::BUFFER_LEN`]).
    pub psram: &'static mut [u8],
    /// The strip path's frame buffers.
    pool: frames::Pool,
    pub i2c0: I2C0<'static>,
    pub vdma: VDMA_CH0<'static>,
    pub mode: Mode,
    pub controls: Controls,
    /// Core 1 (`None`: it did not start; whole frames only, no detection).
    core1: Option<&'static mut espion_core1::Core1>,
    /// The result side of detection.
    work: detect::Work,
}

/// The stream state the tasks share; every access is one short critical section.
struct Shared {
    /// Where answers and frames go: the source of the last valid request.
    peer: Option<IpEndpoint>,
    last_request: Instant,
    stream: StreamSettings,
    generation: u32,
    /// Bumped to abort the frame in flight (new mode, new encoding, stream off).
    epoch: u32,
    /// A frame is being sent.
    sending: bool,
    /// `max_fps`: the earliest capture time of the next frame to send.
    next_send: Instant,
    counters: Counters,
    /// The JPEG encoder's part of the state (the encode task keeps it).
    encoder: EncoderState,
    /// The last frame sent whole (the send task keeps it).
    timing: Option<FrameTiming>,
    /// The detector's state: settings, threshold of the next frame, background, counters.
    /// Its detect times are summed in `detect_us`.
    detect: DetectState,
    /// Detect times since the last [`State`]: sum, frames, longest (µs).
    detect_us: (u64, u32, u32),
    /// Bumped by every camera start and settings change: core 1's results of frames detected
    /// with the settings before are not used.
    detect_serial: u32,
    /// The sensor format the detection settings are for ([`DetectSettings::for_format`] at a
    /// start in the other format).
    detect_format: Format,
    /// The automatic exposure is on (the applied `CameraSettings::auto_exposure`).
    auto_exposure: bool,
    /// Exposure and gain in use (the capture task keeps it).
    exposure: Exposure,
    /// The automatic exposure's cap in the running mode, lines (0: no mode running yet).
    ae_cap: u16,
}

impl Shared {
    /// Abandons the frame in flight: the send task drops it at its next chunk, and the next
    /// captured frame may go out at once.
    fn abort(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.sending = false;
    }

    /// Frames go out: the stream is on and a host asked within [`STREAM_TIMEOUT_MS`].
    fn active(&self, now: Instant) -> bool {
        self.stream.enabled && self.host_active(now)
    }

    /// A host asked within [`STREAM_TIMEOUT_MS`]: `FrameBlobs` go out, whether frames do or
    /// not.
    fn host_active(&self, now: Instant) -> bool {
        self.peer.is_some()
            && now - self.last_request < Duration::from_millis(u64::from(STREAM_TIMEOUT_MS))
    }
}

static SHARED: Mutex<RefCell<Shared>> = Mutex::new(RefCell::new(Shared {
    peer: None,
    last_request: Instant::from_ticks(0),
    stream: DEFAULT_STREAM,
    generation: 1,
    epoch: 0,
    sending: false,
    next_send: Instant::from_ticks(0),
    counters: Counters {
        frames_captured: 0,
        frames_sent: 0,
        frames_skipped: 0,
        capture_errors: 0,
        datagrams_sent: 0,
    },
    encoder: EncoderState {
        quality: DEFAULT_STREAM.jpeg_quality,
        frame_len: 0,
        encode_us: 0,
        errors: 0,
        frames_skipped: 0,
    },
    timing: None,
    detect: DetectState {
        settings: DetectSettings::DEFAULT,
        threshold: DetectSettings::DEFAULT.threshold,
        background_median: 0,
        background_sigma: 0,
        detect_us_mean: 0,
        detect_us_max: 0,
        frames_detected: 0,
        frames_skipped: 0,
        frames_cut: 0,
        blobs_dropped: 0,
        blobs_filtered: 0,
        fault: false,
        available: true,
    },
    detect_us: (0, 0, 0),
    detect_serial: 0,
    detect_format: DEFAULT_CAMERA.format,
    auto_exposure: DEFAULT_CAMERA.auto_exposure,
    exposure: Exposure {
        lines: DEFAULT_CAMERA.exposure_lines,
        gain_sixteenths: DEFAULT_CAMERA.gain_sixteenths,
    },
    ae_cap: 0,
}));

/// Microseconds the send task spent on frames, from a frame's first chunk to its end (wraps):
/// how busy the link is, for the automatic quality.
static SEND_BUSY_US: AtomicU32 = AtomicU32::new(0);

fn shared<R>(f: impl FnOnce(&mut Shared) -> R) -> R {
    critical_section::with(|cs| f(&mut SHARED.borrow_ref_mut(cs)))
}

/// Datagrams on [`CAM_PORT`] that were not a valid request (for the diag report).
static BAD_DATAGRAMS: AtomicU32 = AtomicU32::new(0);

/// What the control task (and diag) ask of the capture task.
enum Cmd {
    /// Restart the camera in `mode`; bump the generation if the layout changes. Answered on
    /// [`DONE`].
    Apply {
        mode: Mode,
        controls: Controls,
        layout_changed: bool,
    },
    /// Exposure, gain, test pattern of the running mode. Answered on [`DONE`].
    Controls(Controls),
    /// Sensor stream off behind the receiver's back (diag; no answer).
    Stall,
    /// Exposure and gain of the running mode from the automatic exposure (the detect task; no
    /// answer).
    Exposure(Exposure),
}

static CMDS: Channel<CriticalSectionRawMutex, Cmd, 2> = Channel::new();
static DONE: Signal<CriticalSectionRawMutex, Result<(), CamError>> = Signal::new();

/// An answer for the send task, which encodes it straight into the socket's ring.
struct Outgoing {
    to: IpEndpoint,
    response: Response,
}

static RESPONSES: Channel<CriticalSectionRawMutex, Outgoing, 2> = Channel::new();

/// What a [`Job`] sends: a captured frame, or a JPEG file the encode task made of one.
enum Payload {
    /// A captured frame, held until the send ends or is abandoned.
    Raw(frames::FrameBuf),
    /// An output buffer of the encode task ([`jpeg::Out`]), freed the same way.
    Jpeg(jpeg::Out),
}

impl core::ops::Deref for Payload {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Raw(f) => f,
            Self::Jpeg(o) => o,
        }
    }
}

/// A frame handed to the send task (by the capture task, or by the encode task for
/// [`Encoding::Jpeg`]), with what its chunks carry: the send task sends the first `len` bytes
/// of `data` with `encoding`, `width` and `height`, copying chunk by chunk straight into the
/// socket's ring.
struct Job {
    data: Payload,
    /// `data` is packed RAW10 and goes out as RAW8: each chunk takes the first 4 bytes of
    /// every 5.
    high_bits: bool,
    /// Frame number: captured frames since boot, from 0.
    number: u32,
    generation: u32,
    /// [`Shared::epoch`] when it was handed over; the send task drops it when that changes.
    epoch: u32,
    /// Encoding on the wire.
    encoding: Encoding,
    /// Bytes of the frame on the wire (`frame_len` of its chunks): width x height for a raw
    /// encoding, the file's length for JPEG.
    len: u32,
    width: u16,
    height: u16,
    timestamp_us: u64,
    /// The JPEG encode the frame went through ([`FrameTiming::encode`]).
    encode: Option<Span>,
}

/// The next frame for the send task. Raw: at most one in flight ([`Shared::sending`]). JPEG:
/// the newest file waits here while the send task sends the one before; the encode task
/// replaces a waiting file with a newer one.
static JOB: Signal<CriticalSectionRawMutex, Job> = Signal::new();

/// A fault the next mode change injects ([`camera::Fault`]; 0 none, set by diag).
static FAULT: AtomicU8 = AtomicU8::new(0);

/// Spawns the service's tasks. Logs `stream: ready` once the socket listens; the camera and
/// the JPEG encoder come up in their tasks right after.
///
/// PSRAM: the whole-frame receiver's buffers at its start, then the strip path's frame
/// buffers, then the encoder's output buffers.
#[allow(
    clippy::large_stack_frames,
    reason = "once at boot, on the main stack, which has most of internal RAM"
)]
pub fn start(spawner: Spawner, stack: Stack<'static>, res: Resources) {
    let mode = match check::validate(&DEFAULT_CAMERA, DEFAULT_STREAM.encoding) {
        Ok(m) => m,
        Err(e) => {
            error!("stream: default settings refused: {e}");
            return;
        }
    };
    let cam_len = camera::BUFFERS * camera::BUFFER_LEN;
    let pool_len = frames::POOL_BUFFERS * camera::BUFFER_LEN;
    let out_len = jpeg::OUT_BUFFERS * jpeg::OUT_LEN;
    if res.psram.len() < cam_len + pool_len + out_len {
        error!("stream: PSRAM too small for {cam_len} + {pool_len} + {out_len} B of buffers");
        return;
    }
    let (cam_mem, rest) = res.psram.split_at_mut(cam_len);
    let (pool_mem, rest) = rest.split_at_mut(pool_len);
    let (out_mem, _) = rest.split_at_mut(out_len);
    let mut sock = UdpSocket::new(
        stack,
        RX_META_BUF.take(),
        RX_BUF.take(),
        TX_META_BUF.take(),
        TX_BUF.take(),
    );
    sock.bind(CAM_PORT).expect("bind the camera port");
    let sock = &*SOCKET.init(RefCell::new(sock));
    let applied = check::applied(&DEFAULT_CAMERA, &mode);
    let (blobs_tx, blobs_rx) = detect::blobs_queue().expect("the blobs queue, once");
    spawner.spawn(send::task(sock, res.pie, blobs_rx).unwrap());
    spawner.spawn(control_task(sock, applied, mode).unwrap());
    spawner.spawn(diag::task(stack).unwrap());
    info!(
        "stream: ready, camera stream on UDP {CAM_PORT}, test hooks on UDP {DIAG_PORT}, {} B of buffers in internal RAM",
        RAM_BYTES
    );
    let core1 = detect::start(res.cpu, res.vdma1);
    if core1.is_none() {
        shared(|s| s.detect.fault = true);
    }
    spawner.spawn(detect::task(core1.is_some()).unwrap());
    spawner.spawn(jpeg::task(out_mem).unwrap());
    spawner.spawn(
        capture::task(CameraResources {
            psram: cam_mem,
            pool: frames::Pool::new(pool_mem),
            i2c0: res.i2c0,
            vdma: res.vdma,
            mode,
            controls: check::controls(&applied),
            core1,
            work: detect::Work::new(blobs_tx),
        })
        .unwrap(),
    );
}

/// The settings as applied, owned by the control task.
struct Control {
    camera: CameraSettings,
    mode: Mode,
    /// The last request executed, its source and its result. A repeat of it (the host repeats
    /// a request it has no answer to yet, with the same number) is answered with that result
    /// and not executed again: a mode change takes up to a second (2 Hz modes, a failed start)
    /// and its repeats queue meanwhile; executed again, a repeat of a refused change could
    /// succeed after the host took the refusal as the answer.
    last: Option<(IpEndpoint, Request, Result<(), CamError>)>,
}

#[allow(
    clippy::large_stack_frames,
    reason = "a spawned task: its future lives in the task's static storage, and its poll frame \
    (1.6 KiB, mostly the inlined channel futures) is on the main stack, which has most of \
    internal RAM"
)]
#[embassy_executor::task]
async fn control_task(
    sock: &'static RefCell<UdpSocket<'static>>,
    camera: CameraSettings,
    mode: Mode,
) {
    cpu::name_current("control");
    let buf = REQUEST_BUF.take();
    let mut c = Control {
        camera,
        mode,
        last: None,
    };
    loop {
        let Ok((n, meta)) = poll_fn(|cx| poll_recv(sock, buf, cx)).await else {
            BAD_DATAGRAMS.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let Some(req) = accept(&buf[..n], meta.endpoint) else {
            continue;
        };
        let result = match c.prepare(&req, meta.endpoint) {
            Step::Done(r) => r,
            Step::Camera { cmd, applied, mode } => {
                // No automatic-exposure step is queued while the request runs: one queued
                // after it would land on the sensor after the request's controls.
                let ae = shared(|s| core::mem::replace(&mut s.auto_exposure, false));
                CMDS.send(cmd).await;
                let r = DONE.wait().await;
                if r.is_err() {
                    shared(|s| s.auto_exposure = ae);
                }
                c.finish(r, applied, mode)
            }
        };
        c.last = Some((meta.endpoint, req, result));
        RESPONSES
            .send(c.answer(req.seq, meta.endpoint, result))
            .await;
    }
}

/// One poll of the socket's receive side (the send task polls the send side).
#[inline(never)]
fn poll_recv(
    sock: &RefCell<UdpSocket<'static>>,
    buf: &mut [u8],
    cx: &mut core::task::Context<'_>,
) -> core::task::Poll<Result<(usize, embassy_net::udp::UdpMetadata), embassy_net::udp::RecvError>> {
    sock.borrow().poll_recv_from(buf, cx)
}

/// Decodes a datagram; a valid request makes its source the destination and refreshes the
/// timeout. Anything else is counted and dropped.
#[inline(never)]
fn accept(datagram: &[u8], from: IpEndpoint) -> Option<Request> {
    let Ok(Message::Request(req)) = Message::decode(datagram) else {
        BAD_DATAGRAMS.fetch_add(1, Ordering::Relaxed);
        return None;
    };
    let now = Instant::now();
    shared(|s| {
        s.peer = Some(from);
        s.last_request = now;
    });
    Some(req)
}

/// What a request needs after its checks.
enum Step {
    /// Answered without the camera.
    Done(Result<(), CamError>),
    /// The capture task applies `cmd`; then the settings are `applied` with `mode`.
    Camera {
        cmd: Cmd,
        applied: CameraSettings,
        mode: Mode,
    },
}

impl Control {
    /// Every check of the request, and what applying it takes; a repeat of the last request
    /// is not applied again.
    #[inline(never)]
    fn prepare(&mut self, req: &Request, from: IpEndpoint) -> Step {
        if let Some((ep, last, r)) = &self.last
            && *ep == from
            && last == req
        {
            return Step::Done(*r);
        }
        match &req.op {
            Op::GetState | Op::Poll => Step::Done(Ok(())),
            Op::SetStream(s) => Step::Done(self.set_stream(*s)),
            Op::SetDetect(d) => Step::Done(self.set_detect(*d)),
            Op::SetCamera(c) => {
                let mode = match check::validate(c, shared(|s| s.stream.encoding)) {
                    Ok(m) => m,
                    Err(e) => return Step::Done(Err(e)),
                };
                let applied = check::applied(c, &mode);
                let controls = check::controls(&applied);
                if mode != self.mode {
                    Step::Camera {
                        cmd: Cmd::Apply {
                            mode,
                            controls,
                            layout_changed: check::layout(&applied) != check::layout(&self.camera),
                        },
                        applied,
                        mode,
                    }
                } else if controls != self.controls_in_use() {
                    Step::Camera {
                        cmd: Cmd::Controls(controls),
                        applied,
                        mode,
                    }
                } else {
                    self.set_applied(applied);
                    Step::Done(Ok(()))
                }
            }
        }
    }

    /// The controls the sensor runs with: exposure and gain in use (the automatic exposure's
    /// while it is on), the applied test pattern.
    fn controls_in_use(&self) -> Controls {
        let e = shared(|s| s.exposure);
        Controls {
            exposure_lines: e.lines,
            gain: ov9281::Gain::from_sixteenths(e.gain_sixteenths),
            ..check::controls(&self.camera)
        }
    }

    /// The camera settings as applied, and whether the automatic exposure runs.
    fn set_applied(&mut self, applied: CameraSettings) {
        self.camera = applied;
        shared(|s| s.auto_exposure = applied.auto_exposure);
    }

    /// Takes the capture task's result: the new settings on success, nothing otherwise.
    #[inline(never)]
    fn finish(
        &mut self,
        r: Result<(), CamError>,
        applied: CameraSettings,
        mode: Mode,
    ) -> Result<(), CamError> {
        if r.is_ok() {
            self.set_applied(applied);
            self.mode = mode;
        }
        r
    }

    /// The response: the state after the request, or the error.
    #[inline(never)]
    fn answer(&self, seq: u32, to: IpEndpoint, r: Result<(), CamError>) -> Outgoing {
        Outgoing {
            to,
            response: Response {
                seq,
                reply: match r {
                    Ok(()) => Reply::State(self.state()),
                    Err(e) => Reply::Error(e),
                },
            },
        }
    }

    /// The checks of [`StreamSettings::check`] against the running mode, then the settings;
    /// the quality in use restarts at the set quality (as the fake camera's).
    fn set_stream(&mut self, st: StreamSettings) -> Result<(), CamError> {
        st.check(self.camera.format, self.camera.width, self.camera.height)?;
        let now = Instant::now();
        shared(|s| {
            if st.encoding != s.stream.encoding {
                s.generation = s.generation.wrapping_add(1);
                s.abort();
            } else if !st.enabled {
                s.abort();
            }
            s.stream = st;
            s.encoder.quality = st.jpeg_quality;
            s.next_send = now;
        });
        Ok(())
    }

    /// The checks of [`DetectSettings::check`] against the running format, then the settings.
    /// The threshold in use becomes the set one with the automatic threshold off; with it on,
    /// the next frame's background and blobs choose it. Frames core 1 detected under the
    /// settings before are stale (a new detection serial): neither counted nor sent.
    fn set_detect(&mut self, d: DetectSettings) -> Result<(), CamError> {
        check::detect(&d, &self.camera)?;
        shared(|s| {
            s.detect.settings = d;
            if !d.auto_threshold {
                s.detect.threshold = d.threshold;
            }
            s.detect_serial = s.detect_serial.wrapping_add(1) & !(1 << 31);
        });
        Ok(())
    }

    /// The state now; the detect time's mean and maximum cover the frames detected since the
    /// last state, and start again.
    fn state(&self) -> State {
        let (stream, generation, counters, encoder, timing, detect, exposure) = shared(|s| {
            let (sum, n, max) = core::mem::take(&mut s.detect_us);
            s.detect.detect_us_mean = sum.checked_div(u64::from(n)).unwrap_or(0) as u32;
            s.detect.detect_us_max = max;
            (
                s.stream,
                s.generation,
                s.counters,
                s.encoder,
                s.timing,
                s.detect,
                s.exposure,
            )
        });
        let mut camera = self.camera;
        if camera.auto_exposure {
            camera.exposure_lines = exposure.lines;
            camera.gain_sixteenths = exposure.gain_sixteenths;
        }
        State {
            camera,
            stream,
            mode: check::mode_info(&self.mode),
            generation,
            counters,
            encoder,
            uptime_us: Instant::now().as_micros(),
            timing,
            detect,
        }
    }
}
