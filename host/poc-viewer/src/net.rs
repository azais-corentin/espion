//! The viewer's network thread: requests with retry, polling, chunk reassembly, the newest
//! complete frame, statistics.
//!
//! One thread owns the socket (std, blocking with a short timeout). It sends one request at a
//! time and repeats it every [`RETRY_INTERVAL`] until the matching response arrives; requests
//! queued meanwhile replace earlier queued ones of the same kind (only the latest settings
//! matter). With nothing to send it polls every [`POLL_INTERVAL`], which keeps the board's
//! stream alive and fetches its counters.
//!
//! Chunks are copied into the frame they belong to; a frame's length comes from its first
//! chunk (fixed for the raw encodings, the file's length for JPEG). A frame is handed over
//! only when every chunk arrived; a frame missing a chunk is dropped when a chunk of a newer frame arrives (or
//! after [`ASSEMBLY_TIMEOUT`]) and counted, never repaired: as cut by the board when the newer
//! chunk has another generation (the board abandons the frame in flight at a mode or encoding
//! change), otherwise as incomplete (a chunk was lost). A chunk whose encoding, size or frame
//! length disagrees with the frame it belongs to drops that frame too (counted as incomplete
//! and in [`Stats::mismatched_chunks`]). Only the newest complete frame
//! waits for the consumer: one it has not taken yet is replaced (and counted). Frame buffers
//! come back through [`Client::recycle`], so a steady stream allocates nothing.

use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use espion_proto::cam::{
    CamError, CameraSettings, Encoding, FrameChunk, MAX_DATAGRAM, Message, Op, Reply, Request,
    Response, State, StreamSettings, chunk_count,
};
use socket2::{Domain, Protocol, Socket, Type};

use crate::addr;

/// Poll period when no other request is pending.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// A request is repeated this often until answered.
pub const RETRY_INTERVAL: Duration = Duration::from_millis(200);
/// No response for this long: the link is shown as disconnected.
pub const DISCONNECTED_AFTER: Duration = Duration::from_millis(1500);
/// A frame still missing chunks this long after its first is dropped.
pub const ASSEMBLY_TIMEOUT: Duration = Duration::from_secs(1);
/// Receive buffer asked of the kernel; it grants at most `net.core.rmem_max` (doubled for its
/// bookkeeping) without privileges.
pub const RCVBUF_REQUEST: usize = 8 << 20;

/// How long the network thread blocks in `recv` before it looks at its timers.
const RECV_TIMEOUT: Duration = Duration::from_millis(5);
/// Statistics are published at least this often.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(50);

/// What the UI asks the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Read the state.
    GetState,
    /// Apply camera settings.
    SetCamera(CameraSettings),
    /// Apply stream settings.
    SetStream(StreamSettings),
}

impl Command {
    fn op(self) -> Op {
        match self {
            Self::GetState => Op::GetState,
            Self::SetCamera(c) => Op::SetCamera(c),
            Self::SetStream(s) => Op::SetStream(s),
        }
    }

    fn same_kind(self, other: Self) -> bool {
        core::mem::discriminant(&self) == core::mem::discriminant(&other)
    }
}

/// A complete frame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// The board's frame number.
    pub number: u32,
    /// Configuration generation of the frame.
    pub generation: u32,
    /// Pixel encoding of `data`.
    pub encoding: Encoding,
    /// Width in pixels.
    pub width: u16,
    /// Height in lines.
    pub height: u16,
    /// The board's capture time, µs since its boot.
    pub timestamp_us: u64,
    /// The frame's bytes.
    pub data: Vec<u8>,
    /// When its last chunk arrived.
    pub completed: Instant,
}

/// A request the board refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    /// What was asked.
    pub op: Op,
    /// Why it was refused.
    pub error: CamError,
    /// When the answer came.
    pub at: Instant,
}

/// Counters and the board's latest answers. Counters only grow.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// Datagrams received from the camera.
    pub datagrams: u64,
    /// UDP payload bytes received from the camera.
    pub bytes: u64,
    /// Datagrams that did not decode (foreign, other version, malformed).
    pub undecodable: u64,
    /// Frame chunks received.
    pub chunks: u64,
    /// Chunks of a frame older than the one being assembled, or repeated.
    pub stale_chunks: u64,
    /// Chunks whose encoding, size or frame length disagreed with their frame's first chunk.
    pub mismatched_chunks: u64,
    /// Frames received whole.
    pub frames_whole: u64,
    /// Bytes of the frames received whole (their `frame_len`, a JPEG file's length).
    pub frame_bytes: u64,
    /// Bytes of the last frame received whole.
    pub last_frame_len: u32,
    /// Frames dropped because a chunk was missing.
    pub frames_incomplete: u64,
    /// Frames the board stopped sending part-way for a new frame layout (its next chunk has
    /// another generation: a mode or encoding change), dropped without counting as missing.
    pub frames_cut: u64,
    /// Whole frames replaced by a newer one before the consumer took them.
    pub frames_replaced: u64,
    /// Requests sent, repeats included.
    pub requests_sent: u64,
    /// Repeats of unanswered requests.
    pub retries: u64,
    /// Round trip of the last answered request (from its last transmission).
    pub rtt: Option<Duration>,
    /// Answered requests whose round trip is counted in [`Stats::rtt_sum`] and
    /// [`Stats::rtt_max`].
    pub rtt_count: u64,
    /// Sum of the round trips of the answered requests.
    pub rtt_sum: Duration,
    /// Longest round trip of an answered request.
    pub rtt_max: Duration,
    /// Answers to the UI's commands ([`Command`], not the polls).
    pub answers: u64,
    /// The latest answer to a command: what was asked and the reply.
    pub last_answer_to: Option<(Op, Reply)>,
    /// When the last response arrived.
    pub last_answer: Option<Instant>,
    /// The board's latest state.
    pub state: Option<State>,
    /// The latest refusal, until a request of the same kind succeeds.
    pub refusal: Option<Refusal>,
    /// A request is waiting for its answer.
    pub pending: bool,
}

impl Stats {
    /// A response arrived within [`DISCONNECTED_AFTER`].
    pub fn connected(&self, now: Instant) -> bool {
        self.last_answer
            .is_some_and(|t| now.saturating_duration_since(t) < DISCONNECTED_AFTER)
    }
}

#[derive(Default)]
struct Inner {
    stats: Stats,
    latest: Option<Frame>,
    pool: Vec<Vec<u8>>,
}

type Shared = Arc<(Mutex<Inner>, Condvar)>;

fn lock(shared: &Shared) -> MutexGuard<'_, Inner> {
    shared.0.lock().unwrap_or_else(|e| e.into_inner())
}

/// Connection to one camera, served by its own thread; stops when dropped.
pub struct Client {
    shared: Shared,
    tx: Sender<Command>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    camera: SocketAddr,
    local: SocketAddr,
    rcvbuf: usize,
}

impl Client {
    /// Opens a socket bound to the local address for `camera` ([`addr::local_for`]) and starts
    /// the network thread. `wake` is called whenever a frame or an answer arrives (the UI's
    /// `request_repaint`).
    pub fn connect(camera: SocketAddr, wake: impl Fn() + Send + 'static) -> io::Result<Self> {
        let local = addr::local_for(camera)?;
        let domain = if local.is_ipv6() {
            Domain::IPV6
        } else {
            Domain::IPV4
        };
        let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
        // Capped silently at net.core.rmem_max; what was granted is read back below.
        socket.set_recv_buffer_size(RCVBUF_REQUEST)?;
        socket.bind(&local.into())?;
        let rcvbuf = socket.recv_buffer_size()?;
        let sock: UdpSocket = socket.into();
        // Only the camera's datagrams reach this socket.
        sock.connect(camera)?;
        sock.set_read_timeout(Some(RECV_TIMEOUT))?;
        let local = sock.local_addr()?;

        let shared: Shared = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let worker = Worker {
            sock,
            rx,
            shared: shared.clone(),
            wake: Box::new(wake),
            stats: Stats::default(),
            queue: Vec::new(),
            inflight: None,
            seq: 1,
            last_sent: None,
            assembly: None,
            last_done: None,
            published: Instant::now(),
        };
        let tstop = stop.clone();
        let thread = thread::Builder::new()
            .name("poc-viewer-net".into())
            .spawn(move || worker.run(&tstop))?;
        Ok(Self {
            shared,
            tx,
            stop,
            thread: Some(thread),
            camera,
            local,
            rcvbuf,
        })
    }

    /// Queues a request.
    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }

    /// Takes the newest complete frame, if one arrived since the last call.
    pub fn take_frame(&self) -> Option<Frame> {
        lock(&self.shared).latest.take()
    }

    /// Gives a frame buffer back for reuse.
    pub fn recycle(&self, data: Vec<u8>) {
        let mut inner = lock(&self.shared);
        if inner.pool.len() < 4 {
            inner.pool.push(data);
        }
    }

    /// A snapshot of the statistics.
    pub fn stats(&self) -> Stats {
        lock(&self.shared).stats.clone()
    }

    /// Waits until `done` holds for the statistics (checked at every frame, answer and
    /// publication), at most `timeout`; returns them if it did.
    pub fn wait_until(&self, timeout: Duration, done: impl Fn(&Stats) -> bool) -> Option<Stats> {
        let end = Instant::now() + timeout;
        let mut inner = lock(&self.shared);
        loop {
            if done(&inner.stats) {
                return Some(inner.stats.clone());
            }
            let left = end.checked_duration_since(Instant::now())?;
            inner = self
                .shared
                .1
                .wait_timeout(inner, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// The camera's address.
    pub fn camera(&self) -> SocketAddr {
        self.camera
    }

    /// The local address bound.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// The receive buffer the kernel granted, bytes as it reports them (twice the payload
    /// budget: the kernel counts its bookkeeping).
    pub fn rcvbuf(&self) -> usize {
        self.rcvbuf
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Inflight {
    seq: u32,
    op: Op,
    last_sent: Instant,
}

struct Assembly {
    frame: u32,
    generation: u32,
    encoding: Encoding,
    width: u16,
    height: u16,
    timestamp_us: u64,
    frame_len: u32,
    data: Vec<u8>,
    have: Vec<u64>,
    got: u32,
    chunks: u32,
    started: Instant,
}

struct Worker {
    sock: UdpSocket,
    rx: Receiver<Command>,
    shared: Shared,
    wake: Box<dyn Fn() + Send>,
    stats: Stats,
    queue: Vec<Command>,
    inflight: Option<Inflight>,
    seq: u32,
    last_sent: Option<Instant>,
    assembly: Option<Assembly>,
    /// Frame number and generation of the last frame handed over (or dropped for a mismatched
    /// chunk).
    last_done: Option<(u32, u32)>,
    published: Instant,
}

/// `a` comes before `b` in wrapping frame numbers.
fn older(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

impl Worker {
    fn run(mut self, stop: &AtomicBool) {
        let mut buf = [0u8; MAX_DATAGRAM + 1];
        while !stop.load(Ordering::Relaxed) {
            while let Ok(c) = self.rx.try_recv() {
                // The latest of a kind replaces a queued one and goes last, so requests
                // keep the order they were last asked in.
                self.queue.retain(|q| !q.same_kind(c));
                self.queue.push(c);
            }
            let now = Instant::now();
            self.timers(now);
            // Errors are timeouts, and ICMP errors reported on the connected socket (nobody
            // listening): nothing to do but wait.
            if let Ok(n) = self.sock.recv(&mut buf) {
                self.datagram(&buf[..n]);
            }
            if self.published.elapsed() >= PUBLISH_INTERVAL {
                self.publish(None);
            }
        }
    }

    fn timers(&mut self, now: Instant) {
        match &mut self.inflight {
            Some(f) if now - f.last_sent >= RETRY_INTERVAL => {
                let (seq, op) = (f.seq, f.op);
                f.last_sent = now;
                self.stats.retries += 1;
                self.transmit(seq, op, now);
            }
            Some(_) => {}
            None => {
                let next = if self.queue.is_empty() {
                    let due = self.last_sent.is_none_or(|t| now - t >= POLL_INTERVAL);
                    due.then_some(Op::Poll)
                } else {
                    Some(self.queue.remove(0).op())
                };
                if let Some(op) = next {
                    let seq = self.seq;
                    self.seq = self.seq.wrapping_add(1);
                    self.inflight = Some(Inflight {
                        seq,
                        op,
                        last_sent: now,
                    });
                    self.stats.pending = true;
                    self.transmit(seq, op, now);
                }
            }
        }
        if self
            .assembly
            .as_ref()
            .is_some_and(|a| now - a.started > ASSEMBLY_TIMEOUT)
        {
            self.drop_assembly(false);
        }
    }

    fn transmit(&mut self, seq: u32, op: Op, now: Instant) {
        let mut buf = [0u8; MAX_DATAGRAM];
        let d = Request { seq, op }
            .encode(&mut buf)
            .expect("a request fits a datagram");
        // Errors (no route yet, nobody listening) are retried like a lost datagram.
        let _ = self.sock.send(d);
        self.stats.requests_sent += 1;
        self.last_sent = Some(now);
    }

    fn datagram(&mut self, d: &[u8]) {
        self.stats.datagrams += 1;
        self.stats.bytes += d.len() as u64;
        match Message::decode(d) {
            Ok(Message::Chunk(c)) => self.chunk(&c),
            Ok(Message::Response(r)) => self.response(r),
            Ok(Message::Request(_)) | Err(_) => self.stats.undecodable += 1,
        }
    }

    fn response(&mut self, r: Response) {
        let now = Instant::now();
        self.stats.last_answer = Some(now);
        let Some(f) = self.inflight.take_if(|f| f.seq == r.seq) else {
            // An answer to an earlier repeat.
            return;
        };
        let rtt = now - f.last_sent;
        self.stats.rtt = Some(rtt);
        self.stats.rtt_count += 1;
        self.stats.rtt_sum += rtt;
        self.stats.rtt_max = self.stats.rtt_max.max(rtt);
        if f.op != Op::Poll {
            self.stats.answers += 1;
            self.stats.last_answer_to = Some((f.op, r.reply));
        }
        self.stats.pending = false;
        match r.reply {
            Reply::State(s) => {
                self.stats.state = Some(s);
                let same =
                    |a: Op, b: Op| core::mem::discriminant(&a) == core::mem::discriminant(&b);
                if self.stats.refusal.is_some_and(|e| same(e.op, f.op)) {
                    self.stats.refusal = None;
                }
            }
            Reply::Error(error) => {
                self.stats.refusal = Some(Refusal {
                    op: f.op,
                    error,
                    at: now,
                });
            }
        }
        self.publish(None);
    }

    /// Drops the frame being assembled; `cut`: the board abandoned it for a new layout.
    fn drop_assembly(&mut self, cut: bool) {
        if let Some(a) = self.assembly.take() {
            if cut {
                self.stats.frames_cut += 1;
            } else {
                self.stats.frames_incomplete += 1;
            }
            let mut inner = lock(&self.shared);
            if inner.pool.len() < 4 {
                inner.pool.push(a.data);
            }
        }
    }

    fn chunk(&mut self, c: &FrameChunk<'_>) {
        self.stats.chunks += 1;
        let key = (c.frame, c.generation);
        if let Some(a) = &self.assembly
            && (a.frame, a.generation) != key
        {
            if older(c.frame, a.frame) {
                self.stats.stale_chunks += 1;
                return;
            }
            let cut = a.generation != c.generation;
            self.drop_assembly(cut);
        }
        if let Some(a) = &self.assembly
            && (a.encoding, a.width, a.height, a.frame_len)
                != (c.encoding, c.width, c.height, c.frame_len)
        {
            self.stats.mismatched_chunks += 1;
            self.drop_assembly(false);
            // The frame's other chunks are stale now.
            self.last_done = Some(key);
            return;
        }
        if self.assembly.is_none() {
            if self
                .last_done
                .is_some_and(|(f, g)| (f, g) == key || older(c.frame, f))
            {
                self.stats.stale_chunks += 1;
                return;
            }
            let mut data = lock(&self.shared).pool.pop().unwrap_or_default();
            data.resize(c.frame_len as usize, 0);
            let chunks = chunk_count(c.frame_len);
            self.assembly = Some(Assembly {
                frame: c.frame,
                generation: c.generation,
                encoding: c.encoding,
                width: c.width,
                height: c.height,
                timestamp_us: c.timestamp_us,
                frame_len: c.frame_len,
                data,
                have: vec![0; chunks.div_ceil(64) as usize],
                got: 0,
                chunks,
                started: Instant::now(),
            });
        }
        let a = self.assembly.as_mut().expect("assembly started above");
        let i = c.index() as usize;
        let bit = 1u64 << (i % 64);
        if a.have[i / 64] & bit != 0 {
            self.stats.stale_chunks += 1;
            return;
        }
        a.have[i / 64] |= bit;
        a.got += 1;
        a.data[c.offset as usize..][..c.data.len()].copy_from_slice(c.data);
        if a.got == a.chunks {
            let a = self.assembly.take().expect("checked above");
            self.last_done = Some(key);
            self.stats.frames_whole += 1;
            self.stats.frame_bytes += u64::from(a.frame_len);
            self.stats.last_frame_len = a.frame_len;
            self.publish(Some(Frame {
                number: a.frame,
                generation: a.generation,
                encoding: a.encoding,
                width: a.width,
                height: a.height,
                timestamp_us: a.timestamp_us,
                data: a.data,
                completed: Instant::now(),
            }));
        }
    }

    /// Publishes the statistics and, if given, a new newest frame; wakes the consumer.
    fn publish(&mut self, frame: Option<Frame>) {
        let mut inner = lock(&self.shared);
        if let Some(f) = frame
            && let Some(old) = inner.latest.replace(f)
        {
            self.stats.frames_replaced += 1;
            if inner.pool.len() < 4 {
                inner.pool.push(old.data);
            }
        }
        inner.stats = self.stats.clone();
        drop(inner);
        self.shared.1.notify_all();
        self.published = Instant::now();
        (self.wake)();
    }
}
