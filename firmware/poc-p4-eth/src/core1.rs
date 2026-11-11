//! The blob detector alone on the second HP core, started bare (performance plan step 5.3,
//! shape B of the two-core analysis). Board results: `README.md`, "Detector on core 1".
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
//! Take a new [`StripReader`] from the mailbox if core 0 left one; answer a clock request
//! ([`Core1::clock_offset`]); poll; on a strip, do the run's [`Job`], release the strip.
//! Between empty polls it waits [`POLL_GAP_CYCLES`]. It increments [`HEARTBEAT`] every turn.
//!
//! - [`Job::Detect`]: the detector on the strip's rows ([`detect_rows`], the only detector
//!   call: `push_rows_with` with the skip chosen at the entry), or on a synthetic strip core 0
//!   prepared, with the L1 data autoload over the strip if the run asks for it; at the end of
//!   a frame a [`FrameResult`] in the queue.
//! - [`Job::Check`]: every row's checksum against the reference row checksums core 0 took
//!   from whole frames of the sensor's random test pattern, the strip sequence, and the test
//!   holds of the lap and stall tests; counters in [`CHECK`] and [`CHECK_MAX`].
//!
//! What core 1 may call: atomics, `mcycle`/`minstret`, the strip reader (no critical
//! section, no esp-hal driver call, see `esp32p4-csi`'s README), `esp32p4-cache` (the reader's
//! invalidate, the flush of a synthetic strip and the autoload registers), `esp32p4-perf`,
//! and the detector. Never `critical_section`, `log`/`esp-println`, esp-rtos, embassy or an
//! esp-hal driver.
//!
//! # L1 data autoload on core 1
//!
//! The autoload registers exist once for the chip (the L1 data cache is shared by both
//! cores), so one context owns them: here core 1, and core 0 never touches them while core 1
//! runs. The section is the strip being read and nothing else, turned off (waiting until no
//! fetch is in flight) before the next poll, as on core 0 (`strip_soak`).
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
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use esp_hal::peripherals::CPU_CTRL;
use esp_hal::system::{AppCoreGuard, CpuControl, Stack};
use esp32p4_cache::prefetch::{self, Autoload, Level, Order, Section, Trigger};
use esp32p4_csi::{StripReader, StripRef};
use esp32p4_perf::{CpuSample, cycles};
use espion_vision::{Blob, DefaultDetector, Scalar, Skip, Stats};
use static_cell::{ConstStaticCell, StaticCell};

use crate::SkipChoice;
use crate::spsc::{Consumer, Producer, Spsc};
use crate::strip_soak::row_sum;

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
    /// Longest time from a strip's `queued_at_cycles` to the start of its detector call.
    pub start_latency: u32,
    /// From the last strip's `queued_at_cycles` to the publication of this result.
    pub ready: u32,
    /// Core 1's `mcycle` at the publication (see [`Core1::clock_offset`]).
    pub pushed_at: u32,
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
        start_latency: 0,
        ready: 0,
        pushed_at: 0,
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

/// What core 1 does with each strip of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    /// The blob detector, a [`FrameResult`] per frame.
    Detect,
    /// Row checksums against a reference, the strip sequence and the test holds.
    Check,
}

/// Run parameters. Writer: core 0 ([`Core1::set_run`]), only while the strip path is
/// stopped; the reader's start publishes them to core 1 (they are stored before the start's
/// SeqCst run number, which core 1 loads before it uses them).
struct Params {
    /// 0 [`Job::Detect`], 1 [`Job::Check`].
    job: AtomicU32,
    /// Width (bits 0..16), threshold (16..26), RAW10 (bit 31).
    format: AtomicU32,
    /// A strip consumed later than this many cycles after the reader took it counts as late.
    late: AtomicU32,
    /// 1: L1 data autoload (miss and hit) over each strip the detector reads.
    autoload: AtomicU32,
    /// Synthetic strip the detector reads instead of each strip (address, bytes; 0: live).
    synth: AtomicUsize,
    synth_len: AtomicUsize,
    /// Reference row checksums of the check (address of `ref_rows` words).
    reference: AtomicUsize,
    ref_rows: AtomicU32,
    /// Bytes per row (check).
    line_bytes: AtomicU32,
    /// Test hold (check): the strip starting at row `hold_row` of every `hold_every`-th frame
    /// is kept until `hold` cycles after the reader took it (0: no hold).
    hold: AtomicU32,
    hold_row: AtomicU32,
    hold_every: AtomicU32,
    /// The ring (diagnostics of the test holds): strip buffers, list items, bytes per strip
    /// and strips.
    ring: AtomicUsize,
    links: AtomicUsize,
    strip_bytes: AtomicU32,
    count: AtomicU32,
}

static PARAMS: Params = Params {
    job: AtomicU32::new(0),
    format: AtomicU32::new(0),
    late: AtomicU32::new(u32::MAX),
    autoload: AtomicU32::new(0),
    synth: AtomicUsize::new(0),
    synth_len: AtomicUsize::new(0),
    reference: AtomicUsize::new(0),
    ref_rows: AtomicU32::new(0),
    line_bytes: AtomicU32::new(0),
    hold: AtomicU32::new(0),
    hold_row: AtomicU32::new(0),
    hold_every: AtomicU32::new(1),
    ring: AtomicUsize::new(0),
    links: AtomicUsize::new(0),
    strip_bytes: AtomicU32::new(0),
    count: AtomicU32::new(0),
};

/// Writer: core 0 ([`Core1::note_stop`]), right after each `StripControl::stop` returns.
/// The check compares it before a strip's checks and before its release: a strip whose stop
/// count changed in between belongs to a run that had ended before its release began.
static STOPS: AtomicU32 = AtomicU32::new(0);

/// Clock request: core 0 raises `SYNC_REQ`; core 1 stores its `mcycle` in `SYNC_CYCLES` and
/// then copies `SYNC_REQ` into `SYNC_ACK`.
static SYNC_REQ: AtomicU32 = AtomicU32::new(0);
static SYNC_ACK: AtomicU32 = AtomicU32::new(0);
static SYNC_CYCLES: AtomicU32 = AtomicU32::new(0);

macro_rules! counters {
    ($(#[$m:meta])* $name:ident, $snap:ident { $($(#[$fm:meta])* $f:ident),* $(,)? }) => {
        $(#[$m])*
        pub struct $name { $($(#[$fm])* $f: AtomicU32,)* }
        /// A copy of the counters.
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $snap { $($(#[$fm])* pub $f: u32,)* }
        impl $name {
            const fn new() -> Self {
                Self { $($f: AtomicU32::new(0),)* }
            }
            /// The counters now.
            pub fn snap(&self) -> $snap {
                $snap { $($f: self.$f.load(Ordering::Relaxed),)* }
            }
        }
        impl $snap {
            /// Field by field `self - earlier`.
            #[allow(dead_code, reason = "used for the counters, not the maxima")]
            pub fn since(&self, earlier: &Self) -> Self {
                Self { $($f: self.$f.wrapping_sub(earlier.$f),)* }
            }
        }
    };
}

counters!(
    /// The check's counters (writer: core 1; they only grow).
    CheckCounters, CheckSnap {
    /// Strips released `true` and `false`.
    kept,
    dropped,
    /// Rows checked and rows unlike the reference, of strips released `true`.
    rows,
    rows_bad,
    /// Rows unlike the reference in strips released `false` (written over, not an error).
    rows_bad_dropped,
    /// Strips not where the frame is (row, frame, end of frame).
    seq_errors,
    /// Strips whose length is not their rows times the row bytes.
    size_errors,
    /// Frames whose every strip came in order and released `true`, and those of them whose
    /// every row matched.
    frames,
    frames_good,
    /// Test holds, by (bytes changed during the hold, release result). `changed_kept` must
    /// stay 0: a changed strip released as good.
    held,
    held_changed_kept,
    held_changed_dropped,
    held_same_dropped,
    held_same_kept,
    /// Holds where the strip's first bytes changed while the done bit of the item before it
    /// was not yet visible (read after the bytes): the order the lap check relies on, broken.
    /// Must stay 0.
    order_errors,
    /// Strips whose run ended (stop) between their checks and their release: released
    /// `true` (must stay 0) and `false`.
    old_kept,
    old_dropped,
    /// Good frames ended after a test hold.
    recoveries,
});

counters!(
    /// The check's maxima (writer: core 1; core 0 zeroes them while stopped).
    CheckMaxima, CheckMaxSnap {
    /// Cycles from the end of a test hold to the end of the next good frame.
    recovery,
    /// Cycles from a strip's `queued_at_cycles` to the start of its check.
    start_latency,
    /// Cycles of one strip's check (without a hold).
    check,
    /// First bad row of a strip released `true`: frame << 12 | row, plus 1 (0: none).
    first_bad,
});

counters!(
    /// When things happened during the test holds, in cycles after the reader took the held
    /// strip `k` (writer: core 1; zeroed by [`Core1::set_run`]; 0: not seen in any hold).
    /// `doneN`: the done bit of item `k + N` (ring order) set again; `change`: the first 16
    /// bytes of `k` changed (read through the non-cacheable alias). Minimum and maximum over
    /// the holds where it was seen.
    HoldTimes, HoldTimesSnap {
    done1_min, done1_max, done2_min, done2_max, done3_min, done3_max, done0_min, done0_max,
    change_min, change_max,
});

/// The test holds' timeline.
pub static HOLD_TIMES: HoldTimes = HoldTimes::new();

/// The check's counters.
pub static CHECK: CheckCounters = CheckCounters::new();
/// The check's maxima.
pub static CHECK_MAX: CheckMaxima = CheckMaxima::new();

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

/// The parameters of a run ([`Core1::set_run`]).
#[derive(Clone, Copy)]
pub struct Run {
    pub job: Job,
    /// Row width in pixels, RAW10 or RAW8, threshold in 8 or 10 bits (detector).
    pub width: u16,
    pub raw10: bool,
    pub threshold: u16,
    /// Cycles after which a strip counts as late (detector).
    pub late_cycles: u32,
    /// L1 data autoload over each strip (detector).
    pub autoload: bool,
    /// A synthetic strip read instead of each strip, from where core 0 left it (detector).
    pub synth: Option<&'static [u8]>,
    /// Reference row checksums, one per row of the frame (check).
    pub reference: &'static [u32],
    /// Bytes per row (check).
    pub line_bytes: u32,
    /// The test hold (check): cycles after the reader's take, the strip's first row, and
    /// one frame in how many.
    pub hold: Option<(u32, u16, u32)>,
    /// The ring, for the test holds' timeline: strip buffers, list items, bytes per strip,
    /// strips.
    pub ring: (usize, usize, u32, u32),
}

/// Starts core 1 with `det` and `scratch` (where a frame's blobs go when the queue is full).
/// Call once; `None` if core 1 runs already.
#[inline(never)]
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

    /// The parameters of the next run. Only while the strip path is stopped. Zeroes the
    /// check's maxima.
    pub fn set_run(&mut self, run: &Run) {
        let p = &PARAMS;
        let format = u32::from(run.width)
            | u32::from(run.threshold & 0x3FF) << 16
            | u32::from(run.raw10) << 31;
        p.job.store(
            match run.job {
                Job::Detect => 0,
                Job::Check => 1,
            },
            Ordering::Relaxed,
        );
        p.format.store(format, Ordering::Relaxed);
        p.late.store(run.late_cycles, Ordering::Relaxed);
        p.autoload.store(u32::from(run.autoload), Ordering::Relaxed);
        let (synth, synth_len) = run.synth.map_or((0, 0), |s| (s.as_ptr() as usize, s.len()));
        p.synth.store(synth, Ordering::Relaxed);
        p.synth_len.store(synth_len, Ordering::Relaxed);
        p.reference
            .store(run.reference.as_ptr() as usize, Ordering::Relaxed);
        p.ref_rows
            .store(run.reference.len() as u32, Ordering::Relaxed);
        p.line_bytes.store(run.line_bytes, Ordering::Relaxed);
        let (hold, row, every) = run.hold.unwrap_or((0, 0, 1));
        p.hold.store(hold, Ordering::Relaxed);
        p.hold_row.store(u32::from(row), Ordering::Relaxed);
        p.hold_every.store(every.max(1), Ordering::Relaxed);
        let (strips, links, strip_bytes, count) = run.ring;
        p.ring.store(strips, Ordering::Relaxed);
        p.links.store(links, Ordering::Relaxed);
        p.strip_bytes.store(strip_bytes, Ordering::Relaxed);
        p.count.store(count, Ordering::Relaxed);
        let m = &CHECK_MAX;
        for a in [&m.recovery, &m.start_latency, &m.check, &m.first_bad] {
            a.store(0, Ordering::Relaxed);
        }
        let t = &HOLD_TIMES;
        for a in [
            &t.done1_min,
            &t.done1_max,
            &t.done2_min,
            &t.done2_max,
            &t.done3_min,
            &t.done3_max,
            &t.done0_min,
            &t.done0_max,
            &t.change_min,
            &t.change_max,
        ] {
            a.store(0, Ordering::Relaxed);
        }
    }

    /// Call right after each `StripControl::stop` (see [`STOPS`]).
    pub fn note_stop(&mut self) {
        STOPS.fetch_add(1, Ordering::Release);
    }

    /// Hands the oldest frame result to `read`; `None` if there is none.
    pub fn pop_with<R>(&mut self, read: impl FnOnce(&FrameResult) -> R) -> Option<R> {
        self.results.pop_with(read)
    }

    /// Frame results core 1 dropped because the queue was full.
    pub fn dropped(&self) -> u32 {
        self.results.dropped()
    }

    /// The offset of core 1's `mcycle` against core 0's (add it to a core-0 reading to get
    /// core 1's time) and its uncertainty (half the round trip), in cycles: the best of 16
    /// requests. Core 1 answers between polls and while it pauses. Spins until core 1 answers,
    /// at most `max_cycles` per request; `None` if it did not.
    pub fn clock_offset(&mut self, max_cycles: u32) -> Option<(u32, u32)> {
        let mut best: Option<(u32, u32)> = None;
        for _ in 0..16 {
            let (offset, half) = self.clock_offset_once(max_cycles)?;
            if best.is_none_or(|(_, h)| half < h) {
                best = Some((offset, half));
            }
        }
        best
    }

    fn clock_offset_once(&mut self, max_cycles: u32) -> Option<(u32, u32)> {
        let req = SYNC_REQ.load(Ordering::Relaxed).wrapping_add(1);
        let t0 = cycles();
        SYNC_REQ.store(req, Ordering::Release);
        while SYNC_ACK.load(Ordering::Acquire) != req {
            if cycles().wrapping_sub(t0) > max_cycles {
                return None;
            }
            core::hint::spin_loop();
        }
        let t1 = cycles();
        let c1 = SYNC_CYCLES.load(Ordering::Relaxed);
        let half = t1.wrapping_sub(t0) / 2;
        Some((c1.wrapping_sub(t0.wrapping_add(half)), half))
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

/// The frame being detected.
struct Open {
    frame: u32,
    strips: u16,
    late: u16,
    cycles: u32,
    instructions: u32,
    worst: u32,
    start: u32,
}

/// The frame being checked.
struct CheckOpen {
    frame: u32,
    next_row: u16,
    good: bool,
    /// [`STOPS`] when the frame began: a frame of an earlier run left open is not an error.
    stops: u32,
}

/// Core 1's state between strips.
struct Core1State<'a> {
    det: &'a mut DefaultDetector,
    scratch: &'a mut [Blob; MAX_BLOBS],
    out: Producer<FrameResult, QUEUE_FRAMES>,
    open: Option<Open>,
    check: Option<CheckOpen>,
    /// `mcycle` at the end of the last test hold and [`STOPS`] then, until the next good
    /// frame of the same run ends.
    hold_end: Option<(u32, u32)>,
}

/// Core 1's loop with the dark skip `skip`. Never returns.
#[inline(never)]
fn run<S: Skip>(
    skip: &S,
    det: &mut DefaultDetector,
    scratch: &mut [Blob; MAX_BLOBS],
    out: Producer<FrameResult, QUEUE_FRAMES>,
) -> ! {
    let mut reader: Option<&'static mut StripReader> = None;
    let mut current = 0;
    let mut st = Core1State {
        det,
        scratch,
        out,
        open: None,
        check: None,
        hold_end: None,
    };
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
            if st.open.take().is_some() {
                ABANDONED.fetch_add(1, Ordering::Relaxed);
            }
            st.check = None;
        }
        answer_clock();
        let strip = match reader.as_mut() {
            Some(r) => r.poll(),
            None => None,
        };
        match strip {
            Some(s) if PARAMS.job.load(Ordering::Relaxed) == 1 => check_strip(&mut st, s),
            Some(s) => on_strip(skip, &mut st, s),
            None => pause(),
        }
    }
}

/// Waits [`POLL_GAP_CYCLES`].
fn pause() {
    let t0 = cycles();
    while cycles().wrapping_sub(t0) < POLL_GAP_CYCLES {
        answer_clock();
        core::hint::spin_loop();
    }
}

/// Answers core 0's clock request ([`Core1::clock_offset`]), if there is one.
#[inline(always)]
fn answer_clock() {
    let req = SYNC_REQ.load(Ordering::Acquire);
    if req != SYNC_ACK.load(Ordering::Relaxed) {
        SYNC_CYCLES.store(cycles(), Ordering::Relaxed);
        SYNC_ACK.store(req, Ordering::Release);
    }
}

/// Adds `n` to a counter only core 1 writes.
#[inline(always)]
fn bump(a: &AtomicU32, n: u32) {
    a.store(a.load(Ordering::Relaxed).wrapping_add(n), Ordering::Relaxed);
}

/// Raises a maximum only core 1 writes (core 0 may zero it while stopped).
#[inline(always)]
fn raise(a: &AtomicU32, v: u32) {
    if v > a.load(Ordering::Relaxed) {
        a.store(v, Ordering::Relaxed);
    }
}

/// The detector input for strip `s`: the strip, or the synthetic strip of the run (written
/// back and dropped from the cache first, so it is read cold like a strip the DMA wrote).
fn detector_input(s: &[u8]) -> &[u8] {
    let addr = PARAMS.synth.load(Ordering::Relaxed);
    let len = PARAMS.synth_len.load(Ordering::Relaxed);
    if addr == 0 || len < s.len() {
        return s;
    }
    esp32p4_cache::writeback_invalidate(addr, s.len().next_multiple_of(esp32p4_cache::L1_LINE));
    // SAFETY: core 0 left `len` bytes there for the whole run and writes them only while
    // the strip path is stopped (`Core1::set_run`).
    unsafe { core::slice::from_raw_parts(addr as *const u8, s.len()) }
}

/// One strip of [`Job::Detect`]: begin a frame on row 0, the detector on the rows, the
/// release check, and the frame result after the last strip.
fn on_strip<S: Skip>(skip: &S, st: &mut Core1State<'_>, s: StripRef<'_>) {
    let det = &mut *st.det;
    if s.first_row() == 0 {
        if st.open.is_some() {
            ABANDONED.fetch_add(1, Ordering::Relaxed);
        }
        let format = PARAMS.format.load(Ordering::Acquire);
        let (width, threshold) = (format as u16, (format >> 16) as u16 & 0x3FF);
        if format >> 31 != 0 {
            det.begin_frame_raw10(width, threshold);
        } else {
            det.begin_frame(width, threshold as u8);
        }
        st.open = Some(Open {
            frame: s.frame(),
            strips: 0,
            late: 0,
            cycles: 0,
            instructions: 0,
            worst: 0,
            start: 0,
        });
    }
    let o = match &mut st.open {
        Some(o) if o.frame == s.frame() => o,
        _ => {
            // Not part of a frame begun here: give it back unused.
            let _ = s.release();
            return;
        }
    };
    let rows = detector_input(&s);
    let autoload = PARAMS.autoload.load(Ordering::Relaxed) != 0;
    let a = CpuSample::now();
    let start = a.cycles.wrapping_sub(s.queued_at_cycles());
    if autoload {
        autoload_on(rows);
    }
    detect_rows(skip, det, rows, s.first_row());
    if autoload {
        prefetch::autoload_disable(Level::L1Data);
    }
    let d = CpuSample::now().since(&a);
    let latency = cycles().wrapping_sub(s.queued_at_cycles());
    o.strips = o.strips.wrapping_add(1);
    o.cycles = o.cycles.wrapping_add(d.cycles);
    o.instructions = o.instructions.wrapping_add(d.instructions);
    o.worst = o.worst.max(latency);
    o.start = o.start.max(start);
    if latency > PARAMS.late.load(Ordering::Relaxed) {
        o.late = o.late.wrapping_add(1);
    }
    let last = s.is_last();
    let queued_at = s.queued_at_cycles();
    if !s.release() {
        // Lapped or stopped: what the detector read may not be this frame's.
        st.open = None;
        ABANDONED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if !last {
        return;
    }
    let Some(o) = st.open.take() else {
        return;
    };
    let a = CpuSample::now();
    let queued = st.out.push_with(|r| {
        r.blob_count = det.finish(&mut r.blobs) as u16;
        let d = CpuSample::now().since(&a);
        r.frame = o.frame;
        r.strips = o.strips;
        r.late_strips = o.late;
        r.stats = det.stats();
        r.cycles = o.cycles.wrapping_add(d.cycles);
        r.instructions = o.instructions.wrapping_add(d.instructions);
        r.worst_latency = o.worst;
        r.start_latency = o.start;
        let now = cycles();
        r.ready = now.wrapping_sub(queued_at);
        r.pushed_at = now;
    });
    if !queued {
        // Counted as dropped by the queue; the detector still has to close the frame.
        det.finish(st.scratch);
    }
}

/// The L1 data autoload (miss and hit, ascending) over `strip` and nothing else.
#[inline(always)]
fn autoload_on(strip: &[u8]) {
    prefetch::autoload_configure(
        Level::L1Data,
        &Autoload {
            trigger: Trigger::MissHit,
            order: Order::Ascending,
            sections: [
                Some(Section {
                    addr: strip.as_ptr() as usize,
                    len: strip.len(),
                }),
                None,
                None,
                None,
            ],
        },
    );
    prefetch::autoload_enable(Level::L1Data);
}

/// The detector on one strip's rows with the dark skip `skip`: the only detector call of the
/// runner.
#[inline(always)]
fn detect_rows<S: Skip>(skip: &S, det: &mut DefaultDetector, rows: &[u8], first_row: u16) {
    det.push_rows_with(skip, rows, first_row);
}

/// The number of `data`'s rows of `line_bytes` unlike `want` (row `first` on) and the first
/// of them.
#[inline(never)]
fn check_rows(data: &[u8], line_bytes: usize, want: &[u32], first: usize) -> (u32, Option<u16>) {
    let (mut bad, mut first_bad) = (0u32, None);
    if line_bytes == 0 {
        return (bad, first_bad);
    }
    for (i, row) in data.chunks_exact(line_bytes).enumerate() {
        if want.get(first + i) != Some(&row_sum(row)) {
            bad += 1;
            first_bad.get_or_insert((first + i) as u16);
        }
    }
    (bad, first_bad)
}

/// The test hold: spins until `hold` cycles after the reader took `s` (strip `k`), recording
/// when the other list items finish and when the strip's first 16 bytes change
/// ([`HOLD_TIMES`], read through the non-cacheable alias), and counting
/// [`CheckCounters::order_errors`]. Says whether those bytes changed during the hold; the
/// caller releases the strip right after.
#[inline(never)]
fn hold_changed(s: &StripRef<'_>, hold: u32) -> bool {
    let p = &PARAMS;
    let (ring, links) = (
        p.ring.load(Ordering::Relaxed),
        p.links.load(Ordering::Relaxed),
    );
    let strip_bytes = p.strip_bytes.load(Ordering::Relaxed) as usize;
    let count = p.count.load(Ordering::Relaxed) as usize;
    let head = esp32p4_cache::non_cacheable(s.as_ptr() as usize) as *const [u32; 4];
    // SAFETY: the first 16 bytes of the strip buffer (L2MEM, owned by the driver; strips are
    // longer), read through the non-cacheable alias.
    let read_head = || unsafe { ptr::read_volatile(head) };
    let before = read_head();
    let slot = (s.as_ptr() as usize)
        .wrapping_sub(ring)
        .checked_div(strip_bytes)
        .filter(|_| count >= 2);
    // SAFETY: word 13 of a list item of the ring (L2MEM), read through the non-cacheable
    // alias; reading changes nothing.
    let done = |item: usize| unsafe {
        let item = links + item * core::mem::size_of::<[u32; 16]>();
        ptr::read_volatile((esp32p4_cache::non_cacheable(item) as *const u32).add(13)) & 1 << 30
            != 0
    };
    // [done k+1, k+2, k+3, k (again)], first bytes changed.
    let mut seen = [0u32; 5];
    loop {
        let t = cycles().wrapping_sub(s.queued_at_cycles());
        if t >= hold {
            break;
        }
        let Some(k) = slot else { continue };
        for (d, v) in [1, 2, 3, 0].into_iter().zip(&mut seen[..4]) {
            if *v == 0 && d < count && done((k + d) % count) {
                *v = t.max(1);
            }
        }
        // The data first, then the done bit of item k - 1: bytes of `k` written while that
        // bit is not yet visible are an order error.
        if seen[4] == 0 && read_head() != before {
            seen[4] = t.max(1);
            if !done((k + count - 1) % count) {
                bump(&CHECK.order_errors, 1);
            }
        }
    }
    record_hold_times(&seen);
    read_head() != before
}

/// Folds one hold's timeline into [`HOLD_TIMES`].
#[inline(never)]
fn record_hold_times(seen: &[u32; 5]) {
    let h = &HOLD_TIMES;
    let pairs = [
        (&h.done1_min, &h.done1_max),
        (&h.done2_min, &h.done2_max),
        (&h.done3_min, &h.done3_max),
        (&h.done0_min, &h.done0_max),
        (&h.change_min, &h.change_max),
    ];
    for ((lo, hi), &v) in pairs.into_iter().zip(seen) {
        if v != 0 {
            let cur = lo.load(Ordering::Relaxed);
            if cur == 0 || v < cur {
                lo.store(v, Ordering::Relaxed);
            }
            raise(hi, v);
        }
    }
}

/// One strip of [`Job::Check`] (see [`CheckCounters`]).
#[inline(never)]
fn check_strip(st: &mut Core1State<'_>, s: StripRef<'_>) {
    let start = cycles();
    let c = &CHECK;
    let stops = STOPS.load(Ordering::Acquire);
    raise(
        &CHECK_MAX.start_latency,
        start.wrapping_sub(s.queued_at_cycles()),
    );
    let (frame, first, rows, last) = (s.frame(), s.first_row(), s.rows(), s.is_last());
    let p = &PARAMS;
    let line_bytes = p.line_bytes.load(Ordering::Relaxed) as usize;
    // SAFETY: core 0 keeps the reference (`ref_rows` words) for the whole run and writes it
    // only while the strip path is stopped.
    let want = unsafe {
        core::slice::from_raw_parts(
            p.reference.load(Ordering::Relaxed) as *const u32,
            p.ref_rows.load(Ordering::Relaxed) as usize,
        )
    };
    if first == 0 {
        if matches!(&st.check, Some(o) if o.stops == stops) {
            bump(&c.seq_errors, 1);
        }
        st.check = Some(CheckOpen {
            frame,
            next_row: 0,
            good: true,
            stops,
        });
    }
    let in_order = matches!(&st.check, Some(o) if o.frame == frame && o.next_row == first);
    if !in_order {
        bump(&c.seq_errors, 1);
        st.check = None;
    }
    if s.len() != usize::from(rows) * line_bytes {
        bump(&c.size_errors, 1);
    }
    let (bad, first_bad) = check_rows(&s, line_bytes, want, usize::from(first));
    raise(&CHECK_MAX.check, cycles().wrapping_sub(start));
    let every = p.hold_every.load(Ordering::Relaxed);
    let hold = p.hold.load(Ordering::Relaxed);
    let held = hold != 0
        && u32::from(first) == p.hold_row.load(Ordering::Relaxed)
        && frame.checked_rem(every) == Some(0);
    let changed = held && hold_changed(&s, hold);
    let stops_before_release = STOPS.load(Ordering::Acquire);
    let kept = s.release();
    let end = cycles();
    if held {
        bump(&c.held, 1);
        bump(
            match (changed, kept) {
                (true, true) => &c.held_changed_kept,
                (true, false) => &c.held_changed_dropped,
                (false, false) => &c.held_same_dropped,
                (false, true) => &c.held_same_kept,
            },
            1,
        );
        st.hold_end = Some((end, stops));
    }
    if stops_before_release != stops {
        bump(if kept { &c.old_kept } else { &c.old_dropped }, 1);
    }
    if !kept {
        bump(&c.dropped, 1);
        bump(&c.rows_bad_dropped, bad);
        st.check = None;
        return;
    }
    bump(&c.kept, 1);
    bump(&c.rows, u32::from(rows));
    bump(&c.rows_bad, bad);
    if let Some(row) = first_bad
        && CHECK_MAX.first_bad.load(Ordering::Relaxed) == 0
    {
        CHECK_MAX.first_bad.store(
            (frame << 12 | u32::from(row)).wrapping_add(1),
            Ordering::Relaxed,
        );
    }
    let Some(o) = st.check.as_mut() else {
        return;
    };
    o.next_row = o.next_row.wrapping_add(rows);
    o.good &= bad == 0;
    if !last {
        return;
    }
    if usize::from(o.next_row) != want.len() {
        bump(&c.seq_errors, 1);
    }
    let good = o.good && usize::from(o.next_row) == want.len();
    st.check = None;
    bump(&c.frames, 1);
    if good {
        bump(&c.frames_good, 1);
        if let Some((h, hold_stops)) = st.hold_end.take()
            && hold_stops == stops
        {
            bump(&c.recoveries, 1);
            raise(&CHECK_MAX.recovery, end.wrapping_sub(h));
        }
    }
}
