//! Marker pins (feature `markers`; README, "Marker pins"): dedicated-IO channels toggled by
//! the strip soak on core 0 and by the runner on core 1, on four pads of the board's 40-pin
//! header that nothing else on the board uses.
//!
//! Without the feature, [`toggle`] is empty and nothing is routed.

/// Core 0's channel the strip soak toggles when `wait_strip` has handed it a strip, before the
/// detector.
pub const STRIP: u8 = 0;
/// Core 0's channel the strip soak toggles when a frame's blobs are final (`finish`).
pub const FRAME: u8 = 1;

/// Toggles core 0's marker channel `channel` (nothing without feature `markers`).
#[inline(always)]
pub fn toggle(channel: u8) {
    #[cfg(feature = "markers")]
    esp32p4_perf::marker::toggle(1 << channel);
    #[cfg(not(feature = "markers"))]
    let _ = channel;
}

#[cfg(feature = "markers")]
pub use on::start;

#[cfg(feature = "markers")]
mod on {
    use embassy_executor::Spawner;
    use esp_hal::gpio::AnyPin;
    use esp_hal::system::Cpu;
    use espion_core1::markers::{Marker, self_check};

    use super::*;

    /// Core 0, strip taken (strip soak).
    pub const CORE0_STRIP_GPIO: u8 = 22;
    /// Core 0, frame done (strip soak).
    pub const CORE0_FRAME_GPIO: u8 = 23;
    /// Core 1, strip taken (`RUN_CORE1`; `espion_core1::MARKER_STRIP`).
    pub const CORE1_STRIP_GPIO: u8 = 20;
    /// Core 1, frame done (`RUN_CORE1`; `espion_core1::MARKER_FRAME`).
    pub const CORE1_FRAME_GPIO: u8 = 21;

    /// Routes the four marker pads, enables core 0's two channels (core 1 enables its own at
    /// its entry) and spawns the self-check. Call on core 0 before core 1 starts.
    pub fn start(spawner: Spawner) {
        // SAFETY: GPIO20 to GPIO23 are used by nothing else in this firmware, and on the
        // board only by the 40-pin header.
        let pad = |gpio| unsafe { AnyPin::steal(gpio) };
        let markers = [
            Marker::new("core0 strip", pad(CORE0_STRIP_GPIO), Cpu::ProCpu, STRIP),
            Marker::new("core0 frame", pad(CORE0_FRAME_GPIO), Cpu::ProCpu, FRAME),
            Marker::new(
                "core1 strip",
                pad(CORE1_STRIP_GPIO),
                Cpu::AppCpu,
                espion_core1::MARKER_STRIP,
            ),
            Marker::new(
                "core1 frame",
                pad(CORE1_FRAME_GPIO),
                Cpu::AppCpu,
                espion_core1::MARKER_FRAME,
            ),
        ];
        esp32p4_perf::marker::enable((1 << STRIP) | (1 << FRAME));
        spawner.spawn(check(markers).unwrap());
    }

    #[embassy_executor::task]
    async fn check(mut markers: [Marker; 4]) {
        self_check(&mut markers).await;
    }
}
