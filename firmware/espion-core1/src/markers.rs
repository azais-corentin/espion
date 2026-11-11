//! Marker pins (feature `markers`), core 0's side: routing a dedicated-IO channel of either
//! core to a pad, and the boot self-check that reads every marker pad back. Core 1's side is
//! in the runner: its entry enables channels [`MARKER_STRIP`] and [`MARKER_FRAME`] of core 1
//! (`esp32p4_perf::marker::enable`) and its loop toggles them. Nothing here is reachable from
//! core 1's entry.
//!
//! Routing (esp-hal 1.2.2, `gpio/dedicated`): channel n of core 0 is the GPIO-matrix output
//! signal `CPU_GPIO_n` (214 + n), of core 1 `CPU_GPIO_{8 + n}` (222 + n). esp-hal's
//! `DedicatedGpioOutput` picks the signal of the core it runs on and must stay on that core,
//! so it cannot route core 1's channels; [`Marker::new`] routes either core's channel with
//! `OutputSignal::connect_to`, and the driving core enables its output itself. The matrix
//! leaves the output enable to the signal (`oen_sel` 0), so a pad stays undriven until its
//! core has enabled the channel; the pull-down holds it low until then.
//!
//! The self-check ([`self_check`]): each marker pad's input path is on, so the GPIO input
//! register reads back what the channel drives, without a wire. A GPIO interrupt on any edge
//! (core 0 only, bound directly: esp-hal's `Io` handler would also map `GPIO_INT1` to core 1)
//! reads `GPIO_IN` and counts the level changes of each pad. Once a run toggles the markers
//! it counts for one second, then waits until the markers rest (between runs) to stop
//! listening and log the counts, so its log lines never hold up a run on core 0.
//!
//! [`MARKER_STRIP`]: crate::MARKER_STRIP
//! [`MARKER_FRAME`]: crate::MARKER_FRAME

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::{Event, Flex, InputConfig, OutputSignal, Pin, Pull};
use esp_hal::handler;
use esp_hal::interrupt::{self, Priority};
use esp_hal::peripherals::Interrupt;
use esp_hal::system::Cpu;
use esp32p4_perf::{CpuSample, marker};
use log::{info, warn};

/// Core 1's channels, enabled by its entry.
pub(crate) const CORE1_MASK: u32 = (1 << crate::MARKER_STRIP) | (1 << crate::MARKER_FRAME);

/// GPIO-matrix output signal of each dedicated-IO channel: `[core][channel]`.
const SIGNALS: [[OutputSignal; 8]; 2] = [
    [
        OutputSignal::CPU_GPIO_0,
        OutputSignal::CPU_GPIO_1,
        OutputSignal::CPU_GPIO_2,
        OutputSignal::CPU_GPIO_3,
        OutputSignal::CPU_GPIO_4,
        OutputSignal::CPU_GPIO_5,
        OutputSignal::CPU_GPIO_6,
        OutputSignal::CPU_GPIO_7,
    ],
    [
        OutputSignal::CPU_GPIO_8,
        OutputSignal::CPU_GPIO_9,
        OutputSignal::CPU_GPIO_10,
        OutputSignal::CPU_GPIO_11,
        OutputSignal::CPU_GPIO_12,
        OutputSignal::CPU_GPIO_13,
        OutputSignal::CPU_GPIO_14,
        OutputSignal::CPU_GPIO_15,
    ],
];

/// The calling core's channel the cost measurement toggles: never routed to a pad.
const COST_CHANNEL: u32 = 7;
/// After the first marker edge, how long the self-check waits for every marker to toggle
/// before it counts anyway (a run that drives only some of them).
const ALL_WAIT: Duration = Duration::from_secs(5);
/// The self-check's counting window.
const WINDOW: Duration = Duration::from_secs(1);
/// After the window, how long every marker must stay still before the self-check logs. Its
/// lines hold core 0's executor for about 3 ms at 2 Mbaud, longer than a strip consumer on
/// core 0 may keep a strip (3 strip periods, 2 ms, in the 1280x800 ring of 4): a first
/// version that logged in the middle of `poc-p4-eth`'s strip soak saw that run's only DMA
/// halt [INFERENCE: the log the cause]. Between runs the markers rest.
const QUIET: Duration = Duration::from_millis(20);
/// The longest the self-check waits for that rest before it logs anyway.
const QUIET_MAX: Duration = Duration::from_secs(120);

/// A marker pad: a dedicated-IO channel routed to a GPIO below 32 (bank 0 of `GPIO_IN`).
pub struct Marker {
    name: &'static str,
    gpio: u8,
    core: Cpu,
    channel: u8,
    pad: Flex<'static>,
}

impl Marker {
    /// Routes channel `channel` (0 to 7) of `core` to `pin` through the GPIO matrix, with the
    /// pad's input path on (for [`self_check`]) and its pull-down on. The core that drives
    /// the channel enables its output with `esp32p4_perf::marker::enable` (core 1: its entry,
    /// for [`crate::MARKER_STRIP`] and [`crate::MARKER_FRAME`]). Call on core 0.
    pub fn new(name: &'static str, pin: impl Pin + 'static, core: Cpu, channel: u8) -> Self {
        let gpio = pin.number();
        assert!(gpio < 32 && channel < 8);
        let mut pad = Flex::new(pin);
        pad.apply_input_config(&InputConfig::default().with_pull(Pull::Down));
        pad.set_input_enable(true);
        SIGNALS[core as usize][usize::from(channel)].connect_to(&pad);
        Self {
            name,
            gpio,
            core,
            channel,
            pad,
        }
    }

    fn mask(&self) -> u32 {
        1 << self.gpio
    }
}

/// The marker pads the GPIO interrupt watches (bit n: GPIO n).
static WATCH: AtomicU32 = AtomicU32::new(0);
/// Their levels at the last interrupt.
static LEVELS: AtomicU32 = AtomicU32::new(0);
/// Level changes per GPIO.
static CHANGES: [AtomicU32; 32] = [const { AtomicU32::new(0) }; 32];
/// Edges latched with the level back where it was at the last interrupt (two edges or more
/// between two reads): 0 when the interrupt keeps up.
static MERGED: [AtomicU32; 32] = [const { AtomicU32::new(0) }; 32];

fn gpio() -> &'static esp32p4::gpio::RegisterBlock {
    // SAFETY: the GPIO block's registers; the handler only reads `GPIO_IN` and `GPIO_STATUS`
    // and clears the status bits of the watched pads.
    unsafe { &*esp32p4::GPIO::ptr() }
}

/// GPIO interrupt (core 0): the edges latched on the watched pads, cleared before `GPIO_IN`
/// is read so that an edge after the read is latched again.
#[handler(priority = Priority::Priority1)]
fn on_edge() {
    let g = gpio();
    let watch = WATCH.load(Ordering::Relaxed);
    let latched = g.status().read().bits() & watch;
    // SAFETY: write-1-to-clear of the watched pads' status bits only.
    g.status_w1tc().write(|w| unsafe { w.bits(latched) });
    let now = g.in_().read().bits() & watch;
    let last = LEVELS.load(Ordering::Relaxed);
    let mut bits = latched;
    while bits != 0 {
        let n = bits.trailing_zeros() as usize;
        bits &= bits - 1;
        let counter = if (now ^ last) & (1 << n) != 0 {
            &CHANGES[n]
        } else {
            &MERGED[n]
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
    LEVELS.store((last & !latched) | (now & latched), Ordering::Relaxed);
}

/// The counters of `m` now: (level changes, merged edges).
fn snapshot(m: &Marker) -> (u32, u32) {
    let n = usize::from(m.gpio);
    (
        CHANGES[n].load(Ordering::Relaxed),
        MERGED[n].load(Ordering::Relaxed),
    )
}

/// The boot self-check of the marker pads `markers` (at most 8). Logs the cost of one marker
/// toggle on core 0, then listens on every pad; at the first edge it waits until every marker
/// has toggled (at most [`ALL_WAIT`]), counts each pad's level changes for [`WINDOW`], waits
/// until the markers rest for [`QUIET`] (at most [`QUIET_MAX`]), stops listening and logs the
/// counts. Runs on core 0, in a task of its own.
pub async fn self_check(markers: &mut [Marker]) {
    log_toggle_cost();
    let n = markers.len().min(8);
    let markers = &mut markers[..n];
    let mask = markers.iter().fold(0, |m, k| m | k.mask());
    LEVELS.store(gpio().in_().read().bits() & mask, Ordering::Relaxed);
    WATCH.store(mask, Ordering::Relaxed);
    interrupt::bind_handler(Interrupt::GPIO, on_edge);
    for m in markers.iter_mut() {
        m.pad.clear_interrupt();
        m.pad.listen(Event::AnyEdge);
    }
    info!("marker self-check: listening on GPIO mask {mask:#010x}, waiting for a run");
    let changes = |ms: &[Marker]| ms.iter().map(|m| snapshot(m).0).sum::<u32>();
    while changes(markers) == 0 {
        Timer::after_millis(10).await;
    }
    let first = Instant::now();
    while markers.iter().any(|m| snapshot(m).0 == 0) && first.elapsed() < ALL_WAIT {
        Timer::after_millis(10).await;
    }
    let mut before = [(0, 0); 8];
    for (b, m) in before.iter_mut().zip(markers.iter()) {
        *b = snapshot(m);
    }
    let t0 = Instant::now();
    Timer::after(WINDOW).await;
    let mut after = [(0, 0); 8];
    for (a, m) in after.iter_mut().zip(markers.iter()) {
        *a = snapshot(m);
    }
    let us = t0.elapsed().as_micros();
    let mut seen = changes(markers);
    let mut still = Instant::now();
    while still.elapsed() < QUIET && t0.elapsed() < QUIET_MAX {
        Timer::after_millis(5).await;
        let now = changes(markers);
        if now != seen {
            (seen, still) = (now, Instant::now());
        }
    }
    let waited = t0.elapsed().as_millis() - us / 1000;
    for m in markers.iter_mut() {
        m.pad.unlisten();
        m.pad.clear_interrupt();
    }
    interrupt::disable(Cpu::ProCpu, Interrupt::GPIO);
    WATCH.store(0, Ordering::Relaxed);
    info!(
        "marker self-check: level changes read back from GPIO_IN in {us} us (logged {waited} ms after the window):"
    );
    for ((m, b), a) in markers.iter().zip(before).zip(after) {
        let (edges, merged) = (a.0.wrapping_sub(b.0), a.1.wrapping_sub(b.1));
        let level = u8::from(m.pad.is_high());
        info!(
            "marker self-check: {} GPIO{} core {} channel {}: {edges} edges, {merged} merged, level {level}",
            m.name, m.gpio, m.core as u8, m.channel
        );
        if edges == 0 {
            warn!("marker self-check: {} GPIO{} never toggled", m.name, m.gpio);
        }
    }
}

/// Logs what one marker toggle costs on this core: 16 toggles back to back between two
/// `CpuSample`s, the least of 64 tries, against the same samples with nothing between them.
/// Channel [`COST_CHANNEL`] is not routed and ends where it began (16 toggles).
#[inline(never)]
fn log_toggle_cost() {
    const M: u32 = 1 << COST_CHANNEL;
    let mut empty = (u32::MAX, 0);
    let mut sixteen = (u32::MAX, 0);
    for _ in 0..64 {
        let a = CpuSample::now();
        let d = CpuSample::now().since(&a);
        if d.cycles < empty.0 {
            empty = (d.cycles, d.instructions);
        }
        let a = CpuSample::now();
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        marker::toggle(M);
        let d = CpuSample::now().since(&a);
        if d.cycles < sixteen.0 {
            sixteen = (d.cycles, d.instructions);
        }
    }
    info!(
        "marker: 16 toggles {} cycles {} instructions, empty window {} cycles {} instructions (least of 64)",
        sixteen.0, sixteen.1, empty.0, empty.1
    );
}
