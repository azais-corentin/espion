//! CPU time accounting: the cycle counter, and the cycles each embassy task spends being
//! polled, from the executor's `trace` hooks.
//!
//! Every poll of a task is timed with `mcycle` (from `_embassy_trace_task_exec_begin` to
//! `_embassy_trace_task_exec_end`), and every executor pass from `_embassy_trace_poll_start`
//! to `_embassy_trace_executor_idle`. Interrupts that fire during a poll count towards the
//! polled task. A task gets its name with [`name_current`] from its own first poll.
//!
//! Each core has its own accounts (the stream service runs an executor on each): every hook
//! updates the accounts of the core it runs on. The hooks take a critical section each (two
//! per poll, about 0.1 us); they are on in every build, so the measurements see the firmware as
//! it runs.

use core::cell::RefCell;

use critical_section::Mutex;

/// Most tasks the table tracks; later ones are summed in the last row.
const SLOTS: usize = 10;

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

/// Snapshot of all accounts.
#[derive(Clone, Copy)]
pub struct Snapshot {
    pub tasks: [Account; SLOTS],
    /// Cycles from each executor pass's start to its idle.
    pub busy: u64,
    /// Cycle counter at the snapshot.
    pub at: u64,
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
        out
    }
}

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

/// Cores of the P4's HP cluster.
pub const CORES: usize = 2;

const IDLE: State = State {
    tasks: [Account {
        id: 0,
        name: "?",
        cycles: 0,
        polls: 0,
    }; SLOTS],
    current: None,
    poll_start: 0,
    exec_start: 0,
    busy: 0,
    last: 0,
    high: 0,
};

static STATE: Mutex<RefCell<[State; CORES]>> = Mutex::new(RefCell::new([IDLE; CORES]));

/// The core this runs on.
fn core() -> usize {
    esp_hal::system::Cpu::current() as usize
}

/// The accounts of the core this runs on.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let c = core();
    critical_section::with(|cs| f(&mut STATE.borrow_ref_mut(cs)[c]))
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

/// Names the task being polled. Call it from the task itself, in its first poll.
pub fn name_current(name: &'static str) {
    with_state(|s| {
        if let Some(i) = s.current {
            s.tasks[i].name = name;
        }
    });
}

/// All accounts of the core this runs on, now.
pub fn snapshot() -> Snapshot {
    with_state(|s| {
        let at = extend(s, mcycle());
        Snapshot {
            tasks: s.tasks,
            busy: s.busy,
            at,
        }
    })
}

/// The accounts of core `c` as its own hooks last left them (its counter is not readable from
/// here: `at` is its cycle count at its last hook, at most one executor pass old).
pub fn snapshot_of(c: usize) -> Snapshot {
    critical_section::with(|cs| {
        let s = &STATE.borrow_ref(cs)[c];
        Snapshot {
            tasks: s.tasks,
            busy: s.busy,
            at: s.high | u64::from(s.last),
        }
    })
}

fn extend(s: &mut State, now: u32) -> u64 {
    if now < s.last {
        s.high += 1 << 32;
    }
    s.last = now;
    s.high | u64::from(now)
}

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

#[unsafe(no_mangle)]
fn _embassy_trace_poll_start(_executor_id: u32) {
    let now = mcycle();
    with_state(|s| {
        s.poll_start = now;
        extend(s, now);
    });
}

#[unsafe(no_mangle)]
fn _embassy_trace_executor_idle(_executor_id: u32) {
    let now = mcycle();
    with_state(|s| {
        s.busy += u64::from(now.wrapping_sub(s.poll_start));
        extend(s, now);
    });
}

#[unsafe(no_mangle)]
fn _embassy_trace_task_exec_begin(_executor_id: u32, task_id: u32) {
    let now = mcycle();
    with_state(|s| {
        let i = slot(s, task_id);
        s.current = Some(i);
        s.exec_start = now;
    });
}

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

#[unsafe(no_mangle)]
fn _embassy_trace_task_new(_executor_id: u32, _task_id: u32) {}

#[unsafe(no_mangle)]
fn _embassy_trace_task_end(_executor_id: u32, _task_id: u32) {}

#[unsafe(no_mangle)]
fn _embassy_trace_task_ready_begin(_executor_id: u32, _task_id: u32) {}
