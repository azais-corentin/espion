//! The network test service, driven by `tools/netbench.py` on the host.
//!
//! **Board to host** (UDP [`TX_PORT`]): the host sends a 4-byte request `[kind, 0, seconds
//! (u16 LE)]`; the board then sends datagrams of [`DATAGRAM`] bytes to the address and port the
//! request came from, as fast as the stack takes them, for that many seconds, and ends with one
//! report datagram. Every datagram starts with a [`HEADER`]-byte header, four u32 LE: the
//! datagram sequence number (from 0 per request), the frame number, the byte offset of the
//! payload in the frame, and the frame length; the report has frame number [`REPORT_FRAME`]
//! and ASCII text after the header. Kinds:
//!
//! - `I`: a [`INTERNAL_LEN`]-byte buffer in internal RAM, sent over and over;
//! - `P`: a frame-sized buffer in PSRAM, its cache lines written back and invalidated before
//!   each pass (cold, as after a DMA write);
//! - `F`: camera frames (the newest complete frame each time; `camera::MODE`);
//! - `C`: capture only, no datagrams but the report (camera frames counted and dropped);
//! - `D` (1 byte or more): nothing is sent; the board logs `bench: host done`, the end of a
//!   host test sequence.
//!
//! In `I` and `P`, byte `j` of a frame is [`pattern`]`(j)`, so the host checks every byte.
//!
//! **Host to board** (UDP [`RX_PORT`]): datagrams `[seq u32 LE, stream u32 LE, payload]`; the
//! payload's byte `j` must be [`pattern`]`(seq % 251 + j)` (shifted per datagram, so a stale or
//! misplaced buffer does not pass). The board checks every byte and counts datagrams, bad ones
//! and sequence gaps per stream; a new stream id resets the counts. A datagram with stream
//! [`QUERY`] gets the counts as an ASCII reply. Requests to the two ports can overlap: video
//! out and control in.
//!
//! Results also go to the log, after each run (nothing is logged while one runs).

use core::cell::RefCell;
use core::fmt::{self, Write};

use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpEndpoint, Stack};
use embassy_time::{Duration, Instant};
use esp_hal::peripherals::{I2C0, VDMA_CH0};
use log::{info, warn};
use static_cell::ConstStaticCell;

use crate::camera::{self, Camera};
use crate::{cpu, net};

/// Port of the board-to-host service.
pub const TX_PORT: u16 = 5010;
/// Port of the host-to-board check.
pub const RX_PORT: u16 = 5011;
/// UDP payload per datagram: the IPv6 MTU of 1500 minus the IPv6 (40) and UDP (8) headers.
pub const DATAGRAM: usize = 1452;
/// Header of each board-to-host datagram.
pub const HEADER: usize = 16;
/// Frame bytes per datagram.
pub const CHUNK: usize = DATAGRAM - HEADER;
/// Frame number of the report datagram.
pub const REPORT_FRAME: u32 = u32::MAX;
/// Stream id of a stats query on [`RX_PORT`].
pub const QUERY: u32 = u32::MAX;
/// Size of the internal-RAM source buffer.
pub const INTERNAL_LEN: usize = 64 * 1024;
/// Period of the per-datagram shift of the RX payload.
const RX_SHIFT: usize = 251;
/// Longest run a request may ask for.
const MAX_SECONDS: u16 = 600;
/// How long the frame sender waits for a camera frame.
const FRAME_TIMEOUT: Duration = Duration::from_millis(100);

/// Datagrams the TX socket buffers (the app copies into it, the stack drains it).
const TX_SOCKET_PACKETS: usize = 8;
/// Datagrams the RX socket buffers.
const RX_SOCKET_PACKETS: usize = 16;
/// Length of the RX check's query reply buffer, and the most a reply may take.
const REPLY_LEN: usize = 512;
/// Metadata slots of a socket's packet ring: one per 300 B of payload ring, plus two.
///
/// On the TX side they must outnumber the datagrams the payload ring can hold, with one to
/// spare for the wrap padding: smoltcp 0.13.1's `PacketBuffer::enqueue_with_infallible` calls
/// the fill closure *before* it takes the packet's metadata slot; when the wrap padding took
/// the last free slot, it returns `Full` after the closure ran, and embassy-net's
/// `send_to_with` panics on its next poll (`unwrap of f.take()`, udp.rs:304; fixed in smoltcp
/// 0.14). This service queues full datagrams, one short one per frame and the reports, so
/// the TX ring holds at most about ten. On the RX side the slots only bound how many small
/// datagrams can wait (more are dropped).
const fn meta_slots(payload: usize) -> usize {
    payload / 300 + 2
}
const TX_META: usize = meta_slots(TX_SOCKET_PACKETS * DATAGRAM);
const RX_META: usize = meta_slots(RX_SOCKET_PACKETS * DATAGRAM);
const REPLY_META: usize = meta_slots(4 * REPLY_LEN);

/// Byte `i` of every test frame and of the RX payload base.
pub const fn pattern(i: u32) -> u8 {
    (i.wrapping_mul(2_654_435_761) >> 24) as u8
}

#[repr(C, align(64))]
struct Internal([u8; INTERNAL_LEN]);

static INTERNAL: ConstStaticCell<Internal> = ConstStaticCell::new(Internal([0; INTERNAL_LEN]));
static RX_BASE: ConstStaticCell<[u8; DATAGRAM + RX_SHIFT]> =
    ConstStaticCell::new([0; DATAGRAM + RX_SHIFT]);

// Socket buffers: TX socket (requests in, the stream out), RX socket (checked datagrams in,
// query replies out).
static TX_RX_META: ConstStaticCell<[PacketMetadata; 4]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 4]);
static TX_RX_BUF: ConstStaticCell<[u8; 256]> = ConstStaticCell::new([0; 256]);
static TX_TX_META: ConstStaticCell<[PacketMetadata; TX_META]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; TX_META]);
static TX_TX_BUF: ConstStaticCell<[u8; TX_SOCKET_PACKETS * DATAGRAM]> =
    ConstStaticCell::new([0; TX_SOCKET_PACKETS * DATAGRAM]);
static RX_RX_META: ConstStaticCell<[PacketMetadata; RX_META]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; RX_META]);
static RX_RX_BUF: ConstStaticCell<[u8; RX_SOCKET_PACKETS * DATAGRAM]> =
    ConstStaticCell::new([0; RX_SOCKET_PACKETS * DATAGRAM]);
static RX_TX_META: ConstStaticCell<[PacketMetadata; REPLY_META]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; REPLY_META]);
static RX_TX_BUF: ConstStaticCell<[u8; 4 * REPLY_LEN]> = ConstStaticCell::new([0; 4 * REPLY_LEN]);

/// Bytes of internal RAM the service's socket and source buffers take.
pub const RAM_BYTES: usize = INTERNAL_LEN
    + DATAGRAM
    + RX_SHIFT
    + 256
    + (TX_SOCKET_PACKETS + RX_SOCKET_PACKETS) * DATAGRAM
    + 4 * REPLY_LEN
    + (4 + TX_META + RX_META + REPLY_META) * core::mem::size_of::<PacketMetadata>();

/// What the TX service needs besides the stack: PSRAM for the camera buffers and the PSRAM
/// source, and the camera's peripherals.
pub struct Resources {
    pub psram: &'static mut [u8],
    pub i2c0: I2C0<'static>,
    pub vdma: VDMA_CH0<'static>,
}

/// Spawns the two service tasks.
pub fn start(spawner: Spawner, stack: Stack<'static>, res: Resources) {
    spawner.spawn(tx_task(stack, res).unwrap());
    spawner.spawn(rx_task(stack).unwrap());
    info!(
        "bench: TX service on UDP {TX_PORT}, RX check on UDP {RX_PORT}, {DATAGRAM}-byte datagrams, {} B of buffers in internal RAM",
        RAM_BYTES
    );
}

/// Writes back and invalidates `buf`'s cache lines (L1 and L2): the next read comes from
/// PSRAM, as after a DMA write.
fn flush_cache(buf: &[u8]) {
    unsafe extern "C" {
        fn Cache_WriteBack_Invalidate_Addr(map: u32, addr: u32, size: u32) -> i32;
    }
    // ROM cache maps (`cache_map_t`): L1 data cache and L2 cache.
    const L1_L2: u32 = (1 << 4) | (1 << 5);
    // SAFETY: ROM call on a range the caller owns; dirty lines are written back first.
    unsafe {
        Cache_WriteBack_Invalidate_Addr(L1_L2, buf.as_ptr() as u32, buf.len() as u32);
    }
}

// ── Board to host ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Internal,
    Psram,
    Frames,
    Capture,
}

impl Kind {
    fn parse(b: u8) -> Option<Self> {
        match b {
            b'I' => Some(Self::Internal),
            b'P' => Some(Self::Psram),
            b'F' => Some(Self::Frames),
            b'C' => Some(Self::Capture),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Psram => "psram",
            Self::Frames => "camera",
            Self::Capture => "capture",
        }
    }
}

/// State at the start and end of a run.
#[derive(Clone, Copy)]
struct Probe {
    at: Instant,
    cpu: cpu::Snapshot,
    net: net::Stats,
}

/// The probes of the current run, start and end (kept out of the task's stack frame).
static PROBES: Mutex<RefCell<[Option<Probe>; 2]>> = Mutex::new(RefCell::new([None, None]));
const START: usize = 0;
const END: usize = 1;

/// Takes probe `i` now. Returns its time.
#[inline(never)]
fn mark(i: usize) -> Instant {
    let p = Probe {
        at: Instant::now(),
        cpu: cpu::snapshot(),
        net: net::stats(),
    };
    critical_section::with(|cs| PROBES.borrow_ref_mut(cs)[i] = Some(p));
    p.at
}

/// Counts of one run.
#[derive(Default)]
struct Tally {
    datagrams: u32,
    bytes: u64,
    errors: u32,
    /// Frames sent whole (every datagram of the frame queued).
    frames: u32,
    /// Cycles in the copy into the socket buffer (header and payload).
    copy_cycles: u64,
    /// Cycles of the cache write-back + invalidation of the PSRAM source.
    flush_cycles: u64,
}

#[allow(
    clippy::large_stack_frames,
    reason = "a spawned task: its future lives in the task's static storage, and its poll frame \
    (2.4 KiB) is on the main stack, which has most of internal RAM"
)]
#[embassy_executor::task]
async fn tx_task(stack: Stack<'static>, res: Resources) {
    cpu::name_current("tx");
    let (cam_mem, rest) = res.psram.split_at_mut(camera::BUFFERS * camera::BUFFER_LEN);
    let psram_src = &mut rest[..Camera::frame_bytes()];
    for (i, b) in psram_src.iter_mut().enumerate() {
        *b = pattern(i as u32);
    }
    flush_cache(psram_src);
    let internal = &mut INTERNAL.take().0;
    for (i, b) in internal.iter_mut().enumerate() {
        *b = pattern(i as u32);
    }
    let mut camera = camera::init(res.i2c0, res.vdma, cam_mem).await;

    let mut sock = UdpSocket::new(
        stack,
        TX_RX_META.take(),
        TX_RX_BUF.take(),
        TX_TX_META.take(),
        TX_TX_BUF.take(),
    );
    sock.bind(TX_PORT).expect("bind TX port");
    log_ready();
    let mut req = [0u8; 8];
    loop {
        let Ok((n, meta)) = sock.recv_from(&mut req).await else {
            continue;
        };
        if n >= 1 && req[0] == b'D' {
            log_done();
            continue;
        }
        let (Some(kind), true) = (Kind::parse(req[0]), n >= 4) else {
            warn!("bench: bad request of {n} B from {}", meta.endpoint);
            continue;
        };
        let secs = u16::from_le_bytes([req[2], req[3]]).clamp(1, MAX_SECONDS);
        let ep = meta.endpoint;
        let needs_camera = matches!(kind, Kind::Frames | Kind::Capture);
        let cam = match camera.as_deref_mut() {
            Some(c) if needs_camera => {
                if !c.start().await {
                    continue;
                }
                c.reset_stats();
                Some(c)
            }
            None if needs_camera => {
                warn!("bench: {} requested, no camera", kind.name());
                continue;
            }
            _ => None,
        };
        let window = Duration::from_secs(u64::from(secs));
        let deadline = mark(START) + window;
        let mut t = Tally::default();
        match kind {
            Kind::Internal => {
                let mut frame = 0;
                while send_frame(&mut sock, ep, &internal[..], frame, deadline, &mut t).await {
                    frame += 1;
                }
            }
            Kind::Psram => {
                let mut frame = 0;
                loop {
                    let c0 = cpu::mcycle();
                    flush_cache(psram_src);
                    t.flush_cycles += u64::from(cpu::mcycle().wrapping_sub(c0));
                    if !send_frame(&mut sock, ep, psram_src, frame, deadline, &mut t).await {
                        break;
                    }
                    frame += 1;
                }
            }
            Kind::Frames | Kind::Capture => {
                let Some(c) = cam else { continue };
                let mut held = None;
                while Instant::now() < deadline {
                    let Some(f) = c.next(held.take(), FRAME_TIMEOUT).await else {
                        t.errors += 1;
                        break;
                    };
                    if kind == Kind::Frames
                        && !send_frame(&mut sock, ep, &f, f.seq(), deadline, &mut t).await
                    {
                        break;
                    }
                    if kind == Kind::Capture {
                        t.frames += 1;
                    }
                    held = Some(f);
                }
                drop(held);
            }
        }
        mark(END);
        let cam = if needs_camera {
            camera.as_deref().map(Camera::stats)
        } else {
            None
        };
        if let (Some(c), true) = (camera.as_deref_mut(), needs_camera) {
            c.stop().await;
        }
        let r = sock
            .send_to_with(DATAGRAM, ep, |buf| {
                let len = write_report(buf, kind, &t, cam.as_ref());
                (len, ())
            })
            .await;
        if let Err(e) = r {
            warn!("bench: report not sent: {e:?}");
        }
    }
}

/// Sends `data` as frame `frame` to `ep`, [`CHUNK`] bytes per datagram. `false` if the
/// deadline came first.
async fn send_frame(
    sock: &mut UdpSocket<'_>,
    ep: IpEndpoint,
    data: &[u8],
    frame: u32,
    deadline: Instant,
    t: &mut Tally,
) -> bool {
    let total = data.len() as u32;
    for (i, chunk) in data.chunks(CHUNK).enumerate() {
        if Instant::now() >= deadline {
            return false;
        }
        let header = [t.datagrams, frame, (i * CHUNK) as u32, total];
        let len = HEADER + chunk.len();
        match sock
            .send_to_with(len, ep, |buf| {
                let c0 = cpu::mcycle();
                let (words, _) = buf.as_chunks_mut::<4>();
                for (d, v) in words.iter_mut().zip(header) {
                    *d = v.to_le_bytes();
                }
                buf[HEADER..len].copy_from_slice(chunk);
                (len, cpu::mcycle().wrapping_sub(c0))
            })
            .await
        {
            Ok(c) => {
                t.copy_cycles += u64::from(c);
                t.datagrams += 1;
                t.bytes += len as u64;
            }
            Err(_) => t.errors += 1,
        }
    }
    t.frames += 1;
    true
}

/// Writes the report datagram of a TX run into `buf` (header, then text) and logs the text.
/// Returns its length.
#[allow(
    clippy::large_stack_frames,
    reason = "once per run, after it, on the main stack (the two probes are 544 B)"
)]
#[inline(never)]
fn write_report(buf: &mut [u8], kind: Kind, t: &Tally, cam: Option<&esp32p4_csi::Stats>) -> usize {
    let (words, _) = buf.as_chunks_mut::<4>();
    for (d, v) in words.iter_mut().zip([t.datagrams, REPORT_FRAME, 0, 0]) {
        *d = v.to_le_bytes();
    }
    let [Some(before), Some(after)] = critical_section::with(|cs| *PROBES.borrow_ref(cs)) else {
        return 0;
    };
    let us = (after.at - before.at).as_micros().max(1);
    let mut w = BufWriter::new(&mut buf[HEADER..]);
    write_tally(&mut w, kind, t, us);
    write_net(&mut w, &after.net.since(&before.net));
    write_cpu(&mut w, &after.cpu.since(&before.cpu), t.datagrams);
    if let Some(s) = cam {
        write_cam(&mut w, s, us);
    }
    write_emac(&mut w);
    let len = w.len;
    log_text(&buf[HEADER..HEADER + len]);
    HEADER + len
}

#[inline(never)]
fn log_text(text: &[u8]) {
    info!("bench: {}", core::str::from_utf8(text).unwrap_or("?"));
}

/// `a / b` with one decimal, for per-datagram and per-frame averages.
struct Ratio(u64, u64);

impl fmt::Display for Ratio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tenths = (self.0 * 10).checked_div(self.1).unwrap_or(0);
        write!(f, "{}.{}", tenths / 10, tenths % 10)
    }
}

#[inline(never)]
fn write_tally(w: &mut BufWriter<'_>, kind: Kind, t: &Tally, us: u64) {
    let n = u64::from(t.datagrams.max(1));
    let _ = write!(
        w,
        "tx source={} us={us} datagrams={} bytes={} frames={} errors={} rate_kBps={} copy_cyc={} flush_us={}",
        kind.name(),
        t.datagrams,
        t.bytes,
        t.frames,
        t.errors,
        t.bytes * 1000 / us,
        Ratio(t.copy_cycles, n),
        t.flush_cycles / 400,
    );
}

#[inline(never)]
fn write_net(w: &mut BufWriter<'_>, s: &net::Stats) {
    let tx = u64::from(s.tx_frames.max(1));
    let rx = u64::from(s.rx_frames.max(1));
    let _ = write!(
        w,
        " | net tx_frames={} tx_stack_cyc={} tx_driver_cyc={} rx_frames={} rx_stack_cyc={} rx_driver_cyc={}",
        s.tx_frames,
        Ratio(s.tx_stack, tx),
        Ratio(s.tx_driver, tx),
        s.rx_frames,
        Ratio(s.rx_stack, rx),
        Ratio(s.rx_driver, rx),
    );
    let _ = write!(
        w,
        " token_calls={} token_cyc={}",
        s.token_calls,
        Ratio(s.token_cycles, u64::from(s.token_calls.max(1))),
    );
}

#[inline(never)]
fn write_cpu(w: &mut BufWriter<'_>, c: &cpu::Snapshot, datagrams: u32) {
    let n = u64::from(datagrams.max(1));
    let _ = write!(
        w,
        " | cpu busy_pct={} cyc_per_datagram:",
        Ratio(c.busy * 100, c.at.max(1))
    );
    for a in c.tasks.iter().filter(|a| a.id != 0) {
        let _ = write!(w, " {}={}/{}polls", a.name, Ratio(a.cycles, n), a.polls);
    }
}

/// EMAC DMA status, missed frames since the last report (the counter clears on read) and the
/// MAC debug register.
#[inline(never)]
fn write_emac(w: &mut BufWriter<'_>) {
    let r = net::emac_regs();
    let _ = write!(
        w,
        " | emac status={:#010x} missed={:#010x} debug={:#010x}",
        r.status, r.missed, r.debug
    );
}

#[inline(never)]
fn write_cam(w: &mut BufWriter<'_>, s: &esp32p4_csi::Stats, us: u64) {
    let _ = write!(
        w,
        " | cam frames_done={} fps={} dropped={} overwritten={} last_bytes={}",
        s.frames_done,
        Ratio(u64::from(s.frames_done) * 1_000_000, us),
        s.frames_dropped,
        s.frames_overwritten,
        s.last_frame_bytes,
    );
    let _ = write!(
        w,
        " dma_err={} host_err={} bridge_err={} (bits {:#x} {:#x} {:#x})",
        s.dma_errors,
        s.host_errors,
        s.bridge_errors,
        s.dma_error_bits,
        s.host_error_main,
        s.bridge_error_bits,
    );
}

// ── Host to board ─────────────────────────────────────────────────────────────────────────

/// Counts of the current RX stream.
#[derive(Clone, Copy, Default)]
struct RxStats {
    stream: u32,
    datagrams: u32,
    bytes: u64,
    bad: u32,
    bad_bytes: u64,
    first_bad: Option<(u32, u16, u8, u8)>,
    /// Highest sequence number seen plus one.
    next: u32,
    /// Datagrams older than the highest seen.
    late: u32,
    first_at: Option<Instant>,
    last_at: Option<Instant>,
    sizes: (u16, u16),
}

impl RxStats {
    /// Checks one datagram against the pattern. Returns the sender if it is a query.
    fn on_datagram(&mut self, base: &[u8], buf: &[u8], from: IpEndpoint) -> Option<IpEndpoint> {
        if buf.len() < 8 {
            return None;
        }
        let seq = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let stream = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if stream == QUERY {
            return Some(from);
        }
        let now = Instant::now();
        if stream != self.stream || self.first_at.is_none() {
            *self = RxStats {
                stream,
                first_at: Some(now),
                sizes: (u16::MAX, 0),
                ..RxStats::default()
            };
        }
        self.last_at = Some(now);
        let payload = &buf[8..];
        let k = seq as usize % RX_SHIFT;
        let expected = &base[k..k + payload.len().min(base.len() - k)];
        if payload != expected {
            self.bad += 1;
            for (j, (&g, &e)) in payload.iter().zip(expected).enumerate() {
                if g != e {
                    self.bad_bytes += 1;
                    self.first_bad.get_or_insert((seq, j as u16, g, e));
                }
            }
        }
        self.datagrams += 1;
        self.bytes += buf.len() as u64;
        let len = buf.len() as u16;
        self.sizes = (self.sizes.0.min(len), self.sizes.1.max(len));
        if seq >= self.next {
            self.next = seq + 1;
        } else {
            self.late += 1;
        }
        None
    }
}

impl fmt::Display for RxStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let span = match (self.first_at, self.last_at) {
            (Some(a), Some(b)) => (b - a).as_micros(),
            _ => 0,
        };
        let lost = i64::from(self.next) - i64::from(self.datagrams);
        write!(
            f,
            "rx stream={} datagrams={} bytes={} bad={} bad_bytes={} lost={} late={} size_min={} size_max={} span_us={} rate_kBps={}",
            self.stream,
            self.datagrams,
            self.bytes,
            self.bad,
            self.bad_bytes,
            lost,
            self.late,
            if self.datagrams == 0 { 0 } else { self.sizes.0 },
            self.sizes.1,
            span,
            (self.bytes * 1000).checked_div(span).unwrap_or(0),
        )?;
        if let Some((seq, j, got, want)) = self.first_bad {
            write!(
                f,
                " first_bad=seq {seq} byte {j} got {got:#04x} want {want:#04x}"
            )?;
        }
        Ok(())
    }
}

#[embassy_executor::task]
async fn rx_task(stack: Stack<'static>) {
    cpu::name_current("rx");
    let base = RX_BASE.take();
    for (i, b) in base.iter_mut().enumerate() {
        *b = pattern(i as u32);
    }
    let base = &base[..];
    let mut sock = UdpSocket::new(
        stack,
        RX_RX_META.take(),
        RX_RX_BUF.take(),
        RX_TX_META.take(),
        RX_TX_BUF.take(),
    );
    sock.bind(RX_PORT).expect("bind RX port");
    let mut st = RxStats::default();
    loop {
        let query = sock
            .recv_from_with(|buf, meta| st.on_datagram(base, buf, meta.endpoint))
            .await;
        if let Some(ep) = query {
            let r = sock
                .send_to_with(REPLY_LEN, ep, |buf| (write_rx(buf, &st), ()))
                .await;
            if let Err(e) = r {
                warn!("bench: RX stats not sent: {e:?}");
            }
        }
    }
}

/// Writes the RX counts into `buf` as text and logs them. Returns the length.
#[inline(never)]
fn write_rx(buf: &mut [u8], st: &RxStats) -> usize {
    let mut w = BufWriter::new(buf);
    let _ = write!(w, "{st}");
    write_emac(&mut w);
    let len = w.len;
    log_text(&buf[..len]);
    len
}

/// `fmt::Write` into a byte buffer; output past its end is dropped.
struct BufWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> BufWriter<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }
}

impl Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

#[inline(never)]
fn log_ready() {
    info!("bench: ready");
}

/// The host's `D` request: its test sequence is over (`board-run.sh` stops on this line).
#[inline(never)]
fn log_done() {
    info!("bench: host done");
}
