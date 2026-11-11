//! Performance and cache counters of the ESP32-P4 HP cores.
//!
//! Each HP core has `mcycle`, `minstret` and three event counters. [`init`] points the event
//! counters at conditional-branch mispredictions (`mhpmcounter8`), conditional branches
//! (`mhpmcounter9`) and stores (`mhpmcounter13`). The cache block at `0x3FF1_0000` adds hit
//! and miss counters for the L1 instruction and data caches of each core and for the shared
//! L2 cache.
//!
//! Call [`init`] once on every core that measures. Then take a [`Sample`] at the start of a
//! window and [`Sample::end`] (or [`Sample::elapsed`]) at its end; the [`Delta`] prints as
//! one `key=value` line. Inside hot loops use [`CpuSample`], which reads only the five CSR
//! counters, or [`cycles`].
//!
//! What the numbers mean (measured on the board, rev v3.2 at 400 MHz):
//! - Every counter is per core and includes interrupt time on that core: nothing saves or
//!   pauses them on an interrupt or a context switch (esp-rtos does not).
//! - The L1 hit counters count accesses: one per load or store that hits, whatever its width
//!   (a 128-bit PIE load counts once).
//! - The miss counters do not count cache lines. They count miss events, including retries
//!   while a line fill is pending: about 10 L1 data misses per 64-byte line read cold from
//!   internal RAM, about 148 per line from PSRAM (and about 76 L2 data misses per line).
//!   Compare them between runs of the same code; do not divide bytes by them.
//! - The L2 counters are shared by both cores; [`Sample`] sums bus 0 and bus 1.
//! - All counters are 32 bits wide. `mcycle` wraps after 10.7 s at 400 MHz, so a window
//!   must be shorter than that; [`cycles64`] reads the full 64-bit cycle count.
//!
//! [`marker`] toggles the calling core's dedicated-IO outputs through their CSRs: marker pins
//! for a logic analyser, three instructions and no memory access per marker.
//!
//! The crate takes no locks and has no dependencies, so a bare second core can use it. It
//! must run in machine mode, which is how esp-hal firmware runs.

#![no_std]
#![warn(missing_docs)]

use core::arch::asm;
use core::fmt;
use core::ptr::{read_volatile, write_volatile};

pub mod marker;

/// Base address of the cache controller (`CACHE` in the `esp32p4` PAC 0.3.0).
const CACHE: usize = 0x3FF1_0000;

/// `L1_CACHE_ACS_CNT_CTRL`: enable bits ibus0 (0), ibus1 (1), dbus0 (4), dbus1 (5) and
/// clear bits 16, 17, 20, 21 at the same positions plus 16.
const L1_CTRL: usize = 0x17C;
/// `L2_CACHE_ACS_CNT_CTRL`: enable bits ibus0 (8), ibus1 (9), dbus0 (12), dbus1 (13) and
/// clear bits 24, 25, 28, 29.
const L2_CTRL: usize = 0x2FC;

/// Enable bits of the L1 units of both cores.
const L1_ENABLE: u32 = (1 << 0) | (1 << 1) | (1 << 4) | (1 << 5);
/// Enable bits of the L2 instruction and data units, bus 0 and bus 1.
const L2_ENABLE: u32 = (1 << 8) | (1 << 9) | (1 << 12) | (1 << 13);

/// Hit counter of `L1_IBUS0` (core 0 instruction fetch); its miss counter follows at +4.
const L1I_CORE0: usize = 0x180;
/// Hit counter of `L1_IBUS1` (core 1 instruction fetch); miss at +4.
const L1I_CORE1: usize = 0x190;
/// Hit counter of `L1_DBUS0` (core 0 data); miss at +4.
const L1D_CORE0: usize = 0x1C0;
/// Hit counter of `L1_DBUS1` (core 1 data); miss at +4.
const L1D_CORE1: usize = 0x1D4;
/// Hit counters of `L2_IBUS0` and `L2_IBUS1`; miss at +4 each.
const L2I: [usize; 2] = [0x300, 0x310];
/// Hit counters of `L2_DBUS0` and `L2_DBUS1`; miss at +4 each.
const L2D: [usize; 2] = [0x340, 0x354];

/// `mcountinhibit` bits CY (0), IR (2), HPM8 (8), HPM9 (9) and HPM13 (13).
const INHIBIT_BITS: u32 = (1 << 0) | (1 << 2) | (1 << 8) | (1 << 9) | (1 << 13);

#[inline(always)]
fn cache_read(offset: usize) -> u32 {
    // SAFETY: an aligned 32-bit register of the cache controller; reading has no side effect.
    unsafe { read_volatile((CACHE + offset) as *const u32) }
}

#[inline(always)]
fn cache_write(offset: usize, value: u32) {
    // SAFETY: an aligned 32-bit control register of the cache controller. The values written
    // only touch the access counter enable and clear bits.
    unsafe { write_volatile((CACHE + offset) as *mut u32, value) }
}

/// Index of the calling core: 0 or 1.
///
/// Not marked `pure`, so the compiler keeps the read on its side of the counter reads and
/// it never falls inside a window.
#[inline(always)]
fn hart() -> u32 {
    let id: u32;
    // SAFETY: reading `mhartid` has no side effect.
    unsafe { asm!("csrr {0}, mhartid", out(reg) id, options(nomem, nostack)) };
    id
}

/// Sets up the counters of the calling core.
///
/// - Writes the event selectors: `mhpmevent8` (CSR 0x328) = 6, conditional-branch
///   mispredictions; `mhpmevent9` (0x329) = 7, conditional branches; `mhpmevent13` (0x32D)
///   = 0xB, stores. They are 0 at boot, and the event counters count nothing until written.
/// - Clears the `mcountinhibit` (0x320) bits of these counters, `mcycle` and `minstret`.
///   They are 0 at boot already; this undoes anything that set them.
/// - Enables the L1 cache counters of both cores and the L2 counters, then clears the L1
///   counters of the calling core. The enable register is shared by the cores, so every
///   call writes all enable bits instead of a read-modify-write that could race with the
///   other core. The shared L2 counters are not cleared, so a call on one core does not
///   break a window open on the other.
///
/// Safe to call again: the second call writes the same values and clears the calling
/// core's L1 counters once more, which spoils the cache counts of a window open on this core.
pub fn init() {
    // SAFETY: machine-mode writes to the event selectors and to the inhibit register; they
    // only change what the performance counters count.
    unsafe {
        asm!(
            "csrwi 0x328, 6",
            "csrwi 0x329, 7",
            "csrwi 0x32D, 11",
            "csrc 0x320, {bits}",
            bits = in(reg) INHIBIT_BITS,
            options(nomem, nostack),
        );
    }
    let clear = if hart() == 0 {
        (1 << 16) | (1 << 20)
    } else {
        (1 << 17) | (1 << 21)
    };
    // The clear bits are write-to-trigger. The second write leaves only the enables set,
    // the sequence the measurements ran with.
    cache_write(L1_CTRL, L1_ENABLE | clear);
    cache_write(L1_CTRL, L1_ENABLE);
    cache_write(L2_CTRL, L2_ENABLE);
}

/// Low 32 bits of `mcycle`: one `csrr`. Wraps after 10.7 s at 400 MHz.
///
/// The read is not marked as memory-free, so the compiler keeps the memory accesses of the
/// code being timed on their side of it.
#[inline(always)]
pub fn cycles() -> u32 {
    let c: u32;
    // SAFETY: reading `mcycle` has no side effect.
    unsafe { asm!("csrr {0}, mcycle", out(reg) c, options(nostack)) };
    c
}

/// Full 64-bit `mcycle`. RV32 has no atomic 64-bit read, so this reads the high half, the
/// low half and the high half again, and retries when the high half changed between them.
#[inline]
pub fn cycles64() -> u64 {
    loop {
        let (hi, lo, hi2): (u32, u32, u32);
        // SAFETY: reading `mcycleh` and `mcycle` has no side effect.
        unsafe {
            asm!(
                "csrr {0}, mcycleh",
                "csrr {1}, mcycle",
                "csrr {2}, mcycleh",
                out(reg) hi,
                out(reg) lo,
                out(reg) hi2,
                options(nostack),
            );
        }
        if hi == hi2 {
            return (u64::from(hi) << 32) | u64::from(lo);
        }
    }
}

/// The five CSR counters of the calling core at one instant. Cheap enough for hot loops:
/// five `csrr` in one block, nothing else.
///
/// All five are per core and include interrupt time on that core. The branch counters count
/// conditional branches only (not jumps or calls).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuSample {
    /// `mcycle`, low 32 bits: core clock cycles.
    pub cycles: u32,
    /// `minstret`, low 32 bits: retired instructions.
    pub instructions: u32,
    /// `mhpmcounter9`: retired conditional branches.
    pub branches: u32,
    /// `mhpmcounter8`: conditional-branch mispredictions.
    pub mispredicts: u32,
    /// `mhpmcounter13`: retired store instructions.
    pub stores: u32,
}

impl CpuSample {
    /// Reads the counters. Needs [`init`] on this core first, or the branch, misprediction
    /// and store counts stay 0.
    ///
    /// The five reads sit in one `asm!` block, so nothing is scheduled between them. The
    /// block is not marked as memory-free, so the compiler keeps memory accesses of the
    /// measured code on their side of it.
    #[inline(always)]
    pub fn now() -> Self {
        let (cycles, instructions, branches, mispredicts, stores): (u32, u32, u32, u32, u32);
        // SAFETY: reading performance counter CSRs has no side effect.
        unsafe {
            asm!(
                "csrr {0}, mcycle",
                "csrr {1}, minstret",
                "csrr {2}, mhpmcounter9",
                "csrr {3}, mhpmcounter8",
                "csrr {4}, mhpmcounter13",
                out(reg) cycles,
                out(reg) instructions,
                out(reg) branches,
                out(reg) mispredicts,
                out(reg) stores,
                options(nostack),
            );
        }
        Self {
            cycles,
            instructions,
            branches,
            mispredicts,
            stores,
        }
    }

    /// Counts from `earlier` to `self`, each a wrapping 32-bit difference.
    #[inline]
    pub fn since(&self, earlier: &CpuSample) -> CpuDelta {
        CpuDelta {
            cycles: self.cycles.wrapping_sub(earlier.cycles),
            instructions: self.instructions.wrapping_sub(earlier.instructions),
            branches: self.branches.wrapping_sub(earlier.branches),
            mispredicts: self.mispredicts.wrapping_sub(earlier.mispredicts),
            stores: self.stores.wrapping_sub(earlier.stores),
        }
    }
}

/// CSR counter differences over one window; see [`CpuSample`] for each field.
///
/// The window includes the reads themselves: the five `csrr` of the closing sample are
/// counted as instructions. Measure an empty window to get the fixed overhead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuDelta {
    /// Core clock cycles.
    pub cycles: u32,
    /// Retired instructions.
    pub instructions: u32,
    /// Retired conditional branches.
    pub branches: u32,
    /// Conditional-branch mispredictions.
    pub mispredicts: u32,
    /// Retired store instructions.
    pub stores: u32,
}

impl CpuDelta {
    /// Instructions per 1000 cycles (IPC times 1000), 0 for an empty window. Saturates at
    /// `u32::MAX`.
    #[inline]
    pub fn ipc_milli(&self) -> u32 {
        if self.cycles == 0 {
            return 0;
        }
        let ipc = u64::from(self.instructions) * 1000 / u64::from(self.cycles);
        u32::try_from(ipc).unwrap_or(u32::MAX)
    }
}

impl fmt::Display for CpuDelta {
    /// One line: `cyc=.. ins=.. ipc=0.531 br=.. mis=.. st=..`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ipc = self.ipc_milli();
        write!(
            f,
            "cyc={} ins={} ipc={}.{:03} br={} mis={} st={}",
            self.cycles,
            self.instructions,
            ipc / 1000,
            ipc % 1000,
            self.branches,
            self.mispredicts,
            self.stores,
        )
    }
}

/// Cache counter values (in a [`Sample`]) or differences (in a [`Delta`]).
///
/// The L1 fields belong to the calling core. The L2 fields are shared by both cores and
/// sum bus 0 and bus 1. Hits count accesses; misses count miss events including retries
/// while a fill is pending (measured: about 10 L1 data misses per 64-byte line from
/// internal RAM, about 148 from PSRAM), not lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheCounts {
    /// L1 instruction cache hits of the calling core.
    pub l1i_hit: u32,
    /// L1 instruction cache miss events of the calling core.
    pub l1i_miss: u32,
    /// L1 data cache hits of the calling core: loads and stores, whatever their width.
    pub l1d_hit: u32,
    /// L1 data cache miss events of the calling core.
    pub l1d_miss: u32,
    /// L2 instruction hits, both cores.
    pub l2i_hit: u32,
    /// L2 instruction miss events, both cores.
    pub l2i_miss: u32,
    /// L2 data hits, both cores.
    pub l2d_hit: u32,
    /// L2 data miss events, both cores.
    pub l2d_miss: u32,
}

impl CacheCounts {
    #[inline(always)]
    fn read() -> Self {
        let (l1i, l1d) = if hart() == 0 {
            (L1I_CORE0, L1D_CORE0)
        } else {
            (L1I_CORE1, L1D_CORE1)
        };
        Self {
            l1i_hit: cache_read(l1i),
            l1i_miss: cache_read(l1i + 4),
            l1d_hit: cache_read(l1d),
            l1d_miss: cache_read(l1d + 4),
            l2i_hit: cache_read(L2I[0]).wrapping_add(cache_read(L2I[1])),
            l2i_miss: cache_read(L2I[0] + 4).wrapping_add(cache_read(L2I[1] + 4)),
            l2d_hit: cache_read(L2D[0]).wrapping_add(cache_read(L2D[1])),
            l2d_miss: cache_read(L2D[0] + 4).wrapping_add(cache_read(L2D[1] + 4)),
        }
    }

    #[inline]
    fn wrapping_sub(&self, earlier: &Self) -> Self {
        Self {
            l1i_hit: self.l1i_hit.wrapping_sub(earlier.l1i_hit),
            l1i_miss: self.l1i_miss.wrapping_sub(earlier.l1i_miss),
            l1d_hit: self.l1d_hit.wrapping_sub(earlier.l1d_hit),
            l1d_miss: self.l1d_miss.wrapping_sub(earlier.l1d_miss),
            l2i_hit: self.l2i_hit.wrapping_sub(earlier.l2i_hit),
            l2i_miss: self.l2i_miss.wrapping_sub(earlier.l2i_miss),
            l2d_hit: self.l2d_hit.wrapping_sub(earlier.l2d_hit),
            l2d_miss: self.l2d_miss.wrapping_sub(earlier.l2d_miss),
        }
    }
}

/// Every counter of the calling core at one instant: the CSR counters and the cache
/// counters.
///
/// The cache counters are memory-mapped and slower to read, so they stay outside the
/// window: [`Sample::now`] reads them before the CSRs, [`Sample::end`] after. Only the five
/// `csrr` of [`CpuSample`] fall inside.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    /// CSR counters.
    pub cpu: CpuSample,
    /// Cache counters.
    pub cache: CacheCounts,
}

impl Sample {
    /// Reads every counter to open a window: cache counters first, then the CSRs.
    ///
    /// When one sample both ends a window and starts the next, `now` is fine; the cache
    /// reads then fall inside the window that it ends.
    #[inline(always)]
    pub fn now() -> Self {
        let cache = CacheCounts::read();
        let cpu = CpuSample::now();
        Self { cpu, cache }
    }

    /// Reads every counter to close a window: the CSRs first, then the cache counters.
    #[inline(always)]
    pub fn end() -> Self {
        let cpu = CpuSample::now();
        let cache = CacheCounts::read();
        Self { cpu, cache }
    }

    /// Closes the window opened by `self`: `Sample::end().since(self)`.
    #[inline(always)]
    pub fn elapsed(&self) -> Delta {
        Sample::end().since(self)
    }

    /// Counts from `earlier` to `self`, each a wrapping 32-bit difference.
    #[inline]
    pub fn since(&self, earlier: &Sample) -> Delta {
        Delta {
            cpu: self.cpu.since(&earlier.cpu),
            cache: self.cache.wrapping_sub(&earlier.cache),
        }
    }
}

/// Counter differences over one window, from [`Sample::since`] or [`Sample::elapsed`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Delta {
    /// CSR counter differences.
    pub cpu: CpuDelta,
    /// Cache counter differences.
    pub cache: CacheCounts,
}

impl Delta {
    /// Instructions per 1000 cycles (IPC times 1000), 0 for an empty window.
    #[inline]
    pub fn ipc_milli(&self) -> u32 {
        self.cpu.ipc_milli()
    }
}

impl fmt::Display for Delta {
    /// One line: the [`CpuDelta`] fields, then `l1i_hit=.. l1i_miss=.. l1d_hit=..
    /// l1d_miss=.. l2i_hit=.. l2i_miss=.. l2d_hit=.. l2d_miss=..`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = &self.cache;
        write!(
            f,
            "{} l1i_hit={} l1i_miss={} l1d_hit={} l1d_miss={} l2i_hit={} l2i_miss={} l2d_hit={} l2d_miss={}",
            self.cpu,
            c.l1i_hit,
            c.l1i_miss,
            c.l1d_hit,
            c.l1d_miss,
            c.l2i_hit,
            c.l2i_miss,
            c.l2d_hit,
            c.l2d_miss,
        )
    }
}
