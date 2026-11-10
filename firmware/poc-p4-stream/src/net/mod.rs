//! Ethernet and IPv6 on the Waveshare ESP32-P4-ETH: the EMAC (esp-hal's `ethernet` driver, RMII)
//! with the IP101GRI PHY, embassy-net with the fixed link-local address [`ADDRESS`], and the
//! tasks that run them.
//!
//! Pins, from the Waveshare schematic (`ESP32-P4-ETH-datasheet.pdf`, sheet 1, the IP101GRI
//! block U10), each matching Waveshare's ESP-IDF example (`ESP32-P4-Platform`,
//! `examples/esp-idf/11_ethernetbasic`, `ethernet_init` Kconfig defaults for the P4: MDC 31,
//! MDIO 52, PHY reset 51, PHY address 1, RMII clock input) and ESPHome's board page (MDC 31,
//! MDIO 52, power 51, `CLK_EXT_IN` on GPIO50, `phy_addr: 1`):
//!
//! | Signal | U10 pin | P4 |
//! |---|---|---|
//! | TXD0 | 9 | GPIO34 |
//! | TXD1 | 8 | GPIO35 (also the BOOT strap, driven by the CH343 auto-reset circuit U5 during reset) |
//! | TX_EN | 5 | GPIO49 |
//! | RXD0 | 18 | GPIO29 |
//! | RXD1 | 17 | GPIO30 |
//! | CRS_DV | 19 | GPIO28 |
//! | MDC | 22 | GPIO31 |
//! | MDIO | 23 (1.5 k pull-up R59) | GPIO52 |
//! | 50 MHz RMII reference clock | 14 (50M_CLKO) tied to 10 (50M_CLKI) | GPIO50, input |
//! | RESET (active low; R67/R70 not fitted) | 32 | GPIO51 |
//!
//! The PHY makes the reference clock from its own 25 MHz crystal (pins 2 and 3) and drives it
//! into its own clock input and into GPIO50 (`EMAC_RMII_CLK`): esp-hal's `ExternalRefClock`.
//! PHY address 1: strap PHY_AD0 (pin 11) high through R65 (5.1 k to PHY_3V3), PHY_AD3 (pin 12)
//! low through R63. RMII mode: strap COL/RMII (pin 4) high through R49.

mod device;
mod ip101;

use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_executor::Spawner;
use embassy_net::{Config, Ipv6Address, Ipv6Cidr, Runner, Stack, StackResources, StaticConfigV6};
use embassy_time::{Duration, Timer, with_timeout};
use esp_hal::Async;
use esp_hal::ethernet::clock::ExternalRefClock;
use esp_hal::ethernet::{Ethernet, EthernetDmaStorage, RmiiPinBundle};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::{
    ETH, GPIO28, GPIO29, GPIO30, GPIO31, GPIO34, GPIO35, GPIO49, GPIO50, GPIO51, GPIO52,
};
use log::{error, info, warn};
use static_cell::{ConstStaticCell, StaticCell};

pub use device::{Stats, stats};
pub use ip101::{LINK_POLL, Report, report};

use crate::cpu;

/// The board's IPv6 address: a fixed link-local one, no SLAAC, no DHCP, no IPv4.
pub const ADDRESS: Ipv6Address = Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0x4553, 0x9281);
/// Prefix length of [`ADDRESS`].
pub const PREFIX_LEN: u8 = 64;
/// MDIO address of U10 (straps PHY_AD0 = 1, PHY_AD3 = 0).
pub const PHY_ADDRESS: u8 = 1;
/// RX DMA descriptors, each with a 1524-byte buffer in internal RAM.
pub const RX_DESCRIPTORS: usize = 16;
/// TX DMA descriptors, each with a 1524-byte buffer in internal RAM.
pub const TX_DESCRIPTORS: usize = 16;
/// Socket slots of the stack.
pub const SOCKETS: usize = 4;
/// How long the PHY reset is held low, and how long to wait after releasing it. The IP101G
/// needs at least 10 ms of reset [INFERENCE: from the IP101G datasheet's reset timing, not
/// measured here].
const PHY_RESET_MS: u64 = 10;
/// How long [`start`] waits for the link before it returns anyway.
const LINK_TIMEOUT: Duration = Duration::from_secs(8);

/// The driver embassy-net runs: esp-hal's EMAC driver with the IP101 PHY, timed per frame.
pub type Driver = device::Measured<Ethernet<'static, Async, ip101::Ip101>>;

/// The EMAC and the RMII pads.
pub struct Pins {
    pub eth: ETH<'static>,
    pub ref_clk: GPIO50<'static>,
    pub rxd0: GPIO29<'static>,
    pub rxd1: GPIO30<'static>,
    pub crs_dv: GPIO28<'static>,
    pub txd0: GPIO34<'static>,
    pub txd1: GPIO35<'static>,
    pub tx_en: GPIO49<'static>,
    pub mdc: GPIO31<'static>,
    pub mdio: GPIO52<'static>,
}

/// Descriptor rings and their buffers (internal RAM; esp-hal's `InternalMemory`, 64-byte aligned).
pub type DmaStorage = EthernetDmaStorage<RX_DESCRIPTORS, TX_DESCRIPTORS>;

/// The DMA storage, in the `.dram_dma` section of `dram_dma.x`: at 0x4FF40000 or above, where
/// esp-hal's DMA buffer check (which assumes a 256 KiB L2 cache) accepts it. NOLOAD, so
/// [`dma_storage`] zeroes it.
#[unsafe(link_section = ".dram_dma")]
static mut DMA: MaybeUninit<DmaStorage> = MaybeUninit::uninit();
static DMA_TAKEN: AtomicBool = AtomicBool::new(false);

/// The DMA storage, zeroed (the value of `EthernetDmaStorage::new()`). Once only.
fn dma_storage() -> &'static mut DmaStorage {
    assert!(
        !DMA_TAKEN.swap(true, Ordering::AcqRel),
        "DMA storage taken twice"
    );
    // SAFETY: first and only reference to `DMA` (checked above). The storage holds only
    // integers (descriptor words and buffer bytes), so all-zero is a valid value, and it is
    // the one `EthernetDmaStorage::new()` builds.
    unsafe {
        let p = (&raw mut DMA).cast::<DmaStorage>();
        p.write_bytes(0, 1);
        &mut *p
    }
}
static RESOURCES: ConstStaticCell<StackResources<SOCKETS>> =
    ConstStaticCell::new(StackResources::new());
static PHY_RESET: StaticCell<Output<'static>> = StaticCell::new();

/// Resets the PHY (`phy_reset`, U10 pin 32), brings up the EMAC and the stack, spawns the
/// runner and the link task on `spawner`, and waits (up to [`LINK_TIMEOUT`]) for the link.
/// `None` if the EMAC or the PHY does not come up.
pub async fn start(
    spawner: Spawner,
    pins: Pins,
    phy_reset: GPIO51<'static>,
) -> Option<Stack<'static>> {
    let reset = PHY_RESET.init(Output::new(phy_reset, Level::Low, OutputConfig::default()));
    Timer::after_millis(PHY_RESET_MS).await;
    reset.set_high();
    Timer::after_millis(PHY_RESET_MS).await;
    let (stack, runner) = bring_up(pins)?;
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(link_task().unwrap());
    if with_timeout(LINK_TIMEOUT, stack.wait_link_up())
        .await
        .is_err()
    {
        warn!(
            "net: no link after {} s, continuing",
            LINK_TIMEOUT.as_secs()
        );
    }
    Some(stack)
}

/// The EMAC driver (which initialises the PHY over MDIO) and the stack.
#[allow(
    clippy::large_stack_frames,
    reason = "once at boot, on the main stack: embassy-net builds its interface state there"
)]
#[inline(never)]
fn bring_up(p: Pins) -> Option<(Stack<'static>, Runner<'static, Driver>)> {
    // The factory MAC from eFuse (Espressif OUI, unique per chip). The P4 has no radio that
    // would use it too.
    let mut mac = [0u8; 6];
    mac.copy_from_slice(esp_hal::efuse::base_mac_address().as_bytes());
    let eth = match Ethernet::new(
        p.eth,
        dma_storage(),
        mac,
        ip101::Ip101::new(PHY_ADDRESS),
        RmiiPinBundle {
            clock: ExternalRefClock::new(p.ref_clk),
            rxd0: p.rxd0,
            rxd1: p.rxd1,
            rx_dv: p.crs_dv,
            txd0: p.txd0,
            txd1: p.txd1,
            tx_en: p.tx_en,
            mdc: p.mdc,
            mdio: p.mdio,
        },
    ) {
        Ok(e) => {
            log_opmode(tx_threshold_mode());
            e.into_async()
        }
        Err(e) => {
            error!("net: EMAC/PHY init failed: {e:?}");
            return None;
        }
    };
    let r = report();
    info!(
        "net: EMAC up, MAC {:02x?}, PHY at MDIO {PHY_ADDRESS} id {:#06x} {:#06x}, DMA {RX_DESCRIPTORS} RX + {TX_DESCRIPTORS} TX descriptors ({} B)",
        mac,
        r.id.0,
        r.id.1,
        core::mem::size_of::<DmaStorage>()
    );
    let config = Config::ipv6_static(StaticConfigV6 {
        address: Ipv6Cidr::new(ADDRESS, PREFIX_LEN),
        gateway: None,
        dns_servers: Default::default(),
    });
    let rng = esp_hal::rng::Rng::new();
    let seed = u64::from(rng.random()) << 32 | u64::from(rng.random());
    let up = embassy_net::new(device::Measured(eth), config, RESOURCES.take(), seed);
    info!(
        "net: stack up, address {ADDRESS}/{PREFIX_LEN}, {SOCKETS} sockets ({} B of stack resources)",
        core::mem::size_of::<StackResources<SOCKETS>>()
    );
    Some(up)
}

/// EMAC DMA `DMAOPERATION_MODE` bits: transmit store-and-forward (TSF) and the transmit
/// threshold (TTC).
const OPMODE_TSF: u32 = 1 << 21;
const OPMODE_TTC_SHIFT: u32 = 14;

/// Switches the TX DMA from store-and-forward (esp-hal 1.2.2's setting) to threshold mode at
/// 64 bytes, as ESP-IDF does on every chip (`emac_hal_init_dma_default`). Returns
/// `DMAOPERATION_MODE` before and after.
///
/// With store-and-forward, frames larger than a few hundred bytes leave the P4 broken: most
/// never reach the host's NIC and the rest carry wrong UDP/ICMPv6 checksums (measured: 56-byte
/// pings pass, 400-byte ones fail, 1452-byte datagrams are lost or fail the checksum)
/// [INFERENCE: the P4's TX FIFO is smaller than a full frame; ESP-IDF states 256 B for its RX
/// FIFO]. The checksum insertion works only in store-and-forward mode, so smoltcp computes
/// the checksums (`device`).
#[inline(never)]
fn tx_threshold_mode() -> (u32, u32) {
    // SAFETY: the EMAC was just started by `Ethernet::new` and nothing is queued yet; the TX
    // DMA is stopped while its mode changes.
    let dma = unsafe { &*esp32p4::EMAC_DMA::ptr() };
    let before = dma.dmaoperation_mode().read().bits();
    dma.dmaoperation_mode()
        .modify(|_, w| w.start_stop_transmission_command().clear_bit());
    for _ in 0..100_000 {
        if dma.dmastatus().read().trans_proc_state().bits() == 0 {
            break;
        }
    }
    dma.dmaoperation_mode().modify(|_, w| {
        w.tx_str_fwd().clear_bit();
        // SAFETY: TTC 0 = 64 bytes.
        unsafe { w.tx_thresh_ctrl().bits(0) };
        w.flush_tx_fifo().set_bit()
    });
    for _ in 0..100_000 {
        if dma
            .dmaoperation_mode()
            .read()
            .flush_tx_fifo()
            .bit_is_clear()
        {
            break;
        }
    }
    dma.dmaoperation_mode()
        .modify(|_, w| w.start_stop_transmission_command().set_bit());
    (before, dma.dmaoperation_mode().read().bits())
}

#[inline(never)]
fn log_opmode((before, after): (u32, u32)) {
    info!(
        "net: EMAC DMA operation mode {before:#010x} -> {after:#010x} (TX store-and-forward {} -> {}, threshold code {})",
        before & OPMODE_TSF != 0,
        after & OPMODE_TSF != 0,
        (after >> OPMODE_TTC_SHIFT) & 7,
    );
}

/// EMAC registers for a run's report.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmacRegs {
    /// `DMASTATUS` (TX/RX process states in bits 22..20 / 19..17).
    pub status: u32,
    /// `DMAMISSEDFR`: frames missed for lack of RX descriptors and FIFO overflows (clears on
    /// read).
    pub missed: u32,
    /// `EMACDEBUG`: MAC and FIFO states.
    pub debug: u32,
}

/// Reads [`EmacRegs`].
pub fn emac_regs() -> EmacRegs {
    // SAFETY: reads of status registers; `DMAMISSEDFR` clears on read and nothing else reads it.
    let dma = unsafe { &*esp32p4::EMAC_DMA::ptr() };
    let mac = unsafe { &*esp32p4::EMAC_MAC::ptr() };
    EmacRegs {
        status: dma.dmastatus().read().bits(),
        missed: dma.dmamissedfr().read().bits(),
        debug: mac.emacdebug().read().bits(),
    }
}

/// embassy-net's runner: smoltcp and the driver.
#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Driver>) -> ! {
    cpu::name_current("net");
    runner.run().await
}

/// Wakes the runner every [`LINK_POLL`] so the PHY's link is read, and logs link changes.
#[embassy_executor::task]
async fn link_task() -> ! {
    cpu::name_current("link");
    let mut last: Option<(bool, bool, bool)> = None;
    loop {
        Timer::after(LINK_POLL).await;
        ip101::LINK_WAKER.wake();
        let r = report();
        let now = (r.up, r.speed_100, r.full_duplex);
        if r.polls > 0 && last != Some(now) {
            last = Some(now);
            log_link(&r);
        }
    }
}

#[inline(never)]
fn log_link(r: &Report) {
    if r.up {
        info!(
            "net: link up, {} Mbit/s {} duplex (BMSR {:#06x}, ANAR {:#06x}, ANLPAR {:#06x})",
            if r.speed_100 { 100 } else { 10 },
            if r.full_duplex { "full" } else { "half" },
            r.bmsr,
            r.anar,
            r.anlpar
        );
    } else {
        info!("net: link down (BMSR {:#06x})", r.bmsr);
    }
}
