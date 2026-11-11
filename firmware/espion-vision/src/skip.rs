//! The dark skip: where in a row the next bright pixel can be.
//!
//! The detector spends most of a frame here, so the skip is a trait: [`Scalar`] is the
//! portable version (the reference and the host path); with the cargo feature `pie`,
//! `esp32p4_pie::Pie` is the vector version for the ESP32-P4 (16 bytes per load). The
//! detector takes the skip as an argument of each call ([`crate::Detector::push_rows_with`])
//! instead of storing it, so an implementation can hold a token that is neither `Send` nor
//! `Sync` while the detector lives in a static.

use crate::raw10;

/// Bytes after a skip's answer within which it promises a bright byte (RAW8).
pub const WINDOW: usize = 64;
/// Bytes after a skip's answer within which it promises a bright pixel (RAW10): 64 pixels.
pub const WINDOW10: usize = 80;

/// Finds the next place where a pixel can be `>=` a threshold.
///
/// An answer may stop short of the bright pixel, by at most one window, but never past it:
/// the detector reads the window exactly. The detector does not trust the answers for memory
/// safety (it clamps them to the slice), only for correct blobs.
pub trait Skip {
    /// An offset `o <= bytes.len()` such that no byte of `bytes[..o]` is `>= threshold`,
    /// and either `o == bytes.len()` or some byte of `bytes[o..min(o + 64, len)]` is
    /// `>= threshold`. Any alignment and length.
    fn first_bright(&self, bytes: &[u8], threshold: u8) -> usize;

    /// `bytes` is CSI-2 RAW10 data ([`raw10`]) starting on a group boundary, a whole number
    /// of groups. An offset `o`, a multiple of 5, such that no pixel in `bytes[..o]` has its
    /// top 8 bits `>= threshold_top8`, and either `o == bytes.len()` or such a pixel lies in
    /// `bytes[o..min(o + 80, len)]`. The low-bit byte of each group (every fifth byte) is
    /// not a pixel and is ignored.
    fn first_bright10(&self, bytes: &[u8], threshold_top8: u8) -> usize;
}

/// The portable skip: SWAR tests on 32-bit words, 64 bytes per step.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scalar;

// Static dispatch, but not forced inline into the detector's strip loop: there the block
// loop runs short of registers (on the RISC-V build 42 instead of 39 instructions per 64
// bytes, counted under qemu) and the code grows by 12 KB. The kernels are inlined here, so
// each method is one call with no call inside.
impl Skip for Scalar {
    #[inline]
    fn first_bright(&self, bytes: &[u8], threshold: u8) -> usize {
        let (add, high) = swar_params(threshold);
        if high {
            first_bright8::<true>(bytes, threshold, add)
        } else {
            first_bright8::<false>(bytes, threshold, add)
        }
    }

    #[inline]
    fn first_bright10(&self, bytes: &[u8], threshold_top8: u8) -> usize {
        let (add, high) = swar_params(threshold_top8);
        if high {
            first_bright10::<true>(bytes, threshold_top8, add)
        } else {
            first_bright10::<false>(bytes, threshold_top8, add)
        }
    }
}

// The PIE kernels keep this contract as they are (`esp32p4-pie` README: `first_bright`
// answers the start of the 64-byte block holding the bright byte, `first_bright10` the
// start of the 80-byte pass; both exact in the scalar head and tail), so the impl only
// forwards. Inlined: each method is then one call of the kernel's split.
#[cfg(all(feature = "pie", target_arch = "riscv32"))]
impl Skip for esp32p4_pie::Pie {
    #[inline]
    fn first_bright(&self, bytes: &[u8], threshold: u8) -> usize {
        esp32p4_pie::Pie::first_bright(self, bytes, threshold)
    }

    #[inline]
    fn first_bright10(&self, bytes: &[u8], threshold_top8: u8) -> usize {
        esp32p4_pie::Pie::first_bright10(self, bytes, threshold_top8)
    }
}

const L7: u32 = 0x7f7f_7f7f;
const HI: u32 = 0x8080_8080;
const LO: u32 = 0x0101_0101;

/// Non-zero when any byte of `w` is `>= t`, with `add` and `HIGH` from [`swar_params`].
///
/// Adding `add` to the low 7 bits of each byte never carries into the next byte (both are
/// at most 127), and sets bit 7 exactly when the low 7 bits reach the threshold's.
#[inline(always)]
pub(crate) fn hot<const HIGH: bool>(w: u32, add: u32) -> u32 {
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
pub(crate) const fn swar_params(t: u8) -> (u32, bool) {
    if t <= 128 {
        ((128 - t as u32) * LO, false)
    } else {
        ((256 - t as u32) * LO, true)
    }
}

/// Words per RAW8 step: one 64-byte window.
const BLOCK: usize = WINDOW / 4;

/// [`Skip::first_bright`] of [`Scalar`].
///
/// A block passes when the OR of its words is hot: a byte of the OR is at least each byte
/// it combines, so a block with a bright byte always passes. A dark block can pass too (OR
/// of dark bytes reaching the threshold, e.g. 0x40 | 0x24 at threshold 100), so the words of
/// a block that passes are tested exactly and the skip goes on if none is hot. Same loads
/// per byte as a plain word loop; the exact test costs only on blocks that pass. The block
/// loop keeps only the OR, so the words do not hold registers across it (with them live,
/// the loop spills its end pointer: one more load per block); the test reloads them.
///
/// The words after the last whole block are tested as the block that ends with the last
/// word when the slice holds one (the words it shares with the last whole block are dark):
/// one block test instead of one test per word, on the rest of every row after a hit.
#[inline(always)]
fn first_bright8<const HIGH: bool>(bytes: &[u8], t: u8, add: u32) -> usize {
    let or = |b: &[u32; BLOCK]| b.iter().fold(0, |a, &w| a | w);
    let first_hot = |b: &[u32]| b.iter().position(|&w| hot::<HIGH>(w, add) != 0);
    // SAFETY: every bit pattern is a valid `u32`, and `align_to` only reinterprets the
    // middle part, which it aligns for `u32`; the slices cover `bytes` exactly, in order.
    let (pre, words, post) = unsafe { bytes.align_to::<u32>() };
    if let Some(i) = pre.iter().position(|&b| b >= t) {
        return i;
    }
    let lead = pre.len();
    let (blocks, rest) = words.as_chunks::<BLOCK>();
    let mut from = 0;
    while let Some(n) = blocks[from..]
        .iter()
        .position(|b| hot::<HIGH>(or(b), add) != 0)
    {
        let i = from + n;
        if let Some(k) = first_hot(&blocks[i]) {
            return lead + (i * BLOCK + k) * 4;
        }
        from = i + 1;
    }
    let tail = match words.last_chunk::<BLOCK>() {
        Some(last) if !rest.is_empty() && hot::<HIGH>(or(last), add) != 0 => {
            first_hot(last).map(|k| words.len() - BLOCK + k)
        }
        Some(_) => None,
        None => first_hot(rest),
    };
    if let Some(k) = tail {
        return lead + k * 4;
    }
    let base = lead + words.len() * 4;
    match post.iter().position(|&b| b >= t) {
        Some(i) => base + i,
        None => bytes.len(),
    }
}

/// Words per RAW10 step: one 80-byte window, 16 groups, 64 pixels.
const BLOCK10: usize = WINDOW10 / 4;

/// [`Skip::first_bright10`] of [`Scalar`].
///
/// The words are aligned, so their phase against the groups is the same in every block
/// (80 and 20 bytes are whole groups): word `k` of the slice takes mask `k % 5` of
/// [`raw10::SKIP_MASKS`], which zeroes the low-bit bytes; zero is below any threshold but 0,
/// and with threshold 0 every pixel is bright anyway. The rest are top bytes of pixels. A
/// block passes on the OR of its masked words (as in [`first_bright8`], with the same loop
/// split and the same last block), then its words are tested exactly; a hot word answers
/// with the start of the group holding its first byte, which is at most 4 bytes before it.
/// The bytes outside the words belong to the first and the last groups, which are tested
/// whole.
#[inline(always)]
fn first_bright10<const HIGH: bool>(bytes: &[u8], top: u8, add: u32) -> usize {
    let (groups, _) = bytes.as_chunks::<{ raw10::GROUP_BYTES }>();
    // SAFETY: as in `first_bright8`.
    let (pre, words, _) = unsafe { bytes.align_to::<u32>() };
    let lead = pre.len();
    // `lead < 4`: the bytes before the first word are in group 0.
    if lead > 0
        && let Some(g) = groups.first()
        && !raw10::top_below(g, top)
    {
        return 0;
    }
    let group_start = |word: usize| (lead + word * 4) / raw10::GROUP_BYTES * raw10::GROUP_BYTES;
    let m = &raw10::SKIP_MASKS[lead];
    let (blocks, rest) = words.as_chunks::<BLOCK10>();
    // `m` rotated to start at word `start` of the slice.
    let masks = |start: usize| -> [u32; 5] { core::array::from_fn(|j| m[(start + j) % 5]) };
    let or = |b: &[u32; BLOCK10], m: &[u32; 5]| {
        (b[0] | b[5] | b[10] | b[15]) & m[0]
            | (b[1] | b[6] | b[11] | b[16]) & m[1]
            | (b[2] | b[7] | b[12] | b[17]) & m[2]
            | (b[3] | b[8] | b[13] | b[18]) & m[3]
            | (b[4] | b[9] | b[14] | b[19]) & m[4]
    };
    let first_hot =
        |b: &[u32], m: &[u32; 5]| (0..b.len()).position(|k| hot::<HIGH>(b[k] & m[k % 5], add) != 0);
    let mut from = 0;
    while let Some(n) = blocks[from..]
        .iter()
        .position(|b| hot::<HIGH>(or(b, m), add) != 0)
    {
        let i = from + n;
        if let Some(k) = first_hot(&blocks[i], m) {
            return group_start(i * BLOCK10 + k);
        }
        from = i + 1;
    }
    let tail = match words.last_chunk::<BLOCK10>() {
        Some(last) if !rest.is_empty() => {
            let start = words.len() - BLOCK10;
            let m = masks(start);
            if hot::<HIGH>(or(last, &m), add) != 0 {
                first_hot(last, &m).map(|k| start + k)
            } else {
                None
            }
        }
        Some(_) => None,
        None => first_hot(rest, m),
    };
    if let Some(k) = tail {
        return group_start(k);
    }
    // The groups holding the bytes after the last word.
    let tail = (lead + words.len() * 4) / raw10::GROUP_BYTES;
    for (g, group) in groups.iter().enumerate().skip(tail) {
        if !raw10::top_below(group, top) {
            return g * raw10::GROUP_BYTES;
        }
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

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

        fn below(&mut self, n: u32) -> u32 {
            ((self.next() as u64 * n as u64) >> 32) as u32
        }
    }

    /// Thresholds of every class of the SWAR test: 0, 1, 2..=127, 128, 129..=255.
    const THRESHOLDS: [u8; 13] = [0, 1, 2, 3, 64, 100, 127, 128, 129, 130, 192, 254, 255];

    /// Checks `o` against the contract of [`Skip::first_bright`].
    fn check8(bytes: &[u8], t: u8, o: usize, ctx: &dyn Fn() -> std::string::String) {
        assert!(o <= bytes.len(), "{} o {o}", ctx());
        assert!(
            bytes[..o].iter().all(|&b| b < t),
            "{} o {o}: bright before",
            ctx()
        );
        let window = &bytes[o..(o + WINDOW).min(bytes.len())];
        assert!(
            o == bytes.len() || window.iter().any(|&b| b >= t),
            "{} o {o}: no bright in the window",
            ctx()
        );
    }

    /// A dark byte for threshold `t` (`t > 0`): often just below it, or a value whose OR with
    /// another dark value reaches it.
    fn dark(rng: &mut Rng, t: u8) -> u8 {
        let t = t as u32;
        let v = match rng.below(4) {
            0 => rng.below(t),
            1 => t - 1 - rng.below(t.min(4)),
            2 => [0x40, 0x24, 0x3f, 0x7f, 0x01][rng.below(5) as usize].min(t - 1),
            _ => 0,
        };
        v as u8
    }

    fn bright(rng: &mut Rng, t: u8) -> u8 {
        if rng.below(2) == 0 {
            t
        } else {
            (t as u32 + rng.below(256 - t as u32)) as u8
        }
    }

    #[test]
    fn first_bright_matches_contract() {
        let mut rng = Rng(0x0bad_5eed);
        let mut buf = [0u8; 4096 + 8];
        let lengths = (0..=200).chain([255, 256, 257, 1280, 1288, 4096]);
        for len in lengths {
            for align in 0..8 {
                for &t in &THRESHOLDS {
                    // 0: no bright byte; 1: first; 2: last; 3: anywhere; 4: two anywhere.
                    for place in 0..5 {
                        let bytes = &mut buf[align..align + len];
                        if t == 0 {
                            bytes.fill(0);
                        } else {
                            bytes.iter_mut().for_each(|b| *b = dark(&mut rng, t));
                        }
                        if t > 0 {
                            for i in places(&mut rng, place, len) {
                                bytes[i] = bright(&mut rng, t);
                            }
                        }
                        let bytes = &buf[align..align + len];
                        let o = Scalar.first_bright(bytes, t);
                        check8(bytes, t, o, &|| {
                            std::format!("len {len} align {align} t {t} place {place}")
                        });
                    }
                }
            }
        }
        // Every threshold on a few rows with one bright byte.
        for t in 1..=255u8 {
            for len in [63, 64, 65, 300] {
                let i = rng.below(len as u32) as usize;
                let mut row: Vec<u8> = (0..len).map(|_| dark(&mut rng, t)).collect();
                row[i] = t;
                let o = Scalar.first_bright(&row, t);
                check8(&row, t, o, &|| std::format!("len {len} t {t} i {i}"));
            }
        }
    }

    /// Where to put bright values in `n` pixels: 0: nowhere; 1: first; 2: last; 3: one
    /// anywhere; 4: two anywhere.
    fn places(rng: &mut Rng, place: u32, n: usize) -> Vec<usize> {
        match (place, n) {
            (_, 0) | (0, _) => Vec::new(),
            (1, _) => std::vec![0],
            (2, _) => std::vec![n - 1],
            _ => (2..place).map(|_| rng.below(n as u32) as usize).collect(),
        }
    }

    /// Checks `o` against the contract of [`Skip::first_bright10`].
    fn check10(bytes: &[u8], top: u8, o: usize, ctx: &dyn Fn() -> std::string::String) {
        let bright = |g: &[u8; raw10::GROUP_BYTES]| g[..4].iter().any(|&b| b >= top);
        assert!(o <= bytes.len(), "{} o {o}", ctx());
        assert_eq!(o % raw10::GROUP_BYTES, 0, "{} o {o}", ctx());
        let (before, _) = bytes[..o].as_chunks::<{ raw10::GROUP_BYTES }>();
        assert!(!before.iter().any(bright), "{} o {o}: bright before", ctx());
        let (window, _) =
            bytes[o..(o + WINDOW10).min(bytes.len())].as_chunks::<{ raw10::GROUP_BYTES }>();
        assert!(
            o == bytes.len() || window.iter().any(bright),
            "{} o {o}: no bright in the window",
            ctx()
        );
    }

    #[test]
    fn first_bright10_matches_contract() {
        let mut rng = Rng(0x1010_5eed);
        let mut buf = [0u8; 1610 + 8];
        let mut px = [0u16; 1288];
        let groups = (0..=60).chain([64, 128, 320, 322]);
        for ng in groups {
            let len = ng * raw10::GROUP_BYTES;
            for align in 0..8 {
                for &top in &THRESHOLDS {
                    for place in 0..5 {
                        let px = &mut px[..ng * 4];
                        for p in px.iter_mut() {
                            // Top 8 bits dark, low bits random: low-bit bytes often 0xff,
                            // brighter than the threshold if they were read as pixels.
                            let hi = if top == 0 { 0 } else { dark(&mut rng, top) };
                            let lo = if rng.below(2) == 0 { 3 } else { rng.below(4) };
                            *p = (hi as u16) << 2 | lo as u16;
                        }
                        if top > 0 {
                            for i in places(&mut rng, place, px.len()) {
                                px[i] = (bright(&mut rng, top) as u16) << 2 | rng.below(4) as u16;
                            }
                        }
                        let bytes = &mut buf[align..align + len];
                        let (out, _) = bytes.as_chunks_mut::<{ raw10::GROUP_BYTES }>();
                        for (g, p) in out.iter_mut().zip(px.as_chunks::<4>().0) {
                            *g = raw10::pack(*p);
                        }
                        let bytes = &buf[align..align + len];
                        let o = Scalar.first_bright10(bytes, top);
                        check10(bytes, top, o, &|| {
                            std::format!("groups {ng} align {align} top {top} place {place}")
                        });
                    }
                }
            }
        }
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
}
