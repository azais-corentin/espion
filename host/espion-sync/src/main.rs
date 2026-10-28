//! espion-sync: time-sync coordinator on the host.
//!
//! The network clock is PTP (IEEE 1588): the camera boards elect a grandmaster among
//! themselves (BMCA) with automatic failover (see `docs/timing.md`). This tool sets the
//! shared frame epoch, maps network time to host time and reports the current grandmaster
//! and per-board sync health.
//!
//! Not implemented yet.

fn main() {}
