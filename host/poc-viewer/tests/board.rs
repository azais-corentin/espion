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
//! `soak` streams 640x400 binned at the link rate and changes the mode every 20 s, restarts the
//! client and sends malformed datagrams in between, for `POC_SOAK_SECS` (default 600).

use std::{
    fmt::Write as _,
    net::{SocketAddr, UdpSocket},
    thread,
    time::{Duration, Instant},
};

use espion_proto::cam::{
    CHUNK_MAX, CamError, CameraSettings, Counters, Encoding, Format, FrameChunk, MAX_DATAGRAM,
    Message, Op, Origin, Readout, Reply, Request, STREAM_TIMEOUT_MS, State, StreamSettings,
    TestPattern, chunk_count,
};
use poc_viewer::{
    addr, camera,
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
        while Instant::now() < end {
            let Some(f) = c.take_frame() else {
                thread::sleep(Duration::from_micros(500));
                continue;
            };
            w.taken += 1;
            if f.generation != state.generation
                || (f.width, f.height, f.encoding)
                    != (
                        state.camera.width,
                        state.camera.height,
                        state.stream.encoding,
                    )
                || f.data.len() as u64 != len
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
        w.whole = s1.frames_whole - s0.frames_whole;
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
        let (lo, hi) = expected_fps(st);
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

/// The host frame rate to expect, a range (RAW10 sent as RAW8: CPU-bound, see below). Low end: the board sends the frame captured after
/// the previous one is out, so every n-th frame where n = ceil(wire time x sensor rate) (with 3 %
/// margin on the wire time). High end: the sensor rate, or the link's frames per second, since
/// the board's socket and DMA rings let the next frame start before the previous one has left.
/// Both capped by `max_fps`.
fn expected_fps(st: &State) -> (f64, f64) {
    let f = st.mode.frame_rate_mhz as f64 / 1000.0;
    let len = st
        .stream
        .encoding
        .frame_bytes(st.camera.width, st.camera.height) as u32;
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
    let mut lo = f / (t * f * 1.03).ceil().max(1.0);
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

    let sections: [(&str, Section); 11] = [
        ("sizes", sizes),
        ("rates", rates),
        ("refusals", refusals),
        ("brightness", brightness),
        ("patterns", patterns),
        ("raw10", raw10),
        ("stream", stream_switches),
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

    b.set_camera(fake_default()).expect("default mode");
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
    ] {
        if enc == Encoding::Raw8 {
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
