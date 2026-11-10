//! poc-viewer: live view and control of the OV9281 on the ESP32-P4-ETH proof of concept, over
//! the camera stream of `espion-proto` (`docs/protocol.md`, "Camera stream").
//!
//! The library holds everything but the window, so the tests run it without one:
//!
//! - [`camera`]: the camera settings checked with the `ov9281` driver, as the firmware checks
//!   them, and the figures the viewer shows before anything is sent.
//! - [`net`]: the network thread: requests with retry, polling, chunk reassembly, the newest
//!   complete frame, statistics.
//! - [`fake`]: a fake camera that speaks the protocol (`--fake`, the `poc-fake-cam` binary,
//!   the tests).
//! - [`addr`]: camera addresses with an interface scope, and the local address to bind.

pub mod addr;
pub mod camera;
pub mod fake;
pub mod net;
