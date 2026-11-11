//! An `embassy-net-driver` wrapper that times the driver and the stack per frame.
//!
//! The driver's tokens hand smoltcp the DMA buffer itself (no copy in the driver). Inside a
//! token's `consume`, the closure smoltcp passes is the stack's work on the frame (TX: write
//! the headers and copy the payload from the socket buffer into the DMA buffer; RX: parse the
//! frame and copy the payload into the socket buffer); the rest of `consume` is the driver's
//! (descriptor ownership, cache write-back or invalidation, poll demand). The `receive` and
//! `transmit` calls (descriptor checks) count as the driver's too.
//!
//! With the cargo feature `accounting` off, the wrapper passes everything through: no timing,
//! no critical section per frame, [`stats`] returns zeros.

#[cfg(feature = "accounting")]
use core::cell::Cell;
use core::task::Context;

#[cfg(feature = "accounting")]
use critical_section::Mutex;
use embassy_net_driver::{Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken};

/// Frame counts and cycles since boot.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// Frames handed to the TX DMA.
    pub tx_frames: u32,
    /// Cycles in smoltcp's closure of the TX tokens.
    pub tx_stack: u64,
    /// Cycles of the TX tokens outside that closure.
    pub tx_driver: u64,
    /// Frames taken from the RX DMA.
    pub rx_frames: u32,
    /// Cycles in smoltcp's closure of the RX tokens.
    pub rx_stack: u64,
    /// Cycles of the RX tokens outside that closure.
    pub rx_driver: u64,
    /// `receive` and `transmit` calls (token requests).
    pub token_calls: u32,
    /// Cycles in those calls.
    pub token_cycles: u64,
}

impl Stats {
    /// Counts gained since `earlier`.
    pub fn since(&self, earlier: &Stats) -> Stats {
        Stats {
            tx_frames: self.tx_frames.wrapping_sub(earlier.tx_frames),
            tx_stack: self.tx_stack - earlier.tx_stack,
            tx_driver: self.tx_driver - earlier.tx_driver,
            rx_frames: self.rx_frames.wrapping_sub(earlier.rx_frames),
            rx_stack: self.rx_stack - earlier.rx_stack,
            rx_driver: self.rx_driver - earlier.rx_driver,
            token_calls: self.token_calls.wrapping_sub(earlier.token_calls),
            token_cycles: self.token_cycles - earlier.token_cycles,
        }
    }
}

#[cfg(feature = "accounting")]
static STATS: Mutex<Cell<Stats>> = Mutex::new(Cell::new(Stats {
    tx_frames: 0,
    tx_stack: 0,
    tx_driver: 0,
    rx_frames: 0,
    rx_stack: 0,
    rx_driver: 0,
    token_calls: 0,
    token_cycles: 0,
}));

/// The counters now (zeros without `accounting`).
pub fn stats() -> Stats {
    #[cfg(feature = "accounting")]
    return critical_section::with(|cs| STATS.borrow(cs).get());
    #[cfg(not(feature = "accounting"))]
    Stats::default()
}

#[cfg(feature = "accounting")]
fn update(f: impl FnOnce(&mut Stats)) {
    critical_section::with(|cs| {
        let cell = STATS.borrow(cs);
        let mut s = cell.get();
        f(&mut s);
        cell.set(s);
    });
}

#[cfg(not(feature = "accounting"))]
#[inline(always)]
fn update(_: impl FnOnce(&mut Stats)) {}

/// The cycle counter with `accounting`, else 0 (nothing is timed).
#[inline(always)]
fn mcycle() -> u32 {
    if cfg!(feature = "accounting") {
        crate::cpu::mcycle()
    } else {
        0
    }
}

/// The wrapped driver.
pub struct Measured<D>(pub D);

pub struct MeasuredRx<T>(T);
pub struct MeasuredTx<T>(T);

impl<D: Driver> Driver for Measured<D> {
    type RxToken<'a>
        = MeasuredRx<D::RxToken<'a>>
    where
        Self: 'a;
    type TxToken<'a>
        = MeasuredTx<D::TxToken<'a>>
    where
        Self: 'a;

    fn receive(&mut self, cx: &mut Context<'_>) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let c0 = mcycle();
        let r = self.0.receive(cx);
        let d = mcycle().wrapping_sub(c0);
        update(|s| {
            s.token_calls = s.token_calls.wrapping_add(1);
            s.token_cycles += u64::from(d);
        });
        r.map(|(rx, tx)| (MeasuredRx(rx), MeasuredTx(tx)))
    }

    fn transmit(&mut self, cx: &mut Context<'_>) -> Option<Self::TxToken<'_>> {
        let c0 = mcycle();
        let r = self.0.transmit(cx);
        let d = mcycle().wrapping_sub(c0);
        update(|s| {
            s.token_calls = s.token_calls.wrapping_add(1);
            s.token_cycles += u64::from(d);
        });
        r.map(MeasuredTx)
    }

    fn link_state(&mut self, cx: &mut Context<'_>) -> LinkState {
        self.0.link_state(cx)
    }

    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }

    fn hardware_address(&self) -> HardwareAddress {
        self.0.hardware_address()
    }
}

impl<T: RxToken> RxToken for MeasuredRx<T> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let c0 = mcycle();
        let mut inner = 0;
        let r = self.0.consume(|buf| {
            let a = mcycle();
            let r = f(buf);
            inner = mcycle().wrapping_sub(a);
            r
        });
        let total = mcycle().wrapping_sub(c0);
        update(|s| {
            s.rx_frames = s.rx_frames.wrapping_add(1);
            s.rx_stack += u64::from(inner);
            s.rx_driver += u64::from(total - inner);
        });
        r
    }
}

impl<T: TxToken> TxToken for MeasuredTx<T> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let c0 = mcycle();
        let mut inner = 0;
        let r = self.0.consume(len, |buf| {
            let a = mcycle();
            let r = f(buf);
            inner = mcycle().wrapping_sub(a);
            r
        });
        let total = mcycle().wrapping_sub(c0);
        update(|s| {
            s.tx_frames = s.tx_frames.wrapping_add(1);
            s.tx_stack += u64::from(inner);
            s.tx_driver += u64::from(total - inner);
        });
        r
    }
}
