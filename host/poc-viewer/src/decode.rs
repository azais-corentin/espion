//! The decoding stage between the network thread and the window: JPEG frames are decoded to
//! RAW8 on their own thread, raw frames pass through untouched.
//!
//! Only what can be shown is decoded. The stage holds at most one picture ready for the
//! window; while it waits there, nothing is decoded and the network thread keeps only the
//! newest complete frame (replacing older ones, [`Stats::frames_replaced`](crate::net::Stats)).
//! When the window takes the picture, the stage takes the newest frame and decodes it, so the
//! next picture is the newest frame that arrived, and frames replaced meanwhile are never
//! decoded. A frame that fails to decode is counted and dropped. The network thread never
//! waits for a decode: it only hands its newest frame over under its own lock.

use std::{
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use espion_proto::cam::Encoding;

use crate::{
    jpeg,
    net::{Client, Frame},
};

/// A frame ready to show: RAW8 or RAW10 as the display takes them.
#[derive(Debug)]
pub struct Picture {
    /// The frame; a decoded JPEG frame is RAW8 here, with the JPEG frame's number, generation,
    /// size, timestamp and arrival time.
    pub frame: Frame,
    /// For a decoded JPEG frame: the file's length, bytes.
    pub jpeg_len: Option<usize>,
    /// For a decoded JPEG frame: when its decode started and ended.
    pub decode: Option<(Instant, Instant)>,
}

/// What the decoding stage did. Counters only grow.
#[derive(Debug, Clone, Default)]
pub struct DecodeStats {
    /// JPEG frames decoded.
    pub decoded: u64,
    /// JPEG frames that did not decode (dropped).
    pub failures: u64,
    /// Why the last failure happened.
    pub last_failure: Option<String>,
    /// Raw frames passed through.
    pub passed: u64,
    /// Time spent decoding, all frames.
    pub decode_total: Duration,
    /// The last decode's time.
    pub decode_last: Duration,
    /// The longest decode.
    pub decode_max: Duration,
}

#[derive(Default)]
struct Inner {
    ready: Option<Picture>,
    stats: DecodeStats,
    pool: Vec<Vec<u8>>,
}

type Shared = Arc<(Mutex<Inner>, Condvar)>;

fn lock(shared: &Shared) -> MutexGuard<'_, Inner> {
    shared.0.lock().unwrap_or_else(|e| e.into_inner())
}

/// The decoding stage of one [`Client`], on its own thread; stops when dropped.
pub struct Decoder {
    shared: Shared,
    client: Arc<Client>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// How long the stage blocks before it looks at its stop flag.
const WAIT: Duration = Duration::from_millis(50);

impl Decoder {
    /// Starts the stage. `wake` is called whenever a picture is ready (the UI's
    /// `request_repaint`).
    pub fn spawn(client: Arc<Client>, wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let shared: Shared = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (tshared, tclient, tstop) = (shared.clone(), client.clone(), stop.clone());
        let thread = thread::Builder::new()
            .name("poc-viewer-decode".into())
            .spawn(move || run(&tshared, &tclient, &tstop, &wake))?;
        Ok(Self {
            shared,
            client,
            stop,
            thread: Some(thread),
        })
    }

    /// Takes the picture ready to show, if any; the stage then decodes the newest frame.
    pub fn take(&self) -> Option<Picture> {
        let p = lock(&self.shared).ready.take();
        if p.is_some() {
            self.shared.1.notify_all();
        }
        p
    }

    /// Gives a shown picture's buffer back: a decoded picture's to the stage, a raw frame's
    /// to the network thread.
    pub fn recycle(&self, p: Picture) {
        if p.jpeg_len.is_some() {
            let mut inner = lock(&self.shared);
            if inner.pool.len() < 4 {
                inner.pool.push(p.frame.data);
            }
        } else {
            self.client.recycle(p.frame.data);
        }
    }

    /// A snapshot of the statistics.
    pub fn stats(&self) -> DecodeStats {
        lock(&self.shared).stats.clone()
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.shared.1.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(shared: &Shared, client: &Client, stop: &AtomicBool, wake: &dyn Fn()) {
    while !stop.load(Ordering::Relaxed) {
        // Nothing is decoded while a picture waits for the window.
        {
            let inner = lock(shared);
            if inner.ready.is_some() {
                drop(
                    shared
                        .1
                        .wait_timeout(inner, WAIT)
                        .unwrap_or_else(|e| e.into_inner()),
                );
                continue;
            }
        }
        let Some(f) = client.wait_frame(WAIT) else {
            continue;
        };
        let picture = if f.encoding == Encoding::Jpeg {
            let mut pixels = lock(shared).pool.pop().unwrap_or_default();
            let start = Instant::now();
            let r = jpeg::decode(&f.data, f.width, f.height, &mut pixels);
            let end = Instant::now();
            let dt = end - start;
            let jpeg_len = f.data.len();
            let Frame {
                number,
                generation,
                width,
                height,
                timestamp_us,
                data,
                first_chunk,
                completed,
                ..
            } = f;
            client.recycle(data);
            let mut inner = lock(shared);
            let s = &mut inner.stats;
            s.decode_total += dt;
            s.decode_last = dt;
            s.decode_max = s.decode_max.max(dt);
            match r {
                Ok(()) => {
                    s.decoded += 1;
                    Picture {
                        frame: Frame {
                            number,
                            generation,
                            encoding: Encoding::Raw8,
                            width,
                            height,
                            timestamp_us,
                            data: pixels,
                            first_chunk,
                            completed,
                        },
                        jpeg_len: Some(jpeg_len),
                        decode: Some((start, end)),
                    }
                }
                Err(e) => {
                    s.failures += 1;
                    s.last_failure = Some(format!("frame {number}: {e}"));
                    if inner.pool.len() < 4 {
                        inner.pool.push(pixels);
                    }
                    continue;
                }
            }
        } else {
            lock(shared).stats.passed += 1;
            Picture {
                frame: f,
                jpeg_len: None,
                decode: None,
            }
        };
        lock(shared).ready = Some(picture);
        wake();
    }
}
