//! `const fn` encoders for the PIE (Xespv 2.2) instructions the kernels use.
//!
//! rustc's LLVM does not know the PIE mnemonics, so a kernel emits each instruction as a
//! 32-bit word: `asm!(".insn 4, {w}", w = const encode::vmax_u8(Q::Q4, Q::Q4, Q::Q2))`. The
//! bit layouts are the `*_2P2` definitions in Espressif's LLVM fork
//! (`RISCVInstrInfoESPV.td`); the unit test compares every encoder with words produced by
//! Espressif's assembler. Revision 2.1 encodings (chips before rev 3.0) differ and are not
//! provided.
//!
//! The checks are `assert!`s. In a `const` operand or a `const` item they fail the build;
//! called at run time they panic. Use the encoders in const context only.
//!
//! Registers are typed so that an unusable register cannot be named: [`Q`] is one of the
//! eight vector registers, [`Ar`] one of the 14 scalar registers that both PIE and `asm!`
//! accept. PIE address and result operands can only be x8-x15 and x24-x31 (TRM 4.4.1.1.1);
//! `asm!` refuses x8 (`s0`, frame pointer) and x9 (`s1`, used internally by LLVM).
//!
//! An immediate out of range does not compile:
//!
//! ```compile_fail
//! use esp32p4_pie::encode::{Ar, Q, vld_128_ip};
//! const W: u32 = vld_128_ip(Q::Q0, Ar::A0, 8); // not a multiple of 16
//! ```
//!
//! ```compile_fail
//! use esp32p4_pie::encode::{Ar, Q, vst_128_ip};
//! const W: u32 = vst_128_ip(Q::Q0, Ar::A0, 2048); // above 2032
//! ```
//!
//! A fused load into the register the arithmetic writes raises a "PIE illegal" exception
//! (mcause 0x1F, TRM 2.7.2.3), so it does not compile either:
//!
//! ```compile_fail
//! use esp32p4_pie::encode::{Ar, Q, vmax_u8_ld_incp};
//! const W: u32 = vmax_u8_ld_incp(Q::Q4, Ar::A0, Q::Q4, Q::Q0, Q::Q1);
//! ```

/// A PIE vector register (128 bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
#[allow(missing_docs)]
pub enum Q {
    Q0,
    Q1,
    Q2,
    Q3,
    Q4,
    Q5,
    Q6,
    Q7,
}

/// A scalar register usable as a PIE operand from Rust `asm!`; the value is its x number.
///
/// The `asm!` block must bind the same register by name, e.g. `inout("a0") p`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
#[allow(missing_docs)]
pub enum Ar {
    A0 = 10,
    A1 = 11,
    A2 = 12,
    A3 = 13,
    A4 = 14,
    A5 = 15,
    S8 = 24,
    S9 = 25,
    S10 = 26,
    S11 = 27,
    T3 = 28,
    T4 = 29,
    T5 = 30,
    T6 = 31,
}

/// `rs1` field of the load/store forms: bits 17:15 = x[2:0], bit 18 = x[4].
const fn rs1_field(r: Ar) -> u32 {
    let x = r as u32;
    ((x & 7) << 15) | (((x >> 4) & 1) << 18)
}

/// `rd` field of the moves to a scalar register: bits 9:7 = x[2:0], bit 10 = x[4].
const fn rd_field(r: Ar) -> u32 {
    let x = r as u32;
    ((x & 7) << 7) | (((x >> 4) & 1) << 10)
}

/// Post-increment of `esp.vld.128.ip` / `esp.vst.128.ip`: imm / 16 as 8 bits, split into
/// bits 9:7 (low 3) and 30:26 (high 5).
const fn ip_offset(imm: i32) -> u32 {
    assert!(imm % 16 == 0, "PIE .ip offset must be a multiple of 16");
    assert!(
        imm >= -2048 && imm <= 2032,
        "PIE .ip offset must be in -2048..=2032"
    );
    let off = ((imm / 16) as u32) & 0xff;
    ((off & 7) << 7) | ((off >> 3) << 26)
}

/// Post-increment of the multiply-accumulate `.ld.ip` forms: imm / 16 as 4 bits, split into
/// bits 9:8 (low 2) and 20:19 (high 2).
const fn mac_ip_offset(imm: i32) -> u32 {
    assert!(imm % 16 == 0, "PIE MAC .ip offset must be a multiple of 16");
    assert!(
        imm >= -128 && imm <= 112,
        "PIE MAC .ip offset must be in -128..=112"
    );
    let off = ((imm / 16) as u32) & 0xf;
    ((off & 3) << 8) | ((off >> 2) << 19)
}

/// `esp.vld.128.ip qu, rs1, imm`: `qu` = 16 bytes at `rs1` (16-byte aligned), then
/// `rs1 += imm`.
pub const fn vld_128_ip(qu: Q, rs1: Ar, imm: i32) -> u32 {
    0x1F | ((qu as u32) << 10) | rs1_field(rs1) | (1 << 20) | (3 << 24) | ip_offset(imm)
}

/// `esp.vst.128.ip qu, rs1, imm`: 16 bytes at `rs1` (16-byte aligned) = `qu`, then
/// `rs1 += imm`.
pub const fn vst_128_ip(qu: Q, rs1: Ar, imm: i32) -> u32 {
    vld_128_ip(qu, rs1, imm) | (1 << 31)
}

/// `esp.vmax.u8 qz, qx, qy`: per-lane unsigned byte maximum.
pub const fn vmax_u8(qz: Q, qx: Q, qy: Q) -> u32 {
    0x1F | ((qz as u32) << 7)
        | (3 << 11)
        | (1 << 22)
        | (3 << 24)
        | ((qy as u32) << 26)
        | ((qx as u32) << 29)
}

/// `esp.vmax.u8.ld.incp qu, rs1, qz, qx, qy`: `qz = max(qx, qy)` per lane, and in the same
/// instruction `qu` = 16 bytes at `rs1`, then `rs1 += 16`. `qu` must differ from `qz`.
pub const fn vmax_u8_ld_incp(qu: Q, rs1: Ar, qz: Q, qx: Q, qy: Q) -> u32 {
    assert!(
        qu as u32 != qz as u32,
        "fused load into the result register raises a PIE illegal exception"
    );
    0x5F | ((qz as u32) << 7)
        | ((qu as u32) << 10)
        | rs1_field(rs1)
        | ((qy as u32) << 26)
        | ((qx as u32) << 29)
}

/// `esp.andq qz, qx, qy`: bitwise AND of two vectors.
pub const fn andq(qz: Q, qx: Q, qy: Q) -> u32 {
    0x0180_001F | ((qz as u32) << 7) | ((qy as u32) << 26) | ((qx as u32) << 29)
}

/// `esp.src.q qz, qw, qy`: `qz` = the 32-byte concatenation `{qy, qw}` shifted right by
/// `SAR_BYTE` bytes, i.e. 16 bytes starting `SAR_BYTE` bytes into `qw` and running on into
/// `qy`. With `qw` and `qy` two consecutive aligned words, that is the 16 bytes at the
/// misaligned address.
pub const fn src_q(qz: Q, qw: Q, qy: Q) -> u32 {
    let w = qw as u32;
    0x8000_401B | ((qz as u32) << 7) | ((w >> 2) << 19) | ((w & 3) << 24) | ((qy as u32) << 26)
}

/// `esp.movx.w.sar.bytes rs1`: `SAR_BYTE` = `rs1[3:0]`, the byte shift of `esp.src.q`.
pub const fn movx_w_sar_bytes(rs1: Ar) -> u32 {
    0x98C0_201B | rs1_field(rs1)
}

/// `esp.max.u8.a qw, rd`: `rd` = the largest of the 16 unsigned bytes of `qw`.
pub const fn max_u8_a(qw: Q, rd: Ar) -> u32 {
    let q = qw as u32;
    0x1B | rd_field(rd) | (1 << 13) | ((q >> 2) << 19) | ((q & 3) << 24)
}

/// `esp.zero.q qz`: `qz` = 0.
pub const fn zero_q(qz: Q) -> u32 {
    0x1B | ((qz as u32) << 7) | (1 << 12)
}

/// `esp.movx.r.cfg rd`: `rd` = the PIE `CFG` register.
pub const fn movx_r_cfg(rd: Ar) -> u32 {
    0x1B | rd_field(rd) | (1 << 13) | (1 << 23) | (1 << 31)
}

/// `esp.zero.xacc`: the 40-bit accumulator `XACC` = 0.
pub const fn zero_xacc() -> u32 {
    0x1B
}

/// `esp.vmulas.u16.xacc qx, qy`: `XACC` += the sum of the eight products of the unsigned
/// 16-bit lanes of `qx` and `qy`, saturated to 40 bits.
///
/// Bit 15 is the `sat` operand; Espressif's assembler sets it whether `sat` is written or
/// not and accepts no other value, so the encoder sets it too.
pub const fn vmulas_u16_xacc(qx: Q, qy: Q) -> u32 {
    0x02A2_A01B | ((qy as u32) << 26) | ((qx as u32) << 29)
}

/// `esp.vmulas.u16.xacc.ld.ip qu, rs1, imm, qx, qy`: as [`vmulas_u16_xacc`], and in the same
/// instruction `qu` = 16 bytes at `rs1` (16-byte aligned), then `rs1 += imm`
/// (-128..=112, a multiple of 16). `qu` may be `qx`: the product uses the old value.
///
/// Bit 21 is the `sat` operand, set as in [`vmulas_u16_xacc`].
pub const fn vmulas_u16_xacc_ld_ip(qu: Q, rs1: Ar, imm: i32, qx: Q, qy: Q) -> u32 {
    0x00A0_601F
        | ((qu as u32) << 10)
        | rs1_field(rs1)
        | mac_ip_offset(imm)
        | ((qy as u32) << 26)
        | ((qx as u32) << 29)
}

/// `esp.movx.r.xacc.l rd`: `rd` = bits 31:0 of `XACC`.
pub const fn movx_r_xacc_l(rd: Ar) -> u32 {
    0x84C0_201B | rd_field(rd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words from Espressif's assembler (crosstool-NG esp-15.3.0_20261030, binutils 2.45):
    /// `riscv32-esp-elf-as -march=rv32imafcb_zicsr_zifencei_xesploop_xespv -mno-relax`,
    /// read back with `riscv32-esp-elf-objdump-xespv2p2 -d -M max`.
    const TABLE: &[(u32, u32, &str)] = &[
        (
            vld_128_ip(Q::Q0, Ar::A0, 16),
            0x0311_009f,
            "esp.vld.128.ip q0, a0, 16",
        ),
        (
            vld_128_ip(Q::Q1, Ar::A0, 16),
            0x0311_049f,
            "esp.vld.128.ip q1, a0, 16",
        ),
        (
            vld_128_ip(Q::Q7, Ar::T6, -2048),
            0x4317_9c1f,
            "esp.vld.128.ip q7, t6, -2048",
        ),
        (
            vld_128_ip(Q::Q3, Ar::S8, 2032),
            0x3f14_0f9f,
            "esp.vld.128.ip q3, s8, 2032",
        ),
        (
            vld_128_ip(Q::Q5, Ar::A5, 0),
            0x0313_941f,
            "esp.vld.128.ip q5, a5, 0",
        ),
        (
            vld_128_ip(Q::Q2, Ar::T3, -16),
            0x7f16_0b9f,
            "esp.vld.128.ip q2, t3, -16",
        ),
        (
            vst_128_ip(Q::Q0, Ar::A0, 16),
            0x8311_009f,
            "esp.vst.128.ip q0, a0, 16",
        ),
        (
            vst_128_ip(Q::Q6, Ar::S11, -2048),
            0xc315_981f,
            "esp.vst.128.ip q6, s11, -2048",
        ),
        (
            vst_128_ip(Q::Q1, Ar::A3, 2032),
            0xbf12_879f,
            "esp.vst.128.ip q1, a3, 2032",
        ),
        (
            vst_128_ip(Q::Q4, Ar::T4, 112),
            0x8316_939f,
            "esp.vst.128.ip q4, t4, 112",
        ),
        (
            vmax_u8(Q::Q0, Q::Q0, Q::Q1),
            0x0740_181f,
            "esp.vmax.u8 q0, q0, q1",
        ),
        (
            vmax_u8(Q::Q4, Q::Q4, Q::Q2),
            0x8b40_1a1f,
            "esp.vmax.u8 q4, q4, q2",
        ),
        (
            vmax_u8(Q::Q7, Q::Q3, Q::Q5),
            0x7740_1b9f,
            "esp.vmax.u8 q7, q3, q5",
        ),
        (
            vmax_u8(Q::Q2, Q::Q6, Q::Q7),
            0xdf40_191f,
            "esp.vmax.u8 q2, q6, q7",
        ),
        (
            vmax_u8_ld_incp(Q::Q2, Ar::A0, Q::Q4, Q::Q0, Q::Q1),
            0x0401_0a5f,
            "esp.vmax.u8.ld.incp q2, a0, q4, q0, q1",
        ),
        (
            vmax_u8_ld_incp(Q::Q0, Ar::A0, Q::Q4, Q::Q4, Q::Q2),
            0x8801_025f,
            "esp.vmax.u8.ld.incp q0, a0, q4, q4, q2",
        ),
        (
            vmax_u8_ld_incp(Q::Q1, Ar::A0, Q::Q4, Q::Q4, Q::Q3),
            0x8c01_065f,
            "esp.vmax.u8.ld.incp q1, a0, q4, q4, q3",
        ),
        (
            vmax_u8_ld_incp(Q::Q7, Ar::T6, Q::Q0, Q::Q5, Q::Q6),
            0xb807_9c5f,
            "esp.vmax.u8.ld.incp q7, t6, q0, q5, q6",
        ),
        (
            vmax_u8_ld_incp(Q::Q3, Ar::S9, Q::Q6, Q::Q7, Q::Q1),
            0xe404_8f5f,
            "esp.vmax.u8.ld.incp q3, s9, q6, q7, q1",
        ),
        (
            andq(Q::Q0, Q::Q0, Q::Q3),
            0x0d80_001f,
            "esp.andq q0, q0, q3",
        ),
        (
            andq(Q::Q1, Q::Q1, Q::Q4),
            0x3180_009f,
            "esp.andq q1, q1, q4",
        ),
        (
            andq(Q::Q2, Q::Q2, Q::Q5),
            0x5580_011f,
            "esp.andq q2, q2, q5",
        ),
        (
            andq(Q::Q7, Q::Q6, Q::Q5),
            0xd580_039f,
            "esp.andq q7, q6, q5",
        ),
        (
            andq(Q::Q5, Q::Q7, Q::Q1),
            0xe580_029f,
            "esp.andq q5, q7, q1",
        ),
        (
            src_q(Q::Q0, Q::Q0, Q::Q1),
            0x8400_401b,
            "esp.src.q q0, q0, q1",
        ),
        (
            src_q(Q::Q1, Q::Q1, Q::Q0),
            0x8100_409b,
            "esp.src.q q1, q1, q0",
        ),
        (
            src_q(Q::Q2, Q::Q0, Q::Q1),
            0x8400_411b,
            "esp.src.q q2, q0, q1",
        ),
        (
            src_q(Q::Q7, Q::Q5, Q::Q6),
            0x9908_439b,
            "esp.src.q q7, q5, q6",
        ),
        (
            src_q(Q::Q3, Q::Q4, Q::Q2),
            0x8808_419b,
            "esp.src.q q3, q4, q2",
        ),
        (
            src_q(Q::Q6, Q::Q7, Q::Q3),
            0x8f08_431b,
            "esp.src.q q6, q7, q3",
        ),
        (
            movx_w_sar_bytes(Ar::A0),
            0x98c1_201b,
            "esp.movx.w.sar.bytes a0",
        ),
        (
            movx_w_sar_bytes(Ar::A3),
            0x98c2_a01b,
            "esp.movx.w.sar.bytes a3",
        ),
        (
            movx_w_sar_bytes(Ar::T6),
            0x98c7_a01b,
            "esp.movx.w.sar.bytes t6",
        ),
        (
            movx_w_sar_bytes(Ar::S8),
            0x98c4_201b,
            "esp.movx.w.sar.bytes s8",
        ),
        (
            movx_w_sar_bytes(Ar::A2),
            0x98c2_201b,
            "esp.movx.w.sar.bytes a2",
        ),
        (max_u8_a(Q::Q4, Ar::A3), 0x0008_229b, "esp.max.u8.a q4, a3"),
        (max_u8_a(Q::Q0, Ar::A0), 0x0000_211b, "esp.max.u8.a q0, a0"),
        (max_u8_a(Q::Q7, Ar::T6), 0x0308_279b, "esp.max.u8.a q7, t6"),
        (
            max_u8_a(Q::Q3, Ar::S10),
            0x0300_251b,
            "esp.max.u8.a q3, s10",
        ),
        (zero_q(Q::Q0), 0x0000_101b, "esp.zero.q q0"),
        (zero_q(Q::Q5), 0x0000_129b, "esp.zero.q q5"),
        (zero_q(Q::Q7), 0x0000_139b, "esp.zero.q q7"),
        (movx_r_cfg(Ar::A3), 0x8080_229b, "esp.movx.r.cfg a3"),
        (movx_r_cfg(Ar::A0), 0x8080_211b, "esp.movx.r.cfg a0"),
        (movx_r_cfg(Ar::T5), 0x8080_271b, "esp.movx.r.cfg t5"),
        (movx_r_cfg(Ar::S8), 0x8080_241b, "esp.movx.r.cfg s8"),
        (zero_xacc(), 0x0000_001b, "esp.zero.xacc"),
        (
            vmulas_u16_xacc(Q::Q0, Q::Q7),
            0x1ea2_a01b,
            "esp.vmulas.u16.xacc q0, q7",
        ),
        (
            vmulas_u16_xacc(Q::Q1, Q::Q7),
            0x3ea2_a01b,
            "esp.vmulas.u16.xacc q1, q7",
        ),
        (
            vmulas_u16_xacc(Q::Q5, Q::Q2),
            0xaaa2_a01b,
            "esp.vmulas.u16.xacc q5, q2",
        ),
        (
            vmulas_u16_xacc(Q::Q7, Q::Q0),
            0xe2a2_a01b,
            "esp.vmulas.u16.xacc q7, q0",
        ),
        (
            vmulas_u16_xacc_ld_ip(Q::Q0, Ar::A0, 16, Q::Q0, Q::Q7),
            0x1ca1_611f,
            "esp.vmulas.u16.xacc.ld.ip q0, a0, 16, q0, q7",
        ),
        (
            vmulas_u16_xacc_ld_ip(Q::Q1, Ar::A0, 16, Q::Q1, Q::Q7),
            0x3ca1_651f,
            "esp.vmulas.u16.xacc.ld.ip q1, a0, 16, q1, q7",
        ),
        (
            vmulas_u16_xacc_ld_ip(Q::Q3, Ar::T6, -128, Q::Q5, Q::Q2),
            0xa8b7_ec1f,
            "esp.vmulas.u16.xacc.ld.ip q3, t6, -128, q5, q2",
        ),
        (
            vmulas_u16_xacc_ld_ip(Q::Q6, Ar::S9, 112, Q::Q7, Q::Q1),
            0xe4ac_fb1f,
            "esp.vmulas.u16.xacc.ld.ip q6, s9, 112, q7, q1",
        ),
        (
            vmulas_u16_xacc_ld_ip(Q::Q2, Ar::A5, 0, Q::Q2, Q::Q3),
            0x4ca3_e81f,
            "esp.vmulas.u16.xacc.ld.ip q2, a5, 0, q2, q3",
        ),
        (
            vmulas_u16_xacc_ld_ip(Q::Q4, Ar::A2, -16, Q::Q0, Q::Q4),
            0x10ba_731f,
            "esp.vmulas.u16.xacc.ld.ip q4, a2, -16, q0, q4",
        ),
        (movx_r_xacc_l(Ar::A3), 0x84c0_229b, "esp.movx.r.xacc.l a3"),
        (movx_r_xacc_l(Ar::A0), 0x84c0_211b, "esp.movx.r.xacc.l a0"),
        (movx_r_xacc_l(Ar::T6), 0x84c0_279b, "esp.movx.r.xacc.l t6"),
        (movx_r_xacc_l(Ar::S8), 0x84c0_241b, "esp.movx.r.xacc.l s8"),
    ];

    #[test]
    fn encoders_match_espressif_assembler() {
        for &(got, want, text) in TABLE {
            assert_eq!(
                got, want,
                "{text}: encoder {got:#010x}, assembler {want:#010x}"
            );
        }
    }
}
