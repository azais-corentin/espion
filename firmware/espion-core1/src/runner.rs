//! The runner: core 1's entry, trap vector and loop, and core 0's handle on it ([`Core1`]).
//! Board results: `firmware/poc-p4-eth/README.md`, "Detector on core 1", and
//! `firmware/poc-p4-stream/README.md`, "Co-load".
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
//!    With feature `markers`, this core's marker channels' outputs on (its own
//!    `cpu_gpio_oen` CSR); the loop toggles channel [`crate::MARKER_STRIP`] as each strip
//!    of a detector run comes in and [`crate::MARKER_FRAME`] when a frame's blobs are final.
//! 4. PIE on core 1 if [`start`]'s `skip` asks for it: `Pie::enable` reads the chip revision
//!    from eFuse (a read-only register, through the PAC), reads `mhartid`, swaps this core's
//!    flag in the crate's `IN_USE` table and writes this core's PIE status CSR (0x7F2): no
//!    lock, no logging, no esp-hal call. The token is held for ever, by the loop, the only code
//!    that runs on core 1 (no interrupt, no other thread), so the PIE rule of one user per
//!    core holds. Core 0's token is a different one (its own flag and CSR); a `Pie` is neither
//!    `Send` nor `Sync`, so core 0's cannot reach core 1. If it refuses, core 1 runs the scalar
//!    skip; [`skip_used`] tells core 0.
//!
//! # The loop
//!
//! Take a new [`StripReader`] from the mailbox if core 0 left one; answer a clock request
//! ([`Core1::clock_offset`]); handle an abort request ([`Core1::abort_stream`]); poll; on a
//! strip, do the run's job, release the strip; without one, measure the next blob of a stream
//! frame waiting for its spots ([`refine_step`]), or wait [`POLL_GAP_CYCLES`]. Every turn it
//! increments [`HEARTBEAT`] and stores its `mcycle` in [`NOW`].
//!
//! - [`Job::Detect`]: the detector on the strip's rows ([`detect_rows`], the only detector
//!   call: `push_rows_with` with the skip chosen at the entry), or on a synthetic strip core 0
//!   prepared, with the L1 data autoload over the strip if the run asks for it; at the end of
//!   a frame a [`FrameResult`] in the queue.
//! - [`Job::Check`]: every row's checksum ([`row_sum`]) against the reference row checksums
//!   core 0 took from whole frames of the sensor's random test pattern, the strip sequence,
//!   and the test holds of the lap and stall tests, with the L1 data autoload over the strip
//!   if the run asks for it; counters in [`CHECK`] and [`CHECK_MAX`].
//! - The stream job ([`Core1::set_stream`], [`stream_strip`]): at a frame's first strip its
//!   detection settings ([`Core1::set_detect`]) and a frame buffer claimed from [`FREE`] (none
//!   free: the frame is detected, not copied, [`NOT_COPIED`]). Per strip: its hold counted
//!   from a lower bound of its block's end ([`strip_bound`]), the DMA copy of the strip to
//!   its rows in the frame buffer started ([`DmaCopy`], one block), the detector and the
//!   background sample (`espion_vision::auto`) on the strip while the copy runs (with the L1
//!   data autoload over the strip if the run asks for it). The detector is one bounded call
//!   (`push_rows_until`) that stops at a row boundary once its next step could end past
//!   [`StreamRun::budget_cycles`]; stopped short, the rest of the frame is copied and
//!   sampled only ([`FrameResult::cut`]; its background still covers the whole frame). Then
//!   the copy waited for (at most [`COPY_MAX_CYCLES`]) and the strip released. A strip whose
//!   copy ended while it was intact is released with `release_copied`: lapped after that, the
//!   frame goes on without detection. Any other strip released `false` abandons the frame and
//!   its buffer goes back. After the last
//!   strip ([`stream_end`]): `finish`, the
//!   background, the frame buffer's lines invalidated, a reserved result slot; the kept blobs
//!   (area filter, at most [`StreamDetect::spots`]) are measured on the assembled frame
//!   (`espion_vision::refine`) one per loop turn between strips, and the result is published
//!   after the last, handing the buffer to core 0. The next frame's end finishes a waiting
//!   one first. An abort request or a new reader drops the frame being assembled and the one
//!   waiting, with their buffers, once the copy in flight has ended ([`stream_drop`]).
//!
//! What core 1 may call: atomics, `mcycle`/`minstret`, the strip reader (no critical
//! section, no esp-hal driver call, see `esp32p4-csi`'s README), the copier's register
//! accesses (`DmaCopy::start`, `poll`, `wait`, `abort`: no critical section, no interrupt),
//! `esp32p4-cache` (the reader's invalidate, the frame buffer's invalidate, the flush of a
//! synthetic strip and the autoload registers), `esp32p4-perf`, the detector, the background
//! sample and the refined measure. Never `critical_section`, `log`/`esp-println`, esp-rtos,
//! embassy or an esp-hal driver.
//!
//! # L1 data autoload on core 1
//!
//! The autoload registers exist once for the chip (the L1 data cache is shared by both
//! cores), so one context owns them: here core 1 while its run has the autoload on, and core
//! 0 never touches them meanwhile. The section is the strip being read and nothing else,
//! turned off (waiting until no fetch is in flight) before the strip is released.
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
use esp32p4_csi::{DmaCopy, Released, StripReader, StripRef};
use esp32p4_perf::{CpuSample, cycles};
use espion_vision::auto::{self, Background, Histogram};
use espion_vision::refine::{self, Spot};
use espion_vision::{Blob, DefaultDetector, Scalar, Skip, Stats};
use static_cell::{ConstStaticCell, StaticCell};

use crate::SkipChoice;
use crate::spsc::{Consumer, Producer, Spsc};

/// Core 1's stack. The detector state and the result slots are statics, so the loop needs
/// little: 16 KiB leaves room for the watchpoint's guard word and any detector frame.
const STACK_BYTES: usize = 16 * 1024;
/// Blobs per frame result: the detector's own capacity (`DefaultDetector`).
pub const MAX_BLOBS: usize = 256;
/// Blobs per frame [`Job::Stream`] measures with `espion_vision::refine` (the kept ones, at
/// most the limit of [`StreamDetect::spots`]).
pub const MAX_SPOTS: usize = 128;
/// [`FrameResult::buffer`] of a frame that was not copied.
pub const NO_BUFFER: u8 = 0xFF;
/// Most frame buffers of [`Job::Stream`] (one bit each of [`FREE`]).
pub const MAX_FRAME_BUFFERS: usize = 8;
/// Frame results in flight: a few frames (33 ms at 120 fps) of slack for core 0.
const QUEUE_FRAMES: usize = 4;
/// Cycles core 1 waits between two polls that found nothing: 10 us at 400 MHz, a sixtieth of
/// a 1280x800 strip (0.64 ms). Polls share the L2MEM bus with the DMA.
const POLL_GAP_CYCLES: u32 = 4_000;
/// Longest a strip's copy may take before core 1 gives it up (and the frame): 4 ms at
/// 400 MHz. A 100 KiB strip takes under 0.5 ms beside live capture and the JPEG encoder.
const COPY_MAX_CYCLES: u32 = 1_600_000;
/// The stream job's detector's longest step between two of its stop questions
/// (`push_rows_until`), per pixel of a row: one row of the worst case, every other pixel
/// bright (the sensor's colour test patterns on a mono sensor: 324 runs per 1280-pixel row).
/// Measured on the board (2026-11-21, 1280 wide, RAW8 and RAW10, every test pattern): at most
/// 351 us per row (110 cycles per pixel), 387 us (121) with the strip's copy and a lap in
/// flight; this leaves 19 % above that.
const STEP_CYCLES_PER_PIXEL: u32 = 144;
/// The other kind of step, one skip over the dark rest of a strip, per byte of the strip:
/// measured at most 0.6 cycles per byte (152 us for a 1280x64 RAW10 strip of a scene).
const SKIP_CYCLES_PER_BYTE: u32 = 1;
/// The word [`start`] paints core 1's stack with, for [`Core1::stack_unused`].
const PAINT: u32 = 0x5A5A_C0DE;
/// Bytes at the bottom of core 1's stack the scan skips: esp-hal's closure and guard word.
const PAINT_SKIP: usize = 256;

static STACK: ConstStaticCell<Stack<STACK_BYTES>> = ConstStaticCell::new(Stack::new());
static CORE1: StaticCell<Core1> = StaticCell::new();

/// One finished frame, from core 1 to core 0.
pub struct FrameResult {
    /// The strip path's frame number: frame ends since the receiver's start before this frame
    /// (its own end not included).
    pub frame: u32,
    /// [`Job::Stream`]: the run number of [`StreamRun::run`] (0 for the other jobs).
    pub run: u32,
    /// [`Job::Stream`]: the frame buffer holding the assembled frame, now core 0's (give it
    /// back with [`give_frame`]); [`NO_BUFFER`] if the frame was not copied.
    pub buffer: u8,
    /// [`Job::Stream`]: the detector ran on the frame (detection on), and the detection
    /// serial its settings came with ([`StreamDetect::serial`]).
    pub detected: bool,
    pub serial: u32,
    /// [`Job::Stream`]: detection was on but cut short for this frame: the detector's next
    /// step on a strip could have ended past the hold the ring allows
    /// ([`StreamRun::budget_cycles`]), or a strip was lapped after its copy, so the rest of
    /// the frame was copied but not detected (`detected` is false).
    pub cut: bool,
    /// Strips of the frame taken.
    pub strips: u16,
    /// Strips consumed more than one strip period after the reader took them (the detector
    /// fell behind the DMA there; [`Job::Stream`]: released after that).
    pub late_strips: u16,
    /// Valid entries of `blobs`.
    pub blob_count: u16,
    /// The detector's counters for the frame.
    pub stats: Stats,
    /// Core-1 cycles and instructions in the detector (rows, `finish`; [`Job::Stream`]: also
    /// the background sample).
    pub cycles: u32,
    pub instructions: u32,
    /// Longest time from a strip's `queued_at_cycles` (a lower bound of its block's end,
    /// [`Job::Stream`]: [`strip_bound`]) to the end of its detector call ([`Job::Stream`]: to
    /// its release), in cycles.
    pub worst_latency: u32,
    /// Longest time from a strip's `queued_at_cycles` to the start of its detector call.
    pub start_latency: u32,
    /// From the last strip's `queued_at_cycles` to the publication of this result.
    pub ready: u32,
    /// [`Job::Stream`]: from core 1's start on the frame's first strip (its `mcycle` before
    /// the strip's copy and detector call, detection on or off) to the last strip's
    /// `queued_at_cycles` ([`strip_bound`]), in cycles; 0 if that start came after it, and for
    /// the other jobs.
    pub lead: u32,
    /// [`Job::Stream`]: the last strip's hold, from its `queued_at_cycles` to its release, in
    /// cycles (0 for the other jobs).
    pub last_hold: u32,
    /// The last strip's `queued_at_cycles` (core 1's `mcycle`, see [`age`]).
    pub queued_at: u32,
    /// Core 1's `mcycle` at the publication (see [`Core1::clock_offset`]).
    pub pushed_at: u32,
    /// Threshold the frame was detected with, in its format's units, and its format.
    pub threshold: u16,
    pub raw10: bool,
    /// [`Job::Stream`]: the background of the frame's sample, taken over every strip of a
    /// frame whose detection was on at its first strip, a `cut` one's too (`None` for an
    /// empty sample, a frame without detection and the other jobs).
    pub background: Option<Background>,
    pub blobs: [Blob; MAX_BLOBS],
    /// [`Job::Stream`], copied frames: the kept blobs (area filter) measured on the assembled
    /// frame (`espion_vision::refine`: Gaussian-weighted centroid over the local background),
    /// at most [`StreamDetect::spots`]; `spot_blob[i]` is the index in `blobs` of `spots[i]`.
    pub spot_count: u16,
    pub spots: [Spot; MAX_SPOTS],
    pub spot_blob: [u8; MAX_SPOTS],
    /// [`Job::Stream`]: core-1 cycles of the frame's end work after its last strip's release
    /// and before the publication: the frame buffer's invalidate and every loop turn's refined
    /// measure ([`refine_step`]) of this frame (0 when not copied). The detector's `finish` and
    /// the background are in `cycles`.
    pub refine_cycles: u32,
    /// [`Job::Stream`]: per strip, cycles from the reader's take to the release (total and
    /// longest), of the detector (longest), and spent waiting for the copy after the detector
    /// (total and longest).
    pub hold_total: u32,
    pub hold_max: u32,
    pub detect_max: u32,
    pub copy_wait_total: u32,
    pub copy_wait_max: u32,
    /// [`Job::Stream`]: every band of the frame went to the live JPEG encode
    /// (`esp32p4_jpeg::live`): its file comes from that encode ([`live_frame`]).
    pub jpeg: bool,
    /// [`Job::Stream`]: the longest feed of the live JPEG encode after one of the frame's
    /// strips, cycles (after the strip's release, so outside its hold).
    pub feed_max: u32,
}

impl FrameResult {
    const EMPTY: Self = Self {
        frame: 0,
        run: 0,
        buffer: NO_BUFFER,
        detected: false,
        cut: false,
        serial: 0,
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
        lead: 0,
        last_hold: 0,
        queued_at: 0,
        pushed_at: 0,
        threshold: 0,
        raw10: false,
        background: None,
        blobs: [Blob {
            x: 0,
            y: 0,
            area: 0,
            peak: 0,
        }; MAX_BLOBS],
        spot_count: 0,
        spots: [Spot {
            x: 0,
            y: 0,
            width: 0,
            flux: 0,
        }; MAX_SPOTS],
        spot_blob: [0; MAX_SPOTS],
        refine_cycles: 0,
        hold_total: 0,
        hold_max: 0,
        detect_max: 0,
        copy_wait_total: 0,
        copy_wait_max: 0,
        jpeg: false,
        feed_max: 0,
    };
}

/// Frame results, core 1 → core 0. Overwrite-never: a full queue counts a drop.
static RESULTS: Spsc<FrameResult, QUEUE_FRAMES> =
    Spsc::new([const { UnsafeCell::new(FrameResult::EMPTY) }; QUEUE_FRAMES]);

/// [`Job::Stream`]'s run parameters. Writer: core 0 ([`Core1::set_stream`]), only while the
/// strip path is stopped (published to core 1 like [`PARAMS`]).
struct StreamParams {
    /// Width (bits 0..16), RAW10 (bit 31).
    format: AtomicU32,
    height: AtomicU32,
    line_bytes: AtomicU32,
    /// The frame buffers: the first's address, the distance between two, their count, and the
    /// bytes of a frame.
    base: AtomicUsize,
    stride: AtomicUsize,
    count: AtomicU32,
    frame_bytes: AtomicUsize,
    run: AtomicU32,
}

static STREAM: StreamParams = StreamParams {
    format: AtomicU32::new(0),
    height: AtomicU32::new(0),
    line_bytes: AtomicU32::new(0),
    base: AtomicUsize::new(0),
    stride: AtomicUsize::new(0),
    count: AtomicU32::new(0),
    frame_bytes: AtomicUsize::new(0),
    run: AtomicU32::new(0),
};

/// [`Job::Stream`]'s detection settings, read by core 1 at each frame's first strip. Writer:
/// core 0 ([`Core1::set_detect`]), any time: the other words first, `serial` last (Release);
/// core 1 loads `serial` first (Acquire).
struct DetectParams {
    /// Threshold (bits 0..10), detection on (bit 31).
    word: AtomicU32,
    /// Minimum area (bits 0..16), maximum area (16..32).
    area: AtomicU32,
    spots: AtomicU32,
    serial: AtomicU32,
}

static DETECT: DetectParams = DetectParams {
    word: AtomicU32::new(0),
    area: AtomicU32::new(0),
    spots: AtomicU32::new(0),
    serial: AtomicU32::new(0),
};

/// [`Job::Stream`]'s free frame buffers, one bit each: core 1 claims one at a frame's first
/// strip (compare and swap) and gives back those it does not publish; core 0 gives back each
/// frame it was handed once it is done with it ([`give_frame`]).
static FREE: AtomicU32 = AtomicU32::new(0);

/// Frame buffers the live JPEG encode reads (bits 0..8: set by core 1 when it binds a frame
/// to the encode, cleared by core 0 when the encode ended, [`release_encoder`]) and buffers
/// given back while it still did (bits 8..16, [`give_frame`]): the one of the two that comes
/// second puts the buffer in [`FREE`].
static HOLDS: AtomicU32 = AtomicU32::new(0);
const DEFERRED_SHIFT: u32 = 8;

/// The frame bound to the live JPEG encode (`esp32p4_jpeg::live`). Writer: core 1, while it
/// holds the encode's input side (inside `live::begin`'s guard), so core 0 reads it once it
/// took the input side back (the encode's end or its cancel).
struct LiveRecord {
    run: AtomicU32,
    frame: AtomicU32,
    buffer: AtomicU32,
    /// Core 1's `mcycle` at the first band appended, at the last one, and the last strip's
    /// bound ([`strip_bound`]: the frame's capture time) (the last two set with the band that
    /// ends the picture).
    first_feed: AtomicU32,
    last_feed: AtomicU32,
    capture: AtomicU32,
}

static LIVE: LiveRecord = LiveRecord {
    run: AtomicU32::new(0),
    frame: AtomicU32::new(0),
    buffer: AtomicU32::new(NO_BUFFER as u32),
    first_feed: AtomicU32::new(0),
    last_feed: AtomicU32::new(0),
    capture: AtomicU32::new(0),
};

/// Core 0's abort requests ([`Core1::abort_stream`]) and core 1's acknowledgement: the request
/// number it handled, stored once it holds no frame buffer and its copy is idle.
static ABORT_REQ: AtomicU32 = AtomicU32::new(0);
static ABORT_ACK: AtomicU32 = AtomicU32::new(0);

/// Writer: core 1, once per turn of its loop. Core 0 reports core 1 stalled when it stops.
static HEARTBEAT: AtomicU32 = AtomicU32::new(0);
/// Writer: core 1, once per turn of its loop: its `mcycle` then ([`age`]).
static NOW: AtomicU32 = AtomicU32::new(0);
/// Writer: core 1. Frames begun but not published: a strip released `false` (lapped, or the
/// run stopped), or a new frame began before the last strip.
static ABANDONED: AtomicU32 = AtomicU32::new(0);
/// Writer: core 1. [`Job::Stream`]: frames begun without a free frame buffer (detected, not
/// copied), and copies that failed (the channel's error or [`COPY_MAX_CYCLES`]).
static NOT_COPIED: AtomicU32 = AtomicU32::new(0);
static COPY_ERRORS: AtomicU32 = AtomicU32::new(0);
/// The lowest word of core 1's stack ([`Core1::stack_unused`]).
static STACK_BOTTOM: AtomicUsize = AtomicUsize::new(0);
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
    /// [`start`] asked for PIE and `Pie::enable` refused: the scalar skip.
    ScalarPieRefused,
}

const SKIP_SCALAR: u32 = 1;
const SKIP_PIE: u32 = 2;
const SKIP_REFUSED: u32 = 3;

/// Trap record. Writer: [`core1_trap`] only (words 1 to 3 first, then word 0).
#[repr(C)]
struct TrapRecord([AtomicU32; 4]);

/// Word 0: 1 once core 1 trapped; 1: `mcause`; 2: `mepc`; 3: `mtval`.
static TRAP: TrapRecord = TrapRecord([const { AtomicU32::new(0) }; 4]);

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
    /// 0 [`Job::Detect`], 1 [`Job::Check`], 2 the stream job ([`Core1::set_stream`]).
    job: AtomicU32,
    /// Width (bits 0..16), threshold (16..26), RAW10 (bit 31).
    format: AtomicU32,
    /// A strip consumed later than this many cycles after the reader took it counts as late.
    late: AtomicU32,
    /// [`Job::Stream`]: cycles of sensor time per row, 15/16 of the mode's line time: a block
    /// finishes at least its rows' time after the block before it.
    row: AtomicU32,
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
    /// [`Job::Stream`]: cycles from the end of a strip's block within which the detector
    /// ends ([`StreamRun::budget_cycles`]).
    budget: AtomicU32,
    /// [`Job::Stream`]: the detector's longest step, cycles (see [`STEP_CYCLES_PER_PIXEL`]):
    /// it starts no step later than `budget - step` after the strip's block ended.
    step: AtomicU32,
}

static PARAMS: Params = Params {
    job: AtomicU32::new(0),
    format: AtomicU32::new(0),
    late: AtomicU32::new(u32::MAX),
    row: AtomicU32::new(0),
    budget: AtomicU32::new(u32::MAX),
    step: AtomicU32::new(0),
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
    /// L1 data autoload over each strip (detector and check).
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

/// Starts core 1 with `det` and `scratch` (where a frame's blobs go when the queue is full),
/// its detector running the dark skip `skip` ([`SkipChoice::Pie`]: PIE enabled on core 1 by
/// its entry, the scalar skip if `Pie::enable` refuses), and `copy`, the DMA channel of the
/// stream job's strip copies (`None`: that job copies nothing). Paints core 1's stack first
/// ([`Core1::stack_unused`]). Call once; `None` if core 1 runs already.
#[inline(never)]
pub fn start(
    cpu: CPU_CTRL<'static>,
    det: &'static mut DefaultDetector,
    scratch: &'static mut [Blob; MAX_BLOBS],
    skip: SkipChoice,
    copy: Option<DmaCopy<'static>>,
) -> Option<&'static mut Core1> {
    let (producer, results) = RESULTS.split()?;
    let stack = STACK.try_take()?;
    let bottom = stack.bottom();
    for i in 0..stack.len() / 4 {
        // SAFETY: the stack's words, not in use: core 1 has not started.
        unsafe { bottom.add(i).write_volatile(PAINT) };
    }
    STACK_BOTTOM.store(bottom as usize, Ordering::Relaxed);
    let guard = CpuControl::new(cpu)
        .start_app_core(stack, move || entry(skip, det, scratch, producer, copy))
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

    /// The parameters of the next stream run, and the stream job for it (instead of
    /// [`Core1::set_run`]'s): per strip the detector (when [`Core1::set_detect`] has it on),
    /// the background sample and the copy into the frame's buffer; per frame the refined
    /// measure of the kept blobs on the assembled frame and one [`FrameResult`]. Only while
    /// the strip path is stopped.
    pub fn set_stream(&mut self, run: &StreamRun) {
        let s = &STREAM;
        s.format.store(
            u32::from(run.width) | u32::from(run.raw10) << 31,
            Ordering::Relaxed,
        );
        s.height.store(u32::from(run.height), Ordering::Relaxed);
        s.line_bytes.store(run.line_bytes, Ordering::Relaxed);
        s.base.store(run.frames.base, Ordering::Relaxed);
        s.stride.store(run.frames.stride, Ordering::Relaxed);
        s.count.store(
            u32::from(run.frames.count).min(MAX_FRAME_BUFFERS as u32),
            Ordering::Relaxed,
        );
        s.frame_bytes
            .store(run.frames.frame_bytes, Ordering::Relaxed);
        s.run.store(run.run, Ordering::Relaxed);
        let p = &PARAMS;
        p.late.store(run.late_cycles, Ordering::Relaxed);
        let lines = u32::from(run.strip_lines).max(1);
        p.row
            .store(run.late_cycles / lines / 16 * 15, Ordering::Relaxed);
        p.budget.store(run.budget_cycles, Ordering::Relaxed);
        let row = u32::from(run.width) * STEP_CYCLES_PER_PIXEL;
        let skip = run.line_bytes * u32::from(run.strip_lines) * SKIP_CYCLES_PER_BYTE;
        p.step.store(row.max(skip), Ordering::Relaxed);
        p.autoload.store(u32::from(run.autoload), Ordering::Relaxed);
        p.synth.store(0, Ordering::Relaxed);
        p.job.store(2, Ordering::Relaxed);
    }

    /// The detection settings of the stream job's next frames (any time; a frame takes them
    /// at its first strip). The threshold is clamped to the format's range.
    pub fn set_detect(&mut self, d: &StreamDetect) {
        let raw10 = STREAM.format.load(Ordering::Relaxed) >> 31 != 0;
        let max = if raw10 { 0x3FF } else { 0xFF };
        let word = u32::from(d.threshold.clamp(1, max)) | u32::from(d.enabled) << 31;
        let p = &DETECT;
        p.word.store(word, Ordering::Relaxed);
        p.area.store(
            u32::from(d.min_area) | u32::from(d.max_area) << 16,
            Ordering::Relaxed,
        );
        p.spots
            .store(u32::from(d.spots).min(MAX_SPOTS as u32), Ordering::Relaxed);
        p.serial.store(d.serial, Ordering::Release);
    }

    /// Asks core 1 to drop what the stream job holds: the frame being assembled, the frame
    /// waiting for its refined measure (its result is not published), each with its buffer,
    /// after the copy in flight ends (at most [`COPY_MAX_CYCLES`]). Core 1 does it between
    /// two strips or two blobs; [`Core1::stream_idle`] says when it has. Returns the request.
    pub fn abort_stream(&mut self) -> u32 {
        let req = ABORT_REQ.load(Ordering::Relaxed).wrapping_add(1);
        ABORT_REQ.store(req, Ordering::Release);
        req
    }

    /// Core 1 has handled abort request `req` ([`Core1::abort_stream`]): it holds no frame
    /// buffer and its copy is idle until the next run's first strip.
    pub fn stream_idle(&self, req: u32) -> bool {
        ABORT_ACK.load(Ordering::Acquire) == req
    }

    /// Bytes of core 1's stack never written since [`start`] painted it (from its bottom up,
    /// past esp-hal's closure and guard word).
    pub fn stack_unused(&self) -> usize {
        let bottom = STACK_BOTTOM.load(Ordering::Relaxed);
        let words = STACK_BYTES / 4;
        let skip = PAINT_SKIP / 4;
        let painted = (skip..words)
            // SAFETY: words of core 1's stack (a static), read only; a racing write by core 1
            // only ends the count there.
            .take_while(|&i| unsafe { ((bottom + i * 4) as *const u32).read_volatile() } == PAINT)
            .count();
        painted * 4
    }
}

/// Frame buffers of the stream job (PSRAM): `count` buffers of at least `frame_bytes`, the
/// first at `base`, `stride` bytes apart; start and stride multiples of the cache line.
#[derive(Clone, Copy)]
pub struct FrameBuffers {
    pub base: usize,
    pub stride: usize,
    pub count: u8,
    pub frame_bytes: usize,
}

/// The parameters of a stream run ([`Core1::set_stream`]).
#[derive(Clone, Copy)]
pub struct StreamRun {
    /// Row width in pixels, rows, RAW10 or RAW8, bytes per row.
    pub width: u16,
    pub height: u16,
    pub raw10: bool,
    pub line_bytes: u32,
    /// Rows of a full strip.
    pub strip_lines: u16,
    pub frames: FrameBuffers,
    /// Cycles after which a strip counts as late.
    pub late_cycles: u32,
    /// Cycles from the end of a strip's block (as [`strip_bound`] bounds it) within which the
    /// detector must end: the ring's hold ((strips - 1) strip periods) less a margin for the
    /// copy's end and the release. The detector starts no step later than this less its
    /// longest step ([`STEP_CYCLES_PER_PIXEL`]); stopped short, it stops for the rest of the
    /// frame ([`FrameResult::cut`]), which is still copied.
    pub budget_cycles: u32,
    /// L1 data autoload over each strip the detector reads: core 0 must have given the
    /// autoload registers to core 1 (module docs).
    pub autoload: bool,
    /// Core 0's number for the run, echoed in its results ([`FrameResult::run`]).
    pub run: u32,
}

/// The stream job's detection settings ([`Core1::set_detect`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamDetect {
    pub enabled: bool,
    /// Foreground threshold, in the format's units.
    pub threshold: u16,
    /// Blobs of an area in `min_area..=max_area` are kept: measured on the frame.
    pub min_area: u16,
    pub max_area: u16,
    /// Most kept blobs measured per frame (at most [`MAX_SPOTS`]).
    pub spots: u16,
    /// Echoed in the results of the frames detected with these settings.
    pub serial: u32,
}

/// Cycles since core 1's `mcycle` read `then` (a [`FrameResult::queued_at`]), as of core 1's
/// last loop turn (at most one strip's or one blob's work late, 10 us when idle). 0 if `then`
/// is later than that turn (a strip's bound can be, by up to one poll).
pub fn age(then: u32) -> u32 {
    let d = NOW.load(Ordering::Relaxed).wrapping_sub(then);
    if (d as i32) < 0 { 0 } else { d }
}

/// Gives frame buffer `index` to the stream job (core 0: every buffer once at the start,
/// then each one a [`FrameResult::buffer`] handed over, once done with it; core 1: the
/// buffers it does not publish). A buffer the live JPEG encode still reads goes back once the
/// encode ended ([`release_encoder`]).
pub fn give_frame(index: u8) {
    if usize::from(index) >= MAX_FRAME_BUFFERS {
        return;
    }
    let (held, deferred) = (1 << index, 1 << (DEFERRED_SHIFT + u32::from(index)));
    if HOLDS.fetch_or(deferred, Ordering::AcqRel) & held == 0 {
        HOLDS.fetch_and(!deferred, Ordering::Relaxed);
        FREE.fetch_or(held, Ordering::Release);
    }
}

/// The live JPEG encode no longer reads frame buffer `index` (core 0, once the encode bound
/// to it ended or was cancelled): it goes back now if it was given back meanwhile.
pub fn release_encoder(index: u8) {
    if usize::from(index) >= MAX_FRAME_BUFFERS {
        return;
    }
    let (held, deferred) = (1 << index, 1 << (DEFERRED_SHIFT + u32::from(index)));
    if HOLDS.fetch_and(!held, Ordering::AcqRel) & deferred != 0 {
        HOLDS.fetch_and(!deferred, Ordering::Relaxed);
        FREE.fetch_or(held, Ordering::Release);
    }
}

/// The frame core 1 bound to the live JPEG encode (core 0, after the encode's input side was
/// taken back).
#[derive(Clone, Copy, Debug)]
pub struct LiveFrame {
    /// [`StreamRun::run`] and the frame's number in it ([`FrameResult::frame`]).
    pub run: u32,
    pub frame: u32,
    /// Its frame buffer, which the encode holds until [`release_encoder`].
    pub buffer: u8,
    /// Core 1's `mcycle` at the first band fed, at the last, and the last strip's bound (the
    /// frame's capture time, as [`FrameResult::queued_at`]); see [`age`].
    pub first_feed: u32,
    pub last_feed: u32,
    pub capture: u32,
}

/// The frame bound to the live JPEG encode last.
pub fn live_frame() -> LiveFrame {
    let l = &LIVE;
    LiveFrame {
        run: l.run.load(Ordering::Acquire),
        frame: l.frame.load(Ordering::Relaxed),
        buffer: l.buffer.load(Ordering::Relaxed) as u8,
        first_feed: l.first_feed.load(Ordering::Relaxed),
        last_feed: l.last_feed.load(Ordering::Relaxed),
        capture: l.capture.load(Ordering::Relaxed),
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

/// Stream job: frames detected without a free frame buffer (not copied), and strip copies
/// that failed.
pub fn copy_counts() -> (u32, u32) {
    (
        NOT_COPIED.load(Ordering::Relaxed),
        COPY_ERRORS.load(Ordering::Relaxed),
    )
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
    skip: SkipChoice,
    det: &'static mut DefaultDetector,
    scratch: &'static mut [Blob; MAX_BLOBS],
    out: Producer<FrameResult, QUEUE_FRAMES>,
    copy: Option<DmaCopy<'static>>,
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
    // Feature `markers`: this core's marker channels, enabled by this core (the enable CSR is
    // per hart; core 0 routed them to pads before starting core 1).
    #[cfg(feature = "markers")]
    esp32p4_perf::marker::enable(crate::markers::CORE1_MASK);
    let pie = match skip {
        SkipChoice::Pie => esp32p4_pie::Pie::enable().ok(),
        SkipChoice::Scalar => None,
    };
    let st = Core1State {
        det,
        scratch,
        out,
        open: None,
        check: None,
        hold_end: None,
        hist: Histogram::new(),
        copy,
        asm: None,
        pending: None,
        bound: None,
    };
    match &pie {
        Some(p) => {
            SKIP_USED.store(SKIP_PIE, Ordering::Release);
            run(p, st)
        }
        None => {
            let used = match skip {
                SkipChoice::Pie => SKIP_REFUSED,
                SkipChoice::Scalar => SKIP_SCALAR,
            };
            SKIP_USED.store(used, Ordering::Release);
            run(&Scalar, st)
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

/// The frame the stream job is assembling.
struct Asm {
    frame: u32,
    /// Its frame buffer ([`NO_BUFFER`]: not copied).
    buffer: u8,
    /// Every copy so far completed.
    copy_ok: bool,
    /// Detection on, with these settings ([`DETECT`] at the first strip).
    detect: bool,
    /// Detection was on at the first strip: the background sample takes every strip of the
    /// frame, also those after a cut, so a cut frame still has its background.
    sample: bool,
    serial: u32,
    threshold: u16,
    area: (u16, u16),
    spots: u16,
    strips: u16,
    late: u16,
    cycles: u32,
    instructions: u32,
    worst: u32,
    start: u32,
    /// `mcycle` at the detector's start on the frame's first strip (row 0).
    first_start: u32,
    /// The hold of the frame's last strip, from its bound to its release.
    last_hold: u32,
    hold_total: u32,
    detect_max: u32,
    copy_wait_total: u32,
    copy_wait_max: u32,
    /// Detection was on and was cut short for this frame ([`FrameResult::cut`]).
    cut: bool,
    /// The frame and the live JPEG encode ([`feed_jpeg`]).
    jpeg: Jpeg,
    /// Longest [`feed_jpeg`] call of the frame, cycles.
    feed_max: u32,
}

/// A frame's part in the live JPEG encode (`esp32p4_jpeg::live`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Jpeg {
    /// Not bound: it binds an armed encode at its next strip.
    Unbound,
    /// Bound: its copied rows are fed as they come.
    Bound,
    /// Every band fed: the encode has the whole frame.
    Fed,
    /// Not to be encoded: core 0 took the encode back, or it could not be fed.
    Lost,
}

/// A finished frame of the stream job whose result slot is reserved and filled but for the
/// spots: the kept blobs are measured one per loop turn, between strips.
struct Pending {
    /// Next index of the result's `blobs` to look at.
    next: u16,
    buffer: u8,
    area: (u16, u16),
    spots: u16,
    /// The assembled frame, and its geometry.
    frame: &'static [u8],
    width: u16,
    height: u16,
    raw10: bool,
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
    /// The background sample of the frame being detected (stream job).
    hist: Histogram,
    /// The stream job's strip copies.
    copy: Option<DmaCopy<'static>>,
    asm: Option<Asm>,
    pending: Option<Pending>,
    /// The stream job: the last strip's lower bound of the end of its block ([`strip_bound`]).
    bound: Option<u32>,
}

/// Core 1's loop with the dark skip `skip`. Never returns.
#[inline(never)]
fn run<S: Skip>(skip: &S, mut st: Core1State<'_>) -> ! {
    let mut reader: Option<&'static mut StripReader> = None;
    let mut current = 0;
    let mut abort_seen = 0;
    loop {
        HEARTBEAT.fetch_add(1, Ordering::Relaxed);
        NOW.store(cycles(), Ordering::Relaxed);
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
            stream_drop(&mut st);
            st.bound = None;
        }
        answer_clock();
        let abort = ABORT_REQ.load(Ordering::Acquire);
        if abort != abort_seen {
            abort_seen = abort;
            stream_drop(&mut st);
            ABORT_ACK.store(abort, Ordering::Release);
        }
        let strip = match reader.as_mut() {
            Some(r) => r.poll(),
            None => None,
        };
        match strip {
            Some(s) => match PARAMS.job.load(Ordering::Relaxed) {
                1 => check_strip(&mut st, s),
                2 => stream_strip(skip, &mut st, s),
                _ => on_strip(skip, &mut st, s),
            },
            None if st.pending.is_some() => refine_step(&mut st),
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
    mark(crate::MARKER_STRIP);
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
    detect_rows(skip, det, rows, s.first_row(), || false);
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
        let format = PARAMS.format.load(Ordering::Relaxed);
        r.threshold = (format >> 16) as u16 & 0x3FF;
        r.raw10 = format >> 31 != 0;
        r.run = 0;
        r.buffer = NO_BUFFER;
        r.detected = true;
        r.queued_at = queued_at;
        r.lead = 0;
        r.last_hold = 0;
        r.jpeg = false;
        r.feed_max = 0;
        r.background = None;
        r.spot_count = 0;
    });
    if !queued {
        // Counted as dropped by the queue; the detector still has to close the frame.
        det.finish(st.scratch);
    }
    mark(crate::MARKER_FRAME);
}

/// Claims a free frame buffer of the stream run (the lowest), or [`NO_BUFFER`].
fn claim_buffer() -> u8 {
    let count = STREAM.count.load(Ordering::Relaxed);
    let mask = (1u32 << count.min(MAX_FRAME_BUFFERS as u32)) - 1;
    let mut v = FREE.load(Ordering::Acquire);
    loop {
        let avail = v & mask;
        if avail == 0 {
            return NO_BUFFER;
        }
        let bit = avail.isolate_lowest_one();
        match FREE.compare_exchange_weak(v, v & !bit, Ordering::Acquire, Ordering::Acquire) {
            Ok(_) => return bit.trailing_zeros() as u8,
            Err(now) => v = now,
        }
    }
}

/// Gives frame buffer `b` back to [`FREE`] ([`NO_BUFFER`]: nothing).
fn return_buffer(b: u8) {
    if b != NO_BUFFER {
        give_frame(b);
    }
}

/// The address of frame buffer `b`.
fn buffer_addr(b: u8) -> usize {
    STREAM.base.load(Ordering::Relaxed) + usize::from(b) * STREAM.stride.load(Ordering::Relaxed)
}

/// Drops what the stream job holds, after the copy in flight ends: the frame being assembled
/// and the frame waiting for its spots (unpublished), with their buffers. On an abort request
/// and when core 0 posts a new reader.
#[inline(never)]
fn stream_drop(st: &mut Core1State<'_>) {
    if let Some(c) = st.copy.as_mut()
        && c.is_busy()
        && c.wait(COPY_MAX_CYCLES).is_err()
    {
        bump(&COPY_ERRORS, 1);
    }
    if let Some(a) = st.asm.take() {
        drop_frame(&a);
    }
    if let Some(p) = st.pending.take() {
        st.out.cancel();
        return_buffer(p.buffer);
    }
}

/// Lets go of frame `a`, which is not published: the live JPEG encode bound to it, if its
/// bands did not all come, is abandoned (core 0 stops it and releases the buffer's hold), and
/// the buffer goes back (once that encode no longer reads it).
fn drop_frame(a: &Asm) {
    if a.jpeg == Jpeg::Bound {
        esp32p4_jpeg::live::abandon(a.frame);
    }
    return_buffer(a.buffer);
}

/// The first strip of stream frame `frame`: its detection settings, a free frame buffer (if
/// any; else the frame is detected but not copied), the detector and the background sample
/// begun. A frame still open is abandoned.
fn stream_begin(st: &mut Core1State<'_>, frame: u32) {
    if let Some(a) = st.asm.take() {
        ABANDONED.fetch_add(1, Ordering::Relaxed);
        drop_frame(&a);
    }
    let d = &DETECT;
    let serial = d.serial.load(Ordering::Acquire);
    let word = d.word.load(Ordering::Relaxed);
    let area = d.area.load(Ordering::Relaxed);
    let spots = d.spots.load(Ordering::Relaxed) as u16;
    let format = STREAM.format.load(Ordering::Relaxed);
    let (width, raw10) = (format as u16, format >> 31 != 0);
    let detect = word >> 31 != 0;
    let threshold = if raw10 {
        word as u16 & 0x3FF
    } else {
        (word as u16 & 0x3FF).min(0xFF)
    };
    let buffer = match st.copy {
        Some(_) => claim_buffer(),
        None => NO_BUFFER,
    };
    if buffer == NO_BUFFER {
        bump(&NOT_COPIED, 1);
    }
    if detect {
        if raw10 {
            st.det.begin_frame_raw10(width, threshold);
        } else {
            st.det.begin_frame(width, threshold as u8);
        }
        st.hist.clear();
    }
    st.asm = Some(Asm {
        frame,
        buffer,
        copy_ok: true,
        detect,
        sample: detect,
        serial,
        threshold,
        area: (area as u16, (area >> 16) as u16),
        spots,
        strips: 0,
        late: 0,
        cycles: 0,
        instructions: 0,
        worst: 0,
        start: 0,
        first_start: 0,
        last_hold: 0,
        hold_total: 0,
        detect_max: 0,
        copy_wait_total: 0,
        copy_wait_max: 0,
        cut: false,
        jpeg: Jpeg::Unbound,
        feed_max: 0,
    });
}

/// One strip of the stream job: the copy into the frame's buffer started, the detector and
/// the background sample on the strip meanwhile, the copy waited for, the release check, and
/// after the last strip the frame's end ([`stream_end`]).
#[inline(never)]
fn stream_strip<S: Skip>(skip: &S, st: &mut Core1State<'_>, s: StripRef<'_>) {
    mark(crate::MARKER_STRIP);
    let queued_at = strip_bound(&mut st.bound, &s);
    if s.first_row() == 0 {
        stream_begin(st, s.frame());
    }
    let Core1State {
        det,
        hist,
        copy,
        asm,
        ..
    } = st;
    let Some(a) = asm.as_mut().filter(|a| a.frame == s.frame()) else {
        // Not part of a frame begun here: give it back unused.
        let _ = s.release();
        return;
    };
    let t0 = CpuSample::now();
    a.start = a.start.max(t0.cycles.wrapping_sub(queued_at));
    if s.first_row() == 0 {
        a.first_start = t0.cycles;
    }
    // The copy first: the DMA reads the strip while the detector does.
    let mut copying = false;
    if a.buffer != NO_BUFFER
        && a.copy_ok
        && let Some(c) = copy.as_mut()
    {
        let off = usize::from(s.first_row()) * STREAM.line_bytes.load(Ordering::Relaxed) as usize;
        if off + s.len() <= STREAM.frame_bytes.load(Ordering::Relaxed) {
            // SAFETY: the strip is the DMA's again only once released, after the copy ends
            // (or the release reports it written over and the frame is dropped); the frame
            // buffer was claimed from `FREE` for this frame: nobody else touches it until
            // its result is published.
            copying = unsafe { c.start(s.as_ptr() as usize, buffer_addr(a.buffer) + off, s.len()) }
                .is_ok();
        }
        if !copying {
            a.copy_ok = false;
            bump(&COPY_ERRORS, 1);
        }
    }
    // The copy's end, once seen (while the detector runs, between its rows, or after it), and
    // whether the strip was intact right after: then the frame buffer holds the strip's rows
    // whatever the DMA writes into the strip later.
    let mut copy_end = None;
    let mut copy_intact = false;
    if a.detect {
        let autoload = PARAMS.autoload.load(Ordering::Relaxed) != 0;
        if autoload {
            autoload_on(&s);
        }
        let budget = PARAMS.budget.load(Ordering::Relaxed);
        // The strip must be released within the budget: the detector stops at a row boundary
        // once its next step (one row's worst case, or one skip over the rest of the strip)
        // could end past it; the rest of the frame is then copied and sampled only.
        let last_start = budget.saturating_sub(PARAMS.step.load(Ordering::Relaxed));
        // The background sample first, over the whole strip.
        sample_strip(&s, hist);
        let rows = detect_rows(skip, det, &s, s.first_row(), || {
            let age = cycles().wrapping_sub(queued_at);
            // No lap before the budget: the copy's end is worth a look (a register read) only
            // once the strip is past half of it, for a detector that might still overrun.
            if copying
                && copy_end.is_none()
                && age > budget / 2
                && let Some(c) = copy.as_mut()
            {
                copy_end = c.poll();
                copy_intact = copy_end == Some(Ok(())) && s.intact();
            }
            age > last_start
        });
        if rows < usize::from(s.rows()) {
            a.detect = false;
            a.cut = true;
        }
        if autoload {
            prefetch::autoload_disable(Level::L1Data);
        }
    } else if a.sample {
        // Cut: the background sample goes on over the rest of the frame (cheap: a sparse
        // sample), so the automatic threshold sees a cut frame's background too.
        sample_strip(&s, hist);
    }
    let d = CpuSample::now().since(&t0);
    let w0 = cycles();
    if copying
        && copy_end.is_none()
        && let Some(c) = copy.as_mut()
    {
        copy_end = Some(c.wait(COPY_MAX_CYCLES));
        copy_intact = copy_end == Some(Ok(())) && s.intact();
    }
    if copying && copy_end != Some(Ok(())) {
        a.copy_ok = false;
        bump(&COPY_ERRORS, 1);
    }
    let wait = cycles().wrapping_sub(w0);
    let last = s.is_last();
    // Rows of the frame in its buffer once this strip's copy is in (the strip is gone after
    // its release).
    let copied = s.first_row() + s.rows();
    let kept = if copy_intact {
        match s.release_copied() {
            Released::Intact => true,
            Released::Lapped => {
                // The copy is the strip's, the detector's reads after its check may not be:
                // the frame goes on without detection.
                if a.detect {
                    a.detect = false;
                    a.cut = true;
                }
                true
            }
            Released::Stopped => false,
        }
    } else {
        s.release()
    };
    let hold = cycles().wrapping_sub(queued_at);
    a.strips = a.strips.wrapping_add(1);
    if a.detect {
        a.cycles = a.cycles.wrapping_add(d.cycles);
        a.instructions = a.instructions.wrapping_add(d.instructions);
        a.detect_max = a.detect_max.max(d.cycles);
    }
    a.worst = a.worst.max(hold);
    a.hold_total = a.hold_total.wrapping_add(hold);
    a.copy_wait_total = a.copy_wait_total.wrapping_add(wait);
    a.copy_wait_max = a.copy_wait_max.max(wait);
    if hold > PARAMS.late.load(Ordering::Relaxed) {
        a.late = a.late.wrapping_add(1);
    }
    if !kept {
        // Lapped or stopped: neither the copy nor the detector read this frame's rows.
        ABANDONED.fetch_add(1, Ordering::Relaxed);
        drop_frame(a);
        *asm = None;
        return;
    }
    if matches!(a.jpeg, Jpeg::Unbound | Jpeg::Bound) {
        let t = cycles();
        feed_jpeg(a, copied, last, queued_at);
        a.feed_max = a.feed_max.max(cycles().wrapping_sub(t));
    }
    if last {
        a.last_hold = hold;
        stream_end(st, queued_at);
    }
}

/// Feeds the live JPEG encode (`esp32p4_jpeg::live`) frame `a`'s rows copied so far (up to
/// row `copied`), in whole 8-row bands, and the rest of the frame with its last strip;
/// binds an armed encode first if the frame has none yet. `capture`: the strip's bound, the
/// frame's capture time when `last`. Called after the strip's release: never part of its
/// hold. A frame without its buffer or with a failed copy is not encoded (an encode bound to
/// it is abandoned).
#[inline(never)]
fn feed_jpeg(a: &mut Asm, copied: u16, last: bool, capture: u32) {
    use esp32p4_jpeg::live;
    if a.buffer == NO_BUFFER || !a.copy_ok {
        if a.jpeg == Jpeg::Bound {
            live::abandon(a.frame);
        }
        a.jpeg = Jpeg::Lost;
        return;
    }
    let Some(mut f) = live::begin(a.frame) else {
        if a.jpeg == Jpeg::Bound {
            // Core 0 took the encode back (an error, a timeout, the stream moved on).
            a.jpeg = Jpeg::Lost;
        }
        return;
    };
    let format = STREAM.format.load(Ordering::Relaxed);
    let (width, height) = (format as u16, STREAM.height.load(Ordering::Relaxed) as u16);
    let now = cycles();
    if a.jpeg == Jpeg::Unbound {
        let ok = f.geometry() == (width, height) && format >> 31 == 0;
        let l = &LIVE;
        l.run
            .store(STREAM.run.load(Ordering::Relaxed), Ordering::Relaxed);
        l.frame.store(a.frame, Ordering::Relaxed);
        l.first_feed.store(now, Ordering::Relaxed);
        if !ok {
            // Armed for another picture, not this run's: abandoned (core 0 takes it back),
            // with no buffer held.
            l.buffer.store(u32::from(NO_BUFFER), Ordering::Relaxed);
            drop(f);
            live::abandon(a.frame);
            a.jpeg = Jpeg::Lost;
            return;
        }
        HOLDS.fetch_or(1 << a.buffer, Ordering::AcqRel);
        l.buffer.store(u32::from(a.buffer), Ordering::Relaxed);
        a.jpeg = Jpeg::Bound;
    }
    let fed = f.fed_rows();
    let target = if last { height } else { copied & !7 };
    if target <= fed || (!last && f.bands_left() <= 1) {
        return;
    }
    if target == height {
        LIVE.capture.store(capture, Ordering::Relaxed);
    }
    LIVE.last_feed.store(now, Ordering::Relaxed);
    let line = STREAM.line_bytes.load(Ordering::Relaxed) as usize;
    match f.append(
        buffer_addr(a.buffer) + usize::from(fed) * line,
        target - fed,
    ) {
        Ok(()) if target == height => a.jpeg = Jpeg::Fed,
        Ok(()) => {}
        Err(_) => {
            drop(f);
            live::abandon(a.frame);
            a.jpeg = Jpeg::Lost;
        }
    }
}

/// The lower bound of the end of strip `s`'s block that its hold counts from: the reader's
/// ([`StripRef::queued_at_cycles`]: its last look that found the block unfinished), or, if
/// later, the bound of the strip before (`prev`) plus `s`'s rows of sensor time: the sensor
/// sends `s`'s rows after the rows of every block before it, and a block ends after its last
/// row arrived. Never later than now. Updates `prev`.
fn strip_bound(prev: &mut Option<u32>, s: &StripRef<'_>) -> u32 {
    let q = s.queued_at_cycles();
    let now = cycles();
    let b = match *prev {
        // Bounds more than a second apart (2^28 cycles at 400 MHz) are not compared.
        Some(p) if (q.wrapping_sub(p) as i32).unsigned_abs() < 1 << 28 => {
            let min = p.wrapping_add(u32::from(s.rows()) * PARAMS.row.load(Ordering::Relaxed));
            if (min.wrapping_sub(q) as i32) > 0 && (now.wrapping_sub(min) as i32) >= 0 {
                min
            } else {
                q
            }
        }
        _ => q,
    };
    *prev = Some(b);
    b
}

/// The stream frame's last strip was released: the frame waiting for its spots is finished
/// first (unless core 0 asks for an abort); then a result slot, `finish`, the background, the
/// copied frame's lines invalidated, and the spots measured one per loop turn
/// ([`refine_step`]) or the result published at once (nothing to measure).
#[inline(never)]
fn stream_end(st: &mut Core1State<'_>, queued_at: u32) {
    let Some(a) = st.asm.take() else {
        return;
    };
    if st.pending.is_some() {
        if ABORT_REQ.load(Ordering::Acquire) != ABORT_ACK.load(Ordering::Relaxed) {
            // The loop's abort drops the waiting frame.
            drop_frame(&a);
            return;
        }
        // The frame before still has blobs to measure (strips came back to back since):
        // it goes out with the spots measured so far, the rest counted as dropped by core 0.
        // Measuring them all here would hold the next frame's strips past their budget.
        st.pending = None;
        publish(st);
    }
    let buffer = if a.copy_ok {
        a.buffer
    } else {
        drop_frame(&a);
        NO_BUFFER
    };
    if !st.out.reserve() {
        // Counted as dropped by the queue; the detector still has to close the frame. Its
        // live encode, if every band was fed, still ends (the buffer goes back after it).
        if a.detect {
            st.det.finish(st.scratch);
        }
        if buffer != NO_BUFFER {
            drop_frame(&a);
        }
        mark(crate::MARKER_FRAME);
        return;
    }
    let Some(r) = st.out.reserved() else {
        return;
    };
    let t0 = CpuSample::now();
    let (n, stats) = if a.detect {
        let n = st.det.finish(&mut r.blobs) as u16;
        (n, st.det.stats())
    } else {
        (
            0,
            Stats {
                pixels: 0,
                runs: 0,
                blobs: 0,
                overflow: 0,
            },
        )
    };
    // A cut frame's too: its sample went on over every strip.
    let background = if a.sample {
        Background::from_histogram(&st.hist)
    } else {
        None
    };
    let d = CpuSample::now().since(&t0);
    let format = STREAM.format.load(Ordering::Relaxed);
    r.frame = a.frame;
    r.run = STREAM.run.load(Ordering::Relaxed);
    r.buffer = buffer;
    r.detected = a.detect;
    r.cut = a.cut;
    r.serial = a.serial;
    r.strips = a.strips;
    r.late_strips = a.late;
    r.blob_count = n;
    r.stats = stats;
    r.jpeg = a.jpeg == Jpeg::Fed;
    r.feed_max = a.feed_max;
    r.cycles = a.cycles.wrapping_add(if a.detect { d.cycles } else { 0 });
    r.instructions = a
        .instructions
        .wrapping_add(if a.detect { d.instructions } else { 0 });
    r.worst_latency = a.worst;
    r.start_latency = a.start;
    r.queued_at = queued_at;
    let lead = queued_at.wrapping_sub(a.first_start);
    r.lead = if (lead as i32) < 0 { 0 } else { lead };
    r.last_hold = a.last_hold;
    r.threshold = a.threshold;
    r.raw10 = format >> 31 != 0;
    r.background = background;
    r.spot_count = 0;
    r.refine_cycles = 0;
    r.hold_total = a.hold_total;
    r.hold_max = a.worst;
    r.detect_max = a.detect_max;
    r.copy_wait_total = a.copy_wait_total;
    r.copy_wait_max = a.copy_wait_max;
    if buffer != NO_BUFFER {
        // The copies wrote PSRAM behind the caches: drop the buffer's lines (both cores
        // share the L1 data cache and the L2), so the measure here and core 0 read the frame.
        let base = buffer_addr(buffer);
        let bytes = STREAM.frame_bytes.load(Ordering::Relaxed);
        let line = esp32p4_cache::line_size(base);
        let t = cycles();
        esp32p4_cache::invalidate(base, bytes.wrapping_add(line - 1) & !(line - 1));
        r.refine_cycles = cycles().wrapping_sub(t);
        if a.detect && n > 0 && a.spots > 0 {
            st.pending = Some(Pending {
                next: 0,
                buffer,
                area: a.area,
                spots: a.spots,
                // SAFETY: the frame buffer is this frame's until its result is published
                // (claimed from `FREE`); its copies are complete and its lines invalidated.
                frame: unsafe { core::slice::from_raw_parts(base as *const u8, bytes) },
                width: format as u16,
                height: STREAM.height.load(Ordering::Relaxed) as u16,
                raw10: format >> 31 != 0,
            });
            return;
        }
    }
    publish(st);
}

/// Publishes the reserved result.
fn publish(st: &mut Core1State<'_>) {
    if let Some(r) = st.out.reserved() {
        let now = cycles();
        r.ready = now.wrapping_sub(r.queued_at);
        r.pushed_at = now;
    }
    st.out.commit();
    mark(crate::MARKER_FRAME);
}

/// The next kept blob of the frame waiting for its spots measured on the frame
/// (`espion_vision::refine`), at most one per call; the result published after the last.
#[inline(never)]
fn refine_step(st: &mut Core1State<'_>) {
    let Core1State { out, pending, .. } = st;
    let (Some(p), Some(r)) = (pending.as_mut(), out.reserved()) else {
        *pending = None;
        return;
    };
    let t0 = cycles();
    let limit = usize::from(p.spots).min(MAX_SPOTS);
    while p.next < r.blob_count {
        let i = p.next;
        p.next += 1;
        let Some(b) = r.blobs.get(usize::from(i)) else {
            break;
        };
        if b.area < p.area.0 || b.area > p.area.1 {
            continue;
        }
        let n = usize::from(r.spot_count);
        let (Some(spot), Some(index)) = (r.spots.get_mut(n), r.spot_blob.get_mut(n)) else {
            p.next = r.blob_count;
            break;
        };
        if n >= limit {
            p.next = r.blob_count;
            break;
        }
        *spot = if p.raw10 {
            refine::refine_raw10(p.frame, p.width, p.height, b)
        } else {
            refine::refine_raw8(p.frame, p.width, p.height, b)
        };
        *index = i as u8;
        r.spot_count += 1;
        break;
    }
    r.refine_cycles = r.refine_cycles.wrapping_add(cycles().wrapping_sub(t0));
    if p.next >= r.blob_count {
        *pending = None;
        publish(st);
    }
}

/// Toggles core 1's marker channel `channel` (feature `markers`; nothing without it).
#[inline(always)]
fn mark(channel: u8) {
    #[cfg(feature = "markers")]
    esp32p4_perf::marker::toggle(1 << channel);
    #[cfg(not(feature = "markers"))]
    let _ = channel;
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

/// The background sample (`espion_vision::auto`) of one strip of the stream run's format into
/// `hist`.
#[inline(always)]
fn sample_strip(s: &StripRef<'_>, hist: &mut Histogram) {
    let format = STREAM.format.load(Ordering::Relaxed);
    if format >> 31 != 0 {
        auto::sample_raw10(s, format as u16, s.first_row(), hist);
    } else {
        auto::sample_raw8(s, format as u16, s.first_row(), hist);
    }
}

/// The detector on one strip's rows with the dark skip `skip`, stopping at a row boundary once
/// `stop` says so (`push_rows_until`); the rows done. The only detector call of the runner.
#[inline(always)]
fn detect_rows<S: Skip>(
    skip: &S,
    det: &mut DefaultDetector,
    rows: &[u8],
    first_row: u16,
    stop: impl FnMut() -> bool,
) -> usize {
    det.push_rows_until(skip, rows, first_row, stop)
}

/// Checksum of one row, the one [`Job::Check`] compares: two running sums over its
/// little-endian 32-bit words (Fletcher style, so the order of the words counts), then a tail
/// of up to 3 bytes. Core 0 computes the reference with it from whole frames. Rows of both
/// paths start on a word boundary (frame buffers 4 KiB aligned, strip buffers 64 B, rows of a
/// multiple of 4 bytes); another start takes a slower path with the same result.
pub fn row_sum(row: &[u8]) -> u32 {
    let (mut a, mut b) = (0u32, 0u32);
    let mut add = |w: u32| {
        a = a.wrapping_add(w);
        b = b.wrapping_add(a);
    };
    // SAFETY: every bit pattern is a valid `u32`.
    let (pre, words, post) = unsafe { row.align_to::<u32>() };
    let tail = if pre.is_empty() {
        words.iter().for_each(|&w| add(u32::from_le(w)));
        post
    } else {
        let (words, tail) = row.as_chunks::<4>();
        words.iter().for_each(|w| add(u32::from_le_bytes(*w)));
        tail
    };
    tail.iter().for_each(|&x| add(u32::from(x)));
    a ^ b.rotate_left(16)
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
    let autoload = p.autoload.load(Ordering::Relaxed) != 0;
    if autoload {
        autoload_on(&s);
    }
    let (bad, first_bad) = check_rows(&s, line_bytes, want, usize::from(first));
    if autoload {
        prefetch::autoload_disable(Level::L1Data);
    }
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
