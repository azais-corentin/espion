//! A fake camera that speaks the camera stream protocol completely.
//!
//! It checks every `SetCamera` with the `ov9281` driver as the board does ([`camera::validate`]),
//! "captures" frames at the solver's frame rate, and sends them to the address and port of the
//! last valid request while requests keep coming ([`STREAM_TIMEOUT_MS`]). One frame is in
//! flight at a time: a frame captured while the previous one is still being sent is skipped
//! and counted, as on the board. It can imitate the board's link: a line-rate limit in Mbit/s
//! (Ethernet bytes on the wire, [`WIRE_OVERHEAD`]) and a fraction of datagrams lost.
//!
//! The frames ([`render`]) are synthetic and deterministic from the settings and the frame
//! number: diagonal stripes that move every frame, scaled by exposure x gain, a white bar that
//! moves 8 pixels per frame (tearing shows as a broken bar) and the frame number in large
//! digits (repeats and drops show as the number). The test patterns replace the stripes.

use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use espion_proto::cam::{
    CHUNK_MAX, CamError, CameraSettings, Counters, Encoding, Format, FrameChunk, MAX_DATAGRAM,
    Message, Op, Readout, Reply, Response, STREAM_TIMEOUT_MS, State, StreamSettings, TestPattern,
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

/// The stream settings the fake starts with: on, RAW8, no rate limit.
pub const fn default_stream() -> StreamSettings {
    StreamSettings {
        enabled: true,
        encoding: Encoding::Raw8,
        max_fps: 0,
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
        let mode = camera::validate(&cfg.camera, cfg.stream.encoding)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let sock = UdpSocket::bind(cfg.bind)?;
        let addr = sock.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        let rsock = sock.try_clone()?;
        rsock.set_read_timeout(Some(Duration::from_millis(50)))?;
        let rstop = stop.clone();
        let receiver = thread::Builder::new()
            .name("fake-cam-rx".into())
            .spawn(move || {
                let mut buf = [0u8; 2048];
                while !rstop.load(Ordering::Relaxed) {
                    if let Ok((n, src)) = rsock.recv_from(&mut buf)
                        && tx.send((buf[..n].to_vec(), src)).is_err()
                    {
                        break;
                    }
                }
            })?;

        let estop = stop.clone();
        let engine = thread::Builder::new()
            .name("fake-cam".into())
            .spawn(move || Engine::new(sock, rx, cfg, mode).run(&estop))?;
        Ok(Self {
            addr,
            stop,
            threads: vec![receiver, engine],
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
    rx: Receiver<(Vec<u8>, SocketAddr)>,
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
    start: Instant,

    peer: Option<SocketAddr>,
    last_request: Instant,

    period: Duration,
    next_capture: Instant,
    frame: u32,
    next_send: Instant,
    sending: Option<Sending>,
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
        rx: Receiver<(Vec<u8>, SocketAddr)>,
        cfg: FakeConfig,
        mode: Mode,
    ) -> Self {
        let now = Instant::now();
        let period = Duration::from_nanos(mode.frame_ns() as u64);
        Self {
            sock,
            rx,
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
            start: now,
            peer: None,
            last_request: now,
            period,
            next_capture: now + period,
            frame: 0,
            next_send: now,
            sending: None,
            pixels: Vec::new(),
            out: [0; MAX_DATAGRAM],
        }
    }

    fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            while let Ok((d, src)) = self.rx.try_recv() {
                self.handle(&d, src);
            }
            let now = Instant::now();
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
                self.sending = None;
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
            let mut until = self.next_capture.saturating_duration_since(now);
            if let Some(w) = wait_tokens {
                until = until.min(w);
            }
            match self.rx.recv_timeout(until.min(Duration::from_millis(20))) {
                Ok((d, src)) => self.handle(&d, src),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
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
        if self.sending.is_some() {
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
        let encoding = self.stream.encoding;
        render(&self.camera, encoding, number, &mut self.pixels);
        self.sending = Some(Sending {
            frame: number,
            generation: self.generation,
            encoding,
            width: self.camera.width,
            height: self.camera.height,
            timestamp_us: (at - self.start).as_micros() as u64,
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
        if end == s.len as usize {
            self.sending = None;
            self.counters.frames_sent = self.counters.frames_sent.wrapping_add(1);
        }
        self.transmit(n, peer);
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
        let reply = match req.op {
            Op::GetState | Op::Poll => Ok(()),
            Op::SetCamera(c) => self.set_camera(&c, now),
            Op::SetStream(s) => self.set_stream(s, now),
        };
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
            self.sending = None;
        }
        self.camera = applied;
        Ok(())
    }

    fn set_stream(&mut self, s: StreamSettings, now: Instant) -> Result<(), CamError> {
        camera::check_encoding(self.camera.format, s.encoding)?;
        if s.encoding != self.stream.encoding {
            self.generation = self.generation.wrapping_add(1);
            self.sending = None;
        }
        if !s.enabled {
            self.sending = None;
        }
        self.stream = s;
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
            uptime_us: (now - self.start).as_micros() as u64,
        }
    }
}

/// Frame `number` of the fake for these settings, in `encoding`, into `out` (resized to the
/// frame length). Deterministic: the tests compare received frames with it bit for bit.
pub fn render(c: &CameraSettings, encoding: Encoding, number: u32, out: &mut Vec<u8>) {
    let (w, h) = (c.width as usize, c.height as usize);
    let lb = encoding.line_bytes(c.width) as usize;
    out.resize(lb * h, 0);
    let mut line = vec![0u16; w + 1024];
    let mut packed = vec![0u8; encoding.line_bytes((w + 1024) as u16) as usize];
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
                let at = encoding.line_bytes(shift as u16) as usize;
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
