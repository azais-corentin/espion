//! espion-sync: time-sync coordinator on the host.
//!
//! The network clock lives on one camera board's ESP32-C5, elected by the boards
//! themselves with automatic failover (see `docs/timing.md`). This tool sets the shared
//! frame epoch, maps network time to host time and reports the current master, timescale
//! and per-board sync health.
//!
//! Not implemented yet.

fn main() {}
