//! The camera stream service: the protocol of `espion_proto::cam` on UDP [`CAM_PORT`], as the
//! viewer's fake camera (`host/poc-viewer/src/fake.rs`) behaves, with the real OV9281.
//!
//! Everything runs on core 0's executor, with the network (the README has the measurements
//! behind that, and the two-core attempt that hung). Tasks:
//!
//! - **control** ([`control_task`]): receives the requests, checks them ([`check`]), applies
//!   them (a mode or controls through the capture task), and queues the answer, the full
//!   [`State`] or the error, for the send task.
//! - **capture** ([`capture::task`]): owns the sensor and the receiver. Whole frames go into
//!   PSRAM; for each one it counts, applies the stream rules (destination and timeout, one
//!   frame in flight for the raw encodings, `max_fps`) and hands the frame to the send task, to
//!   the encode task ([`Encoding::Jpeg`]) or drops it. It runs the mode changes, and restarts
//!   the camera when frames stop.
//! - **encode** ([`jpeg::task`]): the JPEG stage: the hardware encoder, from the receiver's
//!   buffer into one of three output buffers, the newest frame at each stage, the automatic
//!   quality ([`jpeg`] has the pipeline).
//! - **send** ([`send::task`]): the only task that sends on the socket: the answers
//!   first, then the frame in flight, chunk by chunk on the protocol's grid, to the address and
//!   port of the last valid request; each chunk is encoded straight into the socket's ring, the
//!   one CPU copy of the pixels or of the JPEG file (a RAW10 frame sent as RAW8 is reduced to
//!   its high bits in that same copy).
//! - **diag** ([`diag::task`], UDP [`DIAG_PORT`]): the test hooks: the CPU and counter
//!   report, a log marker, fault injection, and a sensor stall.
//!
//! The socket is shared by control (receive only) and send (send only) through a `RefCell`
//! borrowed only inside a poll, so each of smoltcp's two wakers has one owner.

mod capture;
mod check;
mod diag;
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
use esp_hal::peripherals::{I2C0, VDMA_CH0};
use esp32p4_csi::Frame;
use espion_proto::cam::{
    CAM_PORT, CamError, CameraSettings, Counters, EncoderState, Encoding, Format, MAX_DATAGRAM,
    Message, Op, Readout, Reply, Request, Response, STREAM_TIMEOUT_MS, State, StreamSettings,
    TestPattern,
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

/// Bytes of internal RAM the service's sockets and buffers take.
pub const RAM_BYTES: usize = RX_BYTES
    + TX_PACKETS * MAX_DATAGRAM
    + MAX_DATAGRAM
    + (RX_META + TX_META) * core::mem::size_of::<PacketMetadata>()
    + diag::RAM_BYTES;

/// What the service needs besides the stack: PSRAM for the frame buffers, the camera's
/// peripherals, and core 0's PIE token for the chunk copy.
pub struct Resources {
    pub psram: &'static mut [u8],
    pub i2c0: I2C0<'static>,
    pub vdma: VDMA_CH0<'static>,
    pub pie: &'static esp32p4_pie::Pie,
}

/// What the capture task gets.
pub(crate) struct CameraResources {
    /// The receiver's frame buffers ([`camera::BUFFERS`] x [`camera::BUFFER_LEN`]).
    pub psram: &'static mut [u8],
    pub i2c0: I2C0<'static>,
    pub vdma: VDMA_CH0<'static>,
    pub mode: Mode,
    pub controls: Controls,
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
        self.stream.enabled
            && self.peer.is_some()
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
    /// A receiver buffer, held until the send ends or is abandoned.
    Raw(Frame),
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
/// PSRAM: the receiver's frame buffers at its start, then the encoder's output buffers.
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
    let out_len = jpeg::OUT_BUFFERS * jpeg::OUT_LEN;
    if res.psram.len() < cam_len + out_len {
        error!("stream: PSRAM too small for {cam_len} + {out_len} B of buffers");
        return;
    }
    let (cam_mem, rest) = res.psram.split_at_mut(cam_len);
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
    spawner.spawn(send::task(sock, res.pie).unwrap());
    spawner.spawn(control_task(sock, applied, mode).unwrap());
    spawner.spawn(diag::task(stack).unwrap());
    info!(
        "stream: ready, camera stream on UDP {CAM_PORT}, test hooks on UDP {DIAG_PORT}, {} B of buffers in internal RAM",
        RAM_BYTES
    );
    spawner.spawn(jpeg::task(out_mem).unwrap());
    spawner.spawn(
        capture::task(CameraResources {
            psram: cam_mem,
            i2c0: res.i2c0,
            vdma: res.vdma,
            mode,
            controls: check::controls(&applied),
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
                CMDS.send(cmd).await;
                let r = DONE.wait().await;
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
                } else if controls != check::controls(&self.camera) {
                    Step::Camera {
                        cmd: Cmd::Controls(controls),
                        applied,
                        mode,
                    }
                } else {
                    self.camera = applied;
                    Step::Done(Ok(()))
                }
            }
        }
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
            self.camera = applied;
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

    fn state(&self) -> State {
        let (stream, generation, counters, encoder) =
            shared(|s| (s.stream, s.generation, s.counters, s.encoder));
        State {
            camera: self.camera,
            stream,
            mode: check::mode_info(&self.mode),
            generation,
            counters,
            encoder,
            uptime_us: Instant::now().as_micros(),
        }
    }
}
