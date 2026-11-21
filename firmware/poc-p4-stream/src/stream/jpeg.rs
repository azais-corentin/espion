//! The JPEG stage of the stream ([`Encoding::Jpeg`]): the encode task, its output buffers and
//! the automatic quality.
//!
//! Pipeline, all on core 0 and one frame per stage, never a queue:
//!
//! 1. The capture task offers every captured RAW8 frame it would send ([`offer`]): the frame
//!    waits in [`INPUT`], a one-frame slot. A frame already waiting there is replaced and
//!    counted as one the encoder could not take ([`EncoderState::frames_skipped`]), so the
//!    encoder always gets the newest frame. The encode stage holds at most two captured
//!    frames (the one being encoded, the one waiting), which the whole-frame receiver's four
//!    buffers and the strip path's five allow.
//! 2. The encode task ([`task`]) takes it and encodes it with the hardware encoder straight
//!    from the frame's buffer (no CPU pass over the pixels) into a free output buffer, then
//!    lets go of the frame's buffer at once.
//! 3. The file goes to the send task as the next [`Job`] (`JOB`, a one-frame slot). A file
//!    still waiting there (the send task is busy with the one before) is replaced and counted
//!    as skipped because the link was busy ([`Counters::frames_skipped`]): the send task
//!    always gets the newest file, and the link never waits for an encode.
//!
//! Encoding the next frame and sending the one before overlap: [`OUT_BUFFERS`] output buffers
//! (one being sent, one waiting, one being encoded into) are used in turn. Each is
//! [`JPEG_FRAME_MAX`] bytes in PSRAM, 128-byte aligned (the driver's rule for PSRAM); the
//! encoder gets the part [`esp32p4_jpeg::Config::max_output_len`] says the mode needs (the
//! header and 2 bytes per pixel). A file that does not fit ends in `OutputTooSmall` in under
//! 1 ms: that frame is lost and counted as an encoder error, the next one encodes normally.
//!
//! Encoder errors (output too small, codec, DMA, no end within [`ENCODE_TIMEOUT`]) are
//! counted ([`EncoderState::errors`]); the driver resets the hardware before the next frame.
//! A size or quality change reprograms the encoder before the next frame
//! ([`Encoder::set_config`]). The control task never waits for this task.
//!
//! The automatic quality ([`espion_proto::cam::auto_quality`]) runs here at the end of every
//! [`AUTO_QUALITY_PERIOD_MS`] while JPEG frames go out with `auto_quality` on.

use core::cell::Cell;
use core::future::{Future, poll_fn};
use core::ops::Deref;
use core::pin::pin;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use critical_section::Mutex;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp32p4_jpeg::live::Input as Side;
use esp32p4_jpeg::{Config, Encoder, Error};
use espion_proto::cam::{AUTO_QUALITY_PERIOD_MS, EncodeSpan, Encoding, JPEG_FRAME_MAX};
use log::{error, info, warn};

use super::frames::{FrameBuf, Pool};
use super::{JOB, Job, Payload, SEND_BUSY_US, shared};
use crate::cpu::{self, Timed};
use crate::irq;

/// Output buffers: one being sent, one waiting for the send task, one being encoded into.
pub const OUT_BUFFERS: usize = 3;
/// Bytes per output buffer: the largest JPEG frame the protocol allows, which holds the
/// driver's worst case for 1280x800 (a multiple of the 128-byte PSRAM cache line).
pub const OUT_LEN: usize = JPEG_FRAME_MAX as usize;
const _: () = assert!(OUT_LEN.is_multiple_of(128));
/// Longest an encode may take: 1280x800 takes 6 ms (the driver's measurement).
const ENCODE_TIMEOUT: Duration = Duration::from_millis(100);

/// A captured frame for the encoder, with what its chunks will carry.
pub(super) struct Input {
    /// The captured frame (its buffer goes back once the encode is done).
    pub frame: FrameBuf,
    pub number: u32,
    pub generation: u32,
    pub epoch: u32,
    pub width: u16,
    pub height: u16,
    pub timestamp_us: u64,
}

/// The frame waiting for the encoder (at most one).
static INPUT: Signal<CriticalSectionRawMutex, Input> = Signal::new();

/// Hands `input` to the encoder; a frame still waiting is dropped and counted as one the
/// encoder could not take.
pub(super) fn offer(input: Input) {
    if let Some(old) = INPUT.try_take() {
        drop(old);
        shared(|s| s.encoder.frames_skipped = s.encoder.frames_skipped.wrapping_add(1));
    }
    INPUT.signal(input);
}

// ── Output buffers ──────────────────────────────────────────────────────────────────────────

/// Free output buffers, one bit each, and the start of the first.
static POOL: Mutex<Cell<(u8, usize)>> = Mutex::new(Cell::new((0, 0)));

/// An output buffer taken from the pool; dropping it gives the buffer back.
struct Slot(u8);

impl Drop for Slot {
    fn drop(&mut self) {
        critical_section::with(|cs| {
            let p = POOL.borrow(cs);
            let (free, base) = p.get();
            p.set((free | 1 << self.0, base));
        });
    }
}

/// A JPEG file in an output buffer, held by the send task; dropping it frees the buffer.
pub(super) struct Out {
    data: &'static [u8],
    _slot: Slot,
}

impl Deref for Out {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.data
    }
}

/// An output buffer being encoded into (not yet a file).
struct Blank {
    mem: &'static mut [u8],
    slot: Slot,
}

impl Blank {
    /// The file of `len` bytes at its start.
    fn into_file(self, len: usize) -> Out {
        let mem: &'static [u8] = self.mem;
        Out {
            data: &mem[..len],
            _slot: self.slot,
        }
    }
}

/// Whether an output buffer is free.
fn free_blanks() -> bool {
    critical_section::with(|cs| POOL.borrow(cs).get().0 != 0)
}

/// A free output buffer, if any.
fn take_blank() -> Option<Blank> {
    let (slot, base) = critical_section::with(|cs| {
        let p = POOL.borrow(cs);
        let (free, base) = p.get();
        if free == 0 {
            return None;
        }
        let slot = free.trailing_zeros() as u8;
        p.set((free & !(1 << slot), base));
        Some((slot, base))
    })?;
    // SAFETY: the pool hands each buffer out once until its `Blank` or `Out` is dropped; the
    // buffers lie in the PSRAM range `task` was given for good.
    let mem = unsafe {
        core::slice::from_raw_parts_mut((base + slot as usize * OUT_LEN) as *mut u8, OUT_LEN)
    };
    Some(Blank {
        mem,
        slot: Slot(slot),
    })
}

// ── Measurement (the test hooks' report) ────────────────────────────────────────────────────

/// Frames encoded, and the sum of their encode times in µs (both wrap).
pub(super) static ENCODED: AtomicU32 = AtomicU32::new(0);
pub(super) static ENCODE_US: AtomicU32 = AtomicU32::new(0);
/// Bytes of the files encoded (wraps).
pub(super) static ENCODED_BYTES: AtomicU32 = AtomicU32::new(0);
/// The driver's last error, as text for the report (`""`: none yet), and the time from the
/// encode's start to the error, µs.
pub(super) static LAST_ERROR: Mutex<Cell<&'static str>> = Mutex::new(Cell::new(""));
pub(super) static LAST_ERROR_US: AtomicU32 = AtomicU32::new(0);
/// The encode task's wake-up after a finished encode: from the end of the 2D-DMA RX handler
/// that saw the file's end (`SUC_EOF`) to the start of the encode future's last poll. Includes
/// the wait behind other tasks' polls (one executor on core 0). Feature `accounting`.
pub(super) static WAKE: Timed = Timed::new();
/// The encode future's first poll: the driver's set-up (the header and its write-back, the
/// invalidate of the output, the descriptors, the start).
pub(super) static SETUP: Timed = Timed::new();
/// Its last poll: the driver's work after the end (descriptor read, the invalidate of the
/// file's lines).
pub(super) static END: Timed = Timed::new();

/// Test hooks (set by diag): the next this many encodes get a [`SMALL_OUTPUT`]-byte output (a
/// file that does not fit), and encode-only (files are dropped, not sent: the encode time
/// without the network's traffic on the bus).
pub(super) static SMALL_OUTPUTS: AtomicU32 = AtomicU32::new(0);
pub(super) static ENCODE_ONLY: AtomicBool = AtomicBool::new(false);
const SMALL_OUTPUT: usize = 4096;

/// Test hook (diag `J`): the next this many encoded frames are encoded a second time in the
/// other form, and the two files compared byte for byte ([`CHECK`]): a frame encoded whole is
/// fed again in bands of random heights with random gaps from this core (bands appended while
/// the 2D-DMA works and while it is parked).
pub(super) static CHECKS: AtomicU32 = AtomicU32::new(0);

/// The checks' counts since diag last reset them.
pub(super) struct CheckCounts {
    pub frames: AtomicU32,
    pub same: AtomicU32,
    pub differ: AtomicU32,
    /// Second encodes that failed (an error or no end within [`ENCODE_TIMEOUT`]).
    pub errors: AtomicU32,
    /// Frames left unchecked: no free output buffer for the second file.
    pub no_buffer: AtomicU32,
    /// Bands fed, and of those the ones appended while the TX channel was parked.
    pub bands: AtomicU32,
    pub parked: AtomicU32,
    /// The last difference: the first byte that differs, and the two lengths.
    pub first_diff: AtomicU32,
    pub len_a: AtomicU32,
    pub len_b: AtomicU32,
}

pub(super) static CHECK: CheckCounts = CheckCounts {
    frames: AtomicU32::new(0),
    same: AtomicU32::new(0),
    differ: AtomicU32::new(0),
    errors: AtomicU32::new(0),
    no_buffer: AtomicU32::new(0),
    bands: AtomicU32::new(0),
    parked: AtomicU32::new(0),
    first_diff: AtomicU32::new(0),
    len_a: AtomicU32::new(0),
    len_b: AtomicU32::new(0),
};

impl CheckCounts {
    pub(super) fn reset(&self) {
        for a in [
            &self.frames,
            &self.same,
            &self.differ,
            &self.errors,
            &self.no_buffer,
            &self.bands,
            &self.parked,
            &self.first_diff,
            &self.len_a,
            &self.len_b,
        ] {
            a.store(0, Ordering::Relaxed);
        }
    }
}

/// The check's random band heights and gaps (xorshift32).
static RNG: AtomicU32 = AtomicU32::new(0x9E37_79B9);

fn random() -> u32 {
    let mut x = RNG.load(Ordering::Relaxed);
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    RNG.store(x, Ordering::Relaxed);
    x
}

// ── The task ────────────────────────────────────────────────────────────────────────────────

/// A strip run, for the live encodes ([`super::Shared::live`], set by the capture task).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LiveRun {
    /// Core 1's run number ([`espion_core1::StreamRun::run`]).
    pub id: u32,
    /// Frames captured before it: a frame's number is this plus core 1's frame number.
    pub base: u32,
    pub width: u16,
    pub height: u16,
    /// Bytes of a frame in a pool buffer.
    pub frame_bytes: usize,
    /// The mode's frame period, µs.
    pub period_us: u32,
}

/// How often an armed encode is looked at while it waits: the stream moving on, the feeder
/// giving up (core 1 cannot wake this task), a bound encode's timeout.
const LIVE_LOOK: Duration = Duration::from_millis(2);
/// How often the task looks for a live encode to arm while nothing else wakes it (no output
/// buffer free, no host, another encoding).
const ARM_LOOK: Duration = Duration::from_millis(1);

/// Creates the encoder (on this core, which takes its interrupts) and encodes for good: whole
/// frames from [`INPUT`], and on strips live encodes core 1 feeds ([`live`]).
/// `out_mem`: [`OUT_BUFFERS`] x [`OUT_LEN`] bytes of PSRAM, 128-byte aligned. `pool`: the
/// strip path's frame buffers (the check reads a live frame from there).
#[embassy_executor::task]
pub(super) async fn task(out_mem: &'static mut [u8], pool: Pool) {
    cpu::name_current("encode");
    let base = out_mem.as_mut_ptr() as usize;
    critical_section::with(|cs| POOL.borrow(cs).set(((1 << OUT_BUFFERS) - 1, base)));
    let mut enc = match Encoder::new(Config::new(1280, 800, 90)) {
        Ok(e) => e,
        Err(e) => {
            log_no_encoder(e);
            return;
        }
    };
    crate::irq::install(&[
        crate::irq::Source::Jpeg,
        crate::irq::Source::Dma2dRx,
        crate::irq::Source::Dma2dTx,
    ]);
    log_ready(base);
    let mut auto = Auto::new();
    loop {
        if let Some(want) = live_wanted() {
            auto.step();
            live(&mut enc, want, pool).await;
            continue;
        }
        if let Either::First(input) = select(INPUT.wait(), Timer::after(ARM_LOOK)).await {
            auto.step();
            encode(&mut enc, input).await;
        }
    }
}

/// What a live encode is armed for.
#[derive(Clone, Copy)]
struct Want {
    run: LiveRun,
    epoch: u32,
    quality: u8,
}

/// A live encode to arm now: on strips, while JPEG frames go out, with an output buffer free.
fn live_wanted() -> Option<Want> {
    let now = Instant::now();
    let want = shared(|s| {
        let run = s.live?;
        (s.active(now) && s.stream.encoding == Encoding::Jpeg).then_some(Want {
            run,
            epoch: s.epoch,
            quality: s.encoder.quality,
        })
    })?;
    free_blanks().then_some(want)
}

/// Whether the armed encode `want` still is: the same run and epoch, JPEG frames going out.
fn still_wanted(want: &Want, now: Instant) -> bool {
    shared(|s| {
        s.epoch == want.epoch
            && s.live.is_some_and(|l| l.id == want.run.id)
            && s.active(now)
            && s.stream.encoding == Encoding::Jpeg
    })
}

/// One live encode: armed into a free output buffer for the run's size and the quality in
/// use, bound and fed by core 1 as the frame comes in (`espion_core1`'s stream job), its file
/// handed to the send task with the frame's number and capture time from core 1. Cancelled
/// when the stream moves on before core 1 binds it, or when the quality changed meanwhile.
async fn live(enc: &mut Encoder, want: Want, pool: Pool) {
    let Some((blank, cap)) = prepare_live(enc, &want) else {
        return;
    };
    let polled_at = cpu::mcycle();
    let mut armed = match enc.arm(&mut blank.mem[..cap]) {
        Ok(a) => a,
        Err(e) => {
            fail(e_name(&e), 0);
            return;
        }
    };
    SETUP.add(cpu::mcycle().wrapping_sub(polled_at));
    let mut bound: Option<Instant> = None;
    let r = loop {
        if let Ok(r) = with_timeout(LIVE_LOOK, armed.wait()).await {
            break Some(r);
        }
        if !keep_waiting(armed.input(), &want, &mut bound) {
            break None;
        }
    };
    let end_at = cpu::mcycle();
    let now = Instant::now();
    let input = armed.input();
    let r = match r {
        Some(r) => {
            drop(armed);
            r
        }
        None => {
            cancel(armed);
            return;
        }
    };
    let Some((frame, len)) = settle(r, input, &want, end_at) else {
        return;
    };
    if CHECKS
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
        .is_ok()
        && let Some(data) = pool.held(frame.buffer, want.run.frame_bytes)
    {
        check_whole(enc, data, &blank.mem[..len], cap).await;
    }
    espion_core1::release_encoder(frame.buffer);
    END.add(cpu::mcycle().wrapping_sub(end_at));
    finish_live(&want, frame, blank, len, now);
}

/// Cancels an armed live encode; its hold on a frame bound meanwhile ends.
#[inline(never)]
fn cancel(armed: esp32p4_jpeg::Armed<'_>) {
    release(armed.cancel(), None);
}

/// Whether an armed encode, still running after a [`LIVE_LOOK`] (its input side `input`),
/// is still waited for: not when the stream moved on, when the quality changed before core 1
/// bound it, or when it ran past [`ENCODE_TIMEOUT`] from its bind (first seen bound at
/// `bound`).
#[inline(never)]
fn keep_waiting(input: Side, want: &Want, bound: &mut Option<Instant>) -> bool {
    let now = Instant::now();
    match input {
        Side::Open => {
            let quality = shared(|s| s.encoder.quality);
            still_wanted(want, now) && quality == want.quality
        }
        Side::Feeding(_) | Side::Fed(_) => {
            let since = *bound.get_or_insert(now);
            if !still_wanted(want, now) {
                return false;
            }
            if now - since > ENCODE_TIMEOUT {
                fail("timeout", (now - since).as_micros() as u32);
                return false;
            }
            true
        }
        // `wait` returns the abandon at its next poll.
        Side::Abandoned(_) | Side::Idle => true,
    }
}

/// A live encode's end (`r`, its input side `input`, `end_at` in cycles): its frame and file
/// length if it made a file of the frame core 1 bound in this run; otherwise the encode's hold
/// ends (and an error is counted).
#[inline(never)]
fn settle(
    r: Result<usize, Error>,
    input: Side,
    want: &Want,
    end_at: u32,
) -> Option<(espion_core1::LiveFrame, usize)> {
    if r.is_ok() {
        WAKE.add(end_at.wrapping_sub(irq::last_end(irq::Source::Dma2dRx)));
    }
    let frame = espion_core1::live_frame();
    let tag = match input {
        Side::Fed(t) | Side::Feeding(t) | Side::Abandoned(t) => Some(t),
        Side::Open | Side::Idle => None,
    };
    let ours = tag.is_some_and(|t| t == frame.frame) && frame.run == want.run.id;
    match r {
        Ok(len) if ours => return Some((frame, len)),
        Ok(_) => {}
        Err(Error::Abandoned) => {
            // The frame was lost in capture (core 1 could not copy or keep it): the capture
            // task counts it as skipped (its number missing, or its result without a buffer).
        }
        Err(e) => {
            let number = want.run.base.wrapping_add(frame.frame);
            log_failed(&e, number, want.run.width, want.run.height, 0);
            fail(e_name(&e), 0);
        }
    }
    release(input, Some(frame));
    None
}

/// The encode's hold on its frame's buffer ends (after a cancel, an error or the frame's
/// end): the buffer goes back once core 1's result for it was done with too.
fn release(input: Side, frame: Option<espion_core1::LiveFrame>) {
    let tag = match input {
        Side::Fed(t) | Side::Feeding(t) | Side::Abandoned(t) => t,
        Side::Open | Side::Idle => return,
    };
    let f = frame.unwrap_or_else(espion_core1::live_frame);
    if f.frame == tag {
        espion_core1::release_encoder(f.buffer);
    }
}

/// The encoder set to the run's size and the quality in use; a free output buffer and the
/// part of it the encoder gets.
#[inline(never)]
fn prepare_live(enc: &mut Encoder, want: &Want) -> Option<(Blank, usize)> {
    let cfg = Config::new(want.run.width, want.run.height, want.quality);
    if *enc.config() != cfg
        && let Err(e) = enc.set_config(cfg)
    {
        fail(e_name(&e), 0);
        return None;
    }
    let blank = take_blank()?;
    Some((blank, output_cap(&cfg)))
}

/// The part of an output buffer the encoder gets ([`SMALL_OUTPUT`] while that test hook is
/// on).
fn output_cap(cfg: &Config) -> usize {
    let small = SMALL_OUTPUTS
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
        .is_ok();
    if small {
        SMALL_OUTPUT
    } else {
        cfg.max_output_len().min(OUT_LEN)
    }
}

/// A live encode's file, `len` bytes in `blank`, finished at `now`: counted, and handed to
/// the send task (`max_fps` and the stream rules applied here, as the capture task does for
/// the other frames) with the frame's number, its capture time (core 1's last strip bound)
/// and the encode's span.
#[inline(never)]
fn finish_live(
    want: &Want,
    frame: espion_core1::LiveFrame,
    blank: Blank,
    len: usize,
    now: Instant,
) {
    let mhz = esp_hal::clock::cpu_clock().as_mhz().max(1);
    // The capture time from core 1's cycles, read together with the clock (the check may
    // have run since the encode's end at `now`).
    let at = Instant::now();
    let timestamp_us = at
        .as_micros()
        .saturating_sub(u64::from(espion_core1::age(frame.capture) / mhz));
    let rel = |c: u32| (c.wrapping_sub(frame.capture) as i32) / mhz as i32;
    let end_us = now
        .as_micros()
        .saturating_sub(timestamp_us)
        .min(u64::from(u32::MAX)) as u32;
    let span = EncodeSpan {
        start_us: rel(frame.first_feed),
        input_end_us: rel(frame.last_feed).max(0) as u32,
        end_us,
    };
    let us = (i64::from(end_us) - i64::from(span.start_us)).max(0) as u32;
    ENCODED.fetch_add(1, Ordering::Relaxed);
    ENCODE_US.fetch_add(us, Ordering::Relaxed);
    ENCODED_BYTES.fetch_add(len as u32, Ordering::Relaxed);
    let out = blank.into_file(len);
    let period_us = u64::from(want.run.period_us);
    let job = shared(|s| {
        s.encoder.frame_len = len as u32;
        s.encoder.encode_us = us;
        (s.epoch == want.epoch && s.active(at) && s.admit(at, period_us)).then_some(s.generation)
    });
    let Some(generation) = job else {
        return;
    };
    if ENCODE_ONLY.load(Ordering::Relaxed) {
        return;
    }
    hand_on(Job {
        data: Payload::Jpeg(out),
        high_bits: false,
        number: want.run.base.wrapping_add(frame.frame),
        generation,
        epoch: want.epoch,
        encoding: Encoding::Jpeg,
        len: len as u32,
        width: want.run.width,
        height: want.run.height,
        timestamp_us,
        encode: Some(span),
    });
}

/// The newest file waits for the send task, replacing (and counting as skipped because the
/// link was busy) one still waiting.
fn hand_on(job: Job) {
    if let Some(old) = JOB.try_take() {
        drop(old);
        shared(|s| s.counters.frames_skipped = s.counters.frames_skipped.wrapping_add(1));
    }
    JOB.signal(job);
}

/// Encodes `input` and hands the file to the send task, unless the stream moved on. The work
/// around the await is in `#[inline(never)]` helpers, which keeps this future's poll frame
/// small.
async fn encode(enc: &mut Encoder, input: Input) {
    let Some((blank, cap)) = prepare(enc, &input) else {
        return;
    };
    let start = Instant::now();
    let mut polled_at = 0;
    let r = {
        let mut fut = pin!(with_timeout(
            ENCODE_TIMEOUT,
            enc.encode(&input.frame, &mut blank.mem[..cap]),
        ));
        let mut first = true;
        poll_fn(|cx| {
            polled_at = cpu::mcycle();
            let p = fut.as_mut().poll(cx);
            if first && p.is_pending() {
                SETUP.add(cpu::mcycle().wrapping_sub(polled_at));
            }
            first = false;
            p
        })
        .await
    };
    let r = match r {
        Ok(Ok(len)) => Ok(len),
        Ok(Err(e)) => Err(Some(e)),
        Err(_) => Err(None),
    };
    if r.is_ok() {
        WAKE.add(polled_at.wrapping_sub(irq::last_end(irq::Source::Dma2dRx)));
        END.add(cpu::mcycle().wrapping_sub(polled_at));
    }
    let us = start.elapsed().as_micros() as u32;
    if let Ok(len) = r
        && CHECKS
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_ok()
    {
        check_banded(enc, &input.frame, &blank.mem[..len], cap).await;
    }
    finish(input, blank, r, start, us);
}

/// Encodes `frame` again, fed from this core in bands of random heights (multiples of 8, at
/// most an eighth of the picture) with random gaps (none, or up to 2 ms), and compares the
/// file with `file`, the whole encode's ([`CHECK`]).
#[inline(never)]
async fn check_banded(enc: &mut Encoder, frame: &[u8], file: &[u8], cap: usize) {
    CHECK.frames.fetch_add(1, Ordering::Relaxed);
    let Some(b) = take_blank() else {
        CHECK.no_buffer.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let r = with_timeout(
        ENCODE_TIMEOUT,
        banded(enc, frame.as_ptr() as usize, &mut b.mem[..cap]),
    )
    .await;
    match r {
        Ok(Ok(len)) => compare(file, &b.mem[..len]),
        _ => {
            CHECK.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Encodes `frame`, a live encode's frame (still held for it), again whole, and compares the
/// file with `file`, the live encode's ([`CHECK`]).
#[inline(never)]
async fn check_whole(enc: &mut Encoder, frame: &[u8], file: &[u8], cap: usize) {
    CHECK.frames.fetch_add(1, Ordering::Relaxed);
    let Some(b) = take_blank() else {
        CHECK.no_buffer.fetch_add(1, Ordering::Relaxed);
        return;
    };
    match with_timeout(ENCODE_TIMEOUT, enc.encode(frame, &mut b.mem[..cap])).await {
        Ok(Ok(len)) => compare(file, &b.mem[..len]),
        _ => {
            CHECK.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// One encode of the picture at `addr` (the encoder's size) fed in random bands.
async fn banded(enc: &mut Encoder, addr: usize, out: &mut [u8]) -> Result<usize, Error> {
    let (w, h) = (enc.config().width, enc.config().height);
    let most = (h / 8 / 8).max(1);
    let (mut armed, mut feed) = enc.arm_local(out)?;
    let mut row = 0;
    while row < h {
        let left = h - row;
        let rows = if feed.bands_left() <= 1 {
            left
        } else {
            (8 * (1 + (random() % u32::from(most)) as u16)).min(left)
        };
        let link = esp32p4_jpeg::dma2d::out(
            esp32p4_jpeg::TX_CHANNEL,
            esp32p4_jpeg::dma2d::out_reg::LINK_CONF,
        );
        if row > 0 && link.read() & esp32p4_jpeg::dma2d::out_reg::LINK_PARK != 0 {
            CHECK.parked.fetch_add(1, Ordering::Relaxed);
        }
        feed.append(addr + usize::from(row) * usize::from(w), rows)
            .map_err(Error::Band)?;
        CHECK.bands.fetch_add(1, Ordering::Relaxed);
        row += rows;
        if row < h && !random().is_multiple_of(3) {
            Timer::after_micros(u64::from(random() % 2000)).await;
        }
    }
    drop(feed);
    armed.wait().await
}

fn compare(a: &[u8], b: &[u8]) {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        None if a.len() == b.len() => {
            CHECK.same.fetch_add(1, Ordering::Relaxed);
        }
        diff => {
            let at = diff.unwrap_or(a.len().min(b.len()));
            CHECK.differ.fetch_add(1, Ordering::Relaxed);
            CHECK.first_diff.store(at as u32, Ordering::Relaxed);
            CHECK.len_a.store(a.len() as u32, Ordering::Relaxed);
            CHECK.len_b.store(b.len() as u32, Ordering::Relaxed);
        }
    }
}

/// Whether `input` is still wanted; the encoder set to its size and the quality in use; a free
/// output buffer and the part of it the encoder gets.
#[inline(never)]
fn prepare(enc: &mut Encoder, input: &Input) -> Option<(Blank, usize)> {
    let now = Instant::now();
    let quality = shared(|s| {
        (s.epoch == input.epoch && s.active(now) && s.stream.encoding == Encoding::Jpeg)
            .then_some(s.encoder.quality)
    })?;
    let cfg = Config::new(input.width, input.height, quality);
    if *enc.config() != cfg
        && let Err(e) = enc.set_config(cfg)
    {
        fail(e_name(&e), 0);
        return None;
    }
    let Some(blank) = take_blank() else {
        // Never with three buffers: one sent, one waiting, one free.
        fail("no_free_output_buffer", 0);
        return None;
    };
    Some((blank, output_cap(&cfg)))
}

/// Gives the frame's buffer back, counts the result (`Err(None)`: no end within
/// [`ENCODE_TIMEOUT`]) and hands the file on ([`hand_on`]).
#[inline(never)]
fn finish(input: Input, blank: Blank, r: Result<usize, Option<Error>>, start: Instant, us: u32) {
    let Input {
        frame,
        number,
        generation,
        epoch,
        width,
        height,
        timestamp_us,
    } = input;
    drop(frame);
    let len = match r {
        Ok(len) => len,
        Err(None) => {
            fail("timeout", us);
            return;
        }
        Err(Some(e)) => {
            log_failed(&e, number, width, height, us);
            fail(e_name(&e), us);
            return;
        }
    };
    ENCODED.fetch_add(1, Ordering::Relaxed);
    ENCODE_US.fetch_add(us, Ordering::Relaxed);
    ENCODED_BYTES.fetch_add(len as u32, Ordering::Relaxed);
    let out = blank.into_file(len);
    let live = shared(|s| {
        s.encoder.frame_len = len as u32;
        s.encoder.encode_us = us;
        s.epoch == epoch
    });
    if !live || ENCODE_ONLY.load(Ordering::Relaxed) {
        return;
    }
    hand_on(Job {
        data: Payload::Jpeg(out),
        high_bits: false,
        number,
        generation,
        epoch,
        encoding: Encoding::Jpeg,
        len: len as u32,
        width,
        height,
        timestamp_us,
        encode: Some(encode_span(timestamp_us, start, us)),
    });
}

/// A whole-frame encode of `us` µs begun at `start`, as µs after the frame's capture at
/// `timestamp_us` (its input all there at its start).
fn encode_span(timestamp_us: u64, start: Instant, us: u32) -> EncodeSpan {
    let start_us = start
        .as_micros()
        .saturating_sub(timestamp_us)
        .min(i32::MAX as u64) as u32;
    EncodeSpan {
        start_us: start_us as i32,
        input_end_us: start_us,
        end_us: start_us.saturating_add(us),
    }
}

/// Counts an encoder error (the driver resets the hardware before the next frame).
fn fail(what: &'static str, us: u32) {
    shared(|s| s.encoder.errors = s.encoder.errors.wrapping_add(1));
    critical_section::with(|cs| LAST_ERROR.borrow(cs).set(what));
    LAST_ERROR_US.store(us, Ordering::Relaxed);
}

/// An encoder error in the log, with its raw bits (rare: the stream goes on).
#[inline(never)]
fn log_failed(e: &Error, number: u32, width: u16, height: u16, us: u32) {
    warn!("jpeg: frame {number} ({width}x{height}) not encoded after {us} us: {e:?}");
}

/// The error's name in the test hooks' report (one word).
fn e_name(e: &Error) -> &'static str {
    match e {
        Error::Geometry { .. } => "geometry",
        Error::Quality(_) => "quality",
        Error::AlreadyInUse => "already_in_use",
        Error::DescriptorMemory => "descriptor_memory",
        Error::InputSize { .. } => "input_size",
        Error::InputAlignment => "input_alignment",
        Error::OutputAlignment { .. } => "output_alignment",
        Error::OutputTooSmall { .. } => "output_too_small",
        Error::Codec { .. } => "codec",
        Error::Dma { .. } => "dma",
        Error::Band(_) => "band",
        Error::Abandoned => "abandoned",
    }
}

// ── Automatic quality ───────────────────────────────────────────────────────────────────────

/// The automatic quality's period accounts: link-busy skips and the send task's busy time at
/// the period's start.
struct Auto {
    since: Instant,
    skipped: u32,
    busy_us: u32,
}

impl Auto {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            skipped: 0,
            busy_us: 0,
        }
    }

    /// At the end of a period: the quality for the next one (only while JPEG frames go out
    /// with `auto_quality` on; otherwise the set quality).
    fn step(&mut self) {
        let now = Instant::now();
        let elapsed = now - self.since;
        if elapsed < Duration::from_millis(u64::from(AUTO_QUALITY_PERIOD_MS)) {
            return;
        }
        let busy_us = SEND_BUSY_US.load(Ordering::Relaxed);
        let busy_pct = (u64::from(busy_us.wrapping_sub(self.busy_us)) * 100
            / elapsed.as_micros().max(1))
        .min(100) as u8;
        self.busy_us = busy_us;
        self.since = now;
        shared(|s| {
            let skipped = s.counters.frames_skipped.wrapping_sub(self.skipped);
            self.skipped = s.counters.frames_skipped;
            let set = s.stream.jpeg_quality;
            s.encoder.quality = if s.stream.auto_quality && s.active(now) {
                espion_proto::cam::auto_quality(s.encoder.quality, set, skipped > 0, busy_pct)
            } else {
                set
            };
        });
    }
}

#[inline(never)]
fn log_no_encoder(e: Error) {
    error!("stream: no JPEG encoder ({e:?}), JPEG frames are not sent");
}

#[inline(never)]
fn log_ready(base: usize) {
    info!(
        "stream: JPEG encoder ready, {OUT_BUFFERS} output buffers of {OUT_LEN} B in PSRAM at {base:#010x}"
    );
}
