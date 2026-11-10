//! IP101GRI PHY (U10 on the ESP32-P4-ETH) over the standard Clause 22 registers, with the
//! link polled at most every [`LINK_POLL`].
//!
//! esp-hal's `GenericPhy` reads BMSR twice and ANLPAR on every `poll_link` and wakes the
//! caller each time (`wake_by_ref`). embassy-net calls `link_state` at the end of every poll of
//! its runner, so the runner never sleeps and each of its polls spends three MDIO frames (about
//! 20 us each at the 3.2 MHz MDC the driver sets at a 400 MHz CPU clock) on the link. This
//! driver keeps the last state, reads the PHY again only when [`LINK_POLL`] has passed, and is
//! woken by [`super::link_task`] instead.
//!
//! Speed and duplex come from the highest common ability of ANAR and ANLPAR (IEEE 802.3
//! Annex 28B priority: 100BASE-TX full, 100BASE-TX half, 10BASE-T full, 10BASE-T half).

use core::cell::Cell;
use core::task::Context;

use critical_section::Mutex;
use embassy_time::{Duration, Instant};
use esp_hal::asynch::AtomicWaker;
use esp_hal::delay::Delay;
use esp_hal::ethernet::mac::{Duplex, LinkState, Speed};
use esp_hal::ethernet::phy::{
    ANAR, ANLPAR, BMCR, BMSR, MdioBus, PHYIDR1, PHYIDR2, Phy, PhyError, an, bmcr, bmsr,
};

/// How often the link is read from the PHY.
pub const LINK_POLL: Duration = Duration::from_millis(500);
/// Longest wait for the BMCR reset bit to clear (IEEE 802.3 22.2.4.1.1: 0.5 s).
const RESET_TIMEOUT_US: u32 = 500_000;

/// Woken by the link task every [`LINK_POLL`]; registered by the runner's `link_state`.
pub static LINK_WAKER: AtomicWaker = AtomicWaker::new();

/// What the PHY reported at the last link poll.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// PHYIDR1 and PHYIDR2 read at init.
    pub id: (u16, u16),
    /// BMSR (second of two reads: the link bit latches low).
    pub bmsr: u16,
    /// Our advertisement.
    pub anar: u16,
    /// The link partner's abilities.
    pub anlpar: u16,
    /// Link up and auto-negotiation complete.
    pub up: bool,
    /// 100 Mbit/s (else 10), valid when `up`.
    pub speed_100: bool,
    /// Full duplex, valid when `up`.
    pub full_duplex: bool,
    /// Link polls so far.
    pub polls: u32,
}

static REPORT: Mutex<Cell<Report>> = Mutex::new(Cell::new(Report {
    id: (0, 0),
    bmsr: 0,
    anar: 0,
    anlpar: 0,
    up: false,
    speed_100: false,
    full_duplex: false,
    polls: 0,
}));

/// The last link poll's report.
pub fn report() -> Report {
    critical_section::with(|cs| REPORT.borrow(cs).get())
}

/// The PHY at a fixed MDIO address.
pub struct Ip101 {
    addr: u8,
    state: LinkState,
    last_poll: Option<Instant>,
}

impl Ip101 {
    pub const fn new(addr: u8) -> Self {
        Self {
            addr,
            state: LinkState {
                up: false,
                speed: Speed::_100M,
                duplex: Duplex::Full,
            },
            last_poll: None,
        }
    }

    fn read_link<M: MdioBus>(&mut self, mdio: &mut M) {
        let _ = mdio.read(self.addr, BMSR);
        let bmsr_val = mdio.read(self.addr, BMSR);
        let anar = mdio.read(self.addr, ANAR);
        let anlpar = mdio.read(self.addr, ANLPAR);
        let up = bmsr_val & bmsr::LINK_STATUS != 0 && bmsr_val & bmsr::AN_COMPLETE != 0;
        let common = anar & anlpar;
        let (speed, duplex) = if common & an::BASE_100_FULL != 0 {
            (Speed::_100M, Duplex::Full)
        } else if common & an::BASE_100_HALF != 0 {
            (Speed::_100M, Duplex::Half)
        } else if common & an::BASE_10_FULL != 0 {
            (Speed::_10M, Duplex::Full)
        } else {
            (Speed::_10M, Duplex::Half)
        };
        self.state = LinkState { up, speed, duplex };
        critical_section::with(|cs| {
            let cell = REPORT.borrow(cs);
            let r = cell.get();
            cell.set(Report {
                bmsr: bmsr_val,
                anar,
                anlpar,
                up,
                speed_100: speed == Speed::_100M,
                full_duplex: duplex == Duplex::Full,
                polls: r.polls.wrapping_add(1),
                ..r
            });
        });
    }
}

impl Phy for Ip101 {
    fn init<M: MdioBus>(&mut self, mdio: &mut M) -> Result<(), PhyError> {
        let id = (mdio.read(self.addr, PHYIDR1), mdio.read(self.addr, PHYIDR2));
        if matches!(id.0, 0x0000 | 0xFFFF) {
            return Err(PhyError::NotFound);
        }
        critical_section::with(|cs| {
            let cell = REPORT.borrow(cs);
            cell.set(Report { id, ..cell.get() });
        });
        mdio.write(self.addr, BMCR, bmcr::RESET);
        let delay = Delay::new();
        let mut waited = 0;
        while mdio.read(self.addr, BMCR) & bmcr::RESET != 0 {
            if waited >= RESET_TIMEOUT_US {
                return Err(PhyError::Timeout);
            }
            delay.delay_micros(10);
            waited += 10;
        }
        let adv = an::BASE_10_HALF | an::BASE_10_FULL | an::BASE_100_HALF | an::BASE_100_FULL;
        // Selector field 00001: IEEE 802.3.
        mdio.write(self.addr, ANAR, adv | 0x0001);
        let ctrl = mdio.read(self.addr, BMCR);
        mdio.write(self.addr, BMCR, ctrl | bmcr::ANEN | bmcr::RESTART_AN);
        Ok(())
    }

    fn poll_link<M: MdioBus>(&mut self, mdio: &mut M, cx: Option<&mut Context<'_>>) -> LinkState {
        if let Some(cx) = cx {
            LINK_WAKER.register(cx.waker());
        }
        // The link task wakes the runner every LINK_POLL; allow for the runner being late or
        // early by up to a tenth of it, so each wake reads the PHY once.
        let due = self
            .last_poll
            .is_none_or(|t| t.elapsed() >= LINK_POLL - LINK_POLL / 10);
        if due {
            self.last_poll = Some(Instant::now());
            self.read_link(mdio);
        }
        self.state
    }

    fn address(&self) -> u8 {
        self.addr
    }
}
