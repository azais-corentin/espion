//! Randomised equivalence of the detector with the flood-fill reference ([`reference`]):
//! frame widths and heights, blob counts, sizes and positions (borders included),
//! backgrounds, thresholds, both pixel formats, buffer alignments, strip splits and the
//! answers a [`Skip`] may give. The seeds are fixed, so a failure repeats.

use std::vec::Vec;
use std::{format, vec};

use crate::{Blob, Detector, Scalar, Skip, raw10, reference};

/// Below, at and above one 64-byte window, not multiples of 64 (72, 1288) and the sizes of
/// the board's modes.
const WIDTHS: [usize; 8] = [4, 36, 64, 72, 320, 640, 1280, 1288];
const MAX_HEIGHT: u32 = 24;
/// Frames per width and format.
const CASES: u32 = 1000;
const MAX_BLOBS: u32 = 64;
const MAX_RADIUS: u32 = 12;
/// Thresholds drawn half of the time; the rest are uniform in `1..=max`.
const SPECIAL_RAW8: [u32; 7] = [1, 2, 127, 128, 129, 254, 255];
const SPECIAL_RAW10: [u32; 10] = [1, 3, 4, 508, 511, 512, 513, 516, 1020, 1023];

/// Skips with other answers than [`Scalar`] that keep the [`Skip`] contract: the detector
/// must find the same blobs whichever the contract allows.
#[derive(Clone, Copy, Debug)]
enum Model {
    Scalar,
    /// The start of the window-sized block, counted from the slice start, that holds the
    /// first bright pixel, as a vector skip answers.
    Block,
    /// As early as the contract allows: the bright pixel at the end of the window.
    Early,
}

impl Skip for Model {
    fn first_bright(&self, bytes: &[u8], threshold: u8) -> usize {
        let len = bytes.len();
        let first = bytes.iter().position(|&b| b >= threshold).unwrap_or(len);
        match self {
            Model::Scalar => Scalar.first_bright(bytes, threshold),
            _ if first == len => len,
            Model::Block => first / 64 * 64,
            Model::Early => first.saturating_sub(63),
        }
    }

    fn first_bright10(&self, bytes: &[u8], threshold_top8: u8) -> usize {
        let len = bytes.len();
        let (groups, _) = bytes.as_chunks::<{ raw10::GROUP_BYTES }>();
        let first = groups
            .iter()
            .position(|g| g[..4].iter().any(|&b| b >= threshold_top8))
            .map_or(len, |g| g * raw10::GROUP_BYTES);
        match self {
            Model::Scalar => Scalar.first_bright10(bytes, threshold_top8),
            _ if first == len => len,
            Model::Block => first / 80 * 80,
            // The top byte of the group's last pixel is 3 bytes after the group start.
            Model::Early => first.saturating_sub(75),
        }
    }
}

/// xorshift32.
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

    /// Roughly uniform in `0..n` (`n > 0`).
    fn below(&mut self, n: u32) -> u32 {
        ((self.next() as u64 * n as u64) >> 32) as u32
    }

    /// A coordinate in `0..n`, on the first or last line a quarter of the time.
    fn position(&mut self, n: u32) -> u32 {
        match self.below(8) {
            0 => 0,
            1 => n - 1,
            _ => self.below(n),
        }
    }
}

/// A random frame of `w` x `h` values in `0..=max`, every background value below `t`.
fn scene(rng: &mut Rng, w: usize, h: usize, t: u32, max: u32) -> Vec<u16> {
    let mode = rng.below(4);
    let mut px: Vec<u16> = (0..w * h)
        .map(|_| {
            let v = match mode {
                0 => rng.below(t),
                // Just below the threshold: the skip's worst case.
                1 => t - 1 - rng.below(t.min(8)),
                2 => 0,
                // Low bits set where that stays dark (RAW10: low-bit bytes 0xff).
                _ => {
                    let v = rng.below(t);
                    if v | 3 < t { v | 3 } else { v }
                }
            };
            v as u16
        })
        .collect();
    for _ in 0..rng.below(MAX_BLOBS + 1) {
        let r = 1 + rng.below(MAX_RADIUS) as i32;
        let cx = rng.position(w as u32) as i32;
        let cy = rng.position(h as u32) as i32;
        // Up to 3 in 8 pixels of the disc left as they are: ragged edges, holes, splits.
        let holes = rng.below(4);
        for y in (cy - r).max(0)..=(cy + r).min(h as i32 - 1) {
            for x in (cx - r).max(0)..=(cx + r).min(w as i32 - 1) {
                let (dx, dy) = (x - cx, y - cy);
                if dx * dx + dy * dy <= r * r && rng.below(8) >= holes {
                    px[y as usize * w + x as usize] = (t + rng.below(max + 1 - t)) as u16;
                }
            }
        }
    }
    px
}

fn run(raw10: bool, seed: u32) {
    let (max, special): (u32, &[u32]) = if raw10 {
        (1023, &SPECIAL_RAW10)
    } else {
        (255, &SPECIAL_RAW8)
    };
    let mut rng = Rng(seed);
    // Rows of 1280 pixels hold at most 640 runs; merged slots live until the row ends.
    let mut det = Detector::<1024, 2048, 2048>::new();
    let mut got = vec![Blob::default(); 2048];
    for w in WIDTHS {
        let stride = if raw10 { w / 4 * raw10::GROUP_BYTES } else { w };
        for case in 0..CASES {
            let h = 1 + rng.below(MAX_HEIGHT) as usize;
            let t = if rng.below(2) == 0 {
                special[rng.below(special.len() as u32) as usize]
            } else {
                1 + rng.below(max)
            };
            let px = scene(&mut rng, w, h, t, max);
            let want = reference::flood(&px, w, t, if raw10 { 2 } else { 0 });

            let offset = rng.below(4) as usize;
            let mut buf = vec![0u8; stride * h + 4];
            let f = &mut buf[offset..offset + stride * h];
            if raw10 {
                for (p, g) in px.as_chunks::<4>().0.iter().zip(f.as_chunks_mut().0) {
                    *g = raw10::pack(*p);
                }
                det.begin_frame_raw10(w as u16, t as u16);
            } else {
                for (b, &p) in f.iter_mut().zip(&px) {
                    *b = p as u8;
                }
                det.begin_frame(w as u16, t as u8);
            }
            let model = [Model::Scalar, Model::Block, Model::Early][rng.below(3) as usize];
            let mut splits = Vec::new();
            let mut y = 0;
            while y < h {
                let n = 1 + rng.below((h - y) as u32) as usize;
                det.push_rows_with(&model, &f[y * stride..(y + n) * stride], y as u16);
                splits.push(n);
                y += n;
            }
            let n = det.finish(&mut got);

            let ctx = format!(
                "raw10 {raw10} w {w} case {case} h {h} t {t} offset {offset} {model:?} \
                 strips {splits:?}"
            );
            let s = det.stats();
            assert_eq!(s.overflow, 0, "{ctx}");
            let bright = px.iter().filter(|&&v| u32::from(v) >= t).count();
            assert_eq!(s.pixels as usize, bright, "{ctx}");
            assert_eq!(got[..n], want[..], "{ctx}");
        }
    }
}

#[test]
fn raw8_random_scenes_match_flood_fill() {
    run(false, 0x5eed_0008);
}

#[test]
fn raw10_random_scenes_match_flood_fill() {
    run(true, 0x5eed_0010);
}
