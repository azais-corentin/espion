//! The send task: the only sender on the camera socket. Answers go first; the frame in flight
//! goes out chunk by chunk on the protocol's grid, each chunk encoded straight into the
//! socket's ring (one copy from the PSRAM frame; smoltcp makes the second into the DMA buffer).

use core::cell::RefCell;
use core::future::{Future, poll_fn};
use core::pin::pin;

use embassy_futures::select::{Either, select};
use embassy_net::IpEndpoint;
use embassy_net::udp::{SendError, UdpSocket};
use embassy_time::Instant;
use espion_proto::cam::{CHUNK_MAX, FrameChunk, MAX_DATAGRAM};

use super::{JOB, Job, Outgoing, RESPONSES, shared};
use crate::cpu;

#[embassy_executor::task]
pub(super) async fn task(sock: &'static RefCell<UdpSocket<'static>>) {
    cpu::name_current("send");
    loop {
        match select(RESPONSES.receive(), JOB.wait()).await {
            Either::First(r) => respond(sock, &r).await,
            Either::Second(job) => send_frame(sock, job).await,
        }
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
/// off, no request for the timeout) or the socket refuses.
async fn send_frame(sock: &RefCell<UdpSocket<'static>>, job: Job) {
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
        let n = (len - offset).min(CHUNK_MAX as u32);
        if send(sock, to, |buf| encode(&job, len, offset, n as usize, buf))
            .await
            .is_err()
        {
            shared(|s| {
                if s.epoch == job.epoch {
                    s.sending = false;
                }
            });
            return;
        }
        shared(|s| s.counters.datagrams_sent = s.counters.datagrams_sent.wrapping_add(1));
        offset += n;
    }
    shared(|s| {
        if s.epoch == job.epoch {
            s.sending = false;
            s.counters.frames_sent = s.counters.frames_sent.wrapping_add(1);
        }
    });
}

/// Encodes the chunk at `offset` (`n` bytes) of `job` into `buf`; returns its length. The
/// encoder copies the bytes from the frame: the one CPU copy of the pixels. A RAW10 frame sent
/// as RAW8 is encoded with stand-in bytes, which its 8-bit pixels then overwrite in place.
fn encode(job: &Job, frame_len: u32, offset: u32, n: usize, buf: &mut [u8]) -> usize {
    let data = if job.high_bits {
        &ZEROS[..n]
    } else {
        &job.frame[offset as usize..][..n]
    };
    let Ok(total) = FrameChunk {
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
    .encode(buf)
    .map(<[u8]>::len) else {
        return 0;
    };
    if job.high_bits {
        high_bits(&job.frame, offset as usize, &mut buf[total - n..total]);
    }
    total
}

/// Stand-in data of a RAW8-from-RAW10 chunk (internal RAM, so its copy is cheap).
static ZEROS: [u8; CHUNK_MAX] = [0; CHUNK_MAX];

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
