//! Sub-pixel measurement of each blob on the frame: centre, width and flux, integer math
//! only, `core` only, no heap.
//!
//! The detector's centroid weighs the pixels above the threshold only, so it is pulled
//! towards the pixel centres (0.07 to 0.15 px of pixel locking) and every pixel crossing
//! the threshold jumps it. The threshold only finds the blobs; [`refine_raw8`] and
//! [`refine_raw10`] then measure each one on the frame:
//!
//! 1. Start at the blob's centroid. The window sigma `s` comes from the blob's area: a
//!    Gaussian spot cut at half its peak covers `2 pi ln 2 sigma^2` pixels, so
//!    `s = sqrt(area / (2 pi ln 2))`, clamped to 0.75..=8 px.
//! 2. Local background: the median of the pixels on the perimeter of the square of
//!    half-size `ceil(4 s) + 1` around the start (the pixels of it in the frame; if none,
//!    the border of that square clipped to the frame).
//! 3. Iterated Gaussian-weighted centroid over the square of half-size `ceil(4 s)` around
//!    the current estimate: each pixel weighs `(value - background) * exp(-d^2 / (2 s^2))`,
//!    `d` its distance to the estimate, the difference not clipped (so the noise does not
//!    bias it). Each step moves twice the weighted mean offset (clamped to 1 px per axis):
//!    for a Gaussian spot of the window's width that lands on the centre in one step, and
//!    the centre of any symmetric spot is the fixed point whatever `s`. At most
//!    [`MAX_STEPS`] steps, until a step is below 1/512 px on both axes. A window whose
//!    weights sum to zero or less keeps the start.
//! 4. Over the final window, with the same weights around the final centre: `width` from
//!    the weighted second moment `m = sum(w d^2) / sum(w)` (x and y averaged), `w` the
//!    unclipped `(value - background) * G`, so the noise does not bias it. A Gaussian spot
//!    of sigma `sigma` under the window gives `m = sigma^2 s^2 / (sigma^2 + s^2)`, so
//!    `sigma^2 = m s^2 / (s^2 - m)`, less the 1/12 px^2 that the pixel's own width adds.
//!    For any other profile this is the sigma of the Gaussian with the same weighted
//!    moment. `m <= 0` or weights summing to zero or less give 0; `m >= s^2` (a spot far
//!    wider than the window) gives `u16::MAX`. `flux` is the sum of `value - background`
//!    over the window, unweighted, clamped to 0 and up.
//!
//! Positions are held in 1/4096 px. The Gaussian comes from a table of `exp(-t)`, `t` in
//! steps of 1/32 up to 16, in Q15 with linear interpolation; the table is built at compile
//! time with integer math (`EXP`). Every input is clipped to the frame (blobs at the edge,
//! frames shorter than `width * height`), so no input panics, and host and target give the
//! same result bit for bit.

use crate::{Blob, raw10};

/// Units per pixel of [`Spot::x`], [`Spot::y`] and [`Spot::width`].
pub const SUBPIXEL: u32 = 256;

/// Most centroid steps per blob.
pub const MAX_STEPS: u32 = 8;

/// A blob measured on the frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spot {
    /// Centre column in 1/256 px, pixel centres at integer coordinates (as [`Blob`]).
    pub x: u32,
    /// Centre row in 1/256 px.
    pub y: u32,
    /// Sigma of the spot in 1/256 px (the pixel's own width removed): the Gaussian sigma
    /// with the spot's Gaussian-weighted second moment, so for a non-Gaussian spot the
    /// equivalent Gaussian's. 0 if it cannot be measured, `u16::MAX` if the spot is far
    /// wider than the window.
    pub width: u16,
    /// Sum of `value - background` over the window, at least 0, in frame units (RAW10:
    /// 10-bit).
    pub flux: u32,
}

/// Measures `blob` on a RAW8 frame of `width` x `height` pixels (fewer rows if `frame` is
/// shorter).
pub fn refine_raw8(frame: &[u8], width: u16, height: u16, blob: &Blob) -> Spot {
    let view = View::new(frame, width as usize, width as usize, height, false);
    refine(&view, blob).0
}

/// Measures `blob` on a packed CSI-2 RAW10 frame ([`raw10`], rows of `width / 4 * 5`
/// bytes, `width` a multiple of 4; the pixels of a partial last group are not used) of
/// `height` rows (fewer if `frame` is shorter). Values, background and flux are 10-bit.
pub fn refine_raw10(frame: &[u8], width: u16, height: u16, blob: &Blob) -> Spot {
    let groups = width as usize / 4;
    let view = View::new(frame, groups * raw10::GROUP_BYTES, groups * 4, height, true);
    refine(&view, blob).0
}

/// Fraction bits of positions and sigmas.
const Q: u32 = 12;
/// One pixel.
const ONE: i64 = 1 << Q;
/// Window sigma bounds.
const SIGMA_MIN: i64 = 3 * ONE / 4;
const SIGMA_MAX: i64 = 8 * ONE;
/// Largest window half-size: `ceil(4 * SIGMA_MAX)` px.
const MAX_HALF: usize = 32;
const MAX_SIDE: usize = 2 * MAX_HALF + 1;
/// Pixels on the perimeter of the background square (half-size `MAX_HALF + 1`), at most.
const MAX_RING: usize = 8 * (MAX_HALF + 1);
/// Steps below this (1/512 px) end the iteration.
const STOP: i64 = ONE / 512;

/// Table steps per unit of `t` in [`EXP`].
const EXP_PER_UNIT: usize = 32;
/// Entries of [`EXP`]: `t` from 0 to 16 (`exp(-16)` is below half a Q15 unit).
const EXP_LEN: usize = 16 * EXP_PER_UNIT + 1;

/// `EXP[k] = round(32768 * exp(-k / 32))`. Built at compile time with integer math:
/// `exp(-1/32)` from its Taylor series in Q62, then its powers in Q62, rounded to Q15.
static EXP: [u16; EXP_LEN] = {
    const ONE62: i128 = 1 << 62;
    let mut r = 0i128;
    let mut term = ONE62;
    let mut n = 1;
    while term != 0 {
        r += term;
        term = -term / (EXP_PER_UNIT as i128 * n);
        n += 1;
    }
    let mut table = [0u16; EXP_LEN];
    let mut e = ONE62;
    let mut k = 0;
    while k < EXP_LEN {
        table[k] = ((e + (1 << 46)) >> 47) as u16;
        e = (e * r + (1 << 61)) >> 62;
        k += 1;
    }
    table
};

/// `exp(-d^2 / (2 s^2))` in Q15, `d` in 1/4096 px (`|d| <= 33 px`), `inv = 2^44 / s^2`
/// (`s` in 1/4096 px), so `d^2 * inv >> 32` is `t` in 1/(32 * 256): table index and an
/// 8-bit interpolation fraction.
#[inline(always)]
fn gauss(d: i64, inv: u64) -> i32 {
    let t = ((d * d) as u64 * inv) >> 32;
    let k = (t >> 8) as usize;
    match (EXP.get(k), EXP.get(k + 1)) {
        (Some(&a), Some(&b)) => {
            let f = (t & 255) as i32;
            (a as i32 * (256 - f) + b as i32 * f + 128) >> 8
        }
        _ => 0,
    }
}

/// A frame: rows of `stride` bytes holding `width` pixels, `rows` of them in `data`.
struct View<'a> {
    data: &'a [u8],
    stride: usize,
    width: usize,
    rows: usize,
    raw10: bool,
}

impl<'a> View<'a> {
    fn new(data: &'a [u8], stride: usize, width: usize, height: u16, raw10: bool) -> Self {
        let rows = data
            .len()
            .checked_div(stride)
            .map_or(0, |r| r.min(height as usize));
        Self {
            data,
            stride,
            width,
            rows,
            raw10,
        }
    }

    /// Row `y` (`< rows`).
    fn row(&self, y: usize) -> &[u8] {
        self.data
            .get(y * self.stride..(y + 1) * self.stride)
            .unwrap_or(&[])
    }

    /// Pixel `(x, y)` of the frame (`x < width`, `y < rows`; 0 outside).
    fn pixel(&self, x: usize, y: usize) -> u16 {
        let row = self.row(y);
        if self.raw10 {
            let at = x / 4 * raw10::GROUP_BYTES;
            row.get(at..)
                .and_then(|g| g.first_chunk())
                .map_or(0, |g| raw10::unpack(g)[x % 4] as u16)
        } else {
            row.get(x).map_or(0, |&v| v as u16)
        }
    }

    /// Pixels `x0..x0 + out.len()` of row `y` into `out`, as `value - bg`.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn load(&self, y: usize, x0: usize, bg: i32, out: &mut [i32]) {
        let row = self.row(y);
        if self.raw10 {
            for (i, o) in out.iter_mut().enumerate() {
                let x = x0 + i;
                let at = x / 4 * raw10::GROUP_BYTES;
                *o = row
                    .get(at..)
                    .and_then(|g| g.first_chunk())
                    .map_or(0, |g| raw10::unpack(g)[x % 4] as i32)
                    - bg;
            }
        } else {
            let px = row.get(x0..).unwrap_or(&[]);
            for (o, &v) in out.iter_mut().zip(px) {
                *o = v as i32 - bg;
            }
        }
    }

    /// The pixel range `p - half..=p + half` clipped to `0..len`.
    fn span(p: usize, half: usize, len: usize) -> (usize, usize) {
        (p.saturating_sub(half), (p + half).min(len - 1))
    }
}

/// The window sigma for a blob of `area` pixels cut at half its peak, in 1/4096 px:
/// `sqrt(area / (2 pi ln 2))`, `1 / (2 pi ln 2) = 0.229612`.
fn window_sigma(area: u16) -> i64 {
    let s2 = ((area as u64) << (2 * Q)) * 229_612 / 1_000_000;
    (s2.isqrt() as i64).clamp(SIGMA_MIN, SIGMA_MAX)
}

/// The pixel holding position `c` (1/4096 px, `c >= 0`).
fn pixel_of(c: i64) -> usize {
    ((c + ONE / 2) >> Q) as usize
}

/// Median of the pixels of the border of `x0..=x1` x `y0..=y1` that lie in the frame;
/// `None` if none does.
fn ring_median(view: &View, x0: i64, x1: i64, y0: i64, y1: i64) -> Option<u16> {
    let mut buf = [0u16; MAX_RING];
    let mut n = 0;
    let (w, h) = (view.width as i64, view.rows as i64);
    let mut put = |x: i64, y: i64| {
        if (0..w).contains(&x)
            && (0..h).contains(&y)
            && let Some(slot) = buf.get_mut(n)
        {
            *slot = view.pixel(x as usize, y as usize);
            n += 1;
        }
    };
    for y in y0..=y1 {
        if y == y0 || y == y1 {
            for x in x0..=x1 {
                put(x, y);
            }
        } else {
            put(x0, y);
            if x1 != x0 {
                put(x1, y);
            }
        }
    }
    let ring = buf.get_mut(..n).filter(|r| !r.is_empty())?;
    let mid = (ring.len() - 1) / 2;
    Some(*ring.select_nth_unstable(mid).1)
}

/// One centroid step from `c` (1/4096 px): twice the weighted mean offset over the window
/// of half-size `half`, or `None` when the weights sum to zero or less.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn step(view: &View, c: (i64, i64), half: usize, bg: i32, inv: u64) -> Option<(i64, i64)> {
    let (x0, x1) = View::span(pixel_of(c.0), half, view.width);
    let (y0, y1) = View::span(pixel_of(c.1), half, view.rows);
    let n = x1 - x0 + 1;
    // Per column: the Gaussian `g` (Q15) and `g * dx` (dx in 1/4096 px). `g * |dx|` is at
    // most `32768 * s * exp(-1/2)` plus the table's rounding: below 6.6e8 for s <= 8 px.
    let mut gx = [0i32; MAX_SIDE];
    let mut gxd = [0i32; MAX_SIDE];
    for ((g, gd), x) in gx.iter_mut().zip(&mut gxd).zip(x0..=x1) {
        let d = ((x as i64) << Q) - c.0;
        *g = gauss(d, inv);
        *gd = (*g as i64 * d) as i32;
    }
    let (gx, gxd) = (&gx[..n], &gxd[..n]);
    let mut px = [0i32; MAX_SIDE];
    let px = &mut px[..n];
    let (mut sw, mut sx, mut sy) = (0i128, 0i128, 0i128);
    for y in y0..=y1 {
        let d = ((y as i64) << Q) - c.1;
        let gy = gauss(d, inv) as i64;
        view.load(y, x0, bg, px);
        // |a| <= 1023, g <= 32768: `a * g` fits an i32.
        let (mut s0, mut s1) = (0i64, 0i64);
        for ((&a, &g), &gd) in px.iter().zip(gx).zip(gxd) {
            s0 += (a * g) as i64;
            s1 += a as i64 * gd as i64;
        }
        // Row sums in i128: `gy * s1` can pass 2^60 for a large saturated spot.
        sw += gy as i128 * s0 as i128;
        sx += gy as i128 * s1 as i128;
        sy += (gy * d) as i128 * s0 as i128;
    }
    if sw <= 0 {
        return None;
    }
    // `sx / sw` and `sy / sw` are the mean offsets in 1/4096 px (the weights cancel).
    let step = |m: i128| (2 * m / sw).clamp(-ONE as i128, ONE as i128) as i64;
    Some((step(sx), step(sy)))
}

/// `(width in 1/256 px, flux)` over the window of half-size `half` around `c`, weighted as
/// in [`step`] (window sigma `s`, `inv = 2^44 / s^2`).
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn moments(view: &View, c: (i64, i64), half: usize, bg: i32, s: i64, inv: u64) -> (u16, u32) {
    let (x0, x1) = View::span(pixel_of(c.0), half, view.width);
    let (y0, y1) = View::span(pixel_of(c.1), half, view.rows);
    let n = x1 - x0 + 1;
    // Per column: `g` (Q15) and `g * dx^2` (dx^2 in 1/4096 px^2). `g * dx^2` is at most
    // `32768 * 2 s^2 / e` plus the table's rounding: below 6.4e9 for s <= 8 px.
    let mut gx = [0i32; MAX_SIDE];
    let mut gxd2 = [0i64; MAX_SIDE];
    for ((g, gd2), x) in gx.iter_mut().zip(&mut gxd2).zip(x0..=x1) {
        let d = ((x as i64) << Q) - c.0;
        *g = gauss(d, inv);
        *gd2 = *g as i64 * ((d * d) >> Q);
    }
    let (gx, gxd2) = (&gx[..n], &gxd2[..n]);
    let mut px = [0i32; MAX_SIDE];
    let px = &mut px[..n];
    let mut flux = 0i64;
    let (mut sw, mut sxx, mut syy) = (0i128, 0i128, 0i128);
    for y in y0..=y1 {
        let d = ((y as i64) << Q) - c.1;
        let gy = gauss(d, inv) as i64;
        view.load(y, x0, bg, px);
        let (mut sum, mut s0, mut s2) = (0i64, 0i64, 0i64);
        for ((&a, &g), &gd2) in px.iter().zip(gx).zip(gxd2) {
            sum += a as i64;
            s0 += (a * g) as i64;
            s2 += a as i64 * gd2;
        }
        flux += sum;
        sw += gy as i128 * s0 as i128;
        sxx += gy as i128 * s2 as i128;
        syy += (gy * ((d * d) >> Q)) as i128 * s0 as i128;
    }
    let flux = flux.clamp(0, u32::MAX as i64) as u32;
    if sw <= 0 {
        return (0, flux);
    }
    // Mean weighted second moment of x and y, and the window variance, in 1/4096^2 px^2.
    let m = ((sxx + syy) << Q) / (2 * sw);
    let s2 = (s * s) as i128;
    if m >= s2 {
        return (u16::MAX, flux);
    }
    let var = (m * s2 / (s2 - m)).max(0) - (ONE * ONE / 12) as i128;
    let sigma = (var.clamp(0, u64::MAX as i128) as u64).isqrt();
    (((sigma + 8) >> 4).min(u16::MAX as u64) as u16, flux)
}

/// The spot of `blob` and the number of centroid steps taken.
fn refine(view: &View, blob: &Blob) -> (Spot, u32) {
    if view.width == 0 || view.rows == 0 {
        let spot = Spot {
            x: blob.x as u32 * 8,
            y: blob.y as u32 * 8,
            ..Spot::default()
        };
        return (spot, 0);
    }
    let max = (
        ((view.width - 1) as i64) << Q,
        ((view.rows - 1) as i64) << Q,
    );
    // Q11.5 to Q12.
    let start = (
        ((blob.x as i64) << 7).min(max.0),
        ((blob.y as i64) << 7).min(max.1),
    );
    let s = window_sigma(blob.area);
    let half = ((4 * s + ONE - 1) >> Q) as usize;
    let inv = (1u64 << 44) / (s * s) as u64;

    let (px, py) = (pixel_of(start.0) as i64, pixel_of(start.1) as i64);
    let r = half as i64 + 1;
    let bg = ring_median(view, px - r, px + r, py - r, py + r).or_else(|| {
        let (x0, x1) = View::span(px as usize, r as usize, view.width);
        let (y0, y1) = View::span(py as usize, r as usize, view.rows);
        ring_median(view, x0 as i64, x1 as i64, y0 as i64, y1 as i64)
    });
    let bg = bg.unwrap_or(0) as i32;

    let mut c = start;
    let mut steps = 0;
    while steps < MAX_STEPS {
        steps += 1;
        let Some((dx, dy)) = step(view, c, half, bg, inv) else {
            c = start;
            break;
        };
        c = ((c.0 + dx).clamp(0, max.0), (c.1 + dy).clamp(0, max.1));
        if dx.abs() < STOP && dy.abs() < STOP {
            break;
        }
    }
    let (width, flux) = moments(view, c, half, bg, s, inv);
    let spot = Spot {
        x: ((c.0 + 8) >> 4) as u32,
        y: ((c.1 + 8) >> 4) as u32,
        width,
        flux,
    };
    (spot, steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DefaultDetector, auto};
    use std::vec::Vec;
    use std::{println, vec};

    /// erf, Abramowitz and Stegun 7.1.26 (error below 1.5e-7).
    fn erf(x: f64) -> f64 {
        let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
        let p = t
            * (0.254_829_592
                + t * (-0.284_496_736
                    + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
        let y = 1.0 - p * (-x * x).exp();
        if x < 0.0 { -y } else { y }
    }

    struct Rng(u32);

    impl Rng {
        fn next(&mut self) -> u32 {
            let mut s = self.0;
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            self.0 = s;
            s
        }

        /// Uniform in (0, 1).
        fn unit(&mut self) -> f64 {
            (self.next() as f64 + 0.5) / 4_294_967_296.0
        }

        /// Standard normal (Box-Muller).
        fn normal(&mut self) -> f64 {
            let (u, v) = (self.unit(), self.unit());
            (-2.0 * u.ln()).sqrt() * (2.0 * core::f64::consts::PI * v).cos()
        }
    }

    /// A `w` x `h` scene of noise-free spot values (no background), pixel centres at
    /// integer coordinates.
    struct Scene {
        w: usize,
        h: usize,
        v: Vec<f64>,
    }

    impl Scene {
        fn new(w: usize, h: usize) -> Self {
            Self {
                w,
                h,
                v: vec![0.0; w * h],
            }
        }

        /// Adds a pixel-integrated Gaussian of peak `a` (of the continuous profile).
        fn gauss(&mut self, cx: f64, cy: f64, sigma: f64, a: f64) -> &mut Self {
            let k = sigma * core::f64::consts::SQRT_2;
            let mass = |x: f64, c: f64| {
                // Integral over the pixel of exp(-(u - c)^2 / (2 sigma^2)).
                0.5 * k
                    * core::f64::consts::PI.sqrt()
                    * (erf((x + 0.5 - c) / k) - erf((x - 0.5 - c) / k))
            };
            for y in 0..self.h {
                let my = mass(y as f64, cy);
                for x in 0..self.w {
                    self.v[y * self.w + x] += a * mass(x as f64, cx) * my;
                }
            }
            self
        }

        /// Adds a uniform disc of radius `r` and value `a`, pixel-integrated on 32 x 32
        /// sub-samples.
        fn disc(&mut self, cx: f64, cy: f64, r: f64, a: f64) -> &mut Self {
            const N: usize = 32;
            for y in 0..self.h {
                for x in 0..self.w {
                    let (dx, dy) = (x as f64 - cx, y as f64 - cy);
                    if dx.abs() > r + 1.0 || dy.abs() > r + 1.0 {
                        continue;
                    }
                    let mut inside = 0;
                    for j in 0..N {
                        for i in 0..N {
                            let u = dx + (i as f64 + 0.5) / N as f64 - 0.5;
                            let v = dy + (j as f64 + 0.5) / N as f64 - 0.5;
                            inside += (u * u + v * v <= r * r) as u32;
                        }
                    }
                    self.v[y * self.w + x] += a * inside as f64 / (N * N) as f64;
                }
            }
            self
        }

        fn total(&self) -> f64 {
            self.v.iter().sum()
        }

        /// Pixel values: `scale * (bg + spot)` plus noise of read sigma `read` and shot
        /// variance `value / gain` (in units of `scale`), rounded and clamped to `max`.
        fn values(
            &self,
            bg: f64,
            scale: f64,
            max: f64,
            noise: Option<(&mut Rng, f64, f64)>,
        ) -> Vec<u16> {
            let mut noise = noise;
            self.v
                .iter()
                .map(|&s| {
                    let mut v = bg + s;
                    if let Some((rng, read, gain)) = noise.as_mut() {
                        v += *read * rng.normal() + (v / *gain).sqrt() * rng.normal();
                    }
                    (v * scale).round().clamp(0.0, max) as u16
                })
                .collect()
        }

        fn raw8(&self, bg: f64, noise: Option<(&mut Rng, f64, f64)>) -> Vec<u8> {
            self.values(bg, 1.0, 255.0, noise)
                .iter()
                .map(|&v| v as u8)
                .collect()
        }

        /// Packed RAW10 of 4 x the RAW8 scene (`w` a multiple of 4).
        fn raw10(&self, bg: f64) -> Vec<u8> {
            self.values(bg, 4.0, 1023.0, None)
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|&px| raw10::pack(px))
                .collect()
        }
    }

    /// The blobs at the automatic threshold (brightest pixel as the peak).
    fn detect8(f: &[u8], w: usize) -> Vec<Blob> {
        let mut h = auto::Histogram::new();
        auto::sample_raw8(f, w as u16, 0, &mut h);
        let bg = auto::Background::from_histogram(&h).unwrap();
        let t = auto::threshold_raw8(bg, f.iter().copied().max());
        let mut out = [Blob::default(); 16];
        let n = DefaultDetector::new().process_frame(f, w as u16, t, &mut out);
        out[..n].to_vec()
    }

    fn detect10(f: &[u8], w: usize) -> Vec<Blob> {
        let mut h = auto::Histogram::new();
        auto::sample_raw10(f, w as u16, 0, &mut h);
        let bg = auto::Background::from_histogram(&h).unwrap();
        let peak = f
            .as_chunks::<{ raw10::GROUP_BYTES }>()
            .0
            .iter()
            .flat_map(raw10::unpack)
            .max()
            .map(|v| (v >> 2) as u8);
        let t = auto::threshold_raw10(bg, peak);
        let mut out = [Blob::default(); 16];
        let n = DefaultDetector::new().process_frame_raw10(f, w as u16, t, &mut out);
        out[..n].to_vec()
    }

    /// `refine` with its step count, checked against the public functions.
    fn run(f: &[u8], w: usize, h: usize, raw10: bool, b: &Blob) -> (Spot, u32) {
        let (w16, h16) = (w as u16, h as u16);
        if raw10 {
            let g = w / 4;
            let r = refine(&View::new(f, g * raw10::GROUP_BYTES, g * 4, h16, true), b);
            assert_eq!(r.0, refine_raw10(f, w16, h16, b));
            r
        } else {
            let r = refine(&View::new(f, w, w, h16, false), b);
            assert_eq!(r.0, refine_raw8(f, w16, h16, b));
            r
        }
    }

    fn px(v: u32) -> f64 {
        v as f64 / SUBPIXEL as f64
    }

    /// Worst errors over a set of frames.
    #[derive(Default)]
    struct Worst {
        pos: f64,
        thresholded: f64,
        width: f64,
        flux: f64,
        steps: u32,
        sum_steps: u32,
        n: u32,
        /// Range of the widths read, px.
        widths: (f64, f64),
    }

    impl Worst {
        fn add(&mut self, (s, steps): (Spot, u32), b: &Blob, (cx, cy): (f64, f64)) {
            self.pos = self.pos.max((px(s.x) - cx).abs()).max((px(s.y) - cy).abs());
            let (bx, by) = (b.x as f64 / 32.0, b.y as f64 / 32.0);
            self.thresholded = self.thresholded.max((bx - cx).abs()).max((by - cy).abs());
            self.steps = self.steps.max(steps);
            self.sum_steps += steps;
            let w = px(s.width as u32);
            if self.n == 0 {
                self.widths = (w, w);
            }
            self.widths = (self.widths.0.min(w), self.widths.1.max(w));
            self.n += 1;
        }

        fn mean_steps(&self) -> f64 {
            self.sum_steps as f64 / self.n as f64
        }
    }

    const W: usize = 64;
    const PHASES: [f64; 8] = [0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875];

    #[test]
    fn exp_table() {
        for (k, &e) in EXP.iter().enumerate() {
            let want = (32768.0 * (-(k as f64) / 32.0).exp()).round();
            assert_eq!(e as f64, want, "k {k}");
        }
        assert_eq!(gauss(0, 1 << 44 >> 24), 32768);
    }

    #[test]
    fn window_sigma_from_area() {
        // sigma 1.5 px: area 2 pi ln 2 * 2.25 = 9.80.
        assert_eq!(window_sigma(10) * 1000 / ONE, 1515);
        assert_eq!(window_sigma(0), SIGMA_MIN);
        assert_eq!(window_sigma(u16::MAX), SIGMA_MAX);
    }

    #[test]
    fn gaussian_spots_noise_free() {
        println!("sigma fmt  |err| max  thresholded  width err  flux err  steps max/mean  window");
        for sigma in [1.0, 1.5, 2.5, 4.0] {
            for raw10 in [false, true] {
                let scale = if raw10 { 4.0 } else { 1.0 };
                let mut worst = Worst::default();
                let mut sides = (i64::MAX, 0);
                for &fy in &PHASES {
                    for &fx in &PHASES {
                        let c = (32.0 + fx, 31.0 + fy);
                        let mut scene = Scene::new(W, W);
                        scene.gauss(c.0, c.1, sigma, 220.0);
                        let (f, blobs) = if raw10 {
                            let f = scene.raw10(10.0);
                            let b = detect10(&f, W);
                            (f, b)
                        } else {
                            let f = scene.raw8(10.0, None);
                            let b = detect8(&f, W);
                            (f, b)
                        };
                        assert_eq!(blobs.len(), 1);
                        let r = run(&f, W, W, raw10, &blobs[0]);
                        let width = px(r.0.width as u32);
                        worst.width = worst.width.max((width / sigma - 1.0).abs());
                        let flux = r.0.flux as f64 / (scene.total() * scale);
                        worst.flux = worst.flux.max((flux - 1.0).abs());
                        worst.add(r, &blobs[0], c);
                        let half = (4 * window_sigma(blobs[0].area) + ONE - 1) >> Q;
                        sides.0 = sides.0.min(2 * half + 1);
                        sides.1 = sides.1.max(2 * half + 1);
                    }
                }
                println!(
                    "{sigma:5} {} {:9.4}  {:11.4}  {:8.3}  {:8.4}  {}/{:.2}  {}..={} px",
                    if raw10 { "10" } else { " 8" },
                    worst.pos,
                    worst.thresholded,
                    worst.width,
                    worst.flux,
                    worst.steps,
                    worst.mean_steps(),
                    sides.0,
                    sides.1
                );
                assert!(
                    worst.pos <= 0.01,
                    "sigma {sigma} raw10 {raw10}: {}",
                    worst.pos
                );
                assert!(worst.width <= 0.02, "sigma {sigma}: width {}", worst.width);
                assert!(worst.flux <= 0.05, "sigma {sigma}: flux {}", worst.flux);
            }
        }
    }

    #[test]
    fn symmetric_spots() {
        type Draw = fn(&mut Scene, f64, f64, f64);
        let cases: [(&str, Draw); 3] = [
            ("disc r 2.5", |s, x, y, a| {
                s.disc(x, y, 2.5, a);
            }),
            ("disc r 4", |s, x, y, a| {
                s.disc(x, y, 4.0, a);
            }),
            ("clipped gauss", |s, x, y, a| {
                s.gauss(x, y, 2.0, a * 600.0 / 220.0);
            }),
        ];
        for (name, draw) in cases {
            for raw10 in [false, true] {
                let mut worst = Worst::default();
                for &fy in &PHASES {
                    for &fx in &PHASES {
                        let c = (32.0 + fx, 31.0 + fy);
                        let mut scene = Scene::new(W, W);
                        draw(&mut scene, c.0, c.1, 220.0);
                        let (f, blobs) = if raw10 {
                            let f = scene.raw10(10.0);
                            let b = detect10(&f, W);
                            (f, b)
                        } else {
                            let f = scene.raw8(10.0, None);
                            let b = detect8(&f, W);
                            (f, b)
                        };
                        assert_eq!(blobs.len(), 1);
                        let r = run(&f, W, W, raw10, &blobs[0]);
                        worst.add(r, &blobs[0], c);
                    }
                }
                println!(
                    "{name:14} raw10 {raw10:5}: |err| max {:.4} (thresholded {:.4}), steps {}/{:.2}, width {:.3}..={:.3}",
                    worst.pos,
                    worst.thresholded,
                    worst.steps,
                    worst.mean_steps(),
                    worst.widths.0,
                    worst.widths.1
                );
                assert!(worst.pos <= 0.02, "{name} raw10 {raw10}: {}", worst.pos);
            }
        }
    }

    #[test]
    fn noisy_spot() {
        // Read noise 2 DN, shot noise of 4 electrons per DN, sigma 1.5 px, peak 220.
        let mut rng = Rng(0x2545_f491);
        let (mut se, mut st, mut steps, mut most) = (0.0, 0.0, 0, 0);
        let (mut sw, mut sw2) = (0.0, 0.0);
        const FRAMES: usize = 200;
        for _ in 0..FRAMES {
            let c = (32.0 + rng.unit(), 31.0 + rng.unit());
            let mut scene = Scene::new(W, W);
            scene.gauss(c.0, c.1, 1.5, 220.0);
            let f = scene.raw8(10.0, Some((&mut rng, 2.0, 4.0)));
            let blobs = detect8(&f, W);
            assert_eq!(blobs.len(), 1);
            let (s, n) = run(&f, W, W, false, &blobs[0]);
            let b = &blobs[0];
            se += (px(s.x) - c.0).powi(2) + (px(s.y) - c.1).powi(2);
            st += (b.x as f64 / 32.0 - c.0).powi(2) + (b.y as f64 / 32.0 - c.1).powi(2);
            let w = px(s.width as u32);
            sw += w;
            sw2 += (w - 1.5) * (w - 1.5);
            steps += n;
            most = most.max(n);
        }
        let rms = (se / (2 * FRAMES) as f64).sqrt();
        let rms_t = (st / (2 * FRAMES) as f64).sqrt();
        let width = sw / FRAMES as f64;
        let width_rms = (sw2 / FRAMES as f64).sqrt();
        println!(
            "noise: rms per axis {rms:.4} px (thresholded {rms_t:.4}), width mean {width:.3} rms error {width_rms:.3}, steps {most}/{:.2}",
            steps as f64 / FRAMES as f64
        );
        assert!(rms <= 0.03, "{rms}");
        assert!(rms < rms_t, "{rms} {rms_t}");
        assert!((width / 1.5 - 1.0).abs() <= 0.05, "{width}");
    }

    #[test]
    fn two_spots_six_sigma_apart() {
        let mut worst: f64 = 0.0;
        for (dx, dy) in [(9.0, 0.0), (0.0, 9.0), (6.364, 6.364)] {
            for &f in &PHASES {
                let a = (26.0 + f, 26.0 + f / 2.0);
                let b = (a.0 + dx, a.1 + dy);
                let mut scene = Scene::new(W, W);
                scene
                    .gauss(a.0, a.1, 1.5, 220.0)
                    .gauss(b.0, b.1, 1.5, 220.0);
                let frame = scene.raw8(10.0, None);
                let blobs = detect8(&frame, W);
                assert_eq!(blobs.len(), 2);
                for blob in &blobs {
                    let (s, _) = run(&frame, W, W, false, blob);
                    let (x, y) = (px(s.x), px(s.y));
                    let near = |c: (f64, f64)| (x - c.0).abs().max((y - c.1).abs());
                    worst = worst.max(near(a).min(near(b)));
                }
            }
        }
        println!("two spots: |err| max {worst:.4}");
        assert!(worst <= 0.05, "{worst}");
    }

    fn inside(s: Spot, w: usize, h: usize) {
        assert!(
            s.x <= (w as u32 - 1) * SUBPIXEL && s.y <= (h as u32 - 1) * SUBPIXEL,
            "{s:?}"
        );
    }

    #[test]
    fn frame_edges() {
        // Spot on the corner.
        let mut scene = Scene::new(32, 32);
        scene.gauss(0.3, 0.4, 1.5, 220.0);
        let f8 = scene.raw8(10.0, None);
        let f10 = scene.raw10(10.0);
        let blob = detect8(&f8, 32)[0];
        inside(refine_raw8(&f8, 32, 32, &blob), 32, 32);
        inside(refine_raw10(&f10, 32, 32, &blob), 32, 32);
        // Blobs anywhere, of any size, on the corners and beyond the frame.
        for (x, y) in [
            (0, 0),
            (31 * 32, 31 * 32),
            (u16::MAX, u16::MAX),
            (0, u16::MAX),
        ] {
            for area in [0, 1, 10, 500, u16::MAX] {
                let b = Blob {
                    x,
                    y,
                    area,
                    peak: 255,
                };
                inside(refine_raw8(&f8, 32, 32, &b), 32, 32);
                inside(refine_raw10(&f10, 32, 32, &b), 32, 32);
            }
        }
        // A 4-pixel-wide frame.
        let mut scene = Scene::new(4, 16);
        scene.gauss(1.6, 7.3, 1.0, 220.0);
        let f8 = scene.raw8(10.0, None);
        let f10 = scene.raw10(10.0);
        let blob = detect8(&f8, 4)[0];
        let s = refine_raw8(&f8, 4, 16, &blob);
        inside(s, 4, 16);
        inside(refine_raw10(&f10, 4, 16, &blob), 4, 16);
        println!("4 px wide: {:.3}, {:.3}", px(s.x), px(s.y));
        // A frame shorter than width * height: the missing rows are left out.
        inside(refine_raw8(&f8[..4 * 5 + 2], 4, 16, &blob), 4, 5);
    }

    #[test]
    fn any_input_is_clipped() {
        let mut rng = Rng(77);
        let mut buf = [0u8; 80 * 80];
        for _ in 0..3000 {
            let w = rng.next() as usize % 72;
            let h = rng.next() as usize % 72;
            let len = rng.next() as usize % (w * h + 16);
            for b in &mut buf[..len] {
                *b = rng.next() as u8;
            }
            let blob = Blob {
                x: rng.next() as u16 % (w as u16 * 32 + 64),
                y: rng.next() as u16 % (h as u16 * 32 + 64),
                area: rng.next() as u16 >> (rng.next() % 16),
                peak: rng.next() as u8,
            };
            let f = &buf[..len];
            let rows = (len / w.max(1)).min(h);
            let s = refine_raw8(f, w as u16, h as u16, &blob);
            if w > 0 && rows > 0 {
                inside(s, w, rows);
            }
            let g = w / 4;
            let rows = (len / (g * raw10::GROUP_BYTES).max(1)).min(h);
            let s = refine_raw10(f, w as u16, h as u16, &blob);
            if g > 0 && rows > 0 {
                inside(s, g * 4, rows);
            }
        }
    }
}
