//! The check: each detected frame against what the page showed during its exposure.
//!
//! The page reports every frame it draws (its markers in screen pixels, on its clock mapped to
//! the viewer's); the model takes them to the sensor. A detected frame's exposure (its middle,
//! on the viewer's clock) saw the page's latest frame drawn at least the **delay** before:
//! the time from the page drawing to the light leaving the screen, plus what the clocks do not
//! see (the smallest transit from the board). The delay is fitted over the last [`FIT_SPAN_US`]
//! while markers move; a still scene keeps the last one.
//!
//! Markers are matched to blobs nearest first, within a gate (twice the marker's FWHM, at least
//! [`GATE_MIN`], plus what it moves in a frame period). A marker whose centre is within 3 sigma
//! of the sensor window's edge is "at the edge": found or not, it is counted apart.

use std::collections::VecDeque;

use super::{
    link::{Drawn, FWHM_PER_SIGMA},
    model::Model,
};

/// Smallest gate, sensor pixels.
pub const GATE_MIN: f64 = 6.0;
/// The figures' window, µs.
pub const WINDOW_US: f64 = 10_000_000.0;
/// The delay is fitted over this much of the newest frames, µs, once a second.
pub const FIT_SPAN_US: f64 = 4_000_000.0;
/// The delays tried, µs.
pub const DELAY_RANGE_US: (f64, f64) = (0.0, 300_000.0);
/// Below this speed (sensor pixels per second) markers count as still for the delay.
const STILL: f64 = 20.0;
/// An error counted at most this much in the delay's cost, pixels.
const COST_CAP: f64 = 8.0;
/// Speed bins of the sweep, sensor pixels per frame period: upper bounds.
pub const SPEED_BINS: [f64; 8] = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, f64::INFINITY];

/// A marker of a drawn frame, on the sensor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Marker {
    pub id: u32,
    pub u: f64,
    pub v: f64,
    /// Standard deviation on the sensor, pixels.
    pub sigma: f64,
    /// Speed on the sensor, pixels per second (0 for a marker's first frame).
    pub speed: f64,
}

/// Where a marker is against the sensor window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Inside,
    Edge,
    Outside,
}

impl Marker {
    pub fn place(&self, width: u16, height: u16) -> Place {
        let r = 3.0 * self.sigma;
        let (w, h) = (f64::from(width) - 1.0, f64::from(height) - 1.0);
        if self.u >= r && self.v >= r && self.u <= w - r && self.v <= h - r {
            Place::Inside
        } else if self.u >= -r && self.v >= -r && self.u <= w + r && self.v <= h + r {
            Place::Edge
        } else {
            Place::Outside
        }
    }
}

/// A blob of a detected frame: centre and width (sigma), sensor pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seen {
    pub x: f64,
    pub y: f64,
    pub sigma: f64,
    /// Sum above the local background, the sensor format's units.
    pub flux: f64,
}

#[derive(Debug, Clone)]
struct Draw {
    at_us: f64,
    markers: Vec<Marker>,
}

#[derive(Debug, Clone)]
struct Observed {
    generation: u32,
    frame: u32,
    mid_us: f64,
    period_us: f64,
    blobs: Vec<Seen>,
}

/// One detected frame against the page's frame it saw.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameMatch {
    pub generation: u32,
    pub frame: u32,
    pub mid_us: f64,
    /// The page's frame: when it was drawn, and its markers.
    pub drawn_us: f64,
    pub markers: Vec<(Marker, Place, Option<usize>)>,
    pub blobs: Vec<Seen>,
    /// Blobs matched to no marker.
    pub extra: usize,
    pub period_us: f64,
}

impl FrameMatch {
    /// Every marker inside was found and no blob is extra.
    pub fn exact(&self) -> bool {
        self.extra == 0
            && self
                .markers
                .iter()
                .all(|(_, p, b)| *p != Place::Inside || b.is_some())
    }
}

/// Error figures, sensor pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Errors {
    pub n: usize,
    pub mean: f64,
    pub rms: f64,
    pub p95: f64,
    pub max: f64,
    /// Mean signed error (blob minus marker).
    pub dx: f64,
    pub dy: f64,
}

impl Errors {
    fn of(e: &mut [(f64, f64, f64)]) -> Option<Self> {
        if e.is_empty() {
            return None;
        }
        let n = e.len() as f64;
        e.sort_by(|a, b| a.0.total_cmp(&b.0));
        Some(Self {
            n: e.len(),
            mean: e.iter().map(|x| x.0).sum::<f64>() / n,
            rms: (e.iter().map(|x| x.0 * x.0).sum::<f64>() / n).sqrt(),
            p95: e[((e.len() - 1) as f64 * 0.95).round() as usize].0,
            max: e[e.len() - 1].0,
            dx: e.iter().map(|x| x.1).sum::<f64>() / n,
            dy: e.iter().map(|x| x.2).sum::<f64>() / n,
        })
    }
}

/// Markers of one speed bin.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SpeedBin {
    /// Upper bound, sensor pixels per frame period.
    pub upto: f64,
    pub markers: usize,
    pub found: usize,
    pub rms: f64,
}

/// Counts of frames and markers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Counts {
    pub frames: u64,
    /// Frames with every marker inside found and no extra blob.
    pub exact: u64,
    /// Markers inside, found, missed; blobs matched to no marker.
    pub inside: u64,
    pub found: u64,
    pub missed: u64,
    pub extra: u64,
    /// Markers at the edge, and of them found.
    pub edge: u64,
    pub edge_found: u64,
}

impl Counts {
    fn add(&mut self, m: &FrameMatch) {
        self.frames += 1;
        self.exact += u64::from(m.exact());
        self.extra += m.extra as u64;
        for (_, p, b) in &m.markers {
            match p {
                Place::Inside => {
                    self.inside += 1;
                    if b.is_some() {
                        self.found += 1;
                    } else {
                        self.missed += 1;
                    }
                }
                Place::Edge => {
                    self.edge += 1;
                    self.edge_found += u64::from(b.is_some());
                }
                Place::Outside => {}
            }
        }
    }
}

/// The check's figures.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    /// Over the window, and since the scene started.
    pub window: Counts,
    pub total: Counts,
    /// Position errors of the markers inside, over the window.
    pub errors: Option<Errors>,
    /// Blob width over the marker's drawn sigma, mean over the window.
    pub width_ratio: Option<f64>,
    /// Spread of the found blobs' brightness across the picture, over the window: the
    /// coefficient of variation of each blob's flux over its marker's drawn sigma squared
    /// (alike for any marker size), relative to its frame's mean (the exposure changes), in
    /// frames with two blobs or more. The vignetting widens it; the page's compensation
    /// narrows it.
    pub flux_cv: Option<f64>,
    /// The fitted delay, µs, and whether the markers moved to fit it.
    pub delay_us: Option<f64>,
    /// Markers by speed, over the window.
    pub speeds: Vec<SpeedBin>,
}

/// The check of one scene.
pub struct Check {
    model: Model,
    sensor: (u16, u16),
    draws: VecDeque<Draw>,
    /// Detected frames not matched yet (their page frame or the delay not known yet).
    pending: VecDeque<Observed>,
    /// Detected frames of the last [`FIT_SPAN_US`], for the delay's fit.
    recent: VecDeque<Observed>,
    matches: VecDeque<FrameMatch>,
    delay_us: Option<f64>,
    next_fit_us: f64,
    total: Counts,
    report: Report,
}

impl Check {
    pub fn new(model: Model, sensor: (u16, u16)) -> Self {
        Self {
            model,
            sensor,
            draws: VecDeque::new(),
            pending: VecDeque::new(),
            recent: VecDeque::new(),
            matches: VecDeque::new(),
            delay_us: None,
            next_fit_us: 0.0,
            total: Counts::default(),
            report: Report::default(),
        }
    }

    /// A frame the page drew at `at_us`.
    pub fn drawn(&mut self, at_us: f64, markers: &[Drawn]) {
        let prev = self.draws.back().cloned();
        let markers = markers
            .iter()
            .map(|d| {
                let (u, v) = self.model.to_sensor(d.x, d.y);
                let sigma = d.sigma * self.model.scale_at(d.x, d.y);
                let speed = prev
                    .as_ref()
                    .filter(|p| at_us > p.at_us)
                    .and_then(|p| {
                        let q = p.markers.iter().find(|m| m.id == d.id)?;
                        Some((u - q.u).hypot(v - q.v) / ((at_us - p.at_us) / 1e6))
                    })
                    .unwrap_or(0.0);
                Marker {
                    id: d.id,
                    u,
                    v,
                    sigma,
                    speed,
                }
            })
            .collect();
        self.draws.push_back(Draw { at_us, markers });
    }

    /// A detected frame whose exposure's middle is at `mid_us`, frame period `period_us`.
    pub fn detected(
        &mut self,
        generation: u32,
        frame: u32,
        mid_us: f64,
        period_us: f64,
        blobs: Vec<Seen>,
    ) {
        let o = Observed {
            generation,
            frame,
            mid_us,
            period_us,
            blobs,
        };
        self.recent.push_back(o.clone());
        while self
            .recent
            .front()
            .is_some_and(|f| f.mid_us < mid_us - FIT_SPAN_US)
        {
            self.recent.pop_front();
        }
        self.pending.push_back(o);
    }

    /// The page's frame on the screen at `t_us`: the latest drawn by then, if a later one
    /// shows it was replaced or is still the newest.
    fn draw_at(&self, t_us: f64) -> Option<(&Draw, bool)> {
        let i = self.draws.partition_point(|d| d.at_us <= t_us);
        let d = self.draws.get(i.checked_sub(1)?)?;
        Some((d, i < self.draws.len()))
    }

    fn matched(&self, o: &Observed, d: &Draw) -> FrameMatch {
        let (w, h) = self.sensor;
        let markers: Vec<(Marker, Place)> = d.markers.iter().map(|m| (*m, m.place(w, h))).collect();
        let mut pairs = Vec::new();
        for (i, (m, p)) in markers.iter().enumerate() {
            if *p == Place::Outside {
                continue;
            }
            let gate = (2.0 * FWHM_PER_SIGMA * m.sigma).max(GATE_MIN) + m.speed * o.period_us / 1e6;
            for (j, b) in o.blobs.iter().enumerate() {
                let r = (b.x - m.u).hypot(b.y - m.v);
                if r <= gate {
                    pairs.push((r, i, j));
                }
            }
        }
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut to_blob = vec![None; markers.len()];
        let mut used = vec![false; o.blobs.len()];
        for (_, i, j) in pairs {
            if to_blob[i].is_none() && !used[j] {
                to_blob[i] = Some(j);
                used[j] = true;
            }
        }
        FrameMatch {
            generation: o.generation,
            frame: o.frame,
            mid_us: o.mid_us,
            drawn_us: d.at_us,
            markers: markers
                .into_iter()
                .zip(to_blob)
                .map(|((m, p), b)| (m, p, b))
                .collect(),
            extra: used.iter().filter(|u| !**u).count(),
            blobs: o.blobs.clone(),
            period_us: o.period_us,
        }
    }

    /// The delay's cost: errors of the markers inside, capped, a missed one at the cap.
    fn cost(&self, delay_us: f64, frames: &[&Observed]) -> Option<f64> {
        let mut sum = 0.0;
        let mut n = 0usize;
        for o in frames {
            let Some((d, _)) = self.draw_at(o.mid_us - delay_us) else {
                continue;
            };
            let m = self.matched(o, d);
            for (mk, p, b) in &m.markers {
                if *p != Place::Inside {
                    continue;
                }
                n += 1;
                sum += b.map_or(COST_CAP, |j| {
                    (m.blobs[j].x - mk.u)
                        .hypot(m.blobs[j].y - mk.v)
                        .min(COST_CAP)
                });
            }
        }
        (n > 0).then(|| sum / n as f64)
    }

    /// Fits the delay to the newest frames while markers move.
    fn fit_delay(&mut self, now_us: f64) {
        let frames: Vec<&Observed> = self
            .recent
            .iter()
            .filter(|o| o.mid_us >= now_us - FIT_SPAN_US)
            .collect();
        let moving = self
            .draws
            .iter()
            .filter(|d| d.at_us >= now_us - FIT_SPAN_US)
            .flat_map(|d| d.markers.iter())
            .any(|m| m.speed > STILL);
        if frames.len() < 30 || !moving {
            return;
        }
        let mut best: Option<(f64, f64)> = None;
        let mut t = DELAY_RANGE_US.0;
        while t <= DELAY_RANGE_US.1 {
            if let Some(c) = self.cost(t, &frames)
                && best.is_none_or(|b| c < b.1)
            {
                best = Some((t, c));
            }
            t += 4000.0;
        }
        let Some((coarse, _)) = best else {
            return;
        };
        let mut t = (coarse - 4000.0).max(DELAY_RANGE_US.0);
        while t <= coarse + 4000.0 {
            if let Some(c) = self.cost(t, &frames)
                && best.is_none_or(|b| c < b.1)
            {
                best = Some((t, c));
            }
            t += 250.0;
        }
        self.delay_us = best.map(|b| b.0);
    }

    /// Matches the frames whose page frame is known, refits the delay once a second and makes
    /// the figures; `now_us` on the viewer's clock.
    pub fn update(&mut self, now_us: f64) {
        if now_us >= self.next_fit_us {
            self.next_fit_us = now_us + 1e6;
            self.fit_delay(now_us);
        }
        // Still: a second of frames drawn and none of their markers moving.
        let span = self
            .draws
            .back()
            .zip(self.draws.front())
            .map_or(0.0, |(b, f)| b.at_us - f.at_us);
        let still = span >= 1e6
            && !self
                .draws
                .iter()
                .any(|d| d.markers.iter().any(|m| m.speed > STILL));
        // A still scene needs no delay: the newest frame drawn before the exposure.
        let delay = self.delay_us.or(still.then_some(0.0));
        if let Some(delay) = delay {
            while let Some(o) = self.pending.front() {
                match self.draw_at(o.mid_us - delay) {
                    Some((d, true)) => {
                        let m = self.matched(o, d);
                        self.total.add(&m);
                        self.matches.push_back(m);
                        self.pending.pop_front();
                    }
                    // Not known yet whether a newer frame was drawn.
                    Some((_, false)) => break,
                    // Before the scene's first frame.
                    None => {
                        self.pending.pop_front();
                    }
                }
            }
        }
        // Frames waiting for a delay are kept as long as the window.
        while self
            .pending
            .front()
            .is_some_and(|o| o.mid_us < now_us - WINDOW_US)
        {
            self.pending.pop_front();
        }
        while self
            .matches
            .front()
            .is_some_and(|m| m.mid_us < now_us - WINDOW_US)
        {
            self.matches.pop_front();
        }
        let keep = now_us - WINDOW_US - DELAY_RANGE_US.1 - 1e6;
        while self.draws.len() > 1 && self.draws[1].at_us < keep {
            self.draws.pop_front();
        }
        self.report = self.figures();
    }

    fn figures(&self) -> Report {
        let mut window = Counts::default();
        let mut errors = Vec::new();
        let mut ratio = (0.0, 0usize);
        let mut flux = Vec::new();
        let mut bins: Vec<SpeedBin> = SPEED_BINS
            .iter()
            .map(|&upto| SpeedBin {
                upto,
                ..SpeedBin::default()
            })
            .collect();
        let mut bin_err = vec![0.0; bins.len()];
        for m in &self.matches {
            window.add(m);
            let frame_flux = flux.len();
            for (mk, p, b) in &m.markers {
                if *p != Place::Inside {
                    continue;
                }
                let per_frame = mk.speed * m.period_us / 1e6;
                let k = SPEED_BINS
                    .partition_point(|&u| u < per_frame)
                    .min(bins.len() - 1);
                bins[k].markers += 1;
                if let Some(j) = b {
                    let s = &m.blobs[*j];
                    let (dx, dy) = (s.x - mk.u, s.y - mk.v);
                    let e = dx.hypot(dy);
                    errors.push((e, dx, dy));
                    bins[k].found += 1;
                    bin_err[k] += e * e;
                    if mk.sigma > 0.0 {
                        ratio = (ratio.0 + s.sigma / mk.sigma, ratio.1 + 1);
                        flux.push(s.flux / (mk.sigma * mk.sigma));
                    }
                }
            }
            // Relative to the frame's mean: the exposure changes between frames.
            let this = &mut flux[frame_flux..];
            if this.len() < 2 {
                flux.truncate(frame_flux);
            } else {
                let mean = this.iter().sum::<f64>() / this.len() as f64;
                this.iter_mut().for_each(|f| *f /= mean);
            }
        }
        for (b, e) in bins.iter_mut().zip(bin_err) {
            if b.found > 0 {
                b.rms = (e / b.found as f64).sqrt();
            }
        }
        Report {
            window,
            total: self.total,
            errors: Errors::of(&mut errors),
            width_ratio: (ratio.1 > 0).then(|| ratio.0 / ratio.1 as f64),
            flux_cv: (flux.len() > 1).then(|| {
                // The frames' means are 1.
                let n = flux.len() as f64;
                (flux.iter().map(|f| (f - 1.0).powi(2)).sum::<f64>() / (n - 1.0)).sqrt()
            }),
            delay_us: self.delay_us,
            speeds: bins.into_iter().filter(|b| b.markers > 0).collect(),
        }
    }

    pub fn report(&self) -> &Report {
        &self.report
    }

    /// The match of detected frame `frame` of `generation`, once known.
    pub fn match_of(&self, generation: u32, frame: u32) -> Option<&FrameMatch> {
        self.matches
            .iter()
            .rev()
            .find(|m| m.generation == generation && m.frame == frame)
    }

    /// The newest match.
    pub fn newest(&self) -> Option<&FrameMatch> {
        self.matches.back()
    }

    pub fn model(&self) -> &Model {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen_target::model::tests::sample;

    /// A marker moving across the sensor at 600 px/s, the page drawing at 60 Hz, the camera
    /// exposing at 120 Hz with the light leaving the screen `delay` after the page drew.
    fn run(delay_us: f64) -> Check {
        let m = sample();
        let mut c = Check::new(m, (1280, 800));
        let path = |t_us: f64| {
            let t = t_us / 1e6;
            (
                640.0 + 400.0 * (t / 0.6).sin(),
                400.0 + 150.0 * (t / 0.4).sin(),
            )
        };
        let mut drawn_at = 0.0;
        let mut frame = 0;
        let mut t = 0.0;
        while t < 6e6 {
            while drawn_at <= t {
                let (u, v) = path(drawn_at);
                let (x, y) = m.to_screen(u, v);
                c.drawn(
                    drawn_at,
                    &[Drawn {
                        id: 1,
                        x,
                        y,
                        sigma: 0.5,
                        level: 1.0,
                    }],
                );
                drawn_at += 1e6 / 60.0;
            }
            // The page's frame drawn last before `t - delay`.
            let shown = ((t - delay_us) / (1e6 / 60.0)).floor() * (1e6 / 60.0);
            if shown >= 0.0 {
                let (u, v) = path(shown);
                c.detected(
                    1,
                    frame,
                    t,
                    1e6 / 120.0,
                    vec![Seen {
                        x: u + 0.05,
                        y: v,
                        sigma: 2.0,
                        flux: 100.0,
                    }],
                );
            }
            frame += 1;
            t += 1e6 / 120.0;
            c.update(t);
        }
        c
    }

    #[test]
    fn the_delay_is_found_and_the_markers_match() {
        let c = run(37_000.0);
        let r = c.report();
        let d = r.delay_us.unwrap();
        // Any delay that picks the same page frames fits: within one page frame period.
        assert!((d - 37_000.0).abs() < 1e6 / 60.0, "delay {d}");
        let e = r.errors.unwrap();
        assert!(e.max < 0.06 && (e.dx - 0.05).abs() < 1e-6, "{e:?}");
        assert_eq!(r.window.missed, 0);
        assert_eq!(r.window.extra, 0);
        // 6 s at 120 Hz, less the frames before the delay was first fitted.
        assert!(r.window.frames > 600, "{:?}", r.window);
    }

    #[test]
    fn markers_at_the_edge_are_counted_apart() {
        let m = Marker {
            id: 1,
            u: 2.0,
            v: 400.0,
            sigma: 1.0,
            speed: 0.0,
        };
        assert_eq!(m.place(1280, 800), Place::Edge);
        assert_eq!(Marker { u: -10.0, ..m }.place(1280, 800), Place::Outside);
        assert_eq!(Marker { u: 3.0, ..m }.place(1280, 800), Place::Inside);
    }
}
