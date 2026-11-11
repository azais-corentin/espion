//! PIE (Xespv 2.2), the 128-bit SIMD extension of the ESP32-P4 HP cores, from stable Rust.
//!
//! rustc's LLVM does not know the PIE instructions, so the kernels emit them as 32-bit words
//! built by the `const fn` encoders in [`encode`] (`asm!(".insn 4, {w}", w = const ...)`).
//! Only the revision 2.2 encodings of chips at revision 3.0 or later are provided; older chips
//! implement PIE 2.1 with different encodings and are refused by [`Pie::enable`].
//!
//! # Rule for using PIE
//!
//! PIE state (eight 128-bit `q` registers and the accumulators) is per core, and nothing in
//! the Rust stack saves it: neither the trap entry of riscv-rt / esp-riscv-rt nor the thread
//! switch of esp-rtos 0.4.0. So:
//!
//! - one PIE-using thread per core, pinned to that core ([`Pie`] is neither `Send` nor
//!   `Sync`, and [`Pie::enable`] refuses a second token on the same core);
//! - never in an interrupt handler or an executor that runs at interrupt priority;
//! - kernels keep no vector state between calls: every kernel loads what it reads, and the
//!   `q` registers are caller-clobbered by convention of this crate. `asm!` cannot name the
//!   `q` registers as clobbers, which is harmless because rustc never allocates them.
//!
//! # Kernels
//!
//! - [`Pie::first_bright`]: finds the first 64-byte block of a row that holds a byte at or
//!   above a threshold (the detector's dark skip). [`first_bright_scalar`] is the reference
//!   with the same contract; it builds on any target.
//! - [`Pie::first_bright10`]: the same over CSI-2 RAW10 data (four pixels in five bytes),
//!   80 bytes (64 pixels) per step, ignoring the low-bit byte of each group.
//!   [`first_bright10_scalar`] is the reference.
//! - [`Pie::copy`]: `dst.copy_from_slice(src)` at any alignment of both, 16 bytes per
//!   aligned load and store.
//! - [`Pie::sum16_be`]: the Internet checksum's one's-complement sum of big-endian 16-bit
//!   words, through the 40-bit multiply-accumulator. [`sum16_be_scalar`] is the reference.
//! - [`Pie::self_test`], [`Pie::self_test10`], [`Pie::self_test_copy`] and
//!   [`Pie::self_test_sum`] compare the kernels with the references (`copy_from_slice` for
//!   the copy) on generated cases, on the chip.
//!
//! Everything that executes PIE or CSR instructions exists for `target_arch = "riscv32"`
//! only; the encoders and the scalar reference build and test on the host.

#![no_std]

#[cfg(any(target_arch = "riscv32", test))]
mod check;
pub mod encode;
#[cfg(target_arch = "riscv32")]
mod pie;
#[cfg(any(target_arch = "riscv32", test))]
mod split;

use core::fmt;

#[cfg(target_arch = "riscv32")]
pub use pie::Pie;

/// Why [`Pie::enable`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The chip is older than revision 3.0: its PIE uses the 2.1 encodings.
    Revision {
        /// Major chip revision from eFuse.
        major: u8,
        /// Minor chip revision from eFuse.
        minor: u8,
    },
    /// A [`Pie`] token already exists for this core.
    InUse {
        /// The core (`mhartid`).
        core: u32,
    },
    /// `mhartid` is not one of the two HP cores.
    UnknownCore {
        /// The value read from `mhartid`.
        core: u32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Revision { major, minor } => {
                write!(
                    f,
                    "chip revision {major}.{minor}: PIE kernels need 3.0 or later"
                )
            }
            Self::InUse { core } => write!(f, "PIE already in use on core {core}"),
            Self::UnknownCore { core } => write!(f, "core {core} is not an HP core"),
        }
    }
}

impl core::error::Error for Error {}

/// Scalar reference of [`Pie::first_bright`] at 64-byte granularity from the start of
/// `bytes`.
///
/// Returns the offset of the first 64-byte chunk of `bytes` (the last one may be shorter)
/// that holds a byte `>= threshold`, or `bytes.len()` if there is none. That keeps the
/// contract of [`Pie::first_bright`]: no byte before the offset is `>= threshold`, and
/// either the offset is the length or one of the 64 bytes from it is.
pub fn first_bright_scalar(bytes: &[u8], threshold: u8) -> usize {
    bytes
        .chunks(64)
        .position(|c| c.iter().any(|&b| b >= threshold))
        .map_or(bytes.len(), |i| i * 64)
}

/// Scalar reference of [`Pie::first_bright10`] at 80-byte granularity from the start of
/// `bytes`.
///
/// `bytes` is CSI-2 RAW10 data from a group boundary: in each 5-byte group, bytes 0..=3 are
/// the top 8 bits of four pixels and byte 4 holds their low bits, which is not a pixel and
/// never counts. Returns the offset of the first 80-byte chunk (16 groups, 64 pixels; the
/// last chunk may be shorter) that holds a pixel byte `>= threshold_top8`, or `bytes.len()`
/// if there is none. The offset is a multiple of 5, so it starts a group. If the length is
/// not a multiple of 5, the bytes of the trailing partial group count as pixel bytes.
pub fn first_bright10_scalar(bytes: &[u8], threshold_top8: u8) -> usize {
    bytes
        .chunks(80)
        .position(|c| first_bright_group(c, threshold_top8).is_some())
        .map_or(bytes.len(), |i| i * 80)
}

/// Offset of the first 5-byte RAW10 group of `bytes` with a pixel byte (bytes 0..=3 of the
/// group) `>= threshold_top8`. A trailing partial group has only pixel bytes.
fn first_bright_group(bytes: &[u8], threshold_top8: u8) -> Option<usize> {
    bytes
        .chunks(5)
        .position(|g| g.iter().take(4).any(|&b| b >= threshold_top8))
        .map(|i| i * 5)
}

/// Scalar reference of [`Pie::sum16_be`]: the one's-complement sum of `bytes` taken as
/// big-endian 16-bit words (an odd last byte is the high byte of a last word with low byte
/// 0), as the Internet checksum (RFC 1071) adds them, before the final complement.
///
/// The result is fully folded: it is in `0..=0xFFFF`, congruent to the plain sum of the
/// words modulo 0xFFFF, and 0 only when every word is 0 (a nonzero multiple of 0xFFFF
/// gives 0xFFFF). Any length.
///
/// To finish a checksum, add the other partial sums (pseudo-header, other fragments; each
/// at most 0xFFFF, or a plain sum of words) as `u32`, fold with
/// `while s > 0xFFFF { s = (s & 0xFFFF) + (s >> 16) }`, and take `!s as u16`. A part that
/// starts at an odd byte offset of the checksummed data must be byte-swapped
/// (`(s as u16).swap_bytes()`) before it is added.
pub fn sum16_be_scalar(bytes: &[u8]) -> u32 {
    fold16(
        bytes
            .chunks(2)
            .map(|w| u64::from(w[0]) << 8 | u64::from(w.get(1).copied().unwrap_or(0)))
            .sum(),
    )
}

/// Folds a sum of 16-bit words with end-around carries into `0..=0xFFFF`. Each step keeps
/// the value modulo 0xFFFF (65536 = 1 mod 0xFFFF) and keeps a nonzero value nonzero.
fn fold16(mut sum: u64) -> u32 {
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    sum as u32
}
