//! The EMAC's DMA rings and the `embassy-net-driver` driver, in place of esp-hal's.
//!
//! esp-hal 1.2.2 still brings the EMAC up (`Ethernet::new` in blocking mode: clocks, the RMII
//! pads, the DMA soft reset and bus mode, the MAC configuration and address, the PHY over
//! MDIO), and its `Ethernet` stays alive for the PHY's link polls and the MAC's speed and
//! duplex. [`Eth::take_over`] then stops its DMA, points the descriptor list registers at the
//! rings here, switches the TX DMA to threshold mode, enables the DMA interrupts and binds
//! this module's handler to `ETH_MAC`, and starts the DMA again. esp-hal's own rings (one
//! descriptor each, in `net`'s `.dram_dma` storage) are never used after that.
//!
//! What differs from esp-hal's rings:
//!
//! - **Descriptors through the non-cacheable alias** of internal RAM (+0x4000_0000, as
//!   `esp32p4-jpeg` does for its own): the CPU reads and writes them without a cache call.
//!   The DMA gets the ordinary (cached) addresses. The descriptor lines are written back and
//!   invalidated once at the take-over and never touched through the cached alias after, so
//!   no dirty line can overwrite what the DMA writes.
//! - **Buffers** are cached: a TX buffer is written back (`esp32p4-cache`) after the stack
//!   wrote it and before its descriptor goes to the DMA. An RX buffer holds no cached line
//!   while the DMA owns it: all of them are invalidated at the take-over, and the lines the
//!   CPU read of a frame are invalidated before the buffer goes back to the DMA, so the next
//!   read after the DMA's write misses and fetches the new bytes. One cache call per frame in
//!   each direction; a token request makes none.
//! - **TX completion interrupt on every [`IC_EVERY`]th frame** only (`TDES0` IC). A
//!   driver user that finds the ring full waits for that interrupt: a full ring holds more than
//!   `IC_EVERY` frames, so at least one of them asks for it.
//! - **Threshold mode from the start**: TX store-and-forward breaks frames on the P4 (README,
//!   "The Ethernet driver", item 1: esp-hal's `dma_start` sets it; 56-byte pings passed,
//!   400-byte ones failed, 1452-byte datagrams were lost; ESP-IDF runs threshold mode at 64
//!   bytes on every chip) [INFERENCE: the TX FIFO is smaller than a full frame; the TRM gives
//!   256 bytes]. In threshold mode the EMAC inserts no checksum (item 2; the descriptor's CIC
//!   field is left at 0 here), so smoltcp computes the UDP and ICMPv6 checksums on TX
//!   ([`Eth::capabilities`]). RX checksums stay with the EMAC: frames its checksum engine
//!   flags in RDES4 are dropped here, as esp-hal does.
//!
//! The TX ring is shared state (a [`critical_section::Mutex`]) with two users: the runner's TX
//! tokens (smoltcp) and [`super::chunk`] (datagrams sent beside smoltcp). Both run in core 0's
//! thread-mode executor and take, fill and hand over a frame's descriptor in one poll, so
//! frames are handed over in ring order.

use core::cell::{Cell, UnsafeCell};
use core::mem::offset_of;
use core::sync::atomic::{AtomicU32, Ordering, fence};
use core::task::Context;

use critical_section::Mutex;
use embassy_net_driver::{
    Capabilities, Checksum, Driver, HardwareAddress, LinkState, RxToken, TxToken,
};
use embassy_sync::waitqueue::AtomicWaker;
use esp_hal::Blocking;
use esp_hal::ethernet::Ethernet;
use esp_hal::interrupt::{self, Priority};
use esp_hal::peripherals::Interrupt;

use super::ip101::Ip101;

/// TX descriptors, each with a [`BUF_LEN`]-byte buffer.
pub const TX_DESCRIPTORS: usize = 16;
/// RX descriptors, each with a [`BUF_LEN`]-byte buffer.
pub const RX_DESCRIPTORS: usize = 16;
/// Bytes of each DMA buffer: a whole frame (1514 bytes and the 4-byte FCS the RX DMA writes),
/// rounded up to whole 64-byte cache lines.
pub const BUF_LEN: usize = 1536;
/// Largest frame the stack may send (Ethernet header and payload, no FCS).
const MTU: usize = 1514;
/// Every this many TX frames, one asks for the completion interrupt.
pub const IC_EVERY: u32 = 8;
/// Shortest frame taken from the RX DMA (an Ethernet header).
const RX_MIN: usize = 14;

// TX descriptor word 0 (enhanced 32-byte descriptors, TRM ch. 55, p.3229-3231).
const TDES0_OWN: u32 = 1 << 31;
const TDES0_IC: u32 = 1 << 30;
const TDES0_LS: u32 = 1 << 29;
const TDES0_FS: u32 = 1 << 28;
/// Second address chained: word 3 is the next descriptor ("must be set to 1" on TX).
const TDES0_TCH: u32 = 1 << 20;
/// Error summary, written back with the frame's status (in its last descriptor).
const TDES0_ES: u32 = 1 << 15;
/// Underflow: the DMA did not deliver the frame's bytes to the TX FIFO in time.
const TDES0_UF: u32 = 1 << 1;

// RX descriptor words 0, 1 and 4 (TRM p.3234-3239).
const RDES0_OWN: u32 = 1 << 31;
const RDES0_FL_SHIFT: u32 = 16;
const RDES0_FL_MASK: u32 = 0x3fff;
const RDES0_ES: u32 = 1 << 15;
const RDES0_FS: u32 = 1 << 9;
const RDES0_LS: u32 = 1 << 8;
/// RDES4 holds the checksum engine's status.
const RDES0_ESA: u32 = 1 << 0;
const RDES1_RCH: u32 = 1 << 14;
const RDES4_IP_HEADER_ERROR: u32 = 1 << 3;
const RDES4_IP_PAYLOAD_ERROR: u32 = 1 << 4;
const RDES4_BYPASSED: u32 = 1 << 5;
const RDES4_IPV4: u32 = 1 << 6;
const RDES4_IPV6: u32 = 1 << 7;

// DMASTATUS bits.
const STATUS_TI: u32 = 1 << 0;
const STATUS_RI: u32 = 1 << 6;

/// Descriptors and buffers, in internal RAM (`.bss`). The descriptors come first and fill
/// whole cache lines (32 x 32 bytes), so no buffer shares a line with one.
#[repr(C, align(64))]
struct Storage {
    tx_desc: [[u32; 8]; TX_DESCRIPTORS],
    rx_desc: [[u32; 8]; RX_DESCRIPTORS],
    tx_buf: [[u8; BUF_LEN]; TX_DESCRIPTORS],
    rx_buf: [[u8; BUF_LEN]; RX_DESCRIPTORS],
}

/// Bytes of internal RAM the rings take.
pub const RING_BYTES: usize = size_of::<Storage>();

struct StorageCell(UnsafeCell<Storage>);

// SAFETY: no reference to the storage is ever made. The descriptors are reached through raw
// pointers into the non-cacheable alias, each descriptor by the one owner the ring protocol
// gives it at a time (the DMA, or the user that took it); a buffer by its descriptor's owner.
unsafe impl Sync for StorageCell {}

static STORAGE: StorageCell = StorageCell(UnsafeCell::new(Storage {
    tx_desc: [[0; 8]; TX_DESCRIPTORS],
    rx_desc: [[0; 8]; RX_DESCRIPTORS],
    tx_buf: [[0; BUF_LEN]; TX_DESCRIPTORS],
    rx_buf: [[0; BUF_LEN]; RX_DESCRIPTORS],
}));

/// Cached (DMA) address of the storage.
fn base() -> usize {
    STORAGE.0.get() as usize
}

/// DMA address of TX descriptor `i`.
fn tx_desc_addr(i: usize) -> usize {
    base() + offset_of!(Storage, tx_desc) + 32 * i
}

/// DMA address of RX descriptor `i`.
fn rx_desc_addr(i: usize) -> usize {
    base() + offset_of!(Storage, rx_desc) + 32 * i
}

/// Address of TX buffer `i`.
pub(super) fn tx_buf_addr(i: usize) -> usize {
    base() + offset_of!(Storage, tx_buf) + BUF_LEN * i
}

/// Address of RX buffer `i`.
fn rx_buf_addr(i: usize) -> usize {
    base() + offset_of!(Storage, rx_buf) + BUF_LEN * i
}

/// A descriptor through the non-cacheable alias: words read and written as volatile 32-bit
/// accesses, no cache involved.
#[derive(Clone, Copy)]
struct Desc(usize);

impl Desc {
    fn tx(i: usize) -> Self {
        Self(esp32p4_cache::non_cacheable(tx_desc_addr(i)))
    }

    fn rx(i: usize) -> Self {
        Self(esp32p4_cache::non_cacheable(rx_desc_addr(i)))
    }

    fn read(self, word: usize) -> u32 {
        // SAFETY: a word of a descriptor in `STORAGE`, through the uncached alias the chip maps
        // for all of internal RAM; no reference to it exists (see `StorageCell`).
        unsafe { ((self.0 + 4 * word) as *const u32).read_volatile() }
    }

    fn write(self, word: usize, v: u32) {
        // SAFETY: as in `read`; the caller owns the descriptor (or the DMA is stopped).
        unsafe { ((self.0 + 4 * word) as *mut u32).write_volatile(v) }
    }
}

/// The TX ring's software state: the next descriptor to take, and the frames queued so far
/// (for the completion-interrupt rhythm).
#[derive(Clone, Copy)]
struct TxState {
    head: usize,
    frames: u32,
}

static TX: Mutex<Cell<TxState>> = Mutex::new(Cell::new(TxState { head: 0, frames: 0 }));

/// Woken by the RX interrupt and by the TX completion interrupt: the runner.
static RUNNER: AtomicWaker = AtomicWaker::new();

/// `true` when the `n` descriptors from the head are the CPU's (the DMA finished with them).
pub(super) fn tx_room(n: usize) -> bool {
    let head = critical_section::with(|cs| TX.borrow(cs).get().head);
    (0..n).all(|k| Desc::tx((head + k) % TX_DESCRIPTORS).read(0) & TDES0_OWN == 0)
}

/// Takes the descriptor at the head for one frame: returns its index and whether the frame
/// asks for the completion interrupt. The caller checked [`tx_room`] in the same poll, and
/// hands the frame over before its next await.
pub(super) fn tx_take() -> (usize, bool) {
    critical_section::with(|cs| {
        let mut s = TX.borrow(cs).get();
        let i = s.head;
        // The status the DMA wrote back for the frame this descriptor carried last.
        let tdes0 = Desc::tx(i).read(0);
        if tdes0 & TDES0_ES != 0 {
            TX_ERRORS.fetch_add(1, Ordering::Relaxed);
            if tdes0 & TDES0_UF != 0 {
                TX_UNDERFLOWS.fetch_add(1, Ordering::Relaxed);
            }
        }
        s.head = (i + 1) % TX_DESCRIPTORS;
        s.frames = s.frames.wrapping_add(1);
        let ic = s.frames.is_multiple_of(IC_EVERY);
        TX.borrow(cs).set(s);
        (i, ic)
    })
}

/// TX frames the DMA reported with an error, since boot (counted when their descriptors are
/// taken again), and of those the underflows.
static TX_ERRORS: AtomicU32 = AtomicU32::new(0);
static TX_UNDERFLOWS: AtomicU32 = AtomicU32::new(0);

/// `(errors, underflows)` of the TX frames since boot (see [`TX_ERRORS`]).
pub fn tx_errors() -> (u32, u32) {
    (
        TX_ERRORS.load(Ordering::Relaxed),
        TX_UNDERFLOWS.load(Ordering::Relaxed),
    )
}

/// Hands a one-descriptor frame of `len` bytes at DMA address `buf` to the DMA through
/// descriptor `i`, and wakes the TX DMA. The buffer is already written back.
pub(super) fn tx_queue(i: usize, buf: usize, len: usize, ic: bool) {
    let d = Desc::tx(i);
    d.write(2, buf as u32);
    d.write(1, len as u32);
    // The address and length reach memory before the DMA can see the descriptor as its own.
    fence(Ordering::SeqCst);
    let ic = if ic { TDES0_IC } else { 0 };
    d.write(0, TDES0_OWN | TDES0_TCH | TDES0_FS | TDES0_LS | ic);
    fence(Ordering::SeqCst);
    demand_tx_poll();
}

fn demand_tx_poll() {
    // SAFETY: a write to the TX poll-demand register (any value); the PAC has no writer for it.
    unsafe { core::ptr::write_volatile(dma().dmatxpolldemand().as_ptr(), 0) };
}

fn demand_rx_poll() {
    // SAFETY: as `demand_tx_poll`, for RX.
    unsafe { core::ptr::write_volatile(dma().dmarxpolldemand().as_ptr(), 0) };
}

fn dma() -> &'static esp32p4::emac_dma::RegisterBlock {
    // SAFETY: the EMAC DMA register block; `Eth` owns the EMAC (through esp-hal's `Ethernet`).
    unsafe { &*esp32p4::EMAC_DMA::ptr() }
}

/// The EMAC handler: clears the DMA status and wakes the runner on a received frame or a
/// completion interrupt. In RAM: it runs up to once per received frame.
#[esp_hal::ram]
#[esp_hal::handler(priority = Priority::Priority1)]
fn eth_isr() {
    let status = dma().dmastatus().read().bits();
    // SAFETY: write-one-to-clear of the bits just read.
    dma().dmastatus().write(|w| unsafe { w.bits(status) });
    if status & (STATUS_RI | STATUS_TI) != 0 {
        RUNNER.wake();
    }
    if status & STATUS_TI != 0 {
        super::chunk::ROOM.wake();
    }
}

/// The driver embassy-net runs.
pub struct Eth {
    /// esp-hal's driver, for the PHY and the MAC's speed and duplex (its DMA is not used).
    hal: Ethernet<'static, Blocking, Ip101>,
    mac: [u8; 6],
    /// The next RX descriptor to read.
    rx: usize,
}

/// `DMAOPERATION_MODE` before and after [`Eth::take_over`].
pub struct OpMode {
    pub before: u32,
    pub after: u32,
}

impl Eth {
    /// Takes the EMAC's DMA over from esp-hal (see the module docs).
    pub fn take_over(hal: Ethernet<'static, Blocking, Ip101>) -> (Self, OpMode) {
        let dma = dma();
        let before = dma.dmaoperation_mode().read().bits();
        dma.dmaoperation_mode().modify(|_, w| {
            w.start_stop_transmission_command().clear_bit();
            w.start_stop_rx().clear_bit()
        });
        for _ in 0..100_000 {
            let s = dma.dmastatus().read();
            if s.trans_proc_state().bits() == 0 && s.recv_proc_state().bits() == 0 {
                break;
            }
        }
        init_rings();
        // SAFETY: both DMA engines are stopped; the lists are the rings just built.
        unsafe {
            dma.dmatxbaseaddr()
                .write(|w| w.bits(tx_desc_addr(0) as u32));
            dma.dmarxbaseaddr()
                .write(|w| w.bits(rx_desc_addr(0) as u32));
        }
        dma.dmaoperation_mode().modify(|_, w| {
            w.tx_str_fwd().clear_bit();
            // SAFETY: TTC 0 = start sending after 64 bytes in the TX FIFO.
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
        // SAFETY: clears every status bit (write one to clear).
        dma.dmastatus().write(|w| unsafe { w.bits(u32::MAX) });
        // SAFETY: interrupt enables: normal summary, receive, transmit (TI comes only from
        // descriptors with IC).
        dma.dmain_en().write(|w| {
            w.dmain_nise().set_bit();
            w.dmain_rie().set_bit();
            w.dmain_tie().set_bit()
        });
        interrupt::bind_handler(Interrupt::ETH_MAC, eth_isr);
        dma.dmaoperation_mode().modify(|_, w| {
            w.start_stop_rx().set_bit();
            w.start_stop_transmission_command().set_bit()
        });
        let after = dma.dmaoperation_mode().read().bits();
        let mac = hal.mac_addr();
        (Self { hal, mac, rx: 0 }, OpMode { before, after })
    }

    /// Length of the frame in the current RX descriptor, if the DMA finished one. Frames the
    /// DMA or the checksum engine flagged, partial ones and ones of an impossible length go
    /// back to the DMA at once (the CPU never read them, so they hold no cached line).
    fn rx_ready(&mut self) -> Option<usize> {
        loop {
            let d = Desc::rx(self.rx);
            let s = d.read(0);
            if s & RDES0_OWN != 0 {
                return None;
            }
            let ext = if s & RDES0_ESA != 0 { d.read(4) } else { 0 };
            let checksum_bad = ext & (RDES4_IPV4 | RDES4_IPV6) != 0
                && ext & RDES4_BYPASSED == 0
                && ext & (RDES4_IP_HEADER_ERROR | RDES4_IP_PAYLOAD_ERROR) != 0;
            let whole = s & RDES0_FS != 0 && s & RDES0_LS != 0;
            // Less the 4-byte FCS the EMAC leaves in the buffer.
            let len = (((s >> RDES0_FL_SHIFT) & RDES0_FL_MASK) as usize).saturating_sub(4);
            if s & RDES0_ES == 0 && whole && !checksum_bad && (RX_MIN..=MTU + 4).contains(&len) {
                return Some(len);
            }
            rx_recycle(&mut self.rx);
        }
    }
}

/// Gives RX descriptor `*rx` back to the DMA and moves to the next one.
fn rx_recycle(rx: &mut usize) {
    Desc::rx(*rx).write(0, RDES0_OWN);
    fence(Ordering::SeqCst);
    *rx = (*rx + 1) % RX_DESCRIPTORS;
    demand_rx_poll();
}

/// Builds both rings, with the DMA stopped: every line of the storage written back and
/// invalidated (the `.bss` zeroing at boot left them cached), then each descriptor written
/// through the uncached alias, RX ones owned by the DMA.
fn init_rings() {
    esp32p4_cache::writeback_invalidate(base(), RING_BYTES);
    for i in 0..TX_DESCRIPTORS {
        let d = Desc::tx(i);
        d.write(0, TDES0_TCH);
        d.write(1, 0);
        d.write(2, tx_buf_addr(i) as u32);
        d.write(3, tx_desc_addr((i + 1) % TX_DESCRIPTORS) as u32);
        for w in 4..8 {
            d.write(w, 0);
        }
    }
    for i in 0..RX_DESCRIPTORS {
        let d = Desc::rx(i);
        d.write(1, RDES1_RCH | BUF_LEN as u32);
        d.write(2, rx_buf_addr(i) as u32);
        d.write(3, rx_desc_addr((i + 1) % RX_DESCRIPTORS) as u32);
        for w in 4..8 {
            d.write(w, 0);
        }
        d.write(0, RDES0_OWN);
    }
    critical_section::with(|cs| TX.borrow(cs).set(TxState { head: 0, frames: 0 }));
    fence(Ordering::SeqCst);
}

pub struct Rx<'a> {
    rx: &'a mut usize,
    len: usize,
}

pub struct Tx;

impl Driver for Eth {
    type RxToken<'a>
        = Rx<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = Tx
    where
        Self: 'a;

    fn receive(&mut self, cx: &mut Context<'_>) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // Check, and only on failure register and check again: no lock on the way that
        // succeeds, and no wake lost between the check and the registration.
        let ready = |e: &mut Self| e.rx_ready().filter(|_| tx_room(1));
        let len = match ready(self) {
            Some(len) => len,
            None => {
                RUNNER.register(cx.waker());
                ready(self)?
            }
        };
        Some((
            Rx {
                rx: &mut self.rx,
                len,
            },
            Tx,
        ))
    }

    fn transmit(&mut self, cx: &mut Context<'_>) -> Option<Self::TxToken<'_>> {
        if tx_room(1) {
            return Some(Tx);
        }
        RUNNER.register(cx.waker());
        tx_room(1).then_some(Tx)
    }

    fn link_state(&mut self, cx: &mut Context<'_>) -> LinkState {
        let s = self.hal.poll_link(Some(cx));
        if s.up {
            self.hal.set_speed(s.speed);
            self.hal.set_duplex(s.duplex);
            LinkState::Up
        } else {
            LinkState::Down
        }
    }

    fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::default();
        caps.max_transmission_unit = MTU;
        caps.max_burst_size = Some(TX_DESCRIPTORS);
        // No checksum insertion in TX threshold mode (module docs): smoltcp computes UDP and
        // ICMPv6 on TX; the EMAC checks RX and `rx_ready` drops what it flags.
        caps.checksum.ipv4 = Checksum::Tx;
        caps.checksum.udp = Checksum::Tx;
        caps.checksum.tcp = Checksum::Tx;
        caps.checksum.icmpv4 = Checksum::Tx;
        caps.checksum.icmpv6 = Checksum::Tx;
        caps
    }

    fn hardware_address(&self) -> HardwareAddress {
        HardwareAddress::Ethernet(self.mac)
    }
}

impl RxToken for Rx<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let addr = rx_buf_addr(*self.rx);
        // SAFETY: the CPU owns this RX buffer (its descriptor's OWN is clear) until
        // `rx_recycle`; no other reference to it exists.
        let buf = unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, self.len) };
        super::chunk::learn(buf);
        let r = f(buf);
        // Drop the lines the stack read (or wrote), so none is cached while the DMA owns the
        // buffer again.
        esp32p4_cache::invalidate(addr, self.len.next_multiple_of(esp32p4_cache::L1_LINE));
        rx_recycle(self.rx);
        r
    }
}

impl TxToken for Tx {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let len = len.min(MTU);
        let (i, ic) = tx_take();
        let addr = tx_buf_addr(i);
        // SAFETY: descriptor `i` was the CPU's (`tx_room` in this poll) and is now taken; its
        // buffer has no other user until the DMA gets it below.
        let buf = unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, len) };
        let r = f(buf);
        esp32p4_cache::write_back(addr, len);
        tx_queue(i, addr, len, ic);
        r
    }
}
