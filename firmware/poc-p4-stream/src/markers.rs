//! Marker pins (feature `markers`; README, "Marker pins"): a dedicated-IO channel of core 0
//! toggled as each datagram of the direct path (`net::chunk::send`: the stream's frame chunks
//! and the bench's datagrams) goes to the EMAC's TX ring, and core 1's two channels
//! (`Service::BenchVision`), on three pads of the board's 40-pin header that nothing else on
//! the board uses.
//!
//! Without the feature, [`toggle`] is empty and nothing is routed.

/// Core 0's channel `net::chunk::send` toggles when it has handed a datagram to the TX ring.
pub const DATAGRAM: u8 = 0;

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

    /// Core 0, datagram queued (`net::chunk::send`).
    pub const DATAGRAM_GPIO: u8 = 26;
    /// Core 1, strip taken (`Service::BenchVision`; `espion_core1::MARKER_STRIP`).
    pub const CORE1_STRIP_GPIO: u8 = 20;
    /// Core 1, frame done (`Service::BenchVision`; `espion_core1::MARKER_FRAME`).
    pub const CORE1_FRAME_GPIO: u8 = 21;

    /// Routes the three marker pads, enables core 0's channel (core 1 enables its own at its
    /// entry) and spawns the self-check. Call on core 0 before core 1 starts.
    pub fn start(spawner: Spawner) {
        // SAFETY: GPIO20, GPIO21 and GPIO26 are used by nothing else in this firmware, and on
        // the board only by the 40-pin header.
        let pad = |gpio| unsafe { AnyPin::steal(gpio) };
        let markers = [
            Marker::new("core0 datagram", pad(DATAGRAM_GPIO), Cpu::ProCpu, DATAGRAM),
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
        esp32p4_perf::marker::enable(1 << DATAGRAM);
        spawner.spawn(check(markers).unwrap());
    }

    #[embassy_executor::task]
    async fn check(mut markers: [Marker; 3]) {
        self_check(&mut markers).await;
    }
}
