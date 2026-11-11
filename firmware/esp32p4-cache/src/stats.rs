//! Counters of the cache lock, per HP core (feature `stats`): operations, the longest hold
//! (lock taken to lock released, the ROM call inside) and the longest wait (first attempt to
//! lock taken), in `mcycle` cycles of the core that ran the operation. Written by every
//! cache operation with relaxed atomics; no lock, so a bare second core can call it too.

use core::arch::asm;
use core::sync::atomic::{AtomicU32, Ordering};

/// One set per HP core, indexed by `mhartid`.
struct Core {
    ops: AtomicU32,
    hold_max: AtomicU32,
    wait_max: AtomicU32,
}

static CORES: [Core; 2] = [const {
    Core {
        ops: AtomicU32::new(0),
        hold_max: AtomicU32::new(0),
        wait_max: AtomicU32::new(0),
    }
}; 2];

/// What [`get`] returns for one core.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LockStats {
    /// Cache operations run (each a hold of the lock).
    pub ops: u32,
    /// Longest hold, in cycles of that core.
    pub hold_max: u32,
    /// Longest wait for the lock, in cycles of that core.
    pub wait_max: u32,
}

/// `mcycle` of the calling core.
#[inline(always)]
pub(crate) fn now() -> u32 {
    let c: u32;
    // SAFETY: reads the cycle counter CSR; no memory access, no side effect.
    unsafe { asm!("csrr {0}, mcycle", out(reg) c, options(nomem, nostack)) };
    c
}

/// Records one operation: first attempt at `tried`, lock taken at `taken`, released at `done`.
#[inline(always)]
pub(crate) fn record(tried: u32, taken: u32, done: u32) {
    let hart: usize;
    // SAFETY: reads the hart id; no memory access, no side effect.
    unsafe { asm!("csrr {0}, mhartid", out(reg) hart, options(nomem, nostack)) };
    let c = &CORES[hart & 1];
    c.ops.fetch_add(1, Ordering::Relaxed);
    c.hold_max
        .fetch_max(done.wrapping_sub(taken), Ordering::Relaxed);
    c.wait_max
        .fetch_max(taken.wrapping_sub(tried), Ordering::Relaxed);
}

/// The counters of core `core` (0 or 1; other values read core 1's).
pub fn get(core: usize) -> LockStats {
    let c = &CORES[core & 1];
    LockStats {
        ops: c.ops.load(Ordering::Relaxed),
        hold_max: c.hold_max.load(Ordering::Relaxed),
        wait_max: c.wait_max.load(Ordering::Relaxed),
    }
}

/// Zeroes the maxima of both cores (the operation counts keep running). An operation that
/// ends while this runs may still record its figures from before.
pub fn reset_max() {
    for c in &CORES {
        c.hold_max.store(0, Ordering::Relaxed);
        c.wait_max.store(0, Ordering::Relaxed);
    }
}
