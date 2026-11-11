//! UDP datagrams sent beside smoltcp, straight onto the TX ring: the camera's frame chunks
//! and the bench's datagrams.
//!
//! A datagram is a small *head* the caller writes (the chunk's header; the bench's 16-byte
//! header) and a *body* (a part of a PSRAM frame or JPEG file, of an internal buffer). The
//! driver writes the Ethernet, IPv6 and UDP headers, the head and a PIE copy of the body into a
//! TX descriptor's own buffer in internal RAM: one copy, instead of the application's copy into
//! smoltcp's ring and smoltcp's copy into the DMA buffer.
//!
//! The body is copied, not sent from where it lies by a second descriptor: the EMAC's DMA
//! reads PSRAM, but with the JPEG encoder working in PSRAM it fell behind the wire and the
//! frame was cut (TX underflow, 3 in 10 s at 154 fps), a body sent in place must stay
//! unchanged until the DMA has read it, and the checksum then read PSRAM, which cost what the
//! copy costs (README, step 3).
//!
//! - **Peer.** smoltcp does not expose its neighbour cache, so the driver learns the MAC itself:
//!   every IPv6 frame it receives records its source address and source MAC ([`learn`], a
//!   table of [`PEERS`] entries, the oldest replaced). The board only ever sends to the source
//!   of a request it received, so the MAC of its destination is always there; if it is not,
//!   [`send`] says so and the caller sends through its socket instead.
//! - **Headers.** Ethernet (the peer's MAC, the board's MAC, IPv6), IPv6 (traffic class and
//!   flow label 0, next header UDP, hop limit 64 as smoltcp sends) and UDP, written per
//!   datagram (62 bytes).
//! - **Checksum** (mandatory over IPv6, and the EMAC inserts none in threshold mode): the
//!   pseudo-header summed as scalar 16-bit words, the UDP header, the head and the copied body
//!   by [`Pie::sum16_be`] (from internal RAM, after the copy). The bench service checks it
//!   against smoltcp's routine at boot ([`self_test`]).
//! - **Cache.** The frame is written back before its descriptor goes to the DMA. The CPU reads
//!   the body through the cache: a body a DMA wrote (the camera's frames, the encoder's files)
//!   must have no stale lines (the drivers invalidate them).
//! - **Sharing the ring with smoltcp.** Both users run in core 0's thread-mode executor and take,
//!   fill and hand over the descriptor of a frame within one poll, so frames never interleave.
//!   A datagram here waits until [`RESERVE`] descriptors more than its own are free, so smoltcp
//!   (answers, neighbour discovery, ping, the RX path's TX token) always finds room within a
//!   frame time.
//! - **Waiting.** The TX completion interrupt (every [`super::IC_EVERY`]th frame) wakes the
//!   waiter. A sender waits only when more than `TX_DESCRIPTORS - 1 - RESERVE` descriptors are
//!   in flight, which is more than `IC_EVERY` frames, so one of them asks for that interrupt.

use core::cell::Cell;
use core::future::poll_fn;
use core::sync::atomic::{AtomicU32, Ordering};
use core::task::Poll;

use critical_section::Mutex;
use embassy_net::{IpAddress, IpEndpoint, Ipv6Address};
use embassy_sync::waitqueue::AtomicWaker;
use esp32p4_pie::Pie;

use super::eth;
use crate::cpu::{self, Counter64};

/// Peers remembered.
pub const PEERS: usize = 4;
/// Descriptors a datagram leaves free for smoltcp.
pub const RESERVE: usize = 4;
/// Ethernet (14), IPv6 (40) and UDP (8) headers.
pub const HEADERS: usize = 62;
/// Room for the head after the headers.
pub const HEAD_MAX: usize = eth::BUF_LEN - HEADERS;
/// The IPv6 hop limit smoltcp uses for UDP.
const HOP_LIMIT: u8 = 64;
const ETHERTYPE_IPV6: [u8; 2] = [0x86, 0xDD];
const NEXT_HEADER_UDP: u8 = 17;

// A sender waits only with more than `TX_DESCRIPTORS - 1 - RESERVE` descriptors in flight, one
// per frame: more than `IC_EVERY` frames, one of which asks for the interrupt that wakes it.
const _: () = assert!(eth::TX_DESCRIPTORS - 1 - RESERVE > eth::IC_EVERY as usize);

/// Counts of the datagrams sent here, since boot (zeros without the `accounting` feature).
#[derive(Clone, Copy, Default)]
pub struct Stats {
    /// Datagrams queued.
    pub datagrams: u32,
    /// Cycles from the room found to the frame queued: head, body copy, headers, checksum,
    /// write-back, descriptor.
    pub cycles: u64,
    /// Of those, the cycles in the headers and the checksum (the sum of the copied payload).
    pub header_cycles: u64,
    /// Datagrams that had to wait for room on the ring.
    pub waits: u32,
}

impl Stats {
    /// Counts gained since `earlier`.
    pub fn since(&self, earlier: &Stats) -> Stats {
        Stats {
            datagrams: self.datagrams.wrapping_sub(earlier.datagrams),
            cycles: self.cycles - earlier.cycles,
            header_cycles: self.header_cycles - earlier.header_cycles,
            waits: self.waits.wrapping_sub(earlier.waits),
        }
    }
}

static DATAGRAMS: AtomicU32 = AtomicU32::new(0);
static CYCLES: Counter64 = Counter64::new();
static HEADER_CYCLES: Counter64 = Counter64::new();
static WAITS: AtomicU32 = AtomicU32::new(0);

/// The counts now.
pub fn stats() -> Stats {
    Stats {
        datagrams: DATAGRAMS.load(Ordering::Relaxed),
        cycles: CYCLES.load(),
        header_cycles: HEADER_CYCLES.load(),
        waits: WAITS.load(Ordering::Relaxed),
    }
}

/// Books one datagram.
#[inline(always)]
fn count(cycles: u32, header_cycles: u32) {
    #[cfg(feature = "accounting")]
    {
        DATAGRAMS.fetch_add(1, Ordering::Relaxed);
        CYCLES.add(cycles);
        HEADER_CYCLES.add(header_cycles);
    }
    #[cfg(not(feature = "accounting"))]
    let _ = (cycles, header_cycles);
}

/// An IPv6 address and its MAC.
type Peer = (Ipv6Address, [u8; 6]);

/// The peers learned, most recent first.
static TABLE: Mutex<Cell<[Option<Peer>; PEERS]>> = Mutex::new(Cell::new([None; PEERS]));

/// Woken by the TX completion interrupt: a sender waiting for room.
pub(super) static ROOM: AtomicWaker = AtomicWaker::new();

/// Records the source of a received frame if it is IPv6 (called by the RX path for every
/// frame, before smoltcp sees it).
pub(super) fn learn(frame: &[u8]) {
    if frame.len() < 14 + 40 || frame[12..14] != ETHERTYPE_IPV6 {
        return;
    }
    let mac: [u8; 6] = frame[6..12].try_into().unwrap_or_default();
    let addr = Ipv6Address::from(<[u8; 16]>::try_from(&frame[22..38]).unwrap_or_default());
    critical_section::with(|cs| {
        let cell = TABLE.borrow(cs);
        let mut t = cell.get();
        if t[0] == Some((addr, mac)) {
            return;
        }
        let at = t
            .iter()
            .position(|e| e.is_some_and(|(a, _)| a == addr))
            .unwrap_or(PEERS - 1);
        t.copy_within(0..at, 1);
        t[0] = Some((addr, mac));
        cell.set(t);
    });
}

/// The MAC of `addr`, if a frame from it was received.
pub fn peer_mac(addr: Ipv6Address) -> Option<[u8; 6]> {
    critical_section::with(|cs| {
        TABLE
            .borrow(cs)
            .get()
            .iter()
            .flatten()
            .find(|(a, _)| *a == addr)
            .map(|&(_, m)| m)
    })
}

/// Why [`send`] did not queue the datagram: no frame from the destination was received, so its
/// MAC is not known (or `net` has not set the board's own addresses yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoPeer;

/// The board's own address and MAC, set by `net` at bring-up.
static SELF: Mutex<Cell<Option<Peer>>> = Mutex::new(Cell::new(None));

/// Sets the source MAC and IPv6 address of the datagrams sent here.
pub(super) fn set_source(mac: [u8; 6], addr: Ipv6Address) {
    critical_section::with(|cs| SELF.borrow(cs).set(Some((addr, mac))));
}

/// Both ends of a datagram to `to` from port `from`.
fn ends(to: IpEndpoint, from: u16) -> Result<Ends, NoPeer> {
    // The stack is built IPv6 only (`embassy-net` without `proto-ipv4`).
    let IpAddress::Ipv6(dst) = to.addr;
    let dst_mac = peer_mac(dst).ok_or(NoPeer)?;
    let (src, src_mac) = critical_section::with(|cs| SELF.borrow(cs).get()).ok_or(NoPeer)?;
    Ok(Ends {
        src_mac,
        dst_mac,
        src,
        dst,
        src_port: from,
        dst_port: to.port,
    })
}

/// Sends one UDP datagram from port `from` to `to`: the head written by `head` (into a
/// buffer of [`HEAD_MAX`] bytes; it returns the length), then a copy of `body` (at most
/// `HEAD_MAX` bytes with the head). Waits for room on the TX ring.
pub async fn send(
    pie: &Pie,
    to: IpEndpoint,
    from: u16,
    head: impl FnOnce(&mut [u8]) -> usize,
    body: &[u8],
) -> Result<(), NoPeer> {
    let ends = ends(to, from)?;
    wait_room(1 + RESERVE).await;
    let c0 = cpu::mcycle();
    let (i, ic) = eth::tx_take();
    let addr = eth::tx_buf_addr(i);
    // SAFETY: descriptor `i` is taken (above) and its buffer is the CPU's until `tx_queue`.
    let buf = unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, eth::BUF_LEN) };
    let n = head(&mut buf[HEADERS..]).min(HEAD_MAX);
    let payload = n + body.len();
    pie.copy(&mut buf[HEADERS + n..HEADERS + payload], body);
    let c1 = cpu::mcycle();
    let len = write_headers(pie, buf, &ends, payload, &[]);
    let c2 = cpu::mcycle();
    esp32p4_cache::write_back(addr, len);
    eth::tx_queue(i, addr, len, ic);
    count(cpu::mcycle().wrapping_sub(c0), c2.wrapping_sub(c1));
    Ok(())
}

/// Waits until `n` descriptors from the head are free.
async fn wait_room(n: usize) {
    let mut waited = false;
    poll_fn(|cx| {
        if eth::tx_room(n) {
            return Poll::Ready(());
        }
        ROOM.register(cx.waker());
        if eth::tx_room(n) {
            Poll::Ready(())
        } else {
            waited = true;
            Poll::Pending
        }
    })
    .await;
    if waited && cfg!(feature = "accounting") {
        WAITS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Both ends of a datagram.
pub struct Ends {
    pub src_mac: [u8; 6],
    pub dst_mac: [u8; 6],
    pub src: Ipv6Address,
    pub dst: Ipv6Address,
    pub src_port: u16,
    pub dst_port: u16,
}

/// Writes the Ethernet, IPv6 and UDP headers into `buf[..HEADERS]` for a UDP payload of the
/// `head` bytes at `buf[HEADERS..HEADERS + head]` followed by `body`, with the UDP checksum.
/// Returns the length of the first segment (`HEADERS + head`).
pub fn write_headers(pie: &Pie, buf: &mut [u8], e: &Ends, head: usize, body: &[u8]) -> usize {
    let udp_len = (8 + head + body.len()) as u16;
    buf[0..6].copy_from_slice(&e.dst_mac);
    buf[6..12].copy_from_slice(&e.src_mac);
    buf[12..14].copy_from_slice(&ETHERTYPE_IPV6);
    let ip = &mut buf[14..54];
    ip[0..4].copy_from_slice(&[0x60, 0, 0, 0]);
    ip[4..6].copy_from_slice(&udp_len.to_be_bytes());
    ip[6] = NEXT_HEADER_UDP;
    ip[7] = HOP_LIMIT;
    ip[8..24].copy_from_slice(&e.src.octets());
    ip[24..40].copy_from_slice(&e.dst.octets());
    let udp = &mut buf[54..62];
    udp[0..2].copy_from_slice(&e.src_port.to_be_bytes());
    udp[2..4].copy_from_slice(&e.dst_port.to_be_bytes());
    udp[4..6].copy_from_slice(&udp_len.to_be_bytes());
    udp[6..8].copy_from_slice(&[0, 0]);
    // Pseudo-header (both addresses, the UDP length, the next header), then the UDP header and
    // the head (contiguous from byte 54), then the body: its offset in the UDP payload is
    // `8 + head`, so its big-endian word sum is byte-swapped when `head` is odd.
    let mut sum = sum_words(&buf[22..54]) + u32::from(udp_len) + u32::from(NEXT_HEADER_UDP);
    sum += pie.sum16_be(&buf[54..HEADERS + head]);
    let b = pie.sum16_be(body);
    sum += if head % 2 == 1 {
        u32::from((b as u16).swap_bytes())
    } else {
        b
    };
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    // A computed 0 goes out as 0xFFFF (RFC 768; 0 means "no checksum").
    let c = match !(sum as u16) {
        0 => 0xFFFF,
        c => c,
    };
    buf[60..62].copy_from_slice(&c.to_be_bytes());
    HEADERS + head
}

/// Plain sum of big-endian 16-bit words of an even-length slice (at most 2^16 words).
fn sum_words(b: &[u8]) -> u32 {
    b.as_chunks::<2>()
        .0
        .iter()
        .map(|&w| u32::from(u16::from_be_bytes(w)))
        .sum()
}

/// Bytes of scratch memory [`self_test`] takes.
pub const SELF_TEST_SCRATCH: usize = 3 * 1600;

/// Compares [`write_headers`]'s checksum with smoltcp's on `cases` datagrams with random heads
/// and bodies (body 0..=1440 bytes at any alignment, head 0..=40 bytes, odd and even; random,
/// all-0xFF and all-zero bodies): the field must equal what `UdpPacket::fill_checksum` writes
/// and pass `UdpPacket::verify_checksum`. Returns how many cases failed. `scratch` holds the
/// frames (at least [`SELF_TEST_SCRATCH`] bytes).
#[allow(
    clippy::large_stack_frames,
    reason = "once at boot of the bench service, on the main stack, which has most of internal \
    RAM (smoltcp's packet views are inlined here)"
)]
pub fn self_test(pie: &Pie, seed: u32, cases: u32, scratch: &mut [u8]) -> u32 {
    use smoltcp::wire::UdpPacket;
    let mut rng = seed | 1;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        rng
    };
    let (frame, rest) = scratch[..SELF_TEST_SCRATCH].split_at_mut(1600);
    let (body, copy) = rest.split_at_mut(1600);
    let mut failures = 0;
    for _ in 0..cases {
        let head = (next() % 41) as usize;
        let start = (next() % 16) as usize;
        let blen = (next() % 1441) as usize;
        let fill = next() % 4;
        for b in body.iter_mut() {
            *b = match fill {
                0 => 0xFF,
                1 => 0,
                _ => next() as u8,
            };
        }
        for b in frame[HEADERS..HEADERS + head].iter_mut() {
            *b = next() as u8;
        }
        let body = &body[start..start + blen];
        let e = Ends {
            src_mac: [2, 0, 0, 0, 0, 1],
            dst_mac: [2, 0, 0, 0, 0, 2],
            src: Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0x4553, next() as u16),
            dst: Ipv6Address::new(0xfe80, 0, 0, 0, next() as u16, 0, 0, next() as u16),
            src_port: next() as u16,
            dst_port: next() as u16,
        };
        let n = write_headers(pie, frame, &e, head, body);
        frame[n..n + blen].copy_from_slice(body);
        let udp_len = n - 54 + blen;
        copy[..udp_len].copy_from_slice(&frame[54..n + blen]);
        let mut theirs = UdpPacket::new_unchecked(&mut copy[..udp_len]);
        theirs.fill_checksum(&e.src.into(), &e.dst.into());
        let ours = UdpPacket::new_unchecked(&frame[54..n + blen]);
        let ok = ours.checksum() == theirs.checksum()
            && ours.verify_checksum(&e.src.into(), &e.dst.into());
        failures += u32::from(!ok);
    }
    failures
}
