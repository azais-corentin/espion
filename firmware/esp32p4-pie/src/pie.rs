//! The [`Pie`] token and the kernels (RISC-V only).

use core::arch::asm;
use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::Error;
use crate::check::{self, Buffer, Rng};
use crate::encode::{Ar, Q, andq, max_u8_a, vld_128_ip, vmax_u8, vmax_u8_ld_incp};
use crate::split;

/// One flag per HP core: set while a [`Pie`] token exists for that core.
static IN_USE: [AtomicBool; 2] = [const { AtomicBool::new(false) }; 2];

/// Proof that PIE is enabled on the calling core, and the handle the kernels hang off.
///
/// Neither `Send` nor `Sync`: it stays with the thread that enabled PIE, which must stay on
/// that core (see the crate docs for the full rule). Dropping it turns PIE off on the core
/// and frees the core's slot.
pub struct Pie {
    core: usize,
    _not_send_sync: PhantomData<*const ()>,
}

impl Pie {
    /// Enables PIE on the calling core: `mext_pie_status` (CSR 0x7F2) `STATE` = 1 (INITIAL).
    ///
    /// The hardware-loop CSR (0x7F1) is left alone. Refuses chips before revision 3.0
    /// (eFuse, read as esp-hal 1.2.2 reads it) and a second token on the same core.
    pub fn enable() -> Result<Self, Error> {
        let (major, minor) = chip_revision();
        if major < 3 {
            return Err(Error::Revision { major, minor });
        }
        let hart: u32;
        // SAFETY: reads the hart id; no memory access, no side effect.
        unsafe { asm!("csrr {0}, mhartid", out(reg) hart, options(nomem, nostack)) };
        let core = hart as usize;
        let flag = IN_USE.get(core).ok_or(Error::UnknownCore { core: hart })?;
        if flag.swap(true, Ordering::Acquire) {
            return Err(Error::InUse { core: hart });
        }
        // SAFETY: machine-mode write of this core's PIE status; no memory access. PIE has no
        // other user on this core (flag above), so no state is lost.
        unsafe { asm!("csrwi 0x7f2, 1", options(nomem, nostack)) };
        Ok(Self {
            core,
            _not_send_sync: PhantomData,
        })
    }

    /// Returns an offset `o <= bytes.len()` such that no byte of `bytes[..o]` is
    /// `>= threshold`, and either `o == bytes.len()` or a byte of
    /// `bytes[o..min(o + 64, len)]` is `>= threshold`.
    ///
    /// Any alignment and length: the bytes before the first 16-byte boundary and after the
    /// last one are scanned one by one (an exact offset if one of them is bright), the
    /// 16-byte-aligned middle by the vector kernel (the start of the 64-byte block, or of
    /// the last 16-byte step, that holds the bright byte). Never reads outside `bytes`.
    /// Overwrites `q0`-`q4`.
    pub fn first_bright(&self, bytes: &[u8], threshold: u8) -> usize {
        let head = ((bytes.as_ptr() as usize).wrapping_neg() & 15).min(bytes.len());
        let (head_bytes, rest) = bytes.split_at(head);
        if let Some(i) = head_bytes.iter().position(|&b| b >= threshold) {
            return i;
        }
        let (mid, tail) = rest.split_at(rest.len() & !15);
        // SAFETY: `mid` starts on a 16-byte boundary and its length is a multiple of 16;
        // `self` proves PIE is enabled on this core and has no other user.
        let o = unsafe { skip_dark(mid.as_ptr(), mid.len(), threshold) };
        if o < mid.len() {
            return head + o;
        }
        head + mid.len()
            + tail
                .iter()
                .position(|&b| b >= threshold)
                .unwrap_or(tail.len())
    }

    /// Runs `cases` randomised comparisons of [`first_bright`](Self::first_bright) with
    /// [`first_bright_scalar`](crate::first_bright_scalar) and returns how many failed.
    ///
    /// A case fails when the kernel breaks the contract (checked byte by byte) or disagrees
    /// with the reference on whether the row holds a bright byte. Rows are 0..=1344 bytes in
    /// an internal buffer, at every start alignment 0..16 in turn, with thresholds that
    /// include 1, 0x7F, 0x80, 0x81 and 0xFF, backgrounds at threshold - 1, bright bytes
    /// exactly at threshold and in the first and last position. Every byte around the row is
    /// 0xFF, so an over-read shows as a wrong result. The same `seed` gives the same cases.
    pub fn self_test(&self, seed: u32, cases: u32) -> u32 {
        // SAFETY: taken once; nothing below takes it again.
        let buf = unsafe { self.case_buffer() };
        let mut rng = Rng::new(seed);
        let mut failures = 0;
        for i in 0..cases {
            let case = check::generate(&mut rng, i, buf);
            let row = &buf.0[case.start..case.start + case.len];
            let o = self.first_bright(row, case.threshold);
            failures += u32::from(!check::agrees(row, case.threshold, o));
        }
        failures
    }

    /// RAW10 dark skip: returns an offset `o`, a multiple of 5 or `bytes.len()`, such that no
    /// pixel byte of `bytes[..o]` is `>= threshold_top8`, and either `o == bytes.len()` or a
    /// pixel byte of `bytes[o..min(o + 80, len)]` is.
    ///
    /// `bytes` is CSI-2 RAW10 data from a group boundary: bytes 0..=3 of each 5-byte group
    /// are the top 8 bits of four pixels, byte 4 holds their low bits and never counts. If
    /// the length is not a multiple of 5, the trailing partial group's bytes count as pixel
    /// bytes. `threshold_top8` compares with the top 8 bits (a 10-bit threshold `t` is
    /// exact as `t >> 2` only when `t` is a multiple of 4).
    ///
    /// Any alignment and length; never reads outside `bytes`. The first 0 to 15 groups
    /// (up to 75 bytes) are scanned one group at a time until the data reaches a 16-byte
    /// boundary; from there every 80-byte vector pass starts on a group boundary, so the
    /// low-bit byte positions are the same five constant masks in every pass, and the
    /// tail of fewer than 80 bytes is scanned one group at a time. A hit in the head or tail
    /// gives the offset of the group; a hit in the vector part gives the start of the
    /// 80-byte pass. Overwrites `q0`-`q7`.
    pub fn first_bright10(&self, bytes: &[u8], threshold_top8: u8) -> usize {
        split::first_bright10(bytes, threshold_top8, |mid, t| {
            // SAFETY: the split passes a 16-byte-aligned `mid` of whole 80-byte passes
            // starting on a group boundary; `self` proves PIE is enabled on this core and
            // has no other user.
            unsafe { skip_dark10(mid.as_ptr(), mid.len(), t) }
        })
    }

    /// Runs `cases` randomised comparisons of [`first_bright10`](Self::first_bright10) with
    /// [`first_bright10_scalar`](crate::first_bright10_scalar) and returns how many failed.
    ///
    /// A case fails when the kernel breaks the contract (checked byte by byte) or disagrees
    /// with the reference on whether the row holds a bright pixel. Rows are 0..=1680 bytes
    /// (whole groups) at every start alignment 0..16 in turn, with the thresholds of
    /// [`self_test`](Self::self_test), low-bit bytes mostly 0xFF (they must not count),
    /// backgrounds at threshold - 1, bright pixels exactly at threshold and in the first and
    /// last group. Every byte around the row is 0xFF. The same `seed` gives the same cases.
    pub fn self_test10(&self, seed: u32, cases: u32) -> u32 {
        // SAFETY: taken once; nothing below takes it again.
        let buf = unsafe { self.case_buffer() };
        let mut rng = Rng::new(seed);
        let mut failures = 0;
        for i in 0..cases {
            let case = check::raw10::generate(&mut rng, i, buf);
            let row = &buf.0[case.start..case.start + case.len];
            let o = self.first_bright10(row, case.threshold);
            failures += u32::from(!check::raw10::agrees(row, case.threshold, o));
        }
        failures
    }

    /// This core's self-test case buffer.
    ///
    /// # Safety
    ///
    /// The caller holds the reference only while it calls no other function that takes it
    /// (the self-tests call no user code and take it once each).
    #[allow(clippy::mut_from_ref)]
    unsafe fn case_buffer(&self) -> &mut Buffer {
        // SAFETY: only the token of this core touches `CASE_BUFFERS[self.core]`; the token is
        // unique per core and stays on its thread, and the caller keeps the reference
        // exclusive (see above).
        unsafe { &mut *CASE_BUFFERS[self.core].0.get() }
    }
}

impl Drop for Pie {
    fn drop(&mut self) {
        // SAFETY: machine-mode write of this core's PIE status (0 = OFF): a later PIE
        // instruction on this core faults instead of using stale state.
        unsafe { asm!("csrwi 0x7f2, 0", options(nomem, nostack)) };
        IN_USE[self.core].store(false, Ordering::Release);
    }
}

/// Self-test case buffers, one per core.
struct CaseBuffer(UnsafeCell<Buffer>);

// SAFETY: each buffer is only used through the token of its core (see `Pie::self_test`).
unsafe impl Sync for CaseBuffer {}

static CASE_BUFFERS: [CaseBuffer; 2] = [const { CaseBuffer(UnsafeCell::new(Buffer::new())) }; 2];

/// Chip revision `(major, minor)` from eFuse block 1 word 2 (`EFUSE_RD_MAC_SYS2`):
/// `WAFER_VERSION_MINOR` in bits 3:0, `WAFER_VERSION_MAJOR` in bits 5:4. These are the
/// fields esp-hal 1.2.2 reads (`efuse/esp32p4/fields.rs`, block 1 bits 64..68 and 68..70),
/// so the answer matches `esp_hal::efuse::chip_revision()`.
fn chip_revision() -> (u8, u8) {
    // SAFETY: a read of a read-only eFuse register; no other user can be disturbed.
    let word = unsafe { esp32p4::EFUSE::steal() }
        .rd_mac_sys2()
        .read()
        .bits();
    (((word >> 4) & 3) as u8, (word & 0xF) as u8)
}

/// Offset of the first 16-byte-aligned 64-byte block (or, in the last 16..48 bytes, of the
/// first 16-byte step) of `p[..len]` that holds a byte `>= threshold`; `len` if none.
///
/// The 64-byte loop is the spike's software-pipelined kernel (0.29 cycles/px in L1): the
/// first two loads of the next block are fused into this block's max steps. The last block
/// is finished without that read-ahead, so nothing past `p + len` is read.
///
/// Registers: `a0` the pointer (PIE address operand, post-incremented by the loads), `a3`
/// the block maximum (`esp.max.u8.a` result); both must be PIE-addressable, so they are
/// named. The loop limits and the threshold are only used by branches and take any
/// register. Vector registers: `q0`, `q1` (loads of the block's first half, preloaded),
/// `q2`, `q3` (second half), `q4` (running maximum).
///
/// `asm!` options: `nostack` (no stack use); `readonly` (reads `p[..len]`, writes no
/// memory). Not `pure`: the block also changes PIE state, which the compiler cannot see,
/// so calls must not be merged or removed on the strength of their inputs alone.
///
/// # Safety
///
/// `p` is 16-byte aligned, `p[..len]` is readable, `len` is a multiple of 16, and PIE is
/// enabled on this core with no other user.
#[inline(never)]
unsafe fn skip_dark(p: *const u8, len: usize, threshold: u8) -> usize {
    let start = p as usize;
    let end16 = start + len;
    let end64 = start + (len & !63);
    let mut a0 = start;
    // SAFETY: the caller's contract; every load lies in `[start, end16)` (see below).
    unsafe {
        asm!(
            // 64-byte blocks B0..B(n-1). Prologue: load B0's first half; a0 = B0 + 32.
            "bgeu a0, {end64}, 5f",
            ".insn 4, {ld_q0}",      // esp.vld.128.ip q0, a0, 16
            ".insn 4, {ld_q1}",      // esp.vld.128.ip q1, a0, 16
            // One block only: no next block to read ahead into.
            "bgeu a0, {last}, 3f",
            // Block Bi, reading ahead B(i+1)'s first half; runs for B0..B(n-2).
            "1:",
            ".insn 4, {mx_ld_q2}",   // esp.vmax.u8.ld.incp q2, a0, q4, q0, q1
            ".insn 4, {ld_q3}",      // esp.vld.128.ip q3, a0, 16
            ".insn 4, {mx_ld_q0}",   // esp.vmax.u8.ld.incp q0, a0, q4, q4, q2
            ".insn 4, {mx_ld_q1}",   // esp.vmax.u8.ld.incp q1, a0, q4, q4, q3
            ".insn 4, {red_q4}",     // esp.max.u8.a q4, a3
            "bgeu a3, {t}, 2f",      // a0 = B(i+1) + 32
            "bltu a0, {last}, 1b",
            // Last block B(n-1), its first half already in q0, q1; a0 = B(n-1) + 32.
            "3:",
            ".insn 4, {mx_ld_q2}",   // esp.vmax.u8.ld.incp q2, a0, q4, q0, q1
            ".insn 4, {ld_q3}",      // esp.vld.128.ip q3, a0, 16
            ".insn 4, {mx_q4_q2}",   // esp.vmax.u8 q4, q4, q2
            ".insn 4, {mx_q4_q3}",   // esp.vmax.u8 q4, q4, q3
            ".insn 4, {red_q4}",     // esp.max.u8.a q4, a3
            "bgeu a3, {t}, 4f",      // a0 = end64 = B(n-1) + 64
            // Up to three 16-byte steps from end64 to end16.
            "5:",
            "bgeu a0, {end16}, 6f",
            ".insn 4, {ld_q0}",      // esp.vld.128.ip q0, a0, 16
            ".insn 4, {red_q0}",     // esp.max.u8.a q0, a3
            "bltu a3, {t}, 5b",
            "addi a0, a0, -16",
            "j 6f",
            // Hits: move a0 back to the start of the block.
            "2:",
            "addi a0, a0, -32",
            "4:",
            "addi a0, a0, -64",
            "6:",
            ld_q0 = const vld_128_ip(Q::Q0, Ar::A0, 16),
            ld_q1 = const vld_128_ip(Q::Q1, Ar::A0, 16),
            ld_q3 = const vld_128_ip(Q::Q3, Ar::A0, 16),
            mx_ld_q2 = const vmax_u8_ld_incp(Q::Q2, Ar::A0, Q::Q4, Q::Q0, Q::Q1),
            mx_ld_q0 = const vmax_u8_ld_incp(Q::Q0, Ar::A0, Q::Q4, Q::Q4, Q::Q2),
            mx_ld_q1 = const vmax_u8_ld_incp(Q::Q1, Ar::A0, Q::Q4, Q::Q4, Q::Q3),
            mx_q4_q2 = const vmax_u8(Q::Q4, Q::Q4, Q::Q2),
            mx_q4_q3 = const vmax_u8(Q::Q4, Q::Q4, Q::Q3),
            red_q4 = const max_u8_a(Q::Q4, Ar::A3),
            red_q0 = const max_u8_a(Q::Q0, Ar::A3),
            end64 = in(reg) end64,
            // The read-ahead loop continues while a0 = B(i+1) + 32 is below B(n-1) + 32.
            last = in(reg) end64.wrapping_sub(32),
            end16 = in(reg) end16,
            t = in(reg) u32::from(threshold),
            inout("a0") a0,
            out("a3") _,
            options(nostack, readonly),
        );
    }
    a0 - start
}

/// 16-byte-aligned constant data for the vector loads.
#[repr(C, align(16))]
struct Aligned<const N: usize>([u8; N]);

/// The five mask vectors of one 80-byte RAW10 pass from a group boundary: 0xFF on pixel
/// bytes, 0 on the low-bit byte (offset 4 of each group). ANDing them clears the low-bit
/// bytes, which then never raise the maximum (0 is below any threshold but 0, and at
/// threshold 0 every pixel byte is bright anyway).
static MASKS10: Aligned<{ split::RAW10_BLOCK }> = {
    let mut m = [0xFF; split::RAW10_BLOCK];
    let mut i = 4;
    while i < m.len() {
        m[i] = 0;
        i += 5;
    }
    Aligned(m)
};

/// Offset of the first 80-byte pass of `p[..len]` that holds a pixel byte `>= threshold`;
/// `len` if none.
///
/// Per pass: five loads, five ANDs with the masks in `q3`-`q7` (loaded once per call), four
/// unsigned maxima into `q0`, one horizontal maximum. The loads alternate between `q1` and
/// `q2` so that no instruction uses a register loaded by the one just before it (a load's
/// result is ready two stages later). No read-ahead: nothing past `p + len` is read.
///
/// Registers: `a0` the pointer and `a1` the mask pointer (PIE address operands, post-
/// incremented by the loads), `a3` the pass maximum (`esp.max.u8.a` result); all must be
/// PIE-addressable, so they are named. The end and the threshold are only used by branches.
///
/// `asm!` options: `nostack`; `readonly` (reads `p[..len]` and `MASKS10`, writes no memory).
/// Not `pure`, for the reason given at [`skip_dark`].
///
/// # Safety
///
/// `p` is 16-byte aligned and starts a RAW10 group, `p[..len]` is readable, `len` is a
/// multiple of 80, and PIE is enabled on this core with no other user.
#[inline(never)]
unsafe fn skip_dark10(p: *const u8, len: usize, threshold: u8) -> usize {
    let start = p as usize;
    let end = start + len;
    let mut a0 = start;
    // SAFETY: the caller's contract; the loads read `MASKS10` and `[start, end)` only.
    unsafe {
        asm!(
            "bgeu a0, {end}, 3f",
            ".insn 4, {ld_m3}",      // esp.vld.128.ip q3, a1, 16
            ".insn 4, {ld_m4}",      // esp.vld.128.ip q4, a1, 16
            ".insn 4, {ld_m5}",      // esp.vld.128.ip q5, a1, 16
            ".insn 4, {ld_m6}",      // esp.vld.128.ip q6, a1, 16
            ".insn 4, {ld_m7}",      // esp.vld.128.ip q7, a1, 16
            "1:",
            ".insn 4, {ld_q0}",      // esp.vld.128.ip q0, a0, 16
            ".insn 4, {ld_q1}",      // esp.vld.128.ip q1, a0, 16
            ".insn 4, {and_q0}",     // esp.andq q0, q0, q3
            ".insn 4, {ld_q2}",      // esp.vld.128.ip q2, a0, 16
            ".insn 4, {and_q1_m4}",  // esp.andq q1, q1, q4
            ".insn 4, {mx_q1}",      // esp.vmax.u8 q0, q0, q1
            ".insn 4, {ld_q1}",      // esp.vld.128.ip q1, a0, 16
            ".insn 4, {and_q2_m5}",  // esp.andq q2, q2, q5
            ".insn 4, {mx_q2}",      // esp.vmax.u8 q0, q0, q2
            ".insn 4, {ld_q2}",      // esp.vld.128.ip q2, a0, 16
            ".insn 4, {and_q1_m6}",  // esp.andq q1, q1, q6
            ".insn 4, {mx_q1}",      // esp.vmax.u8 q0, q0, q1
            ".insn 4, {and_q2_m7}",  // esp.andq q2, q2, q7
            ".insn 4, {mx_q2}",      // esp.vmax.u8 q0, q0, q2
            ".insn 4, {red_q0}",     // esp.max.u8.a q0, a3
            "bgeu a3, {t}, 2f",
            "bltu a0, {end}, 1b",
            "j 3f",
            // Hit: a0 is 80 bytes past the start of the pass.
            "2:",
            "addi a0, a0, -80",
            "3:",
            ld_m3 = const vld_128_ip(Q::Q3, Ar::A1, 16),
            ld_m4 = const vld_128_ip(Q::Q4, Ar::A1, 16),
            ld_m5 = const vld_128_ip(Q::Q5, Ar::A1, 16),
            ld_m6 = const vld_128_ip(Q::Q6, Ar::A1, 16),
            ld_m7 = const vld_128_ip(Q::Q7, Ar::A1, 16),
            ld_q0 = const vld_128_ip(Q::Q0, Ar::A0, 16),
            ld_q1 = const vld_128_ip(Q::Q1, Ar::A0, 16),
            ld_q2 = const vld_128_ip(Q::Q2, Ar::A0, 16),
            and_q0 = const andq(Q::Q0, Q::Q0, Q::Q3),
            and_q1_m4 = const andq(Q::Q1, Q::Q1, Q::Q4),
            and_q2_m5 = const andq(Q::Q2, Q::Q2, Q::Q5),
            and_q1_m6 = const andq(Q::Q1, Q::Q1, Q::Q6),
            and_q2_m7 = const andq(Q::Q2, Q::Q2, Q::Q7),
            mx_q1 = const vmax_u8(Q::Q0, Q::Q0, Q::Q1),
            mx_q2 = const vmax_u8(Q::Q0, Q::Q0, Q::Q2),
            red_q0 = const max_u8_a(Q::Q0, Ar::A3),
            end = in(reg) end,
            t = in(reg) u32::from(threshold),
            inout("a0") a0,
            inout("a1") MASKS10.0.as_ptr() => _,
            out("a3") _,
            options(nostack, readonly),
        );
    }
    a0 - start
}
