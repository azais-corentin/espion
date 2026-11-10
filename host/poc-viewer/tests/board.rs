//! The viewer's network layer ([`poc_viewer::net::Client`]) against the real board running
//! `poc-p4-stream`'s `Stream` service. Ignored by default: run them with the board on `eth0`
//! (`POC_CAMERA` overrides the address, default `[fe80::4553:9281%eth0]:5009`):
//!
//! ```sh
//! cargo test -p poc-viewer --release --test board -- --ignored --nocapture --test-threads=1 settings
//! POC_SOAK_SECS=600 cargo test -p poc-viewer --release --test board -- --ignored --nocapture soak
//! ```
//!
//! `settings` applies a list of camera and stream settings and checks every answer (the state,
//! or the refusal with its values, against the `ov9281` driver run here) and, for each accepted
//! mode, the frames: size, whole, numbers increasing, the rate at the host against
//! min(sensor rate, link rate for the frame size), no capture error on the board. It also checks
//! exposure and gain against the frames' brightness, the test patterns, RAW10 sent as RAW8,
//! `max_fps`, stream off and on, the stream timeout, the request round trip while frames go out
//! at the link rate, and the board's error paths through its test hooks (UDP 5010). It prints
//! one line per step and fails at the end with every failed check.
//!
//! JPEG (sections `jpeg`, `quality`, `auto`, `jpeg_refusals`, `switching`, `agreement`): every
//! frame taken is decoded and must be one baseline greyscale JPEG of the mode's size; sizes,
//! the quality and encode time in use and the rate at the host against the sensor's are
//! printed for 1280x800 at 120 Hz and at the sensor's maximum and the other windows at theirs;
//! quality settings are applied and reported and a higher quality gives larger files;
//! automatic quality lowers the quality when the link is full and raises it back to the set
//! quality when there is room, and holds it when off; the refusals (JPEG with a RAW10 mode,
//! with a size not a multiple of 8, a quality outside 1..=100) carry their values; switching
//! between JPEG and the raw encodings and back; and a JPEG frame agrees with a raw frame of
//! the same static scene taken right after (PSNR; the bar test pattern within the codec's own
//! error: no worse than the same raw frame encoded here at the same quality).
//! Run only these with `POC_ONLY=jpeg,quality,auto,jpeg_refusals,switching,agreement`.
//!
//! `soak` streams 640x400 binned at the link rate and changes the mode every 20 s, restarts the
//! client and sends malformed datagrams in between, for `POC_SOAK_SECS` (default 600).

use std::{
    fmt::Write as _,
    net::{SocketAddr, UdpSocket},
    thread,
    time::{Duration, Instant},
};

use espion_proto::cam::{
    AUTO_QUALITY_MIN, CHUNK_MAX, CamError, CameraSettings, Counters, EncoderState, Encoding,
    Format, FrameChunk, JPEG_QUALITY_MAX, JPEG_QUALITY_MIN, JpegRefusal, MAX_DATAGRAM, Message, Op,
    Origin, Readout, Reply, Request, STREAM_TIMEOUT_MS, State, StreamSettings, TestPattern,
    chunk_count,
};
use poc_viewer::{
    addr, camera, jpeg,
    net::{Client, Command, Frame, Stats},
};

/// The board's test hooks (`firmware/poc-p4-stream/src/stream/diag.rs`).
const DIAG_PORT: u16 = 5010;
/// How long an answer may take: a mode change waits for the first frame (2 Hz: 0.5 s).
const ANSWER_WAIT: Duration = Duration::from_secs(5);
/// Line rate of the link, bytes per second.
const LINE_BYTES_PER_S: f64 = 100e6 / 8.0;
/// Ethernet bytes per datagram besides its UDP payload (`poc_viewer::fake::WIRE_OVERHEAD`).
const WIRE_OVERHEAD: f64 = 86.0;

fn camera_addr() -> SocketAddr {
    let s = std::env::var("POC_CAMERA").unwrap_or_else(|_| "[fe80::4553:9281%eth0]:5009".into());
    addr::parse(&s).expect("camera address")
}

/// The board: a client of its camera stream, and a socket for its test hooks.
struct Board {
    camera: SocketAddr,
    client: Option<Client>,
    diag: UdpSocket,
    /// Every failed check, reported at the end.
    failures: Vec<String>,
}

/// Frames of one measurement window.
#[derive(Debug, Default)]
struct Window {
    /// Whole frames the client received (all generations).
    whole: u64,
    /// Frames taken and checked (the newest at each look).
    taken: u32,
    /// Frames of another size, encoding or generation.
    wrong: u32,
    /// Frame numbers that did not increase.
    backwards: u32,
    /// Frames dropped by the client for a missing chunk.
    incomplete: u64,
    /// JPEG frames taken that are not one baseline greyscale JPEG of the mode's size.
    bad_jpeg: u32,
    /// Why the first of them failed.
    jpeg_error: Option<String>,
    /// Smallest and largest frame taken, bytes.
    sizes: (usize, usize),
    /// Mean length of the frames received whole, bytes.
    mean_len: f64,
    /// Time to decode a JPEG frame here (the viewer's decoder), mean, ms.
    decode_ms: f64,
    /// The encoder over the window: errors and frames skipped as differences, quality, frame
    /// length and encode time at its end.
    encoder: EncoderState,
    /// Host rate from the first to the last whole frame (frames per second).
    fps: f64,
    /// Payload rate at the host, Mbit/s.
    mbps: f64,
    /// The board's counters over the window.
    board: Counters,
    /// The last frame taken.
    last: Option<Frame>,
}

impl Board {
    fn connect() -> Self {
        let camera = camera_addr();
        let local = addr::local_for(camera).expect("local address");
        let diag = UdpSocket::bind(local).expect("diag socket");
        let mut d = camera;
        d.set_port(DIAG_PORT);
        diag.connect(d).expect("connect diag");
        diag.set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut b = Self {
            camera,
            client: None,
            diag,
            failures: Vec::new(),
        };
        b.reconnect();
        b
    }

    fn reconnect(&mut self) {
        self.client = None;
        let c = Client::connect(self.camera, || {}).expect("client connects");
        println!(
            "client bound {}, receive buffer {} B",
            c.local_addr(),
            c.rcvbuf()
        );
        self.client = Some(c);
        let s = self
            .c()
            .wait_until(ANSWER_WAIT, |s| s.state.is_some())
            .expect("the board answers");
        assert!(s.state.is_some());
    }

    fn c(&self) -> &Client {
        self.client.as_ref().expect("client")
    }

    fn check(&mut self, ok: bool, what: impl FnOnce() -> String) -> bool {
        if !ok {
            let w = what();
            println!("    FAIL {w}");
            self.failures.push(w);
        }
        ok
    }

    /// One test hook command; its text answer.
    fn diag(&self, cmd: &[u8]) -> String {
        let mut buf = [0u8; 2048];
        for _ in 0..5 {
            self.diag.send(cmd).unwrap();
            if let Ok(n) = self.diag.recv(&mut buf) {
                return String::from_utf8_lossy(&buf[..n]).into_owned();
            }
        }
        panic!("no answer from the test hooks");
    }

    /// Sends `cmd` and waits for its answer.
    fn ask(&self, cmd: Command) -> Reply {
        let before = self.c().stats().answers;
        self.c().send(cmd);
        let op = match cmd {
            Command::GetState => Op::GetState,
            Command::SetCamera(c) => Op::SetCamera(c),
            Command::SetStream(s) => Op::SetStream(s),
        };
        let s = self
            .c()
            .wait_until(ANSWER_WAIT, |s| {
                s.answers > before && s.last_answer_to.is_some_and(|(o, _)| o == op)
            })
            .unwrap_or_else(|| panic!("no answer to {cmd:?}"));
        s.last_answer_to.unwrap().1
    }

    fn state(&self) -> State {
        match self.ask(Command::GetState) {
            Reply::State(s) => s,
            Reply::Error(e) => panic!("GetState refused: {e}"),
        }
    }

    fn set_camera(&self, c: CameraSettings) -> Result<State, CamError> {
        match self.ask(Command::SetCamera(c)) {
            Reply::State(s) => Ok(s),
            Reply::Error(e) => Err(e),
        }
    }

    fn set_stream(&self, s: StreamSettings) -> Result<State, CamError> {
        match self.ask(Command::SetStream(s)) {
            Reply::State(st) => Ok(st),
            Reply::Error(e) => Err(e),
        }
    }

    /// Takes frames for `secs` after `settle` (frames of `state`'s generation only count as
    /// right); the board's counters from a state before and after.
    fn measure(&self, state: &State, settle: Duration, secs: f64) -> Window {
        let c = self.c();
        let end = Instant::now() + settle;
        while Instant::now() < end {
            if let Some(f) = c.take_frame() {
                c.recycle(f.data);
            }
            thread::sleep(Duration::from_millis(1));
        }
        let before = self.state();
        let s0 = c.stats();
        let start = Instant::now();
        let end = start + Duration::from_secs_f64(secs);
        let mut w = Window::default();
        let mut times: Vec<Instant> = Vec::new();
        let mut last_number: Option<u32> = None;
        let len = state
            .stream
            .encoding
            .frame_bytes(state.camera.width, state.camera.height);
        let mut pixels = Vec::new();
        let mut decode = Duration::ZERO;
        w.sizes = (usize::MAX, 0);
        while Instant::now() < end {
            let Some(f) = c.take_frame() else {
                thread::sleep(Duration::from_micros(500));
                continue;
            };
            w.taken += 1;
            w.sizes = (w.sizes.0.min(f.data.len()), w.sizes.1.max(f.data.len()));
            if f.encoding == Encoding::Jpeg {
                let t = Instant::now();
                if let Err(e) = jpeg::decode(&f.data, f.width, f.height, &mut pixels) {
                    w.bad_jpeg += 1;
                    w.jpeg_error
                        .get_or_insert_with(|| format!("frame {}: {e}", f.number));
                }
                decode += t.elapsed();
            }
            if f.generation != state.generation
                || (f.width, f.height, f.encoding)
                    != (
                        state.camera.width,
                        state.camera.height,
                        state.stream.encoding,
                    )
                || len.is_some_and(|l| f.data.len() as u64 != l)
            {
                w.wrong += 1;
            }
            if last_number.is_some_and(|n| f.number.wrapping_sub(n) as i32 <= 0) {
                w.backwards += 1;
            }
            last_number = Some(f.number);
            times.push(f.completed);
            if let Some(old) = w.last.replace(f) {
                c.recycle(old.data);
            }
        }
        let s1 = c.stats();
        let after = self.state();
        if w.taken > 0 {
            w.decode_ms = decode.as_secs_f64() * 1e3 / f64::from(w.taken);
        }
        w.whole = s1.frames_whole - s0.frames_whole;
        w.mean_len = (s1.frame_bytes - s0.frame_bytes) as f64 / w.whole.max(1) as f64;
        w.incomplete = s1.frames_incomplete - s0.frames_incomplete;
        w.mbps = (s1.bytes - s0.bytes) as f64 * 8.0 / secs / 1e6;
        // Every whole frame counts for the rate: the taken ones' times are a subset, so the
        // rate is the whole count over the window (the window is long against a frame).
        w.fps = w.whole as f64 / secs;
        let d = |a: u32, b: u32| a.wrapping_sub(b);
        let (a, b) = (after.counters, before.counters);
        w.board = Counters {
            frames_captured: d(a.frames_captured, b.frames_captured),
            frames_sent: d(a.frames_sent, b.frames_sent),
            frames_skipped: d(a.frames_skipped, b.frames_skipped),
            capture_errors: d(a.capture_errors, b.capture_errors),
            datagrams_sent: d(a.datagrams_sent, b.datagrams_sent),
        };
        let (a, b) = (after.encoder, before.encoder);
        w.encoder = EncoderState {
            errors: d(a.errors, b.errors),
            frames_skipped: d(a.frames_skipped, b.frames_skipped),
            ..a
        };
        w
    }

    /// Applies `want`, checks the answer against the driver's choice, measures and checks the
    /// frames. Returns the state.
    fn mode(&mut self, label: &str, want: CameraSettings, secs: f64) -> Option<State> {
        // The list's exposure, shortened to the mode's longest.
        let want = match camera::build_mode(&want) {
            Ok(m) => CameraSettings {
                exposure_lines: want.exposure_lines.min(m.max_exposure_lines()),
                ..want
            },
            Err(_) => want,
        };
        let encoding = self
            .c()
            .stats()
            .state
            .map_or(Encoding::Raw8, |s| s.stream.encoding);
        let expect = camera::validate(&want, encoding);
        let got = self.set_camera(want);
        let state = match (&expect, got) {
            (Ok(m), Ok(st)) => {
                let ok = st.camera == camera::applied(&want, m) && st.mode == camera::mode_info(m);
                self.check(ok, || {
                    format!("{label}: state {st:?} is not the driver's {m:?}")
                });
                st
            }
            (Err(e), Err(g)) => {
                self.check(*e == g, || {
                    format!("{label}: refused {g:?}, expected {e:?}")
                });
                println!("  {label}: refused as expected: {g}");
                return None;
            }
            (e, g) => {
                self.check(false, || format!("{label}: answer {g:?}, expected {e:?}"));
                return None;
            }
        };
        let w = self.measure(&state, settle(&state), secs);
        self.judge(label, &state, &w);
        Some(state)
    }

    /// Checks a window of an accepted mode and prints its line.
    fn judge(&mut self, label: &str, st: &State, w: &Window) {
        let (lo, hi) = expected_fps(st, w.mean_len);
        println!(
            "  {label}: {:?} {}x{} {:?} at {:?}, {} MHz HTS {} VTS {}, sensor {:.3} Hz; host {:.2} fps \
             (expected {:.2}..{:.2}), {:.1} Mbit/s, {} whole, {} incomplete, {} wrong, {} backwards; \
             board captured {} sent {} skipped {} errors {}",
            st.camera.format,
            st.camera.width,
            st.camera.height,
            st.camera.readout,
            st.camera.origin.map(|o| (o.x, o.y)),
            st.mode.sensor_clock_mhz,
            st.mode.hts,
            st.mode.vts,
            st.mode.frame_rate_mhz as f64 / 1000.0,
            w.fps,
            lo,
            hi,
            w.mbps,
            w.whole,
            w.incomplete,
            w.wrong,
            w.backwards,
            w.board.frames_captured,
            w.board.frames_sent,
            w.board.frames_skipped,
            w.board.capture_errors,
        );
        if st.stream.encoding == Encoding::Jpeg {
            println!(
                "    JPEG: quality set {} in use {} (auto {}), frames {:.0} B mean, {}..{} B taken, \
                 encode {} us, decode here {:.2} ms, {} bad; encoder skipped {} errors {}",
                st.stream.jpeg_quality,
                w.encoder.quality,
                st.stream.auto_quality,
                w.mean_len,
                w.sizes.0,
                w.sizes.1,
                w.encoder.encode_us,
                w.decode_ms,
                w.bad_jpeg,
                w.encoder.frames_skipped,
                w.encoder.errors,
            );
            self.check(w.bad_jpeg == 0, || {
                format!(
                    "{label}: {} frames are not a baseline greyscale JPEG of the mode: {}",
                    w.bad_jpeg,
                    w.jpeg_error.as_deref().unwrap_or("")
                )
            });
            self.check(w.encoder.errors == 0, || {
                format!("{label}: {} encoder errors", w.encoder.errors)
            });
        }
        self.check(w.taken > 0, || format!("{label}: no frame"));
        self.check(w.wrong == 0, || {
            format!("{label}: {} frames of the wrong layout", w.wrong)
        });
        self.check(w.backwards == 0, || {
            format!("{label}: frame numbers went back")
        });
        self.check(w.incomplete == 0, || {
            format!("{label}: {} incomplete frames", w.incomplete)
        });
        self.check(w.board.capture_errors == 0, || {
            format!("{label}: {} capture errors", w.board.capture_errors)
        });
        self.check(w.fps >= lo * 0.95 - 0.5 && w.fps <= hi * 1.02 + 0.5, || {
            format!("{label}: {:.2} fps, expected {lo:.2}..{hi:.2}", w.fps)
        });
    }
}

/// Frames to let pass after a change: at least 300 ms and 3 frame periods.
fn settle(st: &State) -> Duration {
    Duration::from_millis(300).max(Duration::from_nanos(3 * frame_ns(st)))
}

fn frame_ns(st: &State) -> u64 {
    1_000_000_000_000 / u64::from(st.mode.frame_rate_mhz.max(1))
}

/// The host frame rate to expect, a range (RAW10 sent as RAW8: CPU-bound, see below). Raw: low
/// end: the board sends the frame captured after the previous one is out, so every n-th frame
/// where n = ceil(wire time x sensor rate) (with 3 % margin on the wire time). High end: the
/// sensor rate, or the link's frames per second, since the board's socket and DMA rings let the
/// next frame start before the previous one has left. JPEG (frames of `mean_len` bytes): an
/// encoded frame waits for the link, so the link never idles: min(sensor rate, link rate),
/// low end 90 % of it. Both capped by `max_fps`.
fn expected_fps(st: &State, mean_len: f64) -> (f64, f64) {
    let f = st.mode.frame_rate_mhz as f64 / 1000.0;
    let jpeg = st.stream.encoding == Encoding::Jpeg;
    let len = st
        .stream
        .encoding
        .frame_bytes(st.camera.width, st.camera.height)
        .unwrap_or(mean_len.round() as u64) as u32;
    let chunks = chunk_count(len);
    // The chunks' real header length: encode one with values of this order.
    let data = [0u8; CHUNK_MAX];
    let header = FrameChunk {
        frame: st.counters.frames_captured,
        generation: st.generation,
        encoding: st.stream.encoding,
        width: st.camera.width,
        height: st.camera.height,
        timestamp_us: st.uptime_us,
        frame_len: len,
        offset: len.saturating_sub(1) / CHUNK_MAX as u32 * CHUNK_MAX as u32,
        data: &data,
    }
    .encode(&mut [0u8; 2 * MAX_DATAGRAM])
    .map_or(MAX_DATAGRAM - CHUNK_MAX, |d| d.len() - CHUNK_MAX);
    let wire = len as f64 + chunks as f64 * (header as f64 + WIRE_OVERHEAD);
    let t = wire / LINE_BYTES_PER_S;
    let mut lo = if jpeg {
        0.9 * f.min(1.0 / t)
    } else {
        f / (t * f * 1.03).ceil().max(1.0)
    };
    // RAW10 sent as RAW8: the board reduces each chunk as it copies it, which keeps its one
    // core busy (`rtt` section); it reaches about 90 % of the link rate.
    if st.camera.format == Format::Raw10 && st.stream.encoding == Encoding::Raw8 {
        lo *= 0.85;
    }
    let mut hi = f.min(1.0 / t);
    if st.stream.max_fps > 0 {
        let m = st.stream.max_fps as f64;
        lo = lo.min(m);
        hi = hi.min(m);
    }
    (lo, hi)
}

fn crop(format: Format, w: u16, h: u16, hz: f64) -> CameraSettings {
    CameraSettings {
        format,
        width: w,
        height: h,
        readout: Readout::Crop,
        origin: None,
        rate_mhz: (hz * 1000.0).round() as u32,
        tolerance_mhz: 1000,
        exposure_lines: 100,
        gain_sixteenths: 32,
        test_pattern: TestPattern::Off,
    }
}

fn binned(format: Format, w: u16, h: u16, hz: f64) -> CameraSettings {
    CameraSettings {
        readout: Readout::Binned,
        ..crop(format, w, h, hz)
    }
}

fn stream(encoding: Encoding, max_fps: u16) -> StreamSettings {
    StreamSettings {
        enabled: true,
        encoding,
        max_fps,
        jpeg_quality: 90,
        auto_quality: false,
    }
}

/// A JPEG stream at quality `q`, automatic quality `auto`.
fn jstream(q: u8, auto: bool) -> StreamSettings {
    StreamSettings {
        jpeg_quality: q,
        auto_quality: auto,
        ..stream(Encoding::Jpeg, 0)
    }
}

/// The brightness and pattern mode: crop 640x480 RAW8 at 30 Hz (exposures to 3300 lines).
fn scene() -> CameraSettings {
    crop(Format::Raw8, 640, 480, 30.0)
}

/// Mean of a RAW8 frame.
fn mean(f: &Frame) -> f64 {
    f.data.iter().map(|&b| b as u64).sum::<u64>() as f64 / f.data.len() as f64
}

/// The next whole frame of `st`'s generation taken after `skip` newer ones arrived. Panics
/// with the client's and the board's view when none comes.
fn frame_after(b: &Board, st: &State, skip: u64) -> Frame {
    let c = b.c();
    let base = c.stats().frames_whole;
    let end = Instant::now() + ANSWER_WAIT;
    loop {
        if Instant::now() > end {
            let s = c.stats();
            panic!(
                "no frame of generation {} after {} whole; client {}; state {:?}; board {}",
                st.generation,
                s.frames_whole - base,
                line(&s),
                s.state,
                report(b)
            );
        }
        if let Some(f) = c.take_frame() {
            if f.generation == st.generation && c.stats().frames_whole > base + skip {
                return f;
            }
            c.recycle(f.data);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// One part of the `settings` test.
type Section = fn(&mut Board);

/// Runs section `name` unless `POC_ONLY` (comma-separated names) leaves it out.
fn section(name: &str) -> bool {
    let run = std::env::var("POC_ONLY").map_or(true, |o| o.split(',').any(|s| s == name));
    if run {
        println!("== {name}");
    }
    run
}

/// Bars: every even row equals row 0 and every odd row row 1 (RAW8).
fn bars_exact(f: &Frame) -> bool {
    let w = f.width as usize;
    let rows: Vec<&[u8]> = f.data.chunks(w).collect();
    rows.iter().enumerate().all(|(i, r)| *r == rows[i % 2])
}

/// The 8 high bits of a CSI-2 packed RAW10 frame.
fn high_bits(packed: &[u8]) -> Vec<u8> {
    packed
        .chunks(5)
        .flat_map(|g| g[..4].iter().copied())
        .collect()
}

fn report(b: &Board) -> String {
    b.diag(b"R")
}

/// `key=value` from a test-hook report.
fn field(report: &str, key: &str) -> Option<f64> {
    report
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.parse().ok())
}

#[test]
#[ignore = "needs the board on eth0 running poc-p4-stream"]
fn settings() {
    let mut b = Board::connect();
    let st = b.state();
    println!(
        "board up {:.1} s, generation {}, {:?}",
        st.uptime_us as f64 / 1e6,
        st.generation,
        st.counters
    );
    b.set_stream(stream(Encoding::Raw8, 0))
        .expect("RAW8 stream");

    let sections: [(&str, Section); 17] = [
        ("sizes", sizes),
        ("rates", rates),
        ("refusals", refusals),
        ("brightness", brightness),
        ("patterns", patterns),
        ("raw10", raw10),
        ("stream", stream_switches),
        ("jpeg", jpeg_modes),
        ("quality", quality),
        ("auto", auto_quality),
        ("jpeg_refusals", jpeg_refusals),
        ("switching", switching),
        ("agreement", agreement),
        ("rtt", rtt),
        ("faults", faults),
        ("garbage", garbage),
        ("timeout", timeout),
    ];
    for (name, run) in sections {
        if section(name) {
            run(&mut b);
        }
    }

    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
    b.set_camera(fake_default()).expect("default mode");
    b.set_stream(poc_viewer::fake::default_stream())
        .expect("default stream");
    let mut summary = String::new();
    for f in &b.failures {
        let _ = writeln!(summary, "  {f}");
    }
    println!("== {} failed checks", b.failures.len());
    assert!(b.failures.is_empty(), "failed checks:\n{summary}");
}

/// Sizes, readouts, formats (RAW8 stream), origins.
fn sizes(b: &mut Board) {
    let modes: Vec<(&str, CameraSettings)> = vec![
        (
            "crop 1280x800 RAW8 120 Hz",
            crop(Format::Raw8, 1280, 800, 120.0),
        ),
        (
            "crop 1280x720 RAW8 140 Hz",
            crop(Format::Raw8, 1280, 720, 140.0),
        ),
        (
            "crop 1024x768 RAW8 60 Hz",
            crop(Format::Raw8, 1024, 768, 60.0),
        ),
        (
            "crop 640x480 RAW8 200 Hz",
            crop(Format::Raw8, 640, 480, 200.0),
        ),
        (
            "crop 320x240 RAW8 100 Hz",
            crop(Format::Raw8, 320, 240, 100.0),
        ),
        (
            "crop 128x2 RAW8 (smallest) 1000 Hz",
            crop(Format::Raw8, 128, 2, 1000.0),
        ),
        (
            "crop 1280x64 RAW8 1000 Hz",
            crop(Format::Raw8, 1280, 64, 1000.0),
        ),
        (
            "binned 640x400 RAW8 250 Hz",
            binned(Format::Raw8, 640, 400, 250.0),
        ),
        (
            "binned 320x200 RAW8 400 Hz",
            binned(Format::Raw8, 320, 200, 400.0),
        ),
        (
            "binned 128x2 RAW8 (smallest) 200 Hz",
            binned(Format::Raw8, 128, 2, 200.0),
        ),
        (
            "crop 1280x800 RAW10 120 Hz",
            crop(Format::Raw10, 1280, 800, 120.0),
        ),
        (
            "binned 640x400 RAW10 200 Hz",
            binned(Format::Raw10, 640, 400, 200.0),
        ),
        (
            "crop 128x2 RAW10 (smallest) 300 Hz",
            crop(Format::Raw10, 128, 2, 300.0),
        ),
        (
            "crop 640x480 at (0,0)",
            CameraSettings {
                origin: Some(Origin { x: 0, y: 0 }),
                ..crop(Format::Raw8, 640, 480, 100.0)
            },
        ),
        (
            "crop 640x480 at (640,320), bottom right",
            CameraSettings {
                origin: Some(Origin { x: 640, y: 320 }),
                ..crop(Format::Raw8, 640, 480, 100.0)
            },
        ),
        (
            "crop 1216x704 RAW10 at (32,48)",
            CameraSettings {
                origin: Some(Origin { x: 32, y: 48 }),
                ..crop(Format::Raw10, 1216, 704, 100.0)
            },
        ),
        (
            "binned 320x200 at (96,36)",
            CameraSettings {
                origin: Some(Origin { x: 96, y: 36 }),
                ..binned(Format::Raw8, 320, 200, 300.0)
            },
        ),
    ];
    for (label, m) in modes {
        b.mode(label, m, 3.0);
    }
}

/// Rates: 2 Hz to the fastest at 80 and at 96 MHz.
fn rates(b: &mut Board) {
    for (name, f, w, h, r) in [
        ("crop 1280x800", Format::Raw8, 1280, 800, Readout::Crop),
        ("crop 640x480", Format::Raw8, 640, 480, Readout::Crop),
        ("binned 640x400", Format::Raw8, 640, 400, Readout::Binned),
        ("binned 320x200", Format::Raw8, 320, 200, Readout::Binned),
        (
            "crop 1280x720 RAW10",
            Format::Raw10,
            1280,
            720,
            Readout::Crop,
        ),
    ] {
        let (f80, f96) = camera::fastest_rates(f, w, h, r);
        for (what, mhz) in [
            ("2 Hz", 2_000),
            ("fastest at 80 MHz", f80),
            ("fastest at 96 MHz", f96),
        ] {
            let c = CameraSettings {
                readout: r,
                rate_mhz: mhz,
                tolerance_mhz: 0,
                ..crop(f, w, h, 1.0)
            };
            let secs = if mhz < 10_000 { 4.0 } else { 2.5 };
            if let Some(st) = b.mode(&format!("{name} {what}"), c, secs) {
                let clock = if mhz == f96 { 96 } else { 80 };
                b.check(st.mode.sensor_clock_mhz == clock, || {
                    format!("{name} {what}: clock {} MHz", st.mode.sensor_clock_mhz)
                });
            }
        }
    }
}

/// Refusals.
fn refusals(b: &mut Board) {
    let base = crop(Format::Raw8, 1280, 800, 120.0);
    b.mode("base", base, 1.0);
    let before = b.state();
    let (f80, f96) = camera::fastest_rates(Format::Raw8, 1280, 800, Readout::Crop);
    let _ = f80;
    let refusals = [
        (
            "too fast",
            CameraSettings {
                rate_mhz: f96 + 1000,
                tolerance_mhz: 0,
                ..base
            },
        ),
        (
            "too slow",
            CameraSettings {
                rate_mhz: 100,
                tolerance_mhz: 0,
                ..base
            },
        ),
        (
            "tolerance",
            CameraSettings {
                rate_mhz: 120_001,
                tolerance_mhz: 0,
                ..base
            },
        ),
        (
            "size",
            CameraSettings {
                width: 1288,
                ..base
            },
        ),
        ("size, too small", CameraSettings { width: 120, ..base }),
        (
            "alignment",
            CameraSettings {
                width: 1276,
                ..base
            },
        ),
        (
            "alignment RAW10",
            CameraSettings {
                format: Format::Raw10,
                width: 1248,
                ..base
            },
        ),
        (
            "window",
            CameraSettings {
                width: 640,
                height: 480,
                origin: Some(Origin { x: 700, y: 0 }),
                ..base
            },
        ),
        (
            "exposure above the maximum",
            CameraSettings {
                exposure_lines: 5000,
                ..base
            },
        ),
        (
            "exposure 0",
            CameraSettings {
                exposure_lines: 0,
                ..base
            },
        ),
        (
            "gain",
            CameraSettings {
                gain_sixteenths: 249,
                ..base
            },
        ),
    ];
    for (label, c) in refusals {
        let expect = camera::validate(&c, Encoding::Raw8);
        let got = b.set_camera(c);
        println!(
            "  {label}: {got:?} (driver here: {:?})",
            expect.as_ref().err()
        );
        b.check(
            expect.is_err() && got.as_ref().err() == expect.as_ref().err(),
            || format!("{label}: {got:?}, expected {expect:?}"),
        );
    }
    let raw10 = b.set_stream(stream(Encoding::Raw10, 0));
    println!("  RAW10 from a RAW8 sensor: {raw10:?}");
    b.check(
        raw10
            == Err(CamError::Encoding {
                format: Format::Raw8,
                encoding: Encoding::Raw10,
            }),
        || format!("RAW10 encoding: {raw10:?}"),
    );
    let after = b.state();
    b.check(
        (after.camera, after.stream, after.generation)
            == (before.camera, before.stream, before.generation),
        || "a refusal changed the state".into(),
    );
    let w = b.measure(&after, Duration::ZERO, 2.0);
    b.judge("after the refusals", &after, &w);
}

/// Exposure and gain against brightness (crop 640x480 at 30 Hz).
fn brightness(b: &mut Board) {
    let scene = scene();
    let mut means = Vec::new();
    for (exp, gain) in [(400, 32), (1600, 32), (400, 128)] {
        let st = b
            .set_camera(CameraSettings {
                exposure_lines: exp,
                gain_sixteenths: gain,
                ..scene
            })
            .expect("scene mode");
        let f = frame_after(b, &st, 3);
        let m = mean(&f);
        println!("  exposure {exp} lines, gain {gain}/16: mean {m:.1}");
        means.push(m);
        b.c().recycle(f.data);
    }
    // Black level about 4 (ov9281 README); 4x the exposure or gain gives at least 2x above it,
    // unless the brighter frame saturates.
    let lit = |m: f64| m - 4.0;
    b.check(
        lit(means[1]) > 2.0 * lit(means[0]) || means[1] > 200.0,
        || format!("exposure x4: mean {:.1} -> {:.1}", means[0], means[1]),
    );
    b.check(
        lit(means[2]) > 2.0 * lit(means[0]) || means[2] > 200.0,
        || format!("gain x4: mean {:.1} -> {:.1}", means[0], means[2]),
    );
    let st = b.state();
    b.check(st.generation == b.state().generation, || {
        "generation".into()
    });
}

/// Test patterns (crop 640x480 RAW8).
fn patterns(b: &mut Board) {
    let scene = scene();
    b.set_camera(scene).expect("scene mode");
    for p in [
        TestPattern::Bars,
        TestPattern::RollingBars,
        TestPattern::GreySquares,
        TestPattern::BlackWhiteSquares,
        TestPattern::Random,
        TestPattern::Off,
    ] {
        let before = b.state().generation;
        let st = b
            .set_camera(CameraSettings {
                test_pattern: p,
                ..scene
            })
            .expect("pattern");
        b.check(st.generation == before, || {
            format!("{p:?}: generation changed")
        });
        let f1 = frame_after(b, &st, 3);
        let f2 = frame_after(b, &st, 1);
        let same = f1.data == f2.data;
        let exact = bars_exact(&f1) && bars_exact(&f2);
        println!(
            "  {p:?}: frames {} and {}, mean {:.1}, identical {same}, every even row = row 0 and odd = row 1: {exact}",
            f1.number,
            f2.number,
            mean(&f1)
        );
        match p {
            TestPattern::Bars => {
                b.check(exact, || "bars not exact".into());
                b.check(same, || "bars changed between frames".into());
            }
            TestPattern::Random | TestPattern::GreySquares | TestPattern::BlackWhiteSquares => {
                b.check(same, || format!("{p:?} changed between frames"));
            }
            TestPattern::RollingBars => {
                b.check(!same, || "rolling bars did not roll".into());
            }
            TestPattern::Off => {
                b.check(!exact, || "the scene looks like bars".into());
            }
        }
        b.c().recycle(f1.data);
        b.c().recycle(f2.data);
    }
}

/// RAW10 sent as RAW10 and as RAW8 (bars, crop 1280x800 RAW10).
fn raw10(b: &mut Board) {
    let r10 = CameraSettings {
        test_pattern: TestPattern::Bars,
        ..crop(Format::Raw10, 1280, 800, 120.0)
    };
    let st8 = b.set_camera(r10).expect("RAW10 mode");
    let f8 = frame_after(b, &st8, 2);
    let st10 = b
        .set_stream(stream(Encoding::Raw10, 0))
        .expect("RAW10 stream");
    b.check(st10.generation != st8.generation, || {
        "encoding change kept the generation".into()
    });
    let f10 = frame_after(b, &st10, 2);
    let hb = high_bits(&f10.data);
    println!(
        "  RAW10 frame {} B, RAW8 frame {} B, RAW8 == high bits of RAW10: {}",
        f10.data.len(),
        f8.data.len(),
        hb == f8.data
    );
    b.check(f10.data.len() == 1280 * 800 * 5 / 4, || {
        "RAW10 frame size".into()
    });
    b.check(hb == f8.data, || {
        "RAW8 from RAW10 is not the high bits".into()
    });
    b.check(bars_exact(&f8), || "RAW10 bars as RAW8 not exact".into());
    let w = b.measure(&st10, Duration::ZERO, 3.0);
    b.judge("crop 1280x800 RAW10 sent as RAW10", &st10, &w);
    let st = b
        .set_camera(CameraSettings {
            test_pattern: TestPattern::Off,
            ..binned(Format::Raw10, 640, 400, 200.0)
        })
        .expect("RAW10 binned");
    let w = b.measure(&st, settle(&st), 3.0);
    b.judge("binned 640x400 RAW10 sent as RAW10", &st, &w);
    let st = b
        .set_stream(stream(Encoding::Raw8, 0))
        .expect("RAW8 stream");
    let w = b.measure(&st, settle(&st), 3.0);
    b.judge("binned 640x400 RAW10 sent as RAW8", &st, &w);
}

/// Max_fps, stream off and on (binned 640x400 RAW8 250 Hz).
fn stream_switches(b: &mut Board) {
    b.mode(
        "binned 640x400 RAW8 250 Hz",
        binned(Format::Raw8, 640, 400, 250.0),
        1.0,
    );
    for fps in [5u16, 30] {
        let st = b.set_stream(stream(Encoding::Raw8, fps)).expect("max_fps");
        let w = b.measure(&st, Duration::from_millis(500), 4.0);
        b.judge(&format!("max_fps {fps}"), &st, &w);
    }
    let st = b
        .set_stream(StreamSettings {
            enabled: false,
            ..stream(Encoding::Raw8, 0)
        })
        .expect("off");
    thread::sleep(Duration::from_millis(300));
    let s0 = b.c().stats();
    thread::sleep(Duration::from_secs(1));
    let s1 = b.c().stats();
    println!("  stream off: {} chunks in 1 s", s1.chunks - s0.chunks);
    b.check(s1.chunks == s0.chunks, || {
        "frames while the stream is off".into()
    });
    let _ = st;
    let st = b.set_stream(stream(Encoding::Raw8, 0)).expect("on");
    let w = b.measure(&st, Duration::from_millis(300), 2.0);
    b.judge("stream on again", &st, &w);
}

/// Request round trip and the board's CPU while frames go out at the link rate.
fn rtt(b: &mut Board) {
    for (label, cam, enc) in [
        (
            "crop 1280x800 RAW8",
            crop(Format::Raw8, 1280, 800, 120.0),
            Encoding::Raw8,
        ),
        (
            "crop 1280x800 RAW10 sent as RAW10",
            crop(Format::Raw10, 1280, 800, 120.0),
            Encoding::Raw10,
        ),
        (
            "crop 1280x800 RAW10 sent as RAW8",
            crop(Format::Raw10, 1280, 800, 120.0),
            Encoding::Raw8,
        ),
        (
            "binned 320x200 RAW8 465 Hz",
            binned(Format::Raw8, 320, 200, 464.895),
            Encoding::Raw8,
        ),
        (
            "crop 1280x800 JPEG q90 at 153.98 Hz",
            crop(Format::Raw8, 1280, 800, 153.984),
            Encoding::Jpeg,
        ),
    ] {
        if enc != Encoding::Raw10 {
            b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
        }
        b.set_camera(cam).expect("mode");
        let st = b.set_stream(stream(enc, 0)).expect("encoding");
        thread::sleep(settle(&st));
        b.diag(b"M");
        let s0 = b.c().stats();
        // Round trips of the client's own polls (one every 250 ms) over 10 s.
        let mut rtts = Vec::new();
        let mut seen = s0.rtt_count;
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            let s = b.c().stats();
            if s.rtt_count != seen {
                seen = s.rtt_count;
                rtts.extend(s.rtt);
            }
            if let Some(f) = b.c().take_frame() {
                b.c().recycle(f.data);
            }
            thread::sleep(Duration::from_millis(2));
        }
        let s1 = b.c().stats();
        let r = report(b);
        rtts.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let n = rtts.len();
        let mean = rtts.iter().map(|&d| ms(d)).sum::<f64>() / n.max(1) as f64;
        println!(
            "  {label}: {n} requests answered, round trip mean {mean:.2} ms, median {:.2}, max {:.2}; \
             retries {}; host {:.2} fps, {:.1} Mbit/s, {} incomplete",
            rtts.get(n / 2).map_or(0.0, |&d| ms(d)),
            rtts.last().map_or(0.0, |&d| ms(d)),
            s1.retries - s0.retries,
            (s1.frames_whole - s0.frames_whole) as f64 / 10.0,
            (s1.bytes - s0.bytes) as f64 * 8.0 / 10.0 / 1e6,
            s1.frames_incomplete - s0.frames_incomplete,
        );
        println!("    board: {r}");
        b.check(n >= 30, || {
            format!("{label}: only {n} requests answered in 10 s")
        });
    }
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
}

/// Error paths through the test hooks.
fn faults(b: &mut Board) {
    let other = binned(Format::Raw8, 640, 400, 250.0);
    for (cmd, expect) in [(b"F1", CamError::SensorBus), (b"F2", CamError::Receiver)] {
        let good = b.state();
        let armed = b.diag(cmd);
        let t = Instant::now();
        let got = b.set_camera(other);
        println!(
            "  {} ({armed}): answer {got:?} after {:.0} ms",
            String::from_utf8_lossy(cmd),
            t.elapsed().as_secs_f64() * 1e3
        );
        b.check(got == Err(expect), || format!("fault {cmd:?}: {got:?}"));
        let now = b.state();
        b.check(
            (now.camera, now.generation) == (good.camera, good.generation),
            || "a failed mode change changed the state".into(),
        );
        let w = b.measure(&now, Duration::from_millis(500), 2.0);
        b.judge("the previous mode after the failure", &now, &w);
        let ok = b.set_camera(other);
        b.check(ok.is_ok(), || format!("the next mode change: {ok:?}"));
        b.set_camera(good.camera).expect("back");
    }
    let before = b.state();
    let stall = b.diag(b"S");
    let t = Instant::now();
    thread::sleep(Duration::from_millis(100));
    let s0 = b.c().stats().frames_whole;
    let resumed = b
        .c()
        .wait_until(Duration::from_secs(5), |s| s.frames_whole > s0 + 2)
        .is_some();
    let after = b.state();
    println!(
        "  stall ({stall}): frames again after {:.0} ms: {resumed}; capture errors +{}",
        t.elapsed().as_secs_f64() * 1e3,
        after.counters.capture_errors - before.counters.capture_errors
    );
    b.check(resumed, || "no frames after the stall".into());
    b.check(
        after.counters.capture_errors > before.counters.capture_errors,
        || "the stall was not counted".into(),
    );
    let w = b.measure(&after, Duration::from_millis(300), 2.0);
    b.judge("after the stall", &after, &w);
}

/// Malformed datagrams.
fn garbage(b: &mut Board) {
    let r0 = report(b);
    send_garbage(b.camera, 2000);
    thread::sleep(Duration::from_millis(300));
    let r1 = report(b);
    let bad = field(&r1, "bad_requests").unwrap_or(0.0) - field(&r0, "bad_requests").unwrap_or(0.0);
    println!("  2000 sent, the board counted {bad}");
    b.check(bad > 0.0, || "no malformed datagram counted".into());
    let st = b.state();
    let w = b.measure(&st, Duration::from_millis(300), 2.0);
    b.judge("after the malformed datagrams", &st, &w);
}

/// The stream timeout (one request, then silence).
fn timeout(b: &mut Board) {
    b.mode(
        "binned 640x400 RAW8 250 Hz",
        binned(Format::Raw8, 640, 400, 250.0),
        1.0,
    );
    b.client = None;
    thread::sleep(Duration::from_millis(3500));
    let (answered, chunks, stopped) = one_request(b.camera);
    println!("  answered {answered}, {chunks} chunks, the last {stopped} ms after the request");
    b.check(answered && chunks > 100, || {
        "no stream after one request".into()
    });
    b.check(
        (STREAM_TIMEOUT_MS - 150..=STREAM_TIMEOUT_MS + 150).contains(&stopped),
        || format!("stopped after {stopped} ms"),
    );
    b.reconnect();
    let st = b.state();
    let w = b.measure(&st, Duration::from_millis(500), 2.0);
    b.judge("resumed after the timeout", &st, &w);
}

/// JPEG at quality 90, automatic quality off: windows at 120 Hz and at their fastest rate.
fn jpeg_modes(b: &mut Board) {
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
    b.set_camera(crop(Format::Raw8, 1280, 800, 120.0))
        .expect("1280x800");
    b.set_stream(jstream(90, false)).expect("JPEG stream");
    let fastest = |f, w, h, r| camera::fastest_rates(f, w, h, r).1 as f64 / 1000.0;
    let modes = [
        ("crop 1280x800 120 Hz", crop(Format::Raw8, 1280, 800, 120.0)),
        (
            "crop 1280x800 at the sensor's maximum",
            crop(
                Format::Raw8,
                1280,
                800,
                fastest(Format::Raw8, 1280, 800, Readout::Crop),
            ),
        ),
        (
            "crop 1280x720 at its maximum",
            crop(
                Format::Raw8,
                1280,
                720,
                fastest(Format::Raw8, 1280, 720, Readout::Crop),
            ),
        ),
        (
            "crop 640x480 at its maximum",
            crop(
                Format::Raw8,
                640,
                480,
                fastest(Format::Raw8, 640, 480, Readout::Crop),
            ),
        ),
        (
            "binned 640x400 at its maximum",
            binned(
                Format::Raw8,
                640,
                400,
                fastest(Format::Raw8, 640, 400, Readout::Binned),
            ),
        ),
        (
            "binned 320x200 at its maximum",
            binned(
                Format::Raw8,
                320,
                200,
                fastest(Format::Raw8, 320, 200, Readout::Binned),
            ),
        ),
        ("crop 128x8 1000 Hz", crop(Format::Raw8, 128, 8, 1000.0)),
        (
            "crop 1216x704 at (32,48)",
            CameraSettings {
                origin: Some(Origin { x: 32, y: 48 }),
                ..crop(Format::Raw8, 1216, 704, 100.0)
            },
        ),
    ];
    for (label, m) in modes {
        let m = CameraSettings {
            tolerance_mhz: 0,
            ..m
        };
        if let Some(st) = b.mode(&format!("JPEG {label}"), m, 4.0) {
            let w = b.measure(&st, Duration::ZERO, 2.0);
            let sensor = st.mode.frame_rate_mhz as f64 / 1000.0;
            println!(
                "    host {:.2} fps against the sensor's {sensor:.3} Hz ({:.1} %), encoder skipped {} of {} captured",
                w.fps,
                100.0 * w.fps / sensor,
                w.encoder.frames_skipped,
                w.board.frames_captured,
            );
            b.check(
                w.encoder.frames_skipped as f64 <= 0.01 * w.board.frames_captured as f64,
                || {
                    format!(
                        "JPEG {label}: the encoder skipped {} of {} frames",
                        w.encoder.frames_skipped, w.board.frames_captured
                    )
                },
            );
        }
    }
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
}

/// The scene with content: crop 1280x800 RAW8 at 120 Hz, gain 4x.
fn lit_scene() -> CameraSettings {
    CameraSettings {
        gain_sixteenths: 64,
        exposure_lines: 400,
        ..crop(Format::Raw8, 1280, 800, 120.0)
    }
}

/// Quality settings applied and reported; higher quality, larger files; out of range refused.
fn quality(b: &mut Board) {
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
    b.set_camera(lit_scene()).expect("scene");
    let mut sizes = Vec::new();
    for q in [20u8, 50, 75, 90, 100] {
        let st = b.set_stream(jstream(q, false)).expect("quality");
        b.check(
            st.stream.jpeg_quality == q && st.encoder.quality == q,
            || {
                format!(
                    "quality {q}: state says set {} in use {}",
                    st.stream.jpeg_quality, st.encoder.quality
                )
            },
        );
        let w = b.measure(&st, Duration::from_millis(300), 2.0);
        b.judge(&format!("quality {q}"), &st, &w);
        b.check(w.encoder.quality == q && w.encoder.frame_len > 0, || {
            format!(
                "quality {q}: encoder reports quality {} last frame {} B",
                w.encoder.quality, w.encoder.frame_len
            )
        });
        sizes.push((q, w.mean_len));
    }
    println!("  mean frame size by quality: {sizes:?}");
    b.check(sizes.windows(2).all(|p| p[1].1 > p[0].1), || {
        format!("sizes do not grow with the quality: {sizes:?}")
    });
    let before = b.state();
    for q in [0u8, 101, u8::MAX] {
        let got = b.set_stream(jstream(q, false));
        println!("  quality {q}: {got:?}");
        b.check(
            got == Err(CamError::Quality {
                min: JPEG_QUALITY_MIN,
                max: JPEG_QUALITY_MAX,
            }),
            || format!("quality {q}: {got:?}"),
        );
    }
    let after = b.state();
    b.check(
        (after.stream, after.generation, after.encoder.quality)
            == (before.stream, before.generation, before.encoder.quality),
        || "a refused quality changed the state".into(),
    );
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
}

/// The quality in use every 200 ms for `secs`, frames taken and dropped meanwhile.
fn quality_trace(b: &Board, secs: f64) -> Vec<u8> {
    let mut trace = Vec::new();
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        let t = Instant::now() + Duration::from_millis(200);
        while Instant::now() < t {
            if let Some(f) = b.c().take_frame() {
                b.c().recycle(f.data);
            }
            thread::sleep(Duration::from_millis(2));
        }
        trace.push(b.state().encoder.quality);
    }
    trace
}

/// Automatic quality: down when the link is full, back up to the set quality when there is
/// room, nothing when off.
fn auto_quality(b: &mut Board) {
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
    let fastest = camera::fastest_rates(Format::Raw8, 1280, 800, Readout::Crop).1;
    let heavy = [
        (
            "the scene at gain 15.5x, longest exposure",
            CameraSettings {
                rate_mhz: fastest,
                tolerance_mhz: 0,
                gain_sixteenths: 248,
                exposure_lines: 1,
                ..crop(Format::Raw8, 1280, 800, 1.0)
            },
        ),
        (
            "the random test pattern",
            CameraSettings {
                rate_mhz: fastest,
                tolerance_mhz: 0,
                test_pattern: TestPattern::Random,
                ..crop(Format::Raw8, 1280, 800, 1.0)
            },
        ),
    ];
    // A load that fills the link at quality 100 without automatic quality.
    let mut loaded = None;
    for (label, mut c) in heavy {
        if let Ok(m) = camera::build_mode(&c) {
            c.exposure_lines = c.exposure_lines.max(m.max_exposure_lines());
        }
        b.set_camera(c).expect("heavy mode");
        let st = b.set_stream(jstream(100, false)).expect("JPEG 100");
        let w = b.measure(&st, Duration::from_millis(500), 3.0);
        b.judge(&format!("auto off, {label}"), &st, &w);
        b.check(w.encoder.quality == 100, || {
            format!("auto off: quality in use {}", w.encoder.quality)
        });
        if w.board.frames_skipped > 0 {
            loaded = Some((label, st, w));
            break;
        }
        println!("  {label}: the link is not full ({:.1} Mbit/s)", w.mbps);
    }
    let Some((label, off, w_off)) = loaded else {
        b.check(false, || "no scene filled the link at quality 100".into());
        b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
        return;
    };
    let st = b.set_stream(jstream(100, true)).expect("auto on");
    b.check(st.encoder.quality == 100, || {
        format!("auto on starts at {}", st.encoder.quality)
    });
    let trace = quality_trace(b, 6.0);
    println!("  auto on, {label}: quality every 200 ms {trace:?}");
    let w_on = b.measure(&st, Duration::ZERO, 3.0);
    b.judge(&format!("auto on, {label}"), &st, &w_on);
    let skip_rate =
        |w: &Window| w.board.frames_skipped as f64 / w.board.frames_captured.max(1) as f64;
    println!(
        "  off: {:.2} fps, {:.0} B, {:.0} % skipped; on: {:.2} fps, {:.0} B, {:.0} % skipped, quality {}",
        w_off.fps,
        w_off.mean_len,
        100.0 * skip_rate(&w_off),
        w_on.fps,
        w_on.mean_len,
        100.0 * skip_rate(&w_on),
        w_on.encoder.quality
    );
    b.check(
        w_on.encoder.quality < 100 && w_on.encoder.quality >= AUTO_QUALITY_MIN,
        || format!("auto on: quality in use {}", w_on.encoder.quality),
    );
    b.check(
        w_on.fps > w_off.fps && w_on.mean_len < w_off.mean_len,
        || {
            format!(
                "auto on did not help: {:.2} fps {:.0} B against {:.2} fps {:.0} B",
                w_on.fps, w_on.mean_len, w_off.fps, w_off.mean_len
            )
        },
    );
    // Settled: the last 2 s of the trace stay within 10 steps.
    let tail = &trace[trace.len().saturating_sub(10)..];
    let (lo, hi) = (tail.iter().min(), tail.iter().max());
    b.check(lo.zip(hi).is_some_and(|(l, h)| h - l <= 10), || {
        format!("auto quality does not settle: {tail:?}")
    });
    // Room: the scene at gain 2x is small (the sensor's bars are not: hundreds of edges per
    // row); back to the set quality.
    let light = CameraSettings {
        test_pattern: TestPattern::Off,
        gain_sixteenths: 32,
        ..off.camera
    };
    let st = b.set_camera(light).expect("scene at gain 2x");
    let need = f64::from(100 - w_on.encoder.quality.min(100)) * 0.2 + 3.0;
    let trace = quality_trace(b, need);
    println!("  auto on, scene at gain 2x: quality every 200 ms {trace:?}");
    b.check(trace.last() == Some(&100), || {
        format!(
            "auto on with room: quality {:?} after {need:.1} s",
            trace.last()
        )
    });
    let w = b.measure(&st, Duration::ZERO, 2.0);
    b.judge("auto on, scene at gain 2x", &st, &w);
    // Off again: the set quality at once.
    let st = b.set_stream(jstream(60, false)).expect("auto off");
    b.check(st.encoder.quality == 60, || {
        format!("auto off: quality in use {}", st.encoder.quality)
    });
    b.set_camera(CameraSettings {
        test_pattern: TestPattern::Off,
        ..off.camera
    })
    .expect("scene");
    let trace = quality_trace(b, 2.0);
    println!("  auto off, heavy scene, quality 60: {trace:?}");
    b.check(trace.iter().all(|&q| q == 60), || {
        format!("auto off: the quality moved {trace:?}")
    });
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
}

/// Refusals: JPEG with a RAW10 mode or a size not a multiple of 8, from either side.
fn jpeg_refusals(b: &mut Board) {
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
    let base = crop(Format::Raw8, 1280, 800, 120.0);
    b.set_camera(base).expect("base");
    b.set_stream(jstream(90, false)).expect("JPEG");
    let before = b.state();
    let jpeg_err = |reason, c: &CameraSettings| CamError::Jpeg {
        reason,
        format: c.format,
        width: c.width,
        height: c.height,
    };
    let modes = [
        (
            "RAW10 mode with JPEG",
            crop(Format::Raw10, 1280, 800, 120.0),
            JpegRefusal::Format,
        ),
        (
            "RAW10 binned mode with JPEG",
            binned(Format::Raw10, 640, 400, 200.0),
            JpegRefusal::Format,
        ),
        (
            "1280x796 with JPEG",
            crop(Format::Raw8, 1280, 796, 120.0),
            JpegRefusal::Size,
        ),
        (
            "640x476 with JPEG",
            crop(Format::Raw8, 640, 476, 120.0),
            JpegRefusal::Size,
        ),
        (
            "binned 320x196 with JPEG",
            binned(Format::Raw8, 320, 196, 200.0),
            JpegRefusal::Size,
        ),
    ];
    for (label, c, reason) in modes {
        let expect = jpeg_err(reason, &c);
        let here = camera::validate(&c, Encoding::Jpeg);
        let got = b.set_camera(c);
        println!("  {label}: {got:?}");
        b.check(got == Err(expect) && here.err() == Some(expect), || {
            format!("{label}: {got:?}, expected {expect:?}")
        });
    }
    let after = b.state();
    b.check(
        (after.camera, after.stream, after.generation)
            == (before.camera, before.stream, before.generation),
        || "a JPEG refusal changed the state".into(),
    );
    let w = b.measure(&after, Duration::ZERO, 2.0);
    b.judge("JPEG after the refusals", &after, &w);
    // From the stream's side: the mode first, then JPEG asked for.
    for (label, c, reason) in [
        (
            "JPEG asked in a RAW10 mode",
            crop(Format::Raw10, 1280, 800, 120.0),
            JpegRefusal::Format,
        ),
        (
            "JPEG asked in a 1280x796 mode",
            crop(Format::Raw8, 1280, 796, 120.0),
            JpegRefusal::Size,
        ),
    ] {
        b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
        b.set_camera(c).expect("raw mode");
        let before = b.state();
        let got = b.set_stream(jstream(90, false));
        let expect = jpeg_err(reason, &c);
        println!("  {label}: {got:?}");
        b.check(got == Err(expect), || {
            format!("{label}: {got:?}, expected {expect:?}")
        });
        let after = b.state();
        b.check(
            (after.stream, after.generation) == (before.stream, before.generation),
            || format!("{label}: the refusal changed the stream"),
        );
    }
    b.set_camera(base).expect("base");
}

/// JPEG to RAW8 and back, to RAW10 (raw first, then the mode) and back.
fn switching(b: &mut Board) {
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
    let m8 = binned(Format::Raw8, 640, 400, 250.0);
    let m10 = binned(Format::Raw10, 640, 400, 250.0);
    b.set_camera(m8).expect("RAW8 mode");
    let mut last = b.state().generation;
    let steps: [(&str, Option<CameraSettings>, StreamSettings); 6] = [
        ("RAW8 -> JPEG", None, jstream(90, false)),
        ("JPEG -> RAW8", None, stream(Encoding::Raw8, 0)),
        ("RAW8 -> JPEG again", None, jstream(75, true)),
        ("JPEG -> RAW8 before RAW10", None, stream(Encoding::Raw8, 0)),
        (
            "RAW10 mode, RAW10 stream",
            Some(m10),
            stream(Encoding::Raw10, 0),
        ),
        ("RAW8 mode, then JPEG", Some(m8), jstream(90, false)),
    ];
    for (label, mode, s) in steps {
        if let Some(m) = mode {
            if m.format == Format::Raw8 {
                b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8 first");
            }
            b.set_camera(m).expect("mode");
        }
        let st = b.set_stream(s).expect("encoding");
        b.check(st.generation != last, || {
            format!("{label}: generation kept")
        });
        last = st.generation;
        let w = b.measure(&st, settle(&st), 2.0);
        b.judge(label, &st, &w);
    }
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
}

/// PSNR of `a` against `b` (8-bit), dB.
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
        .sum::<f64>()
        / a.len().max(1) as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// A JPEG frame decoded against a raw frame of the same static scene taken right after.
fn agreement(b: &mut Board) {
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
    for (label, c, bars) in [
        (
            "bars",
            CameraSettings {
                test_pattern: TestPattern::Bars,
                ..crop(Format::Raw8, 1280, 800, 120.0)
            },
            true,
        ),
        ("scene", lit_scene(), false),
    ] {
        b.set_camera(c).expect("mode");
        let st = b.set_stream(jstream(90, false)).expect("JPEG");
        let jf = frame_after(b, &st, 3);
        let mut decoded = Vec::new();
        if let Err(e) = jpeg::decode(&jf.data, jf.width, jf.height, &mut decoded) {
            b.check(false, || {
                format!("{label}: frame {} does not decode: {e}", jf.number)
            });
            continue;
        }
        let st = b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
        let rf = frame_after(b, &st, 1);
        let p = psnr(&decoded, &rf.data);
        let worst = decoded
            .iter()
            .zip(&rf.data)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        let mean_d = |f: &[u8]| f.iter().map(|&v| f64::from(v)).sum::<f64>() / f.len() as f64;
        println!(
            "  {label}: JPEG frame {} ({} B) against RAW8 frame {}: PSNR {p:.2} dB, largest error {worst}, means {:.2} / {:.2}",
            jf.number,
            jf.data.len(),
            rf.number,
            mean_d(&decoded),
            mean_d(&rf.data)
        );
        if bars {
            // Within the codec's own error: the same raw frame encoded here at the same quality
            // (`jpeg-encoder`, the IJG tables the board uses) and decoded the same way.
            let mut file = Vec::new();
            jpeg_encoder::Encoder::new(&mut file, 90)
                .encode(&rf.data, rf.width, rf.height, jpeg_encoder::ColorType::Luma)
                .expect("reference encode");
            let mut reference = Vec::new();
            jpeg::decode(&file, rf.width, rf.height, &mut reference).expect("reference decode");
            let p_ref = psnr(&reference, &rf.data);
            let worst_ref = reference
                .iter()
                .zip(&rf.data)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0);
            println!(
                "    reference encoder here at quality 90: {} B, PSNR {p_ref:.2} dB, largest error {worst_ref}",
                file.len()
            );
            b.check(bars_exact(&rf), || "raw bars not exact".into());
            b.check(p >= 40.0 && p >= p_ref - 1.0, || {
                format!("{label}: PSNR {p:.2} dB, the reference {p_ref:.2} dB")
            });
            b.check(worst <= worst_ref + 3, || {
                format!("{label}: largest error {worst}, the reference {worst_ref}")
            });
        } else {
            b.check(p >= 30.0, || format!("{label}: PSNR {p:.2} dB"));
        }
        b.c().recycle(jf.data);
        b.c().recycle(rf.data);
    }
}

fn fake_default() -> CameraSettings {
    poc_viewer::fake::default_camera()
}

/// `n` malformed datagrams to the camera port: wrong magic, wrong version, truncated, noise.
fn send_garbage(camera: SocketAddr, n: usize) {
    let s = UdpSocket::bind(addr::local_for(camera).unwrap()).unwrap();
    for i in 0..n {
        let d: &[u8] = match i % 4 {
            0 => b"XX\x02\x20\0\0\0\0\0\0",
            1 => b"ES\x09\x20\0\0\0\0\0\0",
            2 => b"ES",
            _ => &[
                0x45, 0x53, 0x02, 0x20, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            ],
        };
        let _ = s.send_to(d, camera);
    }
}

/// One `GetState` from a fresh socket, then nothing: whether it was answered, the chunks that
/// came, and when the last one came (ms after the request).
fn one_request(camera: SocketAddr) -> (bool, u32, u32) {
    let sock = UdpSocket::bind(addr::local_for(camera).unwrap()).unwrap();
    sock.connect(camera).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut req = [0u8; MAX_DATAGRAM];
    let d = Request {
        seq: 9281,
        op: Op::GetState,
    }
    .encode(&mut req)
    .unwrap();
    let start = Instant::now();
    sock.send(d).unwrap();
    let mut buf = [0u8; MAX_DATAGRAM];
    let (mut answered, mut chunks, mut last) = (false, 0, start);
    while let Ok(n) = sock.recv(&mut buf) {
        match Message::decode(&buf[..n]) {
            Ok(Message::Response(r)) => answered |= r.seq == 9281,
            Ok(Message::Chunk(_)) => {
                chunks += 1;
                last = Instant::now();
            }
            _ => {}
        }
    }
    (answered, chunks, (last - start).as_millis() as u32)
}

/// Client statistics line for the soak.
fn line(s: &Stats) -> String {
    format!(
        "whole {} incomplete {} replaced {} stale {} undecodable {} retries {}",
        s.frames_whole,
        s.frames_incomplete,
        s.frames_replaced,
        s.stale_chunks,
        s.undecodable,
        s.retries
    )
}

#[test]
#[ignore = "needs the board on eth0 running poc-p4-stream; 10 minutes"]
fn soak() {
    let secs: u64 = std::env::var("POC_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let mut b = Board::connect();
    b.set_stream(stream(Encoding::Raw8, 0)).expect("RAW8");
    let modes = [
        binned(Format::Raw8, 640, 400, 250.0),
        crop(Format::Raw8, 1280, 800, 120.0),
        binned(Format::Raw8, 640, 400, 200.0),
        crop(Format::Raw8, 640, 480, 200.0),
        binned(Format::Raw8, 320, 200, 400.0),
        binned(Format::Raw10, 640, 400, 250.0),
    ];
    let start = Instant::now();
    let first = b.state();
    b.diag(b"M");
    let mut step = 0usize;
    let mut total_whole = 0u64;
    while start.elapsed() < Duration::from_secs(secs) {
        // Every 20 s: a mode change; every third step the client is dropped and started again
        // (a viewer that goes away and comes back), every fourth a burst of bad datagrams.
        let m = modes[step % modes.len()];
        let st = b.set_camera(m).expect("soak mode");
        if step % 3 == 2 {
            b.client = None;
            thread::sleep(Duration::from_millis(3500));
            b.reconnect();
        }
        if step % 4 == 3 {
            send_garbage(b.camera, 1000);
        }
        let w = b.measure(&b.state(), Duration::from_millis(500), 18.5);
        total_whole += w.whole;
        b.judge(
            &format!("{:>4} s step {step}", start.elapsed().as_secs()),
            &st,
            &w,
        );
        step += 1;
    }
    let last = b.state();
    let r = report(&b);
    let d = |a: u32, z: u32| z.wrapping_sub(a);
    let (a, z) = (first.counters, last.counters);
    println!(
        "soak: {} s, {step} mode changes, {total_whole} whole frames at the host; board captured {} \
         sent {} skipped {} capture errors {} datagrams {}; client {}",
        start.elapsed().as_secs(),
        d(a.frames_captured, z.frames_captured),
        d(a.frames_sent, z.frames_sent),
        d(a.frames_skipped, z.frames_skipped),
        d(a.capture_errors, z.capture_errors),
        d(a.datagrams_sent, z.datagrams_sent),
        line(&b.c().stats()),
    );
    println!("board report: {r}");
    // Counters consistent: nothing sent or skipped that was not captured, and the board's
    // sent frames cover what the host received whole (frames sent to an earlier client too).
    let (cap, sent, skipped) = (
        d(a.frames_captured, z.frames_captured),
        d(a.frames_sent, z.frames_sent),
        d(a.frames_skipped, z.frames_skipped),
    );
    b.check(sent + skipped <= cap, || {
        format!("sent {sent} + skipped {skipped} > captured {cap}")
    });
    b.check(u64::from(sent) >= total_whole, || {
        format!("host got {total_whole} whole frames, board sent {sent}")
    });
    b.check(last.uptime_us > first.uptime_us, || {
        "uptime went back".into()
    });
    assert!(b.failures.is_empty(), "failed checks: {:#?}", b.failures);
}
