//! Marker blob detection on RAW8 and packed RAW10 mono frames, `core` only, no heap.
//!
//! A pixel is foreground when its value is `>= threshold`. Foreground pixels are grouped
//! into 8-connected components: each row is scanned into runs of foreground pixels, each run
//! is linked to the runs it touches in the row above, and touching components are merged
//! with union-find. A component whose runs stop (no run in the next row) is finished at once
//! and its slot reused, so only the components crossing the current row are held.
//!
//! Per blob: the intensity-weighted centroid (weight = pixel value, pixel centres at integer
//! coordinates) in 1/32 pixel (Q11.5), the area in pixels (saturating at `u16::MAX`) and the
//! peak value. This centroid sees only the pixels above the threshold, so it is pulled
//! towards the pixel centres; it finds the blob, and [`refine`] then measures it.
//!
//! The scan asks a [`Skip`] where the next bright pixel can be ([`Scalar`] here; with the
//! cargo feature `pie`, `esp32p4_pie::Pie` on the ESP32-P4), reads exactly from there until
//! it has passed the skip's window and no run is open, then asks the skip again. The scalar
//! skip tests 64 bytes at a time with a SWAR test on the OR of their words, then confirms on
//! each word.
//!
//! RAW10 frames in the MIPI CSI-2 packing ([`raw10`]) go through the same API
//! ([`Detector::begin_frame_raw10`]): the threshold is in 10-bit units, the weights are the
//! 10-bit values, `Blob::peak` is the top 8 bits. Their skip tests the top 8 bits of 64
//! pixels at a time (80 bytes, low-bit bytes masked out); the scan then tests each group of
//! four pixels exactly.
//!
//! Rows can be pushed as a whole frame or in strips of whole rows as they arrive; both give
//! the same blobs in the same order (the order in which their last row was seen).
//!
//! [`auto`] picks the threshold from the frame: a sparse histogram of the rows (whole frame
//! or strips) gives the background median and spread, and the threshold for the next frame
//! is the noise floor above them or half the brightest blob's peak, whichever is higher.
//!
//! [`refine`] measures each blob on the frame: an iterated Gaussian-weighted centroid
//! around a local background, free of the threshold's bias, with the spot's width and flux,
//! in 1/256 pixel.
//!
//! [`synth`] generates deterministic synthetic frames for the tests and for the board
//! benchmark. Without the feature `pie` the crate has no dependency; with or without it,
//! it builds and tests on the host.

#![no_std]

// The test references collect into `Vec`.
#[cfg(test)]
extern crate std;

pub mod auto;
pub mod refine;
pub mod skip;
pub mod synth;

use core::num::NonZeroUsize;
pub use skip::{Scalar, Skip};

use skip::{WINDOW, WINDOW10, hot, swar_params};

#[cfg(test)]
mod equivalence;
#[cfg(test)]
mod reference;

/// One detected blob: the thresholded centroid, area and peak, and a first estimate of its
/// size. The wire format's position, width and flux come from [`refine`] on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Blob {
    /// Weighted centroid column in 1/32 pixel (Q11.5).
    pub x: u16,
    /// Weighted centroid row in 1/32 pixel (Q11.5).
    pub y: u16,
    /// Foreground pixels, saturating at `u16::MAX`.
    pub area: u16,
    /// The spot's standard deviation as its light says, in 1/32 pixel ([`spot_sigma`]):
    /// [`refine`] sizes its window with it.
    pub sigma: u16,
    /// Brightest pixel value.
    pub peak: u8,
}

/// The standard deviation, in 1/32 pixel, of a Gaussian spot from its pixels at or above a
/// threshold: `flux` their sum above the background, `height` its peak above the threshold
/// (both in the same units, `height` at least 1).
///
/// A Gaussian spot of peak `P` above the background, cut at `c P`, holds
/// `2 pi sigma^2 P (1 - c)` above the background in its pixels above the cut, and
/// `P (1 - c)` is `height`: `sigma^2 = flux / (2 pi height)`, wherever the threshold cuts
/// it. A flat disc of radius `r` gives `r / sqrt(2 (1 - c))`, the right order for a window.
/// The blob's area alone would need the cut: the same spot covers three times the pixels at
/// a tenth of its peak as at a half.
///
/// Exactly: with `k = min(flux.leading_zeros(), 10)`,
/// `sqrt(((flux << k) / height * 163) >> k)` rounded, at most `u16::MAX`
/// (`1024 / (2 pi) = 162.97`); 32-bit divisions only.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
pub fn spot_sigma(flux: u32, height: u32) -> u16 {
    let k = flux.leading_zeros().min(10);
    let q = (flux << k) / height.max(1);
    let v = (q as u64 * 163) >> k;
    refine::sqrt_round(v.min(u32::MAX as u64) as u32).min(u16::MAX as u32) as u16
}

/// Most thresholds of a frame begun with [`Detector::set_split`]: the frame's, then each
/// twice as far above the background as the one before.
pub const LEVELS: usize = 6;

/// A blob inside another counts as a peak of its own when its peak is at least
/// `SPLIT_NUM / SPLIT_DEN` times its threshold (both above the background) and it has at
/// least [`SPLIT_MIN_AREA`] pixels.
pub const SPLIT_NUM: u32 = 5;
/// Denominator of [`SPLIT_NUM`].
pub const SPLIT_DEN: u32 = 4;
/// Fewest pixels of a blob that splits another ([`SPLIT_NUM`]).
pub const SPLIT_MIN_AREA: u32 = 3;

/// How [`Detector::set_split`] splits the blobs of a frame by their peaks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Split {
    /// The level the frame's dark pixels sit at, in the units of the frame begun (8-bit for
    /// [`Detector::begin_frame`], 10-bit for [`Detector::begin_frame_raw10`]).
    pub background: u16,
    /// Fewest pixels of a blob that splits another, if above [`SPLIT_MIN_AREA`] and the
    /// detector's own area filter: the smallest blob the caller will keep.
    pub min_area: u16,
}

/// Counters for the current frame, reset by [`Detector::begin_frame`] and
/// [`Detector::begin_frame_raw10`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Pixels `>= threshold`, including those of dropped runs.
    pub pixels: u32,
    /// Runs kept, those of the higher thresholds of a split frame included (dropped runs
    /// are counted in `overflow`).
    pub runs: u32,
    /// Blobs returned by the last [`Detector::finish`].
    pub blobs: u32,
    /// Items dropped because a capacity was exceeded: runs beyond `RUNS` in a row, runs that
    /// found no free component slot (`COMPS`), blobs beyond `BLOBS` or beyond the output
    /// slice given to `finish`. A component that lost runs this way may be reported split,
    /// shrunk or not at all.
    pub overflow: u32,
}

/// Detector with the capacities sized for the PoC: 256 runs per row, 256 live components,
/// 256 blobs per frame.
pub type DefaultDetector = Detector<256, 256, 256>;

/// Component slots and run labels whose values are below the table size by construction,
/// so the component table is read and written without bounds checks.
mod slot {
    use core::ops::{Index, IndexMut};

    use super::Comp;

    /// A slot of a table of `N` components. Invariant: `self.0 < N`. Only this module makes
    /// slots ([`Slot::FIRST`], [`Slot::all`]; [`Label::slot`] gives back one it was made
    /// from), so the invariant holds for every value.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub(super) struct Slot<const N: usize>(u16);

    impl<const N: usize> Slot<N> {
        /// Slot 0, also the value of unused entries. Fails to build unless `0 < N <
        /// u16::MAX`, which leaves `u16::MAX` free for [`Label::NONE`].
        pub(super) const FIRST: Self = {
            assert!(N > 0 && N < u16::MAX as usize);
            Slot(0)
        };

        /// Every slot: `0..N`.
        pub(super) fn all() -> impl DoubleEndedIterator<Item = Self> {
            let _ = Self::FIRST;
            (0..N).map(|i| Slot(i as u16))
        }
    }

    /// The component of a run: a slot of a table of `N`, or none.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) struct Label<const N: usize>(u16);

    impl<const N: usize> Label<N> {
        pub(super) const NONE: Self = Label(u16::MAX);

        #[inline(always)]
        pub(super) fn slot(self) -> Option<Slot<N>> {
            // A label holds `NONE` or the value of a `Slot<N>` (the only constructors), and
            // no slot is `u16::MAX` (`Slot::FIRST`).
            (self != Self::NONE).then_some(Slot(self.0))
        }
    }

    impl<const N: usize> From<Slot<N>> for Label<N> {
        #[inline(always)]
        fn from(s: Slot<N>) -> Self {
            Label(s.0)
        }
    }

    /// The component table, indexed by slot.
    pub(super) struct Comps<const N: usize>([Comp<N>; N]);

    impl<const N: usize> Comps<N> {
        pub(super) const fn new() -> Self {
            Comps([Comp::ZERO; N])
        }
    }

    impl<const N: usize> Index<Slot<N>> for Comps<N> {
        type Output = Comp<N>;

        #[inline(always)]
        fn index(&self, s: Slot<N>) -> &Comp<N> {
            // SAFETY: `s.0 < N` (the invariant of `Slot`), and the table has `N` entries.
            unsafe { self.0.get_unchecked(usize::from(s.0)) }
        }
    }

    impl<const N: usize> IndexMut<Slot<N>> for Comps<N> {
        #[inline(always)]
        fn index_mut(&mut self, s: Slot<N>) -> &mut Comp<N> {
            // SAFETY: as in `index`.
            unsafe { self.0.get_unchecked_mut(usize::from(s.0)) }
        }
    }
}

use slot::{Comps, Label, Slot};

/// The MIPI CSI-2 RAW10 packing; the only code that knows the byte layout.
///
/// Four pixels take five bytes: bytes 0..=3 hold bits 9..2 of pixels 0..=3, byte 4 holds
/// their bits 1..0 (pixel 0 in bits 1..0, pixel 3 in bits 7..6). Rows have no padding.
pub mod raw10 {
    /// Bytes per group of four pixels.
    pub const GROUP_BYTES: usize = 5;
    /// Index in a group of the byte holding the top 8 bits of each pixel.
    const HIGH: [usize; 4] = [0, 1, 2, 3];
    /// Index in a group of the byte holding the low 2 bits of the pixels.
    const LOW: usize = 4;
    /// Bit position in the `LOW` byte of each pixel's low 2 bits.
    const LOW_SHIFT: [u32; 4] = [0, 2, 4, 6];

    /// The four 10-bit pixels of a group.
    #[inline(always)]
    pub fn unpack(g: &[u8; GROUP_BYTES]) -> [u32; 4] {
        let low = g[LOW] as u32;
        let px = |k: usize| (g[HIGH[k]] as u32) << 2 | (low >> LOW_SHIFT[k]) & 3;
        [px(0), px(1), px(2), px(3)]
    }

    /// Whether the top 8 bits of all four pixels of a group are `< top`.
    #[inline(always)]
    pub(super) fn top_below(g: &[u8; GROUP_BYTES], top: u8) -> bool {
        (g[HIGH[0]] < top) & (g[HIGH[1]] < top) & (g[HIGH[2]] < top) & (g[HIGH[3]] < top)
    }

    /// Packs four pixels (bits above the 10th are dropped).
    pub fn pack(px: [u16; 4]) -> [u8; GROUP_BYTES] {
        let mut g = [0u8; GROUP_BYTES];
        for (k, &p) in px.iter().enumerate() {
            g[HIGH[k]] = (p >> 2) as u8;
            g[LOW] |= ((p & 3) as u8) << LOW_SHIFT[k];
        }
        g
    }

    /// For each phase `o` (the position in its group of the first byte of an aligned word
    /// run), the masks of the run's first five words: 0x00 on the `LOW` bytes, 0xff
    /// elsewhere. The next five words repeat them (20 bytes are four whole groups).
    pub(super) const SKIP_MASKS: [[u32; 5]; GROUP_BYTES] = {
        let mut m = [[0u32; 5]; GROUP_BYTES];
        let mut o = 0;
        while o < GROUP_BYTES {
            let mut w = 0;
            while w < 5 {
                let mut bytes = [0xffu8; 4];
                let mut j = 0;
                while j < 4 {
                    if (o + 4 * w + j) % GROUP_BYTES == LOW {
                        bytes[j] = 0;
                    }
                    j += 1;
                }
                m[o][w] = u32::from_ne_bytes(bytes);
                w += 1;
            }
            o += 1;
        }
        m
    };
}

/// A horizontal run of foreground pixels `start..end` in one row with its sums, labelled
/// with a slot of a table of `N` components.
#[derive(Clone, Copy)]
struct Run<const N: usize> {
    start: u16,
    end: u16,
    label: Label<N>,
    /// Brightest value, in the units of the pixel format.
    peak: u16,
    /// Sum of pixel values.
    sw: u32,
    /// Sum of pixel value times column. Fits: 255 * (0 + .. + 2047) < 2^32 for any run in
    /// a row up to 2048 pixels (the Q11.5 limit), up to 5800 in fact (RAW10: 2896).
    swx: u32,
}

impl<const N: usize> Run<N> {
    const ZERO: Self = Run {
        start: 0,
        end: 0,
        label: Label::NONE,
        peak: 0,
        sw: 0,
        swx: 0,
    };
}

/// Accumulators of a component of a table of `N`; also a union-find node. In a split frame
/// a component of one threshold lies inside one of the threshold below (`up`) and holds what
/// the components inside it reported (`kids`, `pending`).
#[derive(Clone, Copy)]
struct Comp<const N: usize> {
    parent: Slot<N>,
    /// Last row a run was added (also marks a finished root within `end_level`).
    last_row: u16,
    peak: u16,
    /// The component of the threshold below that holds this one's runs of its last row.
    up: Label<N>,
    /// Index of its threshold.
    level: u8,
    /// Blobs reported from inside it: 0, 1 (held in `pending`) or 2 for two or more (all
    /// given out).
    kids: u8,
    area: u32,
    sw: u32,
    swx: u64,
    swy: u64,
    pending: Blob,
}

impl<const N: usize> Comp<N> {
    const ZERO: Self = Comp {
        parent: Slot::FIRST,
        last_row: 0,
        peak: 0,
        up: Label::NONE,
        level: 0,
        kids: 0,
        area: 0,
        sw: 0,
        swx: 0,
        swy: 0,
        pending: Blob {
            x: 0,
            y: 0,
            area: 0,
            peak: 0,
            sigma: 0,
        },
    };
}

/// Run builder, built once per [`Detector::push_rows_with`] call and kept across the rows of
/// the strip: a local, so its fields stay in registers in the strip loop.
struct Scan<'a, const RUNS: usize, const COMPS: usize> {
    /// Runs of the current row.
    cur: &'a mut [Run<COMPS>; RUNS],
    n: usize,
    /// Over the call; folded into `Stats` at its end.
    dropped: u32,
    pixels: u32,
    /// Threshold in the units of the pixel format.
    t: u32,
    /// Threshold of the skip: `t` for RAW8, its top 8 bits for RAW10.
    skip_t: u8,
    /// SWAR parameter of the RAW8 word test.
    add: u32,
    open: bool,
    start: u32,
    sw: u32,
    swx: u32,
    peak: u32,
}

impl<const RUNS: usize, const COMPS: usize> Scan<'_, RUNS, COMPS> {
    #[inline(always)]
    fn pixel(&mut self, x: u32, v: u32) {
        let fg = v >= self.t;
        if fg != self.open {
            if fg {
                self.open = true;
                self.start = x;
                self.sw = 0;
                self.swx = 0;
                self.peak = 0;
            } else {
                self.close(x);
            }
        }
        // Branch-free accumulation: weight 0 for a background pixel.
        let w = v & 0u32.wrapping_sub(fg as u32);
        self.sw += w;
        self.swx += w * x;
        self.peak = self.peak.max(w);
    }

    /// Ends the open run before column `x`.
    #[inline]
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn close(&mut self, x: u32) {
        self.open = false;
        self.pixels += x - self.start;
        if let Some(r) = self.cur.get_mut(self.n) {
            *r = Run {
                start: self.start as u16,
                end: x as u16,
                label: Label::NONE,
                peak: self.peak as u16,
                sw: self.sw,
                swx: self.swx,
            };
            self.n += 1;
        } else {
            self.dropped += 1;
        }
    }

    /// One RAW8 row from `x < row.len()`, an answer of the skip for the row (or for a span
    /// of rows starting with it).
    ///
    /// With no run open, the skip says where the next bright pixel can be; from there the
    /// row is read exactly: pixel by pixel up to the next aligned word, then word by word (a
    /// SWAR test, then pixel by pixel in the words that pass). That goes on until a dark
    /// word with no run open, once a bright word was read or the skip's window (which holds
    /// a bright pixel unless the row ended) was passed; then the skip takes over again, from
    /// a later place each time. The skip's answers are clamped to the row, so a wrong one
    /// gives wrong blobs, never a read outside the row.
    #[inline(always)]
    fn row8<S: Skip, const HIGH: bool>(&mut self, skip: &S, row: &[u8], mut x: usize) {
        // SAFETY: every bit pattern is a valid `u32`, and `align_to` only reinterprets the
        // middle part, which it aligns for `u32`; the slices cover `row` exactly, in order.
        let (pre, words, post) = unsafe { row.align_to::<u32>() };
        let lead = pre.len();
        let body_end = lead + words.len() * 4;
        loop {
            // No run is open, no pixel before `x` is bright, and one can be in the window.
            let end = x + WINDOW;
            let mut seen = false;
            // The bytes before the first word.
            if let Some(head) = pre.get(x..) {
                for &v in head {
                    self.pixel(x as u32, v as u32);
                    x += 1;
                }
            }
            // From here `x >= lead`. An answer inside a word: the rest of that word.
            let mut k = (x - lead) / 4;
            let skew = (x - lead) % 4;
            if skew != 0
                && let Some(w) = words.get(k)
            {
                for (i, v) in w.to_ne_bytes().into_iter().enumerate().skip(skew) {
                    self.pixel((lead + 4 * k + i) as u32, v as u32);
                }
                k += 1;
            }
            while let Some(&w) = words.get(k) {
                let wx = lead + 4 * k;
                if hot::<HIGH>(w, self.add) == 0 {
                    if self.open {
                        self.close(wx as u32);
                    } else if seen || wx >= end {
                        break;
                    }
                } else {
                    seen = true;
                    let [a, b, c, d] = w.to_ne_bytes();
                    let wx = wx as u32;
                    self.pixel(wx, a as u32);
                    self.pixel(wx + 1, b as u32);
                    self.pixel(wx + 2, c as u32);
                    self.pixel(wx + 3, d as u32);
                }
                k += 1;
            }
            if k >= words.len() {
                // The bytes after the last word, from `x` if the answer lay among them.
                let from = x.max(body_end) - body_end;
                if let Some(tail) = post.get(from..) {
                    for (i, &v) in tail.iter().enumerate() {
                        self.pixel((body_end + from + i) as u32, v as u32);
                    }
                }
                break;
            }
            x = lead + 4 * k;
            let rest = &row[x..];
            x += skip.first_bright(rest, self.skip_t).min(rest.len());
            if x >= row.len() {
                break;
            }
        }
        if self.open {
            self.close(row.len() as u32);
        }
    }

    /// One RAW10 row (`width / 4` groups of [`raw10::GROUP_BYTES`] bytes) from `x`, as in
    /// [`Self::row8`].
    ///
    /// Group by group: the skip answers with a group start (its test is on the top 8 bits
    /// against `t >> 2`: a pixel with top 8 bits `< t >> 2` is `< t` whatever its low bits),
    /// then the groups are tested exactly until a dark group with no run open, once a group
    /// with top bits at or above `t >> 2` was read (the skip stops on those, bright or not)
    /// or the skip's window was passed.
    #[inline(always)]
    fn row10<S: Skip>(&mut self, skip: &S, row: &[u8], x: usize) {
        let (groups, _) = row.as_chunks::<{ raw10::GROUP_BYTES }>();
        let (t, top) = (self.t, self.skip_t);
        let mut g = x / raw10::GROUP_BYTES;
        loop {
            // No run is open, no pixel before group `g` is bright, and one can be in the
            // window.
            let end = g + WINDOW10 / raw10::GROUP_BYTES;
            let mut seen = false;
            while let Some(group) = groups.get(g) {
                let x = (g * 4) as u32;
                if raw10::top_below(group, top) {
                    if self.open {
                        self.close(x);
                    } else if seen || g >= end {
                        break;
                    }
                } else {
                    seen = true;
                    let [a, b, c, d] = raw10::unpack(group);
                    if (a >= t) | (b >= t) | (c >= t) | (d >= t) {
                        self.pixel(x, a);
                        self.pixel(x + 1, b);
                        self.pixel(x + 2, c);
                        self.pixel(x + 3, d);
                    } else if self.open {
                        self.close(x);
                    }
                }
                g += 1;
            }
            let Some(rest) = groups.get(g..).filter(|r| !r.is_empty()) else {
                break;
            };
            let rest = rest.as_flattened();
            g += skip.first_bright10(rest, top).min(rest.len()) / raw10::GROUP_BYTES;
        }
        if self.open {
            self.close((groups.len() * 4) as u32);
        }
    }

    /// The runs of the higher thresholds `levels[1..]` of the row just scanned, appended to
    /// `cur` threshold by threshold: those of threshold `k` come from the pixels of the runs
    /// of threshold `k - 1` whose peak reaches it, and end at `ends[k]` (`ends[0]`: where
    /// the scan's runs end).
    ///
    /// Bounded like the scan: a run is read again only while the pixels read again in this
    /// row stay within `room` (the row's pixels: one more pass over the row at most). A run
    /// longer than what is left is taken whole as a run of threshold `k` instead: more
    /// pixels than that threshold has, so peaks inside it may stay together, and none is
    /// made up. A run that finds the buffer full is counted as dropped, and nothing more
    /// comes of this row.
    #[inline]
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn levels<F: Format>(
        &mut self,
        row: &[u8],
        levels: &[u16],
        ends: &mut [u16; LEVELS],
        mut room: usize,
    ) {
        // `pixels` counts those of the frame's threshold only.
        let (pixels, t) = (self.pixels, self.t);
        let mut from = 0;
        'levels: for (k, &level) in levels.iter().enumerate().skip(1) {
            let to = self.n;
            self.t = level as u32;
            for i in from..to {
                let Some(&r) = self.cur.get(i).filter(|r| r.peak >= level) else {
                    continue;
                };
                let Some(free) = self.cur.get_mut(self.n) else {
                    self.dropped += 1;
                    for e in ends.iter_mut().skip(k) {
                        *e = self.n as u16;
                    }
                    break 'levels;
                };
                let (a, b) = (r.start as usize, r.end as usize);
                if b - a > room {
                    *free = Run {
                        label: Label::NONE,
                        ..r
                    };
                    self.n += 1;
                } else {
                    room -= b - a;
                    F::within(self, row, a, b);
                }
            }
            from = to;
            for e in ends.iter_mut().skip(k) {
                *e = self.n as u16;
            }
        }
        self.pixels = pixels;
        self.t = t;
    }

    /// The runs of pixels `a..b` of a RAW8 row at the threshold `t`.
    #[inline(always)]
    fn within8(&mut self, row: &[u8], a: usize, b: usize) {
        if let Some(px) = row.get(a..b) {
            for (i, &v) in px.iter().enumerate() {
                self.pixel((a + i) as u32, v as u32);
            }
        }
        if self.open {
            self.close(b as u32);
        }
    }

    /// The runs of pixels `a..b` of a RAW10 row at the threshold `t`.
    #[inline(always)]
    fn within10(&mut self, row: &[u8], a: usize, b: usize) {
        let (groups, _) = row.as_chunks::<{ raw10::GROUP_BYTES }>();
        let mut x = a;
        while x < b {
            let g = x / 4;
            let Some(group) = groups.get(g) else {
                break;
            };
            let px = raw10::unpack(group);
            for (i, &v) in px.iter().enumerate().take(b - g * 4).skip(x % 4) {
                self.pixel((g * 4 + i) as u32, v);
            }
            x = g * 4 + 4;
        }
        if self.open {
            self.close(b as u32);
        }
    }
}

/// A pixel format: its skip and its row scan. The strip loop is generic over it, so the
/// format and the threshold class are decided once per [`Detector::push_rows_with`] call,
/// not per row.
trait Format {
    /// Pixels of a row of `stride` bytes.
    fn width(stride: usize) -> usize;
    /// The skip's threshold for the detector's threshold `t`.
    fn skip_threshold(t: u16) -> u8;
    /// The skip's answer for `bytes` (whole rows), at most `bytes.len()`.
    fn first_bright<S: Skip>(skip: &S, bytes: &[u8], skip_t: u8) -> usize;
    /// The runs of `row` from `x < row.len()`, an answer of the skip.
    fn scan<S: Skip, const RUNS: usize, const COMPS: usize>(
        scan: &mut Scan<'_, RUNS, COMPS>,
        skip: &S,
        row: &[u8],
        x: usize,
    );
    /// The runs of pixels `a..b` of `row` at the scan's threshold, for a higher threshold
    /// of a split frame.
    fn within<const RUNS: usize, const COMPS: usize>(
        scan: &mut Scan<'_, RUNS, COMPS>,
        row: &[u8],
        a: usize,
        b: usize,
    );
}

/// RAW8, with the SWAR word test of thresholds above 128 (`HIGH`) or not.
struct Raw8<const HIGH: bool>;

impl<const HIGH: bool> Format for Raw8<HIGH> {
    fn width(stride: usize) -> usize {
        stride
    }

    fn skip_threshold(t: u16) -> u8 {
        t as u8
    }

    #[inline(always)]
    fn first_bright<S: Skip>(skip: &S, bytes: &[u8], skip_t: u8) -> usize {
        skip.first_bright(bytes, skip_t).min(bytes.len())
    }

    #[inline(always)]
    fn scan<S: Skip, const RUNS: usize, const COMPS: usize>(
        scan: &mut Scan<'_, RUNS, COMPS>,
        skip: &S,
        row: &[u8],
        x: usize,
    ) {
        scan.row8::<S, HIGH>(skip, row, x);
    }

    #[inline(always)]
    fn within<const RUNS: usize, const COMPS: usize>(
        scan: &mut Scan<'_, RUNS, COMPS>,
        row: &[u8],
        a: usize,
        b: usize,
    ) {
        scan.within8(row, a, b);
    }
}

/// Packed RAW10.
struct Raw10;

impl Format for Raw10 {
    fn width(stride: usize) -> usize {
        stride / raw10::GROUP_BYTES * 4
    }

    /// A pixel `>= t` has its top 8 bits `>= t >> 2`.
    fn skip_threshold(t: u16) -> u8 {
        (t >> 2) as u8
    }

    #[inline(always)]
    fn first_bright<S: Skip>(skip: &S, bytes: &[u8], skip_t: u8) -> usize {
        skip.first_bright10(bytes, skip_t).min(bytes.len())
    }

    #[inline(always)]
    fn scan<S: Skip, const RUNS: usize, const COMPS: usize>(
        scan: &mut Scan<'_, RUNS, COMPS>,
        skip: &S,
        row: &[u8],
        x: usize,
    ) {
        scan.row10(skip, row, x);
    }

    #[inline(always)]
    fn within<const RUNS: usize, const COMPS: usize>(
        scan: &mut Scan<'_, RUNS, COMPS>,
        row: &[u8],
        a: usize,
        b: usize,
    ) {
        scan.within10(row, a, b);
    }
}

/// Component table, free list and finished blobs.
///
/// The table is indexed by [`Slot`], whose values are below `COMPS` by construction: the
/// free list starts with every slot (`reset`) and only ever holds slots, and labels, parents
/// and merged entries are copies of them. So `find`, `union`, `alloc`, `link`, `end_level`
/// and `finish` read and write the table without bounds checks.
///
/// The components of every threshold of a split frame share the table: those of one
/// threshold never touch those of another, each knows its threshold (`Comp::level`).
struct Labels<const COMPS: usize, const BLOBS: usize> {
    comps: Comps<COMPS>,
    /// Stack of free component slots.
    free: [Slot<COMPS>; COMPS],
    nfree: usize,
    /// Slots merged into another root during the current row, freed at its end.
    merged: [Slot<COMPS>; COMPS],
    nmerged: usize,
    blobs: [Blob; BLOBS],
    nblobs: usize,
    /// `Blob::peak` is the component peak shifted right by this (2 for RAW10).
    peak_shift: u8,
    /// The frame's thresholds, in the units of the pixel format: `levels[..nlevels]`.
    nlevels: u8,
    levels: [u16; LEVELS],
    /// The background the thresholds are counted from (0 without a split).
    background: u16,
    /// Fewest pixels of a blob that splits another.
    piece_min: u32,
    min_area: u32,
    max_area: u32,
    stats: Stats,
}

impl<const COMPS: usize, const BLOBS: usize> Labels<COMPS, BLOBS> {
    fn reset(&mut self) {
        // Pop order 0, 1, 2, ...: keeps the labels and thus the merge order predictable.
        for (f, s) in self.free.iter_mut().zip(Slot::all().rev()) {
            *f = s;
        }
        self.nfree = COMPS;
        self.nmerged = 0;
        self.nblobs = 0;
        self.stats = Stats::default();
    }

    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn alloc(&mut self, y: u16, level: u8) -> Option<Slot<COMPS>> {
        let n = self.nfree.checked_sub(1)?;
        self.nfree = n;
        let i = self.free[n];
        self.comps[i] = Comp {
            parent: i,
            last_row: y,
            level,
            ..Comp::ZERO
        };
        Some(i)
    }

    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn release(&mut self, i: Slot<COMPS>) {
        // Each slot is released once per allocation, so the stack never overflows.
        if let Some(f) = self.free.get_mut(self.nfree) {
            *f = i;
            self.nfree += 1;
        }
    }

    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn find(&mut self, mut i: Slot<COMPS>) -> Slot<COMPS> {
        loop {
            let p = self.comps[i].parent;
            if p == i {
                return i;
            }
            // Path halving.
            let gp = self.comps[p].parent;
            self.comps[i].parent = gp;
            i = gp;
        }
    }

    /// Merges the component of `b` into root `a` (or the other way); returns the root. Both
    /// are of the same threshold. What was reported from inside them adds up: two blobs or
    /// more are given out.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn union(&mut self, a: Slot<COMPS>, b: Slot<COMPS>) -> Slot<COMPS> {
        let b = self.find(b);
        if a == b {
            return a;
        }
        let (root, child) = if a < b { (a, b) } else { (b, a) };
        let c = self.comps[child];
        let r = &mut self.comps[root];
        r.area += c.area;
        r.sw += c.sw;
        r.swx += c.swx;
        r.swy += c.swy;
        r.peak = r.peak.max(c.peak);
        r.last_row = r.last_row.max(c.last_row);
        if c.kids != 0 {
            let (kids, held) = (r.kids, r.pending);
            if kids == 0 {
                r.kids = c.kids;
                r.pending = c.pending;
            } else {
                r.kids = 2;
                if kids == 1 {
                    self.give(held);
                }
                if c.kids == 1 {
                    self.give(c.pending);
                }
            }
        }
        self.comps[child].parent = root;
        if let Some(m) = self.merged.get_mut(self.nmerged) {
            *m = child;
            self.nmerged += 1;
        }
        root
    }

    /// Labels the runs of row `y` of the threshold `level` from the touching runs of that
    /// threshold in the row above (8-connected: a run `s..e` touches the runs above that
    /// overlap `s-1..e+1`) and adds them to their components.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn link(&mut self, prev: &[Run<COMPS>], cur: &mut [Run<COMPS>], y: u16, level: u8) {
        let mut j = 0;
        for c in cur.iter_mut() {
            while j < prev.len() && prev[j].end < c.start {
                j += 1;
            }
            let mut label = Label::NONE;
            let mut k = j;
            while k < prev.len() && prev[k].start <= c.end {
                if let Some(pl) = prev[k].label.slot() {
                    label = match label.slot() {
                        None => self.find(pl),
                        Some(l) => self.union(l, pl),
                    }
                    .into();
                }
                k += 1;
            }
            let Some(label) = label.slot().or_else(|| self.alloc(y, level)) else {
                self.stats.overflow += 1;
                c.label = Label::NONE;
                continue;
            };
            let comp = &mut self.comps[label];
            comp.area += (c.end - c.start) as u32;
            comp.sw += c.sw;
            comp.swx += c.swx as u64;
            comp.swy += c.sw as u64 * y as u64;
            comp.peak = comp.peak.max(c.peak);
            comp.last_row = y;
            c.label = label.into();
        }
    }

    /// Tells the components of the runs `cur` (one threshold of the current row, labelled)
    /// which component of the threshold below holds them: that of the run of `below` (the
    /// runs of that threshold in the same row, labelled) each lies in.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn adopt(&mut self, below: &[Run<COMPS>], cur: &[Run<COMPS>]) {
        let mut j = 0;
        for c in cur {
            while j < below.len() && below[j].end <= c.start {
                j += 1;
            }
            let Some(l) = c.label.slot() else {
                continue;
            };
            let up = match below.get(j).and_then(|b| b.label.slot()) {
                Some(u) => self.find(u).into(),
                None => Label::NONE,
            };
            let r = self.find(l);
            self.comps[r].up = up;
        }
    }

    /// Row `y` of the frame: `prev` and `cur` hold the runs of the row above and of the row,
    /// threshold after threshold (those of threshold `k` end at `prev_ends[k]` and
    /// `cur_ends[k]`). Links the runs of each threshold, tells each component which one of
    /// the threshold below holds it, then finishes the components that got no run in the
    /// row, the highest threshold first (a component reports to the one holding it, which
    /// may finish in the same row), and frees the slots merged away.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn row(
        &mut self,
        prev: &[Run<COMPS>],
        prev_ends: &[u16; LEVELS],
        cur: &mut [Run<COMPS>],
        cur_ends: &[u16; LEVELS],
        y: u16,
    ) {
        let levels = usize::from(self.nlevels).min(LEVELS);
        let (mut p0, mut c0, mut below) = (0, 0, 0);
        for k in 0..levels {
            let (p1, c1) = (usize::from(prev_ends[k]), usize::from(cur_ends[k]));
            let (lower, rest) = cur.split_at_mut(c0.min(cur.len()));
            let this = rest.get_mut(..c1.saturating_sub(c0)).unwrap_or(&mut []);
            self.link(prev.get(p0..p1).unwrap_or(&[]), this, y, k as u8);
            if k > 0 {
                self.adopt(lower.get(below..).unwrap_or(&[]), this);
            }
            (p0, below, c0) = (p1, c0, c1);
        }
        for k in (0..levels).rev() {
            let p0 = k.checked_sub(1).map_or(0, |b| usize::from(prev_ends[b]));
            let c0 = k.checked_sub(1).map_or(0, |b| usize::from(cur_ends[b]));
            let (p1, c1) = (usize::from(prev_ends[k]), usize::from(cur_ends[k]));
            self.end_level(
                prev.get(p0..p1).unwrap_or(&[]),
                cur.get_mut(c0..c1).unwrap_or(&mut []),
                y,
            );
        }
        for i in 0..self.nmerged {
            let m = self.merged[i];
            self.release(m);
        }
        self.nmerged = 0;
    }

    /// After row `y`, for the runs of one threshold: points the new runs at their roots and
    /// finishes the components of the row above that got no run in row `y`.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn end_level(&mut self, prev: &[Run<COMPS>], cur: &mut [Run<COMPS>], y: u16) {
        for c in cur.iter_mut() {
            if let Some(l) = c.label.slot() {
                c.label = self.find(l).into();
            }
        }
        for p in prev {
            if let Some(l) = p.label.slot() {
                let r = self.find(l);
                if self.comps[r].last_row != y {
                    // Marks it finished for the other runs of the same component.
                    self.comps[r].last_row = y;
                    self.finish(r);
                    self.release(r);
                }
            }
        }
    }

    /// The finished component `r` (a root). With two blobs or more from inside it, those
    /// are the blobs (given out already). Otherwise it is one blob: given out if it is of
    /// the frame's threshold (or the component holding it was lost to a full table), else
    /// reported to the component holding it if it counts as a peak ([`SPLIT_NUM`]).
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn finish(&mut self, r: Slot<COMPS>) {
        let c = self.comps[r];
        let Some(up) = c.up.slot() else {
            if c.kids < 2 && c.area >= self.min_area && c.area <= self.max_area {
                let b = self.blob(&c);
                self.push(b);
            }
            return;
        };
        let up = self.find(up);
        let holder = self.comps[up];
        if c.kids >= 2 {
            // The blobs inside `r` are the holder's too.
            self.comps[up].kids = 2;
            if holder.kids == 1 {
                self.give(holder.pending);
            }
            return;
        }
        let level = self.levels[usize::from(c.level).min(LEVELS - 1)];
        let above = |v: u16| u32::from(v.saturating_sub(self.background));
        if SPLIT_DEN * above(c.peak) < SPLIT_NUM * above(level) || c.area < self.piece_min {
            return;
        }
        let b = self.blob(&c);
        match holder.kids {
            0 => {
                self.comps[up].kids = 1;
                self.comps[up].pending = b;
            }
            1 => {
                self.comps[up].kids = 2;
                self.give(holder.pending);
                self.give(b);
            }
            _ => self.give(b),
        }
    }

    /// The blob of component `c`. Its size ([`spot_sigma`]): the light above the background
    /// over the peak's height above the component's threshold, taken as a quarter of the
    /// peak above the background at least (a component that barely passes its threshold
    /// says little).
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn blob(&self, c: &Comp<COMPS>) -> Blob {
        let level = self.levels[usize::from(c.level).min(LEVELS - 1)];
        let bg = u32::from(self.background);
        let peak = u32::from(c.peak);
        let flux = c.sw.saturating_sub(bg.saturating_mul(c.area));
        let height = peak
            .saturating_sub(u32::from(level))
            .max(peak.saturating_sub(bg) / 4);
        Blob {
            x: q5(c.swx, c.sw),
            y: q5(c.swy, c.sw),
            area: c.area.min(u16::MAX as u32) as u16,
            sigma: spot_sigma(flux, height),
            peak: (c.peak >> self.peak_shift) as u8,
        }
    }

    /// Gives out a blob reported from inside another, if the area filter keeps it (on its
    /// saturated area).
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn give(&mut self, b: Blob) {
        if u32::from(b.area) >= self.min_area && u32::from(b.area) <= self.max_area {
            self.push(b);
        }
    }

    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn push(&mut self, blob: Blob) {
        if let Some(b) = self.blobs.get_mut(self.nblobs) {
            *b = blob;
            self.nblobs += 1;
        } else {
            self.stats.overflow += 1;
        }
    }
}

/// The weighted mean `s / sw` in 1/32 pixel, rounded to nearest and saturated:
/// `min((32 s + sw / 2) / sw, u16::MAX)`, without a 64-bit division (on RV32 a call to
/// `__udivdi3` for each coordinate of each blob).
///
/// A quotient of 2^16 or more is `n >= sw * 2^16`, a compare. Below that, `n < 2^32` takes
/// one 32-bit division, the rest [`div_q16`]. `sw == 0` (only if the `u32` sum wrapped)
/// falls back to the 64-bit division, as before.
#[inline]
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn q5(s: u64, sw: u32) -> u16 {
    let n = s * 32 + u64::from(sw / 2);
    let d = u64::from(sw);
    if sw == 0 {
        return (n / d).min(u16::MAX as u64) as u16;
    }
    if n >= d << 16 {
        return u16::MAX;
    }
    match u32::try_from(n) {
        Ok(n) => (n / sw) as u16,
        Err(_) => div_q16(n, sw),
    }
}

/// `n / d` for `n < d * 2^16` (a quotient below 2^16) and `n >= 2^32` (so `d > 2^16`), with
/// 32-bit operations: one step of long division in base `B = 2^16` (Knuth, TAOCP vol. 2,
/// 4.3.1, algorithm D, steps D1 to D3), exact for a divisor of two digits.
///
/// Proof. Normalise: `k = d.leading_zeros()`, `dn = d << k` has its top bit set, so its
/// high digit `v1 = dn >> 16 >= B / 2`; `nn = n << k` has the same quotient and
/// `nn < dn * B < 2^48`. Write `nn = u * B + u0` (`u0 < B`) and `dn = v1 * B + v0`
/// (`v0 < B`); `u < dn` because `nn < dn * B`, so `u` fits in 32 bits. Start from
/// `q = min(u / v1, B - 1)` and `r = u - q * v1`. For any `q`,
/// `nn - q * dn = (u - q * v1) * B + u0 - q * v0 = r * B + u0 - q * v0`, so `q * dn <= nn`
/// exactly when `q * v0 <= r * B + u0`: the loop lowers `q` by one (and raises `r` by `v1`)
/// while `q * dn > nn`, and ends on the largest `q <= q_start` with `q * dn <= nn`. That is
/// the quotient, because the quotient is at most `u / v1` (`q * dn <= nn` implies
/// `q * v1 <= u`) and below `B`. No overflow: `q < B` and `v0 < B` make `q * v0 < 2^32`;
/// `r < B` is tested first, so `r * B + u0 < 2^32`; once `r >= B`, `r * B + u0 >= 2^32`,
/// above `q * v0`, and the test fails as it must. With `v1 >= B / 2`, `q_start` is at most 2
/// above the quotient (Knuth's theorem B): two passes at most.
#[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
fn div_q16(n: u64, d: u32) -> u16 {
    const B: u32 = 1 << 16;
    let k = d.leading_zeros();
    let dn = d << k;
    let nn = n << k;
    let (v1, v0) = (dn >> 16, dn & (B - 1));
    let u = (nn >> 16) as u32;
    let u0 = nn as u32 & (B - 1);
    let mut q = (u / v1).min(B - 1);
    let mut r = u - q * v1;
    while r < B && q * v0 > (r << 16 | u0) {
        q -= 1;
        r += v1;
    }
    q as u16
}

/// Blob detector state with fixed capacities: `RUNS` runs per row, `COMPS` components
/// alive at once (those crossing the current row; merged ones count until the row ends),
/// `BLOBS` blobs per frame. Build it once (`new` is `const`, so it can live in a static).
pub struct Detector<const RUNS: usize, const COMPS: usize, const BLOBS: usize> {
    /// Bytes per row.
    stride: usize,
    /// In the units of the pixel format.
    threshold: u16,
    /// SWAR parameters of the exact word test of RAW8 rows.
    add: u32,
    high: bool,
    raw10: bool,
    next_row: u16,
    /// The runs of the row above and of the current row, threshold after threshold.
    runs: [[Run<COMPS>; RUNS]; 2],
    /// Where the runs of each threshold end in `runs`.
    ends: [[u16; LEVELS]; 2],
    /// Index into `runs` of the row above.
    prev: usize,
    /// How the frames begun next are split, if they are.
    split: Option<Split>,
    lab: Labels<COMPS, BLOBS>,
}

impl<const RUNS: usize, const COMPS: usize, const BLOBS: usize> Default
    for Detector<RUNS, COMPS, BLOBS>
{
    fn default() -> Self {
        Self::new()
    }
}

impl<const RUNS: usize, const COMPS: usize, const BLOBS: usize> Detector<RUNS, COMPS, BLOBS> {
    const CAPACITY_OK: () =
        assert!(COMPS > 0 && COMPS < u16::MAX as usize && RUNS <= u16::MAX as usize);

    /// A detector with no area filter (every blob of at least one pixel is kept).
    pub const fn new() -> Self {
        let () = Self::CAPACITY_OK;
        Self {
            stride: 0,
            threshold: 1,
            add: 0,
            high: false,
            raw10: false,
            next_row: 0,
            runs: [[Run::ZERO; RUNS]; 2],
            ends: [[0; LEVELS]; 2],
            prev: 0,
            split: None,
            lab: Labels {
                comps: Comps::new(),
                free: [Slot::FIRST; COMPS],
                nfree: 0,
                merged: [Slot::FIRST; COMPS],
                nmerged: 0,
                blobs: [Blob {
                    x: 0,
                    y: 0,
                    area: 0,
                    peak: 0,
                    sigma: 0,
                }; BLOBS],
                nblobs: 0,
                peak_shift: 0,
                nlevels: 1,
                levels: [1; LEVELS],
                background: 0,
                piece_min: SPLIT_MIN_AREA,
                min_area: 1,
                max_area: u32::MAX,
                stats: Stats {
                    pixels: 0,
                    runs: 0,
                    blobs: 0,
                    overflow: 0,
                },
            },
        }
    }

    /// Keeps only blobs with `min_area <= area <= max_area` pixels (before saturation; a
    /// blob split out of another: on its saturated area). Applies from the next blob
    /// finished; to what splits a blob ([`Self::set_split`]), from the next frame begun.
    pub fn set_area_filter(&mut self, min_area: u32, max_area: u32) {
        self.lab.min_area = min_area;
        self.lab.max_area = max_area;
    }

    /// Splits the blobs of the frames begun from now on by their peaks (`None`, the start:
    /// plain connected components). A threshold at or below the background does not split,
    /// and counts the background as 0.
    ///
    /// The frame is then also cut at up to [`LEVELS`]` - 1` higher thresholds, each twice as
    /// far above the background as the one before, up to the format's largest value. The
    /// components of a threshold lie inside those of the threshold below. A component that
    /// holds two components or more of the next threshold that count as peaks (peak at
    /// least [`SPLIT_NUM`]` / `[`SPLIT_DEN`] of that threshold above the background, at
    /// least [`SPLIT_MIN_AREA`] pixels, [`Split::min_area`] and the area filter's fewest)
    /// is not a blob: they are, each cut the same way in turn. So two markers whose light
    /// touches come out as two blobs when the dip between them is below 2/5 of the lower
    /// peak (and one of the thresholds lies between), and never when it is above 4/5; a
    /// marker in the skirt of a brighter one likewise. Each blob's centroid, area, peak
    /// and size are those of its own component.
    ///
    /// The higher thresholds read again, in each row, the runs that reach them: at most
    /// the row's pixels once more, so a row costs at most one more pass. A run past that
    /// counts whole for the next threshold: in rows with that much light, peaks may stay
    /// together. Their runs share the `RUNS` of a row with the frame's own: past those,
    /// runs are dropped and counted ([`Stats::overflow`]).
    pub fn set_split(&mut self, split: Option<Split>) {
        self.split = split;
    }

    /// Starts a RAW8 frame of rows of `width` pixels (at most 65535; the Q11.5 centroid
    /// saturates beyond 2048). A `threshold` of 0 is taken as 1.
    pub fn begin_frame(&mut self, width: u16, threshold: u8) {
        let t = threshold.max(1);
        let (add, high) = swar_params(t);
        self.begin(width as usize, t as u16, u8::MAX as u16);
        self.add = add;
        self.high = high;
        self.raw10 = false;
        self.lab.peak_shift = 0;
    }

    /// A frame of rows of `stride` bytes at `threshold`, pixels up to `max`.
    fn begin(&mut self, stride: usize, threshold: u16, max: u16) {
        self.stride = stride;
        self.threshold = threshold;
        self.next_row = 0;
        self.ends = [[0; LEVELS]; 2];
        self.lab.reset();
        let lab = &mut self.lab;
        lab.levels = [threshold; LEVELS];
        lab.nlevels = 1;
        lab.background = 0;
        if let Some(s) = self.split.filter(|s| s.background < threshold) {
            lab.background = s.background;
            lab.piece_min = lab.min_area.max(u32::from(s.min_area)).max(SPLIT_MIN_AREA);
            let mut above = u32::from(threshold - s.background);
            for level in lab.levels.iter_mut().skip(1) {
                above *= 2;
                let t = u32::from(s.background) + above;
                if t > u32::from(max) {
                    break;
                }
                *level = t as u16;
                lab.nlevels += 1;
            }
        }
    }

    /// Processes whole rows (`rows.len()` a multiple of the row length: `width` bytes for
    /// RAW8, `width / 4 * 5` for RAW10; a partial last row is ignored), the first being row
    /// `first_row`, in the format the frame was begun with. Strips normally follow each
    /// other; if `first_row` is not the row after the last one pushed, the blobs crossing
    /// the last row are finished first, as if a dark row lay between. Uses the [`Scalar`]
    /// skip.
    pub fn push_rows(&mut self, rows: &[u8], first_row: u16) {
        self.push_rows_with(&Scalar, rows, first_row);
    }

    /// [`Self::push_rows`] with the dark skip `skip`. Every skip that keeps the [`Skip`]
    /// contract gives the same blobs and `Stats`.
    ///
    /// While no component crosses the row above, a dark row changes nothing but the row
    /// counter; so the skip then runs once over the rest of the strip (its rows are
    /// contiguous), and the rows before its answer are counted as passed without being
    /// visited. Row by row work resumes in the row holding the answer, from the answer.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    pub fn push_rows_with<S: Skip>(&mut self, skip: &S, rows: &[u8], first_row: u16) {
        self.push_rows_until(skip, rows, first_row, || false);
    }

    /// [`Self::push_rows_with`] that can stop early, at a row boundary: `stop` is asked
    /// before each row is scanned and before each jump of the skip over dark rows, and the
    /// call returns at the first `true`. Returns the rows done (`rows.len() / stride` if
    /// `stop` never answered `true`): the rows before the one the call stopped at, those
    /// the skip passed included.
    ///
    /// Between two questions the call does at most one row's scan, labelling and end of
    /// row, or one skip jump over at most the rest of `rows`; nothing after the `true`. So
    /// the work after the last `false` is bounded by the larger of one row's worst case
    /// (every other pixel bright) and one skip over `rows`, whatever the pixels.
    ///
    /// Pushing the remaining rows afterwards (`first_row` + the rows done) continues exactly
    /// where the call stopped: the blobs and `Stats` are bit-identical to one unbounded call.
    /// Rows never pushed (a frame cut short) are, as with any gap, dark rows for
    /// [`Self::finish`].
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    pub fn push_rows_until<S: Skip, P: FnMut() -> bool>(
        &mut self,
        skip: &S,
        rows: &[u8],
        first_row: u16,
        stop: P,
    ) -> usize {
        let Some(stride) = NonZeroUsize::new(self.stride) else {
            return 0;
        };
        if first_row != self.next_row {
            self.flush();
        }
        match (self.raw10, self.high) {
            (false, false) => self.strip::<S, Raw8<false>, P>(skip, rows, stride, first_row, stop),
            (false, true) => self.strip::<S, Raw8<true>, P>(skip, rows, stride, first_row, stop),
            (true, _) => self.strip::<S, Raw10, P>(skip, rows, stride, first_row, stop),
        }
    }

    /// The rows of one [`Self::push_rows_until`] call in format `F`. The scan state and the
    /// two run buffers are taken once for the call; the rows swap the buffers by reference.
    /// Returns the rows done.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn strip<S: Skip, F: Format, P: FnMut() -> bool>(
        &mut self,
        skip: &S,
        rows: &[u8],
        stride: NonZeroUsize,
        first_row: u16,
        mut stop: P,
    ) -> usize {
        let Self {
            threshold,
            add,
            next_row,
            runs,
            ends,
            prev: parity,
            lab,
            ..
        } = self;
        let [a, b] = runs;
        let (mut prev, cur) = if *parity == 0 { (a, b) } else { (b, a) };
        let levels = usize::from(lab.nlevels).clamp(1, LEVELS);
        let thresholds = lab.levels;
        let mut prev_ends = ends[*parity];
        let mut n_prev = usize::from(prev_ends[levels - 1]);
        let mut scan = Scan {
            cur,
            n: 0,
            dropped: 0,
            pixels: 0,
            t: *threshold as u32,
            skip_t: F::skip_threshold(*threshold),
            add: *add,
            open: false,
            start: 0,
            sw: 0,
            swx: 0,
            peak: 0,
        };
        let mut nruns_sum = 0;
        let mut y = first_row;
        let mut rest = rows;
        loop {
            let mut first = None;
            if n_prev == 0 {
                if stop() {
                    break;
                }
                let o = F::first_bright(skip, rest, scan.skip_t);
                // `o % stride <= o <= rest.len()`: the slice cannot fail.
                let x = o % stride;
                rest = &rest[o - x..];
                y = y.wrapping_add((o / stride) as u16);
                first = Some(x);
            }
            let Some((row, tail)) = rest.split_at_checked(stride.get()) else {
                break;
            };
            if stop() {
                break;
            }
            let x = first.unwrap_or_else(|| F::first_bright(skip, row, scan.skip_t));
            if x < row.len() {
                F::scan(&mut scan, skip, row, x);
            }
            let mut cur_ends = [scan.n as u16; LEVELS];
            if levels > 1 && scan.n != 0 {
                let room = F::width(stride.get());
                scan.levels::<F>(row, &thresholds[..levels], &mut cur_ends, room);
            }
            let n = scan.n;
            // Both rows empty: nothing to link, nothing to finish.
            if n != 0 || n_prev != 0 {
                lab.row(
                    &prev[..n_prev],
                    &prev_ends,
                    &mut scan.cur[..n],
                    &cur_ends,
                    y,
                );
            }
            nruns_sum += n as u32;
            core::mem::swap(&mut prev, &mut scan.cur);
            *parity ^= 1;
            n_prev = n;
            prev_ends = cur_ends;
            scan.n = 0;
            rest = tail;
            y = y.wrapping_add(1);
        }
        ends[*parity] = prev_ends;
        *next_row = y;
        let stats = &mut lab.stats;
        stats.pixels += scan.pixels;
        stats.runs += nruns_sum;
        stats.overflow += scan.dropped;
        usize::from(y.wrapping_sub(first_row))
    }

    /// Finishes the frame: writes the blobs into `out` (at most `out.len()`, the rest is
    /// counted in `overflow`) and returns how many.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    pub fn finish(&mut self, out: &mut [Blob]) -> usize {
        self.flush();
        let found = self.lab.nblobs;
        let n = found.min(out.len());
        out[..n].copy_from_slice(&self.lab.blobs[..n]);
        self.lab.stats.overflow += (found - n) as u32;
        self.lab.stats.blobs = n as u32;
        self.lab.nblobs = 0;
        n
    }

    /// Detects the blobs of a whole frame of `frame.len() / width` rows.
    pub fn process_frame(
        &mut self,
        frame: &[u8],
        width: u16,
        threshold: u8,
        out: &mut [Blob],
    ) -> usize {
        self.process_frame_with(&Scalar, frame, width, threshold, out)
    }

    /// [`Self::process_frame`] with the dark skip `skip`.
    pub fn process_frame_with<S: Skip>(
        &mut self,
        skip: &S,
        frame: &[u8],
        width: u16,
        threshold: u8,
        out: &mut [Blob],
    ) -> usize {
        self.begin_frame(width, threshold);
        self.push_rows_with(skip, frame, 0);
        self.finish(out)
    }

    /// Starts a RAW10 frame of rows of `width` pixels, packed as described in [`raw10`]:
    /// each row is `width / 4 * 5` bytes. `width` must be a multiple of 4 (a remainder is
    /// ignored, which misreads the rows). `threshold` is in 10-bit units, clamped to
    /// `1..=1023`. Rows then go to [`Self::push_rows`] and the frame ends with
    /// [`Self::finish`], as for RAW8. Centroids weigh pixels by their 10-bit value;
    /// `Blob::peak` holds the top 8 bits of the brightest pixel.
    pub fn begin_frame_raw10(&mut self, width: u16, threshold: u16) {
        let t = threshold.clamp(1, 1023);
        self.begin(width as usize / 4 * raw10::GROUP_BYTES, t, 1023);
        self.raw10 = true;
        self.lab.peak_shift = 2;
    }

    /// Detects the blobs of a whole RAW10 frame of `frame.len() / (width / 4 * 5)` rows
    /// (see [`Self::begin_frame_raw10`]).
    pub fn process_frame_raw10(
        &mut self,
        frame: &[u8],
        width: u16,
        threshold: u16,
        out: &mut [Blob],
    ) -> usize {
        self.process_frame_raw10_with(&Scalar, frame, width, threshold, out)
    }

    /// [`Self::process_frame_raw10`] with the dark skip `skip`.
    pub fn process_frame_raw10_with<S: Skip>(
        &mut self,
        skip: &S,
        frame: &[u8],
        width: u16,
        threshold: u16,
        out: &mut [Blob],
    ) -> usize {
        self.begin_frame_raw10(width, threshold);
        self.push_rows_with(skip, frame, 0);
        self.finish(out)
    }

    /// Counters of the current (or last finished) frame.
    pub fn stats(&self) -> Stats {
        self.lab.stats
    }

    /// Finishes every component crossing the last row pushed.
    #[cfg_attr(feature = "ram", unsafe(link_section = ".rwtext"))]
    fn flush(&mut self) {
        let ends = self.ends[self.prev];
        let n = usize::from(ends[usize::from(self.lab.nlevels).clamp(1, LEVELS) - 1]);
        let prev = &self.runs[self.prev][..n.min(RUNS)];
        self.lab
            .row(prev, &ends, &mut [], &[0; LEVELS], self.next_row);
        self.ends[self.prev] = [0; LEVELS];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 32;
    const H: usize = 16;

    fn frame(pixels: &[(usize, usize, u8)]) -> [u8; W * H] {
        let mut f = [0u8; W * H];
        for &(x, y, v) in pixels {
            f[y * W + x] = v;
        }
        f
    }

    fn detect(f: &[u8], t: u8, out: &mut [Blob]) -> usize {
        DefaultDetector::new().process_frame(f, W as u16, t, out)
    }

    #[test]
    fn single_pixel() {
        let f = frame(&[(5, 3, 200)]);
        let mut out = [Blob::default(); 4];
        assert_eq!(detect(&f, 100, &mut out), 1);
        assert_eq!(
            out[0],
            Blob {
                x: 5 * 32,
                y: 3 * 32,
                area: 1,
                sigma: spot_sigma(200, 100),
                peak: 200,
            }
        );
    }

    #[test]
    fn weighted_centroid() {
        // Threshold 100 keeps 4 pixels and drops the 99. sw = 575,
        // swx = 4 * 100 + 5 * 200 + 6 * 125 + 5 * 150 = 2900 -> 5.0435 px -> 161.39 -> 161,
        // swy = 2 * 425 + 3 * 150 = 1300 -> 2.2609 px -> 72.35 -> 72.
        let f = frame(&[
            (4, 2, 100),
            (5, 2, 200),
            (6, 2, 125),
            (5, 3, 150),
            (9, 2, 99),
        ]);
        let mut out = [Blob::default(); 4];
        assert_eq!(detect(&f, 100, &mut out), 1);
        assert_eq!(
            out[0],
            Blob {
                x: 161,
                y: 72,
                area: 4,
                sigma: spot_sigma(575, 100),
                peak: 200,
            }
        );
    }

    #[test]
    fn u_shape_merges() {
        // Bars at columns 2 and 6 on rows 1..=4, joined by row 5 (columns 2..=6).
        let mut px = [(0, 0, 0); 13];
        for (i, y) in (1..=4).enumerate() {
            px[2 * i] = (2, y, 200);
            px[2 * i + 1] = (6, y, 200);
        }
        for (i, x) in (2..=6).enumerate() {
            px[8 + i] = (x, 5, 200);
        }
        let f = frame(&px);
        let mut out = [Blob::default(); 4];
        let mut det = DefaultDetector::new();
        assert_eq!(det.process_frame(&f, W as u16, 128, &mut out), 1);
        // x: (4 * 2 + 4 * 6 + 20) / 13 = 4 -> 128; y: (2 * 10 + 5 * 5) / 13 * 32 = 110.8.
        assert_eq!(
            out[0],
            Blob {
                x: 128,
                y: 111,
                area: 13,
                sigma: spot_sigma(13 * 200, 72),
                peak: 200,
            }
        );
        assert_eq!(det.stats().runs, 9);
        assert_eq!(det.stats().overflow, 0);
    }

    #[test]
    fn diagonal_touch_is_connected() {
        // (3,2)-(4,3) touch diagonally, (10,2)-(9,3) anti-diagonally, (20,2)/(22,3) do not.
        let f = frame(&[
            (3, 2, 200),
            (4, 3, 200),
            (10, 2, 200),
            (9, 3, 200),
            (20, 2, 200),
            (22, 3, 200),
        ]);
        let mut out = [Blob::default(); 8];
        let n = detect(&f, 128, &mut out);
        assert_eq!(n, 4);
        let areas = out[..n].iter().filter(|b| b.area == 2).count();
        assert_eq!(areas, 2);
        assert!(out[..n].contains(&Blob {
            x: 112,
            y: 80,
            area: 2,
            sigma: spot_sigma(400, 72),
            peak: 200,
        }));
        assert!(out[..n].contains(&Blob {
            x: 304,
            y: 80,
            area: 2,
            sigma: spot_sigma(400, 72),
            peak: 200,
        }));
    }

    #[test]
    fn border_blobs() {
        let f = frame(&[
            (0, 0, 200),
            (1, 0, 200),
            (0, 1, 200),
            (W - 1, H - 1, 180),
            (W - 2, H - 1, 180),
            (W - 1, 0, 150),
        ]);
        let mut out = [Blob::default(); 8];
        let n = detect(&f, 128, &mut out);
        assert_eq!(n, 3);
        assert!(out[..n].contains(&Blob {
            x: 11,
            y: 11,
            area: 3,
            sigma: spot_sigma(600, 72),
            peak: 200,
        }));
        assert!(out[..n].contains(&Blob {
            x: (W as u16 - 1) * 32 - 16,
            y: (H as u16 - 1) * 32,
            area: 2,
            sigma: spot_sigma(360, 52),
            peak: 180,
        }));
        assert!(out[..n].contains(&Blob {
            x: (W as u16 - 1) * 32,
            y: 0,
            area: 1,
            // The peak barely passes the threshold: a quarter of it counts as its height.
            sigma: spot_sigma(150, 37),
            peak: 150,
        }));
    }

    #[test]
    fn strips_match_whole_frame_at_any_alignment() {
        let mut px = [(0, 0, 0); 24];
        let mut i = 0;
        for y in 2..=7 {
            for x in [3, 4, 5, 17] {
                px[i] = (x + y, y, 128 + ((x * 7 + y * 13) % 128) as u8);
                i += 1;
            }
        }
        let f = frame(&px);
        let mut want = [Blob::default(); 16];
        let mut det = DefaultDetector::new();
        let nw = det.process_frame(&f, W as u16, 128, &mut want);
        assert_eq!(nw, 2);
        let want_stats = det.stats();
        for offset in 0..4 {
            let mut buf = [0u8; W * H + 4];
            buf[offset..offset + W * H].copy_from_slice(&f);
            let f = &buf[offset..offset + W * H];
            for strip in [1, 3, 4, 5, H] {
                det.begin_frame(W as u16, 128);
                let mut y = 0;
                while y < H {
                    let n = strip.min(H - y);
                    det.push_rows(&f[y * W..(y + n) * W], y as u16);
                    y += n;
                }
                let mut got = [Blob::default(); 16];
                let ng = det.finish(&mut got);
                assert_eq!(got[..ng], want[..nw], "offset {offset} strip {strip}");
                assert_eq!(det.stats(), want_stats);
            }
        }
    }

    #[test]
    fn capacity_overflow_is_counted() {
        // Row 1: 8 single-pixel runs; capacity 4 runs, 3 components, 2 blobs.
        let mut px = [(0, 0, 0); 8];
        for (i, p) in px.iter_mut().enumerate() {
            *p = (i * 4, 1, 200);
        }
        let f = frame(&px);
        let mut det = Detector::<4, 3, 2>::new();
        let mut out = [Blob::default(); 8];
        let n = det.process_frame(&f, W as u16, 128, &mut out);
        assert_eq!(n, 2);
        let s = det.stats();
        assert_eq!(s.pixels, 8);
        assert_eq!(s.runs, 4);
        // 4 runs dropped, 1 run without a component, 1 blob without room.
        assert_eq!(s.overflow, 6);
        assert_eq!(s.blobs, 2);
        // A too short output slice counts as well.
        let n = det.process_frame(&f, W as u16, 128, &mut out[..1]);
        assert_eq!(n, 1);
        assert_eq!(det.stats().overflow, 7);
    }

    #[test]
    fn area_filter() {
        let f = frame(&[
            (2, 2, 200),
            (10, 2, 200),
            (11, 2, 200),
            (20, 2, 200),
            (20, 3, 200),
            (21, 3, 200),
        ]);
        let mut det = DefaultDetector::new();
        det.set_area_filter(2, 2);
        let mut out = [Blob::default(); 4];
        assert_eq!(det.process_frame(&f, W as u16, 128, &mut out), 1);
        assert_eq!(out[0].area, 2);
    }

    #[test]
    fn threshold_edges() {
        let f = frame(&[(1, 1, 128), (5, 5, 129), (9, 9, 255), (13, 13, 127)]);
        let mut out = [Blob::default(); 8];
        assert_eq!(detect(&f, 128, &mut out), 3);
        assert_eq!(detect(&f, 129, &mut out), 2);
        assert_eq!(detect(&f, 255, &mut out), 1);
        assert_eq!(detect(&f, 0, &mut out), 4);
    }

    #[test]
    fn random_frames_match_flood_fill() {
        const RW: usize = 128;
        const RH: usize = 32;
        let mut s = 0x2545_f491u32;
        let mut rnd = move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        let mut det = Detector::<256, 256, 1024>::new();
        let mut buf = [0u8; RW * RH + 4];
        let mut got = [Blob::default(); 1024];
        for round in 0..200 {
            let offset = round % 4;
            let t = [100u8, 128, 129, 200][round / 4 % 4];
            // Density from sparse to dense; dark pixels 0x40 or 0x24 so that the OR of a
            // dark chunk reaches 100 (exercises the exact fallback of the skip).
            let density = 2 + round as u32 % 7;
            let f = &mut buf[offset..offset + RW * RH];
            for p in f.iter_mut() {
                let r = rnd();
                *p = if r % 16 < density {
                    t.saturating_add((r >> 8) as u8 % (256 - t as u32) as u8)
                } else if r & 0x100 != 0 {
                    0x40
                } else {
                    0x24
                };
            }
            let f = &buf[offset..offset + RW * RH];
            let want = reference::flood(f, RW, t.into(), 0);
            let ng = det.process_frame(f, RW as u16, t, &mut got);
            assert_eq!(det.stats().overflow, 0, "round {round}");
            assert_eq!(got[..ng], want[..], "round {round}");
        }
    }

    #[test]
    fn all_dark() {
        let f = [0u8; W * H];
        let mut det = DefaultDetector::new();
        let mut out = [Blob::default(); 4];
        assert_eq!(det.process_frame(&f, W as u16, 1, &mut out), 0);
        assert_eq!(det.stats(), Stats::default());
    }

    /// [`q5`] against the 64-bit division it replaces, on random and extreme sums.
    #[test]
    fn q5_matches_u64_division() {
        let want = |s: u64, sw: u32| {
            let sw = u64::from(sw);
            ((s * 32 + sw / 2) / sw).min(u16::MAX as u64) as u16
        };
        let mut long = 0;
        let mut check = |s: u64, sw: u32| {
            let n = s * 32 + u64::from(sw / 2);
            long += (n >> 32 != 0 && n < u64::from(sw) << 16) as u32;
            assert_eq!(q5(s, sw), want(s, sw), "s {s} sw {sw}");
        };
        // An all-bright 1280x800 frame, RAW8 and RAW10: the largest sums the board sees.
        let (w, h) = (1280u64, 800u64);
        let (sx, sy) = (w * (w - 1) / 2 * h, h * (h - 1) / 2 * w);
        for v in [255u64, 1023] {
            let sw = (v * w * h) as u32;
            for s in [
                v * sx,
                v * sy,
                0,
                1,
                u64::from(sw) * 2047,
                u64::from(sw) * 2048,
            ] {
                check(s, sw);
            }
        }
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut sws: std::vec::Vec<u32> = std::vec![1, 2, 3, 255, 65535, 65536, 65537];
        sws.extend([1 << 31, (1 << 31) + 1, u32::MAX - 1, u32::MAX]);
        for bits in 1..=32 {
            for _ in 0..40 {
                sws.push((rnd() as u32 >> (32 - bits)) | 1 << (bits - 1));
            }
        }
        for &sw in &sws {
            let d = u64::from(sw);
            // Both ends of each path: the saturation edge `n = d * 2^16`, the 32-bit edge
            // `n = 2^32`, then centroids anywhere in 0..4096 pixels (2048 and up saturate).
            let edges = [(d << 16) - d / 2, 1 << 32];
            for e in edges {
                for delta in 0..64u64 {
                    let s = (e / 32).wrapping_add(delta).wrapping_sub(32);
                    if s < 1 << 58 {
                        check(s, sw);
                    }
                }
            }
            for _ in 0..2000 {
                let px = rnd() % (4096 * 32);
                let s = d * px / 32 + rnd() % (d + 1);
                check(s, sw);
            }
        }
        assert!(long > 10_000, "the 32-bit-step path ran {long} times");
    }

    #[test]
    fn all_bright() {
        let f = [255u8; W * H];
        let mut det = DefaultDetector::new();
        let mut out = [Blob::default(); 4];
        assert_eq!(det.process_frame(&f, W as u16, 200, &mut out), 1);
        assert_eq!(
            out[0],
            Blob {
                x: 496,
                y: 240,
                area: (W * H) as u16,
                sigma: spot_sigma(255 * (W * H) as u32, 63),
                peak: 255,
            }
        );
        let s = det.stats();
        assert_eq!(
            (s.pixels, s.runs, s.overflow),
            ((W * H) as u32, H as u32, 0)
        );
    }

    // RAW10: 10-bit pixels, packed per row.
    const W10: usize = 64;
    const H10: usize = 16;
    const S10: usize = W10 / 4 * raw10::GROUP_BYTES;

    fn pack(px: &[u16], out: &mut [u8]) {
        let groups = out.as_chunks_mut::<{ raw10::GROUP_BYTES }>().0;
        for (p, g) in px.as_chunks::<4>().0.iter().zip(groups) {
            *g = raw10::pack(*p);
        }
    }

    fn frame10(pixels: &[(usize, usize, u16)], fill: u16) -> [u8; S10 * H10] {
        let mut px = [fill; W10 * H10];
        for &(x, y, v) in pixels {
            px[y * W10 + x] = v;
        }
        let mut f = [0u8; S10 * H10];
        pack(&px, &mut f);
        f
    }

    fn detect10(f: &[u8], t: u16, out: &mut [Blob]) -> usize {
        DefaultDetector::new().process_frame_raw10(f, W10 as u16, t, out)
    }

    #[test]
    fn raw10_pack_round_trip() {
        // The layout itself: top bits in bytes 0..=3, low bits of pixel 0 in bits 1..0.
        assert_eq!(
            raw10::pack([0x3ff, 0x000, 0x001, 0x2aa]),
            [0xff, 0x00, 0x00, 0xaa, 0b10_01_00_11]
        );
        for v in 0..1024u16 {
            for k in 0..4 {
                let mut px = [(v * 7 + 300) % 1024, v ^ 0x155, 1023 - v, v / 3];
                px[k] = v;
                let back = raw10::unpack(&raw10::pack(px));
                assert_eq!(back, px.map(u32::from), "{px:?}");
            }
        }
    }

    #[test]
    fn raw10_skip_masks_cover_low_bytes() {
        for (o, m) in raw10::SKIP_MASKS.iter().enumerate() {
            for (b, byte) in m.iter().flat_map(|w| w.to_ne_bytes()).enumerate() {
                let low = (o + b) % raw10::GROUP_BYTES == 4;
                assert_eq!(byte, if low { 0 } else { 0xff }, "phase {o} byte {b}");
            }
        }
    }

    #[test]
    fn raw10_single_pixel() {
        let f = frame10(&[(37, 5, 1001)], 0);
        let mut out = [Blob::default(); 4];
        assert_eq!(detect10(&f, 512, &mut out), 1);
        assert_eq!(
            out[0],
            Blob {
                x: 37 * 32,
                y: 5 * 32,
                area: 1,
                sigma: spot_sigma(1001, 489),
                peak: 250,
            }
        );
    }

    #[test]
    fn raw10_weights_use_low_bits() {
        // 10-bit weights 4, 7, 6: sw = 17, swx = 16 + 35 + 30 = 81 -> 4.765 px -> 152.5
        // -> 152, swy = 8 + 14 + 18 = 40 -> 2.353 px -> 75.3 -> 75. With the top 8 bits
        // only (1, 1, 1) x would be 14 / 3 px -> 149.
        let f = frame10(&[(4, 2, 4), (5, 2, 7), (5, 3, 6)], 0);
        let mut out = [Blob::default(); 4];
        assert_eq!(detect10(&f, 4, &mut out), 1);
        assert_eq!(
            out[0],
            Blob {
                x: 152,
                y: 75,
                area: 3,
                sigma: spot_sigma(17, 3),
                peak: 1,
            }
        );
    }

    #[test]
    fn raw10_thresholds_at_10_bit_edges() {
        let values = [3u16, 4, 510, 511, 512, 513, 515, 516, 1022, 1023];
        let mut px = [(0, 0, 0); 10];
        for (i, &v) in values.iter().enumerate() {
            px[i] = (3 + 6 * i, 1 + i, v);
        }
        let f = frame10(&px, 0);
        let mut out = [Blob::default(); 16];
        for t in [1u16, 4, 5, 511, 512, 513, 516, 517, 1022, 1023, 2000] {
            let want = values.iter().filter(|&&v| v >= t.min(1023)).count();
            assert_eq!(detect10(&f, t, &mut out), want, "t {t}");
        }
        assert_eq!(detect10(&f, 512, &mut out), 6);
        assert_eq!(detect10(&f, 1023, &mut out), 1);
        assert_eq!(out[0].peak, 255);
        // Threshold 0 is taken as 1: the zero background stays dark.
        assert_eq!(detect10(&f, 0, &mut out), 10);
    }

    #[test]
    fn raw10_skip_passes_dark_rows_with_bright_low_bytes() {
        // Pixels 3 everywhere (low-bit bytes 0xff): at every alignment the skip must go
        // through the whole row, else the low bytes defeat it.
        let f = frame10(&[], 3);
        let mut buf = [0u8; S10 + 8];
        for offset in 0..8 {
            buf[offset..offset + S10].copy_from_slice(&f[..S10]);
            let row = &buf[offset..offset + S10];
            assert_eq!(Scalar.first_bright10(row, 1), S10, "offset {offset}");
        }
    }

    #[test]
    fn raw10_low_bytes_are_not_pixels() {
        // Background 3: top bits 0, every low-bit byte 0xff, brighter than any threshold
        // if it were read as a pixel. Background 0: nothing but the pixel can stop the
        // skip, so a top-bits byte masked by mistake loses the pixel. The pixel sits at
        // every column and byte alignment and equals the threshold.
        let mut buf = [0u8; S10 * H10 + 4];
        let mut out = [Blob::default(); 4];
        let mut det = DefaultDetector::new();
        for offset in 0..4 {
            for t in [4u16, 5, 512, 1023] {
                for bg in [0, 3] {
                    let f = frame10(&[], bg);
                    buf[offset..offset + f.len()].copy_from_slice(&f);
                    let f = &buf[offset..offset + S10 * H10];
                    let n = det.process_frame_raw10(f, W10 as u16, t, &mut out);
                    assert_eq!((n, det.stats().pixels), (0, 0), "offset {offset} t {t}");
                    for x in 0..W10 {
                        let f = frame10(&[(x, 7, t)], bg);
                        buf[offset..offset + f.len()].copy_from_slice(&f);
                        let f = &buf[offset..offset + S10 * H10];
                        let n = det.process_frame_raw10(f, W10 as u16, t, &mut out);
                        assert_eq!(n, 1, "offset {offset} t {t} bg {bg} x {x}");
                        assert_eq!(det.stats().pixels, 1);
                        assert_eq!((out[0].x, out[0].y), (x as u16 * 32, 7 * 32));
                    }
                }
            }
        }
    }

    #[test]
    fn raw10_strips_match_whole_frame_at_any_alignment() {
        // A staircase crossing the 32-pixel chunk edge and a thin diagonal, rows 2..=7,
        // with low bits set, on a background whose low-bit bytes are 0xff.
        let mut px = [(0, 0, 0); 30];
        let mut i = 0;
        for y in 2..=7 {
            for x in [26, 27, 28, 29, 50] {
                px[i] = (x + y, y, 520 + ((x * 37 + y * 11) % 500) as u16);
                i += 1;
            }
        }
        let f = frame10(&px, 0x7f);
        let mut want = [Blob::default(); 16];
        let mut det = DefaultDetector::new();
        let nw = det.process_frame_raw10(&f, W10 as u16, 512, &mut want);
        assert_eq!(nw, 2);
        let want_stats = det.stats();
        assert_eq!(want_stats.pixels, 30);
        for offset in 0..5 {
            let mut buf = [0u8; S10 * H10 + 5];
            buf[offset..offset + S10 * H10].copy_from_slice(&f);
            let f = &buf[offset..offset + S10 * H10];
            for strip in [1, 2, 3, 4, 5, H10] {
                det.begin_frame_raw10(W10 as u16, 512);
                let mut y = 0;
                while y < H10 {
                    let n = strip.min(H10 - y);
                    det.push_rows(&f[y * S10..(y + n) * S10], y as u16);
                    y += n;
                }
                let mut got = [Blob::default(); 16];
                let ng = det.finish(&mut got);
                assert_eq!(got[..ng], want[..nw], "offset {offset} strip {strip}");
                assert_eq!(det.stats(), want_stats);
            }
        }
    }

    #[test]
    fn raw10_blob_across_strip_boundary() {
        // 3 x 3 square on rows 7..=9, pushed as strips of 8 rows: one blob.
        let mut px = [(0, 0, 0); 9];
        for (i, p) in px.iter_mut().enumerate() {
            *p = (30 + i % 3, 7 + i / 3, 700 + i as u16);
        }
        let f = frame10(&px, 1);
        let mut det = DefaultDetector::new();
        det.begin_frame_raw10(W10 as u16, 600);
        det.push_rows(&f[..8 * S10], 0);
        det.push_rows(&f[8 * S10..], 8);
        let mut out = [Blob::default(); 4];
        assert_eq!(det.finish(&mut out), 1);
        // Weights 700..=708 in row-major order: sw = 6336,
        // swx = 30 * 2109 + 31 * 2112 + 32 * 2115 = 196422 -> 31.001 px -> 992,
        // swy = 7 * 2103 + 8 * 2112 + 9 * 2121 = 50706 -> 8.0028 px -> 256.
        assert_eq!(
            out[0],
            Blob {
                x: 992,
                y: 256,
                area: 9,
                sigma: spot_sigma(6336, 177),
                peak: 177,
            }
        );
    }

    #[test]
    fn raw10_random_frames_match_flood_fill() {
        const RW: usize = 128;
        const RH: usize = 32;
        const RS: usize = RW / 4 * raw10::GROUP_BYTES;
        let mut s = 0x6c07_8965u32;
        let mut rnd = move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        let mut det = Detector::<256, 256, 1024>::new();
        let mut px = [0u16; RW * RH];
        let mut buf = [0u8; RS * RH + 4];
        let mut got = [Blob::default(); 1024];
        let thresholds = [4u16, 100, 511, 512, 513, 600, 1023];
        for round in 0..280 {
            let offset = round % 4;
            let t = thresholds[round / 4 % thresholds.len()];
            let density = 1 + round as u32 % 8;
            for p in px.iter_mut() {
                let r = rnd();
                *p = if r % 16 < density {
                    t + (r >> 8) as u16 % (1024 - t)
                } else {
                    // Dark, mostly with low bits 3 (low-bit bytes 0xff), top bits up to
                    // just below the threshold's.
                    let v = (r >> 8) as u16 % t;
                    if r & 0x30 != 0 && v | 3 < t { v | 3 } else { v }
                };
            }
            pack(&px, &mut buf[offset..offset + RS * RH]);
            let f = &buf[offset..offset + RS * RH];
            let want = reference::flood(&px, RW, t.into(), 2);
            let ng = det.process_frame_raw10(f, RW as u16, t, &mut got);
            assert_eq!(det.stats().overflow, 0, "round {round}");
            assert_eq!(got[..ng], want[..], "round {round} t {t}");
        }
    }

    /// A frame at `bg` with squares (x, y, side, value) painted in order.
    fn squares(bg: u8, list: &[(usize, usize, usize, u8)]) -> [u8; W * H] {
        let mut f = [bg; W * H];
        for &(x0, y0, side, v) in list {
            for y in y0..y0 + side {
                f[y * W + x0..y * W + x0 + side].fill(v);
            }
        }
        f
    }

    /// The blobs at threshold 22 split above a background of 10 (thresholds 22, 34, 58,
    /// 106, 202), in the detector's order.
    fn split(f: &[u8], min_area: u32) -> std::vec::Vec<Blob> {
        let mut det = DefaultDetector::new();
        det.set_split(Some(Split {
            background: 10,
            min_area: 0,
        }));
        det.set_area_filter(min_area, u32::MAX);
        let mut out = [Blob::default(); 8];
        let n = det.process_frame(f, W as u16, 22, &mut out);
        assert_eq!(det.stats().overflow, 0);
        out[..n].to_vec()
    }

    #[test]
    fn spot_sigma_is_the_gaussians_whatever_the_cut() {
        // flux = 2 pi sigma^2 P (1 - c), height = P (1 - c).
        for sigma in [0.6, 1.5, 3.0, 8.5, 40.0] {
            for (peak, cut) in [(200.0, 0.06), (200.0, 0.5), (60.0, 0.75), (1000.0, 0.1)] {
                let height: f64 = peak * (1.0 - cut);
                let flux = 2.0 * core::f64::consts::PI * sigma * sigma * height;
                let got = spot_sigma(flux.round() as u32, height.round() as u32) as f64 / 32.0;
                assert!(
                    (got / sigma - 1.0).abs() < 0.03,
                    "sigma {sigma} peak {peak} cut {cut}: {got}"
                );
            }
        }
        assert_eq!(spot_sigma(0, 0), 0);
        assert_eq!(spot_sigma(0, 100), 0);
        assert_eq!(spot_sigma(u32::MAX, 1), u16::MAX);
        // 2^32 / 1000 * 163 = 7.0e8: its root.
        assert_eq!(spot_sigma(u32::MAX, 1000), 26459);

        // A sampled spot of sigma 2 px, peak 200 above a background of 10, cut low, at a
        // half and high: the same size within 6 %.
        let mut f = [10u8; W * H];
        for (i, v) in f.iter_mut().enumerate() {
            let (dx, dy) = ((i % W) as f64 - 15.3, (i / W) as f64 - 7.6);
            *v += (200.0 * (-(dx * dx + dy * dy) / 8.0).exp()).round() as u8;
        }
        for t in [22, 110, 160] {
            let mut det = DefaultDetector::new();
            det.set_split(Some(Split {
                background: 10,
                min_area: 0,
            }));
            det.begin_frame(W as u16, t);
            // One threshold only: the spot as cut at `t`.
            det.lab.nlevels = 1;
            det.push_rows(&f, 0);
            let mut out = [Blob::default(); 2];
            assert_eq!(det.finish(&mut out), 1);
            let got = out[0].sigma as f64 / 32.0;
            assert!((got / 2.0 - 1.0).abs() < 0.06, "t {t}: {got}");
        }
    }

    #[test]
    fn peaks_joined_below_two_fifths_split() {
        // Two 3 x 3 squares of 200 joined by a column of 60: one component at 22, 34 and
        // 58, two at 106.
        let f = squares(10, &[(4, 4, 7, 60), (4, 4, 3, 200), (8, 4, 3, 200)]);
        let plain = {
            let mut out = [Blob::default(); 4];
            let n = detect(&f, 22, &mut out);
            out[..n].to_vec()
        };
        assert_eq!(plain.len(), 1);
        assert_eq!((plain[0].area, plain[0].peak), (49, 200));
        let got = split(&f, 1);
        let want = |x: u16| Blob {
            x: x * 32,
            y: 5 * 32,
            area: 9,
            // 9 pixels of 190 above the background, 94 above their threshold.
            sigma: spot_sigma(9 * 190, 94),
            peak: 200,
        };
        assert_eq!(got, [want(5), want(9)]);
        // Pieces the area filter would not keep do not split.
        let whole = split(&f, 10);
        assert_eq!(whole.len(), 1);
        assert_eq!((whole[0].area, whole[0].peak), (49, 200));
    }

    #[test]
    fn peaks_joined_above_four_fifths_stay_one() {
        // Joined at 170: 160 above the background, 0.84 of the squares' 190.
        let f = squares(10, &[(4, 4, 7, 170), (4, 4, 3, 200), (8, 4, 3, 200)]);
        let got = split(&f, 1);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].area, got[0].peak), (49, 200));
        // Bright specks on a plateau are not peaks: fewer than SPLIT_MIN_AREA pixels each.
        let mut f = squares(10, &[(4, 4, 7, 100)]);
        f[5 * W + 5] = 255;
        f[5 * W + 6] = 255;
        f[8 * W + 9] = 255;
        let got = split(&f, 1);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].area, got[0].peak), (49, 255));
        // A peak that barely passes its threshold neither: 110 is above 106, but not by a
        // quarter of 96.
        let f = squares(10, &[(4, 4, 7, 60), (4, 4, 3, 200), (8, 4, 3, 110)]);
        assert_eq!(split(&f, 1).len(), 1);
    }

    #[test]
    fn peaks_split_at_several_thresholds() {
        // A and B joined at 100 (apart at 106), B and C at 40 (apart at 58): A and B come
        // out of the component that C left. 9 pixels a row: every threshold is read again.
        let f = squares(
            10,
            &[
                (2, 4, 5, 100),
                (7, 4, 2, 40),
                (2, 4, 2, 200),
                (5, 4, 2, 200),
                (9, 4, 2, 180),
            ],
        );
        let mut got = split(&f, 1);
        got.sort_unstable_by_key(|b| b.x);
        let seen: std::vec::Vec<_> = got.iter().map(|b| (b.x, b.y, b.area, b.peak)).collect();
        let want = [(80, 144, 4, 200), (176, 144, 4, 200), (304, 144, 4, 180)];
        assert_eq!(seen, want);
        // RAW10: the same picture at four times the values.
        let mut f10 = [0u8; W * H / 4 * raw10::GROUP_BYTES];
        for (g, p) in f10.as_chunks_mut().0.iter_mut().zip(f.as_chunks::<4>().0) {
            *g = raw10::pack(p.map(|v| v as u16 * 4));
        }
        let mut det = DefaultDetector::new();
        det.set_split(Some(Split {
            background: 40,
            min_area: 0,
        }));
        let mut out = [Blob::default(); 8];
        let n = det.process_frame_raw10(&f10, W as u16, 88, &mut out);
        let mut got10 = out[..n].to_vec();
        got10.sort_unstable_by_key(|b| b.x);
        let seen: std::vec::Vec<_> = got10.iter().map(|b| (b.x, b.y, b.area, b.peak)).collect();
        assert_eq!(seen, want);
        // Four times the light over four times the height: the same size.
        for (a, b) in got.iter().zip(&got10) {
            assert!(a.sigma.abs_diff(b.sigma) <= 1, "{a:?} {b:?}");
        }
    }

    #[test]
    fn rows_brighter_than_one_more_pass_keep_their_peaks_together() {
        // Two squares of 200 on a slab of 100. A slab 8 pixels wide is read again at each
        // of the four higher thresholds: 32 pixels, the row's.
        let narrow = squares(10, &[(4, 4, 8, 100), (4, 4, 3, 200), (9, 4, 3, 200)]);
        assert_eq!(split(&narrow, 1).len(), 2);
        // 16 pixels wide: read again at 34 and 58, then taken whole at 106 and 202.
        let mut wide = squares(10, &[(4, 4, 3, 200), (9, 4, 3, 200)]);
        for y in 4..7 {
            for v in &mut wide[y * W + 4..y * W + 20] {
                *v = (*v).max(100);
            }
        }
        let got = split(&wide, 1);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].area, got[0].peak), (48, 200));
    }

    #[test]
    fn split_runs_count_against_the_rows_capacity() {
        // 4 runs a row: the slab and the third square at 22 take two, at 34 two more, none
        // is left for the thresholds that would part the squares on the slab. Nothing is
        // lost of the frame's own threshold.
        let f = squares(
            10,
            &[
                (4, 4, 7, 60),
                (4, 4, 3, 200),
                (8, 4, 3, 200),
                (20, 4, 3, 200),
            ],
        );
        let mut det = Detector::<4, 16, 8>::new();
        det.set_split(Some(Split {
            background: 10,
            min_area: 0,
        }));
        let mut out = [Blob::default(); 8];
        let n = det.process_frame(&f, W as u16, 22, &mut out);
        assert_eq!(n, 2);
        let s = det.stats();
        assert_eq!(s.pixels, 49 + 9);
        // One run dropped in each of the squares' three rows; below them the slab alone,
        // at 22, 34 and 58.
        assert_eq!(s.overflow, 3);
        assert_eq!(s.runs, 3 * 4 + 4 * 3);
    }
}
