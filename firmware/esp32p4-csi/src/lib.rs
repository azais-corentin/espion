//! MIPI CSI-2 receiver driver for the ESP32-P4.
//!
//! esp-hal 1.2.2 has no MIPI CSI, camera or ISP driver for the P4. This crate fills the
//! gap on the `esp32p4` PAC and follows esp-hal driver conventions (`Config`, typed pins,
//! DMA buffers, blocking and async APIs) without being upstreamed. espion uses it for the
//! OV9281: 2 data lanes, RAW8, ISP bypassed. See `docs/architecture.md`.
//!
//! Not implemented yet.

#![no_std]
