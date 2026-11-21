//! The blob detector on the second HP core of the ESP32-P4, started bare (performance plan
//! step 5.3, shape B of the two-core analysis): no scheduler, no interrupts, no critical
//! section, no logging on core 1. `README.md` has the rules for code on core 1, the API and
//! what was verified where.
//!
//! - [`start`] (core 0, once) starts core 1 on its own stack with the detector, its PIE token,
//!   its trap vector and the DMA channel of the stream job's copies; it returns the [`Core1`]
//!   handle, which lives for ever.
//! - Strips: core 0 builds a `CsiStrips` and gives it to [`Core1::hand_over`], which splits
//!   it, posts the `StripReader` to core 1 and returns the `StripControl`. Before each run
//!   (strip path stopped): [`Core1::set_run`] (the [`Job`]: detector or pixel check) or
//!   [`Core1::set_stream`] (the stream job); after each `StripControl::stop`:
//!   [`Core1::note_stop`]. Core 1 polls the reader, runs the job on each strip and publishes a
//!   [`FrameResult`] per frame.
//! - The stream job: per strip the detector and the background sample (detection on,
//!   [`Core1::set_detect`]) and a DMA copy of the strip into a frame buffer in PSRAM taken
//!   from a free set ([`give_frame`]); per frame the refined measure of the kept blobs on the
//!   assembled frame (one blob per loop turn, between strips) and the result, which hands the
//!   frame buffer to core 0. [`Core1::abort_stream`] and [`Core1::stream_idle`]: core 1 drops
//!   what it holds, between two strips or two blobs.
//! - Results go through an overwrite-never single-producer single-consumer queue ([`spsc`]).
//!   A [`Monitor`] on core 0 calls `service()` and drains the results of a strip run every
//!   millisecond, and once a second logs core 1's figures and checks its heartbeat and trap
//!   record; [`health`] is that check alone.
//! - Feature `markers`: core 1 toggles its dedicated-IO channels [`MARKER_STRIP`] and
//!   [`MARKER_FRAME`] (a CSR read and write each, no memory access); the module `markers`
//!   routes them to pads from core 0 and reads every marker pad back at boot.

#![no_std]

#[cfg(feature = "markers")]
pub mod markers;
mod monitor;
mod runner;
pub mod spsc;

pub use monitor::{
    Health, Monitor, health, log_check_second2, log_summary, report_dead, report_trap,
};
pub use runner::{
    CHECK, CHECK_MAX, CheckCounters, CheckMaxSnap, CheckMaxima, CheckSnap, Core1, FrameBuffers,
    FrameResult, HOLD_TIMES, HoldTimes, HoldTimesSnap, Job, MAX_BLOBS, MAX_FRAME_BUFFERS,
    MAX_SPOTS, NO_BUFFER, Run, SkipUsed, StreamDetect, StreamRun, Trap, abandoned, age,
    copy_counts, give_frame, heartbeat, row_sum, skip_used, start, trap,
};

/// The dark skip of the detector on core 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipChoice {
    /// The portable skip (`espion_vision::Scalar`).
    Scalar,
    /// PIE (`esp32p4_pie::Pie`), enabled by core 1 for itself; the scalar skip if it refuses.
    Pie,
}

/// Core 1's dedicated-IO channel toggled when the reader has handed out a strip of a detector
/// run, before the detector reads it (feature `markers`).
pub const MARKER_STRIP: u8 = 0;
/// Core 1's dedicated-IO channel toggled when a frame's blobs are final: its result published,
/// or dropped on a full queue (feature `markers`).
pub const MARKER_FRAME: u8 = 1;
