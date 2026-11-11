//! Randomised cases for [`copy`](crate::Pie::copy): a source and a destination buffer, and
//! the byte-for-byte check.

use super::{Buffer, GUARD, Rng};

/// Longest generated copy: more than one Ethernet payload (1500 bytes).
pub(crate) const MAX_LEN: usize = 1600;
const _: () = assert!(GUARD + 16 + MAX_LEN + GUARD <= super::BUF_LEN);

/// Every source byte outside the slice.
const SRC_FILL: u8 = 0xA5;
/// Every destination byte before the copy; outside the slice it must stay.
const DST_FILL: u8 = 0x5A;

/// One generated copy: `src.0[src..src + len]` to `dst.0[dst..dst + len]`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CopyCase {
    pub(crate) src: usize,
    pub(crate) dst: usize,
    pub(crate) len: usize,
}

/// Fills both buffers for case number `index` and returns where the slices are.
///
/// The source slice is random bytes, the rest of the source [`SRC_FILL`], the whole
/// destination [`DST_FILL`]. Case `index` takes source alignment `index % 16` and
/// destination alignment `index / 16 % 16`, so 256 consecutive cases cover every pair.
pub(crate) fn generate(rng: &mut Rng, index: u32, src: &mut Buffer, dst: &mut Buffer) -> CopyCase {
    let pair = (index % 256) as usize;
    let case = CopyCase {
        src: GUARD + pair % 16,
        dst: GUARD + pair / 16,
        len: length(rng),
    };
    src.0.fill(SRC_FILL);
    dst.0.fill(DST_FILL);
    src.0[case.src..case.src + case.len]
        .iter_mut()
        .for_each(|b| *b = rng.next() as u8);
    case
}

/// Lengths: uniform, multiples of 16, multiples of 64 plus or minus 1..15, and short
/// copies of up to 48 bytes (head and tail only, or one or two vectors).
fn length(rng: &mut Rng) -> usize {
    let any = rng.below(MAX_LEN as u32 + 1) as usize;
    match rng.below(4) {
        0 => any,
        1 => any & !15,
        2 => {
            let near = any & !63;
            let off = 1 + rng.below(15) as usize;
            if rng.below(2) == 0 {
                near.saturating_sub(off)
            } else {
                (near + off).min(MAX_LEN)
            }
        }
        _ => rng.below(49) as usize,
    }
}

/// The destination slice equals the source slice and every other destination byte is
/// still [`DST_FILL`].
pub(crate) fn copied(src: &Buffer, dst: &Buffer, c: &CopyCase) -> bool {
    let end = c.dst + c.len;
    dst.0[c.dst..end] == src.0[c.src..c.src + c.len]
        && dst.0[..c.dst].iter().all(|&b| b == DST_FILL)
        && dst.0[end..].iter().all(|&b| b == DST_FILL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_against_copy_from_slice() {
        let [mut src, mut dst] = [Buffer::new(), Buffer::new()];
        let mut rng = Rng::new(0x0123_4567);
        let mut pairs = [[false; 16]; 16];
        let (mut empty, mut full) = (0, 0);
        for i in 0..20_000 {
            let c = generate(&mut rng, i, &mut src, &mut dst);
            assert!(c.len <= MAX_LEN && dst.0.len() - (c.dst + c.len) >= GUARD);
            // Untouched destination: only an empty copy passes.
            assert_eq!(
                copied(&src, &dst, &c),
                c.len == 0 || src.0[c.src..c.src + c.len].iter().all(|&b| b == DST_FILL)
            );
            dst.0[c.dst..c.dst + c.len].copy_from_slice(&src.0[c.src..c.src + c.len]);
            assert!(copied(&src, &dst, &c), "case {i}: {c:?}");
            // One byte too many on either side is caught.
            if c.len > 0 {
                let mut over = dst.0;
                over[c.dst + c.len] = src.0[c.src + c.len];
                assert!(!copied(&src, &Buffer(over), &c));
                let mut under = dst.0;
                under[c.dst - 1] = src.0[c.src - 1];
                assert!(!copied(&src, &Buffer(under), &c));
            }
            let base = src.0.as_ptr() as usize;
            pairs[(base + c.src) % 16][(dst.0.as_ptr() as usize + c.dst) % 16] = true;
            empty += usize::from(c.len == 0);
            full += usize::from(c.len == MAX_LEN);
        }
        assert!(pairs.iter().flatten().all(|&p| p));
        assert!(empty > 0 && full > 0);
    }
}
