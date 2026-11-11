//! Randomised cases for [`sum16_be`](crate::Pie::sum16_be).

use super::{Buffer, GUARD, Rng};
#[cfg(test)]
use crate::split::SUM_CHUNK;
use crate::sum16_be_scalar;

/// Longest generated slice: more than one Ethernet payload (1500 bytes).
pub(crate) const MAX_LEN: usize = 1600;
const _: () = assert!(GUARD + 16 + MAX_LEN + GUARD <= super::BUF_LEN);

/// Every byte outside the slice. Bytes of 1 add 1 or 256 each to a sum, so a kernel that
/// adds up to the 64 + 15 guard bytes on either side adds between 1 and 40 * 0x0101 =
/// 0x2828, less than 0xFFFF, which changes the folded sum: an over-read never cancels out.
const FILL: u8 = 0x01;

/// One generated case: the slice is `buf[start..start + len]`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SumCase {
    pub(crate) start: usize,
    pub(crate) len: usize,
}

/// Fills `buf` for case number `index` and returns where the slice is. The start alignment
/// cycles through 0..16 with `index`.
///
/// Contents: random bytes; all 0xFF (many end-around carries, sum 0xFFFF); all 0 (sum 0,
/// which any over-read makes nonzero); zeros with one random byte (a single byte at an
/// even or odd offset); 0xFF with a few random bytes.
pub(crate) fn generate(rng: &mut Rng, index: u32, buf: &mut Buffer) -> SumCase {
    let start = GUARD + (index % 16) as usize;
    let len = length(rng);
    buf.0.fill(FILL);
    let slice = &mut buf.0[start..start + len];
    match rng.below(5) {
        0 => slice.iter_mut().for_each(|b| *b = rng.next() as u8),
        1 => slice.fill(0xFF),
        2 => slice.fill(0),
        3 => {
            slice.fill(0);
            if len > 0 {
                slice[rng.below(len as u32) as usize] = 1 + rng.below(255) as u8;
            }
        }
        _ => {
            slice.fill(0xFF);
            if len > 0 {
                for _ in 0..=rng.below(4) {
                    slice[rng.below(len as u32) as usize] = rng.next() as u8;
                }
            }
        }
    }
    SumCase { start, len }
}

/// Lengths: uniform, multiples of 16, multiples of 16 plus or minus 1 or 2 (odd and even
/// tails), and short slices of up to 48 bytes.
fn length(rng: &mut Rng) -> usize {
    let any = rng.below(MAX_LEN as u32 + 1) as usize;
    match rng.below(4) {
        0 => any,
        1 => any & !15,
        2 => {
            let near = any & !15;
            let off = 1 + rng.below(2) as usize;
            if rng.below(2) == 0 {
                near.saturating_sub(off)
            } else {
                (near + off).min(MAX_LEN)
            }
        }
        _ => rng.below(49) as usize,
    }
}

/// A kernel result `s` equals the reference's.
#[cfg_attr(not(target_arch = "riscv32"), allow(dead_code))]
pub(crate) fn agrees(bytes: &[u8], s: u32) -> bool {
    s == sum16_be_scalar(bytes)
}

/// Scalar stand-in for the vector core of `sum16_be` in host tests: the plain sum of the
/// little-endian 16-bit lanes. Checks the preconditions the split promises the core
/// (16-byte-aligned start, whole vectors, at most [`SUM_CHUNK`] bytes) and that the sum
/// fits the 32 bits the core reads back, with the bound of `SUM_CHUNK`.
#[cfg(test)]
pub(crate) fn scalar_lanes(mid: &[u8]) -> u32 {
    assert!((mid.as_ptr() as usize).is_multiple_of(16) || mid.is_empty());
    assert!(mid.len().is_multiple_of(16) && mid.len() <= SUM_CHUNK);
    let sum: u64 = mid
        .chunks(2)
        .map(|w| u64::from(u16::from_le_bytes([w[0], w[1]])))
        .sum();
    assert!(sum < 1 << 31);
    sum as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The definition without folding tricks: add the words one at a time in one's
    /// complement (a carry out of bit 15 comes back in at bit 0).
    fn plain(bytes: &[u8]) -> u32 {
        let mut acc = 0u32;
        for w in bytes.chunks(2) {
            acc += u32::from(w[0]) << 8 | u32::from(w.get(1).copied().unwrap_or(0));
            if acc > 0xFFFF {
                acc -= 0xFFFF;
            }
        }
        acc
    }

    #[test]
    fn scalar_reference_against_plain_definition() {
        let mut buf = Buffer::new();
        let mut rng = Rng::new(0x2468_ace0);
        let (mut zero, mut ones, mut odd) = (0, 0, 0);
        let mut aligned = [false; 16];
        for i in 0..20_000 {
            let c = generate(&mut rng, i, &mut buf);
            let s = &buf.0[c.start..c.start + c.len];
            let r = sum16_be_scalar(s);
            assert_eq!(r, plain(s), "case {i}: {c:?}");
            assert!(agrees(s, r));
            assert!(buf.0[..c.start].iter().all(|&b| b == FILL));
            assert!(buf.0[c.start + c.len..].iter().all(|&b| b == FILL));
            assert!(buf.0.len() - (c.start + c.len) >= GUARD);
            zero += usize::from(r == 0);
            ones += usize::from(r == 0xFFFF);
            odd += usize::from(c.len % 2 == 1);
            aligned[(buf.0.as_ptr() as usize + c.start) % 16] = true;
            // Reading one guard byte past the end changes the result.
            let wider = &buf.0[c.start..c.start + c.len + 1];
            assert_ne!(sum16_be_scalar(wider), r, "case {i}: {c:?}");
        }
        assert!(zero > 1000 && ones > 1000 && odd > 1000);
        assert!(aligned.iter().all(|&a| a));
    }

    #[test]
    fn fixed_values() {
        assert_eq!(sum16_be_scalar(&[]), 0);
        assert_eq!(sum16_be_scalar(&[0x12]), 0x1200);
        assert_eq!(sum16_be_scalar(&[0x12, 0x34]), 0x1234);
        // RFC 1071 section 3 example: 0001 f203 f4f5 f6f7 -> 0xddf2.
        let rfc = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        assert_eq!(sum16_be_scalar(&rfc), 0xddf2);
        assert_eq!(plain(&rfc), 0xddf2);
        // All 0xFF over 64 KiB: 32768 * 0xFFFF = 0x7FFF_8000, folded 0xFFFF.
        assert_eq!(sum16_be_scalar(&[0xFF; 65536]), 0xFFFF);
        assert_eq!(plain(&[0xFF; 65536]), 0xFFFF);
    }
}
