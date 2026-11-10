//! Deterministic synthetic RAW8 frames for timing the blob detector without a camera,
//! `core` only, no heap, integer math only.
//!
//! A frame is a dark noisy background, every pixel in `NOISE_MIN..NOISE_MAX`, with soft
//! discs on top: radius 3 to 5 pixels, a parabolic profile peaking at [`PEAK`] in the
//! centre and falling to the background at radius + 1. Discs sit at pseudo-random integer
//! centres, at least [`MARGIN`] pixels from the frame edge, and their bounding squares
//! stay at least 2 pixels apart, so at any threshold in `NOISE_MAX..=PEAK` every disc is
//! exactly one blob with its centroid on its centre.

use crate::vision::raw10;

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
/// [`NOISE_MAX`] in 10-bit units: every background pixel of a RAW10 frame is below it.
pub const NOISE_MAX_RAW10: u16 = NOISE_MAX as u16 * 4;
/// Value of a disc's centre pixel in a RAW10 frame.
pub const PEAK_RAW10: u16 = PEAK as u16 * 4;

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

    fn new(seed: u32) -> Self {
        let s = seed ^ 0x9e37_79b9;
        Rng(if s == 0 { 1 } else { s })
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
        let mut rng = Rng::new(seed);

        // Background: four pixels per random word, each NOISE_MIN + 0..=15.
        const _: () = assert!(NOISE_MIN as u32 + 15 < NOISE_MAX as u32);
        for chunk in frame.chunks_mut(4) {
            let r = ((rng.next() >> 4) & 0x0f0f_0f0f) + NOISE_MIN as u32 * 0x0101_0101;
            let bytes = r.to_ne_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }

        self.place(&mut rng, width, height, blobs, |disc| {
            draw(frame, width, disc)
        });
    }

    /// The same scene as [`Self::fill`] with the same arguments, as 10-bit pixels packed
    /// in CSI-2 RAW10 ([`crate::vision::raw10`], rows of `width / 4 * 5` bytes) into the
    /// first `width / 4 * 5 * height` bytes of `buf` (fewer rows if `buf` is shorter).
    /// `width` must be a multiple of 4 (else nothing is written). The top 8 bits of every
    /// pixel equal the RAW8 frame; the low 2 bits are random in the background (below
    /// [`NOISE_MAX_RAW10`]) and come from the 10-bit disc profile (peak [`PEAK_RAW10`]).
    pub fn fill_raw10(
        &mut self,
        buf: &mut [u8],
        width: usize,
        height: usize,
        blobs: u16,
        seed: u32,
    ) {
        if width == 0 || !width.is_multiple_of(4) {
            return;
        }
        let stride = width / 4 * raw10::GROUP_BYTES;
        let height = height.min(buf.len() / stride);
        let frame = &mut buf[..stride * height];
        let mut rng = Rng::new(seed);

        // Background: one random word per group, as in `fill`, with bits 1..0 of each
        // byte as the low bits.
        for group in frame.as_chunks_mut::<{ raw10::GROUP_BYTES }>().0 {
            let r = rng.next();
            let px = [0, 8, 16, 24].map(|s| {
                let top = ((r >> (s + 4)) & 0x0f) + NOISE_MIN as u32;
                (top << 2 | (r >> s) & 3) as u16
            });
            *group = raw10::pack(px);
        }
        self.place(&mut rng, width, height, blobs, |disc| {
            draw_raw10(frame, stride, disc);
        });
    }

    /// Places up to `blobs` discs in a `width` x `height` frame and draws each.
    fn place(
        &mut self,
        rng: &mut Rng,
        width: usize,
        height: usize,
        blobs: u16,
        mut draw: impl FnMut((u32, u32, u32)),
    ) {
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
            draw(disc);
        }
    }
}

/// Calls `f(x, y, value)` for each pixel of a disc with value `peak * (rr - d²) / rr`,
/// `rr = (radius + 1)²`.
fn disc(peak: u32, (cx, cy, r): (u32, u32, u32), mut f: impl FnMut(usize, usize, u32)) {
    let reach = r as i32 + 1;
    let rr = (reach * reach) as u32;
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let d2 = (dx * dx + dy * dy) as u32;
            if d2 < rr {
                let (x, y) = ((cx as i32 + dx) as usize, (cy as i32 + dy) as usize);
                f(x, y, peak * (rr - d2) / rr);
            }
        }
    }
}

/// Draws a disc with peak [`PEAK`] over the background where it is brighter.
fn draw(frame: &mut [u8], width: usize, d: (u32, u32, u32)) {
    disc(PEAK as u32, d, |x, y, v| {
        let p = &mut frame[y * width + x];
        *p = (*p).max(v as u8);
    });
}

/// [`draw`] for a packed RAW10 frame, peak [`PEAK_RAW10`]. The top 8 bits of each value
/// are those of `draw` (`floor(floor(4 q) / 4) = floor(q)`).
fn draw_raw10(frame: &mut [u8], stride: usize, d: (u32, u32, u32)) {
    disc(PEAK_RAW10 as u32, d, |x, y, v| {
        let at = y * stride + x / 4 * raw10::GROUP_BYTES;
        let Some((group, _)) = frame[at..].split_first_chunk_mut::<{ raw10::GROUP_BYTES }>() else {
            return;
        };
        let mut px = raw10::unpack(group).map(|p| p as u16);
        px[x % 4] = px[x % 4].max(v as u16);
        *group = raw10::pack(px);
    });
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

    #[test]
    fn raw10_is_the_raw8_scene_and_detects_every_disc() {
        const W: usize = 640;
        const H: usize = 400;
        const S: usize = W / 4 * raw10::GROUP_BYTES;
        let mut buf8 = [0u8; W * H];
        let mut buf10 = [0u8; S * H];
        let mut det = DefaultDetector::new();
        let mut synth = Synth::NEW;
        let mut out = [Blob::default(); 256];
        for blobs in [0u16, 1, 16, 64, 200] {
            for seed in [1, 0xdead_beef] {
                synth.fill(&mut buf8, W, H, blobs, seed);
                synth.fill_raw10(&mut buf10, W, H, blobs, seed);
                let (mut low_dark, mut low_bright) = (0, 0);
                for (p8, g) in buf8.as_chunks::<4>().0.iter().zip(buf10.as_chunks().0) {
                    for (&v8, v) in p8.iter().zip(raw10::unpack(g)) {
                        assert_eq!(v >> 2, v8 as u32);
                        if v & 3 != 0 {
                            if v < NOISE_MAX_RAW10 as u32 {
                                low_dark += 1;
                            } else {
                                low_bright += 1;
                            }
                        }
                    }
                }
                assert!(low_dark > W * H / 2);
                assert_eq!(low_bright > 0, blobs > 0);
                for t in [NOISE_MAX_RAW10, 512, PEAK_RAW10] {
                    let n = det.process_frame_raw10(&buf10, W as u16, t, &mut out);
                    assert_eq!(n, blobs as usize, "blobs {blobs} seed {seed} t {t}");
                    assert_eq!(det.stats().overflow, 0);
                    for b in &out[..n] {
                        assert_eq!((b.x % 32, b.y % 32), (0, 0), "{b:?}");
                        assert_eq!(b.peak, PEAK);
                    }
                }
            }
        }
    }
}
