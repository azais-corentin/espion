//! espion-sync: time-sync coordinator on the host.
//!
//! The network clock lives on one camera board's ESP32-C5 (the elected FTM master, see
//! `docs/timing.md`). This tool selects that master, sets the shared frame epoch, maps
//! network time to host time and reports per-board sync health.
//!
//! Not implemented yet.

fn main() {}
