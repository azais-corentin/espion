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
//! The scan skips dark pixels 32 at a time: a SWAR test tells whether any byte of a `u32`
//! is `>= threshold`; it is run on the OR of eight aligned words, then on each word of a
//! chunk that passes, and only words that pass are looked at pixel by pixel.
//!
//! RAW10 frames in the MIPI CSI-2 packing ([`raw10`]) go through the same API
//! ([`Detector::begin_frame_raw10`]): the threshold is in 10-bit units, the weights are the
//! 10-bit values, `Blob::peak` is the top 8 bits. Their skip runs the same test on the top
//! 8 bits of 32 pixels at a time (ten words, low-bit bytes masked out), then exactly on each
//! group of four pixels.
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

pub mod synth;

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

const L7: u32 = 0x7f7f_7f7f;
const HI: u32 = 0x8080_8080;
const LO: u32 = 0x0101_0101;

/// Non-zero when any byte of `w` is `>= t`, with `add` and `HIGH` from [`swar_params`].
///
/// Adding `add` to the low 7 bits of each byte never carries into the next byte (both are
/// at most 127), and sets bit 7 exactly when the low 7 bits reach the threshold's.
#[inline(always)]
fn hot<const HIGH: bool>(w: u32, add: u32) -> u32 {
    let s = (w & L7) + add;
    if HIGH {
        // t > 128: the byte needs bit 7 and low bits >= t - 128.
        s & w & HI
    } else {
        // t <= 128: bit 7 alone is enough, else low bits >= t.
        (s | w) & HI
    }
}

/// `(add, HIGH)` for [`hot`] for a threshold in `0..=255` (0: every word is hot).
const fn swar_params(t: u8) -> (u32, bool) {
    if t <= 128 {
        ((128 - t as u32) * LO, false)
    } else {
        ((256 - t as u32) * LO, true)
    }
}

/// Words per chunk of the dark skip: 32 pixels.
const CHUNK: usize = 8;

#[inline(always)]
fn or_words(chunk: &[u32; CHUNK]) -> u32 {
    chunk.iter().fold(0, |a, &w| a | w)
}

/// Number of leading chunks with no pixel `>= threshold`.
///
/// Tests the bitwise OR of the chunk's words: a byte of the OR is at least each of the
/// bytes it combines, so a chunk with a bright pixel always stops the skip. A dark chunk can
/// stop it too (OR of dark bytes reaching the threshold, e.g. 0x40 | 0x24 at threshold
/// 100); the caller then tests its words exactly. That never happens while the dark
/// pixels stay below the highest power of two `<=` threshold.
#[inline(never)]
fn skip_dark<const HIGH: bool>(chunks: &[[u32; CHUNK]], add: u32) -> usize {
    chunks
        .iter()
        .position(|c| hot::<HIGH>(or_words(c), add) != 0)
        .unwrap_or(chunks.len())
}

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

    /// For each phase `o` (the position in its group of the first byte of a chunk), the
    /// masks of the chunk's first five words: 0x00 on the `LOW` bytes, 0xff elsewhere.
    /// Words 5..=9 of a chunk repeat them (20 bytes are four whole groups).
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

/// Words per RAW10 skip chunk: 40 bytes, 32 pixels, 8 whole groups.
const CHUNK10: usize = 10;

/// OR of a RAW10 chunk's words with the low-bit bytes masked out.
#[inline(always)]
fn or_masked10(c: &[u32; CHUNK10], m: &[u32; 5]) -> u32 {
    (c[0] | c[5]) & m[0]
        | (c[1] | c[6]) & m[1]
        | (c[2] | c[7]) & m[2]
        | (c[3] | c[8]) & m[3]
        | (c[4] | c[9]) & m[4]
}

/// [`skip_dark`] for RAW10 chunks, testing the top 8 bits of the pixels against
/// `threshold >> 2`.
#[inline(never)]
fn skip_dark10<const HIGH: bool>(chunks: &[[u32; CHUNK10]], m: &[u32; 5], add: u32) -> usize {
    chunks
        .iter()
        .position(|c| hot::<HIGH>(or_masked10(c, m), add) != 0)
        .unwrap_or(chunks.len())
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

/// Row scanner: builds the runs of one row.
struct Scan<'a> {
    runs: &'a mut [Run],
    n: usize,
    dropped: u32,
    pixels: u32,
    /// Threshold in the units of the pixel format.
    t: u32,
    open: bool,
    start: u32,
    sw: u32,
    swx: u32,
    peak: u32,
}

impl Scan<'_> {
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
        if let Some(r) = self.runs.get_mut(self.n) {
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

    /// Four pixels starting at column `x`, from a word read in memory order.
    #[inline(always)]
    fn word<const HIGH: bool>(&mut self, x: u32, w: u32, add: u32) {
        if hot::<HIGH>(w, add) == 0 {
            if self.open {
                self.close(x);
            }
        } else {
            let [a, b, c, d] = w.to_ne_bytes();
            self.pixel(x, a as u32);
            self.pixel(x + 1, b as u32);
            self.pixel(x + 2, c as u32);
            self.pixel(x + 3, d as u32);
        }
    }

    fn row<const HIGH: bool>(&mut self, row: &[u8], add: u32) {
        // SAFETY: every bit pattern is a valid `u32`, and `align_to` only reinterprets the
        // middle part, which it aligns for `u32`; the slices cover `row` exactly, in order.
        let (pre, mid, post) = unsafe { row.align_to::<u32>() };
        let mut x = 0u32;
        for &v in pre {
            self.pixel(x, v as u32);
            x += 1;
        }
        let (chunks, rest) = mid.as_chunks::<CHUNK>();
        let mut i = 0;
        while i < chunks.len() {
            if !self.open {
                let n = skip_dark::<HIGH>(&chunks[i..], add);
                i += n;
                x += (n * CHUNK * 4) as u32;
                if i == chunks.len() {
                    break;
                }
            }
            let chunk = &chunks[i];
            if hot::<HIGH>(or_words(chunk), add) == 0 {
                // Open run, dark chunk: the run ended at the chunk start.
                self.close(x);
            } else {
                for (k, &w) in chunk.iter().enumerate() {
                    self.word::<HIGH>(x + 4 * k as u32, w, add);
                }
            }
            i += 1;
            x += (CHUNK * 4) as u32;
        }
        for &w in rest {
            self.word::<HIGH>(x, w, add);
            x += 4;
        }
        for &v in post {
            self.pixel(x, v as u32);
            x += 1;
        }
        if self.open {
            self.close(x);
        }
    }

    /// The exact path of RAW10: pixel groups `from..to` of `row`.
    #[inline(always)]
    fn groups10(&mut self, row: &[u8], from: usize, to: usize) {
        let (groups, _) = row[from * raw10::GROUP_BYTES..to * raw10::GROUP_BYTES]
            .as_chunks::<{ raw10::GROUP_BYTES }>();
        let t = self.t;
        // A pixel with top 8 bits `< t >> 2` is `< t` whatever its low bits.
        let top = (t >> 2) as u8;
        let mut x = (from * 4) as u32;
        for g in groups {
            if raw10::top_below(g, top) {
                if self.open {
                    self.close(x);
                }
            } else {
                let [a, b, c, d] = raw10::unpack(g);
                if (a >= t) | (b >= t) | (c >= t) | (d >= t) {
                    self.pixel(x, a);
                    self.pixel(x + 1, b);
                    self.pixel(x + 2, c);
                    self.pixel(x + 3, d);
                } else if self.open {
                    self.close(x);
                }
            }
            x += 4;
        }
    }

    /// One RAW10 row (`width / 4` groups of [`raw10::GROUP_BYTES`] bytes).
    ///
    /// The skip works on aligned chunks of [`CHUNK10`] words (40 bytes, 32 pixels, 8
    /// groups, so every chunk of a row has the same phase against the groups). The words'
    /// low-bit bytes are masked to 0, which is below any skip threshold, and the rest are
    /// the top 8 bits of a pixel: if they are all `< t >> 2`, every pixel whose top bits
    /// lie in the chunk is `< t`. A chunk that passes is processed group by group, from the
    /// first group not yet processed up to the group holding its last byte.
    fn row10<const HIGH: bool>(&mut self, row: &[u8], add: u32) {
        let ngroups = row.len() / raw10::GROUP_BYTES;
        let row = &row[..ngroups * raw10::GROUP_BYTES];
        // SAFETY: as in `row`.
        let (pre, mid, _) = unsafe { row.align_to::<u32>() };
        let lead = pre.len();
        let (chunks, _) = mid.as_chunks::<CHUNK10>();
        let masks = &raw10::SKIP_MASKS[lead % raw10::GROUP_BYTES];
        // Groups with a byte before the first chunk.
        let mut next = lead.div_ceil(raw10::GROUP_BYTES).min(ngroups);
        self.groups10(row, 0, next);
        let mut i = 0;
        while i < chunks.len() {
            if !self.open {
                i += skip_dark10::<HIGH>(&chunks[i..], masks, add);
                if i == chunks.len() {
                    break;
                }
            }
            let first = lead + i * CHUNK10 * 4;
            if hot::<HIGH>(or_masked10(&chunks[i], masks), add) == 0 {
                // Open run, dark chunk: group `next` starts in this chunk, so its first
                // pixel is dark and the run ended there.
                self.close((next * 4) as u32);
            } else {
                let last = (first + CHUNK10 * 4 - 1) / raw10::GROUP_BYTES;
                self.groups10(row, next.max(first / raw10::GROUP_BYTES), last + 1);
                next = last + 1;
            }
            i += 1;
        }
        // The bytes after the last chunk; groups before them that were skipped are dark.
        let tail = (lead + chunks.len() * CHUNK10 * 4) / raw10::GROUP_BYTES;
        self.groups10(row, next.max(tail), ngroups);
        if self.open {
            self.close((ngroups * 4) as u32);
        }
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
    /// SWAR parameters of the skip (for RAW10, on the top 8 bits).
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
    /// the last row are finished first, as if a dark row lay between.
    pub fn push_rows(&mut self, rows: &[u8], first_row: u16) {
        if self.stride == 0 {
            return;
        }
        if first_row != self.next_row {
            self.flush();
        }
        let mut y = first_row;
        for row in rows.chunks_exact(self.stride) {
            self.row(row, y);
            y = y.wrapping_add(1);
        }
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
        self.begin_frame(width, threshold);
        self.push_rows(frame, 0);
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
        // The skip tests the top 8 bits: a pixel `>= t` has them `>= t >> 2`.
        let (add, high) = swar_params((t >> 2) as u8);
        self.begin(width as usize / 4 * raw10::GROUP_BYTES, t);
        self.add = add;
        self.high = high;
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
        self.begin_frame_raw10(width, threshold);
        self.push_rows(frame, 0);
        self.finish(out)
    }

    /// Counters of the current (or last finished) frame.
    pub fn stats(&self) -> Stats {
        self.lab.stats
    }

    fn row(&mut self, row: &[u8], y: u16) {
        let [a, b] = &mut self.runs;
        let (prev, cur) = if self.prev == 0 { (a, b) } else { (b, a) };
        let mut scan = Scan {
            runs: cur,
            n: 0,
            dropped: 0,
            pixels: 0,
            t: self.threshold as u32,
            open: false,
            start: 0,
            sw: 0,
            swx: 0,
            peak: 0,
        };
        match (self.raw10, self.high) {
            (false, true) => scan.row::<true>(row, self.add),
            (false, false) => scan.row::<false>(row, self.add),
            (true, true) => scan.row10::<true>(row, self.add),
            (true, false) => scan.row10::<false>(row, self.add),
        }
        let nc = scan.n;
        let stats = &mut self.lab.stats;
        stats.pixels += scan.pixels;
        stats.runs += nc as u32;
        stats.overflow += scan.dropped;
        let prev = &prev[..self.nruns[self.prev]];
        let cur = &mut cur[..nc];
        self.lab.link(prev, cur, y);
        self.lab.end_row(prev, cur, y);
        self.prev ^= 1;
        self.nruns[self.prev] = nc;
        self.next_row = y.wrapping_add(1);
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
    fn swar_matches_bytewise() {
        let mut s = 0x1234_5678u32;
        for t in 1..=255u8 {
            let (add, high) = swar_params(t);
            for i in 0..2000u32 {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                // Mix random words with words of bytes close to the threshold.
                let w = if i % 2 == 0 {
                    s
                } else {
                    u32::from_ne_bytes(
                        (s.to_ne_bytes()).map(|b| t.wrapping_add(b % 5).wrapping_sub(2)),
                    )
                };
                let want = w.to_ne_bytes().iter().any(|&b| b >= t);
                let got = if high {
                    hot::<true>(w, add)
                } else {
                    hot::<false>(w, add)
                } != 0;
                assert_eq!(got, want, "t={t} w={w:#010x}");
            }
        }
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
        let mut buf = [0u8; S10 + 4];
        for offset in 0..4 {
            buf[offset..offset + S10].copy_from_slice(&f[..S10]);
            let row = &buf[offset..offset + S10];
            // SAFETY: as in `Scan::row`.
            let (pre, mid, _) = unsafe { row.align_to::<u32>() };
            let (chunks, _) = mid.as_chunks::<CHUNK10>();
            assert!(!chunks.is_empty());
            let masks = &raw10::SKIP_MASKS[pre.len() % raw10::GROUP_BYTES];
            let (add, high) = swar_params(1);
            assert!(!high);
            let n = skip_dark10::<false>(chunks, masks, add);
            assert_eq!(n, chunks.len(), "offset {offset}");
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
