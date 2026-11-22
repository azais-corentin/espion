//! The screen target: a page the viewer serves to a screen held in front of the camera (an
//! iPad), which draws simulated markers. The viewer calibrates where the screen lands on the
//! sensor ([`calib`]) and checks the board's blobs against what the page drew ([`check`]).
//!
//! - [`link`]: the messages between the viewer and the page.
//! - [`model`]: screen to sensor (homography and radial distortion).
//! - [`server`]: the page connections ([`server::Hub`]) and the HTTP server (axum).
//! - [`calib`], [`check`]: the calibration and the check.
//! - [`sim`]: a simulated screen the fake camera films, and a page for the tests.
//!
//! [`Target`] holds them together without a window: the app (and the tests) feed it the
//! page's events, the frames and the blob frames, and send the camera commands it returns.
//! Times are µs since the target's epoch on this PC's clock; the page's clock and the board's
//! are mapped to it, each by its fastest sample (as `timing.rs` does for the board).

pub mod calib;
pub mod check;
pub mod link;
pub mod model;
pub mod server;
pub mod sim;

use std::{
    collections::VecDeque,
    io,
    net::SocketAddr,
    sync::mpsc,
    time::{Duration, Instant},
};

use espion_proto::cam::{CameraSettings, DetectSettings, Encoding, Format, State};

use crate::{
    blobs::px,
    net::{BlobFrame, Command, Frame},
};
use calib::{Action, Calibration, Fit, Poll};
use check::{Check, Seen};
use link::{Content, FromPage, Scene, ToPage};
use server::{Hub, PageEvent, PageEventKind, Server};

/// The page is pinged this often.
pub const PING_INTERVAL: Duration = Duration::from_millis(250);
/// The dots' detection during the calibration: threshold (8-bit; RAW10 four times it) and
/// smallest area. Low, for the dots at the edges that a lens's vignetting dims; the screen's
/// black stays far below it at the exposure the dots set.
const DOT_THRESHOLD: u16 = 32;
const DOT_MIN_AREA: u16 = 4;
/// Clock samples are kept this long, µs.
const CLOCK_SPAN_US: f64 = 20e6;

/// The page connected now.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub id: u64,
    /// Its viewport (CSS pixels), device pixels per CSS pixel and user agent.
    pub viewport: Option<(f64, f64, f64)>,
    pub agent: String,
    /// Its frame rate, Hz.
    pub hz: Option<f64>,
    /// The fastest clock round trip of the last seconds, µs.
    pub rtt_us: Option<f64>,
    pub messages: u64,
    pub garbled: u64,
    pub last_garbled: Option<String>,
}

/// Offset samples between a clock and this PC's: the fastest wins.
#[derive(Debug, Default)]
struct Clock {
    /// (taken at, round trip or bound, offset), µs.
    samples: VecDeque<(f64, f64, f64)>,
}

impl Clock {
    fn add(&mut self, at: f64, cost: f64, offset: f64) {
        self.samples.push_back((at, cost, offset));
        while self
            .samples
            .front()
            .is_some_and(|s| s.0 < at - CLOCK_SPAN_US)
        {
            self.samples.pop_front();
        }
    }

    fn best(&self) -> Option<(f64, f64)> {
        self.samples
            .iter()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|s| (s.1, s.2))
    }

    fn clear(&mut self) {
        self.samples.clear();
    }
}

/// What the check is running.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Running {
    pub scene: Scene,
    /// The show that started it, and whether the page drew it yet.
    id: u32,
    shown: bool,
}

/// The sensor's timing in the board's mode: exposure and readout, frame period, µs.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SensorTiming {
    exposure: f64,
    readout: f64,
    period: f64,
}

impl SensorTiming {
    fn of(st: &State) -> Self {
        let line = st.mode.line_ns as f64 / 1e3;
        Self {
            exposure: f64::from(st.camera.exposure_lines) * line,
            readout: f64::from(st.camera.height) * line,
            period: 1e9 / st.mode.frame_rate_mhz.max(1) as f64,
        }
    }
}

/// The screen target's session.
pub struct Target {
    epoch: Instant,
    hub: Hub,
    events: mpsc::Receiver<PageEvent>,
    server: Option<Server>,
    pub server_error: Option<String>,
    page: Option<Page>,
    page_clock: Clock,
    board_clock: Clock,
    next_ping: Instant,
    next_id: u32,
    sensor: Option<SensorTiming>,
    /// The board's camera settings in its last state.
    camera: Option<CameraSettings>,
    /// The longest exposure of the board's mode, lines.
    max_exposure: Option<u16>,
    /// The board's detection settings in its last state, and those before the calibration.
    detect: Option<DetectSettings>,
    saved_detect: Option<DetectSettings>,
    calib: Option<Calibration>,
    pub calib_error: Option<String>,
    fit: Option<Fit>,
    /// The camera settings the fit holds for.
    fit_camera: Option<CameraSettings>,
    running: Option<Running>,
    check: Option<Check>,
    commands: Vec<Command>,
}

/// The settings that place the sensor window: a change makes a calibration invalid.
fn window(c: &CameraSettings) -> impl PartialEq {
    (c.width, c.height, c.readout, c.origin)
}

impl Target {
    /// A target; `wake` is called whenever the page sent something.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (hub, events) = Hub::new(wake);
        let now = Instant::now();
        Self {
            epoch: now,
            hub,
            events,
            server: None,
            server_error: None,
            page: None,
            page_clock: Clock::default(),
            board_clock: Clock::default(),
            next_ping: now,
            next_id: 1,
            sensor: None,
            camera: None,
            max_exposure: None,
            detect: None,
            saved_detect: None,
            calib: None,
            calib_error: None,
            fit: None,
            fit_camera: None,
            running: None,
            check: None,
            commands: Vec::new(),
        }
    }

    pub fn hub(&self) -> &Hub {
        &self.hub
    }

    /// `t` on the target's clock, µs.
    pub fn us(&self, t: Instant) -> f64 {
        t.saturating_duration_since(self.epoch).as_secs_f64() * 1e6
            - self.epoch.saturating_duration_since(t).as_secs_f64() * 1e6
    }

    /// Starts serving the page on `listen`.
    pub fn serve(&mut self, listen: SocketAddr) -> io::Result<SocketAddr> {
        self.server = None;
        match Server::start(listen, self.hub.clone()) {
            Ok(s) => {
                let a = s.addr();
                self.server = Some(s);
                self.server_error = None;
                Ok(a)
            }
            Err(e) => {
                self.server_error = Some(format!("{listen}: {e}"));
                Err(e)
            }
        }
    }

    /// Stops serving (a page connected stays until it closes).
    pub fn stop_serving(&mut self) {
        self.server = None;
    }

    /// Where the page is served.
    pub fn serving(&self) -> Option<SocketAddr> {
        self.server.as_ref().map(Server::addr)
    }

    pub fn page(&self) -> Option<&Page> {
        self.page.as_ref()
    }

    pub fn fit(&self) -> Option<&Fit> {
        self.fit.as_ref()
    }

    /// The fit no longer holds: the page's viewport or the sensor window changed since.
    pub fn fit_stale(&self) -> Option<&'static str> {
        let f = self.fit.as_ref()?;
        if let Some((w, h, _)) = self.page.as_ref().and_then(|p| p.viewport)
            && (w, h) != f.page
        {
            return Some("the page's viewport changed since the calibration");
        }
        if let (Some(a), Some(b)) = (&self.fit_camera, &self.camera)
            && window(a) != window(b)
        {
            return Some("the sensor window changed since the calibration");
        }
        None
    }

    pub fn calibrating(&self) -> Option<(usize, usize, String)> {
        self.calib.as_ref().map(Calibration::progress)
    }

    pub fn running(&self) -> Option<Scene> {
        self.running.map(|r| r.scene)
    }

    pub fn check(&self) -> Option<&Check> {
        self.check.as_ref()
    }

    fn show(&mut self, content: Content) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.hub.send(ToPage::Show { id, content });
        id
    }

    /// Starts a calibration; why it cannot start otherwise.
    pub fn calibrate(&mut self) -> Result<(), String> {
        let page = self.page.as_ref().ok_or("no page is connected")?;
        let (w, h, _) = page.viewport.ok_or("the page has not said its size yet")?;
        let st = self.camera.ok_or("the board has not answered yet")?;
        self.stop_scene();
        self.calib_error = None;
        self.calib = Some(Calibration::new((st.width, st.height), (w, h)));
        // The settings the fit holds for, and the calibration puts back.
        self.fit_camera = Some(st);
        self.saved_detect = self.detect;
        Ok(())
    }

    pub fn cancel_calibration(&mut self) {
        if let Some(mut c) = self.calib.take() {
            // Its last actions (the camera as it was, the page dark) still go out.
            c.cancel();
            self.drive(&mut c);
        }
    }

    /// Starts `scene` on the page with the fit; the check starts over.
    pub fn start_scene(&mut self, scene: Scene) -> Result<(), String> {
        let fit = self.fit.clone().ok_or("calibrate first")?;
        if self.calib.is_some() {
            return Err("a calibration is running".into());
        }
        let id = self.show(Content::Scene {
            scene,
            model: fit.model,
            sensor: [fit.sensor.0, fit.sensor.1],
        });
        self.running = Some(Running {
            scene,
            id,
            shown: false,
        });
        self.check = Some(Check::new(fit.model, fit.sensor));
        Ok(())
    }

    /// Stops the scene (the page goes dark); the check keeps its last figures.
    pub fn stop_scene(&mut self) {
        if self.running.take().is_some() {
            self.show(Content::Black);
        }
    }

    /// Runs the calibration's actions until it waits.
    fn drive(&mut self, c: &mut Calibration) -> Option<Result<Fit, String>> {
        let now = self.us(Instant::now());
        loop {
            match c.poll(now) {
                Poll::Wait => return None,
                Poll::Done(r) => {
                    // Its last actions first.
                    while let Poll::Act(a) = c.poll(now) {
                        self.act(c, a);
                    }
                    return Some(r);
                }
                Poll::Act(a) => self.act(c, a),
            }
        }
    }

    fn act(&mut self, c: &mut Calibration, a: Action) {
        match a {
            Action::Show(content) => {
                let id = self.show(content);
                c.sent(id);
            }
            Action::PatternExposure => {
                if let (Some(cam), Some(lines)) = (self.camera, self.max_exposure) {
                    self.commands.push(Command::SetCamera(CameraSettings {
                        auto_exposure: false,
                        exposure_lines: lines,
                        gain_sixteenths: 16,
                        ..cam
                    }));
                }
            }
            Action::RestoreExposure => {
                if let Some(cam) = self.fit_camera {
                    self.commands.push(Command::SetCamera(cam));
                }
            }
            Action::DotDetection => {
                if let Some(cam) = self.camera {
                    let scale = if cam.format == Format::Raw10 { 4 } else { 1 };
                    self.commands.push(Command::SetDetect(DetectSettings {
                        enabled: true,
                        auto_threshold: false,
                        threshold: DOT_THRESHOLD * scale,
                        auto_area: false,
                        min_area: DOT_MIN_AREA,
                        max_area: u16::MAX,
                    }));
                }
            }
            Action::RestoreDetection => {
                if let Some(d) = self.saved_detect {
                    self.commands.push(Command::SetDetect(d));
                }
            }
        }
    }

    fn page_event(&mut self, e: PageEvent) {
        let at = self.us(e.at);
        match e.kind {
            PageEventKind::Open => {
                self.page = Some(Page {
                    id: e.page,
                    viewport: None,
                    agent: String::new(),
                    hz: None,
                    rtt_us: None,
                    messages: 0,
                    garbled: 0,
                    last_garbled: None,
                });
                self.page_clock.clear();
                self.next_ping = Instant::now();
                // A page that reconnects shows black until told otherwise; a running scene
                // goes on.
                if let Some(r) = self.running
                    && let Some(fit) = &self.fit
                {
                    let id = self.show(Content::Scene {
                        scene: r.scene,
                        model: fit.model,
                        sensor: [fit.sensor.0, fit.sensor.1],
                    });
                    self.running = Some(Running {
                        id,
                        shown: false,
                        ..r
                    });
                }
            }
            PageEventKind::Closed => {
                if self.page.as_ref().is_some_and(|p| p.id == e.page) {
                    self.page = None;
                    if self.calib.is_some() {
                        self.cancel_calibration();
                        self.calib_error = Some("the page disconnected".into());
                    }
                }
            }
            PageEventKind::Garbled(text) => {
                if let Some(p) = &mut self.page {
                    p.garbled += 1;
                    p.last_garbled = Some(text);
                }
            }
            PageEventKind::Message(m) => {
                if let Some(p) = &mut self.page {
                    p.messages += 1;
                }
                self.message(m, at);
            }
        }
    }

    /// The page's time `now` (ms) on the target's clock, or `fallback` before any clock sample.
    fn page_us(&self, now: f64, fallback: f64) -> f64 {
        self.page_clock
            .best()
            .map_or(fallback, |(_, off)| now * 1e3 + off)
    }

    fn message(&mut self, m: FromPage, at: f64) {
        match m {
            FromPage::Hello {
                width,
                height,
                dpr,
                agent,
            } => {
                if let Some(p) = &mut self.page {
                    p.viewport = Some((width, height, dpr));
                    p.agent = agent;
                }
                if self.calib.is_some() {
                    self.cancel_calibration();
                    self.calib_error = Some("the page's viewport changed".into());
                }
            }
            FromPage::Pong { t, now } => {
                let rtt = at - t;
                if rtt >= 0.0 {
                    self.page_clock.add(at, rtt, (t + at) / 2.0 - now * 1e3);
                    let best = self.page_clock.best().map(|b| b.0);
                    if let Some(p) = &mut self.page {
                        p.rtt_us = best;
                    }
                }
            }
            FromPage::Shown { id, now } => {
                let t = self.page_us(now, at);
                if let Some(c) = &mut self.calib {
                    c.shown(id, t);
                }
                if let Some(r) = &mut self.running
                    && r.id == id
                {
                    r.shown = true;
                }
            }
            FromPage::Drawn { now, markers } => {
                let t = self.page_us(now, at);
                if self.running.is_some_and(|r| r.shown)
                    && let Some(c) = &mut self.check
                {
                    c.drawn(t, &markers);
                }
            }
            FromPage::Refresh { hz } => {
                if let Some(p) = &mut self.page {
                    p.hz = Some(hz);
                }
            }
        }
    }

    /// Takes the page's events, pings it, runs the calibration and the check; returns the
    /// camera commands to send. `state` is the board's last.
    pub fn update(&mut self, now: Instant, state: Option<&State>) -> Vec<Command> {
        if let Some(st) = state {
            self.sensor = Some(SensorTiming::of(st));
            self.camera = Some(st.camera);
            self.max_exposure = Some(st.mode.max_exposure_lines);
            self.detect = Some(st.detect.settings);
        }
        while let Ok(e) = self.events.try_recv() {
            self.page_event(e);
        }
        if self.page.is_some() && now >= self.next_ping {
            self.next_ping = now + PING_INTERVAL;
            self.hub.send(ToPage::Ping { t: self.us(now) });
        }
        if let Some(mut c) = self.calib.take() {
            match self.drive(&mut c) {
                None => self.calib = Some(c),
                Some(Ok(fit)) => self.fit = Some(fit),
                Some(Err(e)) => {
                    self.calib_error = Some(e);
                    self.fit_camera = None;
                }
            }
        }
        let t = self.us(now);
        if let Some(c) = &mut self.check {
            c.update(t);
        }
        std::mem::take(&mut self.commands)
    }

    /// The board's time `ts` (µs since its boot) on the target's clock.
    fn board_us(&self, ts: u64) -> Option<f64> {
        self.board_clock.best().map(|(off, _)| ts as f64 + off)
    }

    /// A frame received whole (decoded to RAW8 if it was JPEG).
    pub fn frame(&mut self, f: &Frame) {
        // Its arrival bounds the board's clock (by the frame's whole transit).
        let at = self.us(f.completed);
        let off = at - f.timestamp_us as f64;
        self.board_clock.add(at, off, off);
        let (Some(c), Some(s)) = (&mut self.calib, self.sensor) else {
            return;
        };
        let Some(end) = self
            .board_clock
            .best()
            .map(|(o, _)| f.timestamp_us as f64 + o)
        else {
            return;
        };
        let start = end - s.readout - s.exposure;
        match f.encoding {
            Encoding::Raw8 => c.frame(start, f.width, f.height, &f.data),
            Encoding::Raw10 => {
                let w = usize::from(f.width);
                let mut px = Vec::with_capacity(w * usize::from(f.height));
                for row in f.data.chunks_exact(w / 4 * 5) {
                    for g in row.as_chunks::<5>().0 {
                        px.extend_from_slice(&g[..4]);
                    }
                }
                c.frame(start, f.width, f.height, &px);
            }
            Encoding::Jpeg => {}
        }
    }

    /// A blob frame from the board.
    pub fn blobs(&mut self, b: &BlobFrame) {
        let at = self.us(b.arrived);
        let off = at - (b.timestamp_us as f64 + f64::from(b.timing.queued_us));
        self.board_clock.add(at, off, off);
        let (Some(end), Some(s)) = (self.board_us(b.timestamp_us), self.sensor) else {
            return;
        };
        let start = end - s.readout - s.exposure;
        if let Some(c) = &mut self.calib {
            let centres: Vec<(f64, f64)> = b.blobs.iter().map(|x| (px(x.x), px(x.y))).collect();
            c.blobs(start, &centres);
        }
        if self.running.is_some_and(|r| r.shown)
            && let Some(c) = &mut self.check
        {
            let seen = b
                .blobs
                .iter()
                .map(|x| Seen {
                    x: px(x.x),
                    y: px(x.y),
                    sigma: px(u32::from(x.width)),
                })
                .collect();
            c.detected(
                b.generation,
                b.frame,
                start + s.exposure / 2.0,
                s.period,
                seen,
            );
        }
    }
}
