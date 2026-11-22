//! Calibration: where the page's screen lands on the sensor, in two steps.
//!
//! 1. **Gray code.** With automatic exposure paused and the exposure set to the mode's longest
//!    at gain 1 (so each frame integrates whole refreshes of a backlight driven by PWM, without
//!    bands from the rolling shutter), the page shows white, black, then each bit of the Gray
//!    code of the screen column and row, from the coarsest, with its inverse; each pattern is
//!    the mean of [`PATTERN_FRAMES`] frames. Each pixel that sees the screen (white brighter
//!    than black by [`CONTRAST_MIN`]) decodes the bits it resolves, from the top down to the
//!    first unclear one (the finest stripes blur to grey on the sensor). Each axis keeps the
//!    bits most pixels resolve ([`GrayDecoder::precision`]); a pixel then sees the middle of a
//!    band of screen pixels. A homography fitted to the pixels (outliers dropped) is the coarse
//!    model: the bands' quantisation averages out over thousands of pixels.
//! 2. **Dots.** Exposure back as it was, the page shows a grid of Gaussian dots over the
//!    visible area; the board's detector measures them over [`DOT_FRAMES`] frames, each dot
//!    matched to the blob nearest to where the coarse model puts it. The model (homography and
//!    radial distortion, [`Model`]) is fitted to them by Levenberg–Marquardt.
//!
//! The steps are paced by the page's [`FromPage::Shown`](super::link::FromPage::Shown): a frame
//! counts for a pattern only when its exposure started [`SETTLE_US`] after the page drew it.

use std::collections::VecDeque;

use nalgebra::{SMatrix, SVector, SymmetricEigen};

use super::{
    link::{Axis, Content, Dot, FWHM_PER_SIGMA},
    model::{self, Model},
};

/// Brightest-to-darkest difference (8-bit) for a pixel to see the screen.
pub const CONTRAST_MIN: u8 = 24;
/// Frames averaged per Gray code pattern.
pub const PATTERN_FRAMES: u16 = 4;
/// Share of the pixels that see the screen which must resolve a bit for an axis to use it.
pub const RESOLVED_SHARE: f64 = 0.6;
/// From the page drawing a pattern to the start of an exposure that may see it, µs: the
/// display's latency and the panel's response.
pub const SETTLE_US: f64 = 150_000.0;
/// After the page drew the first group of dots, before they are measured (automatic exposure
/// settles), µs; after each next group.
pub const DOTS_SETTLE_US: f64 = 1_500_000.0;
pub const GROUP_SETTLE_US: f64 = 250_000.0;
/// A step not done this long after its pattern was sent fails, µs.
pub const STEP_TIMEOUT_US: f64 = 5_000_000.0;
/// Frames each group of dots is measured over.
pub const DOT_FRAMES: u32 = 20;
/// Of which a dot must be found in at least this many.
pub const DOT_FOUND_MIN: u32 = 15;
/// Smallest distance between the dots on the sensor, pixels (wider until at most
/// [`DOTS_MAX`] fit), and from the dots to the edges.
pub const DOT_SPACING: f64 = 64.0;
pub const DOT_INSET: f64 = 40.0;
/// Most dots, for the calibration's time: each group takes about half a second.
pub const DOTS_MAX: usize = 96;
/// Dots shown at once: the board measures each blob of a frame on core 1 after the frame,
/// and with many it falls behind the next frame's strips (which it then cuts).
pub const DOTS_PER_GROUP: usize = 8;
/// A dot's size on the sensor, FWHM in pixels.
pub const DOT_FWHM: f64 = 8.0;
/// Furthest a dot's blob may be from where the coarse model puts it, pixels.
pub const DOT_GATE: f64 = 24.0;

/// The result of a calibration.
#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    pub model: Model,
    /// The page's viewport it holds for, CSS pixels.
    pub page: (f64, f64),
    /// The sensor window, pixels.
    pub sensor: (u16, u16),
    /// Dots fitted (of those shown; dropped as off), and their residual on the sensor: RMS and
    /// largest, pixels.
    pub dots: usize,
    pub dots_shown: usize,
    pub dots_dropped: usize,
    /// Farthest fitted dot from the distortion's centre, undistorted sensor pixels
    /// ([`Model::project`]): the model holds within.
    pub reach: f64,
    pub rms: f64,
    pub max: f64,
    /// The coarse model's residual against the Gray code, RMS, pixels (the bands'
    /// quantisation included).
    pub coarse_rms: f64,
    /// The Gray code bits used down to, x and y: a band of 2^bit screen pixels.
    pub bits: [u8; 2],
    /// Share of the sensor's pixels that see the screen, and that decoded the Gray code.
    pub visible: f64,
    pub decoded: f64,
    /// The screen's part seen, CSS pixels: min x, min y, max x, max y.
    pub seen: [f64; 4],
    /// Sensor pixels per screen pixel and the screen's rotation, at the sensor's centre.
    pub scale: f64,
    pub rotation: f64,
}

/// The Gray code decoder: per sensor pixel, the white and black levels and the bits so far.
pub struct GrayDecoder {
    width: usize,
    height: usize,
    white: Vec<u8>,
    black: Vec<u8>,
    /// The pattern of the bit being decoded, until its inverse comes.
    pattern: Vec<u8>,
    /// Per axis: the bits decoded so far, and the lowest bit down to which every bit decoded
    /// clearly (bits are decoded from the top; [`NOT_YET`] before the first).
    code: [Vec<u16>; 2],
    lowest: [Vec<u8>; 2],
    /// Per axis: the clear bits still decoding (no unclear bit yet).
    clear: [Vec<bool>; 2],
    /// The pixel sees the screen.
    sees: Vec<bool>,
    /// Per axis: the bit the bits are used down to, once chosen.
    precision: [u8; 2],
}

/// `lowest` before the first bit.
const NOT_YET: u8 = 16;

impl GrayDecoder {
    pub fn new(width: u16, height: u16) -> Self {
        let n = usize::from(width) * usize::from(height);
        Self {
            width: usize::from(width),
            height: usize::from(height),
            white: vec![0; n],
            black: vec![0; n],
            pattern: vec![0; n],
            code: [vec![0; n], vec![0; n]],
            lowest: [vec![NOT_YET; n], vec![NOT_YET; n]],
            clear: [vec![true; n], vec![true; n]],
            sees: vec![false; n],
            precision: [0, 0],
        }
    }

    pub fn white(&mut self, px: &[u8]) {
        self.white.copy_from_slice(px);
    }

    /// The black frame: marks the pixels that see the screen.
    pub fn black(&mut self, px: &[u8]) {
        self.black.copy_from_slice(px);
        for ((g, &w), &b) in self.sees.iter_mut().zip(&self.white).zip(&self.black) {
            *g = w >= b.saturating_add(CONTRAST_MIN);
        }
    }

    pub fn pattern(&mut self, px: &[u8]) {
        self.pattern.copy_from_slice(px);
    }

    /// The inverse of the last pattern, bit `bit` of `axis` (bits come from the top): sets the
    /// bit where the pattern is the brighter; where the two differ by less than a quarter of
    /// the pixel's contrast (the stripes' edge, or stripes too fine to resolve) the pixel's
    /// bits of this axis stop here.
    pub fn inverse(&mut self, axis: Axis, bit: u8, px: &[u8]) {
        let a = axis as usize;
        for (i, &inv) in px.iter().enumerate() {
            if !self.sees[i] || !self.clear[a][i] {
                continue;
            }
            let (p, q) = (i16::from(self.pattern[i]), i16::from(inv));
            let contrast = i16::from(self.white[i]) - i16::from(self.black[i]);
            if (p - q).abs() * 4 < contrast {
                self.clear[a][i] = false;
            } else {
                if p > q {
                    self.code[a][i] |= 1 << bit;
                }
                self.lowest[a][i] = bit;
            }
        }
    }

    /// Chooses each axis's precision: the finest bit that [`RESOLVED_SHARE`] of the pixels that
    /// see the screen resolve down to. Returns them.
    pub fn choose_precision(&mut self) -> [u8; 2] {
        for a in 0..2 {
            let mut counts = [0usize; NOT_YET as usize + 1];
            let mut seen = 0usize;
            for (i, &s) in self.sees.iter().enumerate() {
                if s {
                    seen += 1;
                    counts[usize::from(self.lowest[a][i])] += 1;
                }
            }
            let need = (seen as f64 * RESOLVED_SHARE).ceil() as usize;
            let mut sum = 0;
            self.precision[a] = NOT_YET;
            for (bit, c) in counts.iter().enumerate() {
                sum += c;
                if sum >= need.max(1) {
                    self.precision[a] = bit as u8;
                    break;
                }
            }
        }
        self.precision
    }

    /// The precision chosen per axis: bits below it are not used.
    pub fn precision(&self) -> [u8; 2] {
        self.precision
    }

    /// Sensor pixels that see the screen.
    pub fn seeing(&self) -> usize {
        self.sees.iter().filter(|&&g| g).count()
    }

    /// The pixel sees the screen and resolves both axes to their precision.
    fn good(&self, i: usize) -> bool {
        self.sees[i] && (0..2).all(|a| self.lowest[a][i] <= self.precision[a])
    }

    /// Pixels that decoded both axes to their precision.
    pub fn decoded(&self) -> usize {
        (0..self.sees.len()).filter(|&i| self.good(i)).count()
    }

    /// The screen point seen by sensor pixel `(x, y)` (the middle of its band at the chosen
    /// precision), if it decoded.
    pub fn at(&self, x: usize, y: usize) -> Option<(f64, f64)> {
        let i = y * self.width + x;
        self.good(i).then(|| {
            let c = |a: usize| {
                let k = self.precision[a];
                // The binary bits at and above `k` come from the Gray bits at and above `k`.
                let b = gray_to_binary(self.code[a][i] >> k) << k;
                f64::from(b) + f64::from(1u16 << k) / 2.0
            };
            (c(0), c(1))
        })
    }

    /// Of the pixels within `r` of `(x, y)`, the share that decoded.
    pub fn coverage(&self, x: usize, y: usize, r: usize) -> f64 {
        let (x0, x1) = (x.saturating_sub(r), (x + r).min(self.width - 1));
        let (y0, y1) = (y.saturating_sub(r), (y + r).min(self.height - 1));
        let mut n = 0;
        let mut good = 0;
        for yy in y0..=y1 {
            for xx in x0..=x1 {
                n += 1;
                good += usize::from(self.good(yy * self.width + xx));
            }
        }
        good as f64 / f64::from(n.max(1) as u32)
    }

    /// Screen ↔ sensor pairs of every `step`-th pixel in each direction: `(x, y, u, v)`.
    pub fn pairs(&self, step: usize) -> Vec<[f64; 4]> {
        let mut out = Vec::new();
        for y in (0..self.height).step_by(step) {
            for x in (0..self.width).step_by(step) {
                if let Some((sx, sy)) = self.at(x, y) {
                    out.push([sx, sy, x as f64, y as f64]);
                }
            }
        }
        out
    }
}

/// The binary value of Gray code `g`.
pub fn gray_to_binary(mut g: u16) -> u16 {
    let mut b = g;
    while g > 0 {
        g >>= 1;
        b ^= g;
    }
    b
}

/// Bits of the Gray code that number `n` pixels.
pub fn bits_for(n: f64) -> u8 {
    let n = n.ceil().max(2.0) as u32;
    (32 - (n - 1).leading_zeros()) as u8
}

/// Similarity that moves points to their centroid at a mean distance of √2 (Hartley), as a
/// 3x3 row-major matrix, and its inverse.
fn normaliser(points: impl Iterator<Item = (f64, f64)> + Clone) -> ([f64; 9], [f64; 9]) {
    let n = points.clone().count().max(1) as f64;
    let (mx, my) = points
        .clone()
        .fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
    let (mx, my) = (mx / n, my / n);
    let d = points
        .map(|(x, y)| ((x - mx).powi(2) + (y - my).powi(2)).sqrt())
        .sum::<f64>()
        / n;
    let s = if d > 0.0 { 2f64.sqrt() / d } else { 1.0 };
    (
        [s, 0.0, -s * mx, 0.0, s, -s * my, 0.0, 0.0, 1.0],
        [1.0 / s, 0.0, mx, 0.0, 1.0 / s, my, 0.0, 0.0, 1.0],
    )
}

fn mul(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    std::array::from_fn(|i| {
        let (r, c) = (i / 3, i % 3);
        (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum()
    })
}

/// The homography taking each `(x, y)` to its `(u, v)` (normalised DLT), `None` with fewer
/// than four pairs or a degenerate set.
pub fn fit_homography(pairs: &[[f64; 4]]) -> Option<[f64; 9]> {
    if pairs.len() < 4 {
        return None;
    }
    let (ts, _) = normaliser(pairs.iter().map(|p| (p[0], p[1])));
    let (td, td_inv) = normaliser(pairs.iter().map(|p| (p[2], p[3])));
    let mut ata = SMatrix::<f64, 9, 9>::zeros();
    for p in pairs {
        let (x, y) = model::apply(&ts, p[0], p[1]);
        let (u, v) = model::apply(&td, p[2], p[3]);
        let rows = [
            SVector::<f64, 9>::from([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, -u]),
            SVector::<f64, 9>::from([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, -v]),
        ];
        for r in rows {
            ata += r * r.transpose();
        }
    }
    let eig = SymmetricEigen::new(ata);
    let (i, _) = eig
        .eigenvalues
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))?;
    let h = eig.eigenvectors.column(i);
    let hn: [f64; 9] = std::array::from_fn(|k| h[k]);
    if hn[8].abs() < 1e-12 && hn.iter().all(|v| v.abs() < 1e-12) {
        return None;
    }
    let full = mul(&td_inv, &mul(&hn, &ts));
    (full[8].abs() > 1e-15).then(|| model::normalised(full))
}

/// Residuals of `h` on `pairs`, pixels.
fn residuals(h: &[f64; 9], pairs: &[[f64; 4]]) -> Vec<f64> {
    pairs
        .iter()
        .map(|p| {
            let (u, v) = model::apply(h, p[0], p[1]);
            ((u - p[2]).powi(2) + (v - p[3]).powi(2)).sqrt()
        })
        .collect()
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mid = v.len() / 2;
    *v.select_nth_unstable_by(mid, f64::total_cmp).1
}

/// A homography fitted to the Gray code's pairs, dropping outliers (a pixel decoded wrongly):
/// three rounds keeping the pairs within max(4 px, 3 x the median residual). Its RMS residual.
pub fn fit_coarse(pairs: &[[f64; 4]]) -> Option<([f64; 9], f64)> {
    let mut kept = pairs.to_vec();
    let mut h = fit_homography(&kept)?;
    for _ in 0..3 {
        let mut r = residuals(&h, pairs);
        let gate = (3.0 * median(&mut r)).max(4.0);
        kept = pairs
            .iter()
            .zip(residuals(&h, pairs))
            .filter(|(_, r)| *r <= gate)
            .map(|(p, _)| *p)
            .collect();
        h = fit_homography(&kept)?;
    }
    let r = residuals(&h, &kept);
    let rms = (r.iter().map(|r| r * r).sum::<f64>() / r.len().max(1) as f64).sqrt();
    Some((h, rms))
}

/// Fitted dots must reach this far from the sensor's centre (over [`Model::norm`]) for the
/// radial distortion to be fitted; nearer, it is left at none (it would only be extrapolated).
pub const DISTORTION_REACH: f64 = 0.55;

/// The model fitted to the dots `pairs` (screen `x, y` → sensor `u, v`): [`fit_model`], then
/// again without the dots off by more than max(0.5 px, 4 x the median residual) (a dot matched
/// to the wrong blob). The model, the residuals of the dots kept, and how many were dropped.
pub fn fit_dots(pairs: &[[f64; 4]], width: u16, height: u16) -> Option<(Model, Vec<f64>, usize)> {
    let (m, r) = fit_model(pairs, width, height)?;
    let mut sorted = r.clone();
    let gate = (4.0 * median(&mut sorted)).max(0.5);
    let kept: Vec<[f64; 4]> = pairs
        .iter()
        .zip(&r)
        .filter(|(_, r)| **r <= gate)
        .map(|(p, _)| *p)
        .collect();
    if kept.len() == pairs.len() || kept.len() < 6 {
        return Some((m, r, 0));
    }
    let (m, r) = fit_model(&kept, width, height)?;
    Some((m, r, pairs.len() - kept.len()))
}

/// The model (homography and radial distortion) fitted to `pairs` (screen `x, y` → sensor
/// `u, v`) by Levenberg–Marquardt from the homography alone, on a sensor of `width` x
/// `height`; the distortion only when the dots reach [`DISTORTION_REACH`]. Its residuals,
/// pixels.
pub fn fit_model(pairs: &[[f64; 4]], width: u16, height: u16) -> Option<(Model, Vec<f64>)> {
    let h0 = fit_homography(pairs)?;
    let base = Model::homography(h0, width, height);
    let reach = pairs
        .iter()
        .map(|p| (p[2] - base.cx).hypot(p[3] - base.cy) / base.norm)
        .fold(0.0, f64::max);
    let distortion = reach >= DISTORTION_REACH;
    // The screen side normalised, so the parameters have comparable effects.
    let (ts, _) = normaliser(pairs.iter().map(|p| (p[0], p[1])));
    let (_, ts_inv) = normaliser(pairs.iter().map(|p| (p[0], p[1])));
    let hn = model::normalised(mul(&h0, &ts_inv));
    let pts: Vec<[f64; 4]> = pairs
        .iter()
        .map(|p| {
            let (x, y) = model::apply(&ts, p[0], p[1]);
            [x, y, p[2], p[3]]
        })
        .collect();
    let to_model = |p: &SVector<f64, 10>| {
        let h: [f64; 9] = std::array::from_fn(|i| if i < 8 { p[i] } else { 1.0 });
        Model {
            h,
            k1: p[8],
            k2: p[9],
            ..base
        }
    };
    let mut p = SVector::<f64, 10>::zeros();
    for i in 0..8 {
        p[i] = hn[i];
    }
    let cost = |p: &SVector<f64, 10>| {
        let m = to_model(p);
        pts.iter()
            .map(|q| {
                let (u, v) = m.to_sensor(q[0], q[1]);
                (u - q[2]).powi(2) + (v - q[3]).powi(2)
            })
            .sum::<f64>()
    };
    let mut c = cost(&p);
    let mut lambda = 1e-3;
    for _ in 0..200 {
        let m = to_model(&p);
        let mut jtj = SMatrix::<f64, 10, 10>::zeros();
        let mut jtr = SVector::<f64, 10>::zeros();
        for q in &pts {
            let (j, r) = jacobian(&m, q);
            for (row, res) in j.iter().zip(r) {
                jtj += row * row.transpose();
                jtr += row * res;
            }
        }
        if !distortion {
            // k1, k2 held at 0: their rows and columns out, a unit diagonal so the step is 0.
            for k in 8..10 {
                for i in 0..10 {
                    jtj[(k, i)] = 0.0;
                    jtj[(i, k)] = 0.0;
                }
                jtj[(k, k)] = 1.0;
                jtr[k] = 0.0;
            }
        }
        let mut improved = false;
        for _ in 0..10 {
            let mut a = jtj;
            for i in 0..10 {
                a[(i, i)] += lambda * jtj[(i, i)].max(1e-12);
            }
            let Some(step) = a.lu().solve(&(-jtr)) else {
                lambda *= 10.0;
                continue;
            };
            let np = p + step;
            let nc = cost(&np);
            if nc < c {
                let done = (c - nc) < 1e-12 * c.max(1e-30);
                (p, c, lambda) = (np, nc, (lambda / 10.0).max(1e-12));
                improved = !done;
                break;
            }
            lambda *= 10.0;
        }
        if !improved {
            break;
        }
    }
    let fitted = to_model(&p);
    let model = Model {
        h: model::normalised(mul(&fitted.h, &ts)),
        ..fitted
    };
    let r = pairs
        .iter()
        .map(|q| {
            let (u, v) = model.to_sensor(q[0], q[1]);
            ((u - q[2]).powi(2) + (v - q[3]).powi(2)).sqrt()
        })
        .collect();
    Some((model, r))
}

/// The two residuals (sensor u, v minus the measured) of `q` and their gradients over the
/// parameters (h0..h7, k1, k2).
fn jacobian(m: &Model, q: &[f64; 4]) -> ([SVector<f64, 10>; 2], [f64; 2]) {
    let (x, y) = (q[0], q[1]);
    let h = &m.h;
    let w = h[6] * x + h[7] * y + 1.0;
    let (uu, vv) = (
        (h[0] * x + h[1] * y + h[2]) / w,
        (h[3] * x + h[4] * y + h[5]) / w,
    );
    let mut du_dh = SVector::<f64, 10>::zeros();
    let mut dv_dh = SVector::<f64, 10>::zeros();
    du_dh[0] = x / w;
    du_dh[1] = y / w;
    du_dh[2] = 1.0 / w;
    du_dh[6] = -uu * x / w;
    du_dh[7] = -uu * y / w;
    dv_dh[3] = x / w;
    dv_dh[4] = y / w;
    dv_dh[5] = 1.0 / w;
    dv_dh[6] = -vv * x / w;
    dv_dh[7] = -vv * y / w;
    let (du, dv) = (uu - m.cx, vv - m.cy);
    let n2 = m.norm * m.norm;
    let q2 = (du * du + dv * dv) / n2;
    let f = 1.0 + m.k1 * q2 + m.k2 * q2 * q2;
    let fp = m.k1 + 2.0 * m.k2 * q2;
    let (a, b) = (f + du * fp * 2.0 * du / n2, du * fp * 2.0 * dv / n2);
    let (c, d) = (dv * fp * 2.0 * du / n2, f + dv * fp * 2.0 * dv / n2);
    let mut ju = du_dh * a + dv_dh * b;
    let mut jv = du_dh * c + dv_dh * d;
    ju[8] = du * q2;
    ju[9] = du * q2 * q2;
    jv[8] = dv * q2;
    jv[9] = dv * q2 * q2;
    let (su, sv) = (m.cx + du * f, m.cy + dv * f);
    ([ju, jv], [su - q[2], sv - q[3]])
}

/// A step of the calibration: what the page shows, for what.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Purpose {
    White,
    Black,
    Pattern(Axis, u8),
    Inverse(Axis, u8),
    Dots,
}

impl Purpose {
    fn content(self) -> Content {
        match self {
            Self::White => Content::White,
            Self::Black => Content::Black,
            Self::Pattern(axis, bit) => Content::Gray {
                axis,
                bit,
                inverse: false,
            },
            Self::Inverse(axis, bit) => Content::Gray {
                axis,
                bit,
                inverse: true,
            },
            Self::Dots => unreachable!("the dots are planned from the coarse model"),
        }
    }
}

/// What the calibration needs done next.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Show this on the page, then report [`Calibration::sent`] with its id.
    Show(Content),
    /// Set the exposure for the Gray code: automatic exposure off, the mode's longest
    /// exposure, gain 1.
    PatternExposure,
    /// Put the camera's settings back as they were before the calibration.
    RestoreExposure,
    /// Detect the dots: a manual threshold low enough for the dimmest (a lens's vignetting
    /// dims the edges), from the board's background.
    DotDetection,
    /// Put the detection settings back as they were before the calibration.
    RestoreDetection,
}

/// Where the calibration is.
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    Act(Action),
    Wait,
    Done(Result<Fit, String>),
}

struct Active {
    purpose: Purpose,
    id: Option<u32>,
    sent_us: f64,
    shown_us: Option<f64>,
}

struct DotPlan {
    css: (f64, f64),
    predicted: (f64, f64),
    sum: (f64, f64),
    found: u32,
}

/// A calibration in progress.
pub struct Calibration {
    sensor: (u16, u16),
    page: (f64, f64),
    /// The camera's exposure is the Gray code's ([`Action::PatternExposure`]): to restore.
    exposure_set: bool,
    /// The detection settings are the dots' ([`Action::DotDetection`]): to restore.
    detection_set: bool,
    actions: VecDeque<Action>,
    steps: VecDeque<Purpose>,
    total: usize,
    active: Option<Active>,
    decoder: GrayDecoder,
    /// The current pattern's frames summed, and how many.
    sum: Vec<u16>,
    summed: u16,
    coarse: Option<([f64; 9], f64)>,
    dots: Vec<DotPlan>,
    dot_frames: u32,
    /// The dots shown in `groups` groups; the one shown now.
    group: usize,
    groups: usize,
    coarse_model: Option<Model>,
    result: Option<Result<Fit, String>>,
}

impl Calibration {
    /// A calibration of a sensor window `sensor` against a page `page` (CSS pixels).
    pub fn new(sensor: (u16, u16), page: (f64, f64)) -> Self {
        let mut steps = VecDeque::from([Purpose::White, Purpose::Black]);
        for (axis, n) in [(Axis::X, page.0), (Axis::Y, page.1)] {
            for bit in (0..bits_for(n)).rev() {
                steps.push_back(Purpose::Pattern(axis, bit));
                steps.push_back(Purpose::Inverse(axis, bit));
            }
        }
        steps.push_back(Purpose::Dots);
        Self {
            sensor,
            page,
            exposure_set: true,
            detection_set: false,
            actions: VecDeque::from([Action::PatternExposure]),
            total: steps.len(),
            steps,
            active: None,
            decoder: GrayDecoder::new(sensor.0, sensor.1),
            sum: Vec::new(),
            summed: 0,
            coarse: None,
            dots: Vec::new(),
            dot_frames: 0,
            group: 0,
            groups: 1,
            coarse_model: None,
            result: None,
        }
    }

    /// Steps done and in all, and what is being done.
    pub fn progress(&self) -> (usize, usize, String) {
        let done = self.total - self.steps.len() - usize::from(self.active.is_some());
        let what = match self.active.as_ref().map(|a| a.purpose) {
            None | Some(Purpose::White | Purpose::Black) => "Screen levels".to_owned(),
            Some(Purpose::Pattern(a, b) | Purpose::Inverse(a, b)) => {
                format!("Gray code {a:?} bit {b}")
            }
            Some(Purpose::Dots) => format!(
                "Dots: group {} of {} ({} dots in all), frame {} of {DOT_FRAMES}",
                self.group + 1,
                self.groups,
                self.dots.len(),
                self.dot_frames
            ),
        };
        (done, self.total, what)
    }

    /// The pairs of the Gray code's pixels that decoded clearly (for the tests and the view).
    pub fn decoder(&self) -> &GrayDecoder {
        &self.decoder
    }

    fn fail(&mut self, why: String) -> Poll {
        let r = Err(why);
        self.result = Some(r.clone());
        // The camera and the detection as they were, and the page dark, even when failing.
        if self.exposure_set {
            self.exposure_set = false;
            self.actions.push_back(Action::RestoreExposure);
        }
        if self.detection_set {
            self.detection_set = false;
            self.actions.push_back(Action::RestoreDetection);
        }
        self.actions.push_back(Action::Show(Content::Black));
        Poll::Done(r)
    }

    /// What to do now, `now_us` on the viewer's clock.
    pub fn poll(&mut self, now_us: f64) -> Poll {
        if let Some(a) = self.actions.pop_front() {
            return Poll::Act(a);
        }
        if let Some(r) = &self.result {
            return Poll::Done(r.clone());
        }
        if let Some(a) = &self.active {
            if now_us - a.sent_us
                > STEP_TIMEOUT_US + DOTS_SETTLE_US * f64::from(a.purpose == Purpose::Dots)
            {
                let what = if a.shown_us.is_none() {
                    "the page did not show the pattern"
                } else {
                    "no frame came after the page showed the pattern (frames on?)"
                };
                return self.fail(format!("{}: {what}", self.progress().2));
            }
            return Poll::Wait;
        }
        let Some(purpose) = self.steps.pop_front() else {
            return Poll::Wait;
        };
        let content = if purpose == Purpose::Dots {
            match self.plan_dots() {
                Ok(c) => c,
                Err(e) => return self.fail(e),
            }
        } else {
            purpose.content()
        };
        self.active = Some(Active {
            purpose,
            id: None,
            sent_us: now_us,
            shown_us: None,
        });
        if purpose == Purpose::Dots {
            if self.exposure_set {
                self.exposure_set = false;
                self.actions.push_back(Action::RestoreExposure);
            }
            self.detection_set = true;
            self.actions.push_back(Action::DotDetection);
            self.actions.push_back(Action::Show(content));
            return self.poll(now_us);
        }
        Poll::Act(Action::Show(content))
    }

    /// Stops here; automatic exposure goes back on if it was paused.
    pub fn cancel(&mut self) {
        if self.result.is_none() {
            self.active = None;
            self.steps.clear();
            self.fail("cancelled".into());
        }
    }

    /// The last [`Action::Show`] went out as `id`.
    pub fn sent(&mut self, id: u32) {
        if let Some(a) = &mut self.active {
            a.id = Some(id);
        }
    }

    /// The page drew `id` at `at_us`.
    pub fn shown(&mut self, id: u32, at_us: f64) {
        if let Some(a) = &mut self.active
            && a.id == Some(id)
        {
            a.shown_us = Some(at_us);
        }
    }

    /// The step's pattern is on the screen for an exposure starting at `start_us`.
    fn ready(&self, start_us: f64, settle: f64) -> bool {
        self.active
            .as_ref()
            .and_then(|a| a.shown_us)
            .is_some_and(|s| start_us >= s + settle)
    }

    /// A frame (RAW8, the sensor window's size) whose exposure started at `start_us`.
    pub fn frame(&mut self, start_us: f64, width: u16, height: u16, px: &[u8]) {
        let Some(purpose) = self.active.as_ref().map(|a| a.purpose) else {
            return;
        };
        if purpose == Purpose::Dots || !self.ready(start_us, SETTLE_US) {
            return;
        }
        if (width, height) != self.sensor || px.len() != usize::from(width) * usize::from(height) {
            self.fail(format!(
                "the frames changed size ({width}x{height}): calibrate again"
            ));
            return;
        }
        // The mean of the pattern's frames.
        if self.summed == 0 {
            self.sum.clear();
            self.sum.extend(px.iter().map(|&v| u16::from(v)));
        } else {
            for (s, &v) in self.sum.iter_mut().zip(px) {
                *s += u16::from(v);
            }
        }
        self.summed += 1;
        if self.summed < PATTERN_FRAMES {
            return;
        }
        let n = self.summed;
        self.summed = 0;
        let mean: Vec<u8> = self.sum.iter().map(|&s| ((s + n / 2) / n) as u8).collect();
        match purpose {
            Purpose::White => self.decoder.white(&mean),
            Purpose::Black => self.decoder.black(&mean),
            Purpose::Pattern(..) => self.decoder.pattern(&mean),
            Purpose::Inverse(axis, bit) => self.decoder.inverse(axis, bit, &mean),
            Purpose::Dots => unreachable!(),
        }
        self.active = None;
        if purpose == Purpose::Black && self.decoder.seeing() == 0 {
            self.fail(
                "no pixel sees the screen: white is not brighter than black by \
                 at least 24 levels"
                    .into(),
            );
        }
    }

    /// The Gray code decoded: the coarse model, and the dots over the visible area.
    fn plan_dots(&mut self) -> Result<Content, String> {
        let [px, py] = self.decoder.choose_precision();
        let pairs = self.decoder.pairs(3);
        if pairs.len() < 200 {
            return Err(format!(
                "too few pixels decoded the Gray code ({}; {} see the screen, bits used down \
                 to {px} in x and {py} in y)",
                pairs.len(),
                self.decoder.seeing()
            ));
        }
        let (h, rms) = fit_coarse(&pairs).ok_or("the Gray code gave no homography")?;
        self.coarse = Some((h, rms));
        let coarse = Model::homography(h, self.sensor.0, self.sensor.1);
        let (w, hh) = (f64::from(self.sensor.0), f64::from(self.sensor.1));
        let mut spacing = DOT_SPACING;
        let dots = loop {
            let mut dots = Vec::new();
            let mut v = DOT_INSET;
            while v <= hh - 1.0 - DOT_INSET {
                let mut u = DOT_INSET;
                while u <= w - 1.0 - DOT_INSET {
                    let (x, y) = coarse.to_screen(u, v);
                    let inside =
                        x >= 4.0 && y >= 4.0 && x <= self.page.0 - 4.0 && y <= self.page.1 - 4.0;
                    if inside && self.decoder.coverage(u as usize, v as usize, 6) > 0.3 {
                        dots.push(DotPlan {
                            css: (x, y),
                            predicted: (u, v),
                            sum: (0.0, 0.0),
                            found: 0,
                        });
                    }
                    u += spacing;
                }
                v += spacing;
            }
            if dots.len() <= DOTS_MAX {
                break dots;
            }
            spacing += 8.0;
        };
        if dots.len() < 6 {
            return Err(format!(
                "the visible area holds too few dots ({}): is the screen in view?",
                dots.len()
            ));
        }
        self.groups = dots.len().div_ceil(DOTS_PER_GROUP);
        self.group = 0;
        self.coarse_model = Some(coarse);
        self.dots = dots;
        Ok(self.group_content())
    }

    /// The current group of dots: every `groups`-th dot, so each group spreads over the view.
    fn group_content(&self) -> Content {
        let coarse = self.coarse_model.expect("planned");
        Content::Dots {
            dots: self
                .dots
                .iter()
                .skip(self.group)
                .step_by(self.groups)
                .map(|d| Dot {
                    x: d.css.0,
                    y: d.css.1,
                    sigma: DOT_FWHM / FWHM_PER_SIGMA / coarse.scale_at(d.css.0, d.css.1),
                })
                .collect(),
        }
    }

    /// A detected frame whose exposure started at `start_us`: its blobs' centres.
    pub fn blobs(&mut self, start_us: f64, blobs: &[(f64, f64)]) {
        let settle = if self.group == 0 {
            DOTS_SETTLE_US
        } else {
            GROUP_SETTLE_US
        };
        if self.active.as_ref().map(|a| a.purpose) != Some(Purpose::Dots)
            || !self.ready(start_us, settle)
        {
            return;
        }
        for d in self.dots.iter_mut().skip(self.group).step_by(self.groups) {
            let near = blobs
                .iter()
                .map(|b| (b, (b.0 - d.predicted.0).hypot(b.1 - d.predicted.1)))
                .filter(|(_, r)| *r <= DOT_GATE)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((b, _)) = near {
                d.sum = (d.sum.0 + b.0, d.sum.1 + b.1);
                d.found += 1;
            }
        }
        self.dot_frames += 1;
        if self.dot_frames < DOT_FRAMES {
            return;
        }
        self.dot_frames = 0;
        self.group += 1;
        if self.group < self.groups {
            self.active = Some(Active {
                purpose: Purpose::Dots,
                id: None,
                sent_us: start_us,
                shown_us: None,
            });
            self.actions.push_back(Action::Show(self.group_content()));
            return;
        }
        self.active = None;
        if let Err(e) = self.finish() {
            self.fail(e);
        }
    }

    fn finish(&mut self) -> Result<(), String> {
        let pairs: Vec<[f64; 4]> = self
            .dots
            .iter()
            .filter(|d| d.found >= DOT_FOUND_MIN)
            .map(|d| {
                let n = f64::from(d.found);
                [d.css.0, d.css.1, d.sum.0 / n, d.sum.1 / n]
            })
            .collect();
        if pairs.len() < 6 {
            return Err(format!(
                "the detector found too few of the {} dots ({} in {DOT_FOUND_MIN} of \
                 {DOT_FRAMES} frames): detection on, threshold?",
                self.dots.len(),
                pairs.len()
            ));
        }
        let (model, r, dropped) =
            fit_dots(&pairs, self.sensor.0, self.sensor.1).ok_or("the dots gave no model")?;
        let rms = (r.iter().map(|r| r * r).sum::<f64>() / r.len() as f64).sqrt();
        let max = r.iter().copied().fold(0.0, f64::max);
        let reach = pairs
            .iter()
            .map(|p| {
                let (u, v) = model.undistort(p[2], p[3]);
                (u - model.cx).hypot(v - model.cy)
            })
            .fold(0.0, f64::max);
        let all = usize::from(self.sensor.0) * usize::from(self.sensor.1);
        let mut seen = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for p in self.decoder.pairs(4) {
            seen = [
                seen[0].min(p[0] - 0.5),
                seen[1].min(p[1] - 0.5),
                seen[2].max(p[0] + 0.5),
                seen[3].max(p[1] + 0.5),
            ];
        }
        let (cx, cy) = model.to_screen(model.cx, model.cy);
        self.result = Some(Ok(Fit {
            model,
            page: self.page,
            sensor: self.sensor,
            dots: pairs.len() - dropped,
            dots_shown: self.dots.len(),
            dots_dropped: dropped,
            reach,
            rms,
            max,
            coarse_rms: self.coarse.map_or(0.0, |c| c.1),
            bits: self.decoder.precision(),
            visible: self.decoder.seeing() as f64 / all as f64,
            decoded: self.decoder.decoded() as f64 / all as f64,
            seen,
            scale: model.scale_at(cx, cy),
            rotation: model.rotation_at(cx, cy),
        }));
        // The detection as it was, and the page dark again.
        if self.detection_set {
            self.detection_set = false;
            self.actions.push_back(Action::RestoreDetection);
        }
        self.actions.push_back(Action::Show(Content::Black));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen_target::model::tests::sample;

    #[test]
    fn gray_code_round_trips() {
        for c in 0..2048u16 {
            assert_eq!(gray_to_binary(c ^ (c >> 1)), c);
        }
        assert_eq!(bits_for(1180.0), 11);
        assert_eq!(bits_for(1024.0), 10);
        assert_eq!(bits_for(820.0), 10);
    }

    #[test]
    fn the_homography_of_exact_pairs_is_recovered() {
        let m = Model {
            k1: 0.0,
            k2: 0.0,
            ..sample()
        };
        let pairs: Vec<[f64; 4]> = (0..50)
            .map(|i| {
                let (x, y) = (
                    100.0 + 9.0 * f64::from(i % 10),
                    300.0 + 31.0 * f64::from(i / 10),
                );
                let (u, v) = m.to_sensor(x, y);
                [x, y, u, v]
            })
            .collect();
        let h = fit_homography(&pairs).unwrap();
        for (a, b) in h.iter().zip(m.h) {
            assert!(
                (a - b).abs() <= 1e-6 * b.abs().max(1e-3),
                "{h:?} vs {:?}",
                m.h
            );
        }
    }

    #[test]
    fn the_model_with_distortion_is_recovered_from_noisy_dots() {
        let m = sample();
        let mut s = 7u64;
        let mut noise = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let mut pairs = Vec::new();
        for v in (40..760).step_by(64) {
            for u in (40..1240).step_by(64) {
                let (x, y) = m.to_screen(f64::from(u), f64::from(v));
                let (uu, vv) = m.to_sensor(x, y);
                pairs.push([x, y, uu + 0.1 * noise(), vv + 0.1 * noise()]);
            }
        }
        let (fit, r) = fit_model(&pairs, 1280, 800).unwrap();
        let rms = (r.iter().map(|r| r * r).sum::<f64>() / r.len() as f64).sqrt();
        assert!(rms < 0.05, "rms {rms}");
        for (u, v) in [(0.0, 0.0), (1279.0, 799.0), (640.0, 400.0), (1279.0, 0.0)] {
            let (x, y) = m.to_screen(u, v);
            let (fu, fv) = fit.to_sensor(x, y);
            assert!((fu - u).hypot(fv - v) < 0.1, "({u}, {v}) -> ({fu}, {fv})");
        }
    }
}
