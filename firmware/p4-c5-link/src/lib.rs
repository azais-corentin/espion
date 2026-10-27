//! SPI link between the ESP32-P4 (SPI master: camera, vision, LEDs) and the ESP32-C5
//! (SPI slave: Wi-Fi, UDP, FTM clock sync).
//!
//! The P4 pushes per-frame centroid batches; the C5 raises `DATA_READY` when it has a
//! message for the P4 (configuration, SYNC edge index and timescale, sync state). See
//! `docs/architecture.md` and `docs/timing.md`.
//!
//! Used by `firmware/cam-p4` and `firmware/radio-c5` as a path dependency, and tested on
//! the host as a member of the root workspace.
//!
//! Not implemented yet.

#![no_std]
