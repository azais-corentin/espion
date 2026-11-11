//! The blob detector on the second HP core of the ESP32-P4, started bare (performance plan
//! step 5.3, shape B of the two-core analysis): no scheduler, no interrupts, no critical
//! section, no logging on core 1. `README.md` has the rules for code on core 1, the API and
//! what was verified where.
//!
//! - [`start`] (core 0, once) starts core 1 on its own stack with the detector, its PIE token
//!   and its trap vector; it returns the [`Core1`] handle, which lives for ever.
//! - Core 0 builds a `CsiStrips` and gives it to [`Core1::hand_over`], which splits it, posts
//!   the `StripReader` to core 1 and returns the `StripControl`. Before each run (strip path
//!   stopped): [`Core1::set_run`]; after each `StripControl::stop`: [`Core1::note_stop`].
//! - Core 1 polls the reader, runs the [`Job`] on each strip and publishes a [`FrameResult`]
//!   per frame through an overwrite-never single-producer single-consumer queue ([`spsc`]).
//! - A [`Monitor`] on core 0 calls `service()` and drains the results every millisecond, and
//!   once a second logs core 1's figures and checks its heartbeat and trap record.
//! - Feature `markers`: core 1 toggles its dedicated-IO channels [`MARKER_STRIP`] and
//!   [`MARKER_FRAME`] (a CSR read and write each, no memory access); the module `markers`
//!   routes them to pads from core 0 and reads every marker pad back at boot.

#![no_std]

#[cfg(feature = "markers")]
pub mod markers;
mod monitor;
mod runner;
pub mod spsc;

pub use monitor::{Monitor, log_check_second2, log_summary, report_dead, report_trap};
pub use runner::{
    CHECK, CHECK_MAX, CheckCounters, CheckMaxSnap, CheckMaxima, CheckSnap, Core1, FrameResult,
    HOLD_TIMES, HoldTimes, HoldTimesSnap, Job, MAX_BLOBS, Run, SkipUsed, Trap, abandoned,
    heartbeat, row_sum, skip_used, start, trap,
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
