//! Randomised equivalence of the detector with the flood-fill reference ([`reference`]):
//! frame widths and heights, blob counts, sizes and positions (borders included),
//! backgrounds, thresholds, both pixel formats, buffer alignments and strip splits. The
//! seeds are fixed, so a failure repeats.

use std::vec::Vec;
use std::{format, vec};

use crate::{Blob, Detector, raw10, reference};

const WIDTHS: [usize; 4] = [64, 320, 640, 1280];
const MAX_HEIGHT: u32 = 24;
/// Frames per width and format (both formats together: about 2 s in a debug build).
const CASES: u32 = 1000;
const MAX_BLOBS: u32 = 64;
const MAX_RADIUS: u32 = 12;
/// Thresholds drawn half of the time; the rest are uniform in `1..=max`.
const SPECIAL_RAW8: [u32; 7] = [1, 2, 127, 128, 129, 254, 255];
const SPECIAL_RAW10: [u32; 10] = [1, 3, 4, 508, 511, 512, 513, 516, 1020, 1023];

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
            let mut splits = Vec::new();
            let mut y = 0;
            while y < h {
                let n = 1 + rng.below((h - y) as u32) as usize;
                det.push_rows(&f[y * stride..(y + n) * stride], y as u16);
                splits.push(n);
                y += n;
            }
            let n = det.finish(&mut got);

            let ctx = format!(
                "raw10 {raw10} w {w} case {case} h {h} t {t} offset {offset} strips {splits:?}"
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
