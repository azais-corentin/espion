//! The send task: the only sender on the camera port. Answers go first, through the socket;
//! the frame in flight (a raw frame or a JPEG file) goes out chunk by chunk on the protocol's
//! grid, each chunk beside smoltcp (`net::chunk`): `espion_proto` writes the chunk's header
//! into a TX descriptor's buffer and PIE copies the bytes from PSRAM after it (the one copy).
//! A RAW10 frame sent as RAW8 has its 8-bit pixels written after the header instead. Until a
//! frame from the peer was received (its MAC unknown), chunks go through the socket: the
//! header written into the socket's ring, PIE copying the bytes.

use core::cell::RefCell;
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::sync::atomic::{AtomicU32, Ordering};

use embassy_futures::select::{Either, select};
use embassy_net::IpEndpoint;
use embassy_net::udp::{SendError, UdpSocket};
use embassy_time::Instant;
use esp32p4_pie::Pie;
use espion_proto::cam::{CAM_PORT, CHUNK_MAX, FrameChunk, MAX_DATAGRAM};

use super::{JOB, Job, Outgoing, RESPONSES, SEND_BUSY_US, shared};
use crate::cpu::{self, Counter64};
use crate::net::chunk;

#[embassy_executor::task]
pub(super) async fn task(sock: &'static RefCell<UdpSocket<'static>>, pie: &'static Pie) {
    cpu::name_current("send");
    loop {
        match select(RESPONSES.receive(), JOB.wait()).await {
            Either::First(r) => respond(sock, &r).await,
            Either::Second(job) => send_frame(sock, pie, job).await,
        }
    }
}

/// Counts of the chunk encodings (header and data into the socket's ring) since boot, for the
/// test hooks' network report (zeros without the `accounting` feature).
#[derive(Clone, Copy, Default)]
pub(super) struct Copies {
    /// Chunks encoded.
    pub count: u32,
    /// Cycles in the encodings.
    pub cycles: u64,
    /// Data bytes copied (or reduced to their high bits).
    pub bytes: u64,
    /// Copies whose source and destination differ modulo 4 (the ROM `memcpy` copies those
    /// byte by byte).
    pub unlike4: u32,
    /// Copies whose source and destination differ modulo 16 (PIE then shifts every word).
    pub unlike16: u32,
}

impl Copies {
    /// Counts gained since `earlier`.
    pub fn since(&self, earlier: &Copies) -> Copies {
        Copies {
            count: self.count.wrapping_sub(earlier.count),
            cycles: self.cycles - earlier.cycles,
            bytes: self.bytes - earlier.bytes,
            unlike4: self.unlike4.wrapping_sub(earlier.unlike4),
            unlike16: self.unlike16.wrapping_sub(earlier.unlike16),
        }
    }
}

static COPY_COUNT: AtomicU32 = AtomicU32::new(0);
static COPY_CYCLES: Counter64 = Counter64::new();
static COPY_BYTES: Counter64 = Counter64::new();
static COPY_UNLIKE4: AtomicU32 = AtomicU32::new(0);
static COPY_UNLIKE16: AtomicU32 = AtomicU32::new(0);

/// The chunk encodings' counts now.
pub(super) fn copies() -> Copies {
    Copies {
        count: COPY_COUNT.load(Ordering::Relaxed),
        cycles: COPY_CYCLES.load(),
        bytes: COPY_BYTES.load(),
        unlike4: COPY_UNLIKE4.load(Ordering::Relaxed),
        unlike16: COPY_UNLIKE16.load(Ordering::Relaxed),
    }
}

/// Queues one datagram of at most [`MAX_DATAGRAM`] bytes to `to`, written by `fill` straight
/// into the socket's ring; waits for room.
///
/// The socket is shared with the control task (which only receives), so it is borrowed only
/// inside a poll: each poll builds embassy-net's `send_to_with` future afresh and polls it
/// once. A future that returns `Pending` has registered the send waker and is dropped; the
/// next poll builds a new one with a new closure (which also sidesteps smoltcp 0.13.1's
/// `send_to_with` panic, README item 7: the closure is never taken twice).
async fn send(
    sock: &RefCell<UdpSocket<'static>>,
    to: IpEndpoint,
    fill: impl Fn(&mut [u8]) -> usize,
) -> Result<(), SendError> {
    poll_fn(|cx| {
        let mut s = sock.borrow_mut();
        let fut = s.send_to_with(MAX_DATAGRAM, to, |buf| (fill(buf), ()));
        pin!(fut).poll(cx)
    })
    .await
}

async fn respond(sock: &RefCell<UdpSocket<'static>>, r: &Outgoing) {
    let sent = send(sock, r.to, |buf| {
        r.response.encode(buf).map_or(0, <[u8]>::len)
    })
    .await;
    if sent.is_ok() {
        shared(|s| s.counters.datagrams_sent = s.counters.datagrams_sent.wrapping_add(1));
    }
}

/// Sends `job`'s chunks in order; gives up when the stream stops (new mode or encoding, stream
/// off, no request for the timeout) or the socket refuses. The time it takes counts as the
/// link's busy time ([`SEND_BUSY_US`]).
async fn send_frame(sock: &RefCell<UdpSocket<'static>>, pie: &Pie, job: Job) {
    let start = Instant::now();
    send_chunks(sock, pie, &job).await;
    // No preload of the buffer may run once it goes back (`prefetch` module docs).
    crate::prefetch::settle();
    drop(job);
    let us = start.elapsed().as_micros() as u32;
    SEND_BUSY_US.fetch_add(us, Ordering::Relaxed);
}

async fn send_chunks(sock: &RefCell<UdpSocket<'static>>, pie: &Pie, job: &Job) {
    let len = job.len;
    let mut offset = 0;
    while offset < len {
        while let Ok(r) = RESPONSES.try_receive() {
            respond(sock, &r).await;
        }
        let now = Instant::now();
        let to = shared(|s| {
            let to = s.peer.filter(|_| s.epoch == job.epoch && s.active(now));
            if to.is_none() && s.epoch == job.epoch {
                s.sending = false;
            }
            to
        });
        let Some(to) = to else {
            return;
        };
        let n = (len - offset).min(CHUNK_MAX as u32) as usize;
        let body: &[u8] = if job.high_bits {
            &[]
        } else {
            &job.data[offset as usize..][..n]
        };
        let head = |buf: &mut [u8]| head(job, len, offset, n, buf);
        let sent = match chunk::send(pie, to, CAM_PORT, head, body).await {
            Ok(()) => Ok(()),
            Err(_) => send(sock, to, |buf| encode(pie, job, len, offset, n, buf)).await,
        };
        if sent.is_err() {
            shared(|s| {
                if s.epoch == job.epoch {
                    s.sending = false;
                }
            });
            return;
        }
        shared(|s| s.counters.datagrams_sent = s.counters.datagrams_sent.wrapping_add(1));
        offset += n as u32;
        if !job.high_bits {
            // The next chunk's bytes load while this task waits for room on the ring.
            let rest = &job.data[offset as usize..len as usize];
            crate::prefetch::ahead(&rest[..rest.len().min(CHUNK_MAX)]);
        }
    }
    shared(|s| {
        if s.epoch == job.epoch {
            s.sending = false;
            s.counters.frames_sent = s.counters.frames_sent.wrapping_add(1);
        }
    });
}

/// The chunk at `offset` of `job`, with `data` (only its length counts for the header).
fn chunk<'a>(job: &Job, frame_len: u32, offset: u32, data: &'a [u8]) -> FrameChunk<'a> {
    FrameChunk {
        frame: job.number,
        generation: job.generation,
        encoding: job.encoding,
        width: job.width,
        height: job.height,
        timestamp_us: job.timestamp_us,
        frame_len,
        offset,
        data,
    }
}

/// The head of the chunk at `offset` (`n` bytes) of `job` for `net::chunk::send`: the chunk's
/// header, and for a RAW10 frame sent as RAW8 its 8-bit pixels after it (the body is then
/// empty). Returns its length.
fn head(job: &Job, frame_len: u32, offset: u32, n: usize, buf: &mut [u8]) -> usize {
    // RAW10 as RAW8: `data` only gives the header the chunk's length (`n` bytes of the packed
    // frame, never read as such).
    let start = if job.high_bits { 0 } else { offset as usize };
    let Ok(h) = chunk(job, frame_len, offset, &job.data[start..][..n]).encode_header(buf) else {
        return 0;
    };
    if !job.high_bits {
        return h;
    }
    let c0 = cpu::mcycle();
    let packed = &job.data[offset as usize / 4 * 5..];
    let packed = &packed[..(n.div_ceil(4) + 1).saturating_mul(5).min(packed.len())];
    crate::prefetch::reading(packed, || {
        high_bits(&job.data, offset as usize, &mut buf[h..h + n]);
    });
    if cfg!(feature = "accounting") {
        count_copy(cpu::mcycle().wrapping_sub(c0), n, 0);
    }
    h + n
}

/// Encodes the chunk at `offset` (`n` bytes) of `job` into `buf` (the socket's ring); returns
/// its length. `espion_proto` writes the header, then PIE copies the bytes from the frame (any
/// alignment). A RAW10 frame sent as RAW8 has its 8-bit pixels written straight into the
/// datagram instead.
fn encode(pie: &Pie, job: &Job, frame_len: u32, offset: u32, n: usize, buf: &mut [u8]) -> usize {
    let c0 = cpu::mcycle();
    // RAW10 as RAW8: `data` only gives the header the chunk's length (`n` bytes of the packed
    // frame, never read as such).
    let start = if job.high_bits { 0 } else { offset as usize };
    let data = &job.data[start..][..n];
    let Ok(h) = chunk(job, frame_len, offset, data).encode_header(buf) else {
        return 0;
    };
    let dst = &mut buf[h..h + n];
    let unlike = data.as_ptr() as usize ^ dst.as_ptr() as usize;
    if job.high_bits {
        high_bits(&job.data, offset as usize, dst);
    } else {
        pie.copy(dst, data);
    }
    if cfg!(feature = "accounting") {
        count_copy(cpu::mcycle().wrapping_sub(c0), n, unlike);
    }
    h + n
}

/// Books one chunk encoding of `cycles` and `n` data bytes whose ends' addresses XOR to
/// `unlike`.
#[inline(always)]
fn count_copy(cycles: u32, n: usize, unlike: usize) {
    #[cfg(feature = "accounting")]
    {
        COPY_COUNT.fetch_add(1, Ordering::Relaxed);
        COPY_CYCLES.add(cycles);
        COPY_BYTES.add(n as u32);
        COPY_UNLIKE4.fetch_add(u32::from(!unlike.is_multiple_of(4)), Ordering::Relaxed);
        COPY_UNLIKE16.fetch_add(u32::from(!unlike.is_multiple_of(16)), Ordering::Relaxed);
    }
    #[cfg(not(feature = "accounting"))]
    let _ = (cycles, n, unlike);
}

/// Pixels `first..first + out.len()` of a CSI-2 packed RAW10 frame as their 8 high bits: the
/// first 4 bytes of every 5.
fn high_bits(packed: &[u8], first: usize, out: &mut [u8]) {
    let byte = |p: usize| packed[p / 4 * 5 + p % 4];
    let mut p = first;
    let mut i = 0;
    while !p.is_multiple_of(4) && i < out.len() {
        out[i] = byte(p);
        p += 1;
        i += 1;
    }
    let (groups, tail) = out[i..].as_chunks_mut::<4>();
    let src = &packed[p / 4 * 5..][..groups.len() * 5];
    for (g, s) in groups.iter_mut().zip(src.as_chunks::<5>().0) {
        *g = [s[0], s[1], s[2], s[3]];
    }
    p += 4 * groups.len();
    for o in tail.iter_mut() {
        *o = byte(p);
        p += 1;
    }
}
