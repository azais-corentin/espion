//! Randomised cases for [`first_bright10`](crate::Pie::first_bright10) and its byte-wise
//! contract.

use super::{Buffer, Case, GUARD, Rng, bright, dark, threshold};
use crate::first_bright10_scalar;
use crate::split::RAW10_BLOCK;

/// Longest generated slice: 21 passes of 80 bytes (1344 pixels), more than a 1280-pixel
/// RAW10 row (1600 bytes).
pub(crate) const MAX_LEN: usize = 1680;
const _: () = assert!(GUARD + 16 + MAX_LEN + GUARD <= super::BUF_LEN);

/// Byte `i` of RAW10 data from a group boundary is the top 8 bits of a pixel (not the
/// low-bit byte of its group).
fn is_pixel(i: usize) -> bool {
    i % 5 != 4
}

/// Byte offset of pixel `p` in RAW10 data from a group boundary.
fn pixel_offset(p: usize) -> usize {
    p / 4 * 5 + p % 4
}

/// Fills `buf` for case number `index` and returns where the slice is.
///
/// Every byte outside the slice is 0xFF, so a kernel that reads past either end reports a
/// bright pixel where the slice has none. Three of four low-bit bytes are 0xFF and the rest
/// random, so a kernel that counts low-bit bytes reports a hit on a dark row. The start
/// alignment cycles through 0..16 with `index`.
pub(crate) fn generate(rng: &mut Rng, index: u32, buf: &mut Buffer) -> Case {
    let start = GUARD + (index % 16) as usize;
    let len = 5 * groups(rng);
    let threshold = threshold(rng);

    buf.0.fill(0xFF);
    let slice = &mut buf.0[start..start + len];
    let kind = rng.below(6);
    for (i, b) in slice.iter_mut().enumerate() {
        *b = if !is_pixel(i) {
            if rng.below(4) == 0 {
                rng.below(256) as u8
            } else {
                0xFF
            }
        } else if kind == 1 || kind == 5 {
            // Threshold - 1 on every pixel: the background a `>` in place of `>=` gets wrong.
            threshold.saturating_sub(1)
        } else {
            dark(rng, threshold)
        };
    }
    if len > 0 {
        let pixels = (len / 5 * 4) as u32;
        let any_pixel = |rng: &mut Rng| pixel_offset(rng.below(pixels) as usize);
        match kind {
            2 => slice[any_pixel(rng)] = threshold,
            // A pixel of the first group, then of the last group.
            3 => slice[rng.below(4) as usize] = bright(rng, threshold),
            4 => slice[len - 5 + rng.below(4) as usize] = bright(rng, threshold),
            5 => {
                for _ in 0..=rng.below(4) {
                    let at = any_pixel(rng);
                    slice[at] = if rng.below(2) == 0 {
                        threshold
                    } else {
                        bright(rng, threshold)
                    };
                }
            }
            _ => {}
        }
    }
    Case {
        start,
        len,
        threshold,
    }
}

/// Lengths in groups: uniform, whole 80-byte passes, whole passes plus or minus 1..15
/// groups, and short slices of up to 31 groups.
fn groups(rng: &mut Rng) -> usize {
    const PASS: usize = RAW10_BLOCK / 5;
    let max = MAX_LEN / 5;
    let any = rng.below(max as u32 + 1) as usize;
    match rng.below(4) {
        0 => any,
        1 => any / PASS * PASS,
        2 => {
            let near = any / PASS * PASS;
            let off = 1 + rng.below(PASS as u32 - 1) as usize;
            if rng.below(2) == 0 {
                near.saturating_sub(off)
            } else {
                (near + off).min(max)
            }
        }
        _ => rng.below(32) as usize,
    }
}

/// The contract of `first_bright10`, byte by byte: `o` is a multiple of 5 (or the length),
/// no pixel byte of `bytes[..o]` is at or above `threshold`, and either `o` is the length
/// or a pixel byte of the 80 from `o` is.
pub(crate) fn contract_holds(bytes: &[u8], threshold: u8, o: usize) -> bool {
    let len = bytes.len();
    let bright = |i: usize| is_pixel(i) && bytes[i] >= threshold;
    o <= len
        && (o.is_multiple_of(5) || o == len)
        && !(0..o).any(bright)
        && (o == len || (o..len.min(o + RAW10_BLOCK)).any(bright))
}

/// A kernel result `o` agrees with the reference: it keeps the contract and reports a
/// bright pixel exactly when [`first_bright10_scalar`] does.
#[cfg_attr(not(target_arch = "riscv32"), allow(dead_code))]
pub(crate) fn agrees(bytes: &[u8], threshold: u8, o: usize) -> bool {
    let len = bytes.len();
    contract_holds(bytes, threshold, o)
        && ((o < len) == (first_bright10_scalar(bytes, threshold) < len))
}

/// Scalar stand-in for the vector core of `first_bright10` in host tests. Checks the
/// preconditions the split promises the core: 16-byte-aligned start, whole 80-byte passes.
#[cfg(test)]
pub(crate) fn scalar_blocks(mid: &[u8], threshold: u8) -> usize {
    assert!((mid.as_ptr() as usize).is_multiple_of(16) || mid.is_empty());
    assert!(mid.len().is_multiple_of(RAW10_BLOCK));
    first_bright10_scalar(mid, threshold)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASES: u32 = 50_000;

    #[test]
    fn scalar_reference_keeps_the_contract() {
        let mut buf = Buffer::new();
        let mut rng = Rng::new(0x8765_4321);
        let (mut found, mut empty, mut full, mut passes) = (0, 0, 0, 0);
        let mut aligned = [false; 16];
        let mut thresholds = [false; 256];
        for i in 0..CASES {
            let c = generate(&mut rng, i, &mut buf);
            let s = &buf.0[c.start..c.start + c.len];
            let o = first_bright10_scalar(s, c.threshold);
            assert!(contract_holds(s, c.threshold, o), "case {i}: {c:?} -> {o}");
            assert!(agrees(s, c.threshold, o), "case {i}: {c:?} -> {o}");
            assert!(o == c.len || o.is_multiple_of(80), "case {i}: {c:?} -> {o}");
            assert!(c.len.is_multiple_of(5) && c.len <= MAX_LEN);
            assert!(buf.0[..c.start].iter().all(|&b| b == 0xFF));
            assert!(buf.0[c.start + c.len..].iter().all(|&b| b == 0xFF));
            assert!(buf.0.len() - (c.start + c.len) >= GUARD);
            found += usize::from(o < c.len);
            empty += usize::from(c.len == 0);
            full += usize::from(c.len == MAX_LEN);
            passes += usize::from(c.len > 0 && c.len.is_multiple_of(80));
            aligned[(buf.0.as_ptr() as usize + c.start) % 16] = true;
            thresholds[c.threshold as usize] = true;
        }
        assert!(
            found > CASES as usize / 3 && found < CASES as usize * 9 / 10,
            "{found}"
        );
        assert!(empty > 0 && full > 0 && passes > CASES as usize / 10);
        assert!(aligned.iter().all(|&a| a));
        assert!(
            super::super::THRESHOLDS
                .iter()
                .all(|&t| thresholds[t as usize])
        );
        assert!(thresholds[0]);
    }

    #[test]
    fn low_bit_bytes_never_count() {
        // Every low-bit byte 0xFF, every pixel 0: dark at any threshold above 0.
        let row: [u8; 400] = core::array::from_fn(|i| if is_pixel(i) { 0 } else { 0xFF });
        for t in 1..=255 {
            assert_eq!(first_bright10_scalar(&row, t), row.len());
            assert!(contract_holds(&row, t, row.len()));
            assert!(!contract_holds(&row, t, 0));
        }
    }

    #[test]
    fn contract_rejects_wrong_offsets() {
        let mut row = [0u8; 400];
        row[202] = 9; // pixel 2 of group 40
        // Valid: multiples of 5 from 125 (202 - 80 + 1 rounded up) to 200.
        assert!(contract_holds(&row, 9, 125));
        assert!(contract_holds(&row, 9, 160));
        assert!(contract_holds(&row, 9, 200));
        assert!(!contract_holds(&row, 9, 120));
        assert!(!contract_holds(&row, 9, 205));
        assert!(!contract_holds(&row, 9, 162));
        assert!(!contract_holds(&row, 9, 400));
        assert!(contract_holds(&row, 10, 400));
        assert!(!agrees(&row, 9, 400));
        assert!(agrees(&row, 9, 160));
        assert_eq!(first_bright10_scalar(&row, 9), 160);
        // The same value in the low-bit byte of that group does not count.
        row[202] = 0;
        row[204] = 9;
        assert_eq!(first_bright10_scalar(&row, 9), 400);
        assert!(!contract_holds(&row, 9, 200));
    }
}
