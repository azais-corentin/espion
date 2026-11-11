//! Cache maintenance and cache prefetch for the ESP32-P4, callable from either HP core.
//!
//! # Maintenance
//!
//! [`invalidate`], [`write_back`] and [`writeback_invalidate`] run the ROM cache functions
//! (`Cache_Invalidate_Addr`, `Cache_WriteBack_Addr`; esp-hal's private
//! `soc::cache_invalidate_addr` and ESP-IDF's `esp_cache_msync` call the same ones). Internal
//! L2 memory (L2MEM) is cached by the L1 data cache only, so for it they touch L1; PSRAM and
//! flash go through L1 and L2, so for them they touch both. [`line_size`] gives the alignment
//! a DMA buffer needs; [`non_cacheable`] gives the alias of L2MEM that bypasses the cache.
//!
//! # The lock
//!
//! Both cores share one set of cache-sync registers (`SYNC_CTRL`, `SYNC_MAP`, `SYNC_ADDR`,
//! `SYNC_SIZE` in the `CACHE` block), and the ROM functions program them without any lock.
//! Two operations started at once from the two cores can garble each other. ESP-IDF
//! serialises them with a cross-core spinlock (`s_spinlock` in `esp_mm/esp_cache_msync.c`);
//! this crate does the same: every ROM cache call here runs while holding one spinlock, a
//! single atomic word shared by both cores.
//!
//! The lock is taken with machine interrupts masked on the calling core: `mstatus.MIE` is
//! saved and cleared, the lock word is swapped in, the ROM function runs, the word is released
//! and `MIE` is restored. If the lock is busy, `MIE` is restored while the core waits, so only
//! the hold itself masks interrupts. Masking is what makes it safe for an interrupt handler to
//! call a cache operation too: a holder that its own core could interrupt would leave that
//! handler spinning on a lock nobody can release. The crate uses no `critical_section`, so a
//! bare second core without esp-hal's global critical section can call it.
//!
//! Hold time, from the board measurements in `firmware/poc-p4-eth/README.md`: invalidating a
//! whole frame over L1 and L2 takes 17 to 31 us whatever its size; invalidating an 80 or
//! 40 KiB strip in L2MEM (L1 only) takes 1.9 to 2.3 us on average, 2.6 us at most (9.7 us
//! the first time). Write-back was not measured. So:
//! - on the calling core, interrupts wait up to one hold: about 31 us behind a whole-frame
//!   invalidate, about 3 us behind a strip;
//! - on the other core, nothing is masked; a cache operation it starts meanwhile waits up to
//!   the same hold, with its interrupts enabled.
//!
//! **Rule:** every cache operation in this repository goes through this crate. esp-hal 1.2.2
//! makes its own ROM cache calls, which this lock does not cover: the Ethernet driver on every
//! TX buffer and descriptor (`ethernet/dma.rs` through `dma/aligned.rs` to
//! `soc/esp32p4/mod.rs`), `interrupt::enable_direct` (write-back of the vector table), the
//! DMA buffer helpers (`dma/buffers/mod.rs`, `dma/buffers/scoped.rs`, `dma/aligned.rs`), the
//! SPI master's DMA path, the DSI frame buffers, and at boot the PSRAM set-up and the flash
//! invalidate in `soc/esp32p4/mod.rs`. A firmware that runs esp-hal's Ethernet driver on one
//! core and calls this crate on the other is therefore still exposed, until the Ethernet ring
//! is replaced by one that calls this crate.
//!
//! # Prefetch
//!
//! The [`prefetch`] module sets up the hardware prefetch of the L1 data cache and of the L2
//! cache (autoload) and runs manual L1 data-cache preloads. Those are plain register writes,
//! not cache-sync operations; see the module for why they take no lock and what they risk
//! beside a DMA.
//!
//! # Linking
//!
//! The ROM functions are declared here and get their addresses from the ROM linker script
//! (`esp32p4.rom.ld` of `esp-rom-sys`) that esp-hal adds to every firmware binary; the crate
//! itself depends on no esp crate. A binary without esp-hal must add that script.
//!
//! The crate must run in machine mode, which is how esp-hal firmware runs.

#![no_std]
#![warn(missing_docs)]

pub mod prefetch;

use core::arch::asm;
use core::hint::spin_loop;
use core::ops::Range;
use core::sync::atomic::{AtomicU32, Ordering};

// `esp_rom/esp32p4/include/esp32p4/rom/cache.h`.
const CACHE_MAP_L1_DCACHE: u32 = 1 << 4;
const CACHE_MAP_L2_CACHE: u32 = 1 << 5;

/// Internal L2 memory (HP L2MEM, 768 KB). The CPU reaches it through the L1 data cache only;
/// PSRAM goes through L1 and L2.
const L2MEM: Range<usize> = 0x4FF0_0000..0x4FFC_0000;

/// Offset of the non-cacheable alias of L2MEM (`SOC_NON_CACHEABLE_OFFSET_SRAM`, IDF
/// `CACHE_LL_L2MEM_NON_CACHE_ADDR`): CPU accesses there bypass the L1 cache.
const L2MEM_NON_CACHEABLE_OFFSET: usize = 0x4000_0000;

/// L1 data cache line (`cache_ll_l1_dcache_get_line_size`).
pub const L1_LINE: usize = 64;

unsafe extern "C" {
    fn Cache_Invalidate_Addr(map: u32, addr: u32, size: u32) -> i32;
    fn Cache_WriteBack_Addr(map: u32, addr: u32, size: u32) -> i32;
    fn Cache_Get_L2_Cache_Line_Size() -> u32;
}

/// `mstatus.MIE`: machine interrupts enabled on this core.
const MSTATUS_MIE: usize = 1 << 3;

/// The lock word: 0 free, 1 held. Both cores share the L1 data cache, so an atomic on it is
/// seen by both. On a line of its own, so no caller's line-aligned range can contain it and an
/// invalidate never drops it.
#[repr(align(64))]
struct Lock(AtomicU32);

static LOCK: Lock = Lock(AtomicU32::new(0));

/// Runs `f` (one or two ROM cache calls) holding the cross-core lock, with machine interrupts
/// masked on this core from before the lock is taken until after it is released.
#[inline]
fn locked<R>(f: impl FnOnce() -> R) -> R {
    let mstatus = loop {
        let mstatus: usize;
        // SAFETY: clears MIE and returns the old `mstatus` (machine mode). No `nomem`: the
        // asm is also a compiler barrier, so the swap below cannot move before it.
        unsafe { asm!("csrrci {0}, mstatus, 8", out(reg) mstatus, options(nostack)) };
        if LOCK.0.swap(1, Ordering::Acquire) == 0 {
            break mstatus;
        }
        // Busy: wait with this core's interrupts as they were, then try again masked.
        restore_mie(mstatus);
        while LOCK.0.load(Ordering::Relaxed) != 0 {
            spin_loop();
        }
    };
    let r = f();
    LOCK.0.store(0, Ordering::Release);
    restore_mie(mstatus);
    r
}

/// Sets `mstatus.MIE` again if it was set in `saved`.
#[inline(always)]
fn restore_mie(saved: usize) {
    if saved & MSTATUS_MIE != 0 {
        // SAFETY: sets MIE (machine mode); a compiler barrier like the clear.
        unsafe { asm!("csrsi mstatus, 8", options(nostack)) };
    }
}

fn map(addr: usize) -> u32 {
    if L2MEM.contains(&addr) {
        CACHE_MAP_L1_DCACHE
    } else {
        CACHE_MAP_L1_DCACHE | CACHE_MAP_L2_CACHE
    }
}

/// Alignment (start and length) a DMA buffer at `addr` needs so that invalidating it never
/// touches a neighbour's cache line: 64 B in internal RAM, the larger of the L1 line and the
/// L2 line the second-stage bootloader set (64 or 128 B) elsewhere.
pub fn line_size(addr: usize) -> usize {
    if L2MEM.contains(&addr) {
        L1_LINE
    } else {
        // SAFETY: ROM getter without side effects; under the lock like every ROM cache call.
        (locked(|| unsafe { Cache_Get_L2_Cache_Line_Size() }) as usize).max(L1_LINE)
    }
}

/// Drops every cached line of `addr..addr + len` so the CPU re-reads what the DMA wrote.
/// Dirty lines are discarded, not written back. `addr` and `len` must be multiples of
/// [`line_size`].
pub fn invalidate(addr: usize, len: usize) {
    let map = map(addr);
    // SAFETY: ROM cache operation on a range the caller owns, serialised by the lock.
    locked(|| unsafe { Cache_Invalidate_Addr(map, addr as u32, len as u32) });
}

/// Writes the dirty cached lines of `addr..addr + len` back to memory. Partial lines at either
/// end are written back whole.
pub fn write_back(addr: usize, len: usize) {
    let map = map(addr);
    // SAFETY: as above; writing back never loses data.
    locked(|| unsafe { Cache_WriteBack_Addr(map, addr as u32, len as u32) });
}

/// Writes back and then drops the cached lines of `addr..addr + len` (IDF
/// `esp_cache_msync(C2M | INVALIDATE)`), so that later non-cacheable accesses and DMA writes
/// are never overwritten by a stale dirty line. Same alignment rules as [`invalidate`]. Both
/// ROM calls run in one hold of the lock.
pub fn writeback_invalidate(addr: usize, len: usize) {
    let map = map(addr);
    // SAFETY: as above.
    locked(|| unsafe {
        Cache_WriteBack_Addr(map, addr as u32, len as u32);
        Cache_Invalidate_Addr(map, addr as u32, len as u32)
    });
}

/// `addr..addr + len` lies in internal L2 memory.
pub fn in_l2mem(addr: usize, len: usize) -> bool {
    L2MEM.contains(&addr) && addr + len <= L2MEM.end
}

/// Non-cacheable CPU address of an L2MEM address (the DMA keeps using the cached one).
pub const fn non_cacheable(addr: usize) -> usize {
    addr + L2MEM_NON_CACHEABLE_OFFSET
}
