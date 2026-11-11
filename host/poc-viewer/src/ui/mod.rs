//! The window's look and its controls, painted here rather than egui's stock widgets.
//!
//! - [`theme`]: colours, type, spacing; the hue of each pipeline stage.
//! - [`widgets`]: the controls of the rail and the bars.
//! - [`picture`]: the picture, fitted or zoomed and moved, with the pixel under the pointer.
//! - [`window_picker`]: the sensor's active area with the window to read.
//! - [`diagram`]: the pipeline timing diagrams.

pub mod diagram;
pub mod picture;
pub mod theme;
pub mod widgets;
pub mod window_picker;

/// "on" or "off".
pub const fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

/// A rate in millihertz, in hertz with three decimals.
pub fn hz(mhz: u32) -> String {
    format!("{}.{:03} Hz", mhz / 1000, mhz % 1000)
}
