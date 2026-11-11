//! How each kernel splits a slice into a scalar head, a vector middle on 16-byte boundaries
//! and a scalar tail.
//!
//! The splits take the vector core as a closure and do not depend on the target, so the
//! host tests run them with scalar stand-ins for the cores, at real addresses and every
//! alignment. On the chip the closures are the `asm!` kernels of `pie.rs`.

use crate::{first_bright_group, fold16};

/// RAW10 bytes per vector pass: lcm(5, 16) = 80, 16 groups, 64 pixels. The low-bit byte
/// positions repeat with this period, so five fixed mask vectors cover them.
pub(crate) const RAW10_BLOCK: usize = 80;

/// [`Pie::first_bright10`](crate::Pie::first_bright10) around a vector core.
///
/// The head is whole groups, as many as it takes to reach a 16-byte boundary (0 to 15
/// groups, up to 75 bytes), scanned one group at a time. That keeps every vector pass on
/// the same 80-byte phase (it starts on a group boundary), so the masks are constants and
/// a hit in a pass is a valid answer: the pass starts a group and holds the bright pixel
/// byte. `blocks(mid, t)` gets a 16-byte-aligned `mid` of whole 80-byte passes starting on
/// a group boundary and returns the offset of the first pass with a pixel byte `>= t`, or
/// `mid.len()`. The tail (fewer than 80 bytes) is scanned one group at a time.
pub(crate) fn first_bright10(
    bytes: &[u8],
    threshold_top8: u8,
    blocks: impl FnOnce(&[u8], u8) -> usize,
) -> usize {
    // 5 * 13 = 65 = 1 (mod 16), so 13 * d (mod 16) groups advance the address by d (mod 16).
    let to_boundary = (bytes.as_ptr() as usize).wrapping_neg() & 15;
    let head = (5 * (13 * to_boundary % 16)).min(bytes.len());
    let (head_bytes, rest) = bytes.split_at(head);
    if let Some(o) = first_bright_group(head_bytes, threshold_top8) {
        return o;
    }
    let (mid, tail) = rest.split_at(rest.len() / RAW10_BLOCK * RAW10_BLOCK);
    let o = blocks(mid, threshold_top8);
    if o < mid.len() {
        return head + o;
    }
    head + mid.len() + first_bright_group(tail, threshold_top8).unwrap_or(tail.len())
}

/// [`Pie::copy`](crate::Pie::copy) around a vector core: copies `src` to `dst`, panicking as
/// `copy_from_slice` does when the lengths differ.
///
/// The head (0 to 15 bytes) brings the destination to a 16-byte boundary, so every vector
/// store is aligned. `mid(dst, src)` gets a 16-byte-aligned `dst` and a `src` of the same
/// length, a multiple of 16, at any alignment. The tail (0 to 15 bytes) follows. Head and
/// tail use `copy_from_slice`.
#[track_caller]
pub(crate) fn copy(dst: &mut [u8], src: &[u8], mid: impl FnOnce(&mut [u8], &[u8])) {
    assert!(
        dst.len() == src.len(),
        "source slice length ({}) does not match destination slice length ({})",
        src.len(),
        dst.len(),
    );
    let head = ((dst.as_ptr() as usize).wrapping_neg() & 15).min(dst.len());
    let (dst_head, dst) = dst.split_at_mut(head);
    let (src_head, src) = src.split_at(head);
    dst_head.copy_from_slice(src_head);
    let n = dst.len() & !15;
    let (dst_mid, dst_tail) = dst.split_at_mut(n);
    let (src_mid, src_tail) = src.split_at(n);
    mid(dst_mid, src_mid);
    dst_tail.copy_from_slice(src_tail);
}

/// Most bytes per call of the vector core of `sum16_be`: 4096 vectors of eight 16-bit
/// lanes, whose sum is at most 4096 * 8 * 0xFFFF = 2 147 450 880 < 2^31. So the 40-bit
/// accumulator never saturates and its low 32 bits (`esp.movx.r.xacc.l`) are the whole sum.
pub(crate) const SUM_CHUNK: usize = 65536;

/// [`Pie::sum16_be`](crate::Pie::sum16_be) around a vector core: the same value as
/// [`sum16_be_scalar`](crate::sum16_be_scalar), for any alignment and length.
///
/// `le_lanes(mid)` gets a 16-byte-aligned `mid` of at most [`SUM_CHUNK`] bytes, a multiple
/// of 16, and returns the plain sum of its little-endian 16-bit lanes (lane `j` is
/// `mid[2j] + 256 * mid[2j + 1]`). Head (0 to 15 bytes, up to the first 16-byte boundary)
/// and tail (0 to 15 bytes) are summed one byte at a time.
///
/// Why the lanes give the big-endian sum. Write the whole sum as a weighted byte sum: byte
/// `i` of `bytes` weighs 256 when `i` is even (high byte of its word) and 1 when odd. In the
/// middle, which starts at offset `h` (the head length), a lane holds bytes `h + 2j` (weight
/// 1 in the lane) and `h + 2j + 1` (weight 256).
/// - `h` odd: the lane weights are the big-endian weights, so the lane sum `L` is exactly
///   the middle's share of the sum.
/// - `h` even: the weights are swapped; each lane is the byte swap of the word the
///   checksum takes. For a 16-bit `x = 256 a + b`, `swap(x) = 256 b + a`, and
///   `256 x = 65536 a + 256 b = a + 256 b = swap(x) (mod 0xFFFF)`. So the middle's share
///   is `256 L (mod 0xFFFF)`, which is `swap(fold16(L)) (mod 0xFFFF)` because
///   `fold16(L) = L (mod 0xFFFF)`. Zero is kept too: `fold16(L)` is 0 only when `L` is,
///   that is when every middle byte is 0, and `swap` maps 0 to 0 and nonzero to nonzero.
///
/// The final `fold16` of head share + middle share + tail share then equals
/// `sum16_be_scalar`: both are in `0..=0xFFFF`, agree modulo 0xFFFF and are 0 exactly when
/// all bytes are 0; those three facts determine the value (0xFFFF is the only nonzero
/// value in the zero class).
pub(crate) fn sum16_be(bytes: &[u8], mut le_lanes: impl FnMut(&[u8]) -> u32) -> u32 {
    let head = ((bytes.as_ptr() as usize).wrapping_neg() & 15).min(bytes.len());
    let (head_bytes, rest) = bytes.split_at(head);
    let (mid, tail) = rest.split_at(rest.len() & !15);
    let lanes: u64 = mid
        .chunks(SUM_CHUNK)
        .map(|chunk| u64::from(le_lanes(chunk)))
        .sum();
    let mut mid_share = fold16(lanes);
    if head.is_multiple_of(2) {
        mid_share = u32::from((mid_share as u16).swap_bytes());
    }
    fold16(weighted(head_bytes, 0) + u64::from(mid_share) + weighted(tail, head + mid.len()))
}

/// The big-endian word sum share of `bytes` lying at offset `start` of the checksummed data:
/// a byte at an even offset weighs 256, at an odd offset 1.
fn weighted(bytes: &[u8], start: usize) -> u64 {
    bytes
        .iter()
        .enumerate()
        .map(|(i, &b)| u64::from(b) << if (start + i).is_multiple_of(2) { 8 } else { 0 })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::raw10::{contract_holds, generate, scalar_blocks};
    use crate::check::{Buffer, Rng, copy as copy_case, sum as sum_case};
    use crate::sum16_be_scalar;

    #[test]
    fn first_bright10_split_keeps_the_contract() {
        let mut buf = Buffer::new();
        let mut rng = Rng::new(0x0bad_cafe);
        for i in 0..20_000 {
            let c = generate(&mut rng, i, &mut buf);
            let s = &buf.0[c.start..c.start + c.len];
            let o = first_bright10(s, c.threshold, scalar_blocks);
            assert!(contract_holds(s, c.threshold, o), "case {i}: {c:?} -> {o}");
            assert_eq!(
                o < c.len,
                crate::first_bright10_scalar(s, c.threshold) < c.len,
                "case {i}: {c:?} -> {o}"
            );
        }
    }

    #[test]
    fn copy_split_copies_every_byte() {
        let [mut src, mut dst] = [Buffer::new(), Buffer::new()];
        let mut rng = Rng::new(0x5eed_c0de);
        let mut mids = 0;
        for i in 0..20_000 {
            let c = copy_case::generate(&mut rng, i, &mut src, &mut dst);
            copy(
                &mut dst.0[c.dst..c.dst + c.len],
                &src.0[c.src..c.src + c.len],
                |d, s| {
                    assert!((d.as_ptr() as usize).is_multiple_of(16) || d.is_empty());
                    assert!(d.len().is_multiple_of(16) && d.len() == s.len());
                    mids += usize::from(!d.is_empty());
                    d.copy_from_slice(s);
                },
            );
            assert!(copy_case::copied(&src, &dst, &c), "case {i}: {c:?}");
        }
        assert!(mids > 10_000);
    }

    #[test]
    #[should_panic(expected = "does not match destination slice length")]
    fn copy_split_refuses_different_lengths() {
        copy(&mut [0; 4], &[0; 5], |_, _| {});
    }

    #[test]
    fn sum16_split_equals_reference() {
        let mut buf = Buffer::new();
        let mut rng = Rng::new(0x1357_9bdf);
        for i in 0..20_000 {
            let c = sum_case::generate(&mut rng, i, &mut buf);
            let s = &buf.0[c.start..c.start + c.len];
            assert_eq!(
                sum16_be(s, sum_case::scalar_lanes),
                sum16_be_scalar(s),
                "case {i}: {c:?}"
            );
        }
    }

    /// Several chunks for the core, every start alignment, both head parities.
    #[test]
    fn sum16_split_long_slices() {
        #[repr(C, align(16))]
        struct Big([u8; 3 * SUM_CHUNK + 64]);
        let mut big = Big([0xFF; 3 * SUM_CHUNK + 64]);
        // All 0xFF over 64 KiB: the largest lane sum one call returns (the core asserts
        // it stays below 2^31), and the folded result 0xFFFF.
        for start in 0..16 {
            let s = &big.0[start..start + SUM_CHUNK];
            assert_eq!(sum16_be(s, sum_case::scalar_lanes), 0xFFFF);
        }
        let mut rng = Rng::new(7);
        big.0.iter_mut().for_each(|b| *b = rng.next() as u8);
        for start in 0..16 {
            for len in [
                SUM_CHUNK - 1,
                SUM_CHUNK + 17,
                2 * SUM_CHUNK + 32,
                3 * SUM_CHUNK + 1,
            ] {
                let s = &big.0[start..start + len];
                assert_eq!(
                    sum16_be(s, sum_case::scalar_lanes),
                    sum16_be_scalar(s),
                    "start {start}, len {len}"
                );
            }
        }
    }

    /// The step `sum16_be` rests on: the folded sum of byte-swapped words is the byte swap
    /// of the folded sum of the words.
    #[test]
    fn swapped_words_swap_the_folded_sum() {
        let mut rng = Rng::new(99);
        for n in 0..2000 {
            let words: [u16; 64] = core::array::from_fn(|_| match n % 4 {
                0 => 0xFFFF,
                1 => 0,
                _ => rng.next() as u16,
            });
            let words = &words[..n % 65];
            let sum = |f: fn(u16) -> u16| fold16(words.iter().map(|&w| u64::from(f(w))).sum());
            let swapped = sum(u16::swap_bytes);
            let plain = sum(|w| w);
            assert_eq!(
                swapped,
                u32::from((plain as u16).swap_bytes()),
                "{words:x?}"
            );
        }
    }
}
