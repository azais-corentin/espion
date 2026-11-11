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
//! peak value.
//!
//! The scan asks a [`Skip`] where the next bright pixel can be ([`Scalar`] here, a vector
//! version on the chip), reads exactly from there until it has passed the skip's window and
//! no run is open, then asks the skip again. The scalar skip tests 64 bytes at a time with a
//! SWAR test on the OR of their words, then confirms on each word.
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
//! [`synth`] generates deterministic synthetic frames for the tests and for the board
//! benchmark. The crate has no esp dependency: it builds and tests on the host.

#![no_std]

// The test references collect into `Vec`.
#[cfg(test)]
extern crate std;

pub mod skip;
pub mod synth;

use core::num::NonZeroUsize;
pub use skip::{Scalar, Skip};

use skip::{WINDOW, WINDOW10, hot, swar_params};

#[cfg(test)]
mod equivalence;
#[cfg(test)]
mod reference;

/// One detected blob, as in the `Blob` of the wire format (`docs/protocol.md`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Blob {
    /// Weighted centroid column in 1/32 pixel (Q11.5).
    pub x: u16,
    /// Weighted centroid row in 1/32 pixel (Q11.5).
    pub y: u16,
    /// Foreground pixels, saturating at `u16::MAX`.
    pub area: u16,
    /// Brightest pixel value.
    pub peak: u8,
}

/// Counters for the current frame, reset by [`Detector::begin_frame`] and
/// [`Detector::begin_frame_raw10`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Pixels `>= threshold`, including those of dropped runs.
    pub pixels: u32,
    /// Runs kept (dropped runs are counted in `overflow`).
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

/// Label of a run that has no component.
const NONE: u16 = u16::MAX;

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

/// A horizontal run of foreground pixels `start..end` in one row with its sums.
#[derive(Clone, Copy)]
struct Run {
    start: u16,
    end: u16,
    label: u16,
    /// Brightest value, in the units of the pixel format.
    peak: u16,
    /// Sum of pixel values.
    sw: u32,
    /// Sum of pixel value times column. Fits: 255 * (0 + .. + 2047) < 2^32 for any run in
    /// a row up to 2048 pixels (the Q11.5 limit), up to 5800 in fact (RAW10: 2896).
    swx: u32,
}

impl Run {
    const ZERO: Run = Run {
        start: 0,
        end: 0,
        label: NONE,
        peak: 0,
        sw: 0,
        swx: 0,
    };
}

/// Accumulators of a component; also a union-find node.
#[derive(Clone, Copy)]
struct Comp {
    parent: u16,
    /// Last row a run was added (also marks a finished root within `end_row`).
    last_row: u16,
    peak: u16,
    area: u32,
    sw: u32,
    swx: u64,
    swy: u64,
}

impl Comp {
    const ZERO: Comp = Comp {
        parent: NONE,
        last_row: 0,
        peak: 0,
        area: 0,
        sw: 0,
        swx: 0,
        swy: 0,
    };
}

/// Run builder, built once per [`Detector::push_rows_with`] call and kept across the rows of
/// the strip: a local, so its fields stay in registers in the strip loop.
struct Scan<'a, const RUNS: usize> {
    /// Runs of the current row.
    cur: &'a mut [Run; RUNS],
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

impl<const RUNS: usize> Scan<'_, RUNS> {
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
    fn close(&mut self, x: u32) {
        self.open = false;
        self.pixels += x - self.start;
        if let Some(r) = self.cur.get_mut(self.n) {
            *r = Run {
                start: self.start as u16,
                end: x as u16,
                label: NONE,
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
}

/// A pixel format: its skip and its row scan. The strip loop is generic over it, so the
/// format and the threshold class are decided once per [`Detector::push_rows_with`] call,
/// not per row.
trait Format {
    /// The skip's threshold for the detector's threshold `t`.
    fn skip_threshold(t: u16) -> u8;
    /// The skip's answer for `bytes` (whole rows), at most `bytes.len()`.
    fn first_bright<S: Skip>(skip: &S, bytes: &[u8], skip_t: u8) -> usize;
    /// The runs of `row` from `x < row.len()`, an answer of the skip.
    fn scan<S: Skip, const RUNS: usize>(scan: &mut Scan<'_, RUNS>, skip: &S, row: &[u8], x: usize);
}

/// RAW8, with the SWAR word test of thresholds above 128 (`HIGH`) or not.
struct Raw8<const HIGH: bool>;

impl<const HIGH: bool> Format for Raw8<HIGH> {
    fn skip_threshold(t: u16) -> u8 {
        t as u8
    }

    #[inline(always)]
    fn first_bright<S: Skip>(skip: &S, bytes: &[u8], skip_t: u8) -> usize {
        skip.first_bright(bytes, skip_t).min(bytes.len())
    }

    #[inline(always)]
    fn scan<S: Skip, const RUNS: usize>(scan: &mut Scan<'_, RUNS>, skip: &S, row: &[u8], x: usize) {
        scan.row8::<S, HIGH>(skip, row, x);
    }
}

/// Packed RAW10.
struct Raw10;

impl Format for Raw10 {
    /// A pixel `>= t` has its top 8 bits `>= t >> 2`.
    fn skip_threshold(t: u16) -> u8 {
        (t >> 2) as u8
    }

    #[inline(always)]
    fn first_bright<S: Skip>(skip: &S, bytes: &[u8], skip_t: u8) -> usize {
        skip.first_bright10(bytes, skip_t).min(bytes.len())
    }

    #[inline(always)]
    fn scan<S: Skip, const RUNS: usize>(scan: &mut Scan<'_, RUNS>, skip: &S, row: &[u8], x: usize) {
        scan.row10(skip, row, x);
    }
}

/// Component table, free list and finished blobs.
struct Labels<const COMPS: usize, const BLOBS: usize> {
    comps: [Comp; COMPS],
    /// Stack of free component slots.
    free: [u16; COMPS],
    nfree: usize,
    /// Slots merged into another root during the current row, freed at its end.
    merged: [u16; COMPS],
    nmerged: usize,
    blobs: [Blob; BLOBS],
    nblobs: usize,
    /// `Blob::peak` is the component peak shifted right by this (2 for RAW10).
    peak_shift: u8,
    min_area: u32,
    max_area: u32,
    stats: Stats,
}

impl<const COMPS: usize, const BLOBS: usize> Labels<COMPS, BLOBS> {
    fn reset(&mut self) {
        for (i, f) in self.free.iter_mut().enumerate() {
            // Pop order 0, 1, 2, ...: keeps the labels and thus the merge order predictable.
            *f = (COMPS - 1 - i) as u16;
        }
        self.nfree = COMPS;
        self.nmerged = 0;
        self.nblobs = 0;
        self.stats = Stats::default();
    }

    fn alloc(&mut self, y: u16) -> u16 {
        let Some(n) = self.nfree.checked_sub(1) else {
            return NONE;
        };
        self.nfree = n;
        let i = self.free[n];
        self.comps[i as usize] = Comp {
            parent: i,
            last_row: y,
            ..Comp::ZERO
        };
        i
    }

    fn release(&mut self, i: u16) {
        // Each slot is released once per allocation, so the stack never overflows.
        if let Some(f) = self.free.get_mut(self.nfree) {
            *f = i;
            self.nfree += 1;
        }
    }

    fn find(&mut self, mut i: u16) -> u16 {
        loop {
            let p = self.comps[i as usize].parent;
            if p == i {
                return i;
            }
            // Path halving.
            let gp = self.comps[p as usize].parent;
            self.comps[i as usize].parent = gp;
            i = gp;
        }
    }

    /// Merges the component of `b` into root `a` (or the other way); returns the root.
    fn union(&mut self, a: u16, b: u16) -> u16 {
        let b = self.find(b);
        if a == b {
            return a;
        }
        let (root, child) = if a < b { (a, b) } else { (b, a) };
        let c = self.comps[child as usize];
        let r = &mut self.comps[root as usize];
        r.area += c.area;
        r.sw += c.sw;
        r.swx += c.swx;
        r.swy += c.swy;
        r.peak = r.peak.max(c.peak);
        r.last_row = r.last_row.max(c.last_row);
        self.comps[child as usize].parent = root;
        if let Some(m) = self.merged.get_mut(self.nmerged) {
            *m = child;
            self.nmerged += 1;
        }
        root
    }

    /// Labels the runs of row `y` from the touching runs of the row above (8-connected:
    /// a run `s..e` touches the runs above that overlap `s-1..e+1`) and adds them to their
    /// components.
    fn link(&mut self, prev: &[Run], cur: &mut [Run], y: u16) {
        let mut j = 0;
        for c in cur.iter_mut() {
            while j < prev.len() && prev[j].end < c.start {
                j += 1;
            }
            let mut label = NONE;
            let mut k = j;
            while k < prev.len() && prev[k].start <= c.end {
                let pl = prev[k].label;
                if pl != NONE {
                    label = if label == NONE {
                        self.find(pl)
                    } else {
                        self.union(label, pl)
                    };
                }
                k += 1;
            }
            if label == NONE {
                label = self.alloc(y);
                if label == NONE {
                    self.stats.overflow += 1;
                    c.label = NONE;
                    continue;
                }
            }
            let comp = &mut self.comps[label as usize];
            comp.area += (c.end - c.start) as u32;
            comp.sw += c.sw;
            comp.swx += c.swx as u64;
            comp.swy += c.sw as u64 * y as u64;
            comp.peak = comp.peak.max(c.peak);
            comp.last_row = y;
            c.label = label;
        }
    }

    /// After row `y`: points the new runs at their roots, finishes the components of the
    /// row above that got no run in row `y`, and frees the slots merged away.
    fn end_row(&mut self, prev: &[Run], cur: &mut [Run], y: u16) {
        for c in cur.iter_mut() {
            if c.label != NONE {
                c.label = self.find(c.label);
            }
        }
        for p in prev {
            if p.label != NONE {
                let r = self.find(p.label);
                if self.comps[r as usize].last_row != y {
                    // Marks it finished for the other runs of the same component.
                    self.comps[r as usize].last_row = y;
                    self.emit(r);
                    self.release(r);
                }
            }
        }
        for i in 0..self.nmerged {
            let m = self.merged[i];
            self.release(m);
        }
        self.nmerged = 0;
    }

    fn emit(&mut self, r: u16) {
        let c = self.comps[r as usize];
        if c.area < self.min_area || c.area > self.max_area {
            return;
        }
        let sw = c.sw as u64;
        let q5 = |s: u64| ((s * 32 + sw / 2) / sw).min(u16::MAX as u64) as u16;
        let blob = Blob {
            x: q5(c.swx),
            y: q5(c.swy),
            area: c.area.min(u16::MAX as u32) as u16,
            peak: (c.peak >> self.peak_shift) as u8,
        };
        if let Some(b) = self.blobs.get_mut(self.nblobs) {
            *b = blob;
            self.nblobs += 1;
        } else {
            self.stats.overflow += 1;
        }
    }
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
    runs: [[Run; RUNS]; 2],
    nruns: [usize; 2],
    /// Index into `runs` of the row above.
    prev: usize,
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
    const CAPACITY_OK: () = assert!(COMPS > 0 && COMPS < NONE as usize);

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
            nruns: [0; 2],
            prev: 0,
            lab: Labels {
                comps: [Comp::ZERO; COMPS],
                free: [0; COMPS],
                nfree: 0,
                merged: [0; COMPS],
                nmerged: 0,
                blobs: [Blob {
                    x: 0,
                    y: 0,
                    area: 0,
                    peak: 0,
                }; BLOBS],
                nblobs: 0,
                peak_shift: 0,
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

    /// Keeps only blobs with `min_area <= area <= max_area` pixels (before saturation).
    /// Applies from the next blob finished.
    pub fn set_area_filter(&mut self, min_area: u32, max_area: u32) {
        self.lab.min_area = min_area;
        self.lab.max_area = max_area;
    }

    /// Starts a RAW8 frame of rows of `width` pixels (at most 65535; the Q11.5 centroid
    /// saturates beyond 2048). A `threshold` of 0 is taken as 1.
    pub fn begin_frame(&mut self, width: u16, threshold: u8) {
        let t = threshold.max(1);
        let (add, high) = swar_params(t);
        self.begin(width as usize, t as u16);
        self.add = add;
        self.high = high;
        self.raw10 = false;
        self.lab.peak_shift = 0;
    }

    fn begin(&mut self, stride: usize, threshold: u16) {
        self.stride = stride;
        self.threshold = threshold;
        self.next_row = 0;
        self.nruns = [0; 2];
        self.lab.reset();
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
    pub fn push_rows_with<S: Skip>(&mut self, skip: &S, rows: &[u8], first_row: u16) {
        let Some(stride) = NonZeroUsize::new(self.stride) else {
            return;
        };
        if first_row != self.next_row {
            self.flush();
        }
        match (self.raw10, self.high) {
            (false, false) => self.strip::<S, Raw8<false>>(skip, rows, stride, first_row),
            (false, true) => self.strip::<S, Raw8<true>>(skip, rows, stride, first_row),
            (true, _) => self.strip::<S, Raw10>(skip, rows, stride, first_row),
        }
    }

    /// The rows of one [`Self::push_rows_with`] call in format `F`. The scan state and the
    /// two run buffers are taken once for the call; the rows swap the buffers by reference.
    fn strip<S: Skip, F: Format>(
        &mut self,
        skip: &S,
        rows: &[u8],
        stride: NonZeroUsize,
        first_row: u16,
    ) {
        let Self {
            threshold,
            add,
            next_row,
            runs,
            nruns,
            prev: parity,
            lab,
            ..
        } = self;
        let [a, b] = runs;
        let (mut prev, cur) = if *parity == 0 { (a, b) } else { (b, a) };
        let mut n_prev = nruns[*parity];
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
            let x = first.unwrap_or_else(|| F::first_bright(skip, row, scan.skip_t));
            if x < row.len() {
                F::scan(&mut scan, skip, row, x);
            }
            let n = scan.n;
            // Both rows empty: nothing to link, nothing to finish.
            if n != 0 || n_prev != 0 {
                let prev = &prev[..n_prev];
                let cur = &mut scan.cur[..n];
                lab.link(prev, cur, y);
                lab.end_row(prev, cur, y);
            }
            nruns_sum += n as u32;
            core::mem::swap(&mut prev, &mut scan.cur);
            *parity ^= 1;
            n_prev = n;
            scan.n = 0;
            rest = tail;
            y = y.wrapping_add(1);
        }
        nruns[*parity] = n_prev;
        *next_row = y;
        let stats = &mut lab.stats;
        stats.pixels += scan.pixels;
        stats.runs += nruns_sum;
        stats.overflow += scan.dropped;
    }

    /// Finishes the frame: writes the blobs into `out` (at most `out.len()`, the rest is
    /// counted in `overflow`) and returns how many.
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
        self.begin(width as usize / 4 * raw10::GROUP_BYTES, t);
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
    fn flush(&mut self) {
        let prev = &self.runs[self.prev][..self.nruns[self.prev]];
        self.lab.end_row(prev, &mut [], self.next_row);
        self.nruns[self.prev] = 0;
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
                peak: 200
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
                peak: 200
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
                peak: 200
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
            peak: 200
        }));
        assert!(out[..n].contains(&Blob {
            x: 304,
            y: 80,
            area: 2,
            peak: 200
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
            peak: 200
        }));
        assert!(out[..n].contains(&Blob {
            x: (W as u16 - 1) * 32 - 16,
            y: (H as u16 - 1) * 32,
            area: 2,
            peak: 180
        }));
        assert!(out[..n].contains(&Blob {
            x: (W as u16 - 1) * 32,
            y: 0,
            area: 1,
            peak: 150
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
                peak: 255
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
                peak: 250
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
                peak: 1
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
                peak: 177
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
}
