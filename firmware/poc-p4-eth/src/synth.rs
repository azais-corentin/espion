//! Deterministic synthetic RAW8 frames for timing the blob detector without a camera,
//! `core` only, no heap, integer math only.
//!
//! A frame is a dark noisy background, every pixel in `NOISE_MIN..NOISE_MAX`, with soft
//! discs on top: radius 3 to 5 pixels, a parabolic profile peaking at [`PEAK`] in the
//! centre and falling to the background at radius + 1. Discs sit at pseudo-random integer
//! centres, at least [`MARGIN`] pixels from the frame edge, and their bounding squares
//! stay at least 2 pixels apart, so at any threshold in `NOISE_MAX..=PEAK` every disc is
//! exactly one blob with its centroid on its centre.

/// Lowest background value.
pub const NOISE_MIN: u8 = 4;
/// Every background pixel is strictly below this.
pub const NOISE_MAX: u8 = 20;
/// Value of a disc's centre pixel.
pub const PEAK: u8 = 250;
/// Most discs per frame; larger counts are clamped.
pub const MAX_BLOBS: usize = 256;
/// Largest disc radius; the profile reaches radius + 1.
pub const MAX_RADIUS: u32 = 5;
/// Distance from a disc centre to the frame edge, at least.
pub const MARGIN: u32 = MAX_RADIUS + 2;

/// Placement attempts per disc before giving up (only when the frame is too crowded).
const ATTEMPTS: u32 = 1000;

struct Rng(u32);

impl Rng {
    /// xorshift32.
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
}

/// Frame generator. It holds the list of placed discs (3 KiB), so on the target keep it in
/// a static (built from [`Synth::NEW`]) rather than on a stack.
pub struct Synth {
    placed: [(u32, u32, u32); MAX_BLOBS],
}

impl Synth {
    /// An empty generator, for static initialisers.
    pub const NEW: Self = Self {
        placed: [(0, 0, 0); MAX_BLOBS],
    };

    /// Fills the first `width * height` bytes of `buf` (fewer rows if `buf` is shorter) with
    /// a frame holding `blobs` discs (at most [`MAX_BLOBS`], and fewer only if the frame has
    /// no room left for them). The same arguments always give the same frame.
    pub fn fill(&mut self, buf: &mut [u8], width: usize, height: usize, blobs: u16, seed: u32) {
        if width == 0 {
            return;
        }
        let height = height.min(buf.len() / width);
        let frame = &mut buf[..width * height];
        let mut rng = Rng(seed ^ 0x9e37_79b9);
        if rng.0 == 0 {
            rng.0 = 1;
        }

        // Background: four pixels per random word, each NOISE_MIN + 0..=15.
        const _: () = assert!(NOISE_MIN as u32 + 15 < NOISE_MAX as u32);
        for chunk in frame.chunks_mut(4) {
            let r = ((rng.next() >> 4) & 0x0f0f_0f0f) + NOISE_MIN as u32 * 0x0101_0101;
            let bytes = r.to_ne_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }

        let (w, h) = (width as u32, height as u32);
        if w <= 2 * MARGIN || h <= 2 * MARGIN {
            return;
        }
        let placed = &mut self.placed;
        let mut n = 0;
        let want = (blobs as usize).min(MAX_BLOBS);
        while n < want {
            let mut found = None;
            for _ in 0..ATTEMPTS {
                let r = 3 + rng.below(MAX_RADIUS - 2);
                let cx = MARGIN + rng.below(w - 2 * MARGIN);
                let cy = MARGIN + rng.below(h - 2 * MARGIN);
                // Squares of half side radius + 1 with a gap of at least 2 pixels.
                let clear = placed[..n].iter().all(|&(px, py, pr)| {
                    let gap = r + pr + 4;
                    cx.abs_diff(px) > gap || cy.abs_diff(py) > gap
                });
                if clear {
                    found = Some((cx, cy, r));
                    break;
                }
            }
            let Some(disc) = found else {
                return;
            };
            placed[n] = disc;
            n += 1;
            draw(frame, width, disc);
        }
    }
}

/// Draws a disc with value `PEAK * (rr - d²) / rr`, `rr = (radius + 1)²`, over the
/// background where it is brighter.
fn draw(frame: &mut [u8], width: usize, (cx, cy, r): (u32, u32, u32)) {
    let reach = r as i32 + 1;
    let rr = (reach * reach) as u32;
    for dy in -reach..=reach {
        let y = (cy as i32 + dy) as usize;
        let row = &mut frame[y * width..(y + 1) * width];
        for dx in -reach..=reach {
            let d2 = (dx * dx + dy * dy) as u32;
            if d2 >= rr {
                continue;
            }
            let v = (PEAK as u32 * (rr - d2) / rr) as u8;
            let p = &mut row[(cx as i32 + dx) as usize];
            *p = (*p).max(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vision::{Blob, DefaultDetector};

    #[test]
    fn detects_every_disc() {
        const W: usize = 640;
        const H: usize = 400;
        let mut buf = [0u8; W * H];
        let mut det = DefaultDetector::new();
        let mut synth = Synth::NEW;
        let mut out = [Blob::default(); 256];
        for blobs in [0u16, 1, 16, 64, 200] {
            for seed in [1, 2, 0xdead_beef] {
                synth.fill(&mut buf, W, H, blobs, seed);
                assert!(buf.iter().filter(|&&v| v < NOISE_MAX).count() > W * H / 2);
                for t in [NOISE_MAX, 128, PEAK] {
                    let n = det.process_frame(&buf, W as u16, t, &mut out);
                    assert_eq!(n, blobs as usize, "blobs {blobs} seed {seed} t {t}");
                    assert_eq!(det.stats().overflow, 0);
                    for b in &out[..n] {
                        // Symmetric discs: centroid exactly on an integer pixel.
                        assert_eq!((b.x % 32, b.y % 32), (0, 0), "{b:?}");
                        assert_eq!(b.peak, PEAK);
                    }
                }
            }
        }
    }
}
