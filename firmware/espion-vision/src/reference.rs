//! Test references for the detector: an 8-connected flood fill over unpacked pixels, and
//! the split of a frame's blobs by their peaks stated on flood fills.

use std::vec;
use std::vec::Vec;

use crate::{Blob, LEVELS, SPLIT_DEN, SPLIT_MIN_AREA, SPLIT_NUM};

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
            sigma: sigma(sw, area, peak, t, 0),
        };
        found.push((last, blob));
    }
    found.sort_unstable_by_key(|&(last, _)| last);
    found.into_iter().map(|(_, b)| b).collect()
}

/// `Blob::sigma` of a component cut at `t` above the background `bg`: its pixels sum to
/// `sw`, `area` of them, the brightest `peak`.
fn sigma(sw: u64, area: u32, peak: u32, t: u32, bg: u32) -> u16 {
    let flux = (sw as u32).saturating_sub(bg.saturating_mul(area));
    crate::spot_sigma(flux, (peak - t).max((peak - bg) / 4))
}

/// A component of one threshold of a split frame.
struct Node {
    area: u32,
    /// Brightest value.
    peak: u32,
    blob: Blob,
    /// The components of the next threshold inside it.
    inside: Vec<usize>,
}

/// The components of the pixels of `member` (the pixels the detector takes as `>= t`) and
/// each pixel's component (`usize::MAX`: none); their blobs are cut at `t` above the
/// background `bg`.
fn components<T: Copy + Into<u32>>(
    f: &[T],
    w: usize,
    member: &[bool],
    (t, bg): (u32, u32),
    peak_shift: u32,
) -> (Vec<Node>, Vec<usize>) {
    let h = f.len() / w;
    let px = |p: usize| f[p].into();
    let mut of = vec![usize::MAX; w * h];
    let mut stack = Vec::new();
    let mut nodes = Vec::new();
    for start in 0..w * h {
        if of[start] != usize::MAX || !member[start] {
            continue;
        }
        let (mut area, mut sw, mut sx, mut sy, mut peak) = (0u32, 0u64, 0u64, 0u64, 0);
        of[start] = nodes.len();
        stack.push(start);
        while let Some(p) = stack.pop() {
            let (x, y, v) = (p % w, p / w, px(p));
            area += 1;
            sw += v as u64;
            sx += v as u64 * x as u64;
            sy += v as u64 * y as u64;
            peak = peak.max(v);
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let q = ny * w + nx;
                    if of[q] == usize::MAX && member[q] {
                        of[q] = nodes.len();
                        stack.push(q);
                    }
                }
            }
        }
        let q5 = |s: u64| ((s * 32 + sw / 2) / sw).min(u16::MAX as u64) as u16;
        let blob = Blob {
            x: q5(sx),
            y: q5(sy),
            area: area.min(u16::MAX as u32) as u16,
            peak: (peak >> peak_shift) as u8,
            sigma: sigma(sw, area, peak, t, bg),
        };
        nodes.push(Node {
            area,
            peak,
            blob,
            inside: Vec::new(),
        });
    }
    (nodes, of)
}

/// The thresholds of a split frame.
fn thresholds(t: u32, bg: u32, max: u32) -> Vec<u32> {
    let mut thresholds = vec![t];
    while thresholds.len() < LEVELS {
        let next = bg + 2 * (thresholds[thresholds.len() - 1] - bg);
        if next > max {
            break;
        }
        thresholds.push(next);
    }
    thresholds
}

/// The pixels the detector takes as at or above each threshold of a split frame. Those of
/// the frame's threshold are exact. In each row, threshold by threshold and left to right,
/// a run of one threshold that reaches the next is read again while the pixels read again
/// in the row stay within the row's pixels; a longer run counts whole for the next
/// threshold.
fn members<T: Copy + Into<u32>>(f: &[T], w: usize, thresholds: &[u32]) -> Vec<Vec<bool>> {
    let mut members = vec![vec![false; f.len()]; thresholds.len()];
    for (y, row) in f.chunks_exact(w).enumerate() {
        for (x, &v) in row.iter().enumerate() {
            members[0][y * w + x] = v.into() >= thresholds[0];
        }
        let mut room = w;
        for k in 1..thresholds.len() {
            let (below, this) = members.split_at_mut(k);
            let (below, this) = (&below[k - 1][y * w..][..w], &mut this[0][y * w..][..w]);
            let mut x = 0;
            while x < w {
                let len = below[x..].iter().take_while(|&&m| m == below[x]).count();
                let run = &row[x..x + len];
                if below[x] && run.iter().any(|&v| v.into() >= thresholds[k]) {
                    if len > room {
                        this[x..x + len].fill(true);
                    } else {
                        room -= len;
                        for (m, &v) in this[x..x + len].iter_mut().zip(run) {
                            *m = v.into() >= thresholds[k];
                        }
                    }
                }
                x += len;
            }
        }
    }
    members
}

/// The blobs of a frame split by its peaks (`Detector::set_split` with the background `bg`
/// below the threshold `t` and `Split::min_area` `piece_min`, pixel values up to `max`, the
/// area filter `areas`), in no order: the frame is cut at `t` and at up to `LEVELS - 1`
/// thresholds each twice as far above `bg` as the one before ([`members`]); a component
/// that holds blobs of two or more of the next threshold's components that count gives
/// those, any other gives itself. Also whether every higher threshold was read exactly (no
/// run counted whole).
pub(crate) fn split<T: Copy + Into<u32>>(
    f: &[T],
    w: usize,
    (t, bg, max): (u32, u32, u32),
    peak_shift: u32,
    areas: (u32, u32),
    piece_min: u32,
) -> (Vec<Blob>, bool) {
    let thresholds = thresholds(t, bg, max);
    let members = members(f, w, &thresholds);
    let exact = members
        .iter()
        .zip(&thresholds)
        .all(|(member, &t)| member.iter().zip(f).all(|(&m, &v)| m == (v.into() >= t)));
    let mut levels: Vec<Vec<Node>> = Vec::new();
    let mut below: Vec<usize> = Vec::new();
    for (k, &t) in thresholds.iter().enumerate() {
        let (nodes, of) = components(f, w, &members[k], (t, bg), peak_shift);
        if k > 0 {
            // Each component lies in the one of the threshold below that holds its pixels.
            let mut holder = vec![usize::MAX; nodes.len()];
            for (p, &c) in of.iter().enumerate() {
                if c != usize::MAX {
                    holder[c] = below[p];
                }
            }
            for (c, &up) in holder.iter().enumerate() {
                levels[k - 1][up].inside.push(c);
            }
        }
        levels.push(nodes);
        below = of;
    }
    /// The blobs of component `i` of threshold `k`.
    fn blobs(
        levels: &[Vec<Node>],
        thresholds: &[u32],
        bg: u32,
        min_area: u32,
        (k, i): (usize, usize),
    ) -> Vec<Blob> {
        let node = &levels[k][i];
        let mut found = Vec::new();
        for &c in &node.inside {
            let inner = &levels[k + 1][c];
            let sub = blobs(levels, thresholds, bg, min_area, (k + 1, c));
            let counts = SPLIT_DEN * (inner.peak - bg) >= SPLIT_NUM * (thresholds[k + 1] - bg)
                && inner.area >= min_area.max(SPLIT_MIN_AREA);
            if sub.len() >= 2 || counts {
                found.extend(sub);
            }
        }
        if found.len() >= 2 {
            found
        } else {
            vec![node.blob]
        }
    }
    let mut out = Vec::new();
    for (i, root) in levels[0].iter().enumerate() {
        let found = blobs(&levels, &thresholds, bg, areas.0.max(piece_min), (0, i));
        if found.len() >= 2 {
            out.extend(
                found
                    .into_iter()
                    .filter(|b| (areas.0..=areas.1).contains(&u32::from(b.area))),
            );
        } else if (areas.0..=areas.1).contains(&root.area) {
            out.push(root.blob);
        }
    }
    (out, exact)
}
