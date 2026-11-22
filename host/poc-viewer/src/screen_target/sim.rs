//! A simulated screen in front of the fake camera, and a page for the tests.
//!
//! [`SimScreen`] watches the page's traffic through the hub ([`PageTap`]) and keeps what the
//! page shows over time: a pattern from the page's [`FromPage::Shown`], a scene's markers from
//! each [`FromPage::Drawn`], each lit [`DELAY`] after the page drew it (the display's latency).
//! The page's times are mapped to this PC's by the fastest report (as a display follows the
//! page's frame clock, not the network).
//! The fake camera films it ([`SimScreen::render`]) through a known model ([`model`]): any page
//! does, the real one in a browser as well as [`TestPage`]. So calibration and the check run
//! end to end without a board and are checked against a known answer.
//!
//! [`TestPage`] is a page in Rust for the tests: it speaks the page's protocol through the hub,
//! shows the patterns and dots, and animates three scenes at 60 Hz: static, a random walk
//! ([`Walk`]: markers that cross, leave the view and are replaced), and every other kind as
//! Lissajous paths.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use espion_proto::cam::{CameraSettings, Encoding};

use super::{
    link::{Axis, Content, Drawn, FWHM_PER_SIGMA, FromPage, SceneKind, ToPage},
    model::Model,
    server::{Hub, PageTap},
};
use crate::fake::{MARKER_BRIGHTNESS, encode_line};

/// From the viewer receiving the page's report to the light leaving the simulated screen.
pub const DELAY: Duration = Duration::from_millis(40);
/// The screen's black, 10-bit.
const BLACK: f64 = 30.0;
/// The simulated lens's vignetting: light falls off as `exp(-VIGNETTING q²)`, `q` the distance
/// from the sensor's centre over half its diagonal (0.37 in the corners).
pub const VIGNETTING: f64 = 1.0;

/// The simulated screen's model: seen upside down at about 4.5 sensor pixels per screen pixel,
/// slightly tilted and in perspective, through a barrel-distorting lens. On a 1280x800 window
/// it sees the screen's CSS pixels from about (20, 290) to (300, 470).
pub fn model() -> Model {
    let (s, a) = (4.5, 181.5f64.to_radians());
    let (c, n) = (s * a.cos(), s * a.sin());
    Model {
        k1: -0.04,
        k2: 0.01,
        ..Model::homography(
            [c, -n, 2600.0, n, c, 2100.0, 1.0e-5, -2.0e-5, 1.0],
            1280,
            800,
        )
    }
}

#[derive(Debug, Clone)]
enum Screen {
    Content(Content),
    Markers(Vec<Drawn>),
}

#[derive(Default)]
struct State {
    /// What each show asked for (the newest few).
    shows: HashMap<u32, Content>,
    viewport: (f64, f64),
    /// The instant of the page's time 0: the earliest of each report's arrival less its time.
    page_zero: Option<Instant>,
    /// What the screen shows from when.
    timeline: VecDeque<(Instant, Screen)>,
    /// Sensor window → each pixel's screen point and vignetting.
    map: Option<PixelMap>,
}

/// A sensor window's size, and each of its pixels' screen point and vignetting.
type PixelMap = ((u16, u16), Arc<Pixels>);

struct Pixels {
    screen: Vec<(f32, f32)>,
    falloff: Vec<f32>,
}

/// The simulated screen.
pub struct SimScreen {
    model: Model,
    state: Mutex<State>,
}

impl std::fmt::Debug for SimScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimScreen")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl SimScreen {
    pub fn new(model: Model) -> Arc<Self> {
        Arc::new(Self {
            model,
            state: Mutex::new(State::default()),
        })
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `s` is drawn at the page's time `now` (ms), which reached the viewer at `at`.
    fn push(&self, now: f64, at: Instant, s: Screen) {
        let mut st = self.lock();
        let zero = at
            .checked_sub(Duration::from_secs_f64(now.max(0.0) / 1e3))
            .unwrap_or(at);
        let zero = st.page_zero.map_or(zero, |z| z.min(zero));
        st.page_zero = Some(zero);
        let lit = zero + Duration::from_secs_f64(now.max(0.0) / 1e3) + DELAY;
        st.timeline.push_back((lit, s));
        while st.timeline.len() > 2 && st.timeline[1].0 + Duration::from_secs(2) < lit {
            st.timeline.pop_front();
        }
    }

    /// Each pixel's screen point and vignetting for a window of `w` x `h`.
    fn map(&self, w: u16, h: u16) -> Arc<Pixels> {
        if let Some((size, m)) = &self.lock().map
            && *size == (w, h)
        {
            return m.clone();
        }
        let n = usize::from(w) * usize::from(h);
        let mut p = Pixels {
            screen: Vec::with_capacity(n),
            falloff: Vec::with_capacity(n),
        };
        for v in 0..h {
            for u in 0..w {
                let (u, v) = (f64::from(u), f64::from(v));
                let (x, y) = self.model.to_screen(u, v);
                p.screen.push((x as f32, y as f32));
                p.falloff.push(falloff(&self.model, u, v) as f32);
            }
        }
        let p = Arc::new(p);
        self.lock().map = Some(((w, h), p.clone()));
        p
    }

    /// The camera's frame `number` of the screen at `at`, in `encoding` (RAW8 or RAW10), with
    /// the brightness of the fake's markers (proportional to exposure times gain).
    pub fn render(
        &self,
        c: &CameraSettings,
        encoding: Encoding,
        number: u32,
        at: Instant,
        out: &mut Vec<u8>,
    ) {
        let (w, h) = (usize::from(c.width), usize::from(c.height));
        let lb = encoding
            .line_bytes(c.width)
            .expect("render makes raw encodings") as usize;
        out.resize(lb * h, 0);
        let (screen, viewport) = {
            let st = self.lock();
            let i = st.timeline.partition_point(|(t, _)| *t <= at);
            let s = i.checked_sub(1).map(|i| st.timeline[i].1.clone());
            (s, st.viewport)
        };
        let amp =
            f64::from(c.exposure_lines) * f64::from(c.gain_sixteenths) / 16.0 * MARKER_BRIGHTNESS;
        let mut img = vec![0f32; w * h];
        let noise = |x: usize, y: usize| {
            let mut z = (x as u64) << 40 ^ (y as u64) << 20 ^ u64::from(number);
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            (z >> 61) as f32 - 3.5
        };
        let lit = |x: f32, y: f32| {
            x >= 0.0 && y >= 0.0 && f64::from(x) < viewport.0 && f64::from(y) < viewport.1
        };
        match &screen {
            Some(Screen::Content(Content::White | Content::Gray { .. })) => {
                let map = self.map(c.width, c.height);
                let Some(Screen::Content(content)) = &screen else {
                    unreachable!()
                };
                for (v, &(x, y)) in img.iter_mut().zip(map.screen.iter()) {
                    let on = lit(x, y)
                        && match content {
                            Content::Gray { axis, bit, inverse } => {
                                let p = if *axis == Axis::X { x } else { y } as u32;
                                ((p ^ (p >> 1)) >> bit & 1 == 1) != *inverse
                            }
                            _ => true,
                        };
                    *v = if on { amp as f32 } else { 0.0 };
                }
                // The lens: a box blur of BLUR pixels, so the finest stripes (one screen
                // pixel, about 4.5 sensor pixels) blur to grey as on the real camera.
                box_blur(&mut img, w, h, BLUR);
            }
            Some(Screen::Content(Content::Dots { dots })) => {
                for d in dots {
                    self.spot(&mut img, w, h, d.x, d.y, d.sigma, amp);
                }
            }
            Some(Screen::Markers(markers)) => {
                for d in markers {
                    if lit(d.x as f32, d.y as f32) {
                        self.spot(&mut img, w, h, d.x, d.y, d.sigma, amp * d.level);
                    }
                }
            }
            _ => {}
        }
        let map = self.map(c.width, c.height);
        let mut line = vec![0u16; w];
        for (y, row) in out.chunks_exact_mut(lb).enumerate() {
            for (x, l) in line.iter_mut().enumerate() {
                let i = y * w + x;
                let v = BLACK as f32 + noise(x, y) + img[i] * map.falloff[i];
                *l = v.round().clamp(0.0, 1023.0) as u16;
            }
            encode_line(encoding, &line, row);
        }
    }
}

/// Width of the simulated lens's blur, sensor pixels (odd).
const BLUR: usize = 9;

/// A separable box blur of width `k` (odd), clamped at the edges.
fn box_blur(img: &mut [f32], w: usize, h: usize, k: usize) {
    let r = k / 2;
    let blur = |src: &[f32], out: &mut [f32]| {
        let n = src.len();
        let at = |i: isize| src[i.clamp(0, n as isize - 1) as usize];
        let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
        for (i, o) in out.iter_mut().enumerate() {
            *o = acc / k as f32;
            let i = i as isize;
            acc += at(i + r as isize + 1) - at(i - r as isize);
        }
    };
    let mut src = vec![0f32; w.max(h)];
    let mut out = vec![0f32; w.max(h)];
    for y in 0..h {
        src[..w].copy_from_slice(&img[y * w..(y + 1) * w]);
        blur(&src[..w], &mut out[..w]);
        img[y * w..(y + 1) * w].copy_from_slice(&out[..w]);
    }
    for x in 0..w {
        for y in 0..h {
            src[y] = img[y * w + x];
        }
        blur(&src[..h], &mut out[..h]);
        for y in 0..h {
            img[y * w + x] = out[y];
        }
    }
}

impl SimScreen {
    /// Adds a Gaussian spot of screen sigma `sigma` at screen point `(x, y)`.
    #[allow(clippy::too_many_arguments)]
    fn spot(&self, img: &mut [f32], w: usize, h: usize, x: f64, y: f64, sigma: f64, amp: f64) {
        let (u, v) = self.model.to_sensor(x, y);
        let s = sigma * self.model.scale_at(x, y);
        let two_var = 2.0 * s * s;
        let reach = 4.0 * s + 1.0;
        let (u0, u1) = (
            (u - reach).max(0.0) as usize,
            ((u + reach).max(0.0) as usize).min(w.saturating_sub(1)),
        );
        let (v0, v1) = (
            (v - reach).max(0.0) as usize,
            ((v + reach).max(0.0) as usize).min(h.saturating_sub(1)),
        );
        if u + reach < 0.0 || v + reach < 0.0 {
            return;
        }
        for yy in v0..=v1 {
            for xx in u0..=u1 {
                let r2 = (xx as f64 - u).powi(2) + (yy as f64 - v).powi(2);
                img[yy * w + xx] += (amp * (-r2 / two_var).exp()) as f32;
            }
        }
    }
}

/// The simulated lens's vignetting at sensor point `(u, v)` ([`VIGNETTING`]).
pub fn falloff(m: &Model, u: f64, v: f64) -> f64 {
    let q = (u - m.cx).hypot(v - m.cy) / m.norm;
    (-VIGNETTING * q * q).exp()
}

impl PageTap for SimScreen {
    fn sent(&self, m: &ToPage, _at: Instant) {
        if let ToPage::Show { id, content } = m {
            let mut st = self.lock();
            if st.shows.len() > 64 {
                st.shows.clear();
            }
            st.shows.insert(*id, content.clone());
        }
    }

    fn received(&self, m: &FromPage, at: Instant) {
        match m {
            FromPage::Hello { width, height, .. } => {
                let mut st = self.lock();
                st.viewport = (*width, *height);
                // A page that (re)connects has a clock of its own.
                st.page_zero = None;
            }
            FromPage::Shown { id, now } => {
                let c = self.lock().shows.get(id).cloned();
                match c {
                    Some(Content::Scene { .. }) => {
                        self.push(*now, at, Screen::Markers(Vec::new()));
                    }
                    Some(c) => self.push(*now, at, Screen::Content(c)),
                    None => {}
                }
            }
            FromPage::Drawn { markers, now } => {
                self.push(*now, at, Screen::Markers(markers.clone()));
            }
            FromPage::Pong { .. } | FromPage::Refresh { .. } => {}
        }
    }
}

/// A random walk over a `w` x `h` sensor view, for the test page and the tests:
/// [`Walk::MARKERS`] markers of `size` (each between 0.7 and 1.5 of it), each wandering at its
/// own typical speed (100 to 600 px/s, a velocity lasts about a second), so they cross and
/// touch. One that leaves the view is replaced by a new one, with the next ID, entering
/// elsewhere on the edge. The same walk for the same seed and the same steps.
pub struct Walk {
    rng: u64,
    next_id: u32,
    live: Vec<Walker>,
    size: (f64, f64),
    /// Scene time of the last step, seconds.
    at: f64,
    size_px: f64,
}

struct Walker {
    id: u32,
    u: f64,
    v: f64,
    vu: f64,
    vv: f64,
    speed: f64,
    size: f64,
}

impl Walk {
    pub const MARKERS: usize = 6;
    /// How long a velocity lasts, seconds.
    const TAU: f64 = 1.0;

    pub fn new(seed: u32, w: f64, h: f64, size_px: f64) -> Self {
        let mut walk = Self {
            rng: u64::from(seed),
            next_id: 1,
            live: Vec::new(),
            size: (w - 1.0, h - 1.0),
            at: 0.0,
            size_px,
        };
        for _ in 0..Self::MARKERS {
            let m = walk.spawn();
            walk.live.push(m);
        }
        walk
    }

    /// Uniform in 0..1 (splitmix64).
    fn uniform(&mut self) -> f64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn gauss(&mut self) -> f64 {
        let a = self.uniform().max(f64::MIN_POSITIVE);
        (-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * self.uniform()).cos()
    }

    /// A marker just outside the edge, heading for a point well inside.
    fn spawn(&mut self) -> Walker {
        let (x, y) = self.size;
        let size = self.size_px * (0.7 + 0.8 * self.uniform());
        let s = self.uniform() * 2.0 * (x + y);
        let out = 1.5 * size;
        let (u, v) = if s < x {
            (s, -out)
        } else if s < x + y {
            (x + out, s - x)
        } else if s < 2.0 * x + y {
            (s - x - y, y + out)
        } else {
            (-out, s - 2.0 * x - y)
        };
        let speed = (100f64.ln() + self.uniform() * (600f64 / 100.0).ln()).exp();
        let (tu, tv) = (
            x * (0.15 + 0.7 * self.uniform()),
            y * (0.15 + 0.7 * self.uniform()),
        );
        let d = (tu - u).hypot(tv - v).max(1.0);
        let id = self.next_id;
        self.next_id += 1;
        Walker {
            id,
            u,
            v,
            vu: (tu - u) / d * speed,
            vv: (tv - v) / d * speed,
            speed,
            size,
        }
    }

    /// The markers at scene time `secs`: ID, sensor position, size.
    pub fn step(&mut self, secs: f64) -> Vec<(u32, f64, f64, f64)> {
        let dt = (secs - self.at).clamp(0.0, 0.1);
        self.at = secs;
        let (x, y) = self.size;
        for i in 0..self.live.len() {
            let (gu, gv) = (self.gauss(), self.gauss());
            let m = &mut self.live[i];
            // An Ornstein–Uhlenbeck velocity: each component's deviation is speed/√2.
            let kick = m.speed * (dt / Self::TAU).sqrt();
            m.vu += -m.vu * dt / Self::TAU + kick * gu;
            m.vv += -m.vv * dt / Self::TAU + kick * gv;
            m.u += m.vu * dt;
            m.v += m.vv * dt;
            let outside = (-m.u).max(m.u - x).max(-m.v).max(m.v - y);
            if outside > 2.0 * m.size {
                self.live[i] = self.spawn();
            }
        }
        self.live.iter().map(|m| (m.id, m.u, m.v, m.size)).collect()
    }
}

/// A page in Rust for the tests, on its own thread; stops when dropped.
pub struct TestPage {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// The test page's viewport, CSS pixels.
pub const TEST_VIEWPORT: (f64, f64) = (1180.0, 820.0);

impl TestPage {
    pub fn start(hub: &Hub) -> Self {
        let mut end = hub.open();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = thread::spawn(move || {
            // The page's clock starts elsewhere than the viewer's.
            let epoch = Instant::now() - Duration::from_secs(1234);
            let now = || epoch.elapsed().as_secs_f64() * 1e3;
            end.send(FromPage::Hello {
                width: TEST_VIEWPORT.0,
                height: TEST_VIEWPORT.1,
                dpr: 2.0,
                agent: "test page".into(),
                build: super::server::page_build(),
            });
            let mut showing: Option<(u32, Content, bool)> = None;
            let mut scene_start = 0.0;
            let mut walk: Option<Walk> = None;
            let tick = Duration::from_secs_f64(1.0 / 60.0);
            let mut next = Instant::now();
            while !s.load(Ordering::Relaxed) {
                while let Ok(m) = end.inbox.try_recv() {
                    match m {
                        ToPage::Ping { t } => end.send(FromPage::Pong { t, now: now() }),
                        ToPage::Show { id, content } => showing = Some((id, content, false)),
                        // It is always the viewer's build.
                        ToPage::Reload => {}
                    }
                }
                let t = now();
                if let Some((id, content, shown)) = &mut showing {
                    if !*shown {
                        *shown = true;
                        scene_start = t;
                        walk = None;
                        end.send(FromPage::Shown { id: *id, now: t });
                    }
                    if let Content::Scene {
                        scene,
                        model,
                        sensor,
                        vignette,
                    } = content
                    {
                        let secs = (t - scene_start) / 1e3;
                        let (w, h) = (f64::from(sensor[0]), f64::from(sensor[1]));
                        let n = usize::from(scene.count.max(1));
                        let at: Vec<(u32, f64, f64, f64)> = match scene.kind {
                            SceneKind::RandomWalk => walk
                                .get_or_insert_with(|| Walk::new(scene.seed, w, h, scene.size))
                                .step(secs),
                            kind => (0..n)
                                .map(|i| {
                                    let cx = w * (2 * i + 1) as f64 / (2 * n) as f64;
                                    let (u, v) = if kind == SceneKind::Static {
                                        (cx, h / 2.0)
                                    } else {
                                        let (ax, ay) = (0.35 * w / n as f64, 0.35 * h);
                                        let wt = scene.speed / ax.hypot(2.0 * ay) * secs;
                                        (
                                            cx + ax * (wt + i as f64 * 0.7).sin(),
                                            h / 2.0 + ay * (2.0 * wt).sin(),
                                        )
                                    };
                                    (i as u32, u, v, scene.size)
                                })
                                .collect(),
                        };
                        let markers = at
                            .into_iter()
                            .map(|(id, u, v, size)| {
                                let (x, y) = model.to_screen(u, v);
                                Drawn {
                                    id,
                                    x,
                                    y,
                                    sigma: size / FWHM_PER_SIGMA / model.scale_at(x, y),
                                    level: vignette.as_ref().map_or(1.0, |vg| vg.level(u, v)),
                                }
                            })
                            .collect();
                        end.send(FromPage::Drawn { now: t, markers });
                    }
                }
                next += tick;
                thread::sleep(next.saturating_duration_since(Instant::now()));
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for TestPage {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
