//! How each kernel splits a slice into a scalar head, a vector middle on 16-byte boundaries
//! and a scalar tail.
//!
//! The splits take the vector core as a closure and do not depend on the target, so the
//! host tests run them with scalar stand-ins for the cores, at real addresses and every
//! alignment. On the chip the closures are the `asm!` kernels of `pie.rs`.

use crate::first_bright_group;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::raw10::{contract_holds, generate, scalar_blocks};
    use crate::check::{Buffer, Rng};

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
}
