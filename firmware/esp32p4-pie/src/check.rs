//! Randomised cases for [`first_bright`](crate::Pie::first_bright): the generator shared by
//! `Pie::self_test` on the chip and the host test, and the byte-wise contract. The case
//! buffer and the random numbers serve the other kernels' generators in the submodules.

pub(crate) mod copy;
pub(crate) mod raw10;

use crate::first_bright_scalar;

/// Longest generated slice (a 1344-byte row is the longest the firmware passes).
pub(crate) const MAX_LEN: usize = 1344;
/// Longest slice any generator places in a [`Buffer`] (`raw10::MAX_LEN`).
const SLICE_MAX: usize = 1680;
/// Bytes before the earliest slice start and after the latest slice end.
const GUARD: usize = 64;
/// Bytes in the case buffer: guard, up to 15 bytes of start offset, slice, guard.
pub(crate) const BUF_LEN: usize = GUARD + 16 + SLICE_MAX + GUARD;

/// Thresholds at the edges of the unsigned byte range and around the sign bit.
const THRESHOLDS: [u8; 5] = [1, 0x7F, 0x80, 0x81, 0xFF];

/// The case buffer. 16-byte aligned so that the start offset sets the slice alignment.
#[repr(C, align(16))]
pub(crate) struct Buffer(pub(crate) [u8; BUF_LEN]);

impl Buffer {
    pub(crate) const fn new() -> Self {
        Self([0; BUF_LEN])
    }
}

/// xorshift32: deterministic for a seed, so a failing case can be replayed.
pub(crate) struct Rng(u32);

impl Rng {
    pub(crate) const fn new(seed: u32) -> Self {
        // xorshift has a fixed point at 0.
        Self(if seed == 0 { 0x9E37_79B9 } else { seed })
    }

    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// A value in `0..n`; `n` > 0.
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// One generated case: the slice is `buf[start..start + len]`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Case {
    pub(crate) start: usize,
    pub(crate) len: usize,
    pub(crate) threshold: u8,
}

/// Fills `buf` for case number `index` and returns where the slice is.
///
/// Every byte outside the slice is 0xFF, which is at or above any threshold, so a kernel
/// that reads past either end reports a bright byte where the slice has none. The start
/// alignment cycles through 0..16 with `index`.
pub(crate) fn generate(rng: &mut Rng, index: u32, buf: &mut Buffer) -> Case {
    let start = GUARD + (index % 16) as usize;
    let len = length(rng);
    let threshold = threshold(rng);

    buf.0.fill(0xFF);
    let slice = &mut buf.0[start..start + len];
    let dark = |rng: &mut Rng| dark(rng, threshold);
    let bright = |rng: &mut Rng| bright(rng, threshold);
    let kind = rng.below(6);
    if kind == 1 || kind == 5 {
        // Threshold - 1 everywhere: the background a `>` in place of `>=` gets wrong.
        slice.fill(threshold.saturating_sub(1));
    } else {
        slice.iter_mut().for_each(|b| *b = dark(rng));
    }
    if len > 0 {
        let n = len as u32;
        match kind {
            2 => slice[rng.below(n) as usize] = threshold,
            3 => slice[0] = bright(rng),
            4 => slice[len - 1] = bright(rng),
            5 => {
                for _ in 0..=rng.below(4) {
                    let at = rng.below(n) as usize;
                    slice[at] = if rng.below(2) == 0 {
                        threshold
                    } else {
                        bright(rng)
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

/// A threshold: half the time one of [`THRESHOLDS`], else any byte (0 included).
fn threshold(rng: &mut Rng) -> u8 {
    if rng.below(2) == 0 {
        THRESHOLDS[rng.below(THRESHOLDS.len() as u32) as usize]
    } else {
        rng.below(256) as u8
    }
}

/// A byte below `threshold`; with threshold 0 there is none and any byte is returned.
fn dark(rng: &mut Rng, threshold: u8) -> u8 {
    match threshold {
        0 => rng.below(256) as u8,
        t => rng.below(u32::from(t)) as u8,
    }
}

/// A byte at or above `threshold`.
fn bright(rng: &mut Rng, threshold: u8) -> u8 {
    let t = u32::from(threshold);
    (t + rng.below(256 - t)) as u8
}

/// Row lengths: uniform, multiples of 64 and 16, and multiples of 64 plus or minus 1..15.
fn length(rng: &mut Rng) -> usize {
    let any = rng.below(MAX_LEN as u32 + 1) as usize;
    match rng.below(4) {
        0 => any,
        1 => any & !63,
        2 => any & !15,
        _ => {
            let near = any & !63;
            let off = 1 + rng.below(15) as usize;
            if rng.below(2) == 0 {
                near.saturating_sub(off)
            } else {
                (near + off).min(MAX_LEN)
            }
        }
    }
}

/// The contract of `first_bright`, byte by byte: no byte of `bytes[..o]` is at or above
/// `threshold`, and either `o` is the length or a byte of the 64 from `o` is.
pub(crate) fn contract_holds(bytes: &[u8], threshold: u8, o: usize) -> bool {
    let len = bytes.len();
    o <= len
        && bytes[..o].iter().all(|&b| b < threshold)
        && (o == len || bytes[o..len.min(o + 64)].iter().any(|&b| b >= threshold))
}

/// A kernel result `o` agrees with the reference: it keeps the contract and reports a
/// bright byte exactly when [`first_bright_scalar`] does.
#[cfg_attr(not(target_arch = "riscv32"), allow(dead_code))]
pub(crate) fn agrees(bytes: &[u8], threshold: u8, o: usize) -> bool {
    let len = bytes.len();
    contract_holds(bytes, threshold, o)
        && ((o < len) == (first_bright_scalar(bytes, threshold) < len))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASES: u32 = 50_000;

    #[test]
    fn scalar_reference_keeps_the_contract() {
        let mut buf = Buffer::new();
        let mut rng = Rng::new(0x1234_5678);
        let (mut found, mut empty, mut full) = (0, 0, 0);
        let mut aligned = [false; 16];
        let mut thresholds = [false; 256];
        for i in 0..CASES {
            let c = generate(&mut rng, i, &mut buf);
            let s = &buf.0[c.start..c.start + c.len];
            let o = first_bright_scalar(s, c.threshold);
            assert!(contract_holds(s, c.threshold, o), "case {i}: {c:?} -> {o}");
            assert!(agrees(s, c.threshold, o), "case {i}: {c:?} -> {o}");
            assert!(o == c.len || o.is_multiple_of(64), "case {i}: {c:?} -> {o}");
            // The guards stay 0xFF.
            assert!(buf.0[..c.start].iter().all(|&b| b == 0xFF));
            assert!(buf.0[c.start + c.len..].iter().all(|&b| b == 0xFF));
            assert!(BUF_LEN - (c.start + c.len) >= GUARD);
            found += usize::from(o < c.len);
            empty += usize::from(c.len == 0);
            full += usize::from(c.len == MAX_LEN);
            aligned[(buf.0.as_ptr() as usize + c.start) % 16] = true;
            thresholds[c.threshold as usize] = true;
        }
        // The generator covers hits and misses, both length extremes, every alignment and
        // the edge thresholds.
        assert!(
            found > CASES as usize / 3 && found < CASES as usize * 9 / 10,
            "{found}"
        );
        assert!(empty > 0 && full > 0);
        assert!(aligned.iter().all(|&a| a));
        assert!(THRESHOLDS.iter().all(|&t| thresholds[t as usize]));
        assert!(thresholds[0]);
    }

    #[test]
    fn contract_rejects_wrong_offsets() {
        let mut row = [0u8; 200];
        row[130] = 9;
        // Any offset from 67 to 130 is valid; beyond the bright byte or too far before it is not.
        assert!(contract_holds(&row, 9, 128));
        assert!(contract_holds(&row, 9, 67));
        assert!(contract_holds(&row, 9, 130));
        assert!(!contract_holds(&row, 9, 66));
        assert!(!contract_holds(&row, 9, 131));
        assert!(!contract_holds(&row, 9, 200));
        assert!(!contract_holds(&row, 10, 192));
        assert!(contract_holds(&row, 10, 200));
        assert!(!contract_holds(&row, 10, 201));
        // A miss reported for a row with a bright byte disagrees with the reference.
        assert!(!agrees(&row, 9, 200));
        assert!(agrees(&row, 9, 128));
    }
}
