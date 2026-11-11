//! The blob detector alone on the second HP core, started bare (performance plan step 5.3,
//! shape B of the two-core analysis). **Not run on the board yet.**
//!
//! # How core 1 starts (esp-hal 1.2.2, `system/multi_core.rs`, `soc/esp32p4/cpu_control.rs`)
//!
//! - [`start`] calls `CpuControl::start_app_core` with a 16 KiB static [`Stack`]. esp-hal moves
//!   the closure to the bottom of that stack, enables core 1's clock, releases its reset and
//!   hands the ROM the boot address `start_core1_init`.
//! - There core 1 sets `gp`, turns the FPU on (`mstatus.FS`, `fscsr`), loads `sp` from the
//!   stack top, enables the branch predictor (`mhcr`), clears the ROM boot address, writes
//!   the stack guard word 60 bytes above the stack bottom and arms a data watchpoint on it
//!   (`stack-guard-monitoring` is on by default), then `init_vectoring`: `mtvec` = the
//!   shared `_vector_table` in CLIC mode, and the CLIC lines of core 1 enabled per priority.
//!   No peripheral interrupt is mapped to core 1 (every driver binds on core 0), and nothing
//!   there sets `mstatus.MIE`.
//! - Then it runs the closure; if the closure returned, core 1 would park itself (PMU stall)
//!   in a loop. The closure here never returns.
//! - The returned `AppCoreGuard` parks core 1 when dropped. Parking a core that holds the
//!   `esp32p4-cache` lock would leave that lock taken for ever, so the guard lives in a
//!   `static` ([`Core1`]) and is never dropped.
//!
//! # What the entry does before the loop
//!
//! 1. Clears `mstatus.MIE` once, whatever the ROM left: no interrupt is ever taken on core 1.
//! 2. Points `mtvec` at [`core1_trap`] (keeping the CLIC mode bits), its own trap vector. The
//!    default path for an exception is `_vector_table` → `_start_trap` → esp-hal's
//!    `ExceptionHandler`, which `panic!`s; esp-backtrace then formats and prints through
//!    `esp-println` (a lock shared with core 0, 2 Mbaud, interrupts masked on the printing
//!    core) and loops. Worse, on a stack pointer below `_dram_data_start` the entry code of
//!    `esp-riscv-rt` moves `sp` to `_stack_start`, the top of core 0's main stack.
//!    [`core1_trap`] instead stores `mcause`, `mepc` and `mtval` in [`TRAP`], uses no stack,
//!    and waits for ever; core 0 reports it.
//! 3. `esp32p4_perf::init()`: the event counters are per core and count nothing until then.
//! 4. PIE on core 1 if [`CORE1_SKIP`] asks for it: `Pie::enable` reads the chip revision from
//!    eFuse (a read-only register, through the PAC), reads `mhartid`, swaps this core's flag
//!    in the crate's `IN_USE` table and writes this core's PIE status CSR (0x7F2): no lock, no
//!    logging, no esp-hal call. The token is held for ever, by the loop, the only code that
//!    runs on core 1 (no interrupt, no other thread), so the PIE rule of one user per core
//!    holds. If it refuses, core 1 runs the scalar skip; [`skip_used`] tells core 0.
//!
//! # The loop
//!
//! Take a new [`StripReader`] from the mailbox if core 0 left one; poll; on a strip, run the
//! detector on its rows ([`detect_rows`], the only detector call: `push_rows_with` with the
//! skip chosen at the entry), release it, and at the end of a frame publish a [`FrameResult`]
//! in the queue. Between empty polls it waits [`POLL_GAP_CYCLES`]. It increments
//! [`HEARTBEAT`] every turn.
//!
//! What core 1 may call: atomics, `mcycle`/`minstret`, the strip reader (no critical
//! section, no esp-hal driver call, see `esp32p4-csi`'s README), `esp32p4-cache` through the
//! reader, `esp32p4-perf`, and the detector. Never `critical_section`, `log`/`esp-println`,
//! esp-rtos, embassy or an esp-hal driver.
//!
//! # A trap inside the cache lock
//!
//! If core 1 traps while it holds the `esp32p4-cache` lock (inside a strip invalidate), the
//! lock stays taken and core 0's next cache operation waits for ever. Core 0 logs the trap
//! before it does any cache operation, so the record still comes out.

use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use esp_hal::peripherals::CPU_CTRL;
use esp_hal::system::{AppCoreGuard, CpuControl, Stack};
use esp32p4_csi::{StripReader, StripRef};
use esp32p4_perf::CpuSample;
use espion_vision::{Blob, DefaultDetector, Scalar, Skip, Stats};
use static_cell::{ConstStaticCell, StaticCell};

use crate::SkipChoice;
use crate::spsc::{Consumer, Producer, Spsc};

/// The dark skip of the detector on core 1: [`SkipChoice::Pie`] (PIE enabled on core 1 by its
/// entry; the scalar skip if `Pie::enable` refuses) or [`SkipChoice::Scalar`].
const CORE1_SKIP: SkipChoice = SkipChoice::Pie;

/// Core 1's stack. The detector state and the result slots are statics, so the loop needs
/// little: 16 KiB leaves room for the watchpoint's guard word and any detector frame.
const STACK_BYTES: usize = 16 * 1024;
/// Blobs per frame result: the detector's own capacity (`DefaultDetector`).
pub const MAX_BLOBS: usize = 256;
/// Frame results in flight: a few frames (33 ms at 120 fps) of slack for core 0.
const QUEUE_FRAMES: usize = 4;
/// Cycles core 1 waits between two polls that found nothing: 10 us at 400 MHz, a sixtieth of
/// a 1280x800 strip (0.64 ms). Polls share the L2MEM bus with the DMA.
const POLL_GAP_CYCLES: u32 = 4_000;

static STACK: ConstStaticCell<Stack<STACK_BYTES>> = ConstStaticCell::new(Stack::new());
static CORE1: StaticCell<Core1> = StaticCell::new();

/// One finished frame, from core 1 to core 0.
pub struct FrameResult {
    /// The strip path's frame number.
    pub frame: u32,
    /// Strips of the frame fed to the detector.
    pub strips: u16,
    /// Strips consumed more than one strip period after the reader took them (the detector
    /// fell behind the DMA there).
    pub late_strips: u16,
    /// Valid entries of `blobs`.
    pub blob_count: u16,
    /// The detector's counters for the frame.
    pub stats: Stats,
    /// Core-1 cycles and instructions in the detector (rows and `finish`).
    pub cycles: u32,
    pub instructions: u32,
    /// Longest time from a strip's `queued_at_cycles` (the reader took the block) to the end
    /// of its detector call, in cycles.
    pub worst_latency: u32,
    pub blobs: [Blob; MAX_BLOBS],
}

impl FrameResult {
    const EMPTY: Self = Self {
        frame: 0,
        strips: 0,
        late_strips: 0,
        blob_count: 0,
        stats: Stats {
            pixels: 0,
            runs: 0,
            blobs: 0,
            overflow: 0,
        },
        cycles: 0,
        instructions: 0,
        worst_latency: 0,
        blobs: [Blob {
            x: 0,
            y: 0,
            area: 0,
            peak: 0,
        }; MAX_BLOBS],
    };
}

/// Frame results, core 1 → core 0. Overwrite-never: a full queue counts a drop.
static RESULTS: Spsc<FrameResult, QUEUE_FRAMES> =
    Spsc::new([const { UnsafeCell::new(FrameResult::EMPTY) }; QUEUE_FRAMES]);

/// Writer: core 1, once per turn of its loop. Core 0 reports core 1 stalled when it stops.
pub static HEARTBEAT: AtomicU32 = AtomicU32::new(0);
/// Writer: core 1. Frames begun but not published: a strip released `false` (lapped, or the
/// run stopped), or a new frame began before the last strip.
pub static ABANDONED: AtomicU32 = AtomicU32::new(0);
/// Writer: core 1, once, at its entry: which skip the detector runs with ([`SkipUsed`] as
/// `u32`; 0 until core 1 got there).
static SKIP_USED: AtomicU32 = AtomicU32::new(0);

/// The skip core 1 runs with, as [`skip_used`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipUsed {
    /// Core 1 has not reached its loop yet.
    Unknown,
    Scalar,
    Pie,
    /// [`CORE1_SKIP`] asked for PIE and `Pie::enable` refused: the scalar skip.
    ScalarPieRefused,
}

const SKIP_SCALAR: u32 = 1;
const SKIP_PIE: u32 = 2;
const SKIP_REFUSED: u32 = 3;

/// Trap record. Writer: [`core1_trap`] only (words 1 to 3 first, then word 0).
#[repr(C)]
pub struct TrapRecord([AtomicU32; 4]);

/// Word 0: 1 once core 1 trapped; 1: `mcause`; 2: `mepc`; 3: `mtval`.
pub static TRAP: TrapRecord = TrapRecord([const { AtomicU32::new(0) }; 4]);

/// Run parameters. Writer: core 0 ([`Core1::set_params`]), only while the strip path is
/// stopped; the reader's start publishes them to core 1 (they are stored before the start's
/// SeqCst run number, which core 1 loads before it uses them).
/// Width (bits 0..16), threshold (16..26), RAW10 (bit 31).
static PARAM_FORMAT: AtomicU32 = AtomicU32::new(0);
/// A strip consumed later than this many cycles after the reader took it counts as late.
static PARAM_LATE: AtomicU32 = AtomicU32::new(u32::MAX);

/// Hands [`StripReader`]s from core 0 to core 1 in place (a reader is 304 bytes; core 1 uses
/// it where it lies, it is never moved through a stack there). Two slots: core 0 writes the
/// next reader into the slot core 1 is not using.
struct Mailbox {
    /// Writer: core 0. Readers posted so far; the newest lies in slot `posted % 2`.
    posted: AtomicU32,
    /// Writer: core 1. The `posted` value of the reader it switched to last.
    taken: AtomicU32,
    slots: [UnsafeCell<MaybeUninit<StripReader>>; 2],
}

// SAFETY: core 0 writes slot `(posted + 1) % 2` only while `taken == posted`, so core 1 then
// uses the other slot, and publishes the write with Release on `posted`; core 1 switches to a
// slot only after loading `posted` with Acquire, and then leaves the old slot for good.
unsafe impl Sync for Mailbox {}

static MAILBOX: Mailbox = Mailbox {
    posted: AtomicU32::new(0),
    taken: AtomicU32::new(0),
    slots: [const { UnsafeCell::new(MaybeUninit::uninit()) }; 2],
};

// ── Core 0 ──────────────────────────────────────────────────────────────────────────────

/// Core 0's end: the result queue, the mailbox and the parameters. Lives in a `static` with
/// the guard that keeps core 1 running.
pub struct Core1 {
    results: Consumer<FrameResult, QUEUE_FRAMES>,
    _guard: AppCoreGuard<'static>,
}

/// What [`core1_trap`] recorded.
#[derive(Clone, Copy, Debug)]
pub struct Trap {
    pub mcause: u32,
    pub mepc: u32,
    pub mtval: u32,
}

/// Starts core 1 with `det` and `scratch` (where a frame's blobs go when the queue is full).
/// Call once; `None` if core 1 runs already.
pub fn start(
    cpu: CPU_CTRL<'static>,
    det: &'static mut DefaultDetector,
    scratch: &'static mut [Blob; MAX_BLOBS],
) -> Option<&'static mut Core1> {
    let (producer, results) = RESULTS.split()?;
    let stack = STACK.try_take()?;
    let guard = CpuControl::new(cpu)
        .start_app_core(stack, move || entry(det, scratch, producer))
        .ok()?;
    Some(CORE1.init(Core1 {
        results,
        _guard: guard,
    }))
}

impl Core1 {
    /// Core 1 has switched to the last reader posted, so another one can be posted.
    pub fn can_post(&self) -> bool {
        MAILBOX.taken.load(Ordering::Acquire) == MAILBOX.posted.load(Ordering::Relaxed)
    }

    /// Leaves a copy of `reader` for core 1, which abandons the reader it had and starts
    /// afresh with this one; the caller must drop `reader` without using it again. By
    /// reference so that the 304-byte reader is copied once, straight into the mailbox.
    /// Only after [`Core1::can_post`]; otherwise nothing happens and `false` is returned.
    pub fn post(&mut self, reader: &StripReader) -> bool {
        // A bitwise copy stands for a move only if dropping the original does nothing.
        const _: () = assert!(!core::mem::needs_drop::<StripReader>());
        if !self.can_post() {
            return false;
        }
        let next = MAILBOX.posted.load(Ordering::Relaxed).wrapping_add(1);
        // SAFETY: core 1 uses slot `posted % 2` (it took `posted`), not this one. The copy
        // replaces a move: the caller drops `reader` unused, and a reader has no drop glue.
        unsafe {
            ptr::copy_nonoverlapping(
                reader,
                (*MAILBOX.slots[next as usize % 2].get()).as_mut_ptr(),
                1,
            );
        }
        MAILBOX.posted.store(next, Ordering::Release);
        true
    }

    /// The detector set-up for the next run: RAW8 or RAW10 rows of `width` pixels,
    /// `threshold` (8 or 10 bits), and the latency over which a strip counts as late. Only
    /// while the strip path is stopped.
    pub fn set_params(&mut self, width: u16, raw10: bool, threshold: u16, late_cycles: u32) {
        let format = u32::from(width) | u32::from(threshold & 0x3FF) << 16 | u32::from(raw10) << 31;
        PARAM_FORMAT.store(format, Ordering::Release);
        PARAM_LATE.store(late_cycles, Ordering::Release);
    }

    /// Hands the oldest frame result to `read`; `None` if there is none.
    pub fn pop_with<R>(&mut self, read: impl FnOnce(&FrameResult) -> R) -> Option<R> {
        self.results.pop_with(read)
    }

    /// Frame results core 1 dropped because the queue was full.
    pub fn dropped(&self) -> u32 {
        self.results.dropped()
    }
}

/// Core 1's heartbeat (turns of its loop).
pub fn heartbeat() -> u32 {
    HEARTBEAT.load(Ordering::Relaxed)
}

/// Frames core 1 abandoned.
pub fn abandoned() -> u32 {
    ABANDONED.load(Ordering::Relaxed)
}

/// The skip core 1 runs the detector with.
pub fn skip_used() -> SkipUsed {
    match SKIP_USED.load(Ordering::Acquire) {
        SKIP_SCALAR => SkipUsed::Scalar,
        SKIP_PIE => SkipUsed::Pie,
        SKIP_REFUSED => SkipUsed::ScalarPieRefused,
        _ => SkipUsed::Unknown,
    }
}

/// The trap core 1 took, if any.
pub fn trap() -> Option<Trap> {
    if TRAP.0[0].load(Ordering::Acquire) == 0 {
        return None;
    }
    Some(Trap {
        mcause: TRAP.0[1].load(Ordering::Relaxed),
        mepc: TRAP.0[2].load(Ordering::Relaxed),
        mtval: TRAP.0[3].load(Ordering::Relaxed),
    })
}

// ── Core 1 ──────────────────────────────────────────────────────────────────────────────

// Core 1's trap vector: no stack, no lock, no return. 256-byte aligned: in CLIC mode every
// exception enters at `mtvec` with its low 6 bits cleared. In RAM (`.rwtext`), like esp-hal's
// own vector table, so a flash or cache fault cannot keep it from running.
global_asm!(
    ".section .rwtext.core1_trap, \"ax\"",
    ".balign 256",
    ".global core1_trap",
    ".type core1_trap, @function",
    "core1_trap:",
    ".option push",
    ".option norelax",
    "la t0, {rec}",
    ".option pop",
    "csrr t1, mcause",
    "sw t1, 4(t0)",
    "csrr t1, mepc",
    "sw t1, 8(t0)",
    "csrr t1, mtval",
    "sw t1, 12(t0)",
    "fence rw, rw",
    "li t1, 1",
    "sw t1, 0(t0)",
    "1:",
    "wfi",
    "j 1b",
    ".size core1_trap, . - core1_trap",
    rec = sym TRAP,
);

unsafe extern "C" {
    /// The trap vector above.
    fn core1_trap() -> !;
}

/// Core 1's entry: interrupts off, its own trap vector, the counters, PIE, then the loop.
fn entry(
    det: &'static mut DefaultDetector,
    scratch: &'static mut [Blob; MAX_BLOBS],
    out: Producer<FrameResult, QUEUE_FRAMES>,
) {
    // SAFETY: machine mode on core 1. Clearing MIE only keeps interrupts away; setting
    // `mtvec` changes this core's trap entry only (the CSR is per hart), keeping its mode
    // bits.
    unsafe {
        asm!("csrci mstatus, 8", options(nomem, nostack));
        let old: usize;
        asm!("csrr {0}, mtvec", out(reg) old, options(nomem, nostack));
        let new = (core1_trap as *const () as usize & !0x3F) | (old & 0x3F);
        asm!("csrw mtvec, {0}", in(reg) new, options(nomem, nostack));
    }
    esp32p4_perf::init();
    let pie = match CORE1_SKIP {
        SkipChoice::Pie => esp32p4_pie::Pie::enable().ok(),
        SkipChoice::Scalar => None,
    };
    match &pie {
        Some(p) => {
            SKIP_USED.store(SKIP_PIE, Ordering::Release);
            run(p, det, scratch, out)
        }
        None => {
            let used = match CORE1_SKIP {
                SkipChoice::Pie => SKIP_REFUSED,
                SkipChoice::Scalar => SKIP_SCALAR,
            };
            SKIP_USED.store(used, Ordering::Release);
            run(&Scalar, det, scratch, out)
        }
    }
}

/// The frame being assembled.
struct Open {
    frame: u32,
    strips: u16,
    late: u16,
    cycles: u32,
    instructions: u32,
    worst: u32,
}

/// Core 1's loop with the dark skip `skip`. Never returns.
#[inline(never)]
fn run<S: Skip>(
    skip: &S,
    det: &mut DefaultDetector,
    scratch: &mut [Blob; MAX_BLOBS],
    mut out: Producer<FrameResult, QUEUE_FRAMES>,
) -> ! {
    let mut reader: Option<&'static mut StripReader> = None;
    let mut current = 0;
    let mut open: Option<Open> = None;
    loop {
        HEARTBEAT.fetch_add(1, Ordering::Relaxed);
        let posted = MAILBOX.posted.load(Ordering::Acquire);
        if posted != current {
            current = posted;
            // SAFETY: core 0 wrote this slot before publishing `posted` (Acquire above) and
            // writes only the other one until `taken` says this one was taken; from here on
            // core 1 never touches the slot it used before.
            reader = Some(unsafe { (*MAILBOX.slots[posted as usize % 2].get()).assume_init_mut() });
            MAILBOX.taken.store(posted, Ordering::Release);
            if open.take().is_some() {
                ABANDONED.fetch_add(1, Ordering::Relaxed);
            }
        }
        let strip = match reader.as_mut() {
            Some(r) => r.poll(),
            None => None,
        };
        match strip {
            Some(s) => on_strip(skip, det, scratch, &mut out, &mut open, s),
            None => pause(),
        }
    }
}

/// Waits [`POLL_GAP_CYCLES`].
fn pause() {
    let t0 = esp32p4_perf::cycles();
    while esp32p4_perf::cycles().wrapping_sub(t0) < POLL_GAP_CYCLES {
        core::hint::spin_loop();
    }
}

/// One strip: begin a frame on row 0, the detector on the rows, the release check, and the
/// frame result after the last strip.
fn on_strip<S: Skip>(
    skip: &S,
    det: &mut DefaultDetector,
    scratch: &mut [Blob; MAX_BLOBS],
    out: &mut Producer<FrameResult, QUEUE_FRAMES>,
    open: &mut Option<Open>,
    s: StripRef<'_>,
) {
    if s.first_row() == 0 {
        if open.is_some() {
            ABANDONED.fetch_add(1, Ordering::Relaxed);
        }
        let format = PARAM_FORMAT.load(Ordering::Acquire);
        let (width, threshold) = (format as u16, (format >> 16) as u16 & 0x3FF);
        if format >> 31 != 0 {
            det.begin_frame_raw10(width, threshold);
        } else {
            det.begin_frame(width, threshold as u8);
        }
        *open = Some(Open {
            frame: s.frame(),
            strips: 0,
            late: 0,
            cycles: 0,
            instructions: 0,
            worst: 0,
        });
    }
    let o = match open {
        Some(o) if o.frame == s.frame() => o,
        _ => {
            // Not part of a frame begun here: give it back unused.
            let _ = s.release();
            return;
        }
    };
    let a = CpuSample::now();
    detect_rows(skip, det, &s, s.first_row());
    let d = CpuSample::now().since(&a);
    let latency = esp32p4_perf::cycles().wrapping_sub(s.queued_at_cycles());
    o.strips = o.strips.wrapping_add(1);
    o.cycles = o.cycles.wrapping_add(d.cycles);
    o.instructions = o.instructions.wrapping_add(d.instructions);
    o.worst = o.worst.max(latency);
    if latency > PARAM_LATE.load(Ordering::Relaxed) {
        o.late = o.late.wrapping_add(1);
    }
    let last = s.is_last();
    if !s.release() {
        // Lapped or stopped: what the detector read may not be this frame's.
        *open = None;
        ABANDONED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if !last {
        return;
    }
    let Some(o) = open.take() else {
        return;
    };
    let a = CpuSample::now();
    let queued = out.push_with(|r| {
        r.blob_count = det.finish(&mut r.blobs) as u16;
        let d = CpuSample::now().since(&a);
        r.frame = o.frame;
        r.strips = o.strips;
        r.late_strips = o.late;
        r.stats = det.stats();
        r.cycles = o.cycles.wrapping_add(d.cycles);
        r.instructions = o.instructions.wrapping_add(d.instructions);
        r.worst_latency = o.worst;
    });
    if !queued {
        // Counted as dropped by the queue; the detector still has to close the frame.
        det.finish(scratch);
    }
}

/// The detector on one strip's rows with the dark skip `skip`: the only detector call of the
/// runner.
#[inline(always)]
fn detect_rows<S: Skip>(skip: &S, det: &mut DefaultDetector, rows: &[u8], first_row: u16) {
    det.push_rows_with(skip, rows, first_row);
}
