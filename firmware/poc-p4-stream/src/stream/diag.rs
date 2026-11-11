//! Test hooks on UDP [`DIAG_PORT`] for the host's tools (`tools/streamdiag.py`, the viewer's
//! board test). One ASCII command byte per datagram; every command is answered with a text
//! datagram to its sender:
//!
//! - `M`: mark: the start of a measurement window (CPU accounts, network counters, time).
//! - `R`: report since the mark: per core, the executor's busy share and each task's share,
//!   cycles per sent datagram and polls (shares of the wall-clock window at the CPU clock);
//!   the interrupt handlers' share, the part of it outside executor passes, the wake path and
//!   the busy share with them (`crate::irq`); with the `idle-counter` feature the idle share
//!   and the sleep path;
//!   the stream counters, bad datagrams on the camera port, the receiver's counters, the
//!   EMAC's missed-frame count and the TX frames it reported with an error since boot (of
//!   which underflows). A core whose executor did not run in the window gets only its busy
//!   share.
//!   Without the `accounting` feature the CPU and network fields are replaced by
//!   `accounting=off`. Nothing is logged.
//! - `I`: per core, each wrapped interrupt handler's calls and mean own cycles per call since
//!   the mark (a separate reply: the report fills a datagram).
//! - `N`: the send path since the mark: the chunk encodings (count, cycles each, data bytes,
//!   copies whose ends differ modulo 4 and 16), and the driver wrapper's per-frame cycles
//!   (smoltcp's part and the driver's part of TX and RX) and token requests. Zeros without
//!   `accounting`.
//! - `P`: the frame path since the mark: the cache prefetch in use, and the encode's driver
//!   set-up (first poll), wake-up after the end (from the end of the 2D-DMA RX handler to the
//!   last poll) and end work (last poll): count, mean and longest cycles. Zeros without
//!   `accounting`.
//! - `F1` / `F2`: the next mode change fails: `1` a sensor bus error (every write of the start
//!   goes to an address nobody answers), `2` no first frame (stream-on left out); the
//!   request is answered with the error and the previous mode runs again.
//! - `S`: stall: the sensor's stream is turned off behind the receiver's back; the capture task
//!   must notice and restart the camera.
//! - `D`: logs `stream: host done` (the end of a host test; `board-run.sh` stops on it).
//! - `C<n>` (`n` in decimal ASCII): the next `n` JPEG encodes get a 4 KiB output, too small
//!   for any camera frame: the driver's `OutputTooSmall` path (each costs one frame, counted
//!   as an encoder error; the report has the time to the error).
//! - `X1` / `X0`: encode-only on / off: JPEG files are made and dropped, not sent (the encode
//!   time without the network's traffic on the bus).
//! - `A<d>` (one digit): the cache prefetch around the CPU's reads of PSRAM
//!   ([`crate::prefetch::Mode`]: 0 off, 1 L1 and L2 autoload, 2 L1, 3 L2, 4 L1 and L2 on a hit
//!   only, 5 L1 preload, 6 L1 preload of the next chunk); the `P` reply names the one in use.

use core::cell::{Cell, RefCell};
use core::fmt::{self, Write};
use core::sync::atomic::Ordering;

use critical_section::Mutex;
use embassy_net::Stack;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_time::Instant;
use esp32p4_csi::Stats;
use log::info;
use static_cell::ConstStaticCell;

use super::{BAD_DATAGRAMS, CMDS, Cmd, DIAG_PORT, FAULT, SEND_BUSY_US, jpeg, send, shared};
use crate::{cpu, irq, net};

const RX_BYTES: usize = 256;
/// Twice [`REPLY_MAX`]: smoltcp asks for `REPLY_MAX` contiguous bytes, and with less than
/// twice that a ring whose write position sits mid-way can never offer them (the send then
/// waits for a dispatch that never comes).
const TX_BYTES: usize = 2 * REPLY_MAX;
const REPLY_MAX: usize = 1400;

static RX_META: ConstStaticCell<[PacketMetadata; 4]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 4]);
static RX_BUF: ConstStaticCell<[u8; RX_BYTES]> = ConstStaticCell::new([0; RX_BYTES]);
static TX_META: ConstStaticCell<[PacketMetadata; 4]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 4]);
static TX_BUF: ConstStaticCell<[u8; TX_BYTES]> = ConstStaticCell::new([0; TX_BYTES]);

/// Internal RAM of the hooks' socket.
pub const RAM_BYTES: usize = RX_BYTES + TX_BYTES + 8 * core::mem::size_of::<PacketMetadata>();

/// The receiver's counters as the capture task last read them (it owns the receiver).
pub(super) static CSI_STATS: Mutex<Cell<Stats>> = Mutex::new(Cell::new(Stats {
    frames_done: 0,
    frames_dropped: 0,
    frames_overwritten: 0,
    last_frame_bytes: 0,
    last_block_items: 0,
    dma_errors: 0,
    dma_error_bits: 0,
    host_errors: 0,
    host_error_main: 0,
    host_error_detail: [0; 9],
    bridge_errors: 0,
    bridge_error_bits: 0,
}));

/// The start of the measurement window (in a static: two CPU snapshots are about 800 B).
struct Mark {
    at: Instant,
    cpu: [cpu::Snapshot; cpu::CORES],
    irq: [irq::Snapshot; cpu::CORES],
    net: net::Stats,
    copies: send::Copies,
    chunk: net::chunk::Stats,
    sent: u32,
    path: [cpu::TimedCounts; PATH],
}

/// The timed events of the `P` reply, and their names.
const PATH: usize = 3;
const PATH_NAMES: [&str; PATH] = ["enc_setup", "enc_wake", "enc_end"];

/// The timed events of the `P` reply now; `reset_max` starts new longest ones.
fn path(reset_max: bool) -> [cpu::TimedCounts; PATH] {
    [
        jpeg::SETUP.take(reset_max),
        jpeg::WAKE.take(reset_max),
        jpeg::END.take(reset_max),
    ]
}

static MARK: Mutex<RefCell<Option<Mark>>> = Mutex::new(RefCell::new(None));

#[embassy_executor::task]
pub(super) async fn task(stack: Stack<'static>) {
    cpu::name_current("diag");
    let mut sock = UdpSocket::new(
        stack,
        RX_META.take(),
        RX_BUF.take(),
        TX_META.take(),
        TX_BUF.take(),
    );
    sock.bind(DIAG_PORT).expect("bind the diag port");
    set_mark();
    let mut cmd = [0u8; 16];
    loop {
        let Ok((n, meta)) = sock.recv_from(&mut cmd).await else {
            continue;
        };
        let r = sock
            .send_to_with(REPLY_MAX, meta.endpoint, |buf| {
                let mut w = BufWriter { buf, len: 0 };
                match cmd[..n] {
                    [b'M', ..] => {
                        set_mark();
                        let _ = w.write_str("mark");
                    }
                    [b'R', ..] => write_report(&mut w),
                    [b'I', ..] => write_irq(&mut w),
                    [b'N', ..] => write_net(&mut w),
                    [b'P', ..] => write_path(&mut w),
                    [b'F', d @ b'1'..=b'2', ..] => {
                        FAULT.store(d - b'0', Ordering::Relaxed);
                        let _ = w.write_str("fault armed");
                    }
                    [b'S', ..] => {
                        let _ = w.write_str(if CMDS.try_send(Cmd::Stall).is_ok() {
                            "stall"
                        } else {
                            "busy"
                        });
                    }
                    [b'D', ..] => {
                        log_done();
                        let _ = w.write_str("done");
                    }
                    [b'C', ref digits @ ..] => {
                        let n = digits
                            .iter()
                            .take_while(|d| d.is_ascii_digit())
                            .fold(0u32, |a, d| a * 10 + u32::from(d - b'0'));
                        jpeg::SMALL_OUTPUTS.store(n, Ordering::Relaxed);
                        let _ = write!(w, "small outputs {n}");
                    }
                    [b'X', d @ b'0'..=b'1', ..] => {
                        jpeg::ENCODE_ONLY.store(d == b'1', Ordering::Relaxed);
                        let _ = w.write_str(if d == b'1' { "encode only" } else { "send" });
                    }
                    [b'A', d @ b'0'..=b'9', ..] => {
                        let _ = if crate::prefetch::set_mode(d - b'0') {
                            write!(w, "prefetch {}", crate::prefetch::mode_name())
                        } else {
                            w.write_str("unknown prefetch mode")
                        };
                    }
                    _ => {
                        let _ = w.write_str("unknown command");
                    }
                }
                (w.len, ())
            })
            .await;
        let _ = r;
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "on demand, outside the frame path, on the main stack: the CPU snapshots are about \
    300 B each"
)]
#[inline(never)]
fn set_mark() {
    critical_section::with(|cs| {
        *MARK.borrow_ref_mut(cs) = Some(Mark {
            at: Instant::now(),
            cpu: [cpu::snapshot_of(0), cpu::snapshot_of(1)],
            irq: [irq::snapshot(0), irq::snapshot(1)],
            net: net::stats(),
            copies: send::copies(),
            chunk: net::chunk::stats(),
            sent: shared(|s| s.counters.datagrams_sent),
            path: path(true),
        });
    });
}

#[allow(
    clippy::large_stack_frames,
    reason = "on demand, outside the frame path, on the main stack: the CPU snapshots are about \
    300 B each"
)]
#[inline(never)]
fn write_report(w: &mut BufWriter<'_>) {
    let now = Instant::now();
    let (since, n, sent) = critical_section::with(|cs| {
        let m = MARK.borrow_ref(cs);
        let m = m.as_ref().expect("marked at start");
        (
            m.at,
            net::stats().since(&m.net),
            shared(|s| s.counters.datagrams_sent).wrapping_sub(m.sent),
        )
    });
    let us = (now - since).as_micros().max(1);
    let (counters, enc) = shared(|s| (s.counters, s.encoder));
    let csi = critical_section::with(|cs| CSI_STATS.borrow(cs).get());
    let window = us * u64::from(esp_hal::clock::cpu_clock().as_mhz());
    let _ = write!(w, "window_us={us} uptime_us={}", now.as_micros());
    if !cfg!(feature = "accounting") {
        let _ = w.write_str(" accounting=off");
    }
    for core in 0..cpu::CORES {
        write_core(w, core, window, sent);
    }
    let _ = write!(w, " datagrams={sent}");
    if cfg!(feature = "accounting") {
        let _ = write!(
            w,
            " net_tx_frames={} net_rx_frames={} tx_stack_cyc={} tx_driver_cyc={}",
            n.tx_frames,
            n.rx_frames,
            n.tx_stack / u64::from(n.tx_frames.max(1)),
            n.tx_driver / u64::from(n.tx_frames.max(1)),
        );
    }
    let _ = write!(
        w,
        " captured={} sent={} skipped={} capture_errors={} datagrams_sent={} bad_requests={}",
        counters.frames_captured,
        counters.frames_sent,
        counters.frames_skipped,
        counters.capture_errors,
        counters.datagrams_sent,
        BAD_DATAGRAMS.load(Ordering::Relaxed),
    );
    let emac = net::emac_regs();
    let _ = write!(
        w,
        " csi_done={} csi_dropped={} csi_overwritten={} csi_last_bytes={} dma_err={} host_err={} host_main={:#x} bridge_err={} bridge_bits={:#x} emac_missed={} emac_tx_errors={} emac_tx_underflows={}",
        csi.frames_done,
        csi.frames_dropped,
        csi.frames_overwritten,
        csi.last_frame_bytes,
        csi.dma_errors,
        csi.host_errors,
        csi.host_error_main,
        csi.bridge_errors,
        csi.bridge_error_bits,
        emac.missed,
        emac.tx_errors,
        emac.tx_underflows,
    );
    let _ = write!(
        w,
        " quality={} jpeg_len={} encode_us={} enc_errors={} enc_skipped={} encoded={} encode_us_sum={} encoded_bytes={} send_busy_us={} enc_last_error={} enc_last_error_us={}",
        enc.quality,
        enc.frame_len,
        enc.encode_us,
        enc.errors,
        enc.frames_skipped,
        jpeg::ENCODED.load(Ordering::Relaxed),
        jpeg::ENCODE_US.load(Ordering::Relaxed),
        jpeg::ENCODED_BYTES.load(Ordering::Relaxed),
        SEND_BUSY_US.load(Ordering::Relaxed),
        critical_section::with(|cs| jpeg::LAST_ERROR.borrow(cs).get()),
        jpeg::LAST_ERROR_US.load(Ordering::Relaxed),
    );
}

/// One core's busy shares and its tasks' shares and polls since the mark.
#[allow(
    clippy::large_stack_frames,
    reason = "on demand, outside the frame path; two CPU snapshots of about 400 B"
)]
#[inline(never)]
fn write_core(w: &mut BufWriter<'_>, core: usize, window: u64, sent: u32) {
    let c = core_since_mark(core);
    if cfg!(feature = "idle-counter") {
        let _ = write!(w, " core{core}_idle_pct={}", Pct(c.idle, window));
    }
    if !cfg!(feature = "accounting") {
        return;
    }
    let _ = write!(w, " core{core}_busy_pct={}", Pct(c.busy, window));
    let mut ran = false;
    for t in c.tasks.iter().filter(|t| t.id != 0 && t.polls > 0) {
        ran = true;
        let _ = write!(
            w,
            " core{core}_{}_pct={} core{core}_{}_cyc_per_dgram={} core{core}_{}_polls={}",
            t.name,
            Pct(t.cycles, window),
            t.name,
            t.cycles / u64::from(sent.max(1)),
            t.name,
            t.polls,
        );
    }
    if !ran {
        return;
    }
    let i = irq_since_mark(core);
    let _ = write!(
        w,
        " core{core}_busy_all_pct={} core{core}_irq_pct={} core{core}_irq_outside_pct={} core{core}_wake_pct={} core{core}_wakes={} core{core}_wake_cyc={}",
        Pct(i.busy_all(c.busy), window),
        Pct(i.total(), window),
        Pct(i.outside, window),
        Pct(i.wake_cycles, window),
        i.wakes,
        i.wake_cycles / u64::from(i.wakes.max(1)),
    );
    if cfg!(feature = "idle-counter") {
        let _ = write!(w, " core{core}_sleep_pct={}", Pct(i.sleep_cycles, window));
    }
}

/// Core `core`'s accounts since the mark.
#[allow(
    clippy::large_stack_frames,
    reason = "on demand, outside the frame path; two CPU snapshots of about 270 B"
)]
#[inline(never)]
fn core_since_mark(core: usize) -> cpu::Snapshot {
    let now = cpu::snapshot_of(core);
    critical_section::with(|cs| {
        let m = MARK.borrow_ref(cs);
        now.since(&m.as_ref().expect("marked at start").cpu[core])
    })
}

/// Core `core`'s interrupt-handler accounts since the mark.
#[inline(never)]
fn irq_since_mark(core: usize) -> irq::Snapshot {
    let now = irq::snapshot(core);
    critical_section::with(|cs| {
        let m = MARK.borrow_ref(cs);
        now.since(&m.as_ref().expect("marked at start").irq[core])
    })
}

/// The `I` reply: per core, each wrapped handler's calls and mean own cycles since the mark.
#[allow(
    clippy::large_stack_frames,
    reason = "on demand, outside the frame path; two handler snapshots of about 130 B"
)]
#[inline(never)]
fn write_irq(w: &mut BufWriter<'_>) {
    let us = critical_section::with(|cs| {
        let m = MARK.borrow_ref(cs);
        (Instant::now() - m.as_ref().expect("marked at start").at).as_micros()
    });
    let _ = write!(w, "window_us={us}");
    if !cfg!(feature = "accounting") {
        let _ = w.write_str(" accounting=off");
        return;
    }
    for core in 0..cpu::CORES {
        let i = irq_since_mark(core);
        for (k, name) in irq::NAMES.iter().enumerate() {
            if i.calls[k] > 0 {
                let _ = write!(
                    w,
                    " core{core}_irq_{name}_calls={} core{core}_irq_{name}_cyc={}",
                    i.calls[k],
                    i.cycles[k] / u64::from(i.calls[k]),
                );
            }
        }
    }
}

/// The `N` reply: the chunk encodings, the datagrams sent beside smoltcp (`direct`: cycles from
/// the room found to the frame queued, of which the headers and the checksum, and how many
/// waited for room) and the driver wrapper's counts since the mark.
#[allow(
    clippy::large_stack_frames,
    reason = "on demand, outside the frame path, on the main stack: the counts are about 120 B"
)]
#[inline(never)]
fn write_net(w: &mut BufWriter<'_>) {
    let (at, n, c, d) = critical_section::with(|cs| {
        let m = MARK.borrow_ref(cs);
        let m = m.as_ref().expect("marked at start");
        (
            m.at,
            net::stats().since(&m.net),
            send::copies().since(&m.copies),
            net::chunk::stats().since(&m.chunk),
        )
    });
    let per = |a: u64, b: u32| a / u64::from(b.max(1));
    let _ = write!(
        w,
        "window_us={} copies={} copy_cyc={} copy_bytes={} copy_misaligned={} copy_unlike16={}",
        (Instant::now() - at).as_micros(),
        c.count,
        per(c.cycles, c.count),
        per(c.bytes, c.count),
        c.unlike4,
        c.unlike16,
    );
    let _ = write!(
        w,
        " direct={} direct_cyc={} direct_hdr_cyc={} direct_copy_cyc={} direct_waits={}",
        d.datagrams,
        per(d.cycles, d.datagrams),
        per(d.header_cycles, d.datagrams),
        per(d.copy_cycles, d.datagrams),
        d.waits,
    );
    let _ = write!(
        w,
        " tx_frames={} tx_stack_cyc={} tx_driver_cyc={} rx_frames={} rx_stack_cyc={} rx_driver_cyc={} token_calls={} token_cyc={}",
        n.tx_frames,
        per(n.tx_stack, n.tx_frames),
        per(n.tx_driver, n.tx_frames),
        n.rx_frames,
        per(n.rx_stack, n.rx_frames),
        per(n.rx_driver, n.rx_frames),
        n.token_calls,
        per(n.token_cycles, n.token_calls),
    );
}

/// The `P` reply: the frame path's timed events since the mark.
#[allow(
    clippy::large_stack_frames,
    reason = "on demand, outside the frame path, on the main stack: the formatting of nine counts"
)]
#[inline(never)]
fn write_path(w: &mut BufWriter<'_>) {
    let now = path(false);
    let (at, before) = critical_section::with(|cs| {
        let m = MARK.borrow_ref(cs);
        let m = m.as_ref().expect("marked at start");
        (m.at, m.path)
    });
    let _ = write!(
        w,
        "window_us={} prefetch={}",
        (Instant::now() - at).as_micros(),
        crate::prefetch::mode_name()
    );
    for ((name, n), b) in PATH_NAMES.iter().zip(now).zip(before) {
        let d = n.since(&b);
        let _ = write!(
            w,
            " {name}_n={} {name}_cyc={} {name}_max={}",
            d.n,
            d.mean(),
            d.max
        );
    }
}

/// `part` of `whole` in percent, one decimal.
struct Pct(u64, u64);

impl fmt::Display for Pct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let t = self.0 * 1000 / self.1.max(1);
        write!(f, "{}.{}", t / 10, t % 10)
    }
}

/// `fmt::Write` into a byte buffer; output past its end is dropped.
struct BufWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// The host's `D`: its test is over (`board-run.sh` stops on this line).
#[inline(never)]
fn log_done() {
    info!("stream: host done");
}
