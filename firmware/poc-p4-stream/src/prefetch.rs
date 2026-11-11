//! Cache prefetch for the CPU's reads of PSRAM: the body copy of every datagram sent beside
//! smoltcp (`net::chunk::send`: the stream's raw frames and JPEG files, the bench's PSRAM and
//! camera sources) and the RAW10-to-RAW8 selection (plan step 6.2). The test hooks' `A<n>`
//! (stream) and the bench request's second byte select the [`Mode`]; `esp32p4-cache`'s
//! `prefetch` module does the register work.
//!
//! # Why a prefetch cannot hand the CPU a stale line
//!
//! A prefetch loads lines whether or not a DMA is still writing them, and a line loaded
//! before the DMA finished holds old bytes. The CPU here reads three kinds of PSRAM buffer: a
//! camera frame (`esp32p4-csi`'s `wait_frame` invalidates it when it hands it to the capture
//! task or the bench), a JPEG file (`esp32p4-jpeg` invalidates its lines when the encode ends)
//! and the bench's PSRAM buffer (written back and invalidated before each pass). For each:
//!
//! 1. the invalidate comes after the DMA's last write to the buffer;
//! 2. no DMA writes the buffer between that invalidate and the CPU's last read of it (the
//!    `Frame`, the encoder's `Out` and the bench's buffer are held until then, and the reader
//!    calls [`settle`] before it gives the buffer back);
//! 3. no prefetch of the buffer is in flight during its invalidate. Autoload runs only inside
//!    [`reading`], which turns it off and waits for its `DONE` bit before it returns, and no
//!    invalidate runs inside [`reading`] (the capture task, the encode task, the bench's TX
//!    task and the network runner, for internal RAM, make them outside it; no interrupt
//!    handler makes one). A manual preload loads the lines of the range it is given, all in
//!    the buffer being read, which is held: it is not invalidated again until it was given
//!    back, after [`settle`].
//!
//! So every line of the buffer that the CPU finds in a cache was loaded after the DMA's
//! write ended. An autoload section covers only the lines of the bytes being read, rounded
//! out to the 128-byte line inside their buffer; should the hardware load past a section's
//! end, into a buffer a DMA is still writing, the stale line is dropped by that buffer's own
//! invalidate (point 1) before the CPU reads it.

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering::Relaxed};

use esp32p4_cache::prefetch::{self, Autoload, Level, Order, Section, Trigger};

/// The prefetch around each read of PSRAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    /// None.
    Off = 0,
    /// L1 data and L2 autoload, triggered by a miss or a hit, over the bytes read.
    L1L2 = 1,
    /// L1 data autoload alone, miss or hit.
    L1 = 2,
    /// L2 autoload alone, miss or hit.
    L2 = 3,
    /// L1 data and L2 autoload, triggered by a hit only.
    L1L2Hit = 4,
    /// A manual L1 data preload of the bytes read, started just before the read.
    Preload = 5,
    /// A manual L1 data preload of the next bytes the reader names, started after the read
    /// and left running (the current bytes are preloaded first if the last call did not).
    PreloadAhead = 6,
}

/// Modes, in [`Mode`] order, and their report names.
pub const MODES: u8 = 7;
const ALL: [Mode; MODES as usize] = [
    Mode::Off,
    Mode::L1L2,
    Mode::L1,
    Mode::L2,
    Mode::L1L2Hit,
    Mode::Preload,
    Mode::PreloadAhead,
];
pub const NAMES: [&str; MODES as usize] = [
    "off",
    "l1l2",
    "l1",
    "l2",
    "l1l2_hit",
    "preload",
    "preload_ahead",
];

/// The mode in use: [`Mode::PreloadAhead`] from boot, the fastest measured (README, plan step
/// 6.2); the others stay selectable for measurement.
static MODE: AtomicU8 = AtomicU8::new(Mode::PreloadAhead as u8);

/// The first line of the range [`Mode::PreloadAhead`] preloaded last, which may still be in
/// flight (0: none since the last [`settle`]).
static AHEAD: AtomicUsize = AtomicUsize::new(0);

/// The 128-byte line of the L2 cache in PSRAM, the larger of the two levels' lines.
const LINE: usize = 128;

/// Selects mode `n` (a [`Mode`] number); `false` for an unknown one. Waits for a preload in
/// flight first ([`settle`]); the next [`reading`] starts afresh.
pub fn set_mode(n: u8) -> bool {
    if n >= MODES {
        return false;
    }
    settle();
    MODE.store(n, Relaxed);
    true
}

/// The name of the mode in use.
pub fn mode_name() -> &'static str {
    NAMES[usize::from(MODE.load(Relaxed))]
}

/// The lines of `src`: the first one's address and the length, or `None` for nothing to
/// prefetch (empty, or in internal RAM).
fn lines(src: &[u8]) -> Option<(usize, usize)> {
    let addr = src.as_ptr() as usize;
    if src.is_empty() || esp32p4_cache::in_l2mem(addr, src.len()) {
        return None;
    }
    let start = addr & !(LINE - 1);
    let len = (addr + src.len()).next_multiple_of(LINE) - start;
    Some((start, len.min(prefetch::L1D_PRELOAD_MAX & !(LINE - 1))))
}

/// Runs `f`, which reads `src` of PSRAM (and no other PSRAM), with the selected prefetch over
/// `src`'s lines. Autoload is off again when this returns. Nothing inside `f` may invalidate a
/// cache line (module docs). A buffer in internal RAM gets no prefetch.
#[inline]
pub fn reading<R>(src: &[u8], f: impl FnOnce() -> R) -> R {
    let mode = ALL[usize::from(MODE.load(Relaxed))];
    let Some((start, len)) = lines(src).filter(|_| mode != Mode::Off) else {
        return f();
    };
    let (l1, l2, trigger) = match mode {
        Mode::Off => unreachable!(),
        Mode::Preload => {
            prefetch::l1d_preload(start, len);
            let r = f();
            prefetch::l1d_preload_wait();
            return r;
        }
        Mode::PreloadAhead => {
            // Already preloaded by the `ahead` before this read, unless the reader moved.
            if AHEAD.load(Relaxed) != start {
                prefetch::l1d_preload(start, len);
                AHEAD.store(start, Relaxed);
            }
            return f();
        }
        Mode::L1L2 => (true, true, Trigger::MissHit),
        Mode::L1 => (true, false, Trigger::MissHit),
        Mode::L2 => (false, true, Trigger::MissHit),
        Mode::L1L2Hit => (true, true, Trigger::Hit),
    };
    let config = Autoload {
        trigger,
        order: Order::Ascending,
        sections: [Some(Section { addr: start, len }), None, None, None],
    };
    for (on, level) in [(l1, Level::L1Data), (l2, Level::L2)] {
        if on {
            prefetch::autoload_configure(level, &config);
            prefetch::autoload_enable(level);
        }
    }
    let r = f();
    for (on, level) in [(l1, Level::L1Data), (l2, Level::L2)] {
        if on {
            prefetch::autoload_disable(level);
        }
    }
    r
}

/// With [`Mode::PreloadAhead`], starts the preload of `next`, what the reader will read next
/// from the buffer it holds (empty: nothing), and leaves it running until the next
/// [`reading`] or [`settle`].
pub fn ahead(next: &[u8]) {
    if MODE.load(Relaxed) != Mode::PreloadAhead as u8 {
        return;
    }
    if let Some((start, len)) = lines(next) {
        prefetch::l1d_preload(start, len);
        AHEAD.store(start, Relaxed);
    }
}

/// Waits for a preload [`reading`] or [`ahead`] left running. Call it before the buffer read
/// is given back (to the receiver, the encoder or the bench's flush).
pub fn settle() {
    if AHEAD.swap(0, Relaxed) != 0 {
        prefetch::l1d_preload_wait();
    }
}
