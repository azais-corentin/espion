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
//!    `s = sqrt(area / (2 pi ln 2))`, at least 0.75 px. A spot with `s` above 4 px (an
//!    area of 70 and up) is measured on bins of `2^k` x `2^k` pixels, `k` the smallest
//!    (at most 5, with at least one bin per row and column) that brings `s / 2^k` to 4 or
//!    below: each bin is the rounded mean of its pixels (RAW8 with 2 fraction bits), bin
//!    centres at `(2^k - 1) / 2` px from their first pixel's, the partial bins of the
//!    frame's right and bottom edges left out. Everything below is then in bins, the
//!    result scaled back to pixels. A window too narrow for the spot would see only its
//!    top: the width blows up (step 4) and the centre follows the top's unevenness. The
//!    square of half-size `ceil(4 s) + 1` around the start pixel (at most 35 x 35) is read
//!    from the frame once into a stack buffer (binned: each of its `4^k` pixels once);
//!    everything below works on that buffer.
//! 2. Local background: the median of the pixels on the perimeter of that square (the
//!    pixels of it in the frame; if none, the border of the square clipped to the frame),
//!    selected with two 32-bin counts (top 5 bits, then low 5 bits of the 10-bit value).
//! 3. Iterated Gaussian-weighted centroid over the square of half-size `ceil(4 s)` around
//!    the current estimate (clipped to the buffer: an estimate more than 1 px from the
//!    start pixel loses the far columns): each pixel weighs
//!    `(value - background) * exp(-d^2 / (2 s^2))`, `d` its distance to the estimate, the
//!    difference not clipped (so the noise does not bias it). Each step moves twice the
//!    weighted mean offset (clamped to 1 px per axis): for a Gaussian spot of the window's
//!    width that lands on the centre in one step, and the centre of any symmetric spot is
//!    the fixed point whatever `s`. At most [`MAX_STEPS`] steps, until a step is below
//!    1/512 px on both axes. A window whose weights sum to zero or less keeps the start.
//! 4. Over the window of the last step (whose estimate is within 1/512 px of the result
//!    once converged), with the same weights: `width` from
//!    the weighted second moment `m = sum(w d^2) / sum(w)` (x and y averaged), `w` the
//!    unclipped `(value - background) * G`, so the noise does not bias it. A Gaussian spot
//!    of sigma `sigma` under the window gives `m = sigma^2 s^2 / (sigma^2 + s^2)`, so
//!    `sigma^2 = m s^2 / (s^2 - m)`, less the 1/12 px^2 that the pixel's own width adds
//!    (binned: the bin's, 1/12 bin^2). For any other profile this is the sigma of the
//!    Gaussian with the same weighted moment. `m <= 0` or weights summing to zero or less
//!    give 0; `m >= s^2` (a spot far wider than the window) or a width past 255 px gives
//!    `u16::MAX`. `flux` is the sum of `value - background` over the window, unweighted,
//!    clamped to 0 and up (binned: times the pixels per bin).
//!
//! The weights are separable, `G = gx(dx) gy(dy)`: each step builds `gx` and `gy` once
//! (at most 33 entries each) at the current sub-pixel estimate, and makes one pass over
//! the window whose inner loop is four `i32` multiply-adds per pixel: `v - bg`, times
//! `gx`, `gx * ix` and `gx * dx^2 / 4` (`ix` the column's integer offset from the window's
//! centre pixel); rows are folded into `i64` totals with `gy`. The same pass gives the
//! step, the width and the flux. Positions are held in 1/4096 px. The Gaussian comes from
//! a table of `exp(-t)`, `t` in steps of 1/32 up to 16, in Q15 with linear interpolation;
//! the table is built at compile time with integer math (`EXP`). Divisions are 32-bit
//! (the 64-bit totals are narrowed first): two per step, three per blob; one integer
//! square root per blob (and one for the window sigma); no sort. Every input is clipped
//! to the frame (blobs at the edge, frames shorter than `width * height`), so no input
//! panics, and host and target give the same result bit for bit.

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
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
pub fn refine_raw8(frame: &[u8], width: u16, height: u16, blob: &Blob) -> Spot {
    let view = View::new(frame, width as usize, width as usize, height, false);
    refine(&view, blob).0
}

/// Measures `blob` on a packed CSI-2 RAW10 frame ([`raw10`], rows of `width / 4 * 5`
/// bytes, `width` a multiple of 4; the pixels of a partial last group are not used) of
/// `height` rows (fewer if `frame` is shorter). Values, background and flux are 10-bit.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
pub fn refine_raw10(frame: &[u8], width: u16, height: u16, blob: &Blob) -> Spot {
    let groups = width as usize / 4;
    let view = View::new(frame, groups * raw10::GROUP_BYTES, groups * 4, height, true);
    refine(&view, blob).0
}

/// Fraction bits of positions and sigmas.
const Q: u32 = 12;
/// One pixel.
const ONE: i32 = 1 << Q;
/// Window sigma bounds, in pixels of the frame or, for wider spots, of its bins.
const SIGMA_MIN: i32 = 3 * ONE / 4;
const SIGMA_MAX: i32 = 4 * ONE;
/// Most bin shift: bins of 32 x 32 pixels bring the widest window sigma (about 123 px, for
/// an area of `u16::MAX`) below [`SIGMA_MAX`].
const MAX_SHIFT: u32 = 5;
/// Largest window half-size: `ceil(4 * SIGMA_MAX)` px.
const MAX_HALF: usize = 16;
const MAX_SIDE: usize = 2 * MAX_HALF + 1;
/// Side of the buffered square (window plus the background ring), at most.
const BUF_SIDE: usize = MAX_SIDE + 2;
/// Pixels on the border of the buffered square, at most.
const MAX_RING: usize = 4 * (BUF_SIDE - 1);
/// Steps below this (1/512 px) end the iteration.
const STOP: i32 = ONE / 512;
/// 1/12 px^2 in Q15 px^2: the variance of a uniform pixel (or bin, in bins).
const PIXEL_VAR: i64 = (32768 + 6) / 12;

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

/// `exp(-d^2 / (2 s^2))` in Q15, `d` in 1/4096 px (`|d| <= 16.5 px`), `inv = 2^46 / s^2`
/// ([`gauss_inv`]). `|d|` is rounded to 1/2048 px (`q < 2^16`), so `q^2 * inv >> 32` (one
/// `mulhu`) is `t` in 1/(32 * 256): table index and an 8-bit interpolation fraction.
#[inline(always)]
fn gauss(d: i32, inv: u32) -> i32 {
    let q = (d.unsigned_abs() + 1) >> 1;
    let t = (((q * q) as u64 * inv as u64) >> 32) as u32;
    let k = (t >> 8) as usize;
    match (EXP.get(k), EXP.get(k + 1)) {
        (Some(&a), Some(&b)) => {
            let f = (t & 255) as i32;
            (a as i32 * (256 - f) + b as i32 * f + 128) >> 8
        }
        _ => 0,
    }
}

/// `g * d^2 / 4`, `g` in Q15, `d` in 1/4096 px, the result in Q15 px^2 / 4: at most
/// `8192 * 2 s^2 / e` (below 9.7e4 for `s <= 4` px) plus the table's rounding, so a row of
/// `(v - bg) * g d^2 / 4` sums in an `i32` (bounded by a test).
#[inline(always)]
fn quarter_d2(g: i32, d: i32) -> i32 {
    ((g as i64 * (d as i64 * d as i64) + (1 << 25)) >> 26) as i32
}

/// Rounded square root.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn sqrt_round(v: u32) -> u32 {
    let (mut rem, mut root, mut bit) = (v, 0u32, 1u32 << 30);
    while bit > rem {
        bit >>= 2;
    }
    while bit != 0 {
        if rem >= root + bit {
            rem -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    // rem = v - root^2; round up past (root + 1/2)^2.
    if rem > root { root + 1 } else { root }
}

/// A frame: rows of `stride` bytes holding `width` pixels, `rows` of them in `data`; seen
/// through bins of `2^shift` x `2^shift` pixels, `width` and `rows` counting bins.
struct View<'a> {
    data: &'a [u8],
    stride: usize,
    width: usize,
    rows: usize,
    raw10: bool,
    shift: u32,
    /// Fraction bits of a bin's value: 2 for binned RAW8 (a bin's mean in 1/4 DN, still
    /// 10 bits), else 0.
    frac: u32,
}

impl<'a> View<'a> {
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
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
            shift: 0,
            frac: 0,
        }
    }

    /// The frame (`shift` 0) seen through bins of `2^shift` x `2^shift` pixels; the pixels
    /// of a partial last bin column or row are not used.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn binned(&self, shift: u32) -> Self {
        Self {
            width: self.width >> shift,
            rows: self.rows >> shift,
            shift,
            frac: if shift == 0 || self.raw10 { 0 } else { 2 },
            ..*self
        }
    }

    /// `f(i, value)` for the frame pixels `x0 + i`, `i < n`, of frame row `y` (`< rows`).
    #[inline(always)]
    fn pixels(&self, y: usize, x0: usize, n: usize, mut f: impl FnMut(usize, u16)) {
        let row = self
            .data
            .get(y * self.stride..(y + 1) * self.stride)
            .unwrap_or(&[]);
        if self.raw10 {
            // One unpack per group of four pixels.
            let mut x = x0;
            let end = x0 + n;
            while x < end {
                let at = x / 4 * raw10::GROUP_BYTES;
                let px = row
                    .get(at..)
                    .and_then(|g| g.first_chunk())
                    .map_or([0; 4], raw10::unpack);
                for &v in px.get(x % 4..(end - x / 4 * 4).min(4)).unwrap_or(&[]) {
                    f(x - x0, v as u16);
                    x += 1;
                }
            }
        } else if let Some(px) = row.get(x0..x0 + n) {
            for (i, &v) in px.iter().enumerate() {
                f(i, v as u16);
            }
        }
    }

    /// Bins `x0..x0 + out.len()` of bin row `y` (`< rows`) into `out`: each the rounded
    /// mean of its pixels with `frac` fraction bits, at most 10 bits like the pixels.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn load(&self, y: usize, x0: usize, out: &mut [u16]) {
        if self.shift == 0 {
            self.pixels(y, x0, out.len(), |i, v| {
                if let Some(o) = out.get_mut(i) {
                    *o = v;
                }
            });
            return;
        }
        let shift = self.shift;
        let mut acc = [0u32; BUF_SIDE];
        for j in 0..1 << shift {
            self.pixels((y << shift) + j, x0 << shift, out.len() << shift, |i, v| {
                if let Some(a) = acc.get_mut(i >> shift) {
                    *a += v as u32;
                }
            });
        }
        let drop = 2 * shift - self.frac;
        let round = (1 << drop) >> 1;
        for (o, &a) in out.iter_mut().zip(&acc) {
            *o = ((a + round) >> drop) as u16;
        }
    }
}

/// The pixel range `p - half..=p + half` clipped to `lo..=hi`, if not empty.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn span(p: usize, half: usize, lo: usize, hi: usize) -> Option<(usize, usize)> {
    let (a, b) = (p.saturating_sub(half).max(lo), (p + half).min(hi));
    (a <= b).then_some((a, b))
}

/// The window sigma for a blob of `area` pixels cut at half its peak, in frame pixels
/// (1/4096 px): `sqrt(area / (2 pi ln 2))`, `2^24 / (2 pi ln 2) = 3852251`, at least
/// [`SIGMA_MIN`]. Past `u32` (areas above 1114) the root of a 256th, times 16.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn window_sigma(area: u16) -> i32 {
    let v = area as u64 * 3_852_251;
    let s = match u32::try_from(v) {
        Ok(v) => sqrt_round(v),
        Err(_) => sqrt_round((v >> 8) as u32) << 4,
    };
    (s as i32).max(SIGMA_MIN)
}

/// The fewest bins, `2^shift` x `2^shift` pixels, that bring the window sigma `s` (frame
/// pixels) to [`SIGMA_MAX`] bins or below, keeping at least one bin per row and column of
/// the `width` x `rows` frame.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn bin_shift(s: i32, width: usize, rows: usize) -> u32 {
    let mut shift = 0;
    while shift < MAX_SHIFT
        && s >> shift > SIGMA_MAX
        && width >> (shift + 1) > 0
        && rows >> (shift + 1) > 0
    {
        shift += 1;
    }
    shift
}

/// The pixel holding position `c` (1/4096 px, `c >= 0`).
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn pixel_of(c: i32) -> usize {
    ((c + ONE / 2) >> Q) as usize
}

/// The value of the 32 bins holding the `k`-th smallest (0-based) of their counts, and the
/// count in the bins below it.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn select(bins: &[u16; 32], k: u16) -> (u16, u16) {
    let mut below = 0;
    for (i, &c) in bins.iter().enumerate() {
        if below + c > k {
            return (i as u16, below);
        }
        below += c;
    }
    (31, below)
}

/// The lower median of 10-bit values (not empty): the bin of the top 5 bits, then the low
/// 5 bits within it.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn median(vals: &[u16]) -> u16 {
    let k = (vals.len().saturating_sub(1) / 2) as u16;
    let mut high = [0u16; 32];
    for &v in vals {
        high[(v >> 5) as usize & 31] += 1;
    }
    let (h, below) = select(&high, k);
    let mut low = [0u16; 32];
    for &v in vals {
        if (v >> 5) & 31 == h {
            low[v as usize & 31] += 1;
        }
    }
    h << 5 | select(&low, k - below).0
}

/// The square of half-size `r` around a pixel, clipped to the frame, read once.
struct Window {
    /// Rows of `w` pixels.
    buf: [u16; BUF_SIDE * BUF_SIDE],
    /// Frame column and row of `buf[0]`.
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    /// Which sides of the unclipped square lie in the frame: top, bottom, left, right.
    sides: [bool; 4],
}

impl Window {
    /// Reads the square of half-size `r` (`<= MAX_HALF + 1`) around pixel `(px, py)` of
    /// the frame (`view` not empty) in place (returning the 2.4 KB buffer would copy it).
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn load(&mut self, view: &View, (px, py): (usize, usize), r: usize) {
        let (x0, x1) = (px.saturating_sub(r), (px + r).min(view.width - 1));
        let (y0, y1) = (py.saturating_sub(r), (py + r).min(view.rows - 1));
        self.x0 = x0;
        self.y0 = y0;
        self.w = x1 - x0 + 1;
        self.h = y1 - y0 + 1;
        self.sides = [py >= r, py + r < view.rows, px >= r, px + r < view.width];
        for (row, y) in self.buf.chunks_exact_mut(self.w).zip(y0..=y1) {
            view.load(y, x0, row);
        }
    }

    /// `n` pixels of frame row `y` from frame column `x`.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn row(&self, y: usize, x: usize, n: usize) -> &[u16] {
        let at = (y - self.y0) * self.w + x - self.x0;
        self.buf.get(at..at + n).unwrap_or(&[])
    }

    /// The pixels of the window of half-size `half` around pixel `(px, py)`, clipped to
    /// the buffer: columns and rows, if not empty.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn span(&self, (px, py): (usize, usize), half: usize) -> Option<[(usize, usize); 2]> {
        Some([
            span(px, half, self.x0, self.x0 + self.w - 1)?,
            span(py, half, self.y0, self.y0 + self.h - 1)?,
        ])
    }

    /// The pixels of the given sides of the buffer into `out`; the corners once.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn ring(&self, [top, bottom, left, right]: [bool; 4], out: &mut [u16; MAX_RING]) -> usize {
        let mut n = 0;
        let mut put = |v: u16| {
            if let Some(o) = out.get_mut(n) {
                *o = v;
                n += 1;
            }
        };
        for (y, row) in self.buf.chunks_exact(self.w).take(self.h).enumerate() {
            if (top && y == 0) || (bottom && y == self.h - 1) {
                row.iter().for_each(|&v| put(v));
            } else {
                if let (true, Some(&v)) = (left, row.first()) {
                    put(v);
                }
                if let (true, Some(&v)) = (right, row.last()) {
                    put(v);
                }
            }
        }
        n
    }

    /// Median of the perimeter of the square in the frame; if none of it is, of the
    /// buffer's border.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn background(&self) -> u16 {
        let mut ring = [0u16; MAX_RING];
        let mut n = self.ring(self.sides, &mut ring);
        if n == 0 {
            n = self.ring([true, self.h > 1, true, self.w > 1], &mut ring);
        }
        ring.get(..n).filter(|r| !r.is_empty()).map_or(0, median)
    }
}

/// The weighted sums of one pass over the window of half-size `half` around an estimate:
/// in weight units (`gx * gy * (v - bg)`, Q30 per DN) times 1, the integer offset of the
/// pixel from the window's centre pixel, or its squared distance to the estimate / 4 in
/// Q15 px^2.
struct Pass {
    sw: i64,
    sx: i64,
    sy: i64,
    sxx: i64,
    syy: i64,
    /// Sum of `v - bg`, unweighted.
    flux: i64,
    /// The estimate's offset from the window's centre pixel, 1/4096 px.
    f: (i32, i32),
}

impl Pass {
    /// One pass around `c` (1/4096 px), `None` if the window misses the buffer.
    ///
    /// Bounds (`|v - bg| <= 1023`, `g <= 32768`, `|ix| <= 16`, `s <= 4` px): a row's
    /// `sum((v - bg) g)`, `sum((v - bg) g ix)` and `sum((v - bg) g d^2 / 4)` stay below
    /// `1023 * sum(...)` of the column weights, which a test bounds by `i32::MAX`; the
    /// `i64` totals stay below 2^50, so `total << 12` fits.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn new(win: &Window, c: (i32, i32), half: usize, bg: i32, inv: u32) -> Option<Self> {
        let (pcx, pcy) = (pixel_of(c.0), pixel_of(c.1));
        let f = (c.0 - ((pcx as i32) << Q), c.1 - ((pcy as i32) << Q));
        let [(xa, xb), (ya, yb)] = win.span((pcx, pcy), half)?;
        let n = xb - xa + 1;
        // Per column: `(g, g * ix, g * dx^2 / 4)`, side by side so the row loop walks one
        // table.
        let mut gx = [(0i32, 0i32, 0i32); MAX_SIDE];
        for (gi, x) in gx.iter_mut().zip(xa..=xb) {
            let ix = x as i32 - pcx as i32;
            let d = ix * ONE - f.0;
            let g = gauss(d, inv);
            *gi = (g, g * ix, quarter_d2(g, d));
        }
        let gx = gx.get(..n)?;
        let mut p = Pass {
            sw: 0,
            sx: 0,
            sy: 0,
            sxx: 0,
            syy: 0,
            flux: 0,
            f,
        };
        for y in ya..=yb {
            let iy = y as i32 - pcy as i32;
            let d = iy * ONE - f.1;
            let gy = gauss(d, inv);
            let (mut sum, mut s0, mut s1, mut s2) = (0i32, 0i32, 0i32, 0i32);
            for (&v, &(g, gi, gd2)) in win.row(y, xa, n).iter().zip(gx) {
                let a = v as i32 - bg;
                sum += a;
                s0 += a * g;
                s1 += a * gi;
                s2 += a * gd2;
            }
            let s0 = s0 as i64;
            p.flux += sum as i64;
            p.sw += gy as i64 * s0;
            p.sx += gy as i64 * s1 as i64;
            p.sy += (gy * iy) as i64 * s0;
            p.sxx += gy as i64 * s2 as i64;
            p.syy += quarter_d2(gy, d) as i64 * s0;
        }
        Some(p)
    }

    /// The step: twice the weighted mean offset from the estimate, clamped to 1 px per
    /// axis; `None` when the weights sum to zero or less.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn step(&self) -> Option<(i32, i32)> {
        if self.sw <= 0 {
            return None;
        }
        // Mean offset: `sum(w (ix - f)) / sum(w)`, in 1/4096 px. `sw` narrowed to 16 bits,
        // so the quotients (clamped to 4 px) are 32-bit divisions.
        let (d, k) = narrow(self.sw, 16);
        let lim = 4 * ONE as i64 * d as i64;
        let step = |m: i64, f: i32| {
            let mean = ((m << Q) >> k).clamp(-lim, lim) as i32 / d - f;
            (2 * mean).clamp(-ONE, ONE)
        };
        Some((step(self.sx, self.f.0), step(self.sy, self.f.1)))
    }

    /// The spot's sigma in 1/256 px for a window sigma `s` (1/4096 px).
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn width(&self, s: i32) -> u16 {
        if self.sw <= 0 {
            return 0;
        }
        // Mean weighted second moment of x and y: `4 (sxx + syy) / (2 sw)` px^2, i.e.
        // `65536 (sxx + syy) / sw` in Q15 px^2, with `sw` narrowed to 12 bits: `m` is below
        // `s2 <= 2^19`, so the dividend fits 31 bits.
        let s2 = ((s as i64 * s as i64) >> 9) as i32;
        let (d, k) = narrow(self.sw, 12);
        let sum = self.sxx + self.syy;
        let num = if k >= 16 {
            sum >> (k - 16)
        } else {
            sum.clamp(0, 1 << 40) << (16 - k)
        };
        let m = num.clamp(0, s2 as i64 * d as i64) as i32 / d;
        if m >= s2 {
            return u16::MAX;
        }
        // sigma^2 = m s2 / (s2 - m): the ratio in Q11 (`m << 11 < 2^30`), then times s2.
        let r = (m << 11) / (s2 - m);
        let var = ((s2 as i64 * r as i64) >> 11) - PIXEL_VAR;
        // Q16 px^2, whose square root is in 1/256 px.
        let var = (var.clamp(0, u32::MAX as i64 / 2) * 2) as u32;
        sqrt_round(var).min(u16::MAX as u32) as u16
    }
}

/// `(sw >> k, k)` with `sw >> k` below `2^bits` (`sw > 0`): a 32-bit divisor keeping the
/// top `bits` bits of `sw`.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn narrow(sw: i64, bits: u32) -> (i32, u32) {
    let k = (64 - sw.leading_zeros()).saturating_sub(bits);
    ((sw >> k) as i32, k)
}

/// `2^46 / s^2` for [`gauss`] (`s` in 1/4096 px, 0.75..=4 px, so below 2^23), as
/// `(2^31 / s)^2 / 2^16`: a 32-bit division.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn gauss_inv(s: i32) -> u32 {
    let r = (1u32 << 31) / s as u32;
    ((r as u64 * r as u64) >> 16) as u32
}

/// The spot of `blob` and the number of centroid steps taken.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn refine(view: &View, blob: &Blob) -> (Spot, u32) {
    if view.width == 0 || view.rows == 0 {
        let spot = Spot {
            x: blob.x as u32 * 8,
            y: blob.y as u32 * 8,
            ..Spot::default()
        };
        return (spot, 0);
    }
    // A spot wider than the window is measured on bins of 2^shift x 2^shift pixels, in
    // which it is narrow enough; everything below is in bins.
    let sf = window_sigma(blob.area);
    let shift = bin_shift(sf, view.width, view.rows);
    let view = &view.binned(shift);
    let s = (sf >> shift).clamp(SIGMA_MIN, SIGMA_MAX);
    // Widths and rows are at most 65535: positions fit an i32 in 1/4096 px.
    let max = (
        ((view.width - 1) as i32) << Q,
        ((view.rows - 1) as i32) << Q,
    );
    // Frame position `p` is bin position `(p - off) / 2^shift`: bin centres sit
    // `(2^shift - 1) / 2` px from their first pixel's.
    let off = ((1 << shift) - 1) << (Q - 1);
    // Q11.5 to Q12, then to bins.
    let to_bins = |p: u16| ((p as i32) << 7).saturating_sub(off).max(0) >> shift;
    let start = (to_bins(blob.x).min(max.0), to_bins(blob.y).min(max.1));
    let half = ((4 * s + ONE - 1) >> Q) as usize;
    let inv = gauss_inv(s);
    // A struct literal (a memset), not a const (a 2.4 KB copy from flash).
    let mut win = Window {
        buf: [0; BUF_SIDE * BUF_SIDE],
        x0: 0,
        y0: 0,
        w: 1,
        h: 1,
        sides: [false; 4],
    };
    win.load(view, (pixel_of(start.0), pixel_of(start.1)), half + 1);
    let bg = win.background() as i32;

    // Each pass gives the step and, over the same window, the width and flux: those of
    // the last pass are kept (its estimate is within 1/512 px of the result once the steps
    // have converged).
    let mut c = start;
    let mut steps = 0;
    let mut last = None;
    while steps < MAX_STEPS {
        steps += 1;
        let Some(p) = Pass::new(&win, c, half, bg, inv) else {
            c = start;
            break;
        };
        let step = p.step();
        last = Some(p);
        let Some((dx, dy)) = step else {
            c = start;
            break;
        };
        c = ((c.0 + dx).clamp(0, max.0), (c.1 + dy).clamp(0, max.1));
        if dx.abs() < STOP && dy.abs() < STOP {
            break;
        }
    }
    // Back to frame pixels: the width scales with the bin, the flux sums `4^shift` pixels
    // per bin, in units of `2^-frac`.
    let (width, flux) = last.map_or((0, 0), |p| {
        let width = ((p.width(s) as u32) << shift).min(u16::MAX as u32) as u16;
        let flux = ((p.flux.max(0) << (2 * shift)) >> view.frac).min(u32::MAX as i64) as u32;
        (width, flux)
    });
    let to_frame = |c: i32| (((c << shift) + off + 8) >> 4) as u32;
    let spot = Spot {
        x: to_frame(c.0),
        y: to_frame(c.1),
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
        assert_eq!(gauss(0, 1 << 20), 32768);
    }

    #[test]
    fn window_sigma_from_area() {
        // sigma 1.5 px: area 2 pi ln 2 * 2.25 = 9.80.
        assert_eq!(window_sigma(10) * 1000 / ONE, 1515);
        assert_eq!(window_sigma(0), SIGMA_MIN);
        assert_eq!(window_sigma(69), 16304);
        assert_eq!(window_sigma(70), 16421);
        let k = (1u64 << 24) as f64 / (2.0 * core::f64::consts::PI * core::f64::consts::LN_2);
        assert_eq!(k.round(), 3_852_251.0);
        for area in [70u16, 393, 1114, 1115, 20_000, u16::MAX] {
            let want = (area as f64 * k).sqrt();
            let got = window_sigma(area) as f64;
            assert!((got / want - 1.0).abs() < 1e-4, "area {area}: {got} {want}");
        }
        // The fewest bins that bring the window to SIGMA_MAX, one bin per side at least.
        assert_eq!(bin_shift(SIGMA_MAX, 1280, 800), 0);
        assert_eq!(bin_shift(SIGMA_MAX + 1, 1280, 800), 1);
        // 9.5 px: 4 x 4 bins.
        assert_eq!(bin_shift(window_sigma(393), 1280, 800), 2);
        assert_eq!(bin_shift(window_sigma(u16::MAX), 1280, 800), MAX_SHIFT);
        assert!(window_sigma(u16::MAX) >> MAX_SHIFT <= SIGMA_MAX);
        assert_eq!(bin_shift(window_sigma(u16::MAX), 1280, 3), 1);
    }

    #[test]
    fn sqrt_rounds() {
        for v in (0..100_000u32).chain([u32::MAX, u32::MAX - 1, 1 << 30, (1 << 30) - 1]) {
            assert_eq!(sqrt_round(v), (v as f64).sqrt().round() as u32, "{v}");
        }
    }

    #[test]
    fn median_matches_a_sort() {
        let mut rng = Rng(5);
        for n in 1..=MAX_RING {
            for spread in [1u32, 8, 64, 1024] {
                let vals: Vec<u16> = (0..n).map(|_| (rng.next() % spread) as u16).collect();
                let mut sorted = vals.clone();
                sorted.sort_unstable();
                assert_eq!(median(&vals), sorted[(n - 1) / 2], "{vals:?}");
            }
        }
    }

    /// The per-row `i32` sums of a pass cannot overflow: `1023 * sum(g)`,
    /// `1023 * sum(g |ix|)` and `1023 * sum(g d^2 / 4)` stay below `i32::MAX` for every
    /// window sigma and sub-pixel offset.
    #[test]
    fn row_sums_fit_i32() {
        let mut worst = (0i64, 0i64, 0i64);
        for s in SIGMA_MIN..=SIGMA_MAX {
            let inv = gauss_inv(s);
            let half = (4 * s + ONE - 1) >> Q;
            for f in (-ONE / 2..=ONE / 2).step_by(64) {
                let (mut g0, mut g1, mut g2) = (0i64, 0i64, 0i64);
                for ix in -half..=half {
                    let g = gauss(ix * ONE - f, inv);
                    g0 += g as i64;
                    g1 += (g * ix.abs()) as i64;
                    g2 += quarter_d2(g, ix * ONE - f) as i64;
                }
                worst = (worst.0.max(g0), worst.1.max(g1), worst.2.max(g2));
            }
        }
        println!(
            "max sum(g) {}, sum(g |ix|) {}, sum(g d^2 / 4) {}",
            worst.0, worst.1, worst.2
        );
        assert!(1023 * worst.0 <= i32::MAX as i64);
        assert!(1023 * worst.1 <= i32::MAX as i64);
        assert!(1023 * worst.2 <= i32::MAX as i64);
    }

    #[test]
    fn gaussian_spots_noise_free() {
        println!(
            "sigma fmt  |err| max  thresholded  width err  flux err  steps max/mean  window  pixels/blob"
        );
        for sigma in [1.0, 1.5, 2.5, 4.0] {
            for raw10 in [false, true] {
                let scale = if raw10 { 4.0 } else { 1.0 };
                let mut worst = Worst::default();
                let mut sides = (i32::MAX, 0);
                let mut pixels = 0;
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
                        let sf = window_sigma(blobs[0].area);
                        let shift = bin_shift(sf, W, W);
                        let s = (sf >> shift).clamp(SIGMA_MIN, SIGMA_MAX);
                        let side = 2 * ((4 * s + ONE - 1) >> Q) + 1;
                        // The window in frame pixels.
                        sides.0 = sides.0.min(side << shift);
                        sides.1 = sides.1.max(side << shift);
                        // Reads: the buffer's pixels once, the window's bins once per step.
                        pixels +=
                            (((side + 2) * (side + 2)) << (2 * shift)) + r.1 as i32 * side * side;
                    }
                }
                println!(
                    "{sigma:5} {} {:9.4}  {:11.4}  {:8.3}  {:8.4}  {}/{:.2}  {}..={} px  {}",
                    if raw10 { "10" } else { " 8" },
                    worst.pos,
                    worst.thresholded,
                    worst.width,
                    worst.flux,
                    worst.steps,
                    worst.mean_steps(),
                    sides.0,
                    sides.1,
                    pixels / worst.n as i32
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

    /// Discs wider than the window (radius 6 to 24 px, measured on bins of 2 to 8 px):
    /// centred within 0.5 % of the radius (the bins' sampling of the disc, 0.005 bin), and
    /// a width close to the disc's `R / 2` (a uniform disc's sigma per axis), not the far
    /// larger value of a window too narrow for the spot.
    #[test]
    fn wide_discs() {
        const W: usize = 128;
        const PHASES: [f64; 4] = [0.0, 0.25, 0.5, 0.75];
        for r in [6.0, 9.0, 11.0, 16.0, 24.0] {
            for raw10 in [false, true] {
                let mut worst = Worst::default();
                for &fy in &PHASES {
                    for &fx in &PHASES {
                        let c = (64.0 + fx, 63.0 + fy);
                        let mut scene = Scene::new(W, W);
                        scene.disc(c.0, c.1, r, 200.0);
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
                        let res = run(&f, W, W, raw10, &blobs[0]);
                        worst.add(res, &blobs[0], c);
                    }
                }
                let ratio = (worst.widths.0 / (r / 2.0), worst.widths.1 / (r / 2.0));
                println!(
                    "disc r {r:4} raw10 {raw10:5}: |err| max {:.4} (thresholded {:.4}), steps {}/{:.2}, width / (r / 2) {:.3}..={:.3}",
                    worst.pos,
                    worst.thresholded,
                    worst.steps,
                    worst.mean_steps(),
                    ratio.0,
                    ratio.1
                );
                assert!(worst.pos <= 0.005 * r, "r {r} raw10 {raw10}: {}", worst.pos);
                assert!(
                    ratio.0 >= 1.0 && ratio.1 <= 1.25,
                    "r {r} raw10 {raw10}: width / (r / 2) {ratio:?}"
                );
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
