//! CPU time accounting: the cycle counter; with the cargo feature `accounting` (on by
//! default) the cycles each embassy task spends being polled, from the executor's `trace`
//! hooks, and the time in interrupt handlers and the wake and sleep paths ([`crate::irq`]);
//! with the feature `idle-counter` the idle cycles, counted by the scheduler's idle hook.
//!
//! Every poll of a task is timed with `mcycle` (from `_embassy_trace_task_exec_begin` to
//! `_embassy_trace_task_exec_end`), and every executor pass from `_embassy_trace_poll_start`
//! to `_embassy_trace_executor_idle` (`busy`). Interrupts that fire during a poll count towards
//! the polled task and the pass; those outside a pass, and the wake and sleep paths, are in
//! [`crate::irq`]'s accounts. A task gets its name with [`name_current`] from its own first
//! poll.
//!
//! Each core has its own accounts: every hook updates the accounts of the core it runs on. The
//! hooks take a critical section each (two per poll). Without `accounting` (`cargo build
//! --release --no-default-features`) there are no hooks: snapshots hold zeros and the reports
//! say `accounting=off`.
//!
//! **Idle counter** (`idle-counter`): the idle hook spins instead of waiting for an interrupt,
//! reading `mcycle` in a loop and adding each step shorter than `IDLE_STEP_MAX` cycles to
//! the core's idle count; a longer step is time the core spent elsewhere (an interrupt, the
//! executor's thread). It needs no hook, so it measures builds with and without
//! `accounting` alike. It costs power (no `wfi`), so it is off by default.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

#[cfg(feature = "accounting")]
use core::cell::RefCell;

#[cfg(feature = "accounting")]
use critical_section::Mutex;

#[cfg(feature = "accounting")]
use crate::irq;

/// Most tasks the table tracks; later ones are summed in the last row.
const SLOTS: usize = 10;

/// Cores of the P4's HP cluster.
pub const CORES: usize = 2;

/// Longest step of the idle loop that counts as idle (cycles).
#[cfg(feature = "idle-counter")]
const IDLE_STEP_MAX: u32 = 256;

/// One task's (or the executor's) account.
#[derive(Clone, Copy, Default)]
pub struct Account {
    /// The task id the executor passes to the hooks (`0`: unused row).
    pub id: u32,
    /// Name given with [`name_current`], `"?"` before that.
    pub name: &'static str,
    /// Cycles spent in the task's polls.
    pub cycles: u64,
    /// Polls.
    pub polls: u32,
}

const NO_ACCOUNT: Account = Account {
    id: 0,
    name: "?",
    cycles: 0,
    polls: 0,
};

/// Snapshot of all accounts of one core.
#[derive(Clone, Copy)]
pub struct Snapshot {
    pub tasks: [Account; SLOTS],
    /// Cycles from each executor pass's start to its idle.
    pub busy: u64,
    /// Cycle counter at the snapshot (64-bit extension kept by the hooks: without
    /// `accounting` it is only right over windows shorter than 10.7 s).
    pub at: u64,
    /// Idle cycles from the idle counter (0 without `idle-counter`). The interrupt handlers
    /// and the wake path are in [`crate::irq::snapshot`], separate (a snapshot is copied on the
    /// stack, and both together pass the stack-frame lint).
    pub idle: u64,
}

impl Snapshot {
    /// Cycles each account gained since `earlier`, the busy cycles and the cycles elapsed.
    pub fn since(&self, earlier: &Snapshot) -> Snapshot {
        let mut out = *self;
        for (a, b) in out.tasks.iter_mut().zip(earlier.tasks.iter()) {
            if a.id == b.id {
                a.cycles -= b.cycles;
                a.polls -= b.polls;
            }
        }
        out.busy -= earlier.busy;
        out.at -= earlier.at;
        out.idle -= earlier.idle;
        out
    }
}

/// A 64-bit count that one core adds to (from any priority) and any core reads. The low word
/// is added atomically; a carry out of it goes to the high word, so a reader on another core
/// sees a value 2^32 low for the few instructions in between (once every 10.7 s of counts).
pub struct Counter64 {
    lo: AtomicU32,
    hi: AtomicU32,
}

impl Counter64 {
    pub const fn new() -> Self {
        Self {
            lo: AtomicU32::new(0),
            hi: AtomicU32::new(0),
        }
    }

    #[inline(always)]
    #[cfg(any(feature = "accounting", feature = "idle-counter"))]
    pub fn add(&self, v: u32) {
        let old = self.lo.fetch_add(v, Relaxed);
        if old.checked_add(v).is_none() {
            self.hi.fetch_add(1, Relaxed);
        }
    }

    pub fn load(&self) -> u64 {
        loop {
            let hi = self.hi.load(Relaxed);
            let lo = self.lo.load(Relaxed);
            if self.hi.load(Relaxed) == hi {
                return (u64::from(hi) << 32) | u64::from(lo);
            }
        }
    }
}

/// Count, cycles and longest of one timed event (an invalidate, a wake-up), booked on one
/// core (feature `accounting`; a no-op without it). The longest is since the last
/// [`Timed::take`].
pub struct Timed {
    n: AtomicU32,
    cycles: Counter64,
    max: AtomicU32,
}

/// What a [`Timed`] counted.
#[derive(Clone, Copy, Default)]
pub struct TimedCounts {
    pub n: u32,
    pub cycles: u64,
    pub max: u32,
}

impl TimedCounts {
    /// Counts gained since `earlier` (the longest stays this one's).
    pub fn since(&self, earlier: &TimedCounts) -> TimedCounts {
        TimedCounts {
            n: self.n.wrapping_sub(earlier.n),
            cycles: self.cycles - earlier.cycles,
            max: self.max,
        }
    }

    /// Mean cycles per event.
    pub fn mean(&self) -> u64 {
        self.cycles / u64::from(self.n.max(1))
    }
}

impl Timed {
    pub const fn new() -> Self {
        Self {
            n: AtomicU32::new(0),
            cycles: Counter64::new(),
            max: AtomicU32::new(0),
        }
    }

    /// Books one event of `cycles`.
    #[inline(always)]
    pub fn add(&self, cycles: u32) {
        #[cfg(feature = "accounting")]
        {
            self.n.fetch_add(1, Relaxed);
            self.cycles.add(cycles);
            self.max.fetch_max(cycles, Relaxed);
        }
        #[cfg(not(feature = "accounting"))]
        let _ = cycles;
    }

    /// The counts now; `reset_max` starts a new longest.
    pub fn take(&self, reset_max: bool) -> TimedCounts {
        TimedCounts {
            n: self.n.load(Relaxed),
            cycles: self.cycles.load(),
            max: if reset_max {
                self.max.swap(0, Relaxed)
            } else {
                self.max.load(Relaxed)
            },
        }
    }
}

#[allow(clippy::declare_interior_mutable_const, reason = "array initialiser")]
const IDLE_ZERO: Counter64 = Counter64::new();
/// Idle cycles per core (the idle counter).
static IDLE: [Counter64; CORES] = [IDLE_ZERO; CORES];

/// The core this runs on.
#[inline(always)]
pub fn core() -> usize {
    esp_hal::system::Cpu::current() as usize
}

/// The machine cycle counter of the core this runs on (CPU clock, 400 MHz). Wraps after
/// 10.7 s.
#[inline(always)]
pub fn mcycle() -> u32 {
    let c: u32;
    // SAFETY: reads the machine cycle counter CSR; no side effects.
    unsafe { core::arch::asm!("csrr {0}, mcycle", out(reg) c) };
    c
}

/// The scheduler's idle hook with the idle counter (`esp_rtos::start_with_idle_hook`): see
/// the module docs. esp-rtos enters it afresh each time the core goes idle.
#[cfg(feature = "idle-counter")]
pub extern "C" fn idle_hook() -> ! {
    let c = core();
    let idle = &IDLE[c];
    let mut last = mcycle();
    #[cfg(feature = "accounting")]
    irq::idle_entered(c, last);
    loop {
        let now = mcycle();
        let step = now.wrapping_sub(last);
        last = now;
        if step < IDLE_STEP_MAX {
            idle.add(step);
        }
    }
}

/// Names the task being polled. Call it from the task itself, in its first poll.
pub fn name_current(name: &'static str) {
    #[cfg(feature = "accounting")]
    with_state(|s| {
        if let Some(i) = s.current {
            s.tasks[i].name = name;
        }
    });
    #[cfg(not(feature = "accounting"))]
    let _ = name;
}

/// All accounts of the core this runs on, now.
pub fn snapshot() -> Snapshot {
    let idle = IDLE[core()].load();
    #[cfg(feature = "accounting")]
    return with_state(|s| Snapshot {
        tasks: s.tasks,
        busy: s.busy,
        at: extend(s, mcycle()),
        idle,
    });
    #[cfg(not(feature = "accounting"))]
    Snapshot {
        tasks: [NO_ACCOUNT; SLOTS],
        busy: 0,
        at: u64::from(mcycle()),
        idle,
    }
}

/// The accounts of core `c` as its own hooks last left them (its counter is not readable from
/// here: `at` is its cycle count at its last hook, at most one executor pass old).
pub fn snapshot_of(c: usize) -> Snapshot {
    let idle = IDLE[c].load();
    #[cfg(feature = "accounting")]
    return critical_section::with(|cs| {
        let s = &STATE.borrow_ref(cs)[c];
        Snapshot {
            tasks: s.tasks,
            busy: s.busy,
            at: s.high | u64::from(s.last),
            idle,
        }
    });
    #[cfg(not(feature = "accounting"))]
    Snapshot {
        tasks: [NO_ACCOUNT; SLOTS],
        busy: 0,
        at: 0,
        idle,
    }
}

#[cfg(feature = "accounting")]
struct State {
    tasks: [Account; SLOTS],
    current: Option<usize>,
    poll_start: u32,
    exec_start: u32,
    busy: u64,
    // 64-bit extension of `mcycle` for snapshots.
    last: u32,
    high: u64,
}

#[cfg(feature = "accounting")]
const IDLE_STATE: State = State {
    tasks: [NO_ACCOUNT; SLOTS],
    current: None,
    poll_start: 0,
    exec_start: 0,
    busy: 0,
    last: 0,
    high: 0,
};

#[cfg(feature = "accounting")]
static STATE: Mutex<RefCell<[State; CORES]>> = Mutex::new(RefCell::new([IDLE_STATE; CORES]));

/// The accounts of the core this runs on.
#[cfg(feature = "accounting")]
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let c = core();
    critical_section::with(|cs| f(&mut STATE.borrow_ref_mut(cs)[c]))
}

#[cfg(feature = "accounting")]
fn extend(s: &mut State, now: u32) -> u64 {
    if now < s.last {
        s.high += 1 << 32;
    }
    s.last = now;
    s.high | u64::from(now)
}

#[cfg(feature = "accounting")]
fn slot(s: &mut State, id: u32) -> usize {
    if let Some(i) = s.tasks.iter().position(|a| a.id == id) {
        return i;
    }
    match s.tasks.iter().position(|a| a.id == 0) {
        Some(i) => {
            s.tasks[i].id = id;
            i
        }
        None => SLOTS - 1,
    }
}

#[cfg(feature = "accounting")]
#[unsafe(no_mangle)]
fn _embassy_trace_poll_start(_executor_id: u32) {
    let now = mcycle();
    let c = core();
    critical_section::with(|cs| {
        let s = &mut STATE.borrow_ref_mut(cs)[c];
        s.poll_start = now;
        extend(s, now);
        irq::pass_start(c, now);
    });
}

#[cfg(feature = "accounting")]
#[unsafe(no_mangle)]
fn _embassy_trace_executor_idle(_executor_id: u32) {
    let now = mcycle();
    let c = core();
    critical_section::with(|cs| {
        let s = &mut STATE.borrow_ref_mut(cs)[c];
        s.busy += u64::from(now.wrapping_sub(s.poll_start));
        extend(s, now);
        irq::pass_end(c, now);
    });
}

#[cfg(feature = "accounting")]
#[unsafe(no_mangle)]
fn _embassy_trace_task_exec_begin(_executor_id: u32, task_id: u32) {
    let now = mcycle();
    with_state(|s| {
        let i = slot(s, task_id);
        s.current = Some(i);
        s.exec_start = now;
    });
}

#[cfg(feature = "accounting")]
#[unsafe(no_mangle)]
fn _embassy_trace_task_exec_end(_executor_id: u32, _task_id: u32) {
    let now = mcycle();
    with_state(|s| {
        if let Some(i) = s.current.take() {
            let d = now.wrapping_sub(s.exec_start);
            s.tasks[i].cycles += u64::from(d);
            s.tasks[i].polls += 1;
        }
    });
}

#[cfg(feature = "accounting")]
#[unsafe(no_mangle)]
fn _embassy_trace_task_new(_executor_id: u32, _task_id: u32) {}

#[cfg(feature = "accounting")]
#[unsafe(no_mangle)]
fn _embassy_trace_task_end(_executor_id: u32, _task_id: u32) {}

#[cfg(feature = "accounting")]
#[unsafe(no_mangle)]
fn _embassy_trace_task_ready_begin(_executor_id: u32, _task_id: u32) {}
