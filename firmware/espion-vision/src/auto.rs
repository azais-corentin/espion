//! Automatic threshold: the background level of a frame from a sparse histogram, and the
//! threshold for the next frame from it and the brightest blob, integer math only.
//!
//! Sampling takes every [`COL_STEP`]-th pixel (`x % COL_STEP == 0`) of every
//! [`ROW_STEP`]-th row (`row % ROW_STEP == 0`), so a 1280 x 800 frame gives 16 000 samples.
//! Rows can be sampled as a whole frame or in strips of whole rows as they arrive; both
//! give the same histogram. RAW10 pixels are binned on their top 8 bits, so every figure
//! here is in 8-bit units.
//!
//! From the histogram, [`Background`] holds the median and a robust spread taken on the
//! bright side, `sigma = p84 - p50` (the 84th percentile is one standard deviation above
//! the median for Gaussian noise). The markers cover a tiny share of the frame, so they
//! barely move either figure.
//!
//! The threshold is
//!
//! ```text
//! floor     = median + max(K * sigma, MARGIN)                 (saturating at 255)
//! threshold = max(floor, ceil(brightest_peak / 2)), in 1..=255
//! ```
//!
//! The floor keeps the noise out; half the brightest blob's peak keeps a bright marker's
//! halo (and its neighbours' glare) out of its blob while a dim marker is still found
//! down to the floor. With no blob in the last frame the threshold is the floor.

use crate::raw10;

/// Sampled rows: those with `row % ROW_STEP == 0`.
pub const ROW_STEP: u16 = 16;
/// Sampled pixels of a sampled row: those with `x % COL_STEP == 0`.
pub const COL_STEP: usize = 4;
/// Spreads above the median of the noise floor.
pub const K: u16 = 6;
/// Least distance of the floor above the median, for a flat background (`sigma` 0 or 1).
pub const MARGIN: u16 = 8;

// A RAW10 group holds four pixels; `sample_raw10` takes the first one of each.
const _: () = assert!(COL_STEP == 4);

/// Histogram of sampled pixel values in 8-bit units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Histogram {
    /// Samples per value.
    pub bins: [u32; 256],
}

impl Histogram {
    /// An empty histogram, for static initialisers and stacks.
    pub const fn new() -> Self {
        Self { bins: [0; 256] }
    }

    /// Empties the histogram.
    pub fn clear(&mut self) {
        self.bins = [0; 256];
    }

    /// Number of samples.
    pub fn total(&self) -> u32 {
        self.bins.iter().sum()
    }

    /// The smallest value whose cumulative count is at least `num / den` of `total`
    /// (`0 < total`, `num <= den`).
    fn percentile(&self, total: u32, num: u64, den: u64) -> u8 {
        let want = total as u64 * num;
        let mut cum = 0u64;
        for (v, &n) in self.bins.iter().enumerate() {
            cum += n as u64;
            if cum * den >= want {
                return v as u8;
            }
        }
        u8::MAX
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

/// The first row of `rows` with `(first_row + i) % ROW_STEP == 0`.
fn first_sampled(first_row: u16) -> usize {
    ((ROW_STEP - first_row % ROW_STEP) % ROW_STEP) as usize
}

/// Samples a strip of whole RAW8 rows of `width` bytes starting at frame row `first_row`
/// (a whole frame: `first_row` 0) into `hist`: the rows with `(first_row + i) % ROW_STEP
/// == 0`, their pixels with `x % COL_STEP == 0`. A partial last row is ignored.
pub fn sample_raw8(rows: &[u8], width: u16, first_row: u16, hist: &mut Histogram) {
    let width = width as usize;
    if width == 0 {
        return;
    }
    let sampled = rows
        .chunks_exact(width)
        .skip(first_sampled(first_row))
        .step_by(ROW_STEP as usize);
    for row in sampled {
        for &v in row.iter().step_by(COL_STEP) {
            hist.bins[v as usize] += 1;
        }
    }
}

/// [`sample_raw8`] for packed CSI-2 RAW10 rows ([`raw10`], rows of `width / 4 * 5` bytes,
/// `width` a multiple of 4), binned on the top 8 bits of each pixel.
pub fn sample_raw10(rows: &[u8], width: u16, first_row: u16, hist: &mut Histogram) {
    let stride = width as usize / 4 * raw10::GROUP_BYTES;
    if stride == 0 {
        return;
    }
    let sampled = rows
        .chunks_exact(stride)
        .skip(first_sampled(first_row))
        .step_by(ROW_STEP as usize);
    for row in sampled {
        // One sample per group: its pixel 0 (x % 4 == 0).
        for g in row.as_chunks::<{ raw10::GROUP_BYTES }>().0 {
            hist.bins[(raw10::unpack(g)[0] >> 2) as u8 as usize] += 1;
        }
    }
}

/// Background level of a frame in 8-bit units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Background {
    /// Median (the lower one for an even count) of the samples.
    pub median: u8,
    /// `p84 - p50` of the samples: one standard deviation of Gaussian noise, robust to
    /// the few bright marker pixels.
    pub sigma: u8,
}

impl Background {
    /// The background of the samples in `h`, `None` when it is empty.
    pub fn from_histogram(h: &Histogram) -> Option<Self> {
        let total = h.total();
        if total == 0 {
            return None;
        }
        let median = h.percentile(total, 50, 100);
        let p84 = h.percentile(total, 84, 100);
        Some(Self {
            median,
            sigma: p84 - median,
        })
    }
}

/// The RAW8 threshold: `max(floor, ceil(brightest_peak / 2))` in `1..=255`, with
/// `floor = median + max(K * sigma, MARGIN)` saturating at 255. With no blob
/// (`brightest_peak` `None`), the floor.
pub fn threshold_raw8(bg: Background, brightest_peak: Option<u8>) -> u8 {
    let spread = (K * bg.sigma as u16).max(MARGIN);
    let floor = bg.median as u16 + spread;
    let half_peak = brightest_peak.map_or(0, |p| (p as u16).div_ceil(2));
    floor.max(half_peak).clamp(1, 255) as u8
}

/// The RAW10 threshold: 4 x [`threshold_raw8`], in `4..=1020`. A 10-bit pixel is `>=` it
/// exactly when its top 8 bits are `>=` the RAW8 threshold, so both formats find the same
/// foreground.
pub fn threshold_raw10(bg: Background, brightest_peak: Option<u8>) -> u16 {
    threshold_raw8(bg, brightest_peak) as u16 * 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{NOISE_MAX, PEAK, Synth};

    const W: usize = 640;
    const H: usize = 400;
    const S: usize = W / 4 * raw10::GROUP_BYTES;

    fn hist8(frame: &[u8]) -> Histogram {
        let mut h = Histogram::new();
        sample_raw8(frame, W as u16, 0, &mut h);
        h
    }

    /// A synthetic frame with no disc: background only.
    fn noise(seed: u32) -> std::vec::Vec<u8> {
        let mut buf = std::vec![0u8; W * H];
        let mut synth = Synth::NEW;
        synth.fill(&mut buf, W, H, 0, seed);
        buf
    }

    #[test]
    fn samples_every_fourth_pixel_of_every_sixteenth_row() {
        let h = hist8(&noise(1));
        assert_eq!(h.total() as usize, (H / 16) * (W / 4));
        // Width not a multiple of 4: pixels 0, 4, 8; 17 rows: rows 0 and 16.
        let mut h = Histogram::new();
        sample_raw8(&[7; 9 * 17], 9, 0, &mut h);
        assert_eq!(h.bins[7], 6);
        assert_eq!(h.total(), 6);
        // A strip starting at row 5 samples its rows 11 and 27.
        let mut rows = [0u8; 8 * 30];
        rows[11 * 8..12 * 8].fill(1);
        rows[27 * 8..28 * 8].fill(2);
        let mut h = Histogram::new();
        sample_raw8(&rows, 8, 5, &mut h);
        assert_eq!((h.bins[1], h.bins[2], h.total()), (2, 2, 4));
        h.clear();
        assert_eq!(h, Histogram::default());
    }

    #[test]
    fn empty_histogram_has_no_background() {
        assert_eq!(Background::from_histogram(&Histogram::new()), None);
        let mut h = Histogram::new();
        sample_raw8(&[], 640, 0, &mut h);
        sample_raw8(&[1, 2, 3], 0, 0, &mut h);
        sample_raw10(&[1, 2, 3], 3, 0, &mut h);
        assert_eq!(Background::from_histogram(&h), None);
    }

    #[test]
    fn percentiles() {
        let mut h = Histogram::new();
        // 100 samples: 50 at 10, 34 at 12, 16 at 40.
        h.bins[10] = 50;
        h.bins[12] = 34;
        h.bins[40] = 16;
        let bg = Background::from_histogram(&h).unwrap();
        assert_eq!(
            bg,
            Background {
                median: 10,
                sigma: 2
            }
        );
        h.bins[12] = 33;
        h.bins[40] = 17;
        let bg = Background::from_histogram(&h).unwrap();
        assert_eq!(
            bg,
            Background {
                median: 10,
                sigma: 30
            }
        );
    }

    #[test]
    fn flat_noise_threshold_is_above_the_background() {
        for seed in [1, 2, 0xdead_beef] {
            let frame = noise(seed);
            let bg = Background::from_histogram(&hist8(&frame[..])).unwrap();
            let t = threshold_raw8(bg, None);
            let max = *frame.iter().max().unwrap();
            assert!(max < NOISE_MAX);
            assert!(t > max, "{bg:?} t {t} max {max}");
            assert!(threshold_raw10(bg, None) as u32 > max as u32 * 4 + 3);
        }
    }

    #[test]
    fn no_blob_gives_the_floor() {
        let bg = Background {
            median: 10,
            sigma: 5,
        };
        assert_eq!(threshold_raw8(bg, None), 40);
        // A dim blob below twice the floor leaves the floor.
        assert_eq!(threshold_raw8(bg, Some(60)), 40);
        assert_eq!(threshold_raw10(bg, None), 160);
        // MARGIN when the background is flat.
        let flat = Background {
            median: 10,
            sigma: 0,
        };
        assert_eq!(threshold_raw8(flat, None), 18);
        assert_eq!(threshold_raw8(Background::default(), None), MARGIN as u8);
        // Saturating floor.
        let bright = Background {
            median: 200,
            sigma: 40,
        };
        assert_eq!(threshold_raw8(bright, None), 255);
        assert_eq!(threshold_raw10(bright, Some(255)), 1020);
    }

    #[test]
    fn bright_spot_gives_half_its_peak() {
        let frame = noise(3);
        let bg = Background::from_histogram(&hist8(&frame[..])).unwrap();
        let floor = threshold_raw8(bg, None);
        for p in [PEAK, 201, 200, 2 * floor + 1] {
            assert_eq!(threshold_raw8(bg, Some(p)), p.div_ceil(2), "peak {p}");
            assert_eq!(threshold_raw10(bg, Some(p)), p.div_ceil(2) as u16 * 4);
        }
    }

    #[test]
    fn saturated_spot() {
        let bg = Background {
            median: 12,
            sigma: 3,
        };
        assert_eq!(threshold_raw8(bg, Some(255)), 128);
        assert_eq!(threshold_raw10(bg, Some(255)), 512);
    }

    /// The background here is binomial (6 + the bits set in 16 random bits: mean 14,
    /// sigma 2), so p50 and p84 fall inside a bin as with sensor noise. `synth`'s uniform
    /// background puts its median exactly on the edge between two values, where any marker
    /// sample moves it by one.
    #[test]
    fn markers_leave_the_background_unchanged() {
        let mut plain = std::vec![0u8; W * H];
        let mut discs = std::vec![0u8; W * H];
        let mut synth = Synth::NEW;
        for seed in [1u32, 2, 0xdead_beef] {
            let mut rng = seed;
            let dark: std::vec::Vec<u8> = (0..W * H)
                .map(|_| {
                    rng ^= rng << 13;
                    rng ^= rng >> 17;
                    rng ^= rng << 5;
                    6 + (rng & 0xffff).count_ones() as u8
                })
                .collect();
            // The discs of `synth` (where its frame with discs differs from the one
            // without) on that background.
            synth.fill(&mut plain, W, H, 0, seed);
            synth.fill(&mut discs, W, H, 16, seed);
            let mut lit = dark.clone();
            for ((l, &p), &d) in lit.iter_mut().zip(&plain).zip(&discs) {
                if d != p {
                    *l = d;
                }
            }
            // And a 3 x 48 pixel bar across sampled row 160: 12 more bright samples.
            for y in 160..163 {
                lit[y * W + 300..y * W + 348].fill(240);
            }
            let (hd, hl) = (hist8(&dark), hist8(&lit));
            let bright = hl.bins[64..].iter().sum::<u32>();
            assert!((12..=100).contains(&bright), "{bright} bright samples");
            let bg = Background::from_histogram(&hd);
            assert_eq!(
                bg,
                Some(Background {
                    median: 14,
                    sigma: 2
                })
            );
            assert_eq!(Background::from_histogram(&hl), bg, "seed {seed}");
        }
    }

    #[test]
    fn strips_sum_to_the_frame() {
        let mut buf8 = std::vec![0u8; W * H];
        let mut buf10 = std::vec![0u8; S * H];
        let mut synth = Synth::NEW;
        synth.fill(&mut buf8, W, H, 64, 7);
        synth.fill_raw10(&mut buf10, W, H, 64, 7);
        let whole8 = hist8(&buf8);
        let mut whole10 = Histogram::new();
        sample_raw10(&buf10, W as u16, 0, &mut whole10);
        // Same scene, same top 8 bits: the same histogram.
        assert_eq!(whole8, whole10);
        let mut rng = 0x1234_5678u32;
        for split in [1usize, 3, 7, 15, 16, 17, 33, 64, 399, 0] {
            let (mut h8, mut h10) = (Histogram::new(), Histogram::new());
            let mut row = 0;
            while row < H {
                // split 0: random strip heights 1..=40.
                let n = if split == 0 {
                    rng ^= rng << 13;
                    rng ^= rng >> 17;
                    rng ^= rng << 5;
                    1 + rng as usize % 40
                } else {
                    split
                };
                let n = n.min(H - row);
                sample_raw8(&buf8[row * W..(row + n) * W], W as u16, row as u16, &mut h8);
                sample_raw10(
                    &buf10[row * S..(row + n) * S],
                    W as u16,
                    row as u16,
                    &mut h10,
                );
                row += n;
            }
            assert_eq!(h8, whole8, "split {split}");
            assert_eq!(h10, whole10, "split {split}");
        }
    }

    #[test]
    fn raw10_bins_the_top_8_bits_of_pixel_0() {
        // Pixel 0 of each group: 4 * v + low bits, pixels 1..=3 bright (not sampled).
        let mut row = [0u8; 8 / 4 * raw10::GROUP_BYTES];
        for (k, g) in row
            .as_chunks_mut::<{ raw10::GROUP_BYTES }>()
            .0
            .iter_mut()
            .enumerate()
        {
            *g = raw10::pack([4 * 100 + 3 - k as u16, 1023, 1023, 1023]);
        }
        let mut frame = [0u8; 16 * 10];
        frame[..10].copy_from_slice(&row);
        let mut h = Histogram::new();
        sample_raw10(&frame, 8, 0, &mut h);
        assert_eq!((h.bins[100], h.bins[0], h.total()), (2, 0, 2));
        let top = raw10::pack([1023, 0, 0, 0]);
        let mut h = Histogram::new();
        sample_raw10(&top, 4, 0, &mut h);
        assert_eq!(h.bins[255], 1);
    }
}
