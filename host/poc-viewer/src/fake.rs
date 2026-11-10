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
//! The frames ([`render`]) are synthetic and deterministic from the settings and the frame
//! number: diagonal stripes that move every frame, scaled by exposure x gain, a white bar that
//! moves 8 pixels per frame (tearing shows as a broken bar) and the frame number in large
//! digits (repeats and drops show as the number). The test patterns replace the stripes.

use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use espion_proto::cam::{
    AUTO_QUALITY_PERIOD_MS, CHUNK_MAX, CamError, CameraSettings, Counters, EncoderState, Encoding,
    Format, FrameChunk, JPEG_FRAME_MAX, MAX_DATAGRAM, Message, Op, Readout, Reply, Request,
    Response, STREAM_TIMEOUT_MS, State, StreamSettings, TestPattern, auto_quality,
};
use ov9281::Mode;

use crate::camera;

/// Ethernet bytes per datagram besides its UDP payload: UDP (8) and IPv6 (40) headers,
/// Ethernet header (14) and FCS (4), preamble and start delimiter (8), inter-frame gap (12).
pub const WIRE_OVERHEAD: usize = 8 + 40 + 14 + 4 + 8 + 12;

/// The camera settings the fake starts with: the full window, RAW8, 120 Hz.
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
    }
}

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
#[derive(Debug, Clone, Copy)]
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
        let (estop, eslot) = (stop.clone(), slot.clone());
        let encoder = thread::Builder::new()
            .name("fake-cam-jpeg".into())
            .spawn(move || encoder_thread(&eslot, &tx, &estop))?;

        let estop = stop.clone();
        let engine = thread::Builder::new()
            .name("fake-cam".into())
            .spawn(move || Engine::new(sock, rx, slot, cfg, mode).run(&estop))?;
        Ok(Self {
            addr,
            stop,
            threads: vec![receiver, encoder, engine],
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
}

/// A captured frame for the encoder.
struct EncodeJob {
    frame: u32,
    generation: u32,
    camera: CameraSettings,
    quality: u8,
    timestamp_us: u64,
}

/// A JPEG frame ready for the link.
struct Encoded {
    frame: u32,
    generation: u32,
    width: u16,
    height: u16,
    timestamp_us: u64,
    data: Vec<u8>,
    encode_us: u32,
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
fn encoder_thread(slot: &EncoderSlot, tx: &mpsc::Sender<Event>, stop: &AtomicBool) {
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
        render(&job.camera, Encoding::Raw8, job.frame, &mut raw);
        let t = Instant::now();
        let ok = encode_jpeg(&job.camera, &raw, job.quality, &mut out).is_ok()
            && out.len() <= JPEG_FRAME_MAX as usize;
        let encode_us = t.elapsed().as_micros() as u32;
        let event = if ok {
            Event::Encoded(Encoded {
                frame: job.frame,
                generation: job.generation,
                width: job.camera.width,
                height: job.camera.height,
                timestamp_us: job.timestamp_us,
                data: out,
                encode_us,
            })
        } else {
            Event::EncodeFailed(out)
        };
        if tx.send(event).is_err() {
            return;
        }
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
        }
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
        self.busy_since = Some(Instant::now());
        self.sending = Some(Sending {
            frame: f.frame,
            generation: f.generation,
            encoding: Encoding::Jpeg,
            width: f.width,
            height: f.height,
            timestamp_us: f.timestamp_us,
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

    fn active(&self, now: Instant) -> bool {
        self.stream.enabled
            && self.peer.is_some()
            && now - self.last_request < Duration::from_millis(STREAM_TIMEOUT_MS as u64)
    }

    fn capture(&mut self, at: Instant, now: Instant) {
        self.counters.frames_captured = self.counters.frames_captured.wrapping_add(1);
        let number = self.frame;
        self.frame = self.frame.wrapping_add(1);
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
        let timestamp_us = (at - self.start).as_micros() as u64;
        if jpeg {
            let job = EncodeJob {
                frame: number,
                generation: self.generation,
                camera: self.camera,
                quality: self.encoder.quality,
                timestamp_us,
            };
            if self.slot.lock().job.replace(job).is_some() {
                self.encoder.frames_skipped = self.encoder.frames_skipped.wrapping_add(1);
            }
            self.slot.wake.notify_one();
            return;
        }
        let encoding = self.stream.encoding;
        render(&self.camera, encoding, number, &mut self.pixels);
        self.sending = Some(Sending {
            frame: number,
            generation: self.generation,
            encoding,
            width: self.camera.width,
            height: self.camera.height,
            timestamp_us,
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
        self.transmit(n, peer);
        if done {
            self.sending = None;
            self.counters.frames_sent = self.counters.frames_sent.wrapping_add(1);
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
            }
            self.mode = mode;
            self.period = Duration::from_nanos(mode.frame_ns() as u64);
            self.next_capture = now + self.period;
            self.drop_frames();
        }
        self.camera = applied;
        Ok(())
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

    fn state(&self, now: Instant) -> State {
        State {
            camera: self.camera,
            stream: self.stream,
            mode: camera::mode_info(&self.mode),
            generation: self.generation,
            counters: self.counters,
            encoder: self.encoder,
            uptime_us: (now - self.start).as_micros() as u64,
        }
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

/// Frame `number` of the fake for these settings, in the raw `encoding`, into `out` (resized
/// to the frame length). Deterministic: the tests compare received frames with it bit for bit.
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
    let mut line = vec![0u16; w + 1024];
    let mut packed = vec![0u8; raw_line((w + 1024) as u16)];
    match c.test_pattern {
        TestPattern::Off => {
            // Stripes: one base line, each row a slice of it shifted by a multiple of 4
            // pixels (so RAW10 rows stay on whole 5-byte groups).
            let gain = c.exposure_lines as f32 * c.gain_sixteenths as f32 / (16.0 * 400.0);
            for (i, v) in line.iter_mut().enumerate() {
                let p = (i % 1024) as f32;
                let tri = if p < 512.0 {
                    p * 2.0
                } else {
                    (1023.0 - p) * 2.0
                };
                *v = (tri * gain).min(1023.0) as u16;
            }
            encode_line(encoding, &line, &mut packed);
            for (y, row) in out.chunks_exact_mut(lb).enumerate() {
                let shift = 4 * ((y + 2 * number as usize) % 256);
                let at = raw_line(shift as u16);
                row.copy_from_slice(&packed[at..at + lb]);
            }
        }
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
    // The frame number on a black box, top left.
    let scale = (h / 100).max(1);
    let digits = number.to_string();
    let (x0, y0) = (2 * scale, 2 * scale);
    canvas.fill(
        x0 - scale,
        y0 - scale,
        digits.len() * 4 * scale + scale,
        7 * scale,
        0,
    );
    for (i, d) in digits.bytes().enumerate() {
        let glyph = FONT[(d - b'0') as usize];
        for gy in 0..5 {
            for gx in 0..3 {
                if glyph >> (14 - (gy * 3 + gx)) & 1 == 1 {
                    canvas.fill(
                        x0 + (i * 4 + gx) * scale,
                        y0 + gy * scale,
                        scale,
                        scale,
                        1023,
                    );
                }
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
fn encode_line(encoding: Encoding, line: &[u16], out: &mut [u8]) {
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
            exposure_lines: 20,
            ..c
        };
        render(&dim, Encoding::Raw8, 1, &mut b);
        let mean = |f: &[u8]| f.iter().map(|&v| v as u64).sum::<u64>() / f.len() as u64;
        assert!(mean(&b) < mean(&a));
    }
}
