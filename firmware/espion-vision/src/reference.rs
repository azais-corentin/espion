//! Test reference for the detector: an 8-connected flood fill over unpacked pixels.

use std::vec;
use std::vec::Vec;

use crate::Blob;

/// The blobs of a frame of `f.len() / w` rows of `w` pixels at threshold `t`, with the
/// detector's centroid arithmetic and in its order: by last row, then by the leftmost
/// column in that row. `Blob::peak` is the brightest value shifted right by `peak_shift`
/// (2 for 10-bit values).
pub(crate) fn flood<T: Copy + Into<u32>>(f: &[T], w: usize, t: u32, peak_shift: u32) -> Vec<Blob> {
    let h = f.len() / w;
    let px = |p: usize| f[p].into();
    let mut seen = vec![false; w * h];
    let mut stack = Vec::new();
    let mut found = Vec::new();
    for start in 0..w * h {
        if seen[start] || px(start) < t {
            continue;
        }
        let (mut area, mut sw, mut sx, mut sy, mut peak) = (0u32, 0u64, 0u64, 0u64, 0);
        // The leftmost pixel of the last row: (row, column).
        let mut last = (start / w, start % w);
        seen[start] = true;
        stack.push(start);
        while let Some(p) = stack.pop() {
            let (x, y, v) = (p % w, p / w, px(p));
            area += 1;
            sw += v as u64;
            sx += v as u64 * x as u64;
            sy += v as u64 * y as u64;
            peak = peak.max(v);
            if y > last.0 || (y == last.0 && x < last.1) {
                last = (y, x);
            }
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let q = ny * w + nx;
                    if !seen[q] && px(q) >= t {
                        seen[q] = true;
                        stack.push(q);
                    }
                }
            }
        }
        let q5 = |s: u64| ((s * 32 + sw / 2) / sw) as u16;
        let blob = Blob {
            x: q5(sx),
            y: q5(sy),
            area: area as u16,
            peak: (peak >> peak_shift) as u8,
        };
        found.push((last, blob));
    }
    found.sort_unstable_by_key(|&(last, _)| last);
    found.into_iter().map(|(_, b)| b).collect()
}
