//! Hardware prefetch: autoload of the L1 data cache and the L2 cache, and manual preload of
//! the L1 data cache.
//!
//! Registers of the `CACHE` block (0x3FF1_0000), through the `esp32p4` 0.3.0 PAC:
//! `L1_DCACHE_AUTOLOAD_CTRL` +0x134 with its sections `L1_DCACHE_AUTOLOAD_SCT0..3_ADDR/SIZE`
//! +0x138..+0x154; `L2_CACHE_AUTOLOAD_CTRL` +0x2B4 with `L2_CACHE_AUTOLOAD_SCT0..3_ADDR/SIZE`
//! +0x2B8..+0x2D4; `L1_DCACHE_PRELOAD_CTRL/ADDR/SIZE` +0xD8..+0xE0. The TRM (9.3.3.3) says
//! only that autoload "prefetches continuous data based on the current address where the
//! cache hits or misses"; no field sets how far ahead it fetches.
//!
//! Measured on the board on 2026-11-11, without any DMA running (`poc-p4-eth` spike, whole
//! 1280x800 RAW8 frames scanned by the scalar blob detector):
//! - frame in PSRAM, L1 data and L2 autoload, miss+hit trigger: 9.9 ms to 4.6 ms dark,
//!   10.8 ms to 5.4 ms with 64 blobs; either autoload alone 5.6 to 5.8 ms dark;
//! - frame in internal-RAM strips, L1 data autoload, miss+hit or hit trigger: 4.33 ms to
//!   4.08 ms dark;
//! - L2 autoload does nothing for internal RAM: the L2 cache does not cache L2MEM;
//! - the hit trigger alone is poor on PSRAM (7.1 ms dark) \[INFERENCE on the cause: it needs
//!   a hit before it starts, so the first miss of each run waits];
//! - manual L1 data preload 12 rows (15 360 B) ahead: 4.4 ms dark from PSRAM.
//!
//! # DMA
//!
//! Prefetch loads lines whether or not a DMA is still writing them. A line loaded before the
//! DMA has finished writing it holds stale data; if that happens after the invalidate, nothing
//! drops it before the CPU reads it. So a section must cover only data the DMA has already
//! delivered (and that has been invalidated since), or autoload must be suspended
//! ([`autoload_suspend`]) around the invalidate and resumed only over delivered data.
//!
//! Run under live DMA on the board on 2026-11-11, each time limited to a buffer the DMA had
//! finished and that was invalidated, with no prefetch in flight at its next invalidate
//! (figures in the crate READMEs named):
//! - `poc-p4-eth`, "L1 data autoload on the strip path": L1 data autoload, miss+hit, over
//!   the one strip the detector holds, disabled after each strip; the strip check found 0 bad
//!   rows of 10 874 992 in four modes, and the 120 s live soak at 1280x800 RAW8 120 fps
//!   dropped no frame.
//! - `poc-p4-stream`, "Measured: plan step 6.2": manual L1 data preload of the next datagram's
//!   bytes of a camera frame or JPEG file; a 180 s rolling-pattern check at the wire rate
//!   found 0 failed frames of 1967.
//!
//! Not tested: the L1 and L2 autoload over whole frames in PSRAM under live capture.
//!
//! # No lock
//!
//! These functions write the autoload and preload registers only, not the cache-sync
//! registers the ROM maintenance functions program, so they do not take the crate's lock
//! \[INFERENCE: the PAC lists them as separate registers. Prefetch on one core while the
//! other core runs cache-sync operations ran 2 minutes without a hang or trap (`poc-p4-eth`,
//! core 1's per-strip autoload beside core 0's contention loop, crate README "Lock
//! statistics"); the data on that path was not checked]. Each register set exists once
//! for the whole chip (the L1 data cache is shared by both cores), so one context, on one
//! core, owns the autoload set-up and the manual preloads; these functions do not guard
//! against a second owner. Open point: the detector's per-strip autoload (`poc-p4-eth`) and
//! the stream's preload (`poc-p4-stream`) would share these registers if both ran in one
//! firmware; that combination has not run.

use esp32p4::cache::RegisterBlock;

/// Which cache an autoload set-up applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// The L1 data cache, shared by both cores. Prefetches internal RAM (L2MEM) and, through
    /// L2, PSRAM and flash.
    L1Data,
    /// The L2 cache. Prefetches PSRAM and flash only: it does not cache L2MEM.
    L2,
}

/// What starts an autoload (`*_AUTOLOAD_TRIGGER_MODE`, bits 3:4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// A miss inside a section (field value 0).
    Miss,
    /// A hit inside a section (1). Measured poor on PSRAM: the first miss of each run waits.
    Hit,
    /// A miss or a hit (2). The best setting measured, for PSRAM and for internal RAM.
    MissHit,
}

/// Direction of an autoload (`*_AUTOLOAD_ORDER`, bit 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// Towards higher addresses (0). The only order measured.
    Ascending,
    /// Towards lower addresses (1).
    Descending,
}

/// One autoload section: the cached (0x4...) address of its first byte and its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    /// First byte.
    pub addr: usize,
    /// Length in bytes, below [`SECTION_MAX`].
    pub len: usize,
}

/// Limit of a section's length: the `*_AUTOLOAD_SCTn_SIZE` field has 28 bits.
pub const SECTION_MAX: usize = 1 << 28;

/// An autoload set-up for one cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Autoload {
    /// What starts a prefetch.
    pub trigger: Trigger,
    /// Prefetch direction.
    pub order: Order,
    /// Up to four sections; a `None` entry is left disabled (`*_AUTOLOAD_SCTn_ENA` clear).
    pub sections: [Option<Section>; 4],
}

fn regs() -> &'static RegisterBlock {
    // SAFETY: fixed address of the CACHE block; every access below is a volatile register
    // access, and the module docs make one context the owner of these registers.
    unsafe { &*esp32p4::CACHE::ptr() }
}

/// Writes the sections and the control word of `level`'s autoload and leaves it disabled.
/// A running autoload is disabled first (as [`autoload_disable`]). Turn it on with
/// [`autoload_enable`].
///
/// Panics if a section is not shorter than [`SECTION_MAX`].
pub fn autoload_configure(level: Level, config: &Autoload) {
    autoload_disable(level);
    let c = regs();
    for (n, s) in config.sections.iter().enumerate() {
        let Some(s) = s else { continue };
        assert!(s.len < SECTION_MAX, "autoload section too long");
        let (addr, len) = (s.addr as u32, s.len as u32);
        // SAFETY: plain address and size words (32 and 28 bits); a section only steers
        // prefetch, it never changes what a load returns from memory.
        unsafe {
            match (level, n) {
                (Level::L1Data, 0) => {
                    c.l1_dcache_autoload_sct0_addr().write(|w| w.bits(addr));
                    c.l1_dcache_autoload_sct0_size().write(|w| w.bits(len));
                }
                (Level::L1Data, 1) => {
                    c.l1_dcache_autoload_sct1_addr().write(|w| w.bits(addr));
                    c.l1_dcache_autoload_sct1_size().write(|w| w.bits(len));
                }
                (Level::L1Data, 2) => {
                    c.l1_dcache_autoload_sct2_addr().write(|w| w.bits(addr));
                    c.l1_dcache_autoload_sct2_size().write(|w| w.bits(len));
                }
                (Level::L1Data, _) => {
                    c.l1_dcache_autoload_sct3_addr().write(|w| w.bits(addr));
                    c.l1_dcache_autoload_sct3_size().write(|w| w.bits(len));
                }
                (Level::L2, 0) => {
                    c.l2_cache_autoload_sct0_addr().write(|w| w.bits(addr));
                    c.l2_cache_autoload_sct0_size().write(|w| w.bits(len));
                }
                (Level::L2, 1) => {
                    c.l2_cache_autoload_sct1_addr().write(|w| w.bits(addr));
                    c.l2_cache_autoload_sct1_size().write(|w| w.bits(len));
                }
                (Level::L2, 2) => {
                    c.l2_cache_autoload_sct2_addr().write(|w| w.bits(addr));
                    c.l2_cache_autoload_sct2_size().write(|w| w.bits(len));
                }
                (Level::L2, _) => {
                    c.l2_cache_autoload_sct3_addr().write(|w| w.bits(addr));
                    c.l2_cache_autoload_sct3_size().write(|w| w.bits(len));
                }
            }
        }
    }
    let trigger = match config.trigger {
        Trigger::Miss => 0,
        Trigger::Hit => 1,
        Trigger::MissHit => 2,
    };
    let descending = config.order == Order::Descending;
    let on = |n: usize| config.sections[n].is_some();
    // GID (bits 12:15) stays 0, as at reset and in the measurements.
    match level {
        Level::L1Data => c.l1_dcache_autoload_ctrl().write(|w| unsafe {
            // SAFETY: trigger is one of the three documented values of the 2-bit field.
            w.l1_dcache_autoload_trigger_mode().bits(trigger);
            w.l1_dcache_autoload_order().bit(descending);
            w.l1_dcache_autoload_sct0_ena().bit(on(0));
            w.l1_dcache_autoload_sct1_ena().bit(on(1));
            w.l1_dcache_autoload_sct2_ena().bit(on(2));
            w.l1_dcache_autoload_sct3_ena().bit(on(3))
        }),
        Level::L2 => c.l2_cache_autoload_ctrl().write(|w| unsafe {
            // SAFETY: as above.
            w.l2_cache_autoload_trigger_mode().bits(trigger);
            w.l2_cache_autoload_order().bit(descending);
            w.l2_cache_autoload_sct0_ena().bit(on(0));
            w.l2_cache_autoload_sct1_ena().bit(on(1));
            w.l2_cache_autoload_sct2_ena().bit(on(2));
            w.l2_cache_autoload_sct3_ena().bit(on(3))
        }),
    };
}

/// Turns `level`'s autoload on with the set-up of [`autoload_configure`] (sets
/// `*_AUTOLOAD_ENA`, bit 0).
pub fn autoload_enable(level: Level) {
    let c = regs();
    match level {
        Level::L1Data => c
            .l1_dcache_autoload_ctrl()
            .modify(|_, w| w.l1_dcache_autoload_ena().set_bit()),
        Level::L2 => c
            .l2_cache_autoload_ctrl()
            .modify(|_, w| w.l2_cache_autoload_ena().set_bit()),
    };
}

/// Turns `level`'s autoload off: clears `*_AUTOLOAD_ENA` and waits until `*_AUTOLOAD_DONE`
/// (bit 1) is set, so no prefetch is still in flight on return. The register reads 0x2
/// (disabled, done) at reset and read 0x2 on the board right after autoload was turned off;
/// that the bit also covers a fetch in flight is \[INFERENCE\] from its PAC description.
pub fn autoload_disable(level: Level) {
    let c = regs();
    match level {
        Level::L1Data => {
            c.l1_dcache_autoload_ctrl()
                .modify(|_, w| w.l1_dcache_autoload_ena().clear_bit());
            while c
                .l1_dcache_autoload_ctrl()
                .read()
                .l1_dcache_autoload_done()
                .bit_is_clear()
            {}
        }
        Level::L2 => {
            c.l2_cache_autoload_ctrl()
                .modify(|_, w| w.l2_cache_autoload_ena().clear_bit());
            while c
                .l2_cache_autoload_ctrl()
                .read()
                .l2_cache_autoload_done()
                .bit_is_clear()
            {}
        }
    }
}

/// `level`'s autoload is on.
pub fn autoload_enabled(level: Level) -> bool {
    let c = regs();
    match level {
        Level::L1Data => c
            .l1_dcache_autoload_ctrl()
            .read()
            .l1_dcache_autoload_ena()
            .bit(),
        Level::L2 => c
            .l2_cache_autoload_ctrl()
            .read()
            .l2_cache_autoload_ena()
            .bit(),
    }
}

/// An autoload paused by [`autoload_suspend`]; give it to [`autoload_resume`].
#[must_use = "resume the autoload with autoload_resume"]
#[derive(Debug)]
pub struct Suspended {
    level: Level,
    was_enabled: bool,
}

/// Pauses `level`'s autoload, for example around the invalidate of a buffer a section
/// covers: turns it off as [`autoload_disable`] and remembers whether it was on. The sections
/// stay as configured. The ROM has the same pair (`Cache_Suspend_L1_DCache_Autoload`,
/// `Cache_Suspend_L2_Cache_Autoload` and the `Resume` ones); these write the registers
/// directly so that a bare core needs no ROM call outside the lock.
pub fn autoload_suspend(level: Level) -> Suspended {
    let was_enabled = autoload_enabled(level);
    if was_enabled {
        autoload_disable(level);
    }
    Suspended { level, was_enabled }
}

/// Turns the autoload paused by [`autoload_suspend`] back on if it was on.
pub fn autoload_resume(suspended: Suspended) {
    if suspended.was_enabled {
        autoload_enable(suspended.level);
    }
}

/// Limit of one manual L1 data preload: `L1_DCACHE_PRELOAD_SIZE` has 14 bits. The ROM header
/// adds that a preload should not exceed the L1 data cache (64 KB), which is larger.
pub const L1D_PRELOAD_MAX: usize = (1 << 14) - 1;

/// Starts a manual preload of `addr..addr + len` into the L1 data cache, ascending, loading
/// from the next level (`L1_DCACHE_PRELOAD_ADDR`, `_SIZE`, then `_CTRL` = ENA; GID 0, mode 0).
/// Waits first for a previous preload to finish. Poll [`l1d_preload_done`] or call
/// [`l1d_preload_wait`] before relying on the lines. The board measurements issued the
/// preload of the next 12 rows (15 360 B) before scanning the current ones, with autoload
/// off; the ROM's own preload functions suspend autoload while they run, this one does not.
///
/// Panics if `len` exceeds [`L1D_PRELOAD_MAX`].
pub fn l1d_preload(addr: usize, len: usize) {
    assert!(len <= L1D_PRELOAD_MAX, "L1 data preload too long");
    l1d_preload_wait();
    let c = regs();
    // SAFETY: plain address and 14-bit size words.
    unsafe {
        c.l1_dcache_preload_addr().write(|w| w.bits(addr as u32));
        c.l1_dcache_preload_size().write(|w| w.bits(len as u32));
    }
    // ENA clears itself when the preload is done.
    c.l1_dcache_preload_ctrl()
        .write(|w| w.l1_dcache_preload_ena().set_bit());
}

/// The last L1 data preload has finished (`L1_DCACHE_PRELOAD_CTRL.DONE`, bit 1; set at
/// reset).
pub fn l1d_preload_done() -> bool {
    regs()
        .l1_dcache_preload_ctrl()
        .read()
        .l1_dcache_preload_done()
        .bit()
}

/// Waits until the last L1 data preload has finished.
pub fn l1d_preload_wait() {
    while !l1d_preload_done() {}
}
