//! Marker pins: the calling core's dedicated-IO outputs (TRM 2.13.2), written through its
//! CSRs, for a logic analyser to time code against outside signals.
//!
//! - Each HP core has 8 output channels: bits 0 to 7 of its own `cpu_gpio_out` CSR (0x805),
//!   enabled in its `cpu_gpio_oen` CSR (0x803). The CSRs are per hart. Core 0's channels are
//!   the GPIO-matrix output signals 214 to 221 (esp-hal's `OutputSignal::CPU_GPIO_0..=7`),
//!   core 1's are 222 to 229 (`CPU_GPIO_8..=15`). A core drives only its own channels.
//! - Routing a channel to a pad is a GPIO-matrix register write, which core 0 can do for
//!   either core's channels (esp-hal's `connect_to`); the core that drives a channel must
//!   enable its output itself ([`enable`]), since the enable CSR is that core's.
//! - No lock and no memory access: usable from a bare core with interrupts off.

use core::arch::asm;

/// Enables the outputs of the calling core's channels in `mask` (bit n: channel n).
///
/// The enable is active low: esp-hal's driver (`gpio/dedicated/low_level/riscv_v2.rs`) and
/// ESP-IDF (`dedic_gpio_cpu_ll.h`) clear the bit, while the TRM (p.175) says 1 enables. The
/// marker self-check of `espion-core1` reads the pads back on the board.
#[inline(always)]
pub fn enable(mask: u32) {
    // SAFETY: clears bits of the calling core's dedicated-IO output enable; no memory access.
    unsafe { asm!("csrc 0x803, {0}", in(reg) mask, options(nomem, nostack)) };
}

/// Inverts the calling core's channels in `mask`: one read, one XOR, one write of
/// `cpu_gpio_out`.
///
/// The write covers all 8 channels, so a context that changes another channel of the same
/// core between the read and the write loses that change. Toggle every marker of a core from
/// one context (one thread, or core 1's loop), never also from an interrupt handler.
#[inline(always)]
pub fn toggle(mask: u32) {
    // SAFETY: reads and writes the calling core's dedicated-IO output levels; no memory access.
    unsafe {
        asm!(
            "csrr {t}, 0x805",
            "xor {t}, {t}, {m}",
            "csrw 0x805, {t}",
            t = out(reg) _,
            m = in(reg) mask,
            options(nomem, nostack),
        );
    }
}

/// The calling core's output levels (bit n: channel n).
#[inline(always)]
pub fn outputs() -> u32 {
    let v: u32;
    // SAFETY: reading the dedicated-IO output levels has no side effect.
    unsafe { asm!("csrr {0}, 0x805", out(reg) v, options(nomem, nostack)) };
    v
}
