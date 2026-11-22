//! A fake camera that speaks the camera stream protocol completely.
//!
//! It checks every `SetCamera` with the `ov9281` driver as the board does ([`camera::validate`]),
//! "captures" frames at the solver's frame rate, and sends them to the address and port of the
//! last valid request while requests keep coming ([`STREAM_TIMEOUT_MS`]). One frame is in
//! flight at a time: a frame captured while the previous one is still being sent is skipped
//! and counted, as on the board. It can imitate the board's link: a line-rate limit in Mbit/s
//! (Ethernet bytes on the wire, [`WIRE_OVERHEAD`]) and a fraction of datagrams lost.
//!
//! JPEG goes as on the board: capture, then the encoder (its own thread, `jpeg-encoder`, a
//! baseline greyscale file at the quality in use), then the link; the encoder and the link
//! each have one waiting slot holding the newest frame for them, and a frame replaced there is
//! counted (`encoder.frames_skipped`, `frames_skipped`). The automatic quality takes one step of
//! [`auto_quality`] every [`AUTO_QUALITY_PERIOD_MS`] with the link's busy share of the period.
//!
//! Detection goes as on the board: every captured frame the detector can take (its own thread;
//! a frame captured while it is busy is not detected and counts in `detect.frames_skipped`),
//! whether a host polls and frames are sent or not. The detector is
//! `espion-vision`'s on the frame in the sensor's format (RAW8, or RAW10 with the 10-bit
//! threshold), its blobs split by their peaks above the last background, with its background
//! sample; the engine applies the area filter, takes the next
//! automatic threshold, and sends one [`FrameBlobs`] per detected frame while a host polls,
//! whatever "Send frames" and `max_fps` say, after any answer and before the next chunk. A frame
//! the board's core 1 would not keep up with (a model of its time, [`board_detect_ns`]) is cut
//! as on the board: counted, no blobs, its background still moving the automatic threshold. The
//! automatic exposure takes one step of [`auto_exposure`] every [`AUTO_EXPOSURE_PERIOD_MS`] with
//! the highest peak of the blobs sent in the period.
//!
//! The frames ([`render`]) are synthetic and deterministic from the settings and the frame
//! number. With the test pattern off, the scene is [`MARKERS`] soft spots on a dark, noisy
//! background, each on its own Lissajous path with sub-pixel centres ([`marker_centres`]), their
//! brightness proportional to exposure x gain, and the frame number in dark digits top left
//! (repeats and drops show as the number, with some display gain). The test patterns replace
//! the scene, with a white bar that moves 8 pixels per frame (tearing shows as a broken bar) and
//! the frame number in white digits on black.
//!
//! With a simulated screen ([`FakeConfig::screen`]) and the test pattern off, the scene is what
//! the screen target's page shows instead, as the camera sees it through the screen's model
//! (`screen_target::sim`).

use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc, Condvar, LazyLock, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use espion_proto::cam::{
    AUTO_EXPOSURE_PERIOD_MS, AUTO_QUALITY_PERIOD_MS, BLOB_BYTES, Blob, CHUNK_MAX, CamError,
    CameraSettings, Counters, DetectSettings, DetectState, DetectTiming, EncodeSpan, EncoderState,
    Encoding, Exposure, Format, FrameBlobs, FrameChunk, FrameTiming, JPEG_FRAME_MAX, MAX_DATAGRAM,
    Message, Op, Readout, Reply, Request, Response, STREAM_TIMEOUT_MS, Span, State, StreamSettings,
    TestPattern, auto_exposure, auto_exposure_cap, auto_quality, pack_blobs,
};
use espion_vision::{
    DefaultDetector, Split,
    auto::{self, Background, Histogram},
    refine,
};
use ov9281::Mode;

use crate::{camera, screen_target::sim::SimScreen};

/// Ethernet bytes per datagram besides its UDP payload: UDP (8) and IPv6 (40) headers,
/// Ethernet header (14) and FCS (4), preamble and start delimiter (8), inter-frame gap (12).
pub const WIRE_OVERHEAD: usize = 8 + 40 + 14 + 4 + 8 + 12;

/// The camera settings the fake starts with: the full window, RAW8, 120 Hz, automatic
/// exposure.
pub const fn default_camera() -> CameraSettings {
    CameraSettings {
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
    }
}

/// Period of the automatic exposure ([`AUTO_EXPOSURE_PERIOD_MS`]).
const EXPOSURE_PERIOD: Duration = Duration::from_millis(AUTO_EXPOSURE_PERIOD_MS as u64);

/// Period of the automatic quality ([`AUTO_QUALITY_PERIOD_MS`]).
const AUTO_PERIOD: Duration = Duration::from_millis(AUTO_QUALITY_PERIOD_MS as u64);

/// The stream settings the fake starts with: on, JPEG at quality 90 with automatic quality,
/// no rate limit.
pub const fn default_stream() -> StreamSettings {
    StreamSettings {
        enabled: true,
        encoding: Encoding::Jpeg,
        max_fps: 0,
        jpeg_quality: 90,
        auto_quality: true,
    }
}

/// How the fake camera runs.
#[derive(Debug, Clone)]
pub struct FakeConfig {
    /// Address to listen on (port 0: any free port).
    pub bind: SocketAddr,
    /// Line-rate limit in Mbit/s, Ethernet bytes on the wire; `None`: as fast as it can.
    pub mbps: Option<f64>,
    /// Fraction of datagrams (all kinds) lost, 0..=1.
    pub loss: f64,
    /// Seed of the loss pattern.
    pub seed: u64,
    /// Camera settings at start (must pass [`camera::validate`]).
    pub camera: CameraSettings,
    /// Stream settings at start.
    pub stream: StreamSettings,
    /// Detection settings at start (checked against the camera's format).
    pub detect: DetectSettings,
    /// A simulated screen in front of the camera: with the test pattern off, the scene.
    pub screen: Option<Arc<SimScreen>>,
}

impl Default for FakeConfig {
    fn default() -> Self {
        Self {
            bind: (std::net::Ipv6Addr::LOCALHOST, 0).into(),
            mbps: None,
            loss: 0.0,
            seed: 0x9281,
            camera: default_camera(),
            stream: default_stream(),
            detect: DetectSettings::DEFAULT,
            screen: None,
        }
    }
}

/// A running fake camera; stops when dropped.
#[derive(Debug)]
pub struct FakeCamera {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl FakeCamera {
    /// Binds and starts the fake on its own threads.
    pub fn spawn(cfg: FakeConfig) -> io::Result<Self> {
        let invalid = |e: CamError| io::Error::new(io::ErrorKind::InvalidInput, e.to_string());
        let mode = camera::validate(&cfg.camera, cfg.stream.encoding).map_err(invalid)?;
        cfg.stream
            .check(cfg.camera.format, cfg.camera.width, cfg.camera.height)
            .map_err(invalid)?;
        cfg.detect.check(cfg.camera.format).map_err(invalid)?;
        let sock = UdpSocket::bind(cfg.bind)?;
        let addr = sock.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        let rsock = sock.try_clone()?;
        rsock.set_read_timeout(Some(Duration::from_millis(50)))?;
        let rstop = stop.clone();
        let rtx = tx.clone();
        let receiver = thread::Builder::new()
            .name("fake-cam-rx".into())
            .spawn(move || {
                let mut buf = [0u8; 2048];
                while !rstop.load(Ordering::Relaxed) {
                    if let Ok((n, src)) = rsock.recv_from(&mut buf)
                        && rtx.send(Event::Datagram(buf[..n].to_vec(), src)).is_err()
                    {
                        break;
                    }
                }
            })?;

        let slot: Arc<EncoderSlot> = Arc::default();
        let (estop, eslot, etx) = (stop.clone(), slot.clone(), tx.clone());
        let escreen = cfg.screen.clone();
        let encoder = thread::Builder::new()
            .name("fake-cam-jpeg".into())
            .spawn(move || encoder_thread(&eslot, &etx, &estop, escreen.as_deref()))?;

        let detect: Arc<DetectSlot> = Arc::default();
        let (dstop, dslot) = (stop.clone(), detect.clone());
        let dscreen = cfg.screen.clone();
        let detector = thread::Builder::new()
            .name("fake-cam-detect".into())
            .spawn(move || detector_thread(&dslot, &tx, &dstop, dscreen.as_deref()))?;

        let estop = stop.clone();
        let engine = thread::Builder::new()
            .name("fake-cam".into())
            .spawn(move || Engine::new(sock, rx, slot, detect, cfg, mode).run(&estop))?;
        Ok(Self {
            addr,
            stop,
            threads: vec![receiver, encoder, detector, engine],
        })
    }

    /// The address the fake listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for FakeCamera {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// What wakes the engine.
enum Event {
    /// A datagram from `SocketAddr`.
    Datagram(Vec<u8>, SocketAddr),
    /// The encoder finished a frame.
    Encoded(Encoded),
    /// The encoder failed on a frame (its output buffer back for reuse).
    EncodeFailed(Vec<u8>),
    /// The detector finished a frame.
    Detected(Detected),
}

/// A captured frame for the encoder.
struct EncodeJob {
    frame: u32,
    generation: u32,
    camera: CameraSettings,
    quality: u8,
    timestamp_us: u64,
    /// When it was captured (the instant of `timestamp_us`).
    captured: Instant,
}

/// µs from `from` to `to` (0 when `to` is earlier).
fn us_between(from: Instant, to: Instant) -> u32 {
    to.saturating_duration_since(from)
        .as_micros()
        .min(u128::from(u32::MAX)) as u32
}

/// `us` as a `u16`, saturated.
fn sat16(us: u32) -> u16 {
    us.min(u32::from(u16::MAX)) as u16
}

/// A JPEG frame ready for the link.
struct Encoded {
    frame: u32,
    generation: u32,
    width: u16,
    height: u16,
    timestamp_us: u64,
    captured: Instant,
    data: Vec<u8>,
    encode_us: u32,
    /// The encode, after the capture (the whole frame there at its start).
    encode: EncodeSpan,
}

/// The encoder's waiting slot (the newest captured frame for it) and the output buffers.
#[derive(Default)]
struct EncoderSlot {
    state: Mutex<SlotState>,
    wake: Condvar,
}

#[derive(Default)]
struct SlotState {
    job: Option<EncodeJob>,
    pool: Vec<Vec<u8>>,
}

impl EncoderSlot {
    fn lock(&self) -> MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The fake's JPEG encoder: takes the waiting frame, renders it in RAW8, encodes it at its
/// quality and hands the file to the engine.
fn encoder_thread(
    slot: &EncoderSlot,
    tx: &mpsc::Sender<Event>,
    stop: &AtomicBool,
    screen: Option<&SimScreen>,
) {
    let mut raw = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let (job, mut out) = {
            let mut s = slot.lock();
            if s.job.is_none() {
                let _ = slot
                    .wake
                    .wait_timeout(s, Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner());
                continue;
            }
            let job = s.job.take().expect("checked above");
            (job, s.pool.pop().unwrap_or_default())
        };
        render_at(
            screen,
            &job.camera,
            Encoding::Raw8,
            job.frame,
            job.captured,
            &mut raw,
        );
        let t = Instant::now();
        let ok = encode_jpeg(&job.camera, &raw, job.quality, &mut out).is_ok()
            && out.len() <= JPEG_FRAME_MAX as usize;
        let end = Instant::now();
        let encode_us = us_between(t, end);
        let event = if ok {
            Event::Encoded(Encoded {
                frame: job.frame,
                generation: job.generation,
                width: job.camera.width,
                height: job.camera.height,
                timestamp_us: job.timestamp_us,
                captured: job.captured,
                data: out,
                encode_us,
                encode: {
                    let start_us = us_between(job.captured, t);
                    EncodeSpan {
                        start_us: start_us.min(i32::MAX as u32) as i32,
                        input_end_us: start_us,
                        end_us: us_between(job.captured, end),
                    }
                },
            })
        } else {
            Event::EncodeFailed(out)
        };
        if tx.send(event).is_err() {
            return;
        }
    }
}

/// A captured frame for the detector.
struct DetectJob {
    frame: u32,
    generation: u32,
    camera: CameraSettings,
    timestamp_us: u64,
    /// When the frame was captured ([`DetectJob::timestamp_us`] on this clock).
    captured: Instant,
    /// The frame period, µs: the hold the detector has before the next frame is skipped.
    period_us: u32,
    /// Threshold to detect with, in the sensor format's units.
    threshold: u16,
    /// How the blobs are split by their peaks, as on the board: above the last frame's
    /// background, into blobs the area filter keeps.
    split: Split,
    /// The threshold was chosen automatically.
    auto: bool,
    /// The detection epoch the job was made in ([`Engine::detect_epoch`]).
    epoch: u32,
}

/// A frame through the detector: every blob it found (before the area filter), each measured
/// on the frame (`espion_vision::refine`); none when it was cut.
struct Detected {
    job: DetectJob,
    blobs: Vec<Blob>,
    /// Items the detector lost (`Stats::overflow`: runs, components and blobs beyond its
    /// capacity); with the kept blobs beyond `FRAME_BLOBS_MAX` they make `blobs_dropped`.
    lost: u32,
    /// The board would not have kept up with the frame's detection ([`board_detect_ns`]): cut,
    /// as on the board, its blobs not known.
    cut: bool,
    /// The frame's background sample (a cut frame's too, as on the board).
    background: Option<Background>,
    detect_us: u32,
    /// The detector's start and end on the frame, and the end of the measure.
    started: Instant,
    found: Instant,
    measured: Instant,
}

/// The frame handed to the detector, whether it is busy (a frame handed and not yet done),
/// and the blob buffers.
#[derive(Default)]
struct DetectSlot {
    state: Mutex<DetectSlotState>,
    wake: Condvar,
}

#[derive(Default)]
struct DetectSlotState {
    job: Option<DetectJob>,
    busy: bool,
    pool: Vec<Vec<Blob>>,
}

impl DetectSlot {
    fn lock(&self) -> MutexGuard<'_, DetectSlotState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Most blobs the detector returns per frame (its capacity, `DefaultDetector`).
const DETECT_BLOBS: usize = 256;

/// Core 1's detector time on the board per pixel of a frame with few runs, RAW8 and RAW10, ns
/// (1280x800 at 120 Hz: 1.43 and 2.0 ms a frame, `poc-p4-stream` README, "Measured: detection
/// on strips").
const BOARD_PIXEL_NS: [f64; 2] = [1.4, 1.95];
/// Core 1's detector time on the board per run, ns (`poc-p4-stream` README, "Measured: the
/// automatic threshold and cut frames": a noisy background near the threshold, 3.5 to 4.1 µs a
/// run on top of the frame's time with few runs).
const BOARD_RUN_NS: f64 = 3700.0;

/// Core 1's time to detect a frame of `pixels` in `format` with detector `stats` on the board,
/// ns: a model of its cost, per pixel and per run. The board cuts a frame whose detection falls
/// behind the readout by more than its strips may be held; the fake cuts one whose modelled
/// time is longer than the frame period (the same for a load spread over the frame, as a noisy
/// background near the threshold is).
fn board_detect_ns(format: Format, pixels: usize, stats: &espion_vision::Stats) -> f64 {
    let px = BOARD_PIXEL_NS[usize::from(format == Format::Raw10)];
    pixels as f64 * px + f64::from(stats.runs) * BOARD_RUN_NS
}

/// The fake's detector: takes the frame handed to it, renders it in the sensor's format, finds
/// its blobs, measures each on the frame, samples its background, and hands them to the
/// engine; then it is free for the next captured frame.
fn detector_thread(
    slot: &DetectSlot,
    tx: &mpsc::Sender<Event>,
    stop: &AtomicBool,
    screen: Option<&SimScreen>,
) {
    let mut detector: Box<DefaultDetector> = Box::default();
    let mut found = vec![espion_vision::Blob::default(); DETECT_BLOBS];
    let mut raw = Vec::new();
    let mut hist = Histogram::new();
    while !stop.load(Ordering::Relaxed) {
        let (job, mut blobs) = {
            let mut s = slot.lock();
            if s.job.is_none() {
                let _ = slot
                    .wake
                    .wait_timeout(s, Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner());
                continue;
            }
            let job = s.job.take().expect("checked above");
            (job, s.pool.pop().unwrap_or_default())
        };
        let c = &job.camera;
        let raw10 = c.format == Format::Raw10;
        let encoding = if raw10 {
            Encoding::Raw10
        } else {
            Encoding::Raw8
        };
        render_at(screen, c, encoding, job.frame, job.captured, &mut raw);
        detector.set_split(Some(job.split));
        let t = Instant::now();
        let n = if raw10 {
            detector.process_frame_raw10(&raw, c.width, job.threshold, &mut found)
        } else {
            let t8 = job.threshold.min(255) as u8;
            detector.process_frame(&raw, c.width, t8, &mut found)
        };
        let stats = detector.stats();
        let lost = stats.overflow;
        let pixels = usize::from(c.width) * usize::from(c.height);
        let cut = board_detect_ns(c.format, pixels, &stats) > f64::from(job.period_us) * 1e3;
        let found_at = Instant::now();
        hist.clear();
        if raw10 {
            auto::sample_raw10(&raw, c.width, 0, &mut hist);
        } else {
            auto::sample_raw8(&raw, c.width, 0, &mut hist);
        }
        let background = Background::from_histogram(&hist);
        // A cut frame's blobs are not known.
        let n = if cut { 0 } else { n };
        blobs.clear();
        blobs.extend(found[..n].iter().map(|b| {
            let s = if raw10 {
                refine::refine_raw10(&raw, c.width, c.height, b)
            } else {
                refine::refine_raw8(&raw, c.width, c.height, b)
            };
            Blob {
                x: s.x,
                y: s.y,
                width: s.width,
                flux: s.flux,
                area: b.area,
                peak: b.peak,
            }
        }));
        let measured = Instant::now();
        let detect_us = us_between(t, measured);
        let event = Event::Detected(Detected {
            job,
            blobs,
            lost,
            cut,
            background,
            detect_us,
            started: t,
            found: found_at,
            measured,
        });
        if tx.send(event).is_err() {
            return;
        }
        slot.lock().busy = false;
    }
}

/// The frame being sent.
struct Sending {
    frame: u32,
    generation: u32,
    encoding: Encoding,
    width: u16,
    height: u16,
    timestamp_us: u64,
    captured: Instant,
    /// The JPEG encode it went through.
    encode: Option<EncodeSpan>,
    /// When its send started, µs after the capture.
    send_start_us: u32,
    len: u32,
    offset: u32,
}

struct Engine {
    sock: UdpSocket,
    rx: Receiver<Event>,
    slot: Arc<EncoderSlot>,
    loss: f64,
    rng: u64,
    /// Bytes per second of the line-rate limit.
    rate: Option<f64>,
    tokens: f64,
    refilled: Instant,

    camera: CameraSettings,
    mode: Mode,
    stream: StreamSettings,
    generation: u32,
    counters: Counters,
    encoder: EncoderState,
    start: Instant,
    /// The last frame sent whole.
    timing: Option<FrameTiming>,

    peer: Option<SocketAddr>,
    last_request: Instant,
    /// The last request executed, its source and its result: a repeat (same number, same
    /// source) is answered with that result and not executed again, as on the board.
    last: Option<(SocketAddr, Request, Result<(), CamError>)>,

    period: Duration,
    next_capture: Instant,
    frame: u32,
    next_send: Instant,
    sending: Option<Sending>,
    /// The link's waiting slot: the newest JPEG frame encoded while another was being sent.
    ready: Option<Encoded>,
    /// Automatic quality: end of the current period, link time spent on frames in it, when
    /// the frame being sent started, and `frames_skipped` at the period's start.
    auto_due: Instant,
    link_busy: Duration,
    busy_since: Option<Instant>,
    skipped_at: u32,
    /// The frame being sent (raw: rendered here; JPEG: the encoder's file).
    pixels: Vec<u8>,
    out: [u8; MAX_DATAGRAM],

    detect_slot: Arc<DetectSlot>,
    screen: Option<Arc<SimScreen>>,
    /// The detector as a state reports it: `threshold` is the next frame's, the detect times
    /// are those since the last state (`detect_us_mean` unused here: [`Engine::detect_us`]).
    detect: DetectState,
    /// Changes with the detection settings and the mode: results of frames handed to the
    /// detector before are dropped.
    detect_epoch: u32,
    /// Detect time sum and frames since the last state.
    detect_us: (u64, u32),
    /// Automatic exposure: end of the current period, highest blob peak sent in it.
    exposure_due: Instant,
    exposure_peak: Option<u8>,
    /// A FrameBlobs' blob bytes.
    blob_bytes: Vec<u8>,
}

/// Most chunks sent in a row before requests and captures are looked at again.
const BURST: usize = 32;

/// Token bucket depth: datagrams the line-rate limit lets out back to back.
const BUCKET_DATAGRAMS: f64 = 8.0;

impl Engine {
    fn new(
        sock: UdpSocket,
        rx: Receiver<Event>,
        slot: Arc<EncoderSlot>,
        detect_slot: Arc<DetectSlot>,
        cfg: FakeConfig,
        mode: Mode,
    ) -> Self {
        let now = Instant::now();
        let period = Duration::from_nanos(mode.frame_ns() as u64);
        Self {
            sock,
            rx,
            slot,
            loss: cfg.loss.clamp(0.0, 1.0),
            rng: cfg.seed | 1,
            rate: cfg.mbps.map(|m| m * 1e6 / 8.0),
            tokens: 0.0,
            refilled: now,
            camera: camera::applied(&cfg.camera, &mode),
            mode,
            stream: cfg.stream,
            generation: 1,
            counters: Counters::default(),
            encoder: EncoderState {
                quality: cfg.stream.jpeg_quality,
                ..EncoderState::default()
            },
            start: now,
            timing: None,
            peer: None,
            last_request: now,
            last: None,
            period,
            next_capture: now + period,
            frame: 0,
            next_send: now,
            sending: None,
            ready: None,
            auto_due: now + AUTO_PERIOD,
            link_busy: Duration::ZERO,
            busy_since: None,
            skipped_at: 0,
            pixels: Vec::new(),
            out: [0; MAX_DATAGRAM],
            detect_slot,
            screen: cfg.screen.clone(),
            detect: DetectState {
                settings: cfg.detect,
                threshold: cfg.detect.threshold,
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
            detect_epoch: 0,
            detect_us: (0, 0),
            exposure_due: now + EXPOSURE_PERIOD,
            exposure_peak: None,
            blob_bytes: vec![0; MAX_DATAGRAM],
        }
    }

    fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            while let Ok(e) = self.rx.try_recv() {
                self.event(e);
            }
            let now = Instant::now();
            if now >= self.auto_due {
                self.auto_quality(now);
            }
            if now >= self.exposure_due {
                self.auto_exposure(now);
            }
            if now >= self.next_capture {
                if now - self.next_capture > self.period * 8 {
                    // Fell far behind (the process was stopped): resume from now.
                    self.next_capture = now;
                }
                let at = self.next_capture;
                self.next_capture += self.period;
                self.capture(at, now);
                continue;
            }
            if !self.active(now) {
                self.drop_frames();
            }
            let mut sent = 0;
            let mut wait_tokens = None;
            while self.sending.is_some() && sent < BURST {
                // A whole datagram's line time must be there; the unused part is given back.
                match self.take_tokens(MAX_DATAGRAM + WIRE_OVERHEAD) {
                    None => {
                        let n = self.send_chunk();
                        if self.rate.is_some() {
                            self.tokens += (MAX_DATAGRAM - n) as f64;
                        }
                        sent += 1;
                    }
                    Some(wait) => {
                        wait_tokens = Some(wait);
                        break;
                    }
                }
            }
            if self.sending.is_some() && wait_tokens.is_none() {
                continue;
            }
            let now = Instant::now();
            let mut until = self
                .next_capture
                .min(self.auto_due)
                .min(self.exposure_due)
                .saturating_duration_since(now);
            if let Some(w) = wait_tokens {
                until = until.min(w);
            }
            match self.rx.recv_timeout(until.min(Duration::from_millis(20))) {
                Ok(e) => self.event(e),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn event(&mut self, e: Event) {
        match e {
            Event::Datagram(d, src) => self.handle(&d, src),
            Event::Encoded(f) => self.encoded(f),
            Event::EncodeFailed(buf) => {
                self.encoder.errors = self.encoder.errors.wrapping_add(1);
                self.slot.lock().pool.push(buf);
            }
            Event::Detected(d) => self.detected(d),
        }
    }

    /// A frame through the detector: the area filter, the next automatic threshold, the
    /// counters, and its blobs to the host while one polls. A cut frame, as on the board: only
    /// counted, and its background moves the automatic threshold.
    fn detected(&mut self, d: Detected) {
        let Detected {
            job,
            blobs,
            lost,
            cut,
            background,
            detect_us,
            started,
            found,
            measured,
        } = d;
        if job.epoch != self.detect_epoch {
            self.detect_slot.lock().pool.push(blobs);
            return;
        }
        let det = &mut self.detect;
        let format = job.camera.format;
        if cut {
            det.frames_skipped = det.frames_skipped.wrapping_add(1);
            det.frames_cut = det.frames_cut.wrapping_add(1);
            follow(det, format, background);
            self.detect_slot.lock().pool.push(blobs);
            return;
        }
        det.frames_detected = det.frames_detected.wrapping_add(1);
        det.detect_us_max = det.detect_us_max.max(detect_us);
        self.detect_us.0 += u64::from(detect_us);
        self.detect_us.1 += 1;
        let settings = det.settings;
        let kept = blobs.iter().filter(|b| settings.keeps(b.area));
        let filtered = blobs.len() - kept.clone().count();
        let (n, all) = pack_blobs(kept.clone().copied(), &mut self.blob_bytes);
        let sent = &self.blob_bytes[..n];
        let dropped = lost as usize + (blobs.len() - filtered) - n / BLOB_BYTES;
        det.blobs_filtered = det.blobs_filtered.wrapping_add(filtered as u32);
        det.blobs_dropped = det.blobs_dropped.wrapping_add(dropped as u32);
        follow(det, format, background);
        let now = Instant::now();
        let Some(peer) = self.peer.filter(|_| self.host_active(now)) else {
            self.detect_slot.lock().pool.push(blobs);
            return;
        };
        let sent_peak = sent
            .as_chunks::<BLOB_BYTES>()
            .0
            .iter()
            .map(|b| Blob::from_bytes(*b).peak)
            .max();
        self.exposure_peak = self.exposure_peak.max(sent_peak);
        let c = &job.camera;
        let mut flags = 0;
        if dropped > 0 || !all {
            flags |= FrameBlobs::OVERFLOW;
        }
        if job.auto {
            flags |= FrameBlobs::AUTO_THRESHOLD;
        }
        let n = FrameBlobs {
            frame: job.frame,
            generation: job.generation,
            timestamp_us: job.timestamp_us,
            format: c.format,
            width: c.width,
            height: c.height,
            threshold: job.threshold,
            detect_us,
            // The whole frame as one strip, detected after the capture.
            timing: DetectTiming {
                strips_lead_us: 0,
                strips_end_us: us_between(job.captured, found),
                published_us: us_between(job.captured, measured),
                queued_us: us_between(job.captured, now),
                measure_us: sat16(us_between(found, measured)),
                strips: 1,
                strip_lines: c.height,
                late_strips: 0,
                start_max_us: sat16(us_between(job.captured, started)),
                detect_max_us: sat16(us_between(started, found)),
                copy_wait_max_us: 0,
                hold_max_us: sat16(us_between(job.captured, found)),
                hold_budget_us: sat16(job.period_us),
            },
            flags,
            blobs: sent,
        }
        .encode(&mut self.out)
        .expect("FrameBlobs fit a datagram")
        .len();
        self.detect_slot.lock().pool.push(blobs);
        if self.rate.is_some() {
            // Blobs share the line; like the answers they may overdraw the bucket.
            self.tokens -= (n + WIRE_OVERHEAD) as f64;
        }
        self.transmit(n, peer);
    }

    /// One step of the automatic exposure at the end of a period (`cam::auto_exposure`), on
    /// the highest peak of the blobs sent in it. A controls change: the stream goes on.
    fn auto_exposure(&mut self, now: Instant) {
        if self.camera.auto_exposure {
            let cap = auto_exposure_cap(self.mode.line_ns(), self.mode.max_exposure_lines());
            let next = auto_exposure(
                Exposure {
                    lines: self.camera.exposure_lines,
                    gain_sixteenths: self.camera.gain_sixteenths,
                },
                self.exposure_peak,
                cap,
            );
            self.camera.exposure_lines = next.lines;
            self.camera.gain_sixteenths = next.gain_sixteenths;
        }
        self.exposure_peak = None;
        self.exposure_due = (self.exposure_due + EXPOSURE_PERIOD).max(now);
    }

    /// A JPEG file from the encoder: sent now if the link is free, else it waits in the
    /// link's slot (replacing an older waiting one, which counts as skipped).
    fn encoded(&mut self, f: Encoded) {
        self.encoder.frame_len = f.data.len() as u32;
        self.encoder.encode_us = f.encode_us;
        if f.generation != self.generation
            || self.stream.encoding != Encoding::Jpeg
            || !self.active(Instant::now())
        {
            self.slot.lock().pool.push(f.data);
            return;
        }
        if self.sending.is_some() {
            if let Some(old) = self.ready.replace(f) {
                self.counters.frames_skipped = self.counters.frames_skipped.wrapping_add(1);
                self.slot.lock().pool.push(old.data);
            }
        } else {
            self.start_jpeg(f);
        }
    }

    fn start_jpeg(&mut self, f: Encoded) {
        let old = std::mem::replace(&mut self.pixels, f.data);
        self.slot.lock().pool.push(old);
        let now = Instant::now();
        self.busy_since = Some(now);
        self.sending = Some(Sending {
            frame: f.frame,
            generation: f.generation,
            encoding: Encoding::Jpeg,
            width: f.width,
            height: f.height,
            timestamp_us: f.timestamp_us,
            captured: f.captured,
            encode: Some(f.encode),
            send_start_us: us_between(f.captured, now),
            len: self.pixels.len() as u32,
            offset: 0,
        });
    }

    /// Drops the frame being sent and the frames waiting for the encoder and the link.
    fn drop_frames(&mut self) {
        if self.sending.take().is_some()
            && let Some(t) = self.busy_since.take()
        {
            self.link_busy += t.elapsed();
        }
        let mut slot = self.slot.lock();
        slot.job = None;
        if let Some(r) = self.ready.take() {
            slot.pool.push(r.data);
        }
    }

    /// One step of the automatic quality at the end of a period (`cam::auto_quality`).
    fn auto_quality(&mut self, now: Instant) {
        if let Some(t) = self.busy_since.as_mut() {
            self.link_busy += now - *t;
            *t = now;
        }
        let skipped = self.counters.frames_skipped != self.skipped_at;
        let busy_pct = (self.link_busy.as_secs_f64() * 100.0 / AUTO_PERIOD.as_secs_f64())
            .round()
            .min(100.0) as u8;
        if self.stream.encoding == Encoding::Jpeg && self.stream.auto_quality && self.active(now) {
            self.encoder.quality = auto_quality(
                self.encoder.quality,
                self.stream.jpeg_quality,
                skipped,
                busy_pct,
            );
        }
        self.skipped_at = self.counters.frames_skipped;
        self.link_busy = Duration::ZERO;
        self.auto_due = (self.auto_due + AUTO_PERIOD).max(now);
    }

    /// Takes `bytes` of line time from the bucket, or says how long until there is enough.
    fn take_tokens(&mut self, bytes: usize) -> Option<Duration> {
        let rate = self.rate?;
        let now = Instant::now();
        let cap = BUCKET_DATAGRAMS * (MAX_DATAGRAM + WIRE_OVERHEAD) as f64;
        self.tokens = (self.tokens + (now - self.refilled).as_secs_f64() * rate).min(cap);
        self.refilled = now;
        let need = bytes as f64;
        if self.tokens >= need {
            self.tokens -= need;
            None
        } else {
            Some(Duration::from_secs_f64((need - self.tokens) / rate))
        }
    }

    /// A host polls: answers and blobs go to it.
    fn host_active(&self, now: Instant) -> bool {
        self.peer.is_some()
            && now - self.last_request < Duration::from_millis(STREAM_TIMEOUT_MS as u64)
    }

    /// Frames go to the host.
    fn active(&self, now: Instant) -> bool {
        self.stream.enabled && self.host_active(now)
    }

    fn capture(&mut self, at: Instant, now: Instant) {
        self.counters.frames_captured = self.counters.frames_captured.wrapping_add(1);
        let number = self.frame;
        self.frame = self.frame.wrapping_add(1);
        // The frame's timestamp, which its chunks and its blobs carry.
        let timestamp_us = (at - self.start).as_micros() as u64;
        if self.detect.settings.enabled && !self.detect.fault {
            let job = DetectJob {
                frame: number,
                generation: self.generation,
                camera: self.camera,
                timestamp_us,
                captured: at,
                period_us: self.period.as_micros().min(u128::from(u32::MAX)) as u32,
                threshold: self.detect.threshold,
                split: Split {
                    background: u16::from(self.detect.background_median)
                        << if self.camera.format == Format::Raw10 {
                            2
                        } else {
                            0
                        },
                    min_area: self.detect.settings.area_bounds().0,
                },
                auto: self.detect.settings.auto_threshold,
                epoch: self.detect_epoch,
            };
            let mut s = self.detect_slot.lock();
            if s.busy {
                self.detect.frames_skipped = self.detect.frames_skipped.wrapping_add(1);
            } else {
                (s.job, s.busy) = (Some(job), true);
            }
            drop(s);
            self.detect_slot.wake.notify_one();
        }
        if !self.active(now) {
            return;
        }
        let jpeg = self.stream.encoding == Encoding::Jpeg;
        // Raw: one frame in flight. JPEG: the encoder and the link have waiting slots.
        if !jpeg && self.sending.is_some() {
            self.counters.frames_skipped = self.counters.frames_skipped.wrapping_add(1);
            return;
        }
        if self.stream.max_fps > 0 {
            let slack = self.period / 2;
            if at + slack < self.next_send {
                return;
            }
            let interval = Duration::from_secs_f64(1.0 / self.stream.max_fps as f64);
            self.next_send = (self.next_send + interval).max(at + interval - slack);
        }
        if jpeg {
            let job = EncodeJob {
                frame: number,
                generation: self.generation,
                camera: self.camera,
                quality: self.encoder.quality,
                timestamp_us,
                captured: at,
            };
            if self.slot.lock().job.replace(job).is_some() {
                self.encoder.frames_skipped = self.encoder.frames_skipped.wrapping_add(1);
            }
            self.slot.wake.notify_one();
            return;
        }
        let encoding = self.stream.encoding;
        render_at(
            self.screen.as_deref(),
            &self.camera,
            encoding,
            number,
            at,
            &mut self.pixels,
        );
        self.sending = Some(Sending {
            frame: number,
            generation: self.generation,
            encoding,
            width: self.camera.width,
            height: self.camera.height,
            timestamp_us,
            captured: at,
            encode: None,
            send_start_us: us_between(at, Instant::now()),
            len: self.pixels.len() as u32,
            offset: 0,
        });
    }

    /// Sends the next chunk of the frame being sent; returns the datagram's length.
    fn send_chunk(&mut self) -> usize {
        let (Some(s), Some(peer)) = (self.sending.as_mut(), self.peer) else {
            return 0;
        };
        let start = s.offset as usize;
        let end = (start + CHUNK_MAX).min(s.len as usize);
        let chunk = FrameChunk {
            frame: s.frame,
            generation: s.generation,
            encoding: s.encoding,
            width: s.width,
            height: s.height,
            timestamp_us: s.timestamp_us,
            frame_len: s.len,
            offset: s.offset,
            data: &self.pixels[start..end],
        };
        let n = chunk
            .encode(&mut self.out)
            .expect("a chunk fits a datagram")
            .len();
        s.offset = end as u32;
        let done = end == s.len as usize;
        let (frame, encode, start_us, captured) = (s.frame, s.encode, s.send_start_us, s.captured);
        self.transmit(n, peer);
        if done {
            self.sending = None;
            self.counters.frames_sent = self.counters.frames_sent.wrapping_add(1);
            self.timing = Some(FrameTiming {
                frame,
                encode,
                send: Span {
                    start_us,
                    end_us: us_between(captured, Instant::now()).max(start_us),
                },
            });
            if let Some(t) = self.busy_since.take() {
                self.link_busy += t.elapsed();
            }
            if let Some(next) = self.ready.take() {
                self.start_jpeg(next);
            }
        }
        n
    }

    /// Sends `out[..n]` to `to`, unless the loss pattern drops it.
    fn transmit(&mut self, n: usize, to: SocketAddr) {
        self.counters.datagrams_sent = self.counters.datagrams_sent.wrapping_add(1);
        // xorshift64
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        let draw = (self.rng >> 11) as f64 / (1u64 << 53) as f64;
        if draw < self.loss {
            return;
        }
        // A full socket buffer on the receiver loses the datagram, as a full link would.
        let _ = self.sock.send_to(&self.out[..n], to);
    }

    fn handle(&mut self, datagram: &[u8], src: SocketAddr) {
        let Ok(Message::Request(req)) = Message::decode(datagram) else {
            return;
        };
        let now = Instant::now();
        self.peer = Some(src);
        self.last_request = now;
        let reply = match self.last {
            Some((from, last, r)) if from == src && last == req => r,
            _ => match req.op {
                Op::GetState | Op::Poll => Ok(()),
                Op::SetCamera(c) => self.set_camera(&c, now),
                Op::SetStream(s) => self.set_stream(s, now),
                Op::SetDetect(d) => self.set_detect(d),
            },
        };
        self.last = Some((src, req, reply));
        let reply = match reply {
            Ok(()) => Reply::State(self.state(now)),
            Err(e) => Reply::Error(e),
        };
        let n = Response {
            seq: req.seq,
            reply,
        }
        .encode(&mut self.out)
        .expect("a response fits a datagram")
        .len();
        if self.rate.is_some() {
            // Responses share the line; they may overdraw the bucket.
            self.tokens -= (n + WIRE_OVERHEAD) as f64;
        }
        self.transmit(n, src);
    }

    fn set_camera(&mut self, c: &CameraSettings, now: Instant) -> Result<(), CamError> {
        let mode = camera::validate(c, self.stream.encoding)?;
        let applied = camera::applied(c, &mode);
        if mode != self.mode {
            let layout = |c: &CameraSettings| (c.format, c.width, c.height, c.readout, c.origin);
            if layout(&applied) != layout(&self.camera) {
                self.generation = self.generation.wrapping_add(1);
                // The detector starts over in the new layout, from the settings (the manual
                // threshold in the new format's units).
                let settings = self
                    .detect
                    .settings
                    .for_format(self.camera.format, applied.format);
                self.restart_detection(settings);
            }
            self.mode = mode;
            self.period = Duration::from_nanos(mode.frame_ns() as u64);
            self.next_capture = now + self.period;
            self.drop_frames();
        }
        self.camera = applied;
        Ok(())
    }

    /// As on the board: with the automatic threshold the threshold in use stays until the next
    /// frame chooses one.
    fn set_detect(&mut self, d: DetectSettings) -> Result<(), CamError> {
        d.check(self.camera.format)?;
        let in_use = self.detect.threshold;
        self.restart_detection(d);
        if d.auto_threshold {
            self.detect.threshold = in_use;
        }
        Ok(())
    }

    /// New detection settings: the frames handed to the detector before are dropped, the next
    /// threshold is the settings'.
    fn restart_detection(&mut self, settings: DetectSettings) {
        self.detect_epoch = self.detect_epoch.wrapping_add(1);
        let mut s = self.detect_slot.lock();
        if s.job.take().is_some() {
            s.busy = false;
        }
        drop(s);
        self.detect.settings = settings;
        self.detect.threshold = settings.threshold;
    }

    fn set_stream(&mut self, s: StreamSettings, now: Instant) -> Result<(), CamError> {
        s.check(self.camera.format, self.camera.width, self.camera.height)?;
        if s.encoding != self.stream.encoding {
            self.generation = self.generation.wrapping_add(1);
            self.drop_frames();
        }
        if !s.enabled {
            self.drop_frames();
        }
        self.stream = s;
        self.encoder.quality = s.jpeg_quality;
        self.next_send = now;
        Ok(())
    }

    /// The state; the detect times start over.
    fn state(&mut self, now: Instant) -> State {
        let (sum, n) = std::mem::take(&mut self.detect_us);
        let detect = DetectState {
            detect_us_mean: sum.checked_div(u64::from(n)).unwrap_or(0) as u32,
            ..self.detect
        };
        self.detect.detect_us_max = 0;
        State {
            camera: self.camera,
            stream: self.stream,
            mode: camera::mode_info(&self.mode),
            generation: self.generation,
            counters: self.counters,
            encoder: self.encoder,
            uptime_us: (now - self.start).as_micros() as u64,
            timing: self.timing,
            detect,
        }
    }
}

/// After a frame detected with the settings of `d` in `format`, as on the board: the next
/// frame's threshold and the background in `d`. Automatic: `espion_vision::auto` from the
/// frame's background; a frame without a background keeps the threshold in use. Manual: the
/// set one.
fn follow(d: &mut DetectState, format: Format, background: Option<Background>) {
    if let Some(bg) = background {
        (d.background_median, d.background_sigma) = (bg.median, bg.sigma);
    }
    if !d.settings.auto_threshold {
        d.threshold = d.settings.threshold;
    } else if let Some(bg) = background {
        d.threshold = match format {
            Format::Raw8 => u16::from(auto::threshold_raw8(bg)),
            Format::Raw10 => auto::threshold_raw10(bg),
        };
    }
}

/// A baseline greyscale JPEG of the RAW8 picture `raw` of the settings' size at `quality`,
/// into `out`.
pub fn encode_jpeg(
    c: &CameraSettings,
    raw: &[u8],
    quality: u8,
    out: &mut Vec<u8>,
) -> Result<(), jpeg_encoder::EncodingError> {
    out.clear();
    jpeg_encoder::Encoder::new(&mut *out, quality).encode(
        raw,
        c.width,
        c.height,
        jpeg_encoder::ColorType::Luma,
    )
}

/// The frame `number` captured at `at`: the simulated screen's with the test pattern off, else
/// [`render`]'s.
fn render_at(
    screen: Option<&SimScreen>,
    c: &CameraSettings,
    encoding: Encoding,
    number: u32,
    at: Instant,
    out: &mut Vec<u8>,
) {
    match screen {
        Some(s) if c.test_pattern == TestPattern::Off => s.render(c, encoding, number, at, out),
        _ => render(c, encoding, number, out),
    }
}

/// Frame `number` of the fake for these settings, in the raw `encoding`, into `out` (resized
/// to the frame length). Deterministic: the tests compare received frames with it bit for bit.
/// With the test pattern off, the markers scene ([`marker_centres`]).
///
/// # Panics
///
/// For [`Encoding::Jpeg`]: render RAW8 and [`encode_jpeg`] it.
pub fn render(c: &CameraSettings, encoding: Encoding, number: u32, out: &mut Vec<u8>) {
    let (w, h) = (c.width as usize, c.height as usize);
    let raw_line = |width: u16| {
        encoding
            .line_bytes(width)
            .expect("render makes raw encodings") as usize
    };
    let lb = raw_line(c.width);
    out.resize(lb * h, 0);
    if c.test_pattern == TestPattern::Off {
        render_markers(c, encoding, number, out);
        return;
    }
    let mut line = vec![0u16; w];
    let mut packed = vec![0u8; lb];
    match c.test_pattern {
        TestPattern::Off => unreachable!("rendered above"),
        TestPattern::Bars | TestPattern::RollingBars => {
            let roll = if c.test_pattern == TestPattern::RollingBars {
                number as usize * 4
            } else {
                0
            };
            for (x, v) in line[..w].iter_mut().enumerate() {
                let bar = ((x + roll) % w) * 8 / w;
                *v = (1023 - bar * 1023 / 7) as u16;
            }
            encode_line(encoding, &line[..w], &mut packed);
            for row in out.chunks_exact_mut(lb) {
                row.copy_from_slice(&packed[..lb]);
            }
        }
        TestPattern::GreySquares | TestPattern::BlackWhiteSquares => {
            for (band, rows) in out.chunks_mut(lb * 32).enumerate() {
                for (x, v) in line[..w].iter_mut().enumerate() {
                    let cell = x / 32 + band;
                    *v = if c.test_pattern == TestPattern::GreySquares {
                        ((cell * 64) % 1024) as u16
                    } else {
                        (cell % 2 * 1023) as u16
                    };
                }
                encode_line(encoding, &line[..w], &mut packed);
                for row in rows.chunks_exact_mut(lb) {
                    row.copy_from_slice(&packed[..lb]);
                }
            }
        }
        TestPattern::Random => {
            for (y, row) in out.chunks_exact_mut(lb).enumerate() {
                let mut s = (y as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                for v in line[..w].iter_mut() {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    *v = (s >> 54) as u16;
                }
                encode_line(encoding, &line[..w], row);
            }
        }
    }

    let mut canvas = Canvas {
        encoding,
        w,
        h,
        lb,
        buf: out,
    };
    // The moving bar, 8 pixels per frame.
    let bar_x = (number as usize * 8) % w.saturating_sub(8).max(1);
    canvas.fill(bar_x, 0, 8, h, 1023);
    // The frame number in white on a black box, top left.
    canvas.number(number, true);
}

/// Soft spots of the markers scene.
pub const MARKERS: usize = 3;

/// Standard deviation of a spot, pixels.
pub const MARKER_SIGMA: f64 = 1.5;

/// Brightness of the brightest spot above the background, 10-bit units per exposure line at
/// a gain of 1x: a spot's peak is proportional to exposure x gain ([`marker_amplitudes`]).
pub const MARKER_BRIGHTNESS: f64 = 30.0;

/// Each spot's brightness against the brightest.
const MARKER_LEVELS: [f64; MARKERS] = [1.0, 0.85, 0.7];

/// Each spot's Lissajous path: x and y frequencies (times [`MARKER_RATE`]) and x phase.
const MARKER_PATHS: [(f64, f64, f64); MARKERS] = [
    (2.0, 3.0, std::f64::consts::FRAC_PI_4),
    (3.0, 4.0, std::f64::consts::PI / 5.0),
    (1.0, 1.0, std::f64::consts::FRAC_PI_2),
];

/// Base angular rate of the paths, radians per frame: at 640x400 every spot moves more than
/// half a pixel from one frame to the next (a blob matched to the wrong frame shows).
const MARKER_RATE: f64 = 0.015;

/// Background level, 10-bit.
const BACKGROUND: u16 = 40;

/// Half-width of the background's uniform noise, 10-bit.
const NOISE: u16 = 4;

/// Pixels of the noise sequence each row is a slice of; a multiple of 4.
const NOISE_LEN: usize = 8192;

/// The true centre of each spot in frame `number`, pixels, pixel centres at integer
/// coordinates (the detector's convention). Each spot keeps to its third of the frame's width
/// (x within 0.12 width of the third's centre, y within 0.28 height of 0.55 height), so the
/// spots never touch each other or the frame number.
pub fn marker_centres(c: &CameraSettings, number: u32) -> [(f64, f64); MARKERS] {
    let (w, h) = (f64::from(c.width), f64::from(c.height));
    let t = f64::from(number) * MARKER_RATE;
    std::array::from_fn(|i| {
        let (fx, fy, phase) = MARKER_PATHS[i];
        let cx = w * (2 * i + 1) as f64 / 6.0;
        (
            cx + 0.12 * w * (fx * t + phase).sin(),
            0.55 * h + 0.28 * h * (fy * t).sin(),
        )
    })
}

/// Each spot's peak above the background in these settings, 10-bit (before clipping at 1023).
pub fn marker_amplitudes(c: &CameraSettings) -> [f64; MARKERS] {
    let brightness =
        f64::from(c.exposure_lines) * f64::from(c.gain_sixteenths) / 16.0 * MARKER_BRIGHTNESS;
    MARKER_LEVELS.map(|l| l * brightness)
}

/// The background noise: one sequence of 10-bit values, also as RAW8 and as packed RAW10.
struct Noise {
    v10: Vec<u16>,
    raw8: Vec<u8>,
    raw10: Vec<u8>,
}

impl Noise {
    fn new() -> Self {
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        let v10: Vec<u16> = (0..NOISE_LEN)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                BACKGROUND - NOISE + ((s >> 32) % u64::from(2 * NOISE + 1)) as u16
            })
            .collect();
        let mut raw8 = vec![0; NOISE_LEN];
        let mut raw10 = vec![0; NOISE_LEN / 4 * 5];
        encode_line(Encoding::Raw8, &v10, &mut raw8);
        encode_line(Encoding::Raw10, &v10, &mut raw10);
        Self { v10, raw8, raw10 }
    }
}

static NOISE_TABLE: LazyLock<Noise> = LazyLock::new(Noise::new);

/// Where row `y` of frame `number` starts in the noise sequence: a multiple of 4 (whole RAW10
/// groups) with `width` pixels after it.
fn noise_offset(y: usize, number: u32, width: usize) -> usize {
    // splitmix64
    let mut z = ((y as u64) << 32 | u64::from(number)).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let slots = (NOISE_LEN.saturating_sub(width) / 4).max(1) as u64;
    (z % slots) as usize * 4
}

/// The markers scene: each row a slice of the noise sequence (copied as encoded), the frame
/// number in dark digits, then the spots added to the noise, clipped at full scale.
fn render_markers(c: &CameraSettings, encoding: Encoding, number: u32, out: &mut [u8]) {
    let (w, h) = (c.width as usize, c.height as usize);
    let lb = out.len() / h.max(1);
    let n = &*NOISE_TABLE;
    for (y, row) in out.chunks_exact_mut(lb).enumerate() {
        let at = noise_offset(y, number, w);
        match encoding {
            Encoding::Raw8 => row.copy_from_slice(&n.raw8[at..at + lb]),
            Encoding::Raw10 => row.copy_from_slice(&n.raw10[at / 4 * 5..][..lb]),
            Encoding::Jpeg => unreachable!("render makes raw encodings"),
        }
    }
    let mut canvas = Canvas {
        encoding,
        w,
        h,
        lb,
        buf: out,
    };
    canvas.number(number, false);
    let two_var = 2.0 * MARKER_SIGMA * MARKER_SIGMA;
    for ((cx, cy), a) in marker_centres(c, number)
        .into_iter()
        .zip(marker_amplitudes(c))
    {
        // Out to where the spot adds less than half a level.
        let reach = (two_var * (2.0 * a).max(1.0).ln()).sqrt().ceil() + 1.0;
        let span = |centre: f64, size: usize| {
            let lo = (centre - reach).max(0.0) as usize;
            let hi = ((centre + reach).max(0.0) as usize).min(size.saturating_sub(1));
            lo..=hi
        };
        for y in span(cy, h) {
            let at = noise_offset(y, number, w);
            for x in span(cx, w) {
                let r2 = (x as f64 - cx).powi(2) + (y as f64 - cy).powi(2);
                let v = f64::from(n.v10[at + x]) + a * (-r2 / two_var).exp();
                canvas.fill(x, y, 1, 1, v.round().min(1023.0) as u16);
            }
        }
    }
}

/// Digits 0..=9 in a 3x5 font, row by row from the top, most significant bit first.
const FONT: [u16; 10] = [
    0b111_101_101_101_111,
    0b010_110_010_010_111,
    0b111_001_111_100_111,
    0b111_001_111_001_111,
    0b101_101_111_001_001,
    0b111_100_111_001_111,
    0b111_100_111_101_111,
    0b111_001_001_001_001,
    0b111_101_111_101_111,
    0b111_101_111_001_111,
];

/// Encodes 10-bit pixels: RAW8 the 8 high bits, RAW10 CSI-2 packed (`line.len()` a multiple
/// of 4).
pub(crate) fn encode_line(encoding: Encoding, line: &[u16], out: &mut [u8]) {
    match encoding {
        Encoding::Raw8 => {
            for (o, v) in out.iter_mut().zip(line) {
                *o = (v >> 2) as u8;
            }
        }
        Encoding::Raw10 => {
            let (groups, _) = out.as_chunks_mut::<5>();
            for (o, p) in groups.iter_mut().zip(line.as_chunks::<4>().0) {
                o[0] = (p[0] >> 2) as u8;
                o[1] = (p[1] >> 2) as u8;
                o[2] = (p[2] >> 2) as u8;
                o[3] = (p[3] >> 2) as u8;
                o[4] = ((p[3] & 3) << 6 | (p[2] & 3) << 4 | (p[1] & 3) << 2 | (p[0] & 3)) as u8;
            }
        }
        Encoding::Jpeg => unreachable!("render makes raw encodings"),
    }
}

/// Pixel writes into an encoded frame, clipped to it.
struct Canvas<'a> {
    encoding: Encoding,
    w: usize,
    h: usize,
    lb: usize,
    buf: &'a mut [u8],
}

impl Canvas<'_> {
    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, v: u16) {
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        for yy in y.min(self.h)..y1 {
            let row = &mut self.buf[yy * self.lb..][..self.lb];
            match self.encoding {
                Encoding::Raw8 => row[x.min(x1)..x1].fill((v >> 2) as u8),
                Encoding::Raw10 => {
                    for xx in x..x1 {
                        let (g, i) = (xx / 4 * 5, xx % 4);
                        row[g + i] = (v >> 2) as u8;
                        let shift = 2 * i;
                        row[g + 4] = row[g + 4] & !(3 << shift) | ((v & 3) as u8) << shift;
                    }
                }
                Encoding::Jpeg => unreachable!("render makes raw encodings"),
            }
        }
    }

    /// The frame number in large digits, top left: white on a black box (`white`), or black
    /// on the picture.
    fn number(&mut self, number: u32, white: bool) {
        let scale = (self.h / 100).max(1);
        let digits = number.to_string();
        let (x0, y0) = (2 * scale, 2 * scale);
        if white {
            self.fill(
                x0 - scale,
                y0 - scale,
                digits.len() * 4 * scale + scale,
                7 * scale,
                0,
            );
        }
        let ink = if white { 1023 } else { 0 };
        for (i, d) in digits.bytes().enumerate() {
            let glyph = FONT[(d - b'0') as usize];
            for gy in 0..5 {
                for gx in 0..3 {
                    if glyph >> (14 - (gy * 3 + gx)) & 1 == 1 {
                        self.fill(
                            x0 + (i * 4 + gx) * scale,
                            y0 + gy * scale,
                            scale,
                            scale,
                            ink,
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unpacks CSI-2 RAW10 into 10-bit values.
    fn unpack(raw: &[u8]) -> Vec<u16> {
        raw.as_chunks::<5>()
            .0
            .iter()
            .flat_map(|g| (0..4).map(move |i| (g[i] as u16) << 2 | (g[4] as u16 >> (2 * i)) & 3))
            .collect()
    }

    #[test]
    fn raw8_is_the_high_bits_of_raw10() {
        let c = CameraSettings {
            format: Format::Raw10,
            width: 640,
            height: 400,
            readout: Readout::Binned,
            ..default_camera()
        };
        for pattern in [
            TestPattern::Off,
            TestPattern::Bars,
            TestPattern::RollingBars,
            TestPattern::GreySquares,
            TestPattern::BlackWhiteSquares,
            TestPattern::Random,
        ] {
            let c = CameraSettings {
                test_pattern: pattern,
                ..c
            };
            let (mut r8, mut r10) = (Vec::new(), Vec::new());
            render(&c, Encoding::Raw8, 1234, &mut r8);
            render(&c, Encoding::Raw10, 1234, &mut r10);
            assert_eq!(r8.len(), 640 * 400);
            assert_eq!(r10.len(), 640 * 400 * 5 / 4);
            let high: Vec<u8> = unpack(&r10).iter().map(|v| (v >> 2) as u8).collect();
            assert_eq!(high, r8, "{pattern:?}");
        }
    }

    #[test]
    fn frames_differ_and_exposure_shows() {
        let c = default_camera();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        render(&c, Encoding::Raw8, 1, &mut a);
        render(&c, Encoding::Raw8, 2, &mut b);
        assert_ne!(a, b);
        let dim = CameraSettings {
            exposure_lines: 5,
            ..c
        };
        render(&dim, Encoding::Raw8, 1, &mut b);
        let sum = |f: &[u8]| f.iter().map(|&v| u64::from(v)).sum::<u64>();
        assert!(sum(&b) < sum(&a));
    }

    /// The 10-bit pixel `(x, y)` of a RAW10 frame `width` wide.
    fn pixel10(raw: &[u8], width: usize, x: usize, y: usize) -> u16 {
        unpack(&raw[y * width / 4 * 5..][..width / 4 * 5])[x]
    }

    #[test]
    fn spots_peak_at_their_centres_in_proportion_to_exposure_and_gain() {
        let c = CameraSettings {
            format: Format::Raw10,
            exposure_lines: 10,
            gain_sixteenths: 32,
            ..default_camera()
        };
        let mut raw = Vec::new();
        for number in [0, 77, 1000] {
            render(&c, Encoding::Raw10, number, &mut raw);
            for ((cx, cy), a) in marker_centres(&c, number)
                .into_iter()
                .zip(marker_amplitudes(&c))
            {
                let (x, y) = (cx.round() as usize, cy.round() as usize);
                let v = f64::from(pixel10(&raw, 1280, x, y));
                let r2 = (x as f64 - cx).powi(2) + (y as f64 - cy).powi(2);
                let want = a * (-r2 / (2.0 * MARKER_SIGMA * MARKER_SIGMA)).exp();
                assert!(
                    (v - f64::from(BACKGROUND) - want).abs() <= f64::from(NOISE) + 1.0,
                    "frame {number}: {v} at ({x}, {y}), spot {want:.1}"
                );
            }
        }
        assert_eq!(marker_amplitudes(&c)[0], 600.0);
        // Clipped at full scale.
        let bright = CameraSettings {
            exposure_lines: 200,
            ..c
        };
        render(&bright, Encoding::Raw10, 5, &mut raw);
        let (cx, cy) = marker_centres(&bright, 5)[0];
        let v = pixel10(&raw, 1280, cx.round() as usize, cy.round() as usize);
        assert_eq!(v, 1023);
    }

    #[test]
    fn spots_keep_apart_and_move_more_than_half_a_pixel_per_frame() {
        let c = CameraSettings {
            width: 640,
            height: 400,
            readout: Readout::Binned,
            ..default_camera()
        };
        let mut slowest = f64::MAX;
        let mut closest = f64::MAX;
        let mut prev = marker_centres(&c, 0);
        for n in 1..5000 {
            let now = marker_centres(&c, n);
            for (i, &(x, y)) in now.iter().enumerate() {
                assert!((0.0..640.0).contains(&x) && (100.0..400.0).contains(&y));
                slowest = slowest.min((x - prev[i].0).hypot(y - prev[i].1));
                for &(ox, oy) in &now[i + 1..] {
                    closest = closest.min((x - ox).hypot(y - oy));
                }
            }
            prev = now;
        }
        assert!(slowest > 0.5, "{slowest} px per frame");
        assert!(closest > 30.0, "{closest} px apart");
    }
}
