//! The JPEG stage of the stream ([`Encoding::Jpeg`]): the encode task, its output buffers and
//! the automatic quality.
//!
//! Pipeline, all on core 0 and one frame per stage, never a queue:
//!
//! 1. The capture task offers every captured RAW8 frame it would send ([`offer`]): the frame
//!    waits in [`INPUT`], a one-frame slot. A frame already waiting there is replaced and
//!    counted as one the encoder could not take ([`EncoderState::frames_skipped`]), so the
//!    encoder always gets the newest frame. A captured frame is held at most twice (the one
//!    being encoded, the one waiting), which the receiver's four buffers allow.
//! 2. The encode task ([`task`]) takes it and encodes it with the hardware encoder straight
//!    from the receiver's buffer (no CPU pass over the pixels) into a free output buffer, then
//!    gives the receiver's buffer back at once.
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
use core::ops::Deref;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, with_timeout};
use esp32p4_csi::Frame;
use esp32p4_jpeg::{Config, Encoder, Error};
use espion_proto::cam::{AUTO_QUALITY_PERIOD_MS, Encoding, JPEG_FRAME_MAX};
use log::{error, info};

use super::{JOB, Job, Payload, SEND_BUSY_US, shared};
use crate::cpu;

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
    pub frame: Frame,
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

/// Test hooks (set by diag): the next this many encodes get a [`SMALL_OUTPUT`]-byte output (a
/// file that does not fit), and encode-only (files are dropped, not sent: the encode time
/// without the network's traffic on the bus).
pub(super) static SMALL_OUTPUTS: AtomicU32 = AtomicU32::new(0);
pub(super) static ENCODE_ONLY: AtomicBool = AtomicBool::new(false);
const SMALL_OUTPUT: usize = 4096;

// ── The task ────────────────────────────────────────────────────────────────────────────────

/// Creates the encoder (on this core, which takes its interrupts) and encodes for good.
/// `out_mem`: [`OUT_BUFFERS`] x [`OUT_LEN`] bytes of PSRAM, 128-byte aligned.
#[embassy_executor::task]
pub(super) async fn task(out_mem: &'static mut [u8]) {
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
    log_ready(base);
    let mut auto = Auto::new();
    loop {
        let input = INPUT.wait().await;
        auto.step();
        encode(&mut enc, input).await;
    }
}

/// Encodes `input` and hands the file to the send task, unless the stream moved on. The work
/// around the await is in `#[inline(never)]` helpers, which keeps this future's poll frame
/// small.
async fn encode(enc: &mut Encoder, input: Input) {
    let Some((blank, cap)) = prepare(enc, &input) else {
        return;
    };
    let start = Instant::now();
    let r = match with_timeout(
        ENCODE_TIMEOUT,
        enc.encode(&input.frame, &mut blank.mem[..cap]),
    )
    .await
    {
        Ok(Ok(len)) => Ok(len),
        Ok(Err(e)) => Err(Some(e)),
        Err(_) => Err(None),
    };
    let us = start.elapsed().as_micros() as u32;
    finish(input, blank, r, us);
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
    let small = SMALL_OUTPUTS
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
        .is_ok();
    let cap = if small {
        SMALL_OUTPUT
    } else {
        cfg.max_output_len().min(OUT_LEN)
    };
    Some((blank, cap))
}

/// Gives the receiver's buffer back, counts the result (`Err(None)`: no end within
/// [`ENCODE_TIMEOUT`]) and hands the file on: the newest file waits for the send task,
/// replacing (and counting as skipped because the link was busy) one still waiting.
#[inline(never)]
fn finish(input: Input, blank: Blank, r: Result<usize, Option<Error>>, us: u32) {
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
    let job = Job {
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
    };
    if let Some(old) = JOB.try_take() {
        drop(old);
        shared(|s| s.counters.frames_skipped = s.counters.frames_skipped.wrapping_add(1));
    }
    JOB.signal(job);
}

/// Counts an encoder error (the driver resets the hardware before the next frame).
fn fail(what: &'static str, us: u32) {
    shared(|s| s.encoder.errors = s.encoder.errors.wrapping_add(1));
    critical_section::with(|cs| LAST_ERROR.borrow(cs).set(what));
    LAST_ERROR_US.store(us, Ordering::Relaxed);
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
