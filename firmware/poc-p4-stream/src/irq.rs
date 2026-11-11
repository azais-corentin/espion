//! Time in the interrupt handlers, per core, and the wake and sleep paths (cargo feature
//! `accounting`).
//!
//! The handlers are bound by their drivers: the CSI receiver's three, the JPEG codec's and its
//! two 2D-DMA channels' (`esp32p4-csi`, `esp32p4-jpeg`), esp-hal's EMAC handler and esp-rtos's
//! timer tick. [`install`] puts a wrapper in front of each: the vector table entry gets a
//! trampoline that calls the handler it replaced; the source's mapping and priority stay the
//! driver's. Each call is timed with `mcycle`, and a handler's own time leaves out the handlers
//! that interrupted it (nesting). Not counted: the trap entry and exit around the handlers
//! (esp-hal's `handle_interrupts`), and esp-rtos's context-switch software interrupt, which is
//! bound directly to a CPU interrupt, not through the table (its time falls in the wake path).
//!
//! **Outside the executor**: the own time of handlers that ran while the executor was not in
//! a pass (`cpu`'s hooks count a pass from its start to its idle; a handler during a pass counts
//! towards the pass, and towards the task being polled).
//!
//! **Wake path**: the cycles from the end of a handler that ran outside a pass to the start of
//! the next pass, less the handlers in between, if shorter than `WAKE_MAX` (100 µs): the
//! scheduler's software interrupt, the context switch back to the executor's thread and the
//! executor's own wake-up. A longer gap means that handler woke nothing; it is not counted.
//!
//! **Sleep path** (with `idle-counter` too): the cycles from the executor going idle to the
//! idle loop's start, if shorter than `WAKE_MAX`: the executor thread's wait, the context
//! switch to the idle context. Handlers in between are counted in both (rare).

#[cfg(feature = "accounting")]
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed};

#[cfg(feature = "accounting")]
use esp_hal::peripherals::Interrupt;

#[cfg(feature = "accounting")]
use crate::cpu::{CORES, Counter64, core, mcycle};

/// Interrupt sources with a wrapper.
#[derive(Clone, Copy)]
pub enum Source {
    /// esp-hal's EMAC handler (`ETH_MAC`): one per TX-complete or RX interrupt.
    Eth,
    /// esp-rtos's timer tick (`TG0_T0_LEVEL`, TIMG0 timer 0).
    Tick,
    /// `esp32p4-csi`: the receiver's DMA (`DMA`), host errors (`CSI`), bridge errors.
    CsiDma,
    CsiHost,
    CsiBridge,
    /// `esp32p4-jpeg`: the codec (`JPEG`) and the 2D-DMA RX channel 1 and TX channel 0.
    Jpeg,
    Dma2dRx,
    Dma2dTx,
}

/// Wrapped sources.
pub const SOURCES: usize = 8;

/// Report names, in [`Source`] order.
pub const NAMES: [&str; SOURCES] = [
    "eth",
    "tick",
    "csi_dma",
    "csi_host",
    "csi_bridge",
    "jpeg",
    "dma2d_rx",
    "dma2d_tx",
];

/// Longest wake path counted: 100 µs at 400 MHz.
#[cfg(feature = "accounting")]
const WAKE_MAX: u32 = 40_000;

/// One core's handler and wake-path counts.
#[derive(Clone, Copy, Default)]
pub struct Snapshot {
    /// Calls per source.
    pub calls: [u32; SOURCES],
    /// Own cycles per source.
    pub cycles: [u64; SOURCES],
    /// Own cycles of the handlers that ran outside an executor pass.
    pub outside: u64,
    /// Wake paths counted, and their cycles.
    pub wakes: u32,
    pub wake_cycles: u64,
    /// Sleep paths counted, and their cycles (only with `idle-counter`).
    pub sleeps: u32,
    pub sleep_cycles: u64,
}

impl Snapshot {
    /// Counts gained since `earlier`.
    pub fn since(&self, earlier: &Snapshot) -> Snapshot {
        let mut out = *self;
        for k in 0..SOURCES {
            out.calls[k] = out.calls[k].wrapping_sub(earlier.calls[k]);
            out.cycles[k] -= earlier.cycles[k];
        }
        out.outside -= earlier.outside;
        out.wakes = out.wakes.wrapping_sub(earlier.wakes);
        out.wake_cycles -= earlier.wake_cycles;
        out.sleeps = out.sleeps.wrapping_sub(earlier.sleeps);
        out.sleep_cycles -= earlier.sleep_cycles;
        out
    }

    /// Own cycles of all handlers.
    pub fn total(&self) -> u64 {
        self.cycles.iter().sum()
    }

    /// The executor's busy cycles `executor_busy` plus the handlers outside its passes, the
    /// wake paths and the sleep paths: all the time these accounts see the core away from its
    /// idle loop.
    pub fn busy_all(&self, executor_busy: u64) -> u64 {
        executor_busy + self.outside + self.wake_cycles + self.sleep_cycles
    }
}

/// Puts the wrapper in front of the handler each of `sources` has now. Call it after the
/// driver bound its handler; a source already wrapped is left alone. A no-op without the
/// `accounting` feature.
pub fn install(sources: &[Source]) {
    #[cfg(feature = "accounting")]
    for &s in sources {
        wrap(s as usize);
    }
    #[cfg(not(feature = "accounting"))]
    let _ = sources;
}

/// The counts of core `c` now (zeros without the `accounting` feature).
pub fn snapshot(c: usize) -> Snapshot {
    #[cfg(feature = "accounting")]
    {
        let a = &ACC[c];
        let mut s = Snapshot::default();
        for k in 0..SOURCES {
            s.calls[k] = a.calls[k].load(Relaxed);
            s.cycles[k] = a.cycles[k].load();
        }
        s.outside = a.outside.load();
        s.wakes = a.wakes.load(Relaxed);
        s.wake_cycles = a.wake_cycles.load();
        s.sleeps = a.sleeps.load(Relaxed);
        s.sleep_cycles = a.sleep_cycles.load();
        s
    }
    #[cfg(not(feature = "accounting"))]
    {
        let _ = c;
        Snapshot::default()
    }
}

#[cfg(feature = "accounting")]
mod imp {
    use super::*;

    /// One core's accounts. Only that core writes them (handlers and the executor's hooks);
    /// the atomics make a nested handler's update safe against the one it interrupted.
    pub(super) struct Core {
        pub(super) calls: [AtomicU32; SOURCES],
        pub(super) cycles: [Counter64; SOURCES],
        /// Own cycles of all handlers finished on this core (wraps): what a handler subtracts
        /// for the handlers that nested inside it.
        nested: AtomicU32,
        pub(super) outside: Counter64,
        in_pass: AtomicBool,
        exit_pending: AtomicBool,
        exit_at: AtomicU32,
        exit_nested: AtomicU32,
        pub(super) wakes: AtomicU32,
        pub(super) wake_cycles: Counter64,
        idle_pending: AtomicBool,
        idle_at: AtomicU32,
        pub(super) sleeps: AtomicU32,
        pub(super) sleep_cycles: Counter64,
    }

    #[allow(clippy::declare_interior_mutable_const, reason = "array initialiser")]
    const ZERO: AtomicU32 = AtomicU32::new(0);
    #[allow(clippy::declare_interior_mutable_const, reason = "array initialiser")]
    const ZERO64: Counter64 = Counter64::new();
    #[allow(clippy::declare_interior_mutable_const, reason = "array initialiser")]
    const CORE: Core = Core {
        calls: [ZERO; SOURCES],
        cycles: [ZERO64; SOURCES],
        nested: AtomicU32::new(0),
        outside: Counter64::new(),
        in_pass: AtomicBool::new(false),
        exit_pending: AtomicBool::new(false),
        exit_at: AtomicU32::new(0),
        exit_nested: AtomicU32::new(0),
        wakes: AtomicU32::new(0),
        wake_cycles: Counter64::new(),
        idle_pending: AtomicBool::new(false),
        idle_at: AtomicU32::new(0),
        sleeps: AtomicU32::new(0),
        sleep_cycles: Counter64::new(),
    };

    pub(super) static ACC: [Core; CORES] = [CORE; CORES];

    /// The handler each trampoline calls (0: not installed).
    #[allow(clippy::declare_interior_mutable_const, reason = "array initialiser")]
    const NONE: AtomicUsize = AtomicUsize::new(0);
    static ORIGINAL: [AtomicUsize; SOURCES] = [NONE; SOURCES];

    /// Vector table index of each source. The 2D-DMA ones are not in the PAC's `Interrupt`:
    /// ESP-IDF's `soc/esp32p4/include/soc/interrupts.h` numbers, as `esp32p4-jpeg` binds them
    /// (`DMA2D_IN_CH1` = 105, `DMA2D_OUT_CH0` = 106).
    const VECTOR: [usize; SOURCES] = [
        Interrupt::ETH_MAC as usize,
        Interrupt::TG0_T0_LEVEL as usize,
        Interrupt::DMA as usize,
        Interrupt::CSI as usize,
        Interrupt::CSI_BRIDGE as usize,
        Interrupt::JPEG as usize,
        105,
        106,
    ];

    macro_rules! trampolines {
        ($($name:ident = $k:literal),* $(,)?) => {
            $(
                #[esp_hal::ram]
                extern "C" fn $name() {
                    run($k);
                }
            )*
            const TRAMPOLINES: [extern "C" fn(); SOURCES] = [$($name),*];
        };
    }

    trampolines!(
        t0 = 0,
        t1 = 1,
        t2 = 2,
        t3 = 3,
        t4 = 4,
        t5 = 5,
        t6 = 6,
        t7 = 7
    );

    /// Calls source `k`'s handler and books its own time on this core.
    #[inline(always)]
    fn run(k: usize) {
        let a = &ACC[core()];
        let f = ORIGINAL[k].load(Relaxed);
        // SAFETY: `wrap` stored the address of the handler the table held (a valid
        // `extern "C" fn()`, non-zero) before it put this trampoline in the table.
        let f = unsafe { core::mem::transmute::<usize, extern "C" fn()>(f) };
        // Order: a handler that nests between the two reads at either end is counted in the
        // elapsed time but not in the nested sum, never the other way round, so `own` cannot
        // go below zero (it is at most that handler's time high, rarely).
        let t0 = mcycle();
        let n0 = a.nested.load(Relaxed);
        f();
        let n1 = a.nested.load(Relaxed);
        let t1 = mcycle();
        let own = t1.wrapping_sub(t0).saturating_sub(n1.wrapping_sub(n0));
        let nested = a.nested.fetch_add(own, Relaxed).wrapping_add(own);
        a.calls[k].fetch_add(1, Relaxed);
        a.cycles[k].add(own);
        if !a.in_pass.load(Relaxed) {
            a.outside.add(own);
            a.exit_at.store(t1, Relaxed);
            a.exit_nested.store(nested, Relaxed);
            a.exit_pending.store(true, Relaxed);
        }
    }

    pub(super) fn wrap(k: usize) {
        unsafe extern "Rust" {
            static __EXTERNAL_INTERRUPTS: esp32p4::Vector;
        }
        // SAFETY: the PAC's vector table has 128 entries in RAM (`.rwtext.interrupt`), each a
        // handler address; esp-hal's `bind_handler` and `esp32p4-jpeg` write it the same way.
        let entry = unsafe {
            (&raw const __EXTERNAL_INTERRUPTS)
                .add(VECTOR[k])
                .cast::<usize>()
                .cast_mut()
        };
        let trampoline = TRAMPOLINES[k] as usize;
        // SAFETY: as above; a word read.
        let bound = unsafe { entry.read_volatile() };
        if bound == trampoline || bound == 0 {
            return;
        }
        ORIGINAL[k].store(bound, Relaxed);
        // SAFETY: as above; a word write, atomic against the handler's own dispatch, made
        // after the trampoline's target is in place.
        unsafe { entry.write_volatile(trampoline) };
    }

    /// The executor starts a pass on core `c` at `now`: ends a wake path.
    pub(crate) fn pass_start(c: usize, now: u32) {
        let a = &ACC[c];
        a.in_pass.store(true, Relaxed);
        if a.exit_pending.swap(false, Relaxed) {
            let between = a
                .nested
                .load(Relaxed)
                .wrapping_sub(a.exit_nested.load(Relaxed));
            let gap = now
                .wrapping_sub(a.exit_at.load(Relaxed))
                .wrapping_sub(between);
            if gap < WAKE_MAX {
                a.wakes.fetch_add(1, Relaxed);
                a.wake_cycles.add(gap);
            }
        }
    }

    /// The executor on core `c` goes idle at `now`: starts a sleep path.
    pub(crate) fn pass_end(c: usize, now: u32) {
        let a = &ACC[c];
        a.in_pass.store(false, Relaxed);
        a.idle_at.store(now, Relaxed);
        a.idle_pending.store(true, Relaxed);
    }

    /// The idle loop on core `c` starts at `now` (`idle-counter`): ends a sleep path.
    #[cfg(feature = "idle-counter")]
    pub(crate) fn idle_entered(c: usize, now: u32) {
        let a = &ACC[c];
        if a.idle_pending.swap(false, Relaxed) {
            let gap = now.wrapping_sub(a.idle_at.load(Relaxed));
            if gap < WAKE_MAX {
                a.sleeps.fetch_add(1, Relaxed);
                a.sleep_cycles.add(gap);
            }
        }
    }
}

#[cfg(all(feature = "accounting", feature = "idle-counter"))]
pub(crate) use imp::idle_entered;
#[cfg(feature = "accounting")]
use imp::{ACC, wrap};
#[cfg(feature = "accounting")]
pub(crate) use imp::{pass_end, pass_start};
