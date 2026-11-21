//! Where a frame's time goes, from the start of its exposure to its markers arriving here and
//! its picture's hand-off to the GPU.
//!
//! After the sensor a frame goes two ways, the two [`Lane`]s:
//!
//! - **image**: the board's encode and send, this PC's decode and draw;
//! - **markers**: the board's detection on the frame's strips (while the sensor still reads
//!   out the rest of the frame), the frame's measure, the blobs' send, and the window's pass
//!   that first drew them.
//!
//! The steps come from four places:
//!
//! - the sensor's exposure and readout are computed from the applied mode ([`Sensor`]): they
//!   end at the frame's capture timestamp;
//! - the image's board steps come from the state ([`BoardFrame`]: the board's times of one
//!   frame, joined by the network thread with that frame's arrival here), read at the poll
//!   rate, so they are a sample of the frames;
//! - the markers' board steps come from every blob frame ([`BlobFrame`], its
//!   [`DetectTiming`]);
//! - this PC's steps come from every frame presented ([`HostFrame`]) and every blob frame
//!   drawn ([`OverlayFrame`]).
//!
//! The board and this PC have different clocks. Each board sample bounds their offset: a
//! datagram arrives here no earlier than the board sent it (a frame's chunk) or queued it (a
//! blob frame), so the smallest "arrival minus board time" over both kinds of sample is taken
//! as the offset. The image's send then runs from the board taking the frame to its last chunk
//! arriving here, the markers' from the board queuing the blobs to their arrival. What stays
//! unknown is the smallest transit of one datagram (wire time, the adapter): the PC's steps
//! sit that much too early.
//!
//! [`Timing::update`] makes a [`Snapshot`] once per [`INTERVAL`] from the samples of the last
//! [`WINDOW`]: per step the median length and the slowest sample, chained in order within its
//! lane (a step starts after the one before it plus the median wait between them). The window
//! draws snapshots only, so the diagram changes once a second however fast frames come.
//!
//! The samples are of one configuration: the board's generation (its mode and encoding) and
//! which ways are on ([`Lanes`]: frames sent, detection). [`Timing::configure`] forgets them
//! when that changes, and a sample that arrived before the change is never taken, so a lane
//! shows the new configuration only, or nothing while it has no sample of it. The board's
//! state names the last frame it timed until it times another, which it never does with
//! frames off: each frame is taken once, however long the state repeats it.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use espion_proto::cam::{DetectTiming, State};

use crate::net::{BlobFrame, BoardFrame};

/// Samples older than this are forgotten.
pub const WINDOW: Duration = Duration::from_secs(2);
/// A new snapshot is made this often.
pub const INTERVAL: Duration = Duration::from_secs(1);

/// The way a stage belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    /// The sensor's exposure and readout, before the frame's two ways part.
    Sensor,
    /// The picture: encode, send, decode, draw.
    Image,
    /// The markers: detect, measure, send, draw.
    Markers,
}

impl Lane {
    /// The lane's name in the window.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sensor => "Sensor",
            Self::Image => "Image",
            Self::Markers => "Markers",
        }
    }
}

/// A step of a frame's way, in order within its lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// The sensor's exposure.
    Exposure,
    /// The sensor's readout into the board.
    Readout,
    /// The board's JPEG encode (JPEG streams only).
    Encode,
    /// From the board taking the frame for sending to its last chunk arriving here.
    Send,
    /// The JPEG decode here (JPEG streams only).
    Decode,
    /// The window's pass that took the picture, up to the end of its texture upload.
    Draw,
    /// The board's detector on the frame's strips: from its start on the first strip (during
    /// the readout) to the release of the last one.
    Detect,
    /// The board closing the frame and measuring its blobs on the assembled frame, up to the
    /// hand-off of the result to its network side.
    Measure,
    /// From the board queuing the blobs for sending to their arrival here.
    MarkerSend,
    /// The window's pass that first drew the blobs, up to the renderer's hand-off.
    MarkerDraw,
}

impl Stage {
    /// Every stage: the sensor's, the image's, the markers'.
    pub const ALL: [Self; 10] = [
        Self::Exposure,
        Self::Readout,
        Self::Encode,
        Self::Send,
        Self::Decode,
        Self::Draw,
        Self::Detect,
        Self::Measure,
        Self::MarkerSend,
        Self::MarkerDraw,
    ];

    /// The stage's name within its lane.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Exposure => "Exposure",
            Self::Readout => "Readout",
            Self::Encode => "Encode",
            Self::Send | Self::MarkerSend => "Send",
            Self::Decode => "Decode",
            Self::Draw | Self::MarkerDraw => "Draw",
            Self::Detect => "Detect",
            Self::Measure => "Measure",
        }
    }

    /// The stage's name with its lane's, where two lanes share a name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Send => "Image: Send",
            Self::Draw => "Image: Draw",
            Self::MarkerSend => "Markers: Send",
            Self::MarkerDraw => "Markers: Draw",
            s => s.name(),
        }
    }

    /// The lane the stage belongs to.
    pub const fn lane(self) -> Lane {
        match self {
            Self::Exposure | Self::Readout => Lane::Sensor,
            Self::Encode | Self::Send | Self::Decode | Self::Draw => Lane::Image,
            Self::Detect | Self::Measure | Self::MarkerSend | Self::MarkerDraw => Lane::Markers,
        }
    }
}

/// One stage of the typical frame of a [`Snapshot`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    /// Which stage.
    pub stage: Stage,
    /// Its start, µs after the start of the exposure.
    pub start_us: f32,
    /// Its length, µs: the median of the window.
    pub len_us: f32,
    /// The longest sample of the window, µs.
    pub slowest_us: f32,
    /// The median part at its end that came after its input was all there, µs (the encode's
    /// tail after the frame's last rows; 0 for the other stages).
    pub tail_us: f32,
    /// Measured (the board's and this PC's stages) or computed from the sensor mode.
    pub measured: bool,
}

impl Step {
    /// Its end, µs after the start of the exposure.
    pub fn end_us(&self) -> f32 {
        self.start_us + self.len_us
    }
}

/// The sensor's part of a frame, computed from the applied mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sensor {
    /// Exposure time, µs.
    pub exposure_us: f32,
    /// Readout time of the window's lines, µs.
    pub readout_us: f32,
    /// Line period, µs.
    pub line_us: f32,
    /// Frame period, µs.
    pub period_us: f32,
}

impl Sensor {
    /// The figures of the board's applied mode: the exposure and the lines read out, each a
    /// number of line periods; the exposure is placed right before the readout.
    pub fn of(state: &State) -> Self {
        let line_us = state.mode.line_ns as f32 / 1e3;
        Self {
            exposure_us: f32::from(state.camera.exposure_lines) * line_us,
            readout_us: f32::from(state.camera.height) * line_us,
            line_us,
            period_us: 1e9 / state.mode.frame_rate_mhz.max(1) as f32,
        }
    }
}

/// The board's marker detection, as the markers lane needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detection {
    /// On and running.
    On,
    /// Turned off.
    Off,
    /// Not possible in the running mode (whole frames only).
    Unavailable,
    /// Stopped by a fault.
    Fault,
}

/// Which ways a frame takes after the sensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lanes {
    /// The board sends frames (the image lane).
    pub frames: bool,
    /// The board's detection (the markers lane).
    pub detection: Detection,
}

impl Lanes {
    /// Both ways on.
    pub const ALL: Self = Self {
        frames: true,
        detection: Detection::On,
    };

    /// The ways of the board's state.
    pub fn of(state: &State) -> Self {
        let d = state.detect;
        Self {
            frames: state.stream.enabled,
            detection: if d.fault {
                Detection::Fault
            } else if !d.available {
                Detection::Unavailable
            } else if !d.settings.enabled {
                Detection::Off
            } else {
                Detection::On
            },
        }
    }
}

/// This PC's times of one frame it presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostFrame {
    /// When its last chunk arrived.
    pub completed: Instant,
    /// Start and end of its JPEG decode; `None` for a raw frame.
    pub decode: Option<(Instant, Instant)>,
    /// Start of the window's pass that took the picture.
    pub taken: Instant,
    /// End of its texture upload.
    pub drawn: Instant,
}

/// This PC's times of one blob frame it drew: the window's first pass that drew it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayFrame {
    /// When the blob frame arrived.
    pub arrived: Instant,
    /// Start of the pass.
    pub taken: Instant,
    /// The renderer's hand-off of that pass (its paint callbacks prepared).
    pub drawn: Instant,
}

/// The board's times of one blob frame, with its arrival here.
#[derive(Debug, Clone, Copy)]
struct BlobSample {
    timestamp_us: u64,
    timing: DetectTiming,
    arrived: Instant,
}

/// Core 1's detection of the window's frames, strip by strip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Core1 {
    /// Strips of the newest frame, and rows of a full strip.
    pub strips: u16,
    pub strip_lines: u16,
    /// Sensor time of a full strip, µs.
    pub strip_us: f32,
    /// The longest of the window: from a strip's block end to the detector's start on it,
    /// µs.
    pub start_max_us: f32,
    /// The longest detector time on one strip, µs.
    pub detect_max_us: f32,
    /// The longest wait for a strip's copy after the detector, µs.
    pub copy_wait_max_us: f32,
    /// The longest hold of a strip, from its block end to its release, µs.
    pub hold_max_us: f32,
    /// The hold a strip is allowed, µs (the newest frame's).
    pub hold_budget_us: f32,
    /// Median processor time of the frame's end (cache invalidate and measure), µs.
    pub measure_us: f32,
    /// Strips held longer than one strip period, in the window's frames.
    pub late_strips: u32,
    /// Frames whose detection was cut short, as the board's states in the window count them.
    pub frames_cut: u32,
}

/// The typical frame of the last [`WINDOW`].
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Increases with every snapshot.
    pub serial: u64,
    /// The stages measured or computed: the sensor's, then the image's, then the markers'
    /// (a lane without samples has none). Within a lane, the time between a step's end and
    /// the next one's start is a wait (for the encoder, the link, the decoder, the board's
    /// network side, the window's pass).
    pub steps: Vec<Step>,
    /// End of the image lane's last step, µs after the start of the exposure; `None` without
    /// the board's image samples (frames off).
    pub image_us: Option<f32>,
    /// The image lane reaches the hand-off to the GPU (this PC's samples were there too).
    pub image_complete: bool,
    /// The marker latency: the end of the markers' send (the blobs here), µs after the start
    /// of the exposure; `None` without blob frames (detection off).
    pub markers_us: Option<f32>,
    /// End of the markers' draw, µs after the start of the exposure; `None` when no blob frame
    /// was drawn in the window.
    pub markers_drawn_us: Option<f32>,
    /// End of the last step of any lane, µs after the start of the exposure.
    pub total_us: f32,
    /// Frame period, µs.
    pub period_us: f32,
    /// The ways on: a lane that is off has no steps, whatever samples came.
    pub lanes: Lanes,
    /// How long the samples have been collected: since the configuration changed, up to
    /// [`WINDOW`]. Shorter, the snapshot is of fewer frames than a full window.
    pub collected: Duration,
    /// Core 1's strips, when the window has blob frames.
    pub core1: Option<Core1>,
    /// Frames the board timed in the window (image).
    pub board_frames: u32,
    /// Frames presented in the window.
    pub host_frames: u32,
    /// Blob frames received in the window.
    pub blob_frames: u32,
    /// Blob frames drawn in the window.
    pub overlay_frames: u32,
}

impl Snapshot {
    /// The step of `stage`, if the snapshot has it.
    pub fn step(&self, stage: Stage) -> Option<&Step> {
        self.steps.iter().find(|s| s.stage == stage)
    }

    /// The steps of `lane`, in order.
    pub fn lane(&self, lane: Lane) -> impl Iterator<Item = &Step> {
        self.steps.iter().filter(move |s| s.stage.lane() == lane)
    }
}

/// A time axis for content that changes once a second: a round length (1, 2 or 5 times a power
/// of ten per tick, at most [`Axis::TICKS`] ticks) with a twentieth to spare. It grows as soon
/// as the content no longer fits, and shrinks only once [`Axis::SHRINK_AFTER`] snapshots in a
/// row would fit in four fifths of it, so content near a length's end does not make it flip.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Axis {
    len_us: f32,
    smaller: u8,
}

impl Axis {
    /// Most ticks along an axis.
    pub const TICKS: f32 = 10.0;
    /// Snapshots in a row that fit a shorter axis before it shrinks.
    pub const SHRINK_AFTER: u8 = 3;

    /// Takes the content of a new snapshot, `needed_us` long.
    pub fn fit(&mut self, needed_us: f32) {
        let fit = Self::round(needed_us);
        if self.len_us <= 0.0 || fit > self.len_us {
            self.len_us = fit;
            self.smaller = 0;
        } else if fit < self.len_us * 0.8 {
            self.smaller += 1;
            if self.smaller >= Self::SHRINK_AFTER {
                self.len_us = fit;
                self.smaller = 0;
            }
        } else {
            self.smaller = 0;
        }
    }

    /// The length, µs; 0 before any content.
    pub const fn len_us(&self) -> f32 {
        self.len_us
    }

    /// The space between ticks on an axis `len_us` long: 1, 2 or 5 times a power of ten, the
    /// smallest with at most [`Axis::TICKS`] ticks.
    pub fn tick_us(len_us: f32) -> f32 {
        let raw = (len_us / Self::TICKS).max(1.0);
        let power = 10f32.powf(raw.log10().floor());
        [1.0, 2.0, 5.0, 10.0]
            .into_iter()
            .map(|m| m * power)
            .find(|&t| t >= raw * 0.999)
            .unwrap_or(10.0 * power)
    }

    /// The round length that holds `needed_us` with a twentieth to spare, 1 ms at least.
    fn round(needed_us: f32) -> f32 {
        let n = needed_us.max(1_000.0) * 1.05;
        let tick = Self::tick_us(n);
        (n / tick).ceil() * tick
    }
}

/// The samples of the last [`WINDOW`] and the latest [`Snapshot`].
#[derive(Debug)]
pub struct Timing {
    /// Instants are counted from here, µs.
    epoch: Instant,
    board: VecDeque<(Instant, BoardFrame)>,
    host: VecDeque<(Instant, HostFrame)>,
    blobs: VecDeque<(Instant, BlobSample)>,
    overlay: VecDeque<(Instant, OverlayFrame)>,
    /// The board's count of cut frames each time it changed; the oldest is the window's
    /// baseline.
    cuts: VecDeque<(Instant, u32)>,
    /// When the samples began: a sample that arrived earlier is of another configuration.
    since: Instant,
    /// The configuration of the samples: the board's generation and the ways on; `None` until
    /// the board's state is known (then every sample is taken).
    config: Option<(u32, Lanes)>,
    /// The board's capture time of the last frame it timed that was taken.
    last_board: Option<u64>,
    /// When the next snapshot is due.
    due: Instant,
    snapshot: Option<Snapshot>,
    serial: u64,
}

impl Timing {
    /// An empty model; the first snapshot is due at once.
    pub fn new(now: Instant) -> Self {
        Self {
            epoch: now,
            board: VecDeque::new(),
            host: VecDeque::new(),
            blobs: VecDeque::new(),
            overlay: VecDeque::new(),
            cuts: VecDeque::new(),
            since: now,
            config: None,
            last_board: None,
            due: now,
            snapshot: None,
            serial: 0,
        }
    }

    /// The board's configuration: its generation (mode and encoding) and the ways on. When it
    /// changes, every sample and the snapshot are forgotten and only samples that arrive from
    /// now on are taken.
    pub fn configure(&mut self, now: Instant, generation: u32, lanes: Lanes) {
        if self.config == Some((generation, lanes)) {
            return;
        }
        self.config = Some((generation, lanes));
        self.board.clear();
        self.host.clear();
        self.blobs.clear();
        self.overlay.clear();
        self.cuts.clear();
        self.since = now;
        self.snapshot = None;
        self.due = now;
    }

    /// A frame the board timed. The state repeats it until the board timed another (forever,
    /// with frames off), so a frame already taken, or one that arrived before the
    /// configuration changed, is ignored.
    pub fn board(&mut self, now: Instant, f: BoardFrame) {
        if f.completed < self.since || self.last_board == Some(f.timestamp_us) {
            return;
        }
        self.last_board = Some(f.timestamp_us);
        self.board.push_back((now, f));
    }

    /// A frame this PC presented.
    pub fn host(&mut self, now: Instant, f: HostFrame) {
        if f.completed >= self.since {
            self.host.push_back((now, f));
        }
    }

    /// A blob frame received; one of another generation, or that arrived before the
    /// configuration changed, is ignored.
    pub fn blobs(&mut self, now: Instant, f: &BlobFrame) {
        if f.arrived < self.since || self.config.is_some_and(|(g, _)| g != f.generation) {
            return;
        }
        self.blobs.push_back((
            now,
            BlobSample {
                timestamp_us: f.timestamp_us,
                timing: f.timing,
                arrived: f.arrived,
            },
        ));
    }

    /// A blob frame this PC drew.
    pub fn overlay(&mut self, now: Instant, f: OverlayFrame) {
        if f.arrived >= self.since {
            self.overlay.push_back((now, f));
        }
    }

    /// The board's count of cut frames ([`espion_proto::cam::DetectState::frames_cut`]).
    pub fn frames_cut(&mut self, now: Instant, count: u32) {
        if self.cuts.back().is_none_or(|&(_, c)| c != count) {
            self.cuts.push_back((now, count));
        }
    }

    /// The latest snapshot.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    /// Makes a new snapshot when one is due; returns whether it did. `sensor`: the applied
    /// mode's figures, `None` while the board's state is unknown (then there is no snapshot).
    pub fn update(&mut self, now: Instant, sensor: Option<Sensor>) -> bool {
        if now < self.due {
            return false;
        }
        self.due = now + INTERVAL;
        let fresh = |at: Instant| now.saturating_duration_since(at) <= WINDOW;
        fn prune<T>(q: &mut VecDeque<(Instant, T)>, fresh: impl Fn(Instant) -> bool) {
            while q.front().is_some_and(|(at, _)| !fresh(*at)) {
                q.pop_front();
            }
        }
        prune(&mut self.board, fresh);
        prune(&mut self.host, fresh);
        prune(&mut self.blobs, fresh);
        prune(&mut self.overlay, fresh);
        // The newest count from before the window stays as its baseline.
        while self.cuts.get(1).is_some_and(|&(at, _)| !fresh(at)) {
            self.cuts.pop_front();
        }
        let Some(sensor) = sensor else {
            self.snapshot = None;
            return true;
        };
        self.serial += 1;
        let mut steps = vec![
            Step {
                stage: Stage::Exposure,
                start_us: 0.0,
                len_us: sensor.exposure_us,
                slowest_us: sensor.exposure_us,
                tail_us: 0.0,
                measured: false,
            },
            Step {
                stage: Stage::Readout,
                start_us: sensor.exposure_us,
                len_us: sensor.readout_us,
                slowest_us: sensor.readout_us,
                tail_us: 0.0,
                measured: false,
            },
        ];
        let capture = sensor.exposure_us + sensor.readout_us;
        let lanes = self.config.map_or(Lanes::ALL, |(_, l)| l);
        let markers_on = lanes.detection == Detection::On;
        let offset = self.offset();
        let image = if lanes.frames {
            self.image_steps(&mut steps, capture, offset)
        } else {
            None
        };
        let (markers_us, markers_drawn_us) = if markers_on {
            self.marker_steps(&mut steps, capture, offset)
        } else {
            (None, None)
        };
        let total_us = steps.iter().map(Step::end_us).fold(0.0, f32::max);
        self.snapshot = Some(Snapshot {
            serial: self.serial,
            steps,
            image_us: image.map(|(end, _)| end),
            image_complete: image.is_some_and(|(_, complete)| complete),
            markers_us,
            markers_drawn_us,
            total_us,
            period_us: sensor.period_us,
            lanes,
            collected: now.saturating_duration_since(self.since).min(WINDOW),
            core1: self.core1(&sensor).filter(|_| markers_on),
            board_frames: self.board.len() as u32,
            host_frames: self.host.len() as u32,
            blob_frames: self.blobs.len() as u32,
            overlay_frames: self.overlay.len() as u32,
        });
        true
    }

    /// µs from the epoch to `t`.
    fn us(&self, t: Instant) -> f64 {
        t.saturating_duration_since(self.epoch).as_secs_f64() * 1e6
    }

    /// The clock offset, µs (this PC's µs from the epoch minus the board's µs since its boot):
    /// the smallest "arrival here minus board time" of the window's datagrams, frame chunks
    /// and blob frames alike. `None` without a board sample.
    fn offset(&self) -> Option<f64> {
        let board = |ts: u64, after: u32| ts as f64 + f64::from(after);
        let frames = self.board.iter().map(|(_, f)| {
            let first = self.us(f.first_chunk) - board(f.timestamp_us, f.timing.send.start_us);
            let last = self.us(f.completed) - board(f.timestamp_us, f.timing.send.end_us);
            first.min(last)
        });
        let blobs = self
            .blobs
            .iter()
            .map(|(_, b)| self.us(b.arrived) - board(b.timestamp_us, b.timing.queued_us));
        frames.chain(blobs).reduce(f64::min)
    }

    /// Appends the image's steps from the capture (`capture`, µs after the start of the
    /// exposure): the board's (encode, send), then, if there were any, this PC's (decode,
    /// draw). Returns the lane's end and whether it reaches the GPU; `None` without the
    /// board's samples.
    fn image_steps(
        &self,
        steps: &mut Vec<Step>,
        capture: f32,
        offset: Option<f64>,
    ) -> Option<(f32, bool)> {
        let offset = offset?;
        let end = self.board_steps(steps, capture, offset)?;
        Some(match self.host_steps(steps, end) {
            Some(end) => (end, true),
            None => (end, false),
        })
    }

    /// Appends the board's image steps (encode, send) from the board's frames of the window
    /// that are of the newest one's kind (encoded or not); returns their end, `None` if there
    /// were none.
    fn board_steps(&self, steps: &mut Vec<Step>, capture: f32, offset: f64) -> Option<f32> {
        let (_, newest) = self.board.back()?;
        let encoded = newest.timing.encode.is_some();
        let frames = || {
            self.board
                .iter()
                .map(|(_, f)| f)
                .filter(move |f| f.timing.encode.is_some() == encoded)
        };
        let board_us = |f: &BoardFrame, after_us: u32| f.timestamp_us as f64 + f64::from(after_us);
        let mut end = capture;
        if encoded {
            // The encode may start before the capture (fed during the readout): a negative
            // gap, as the markers' detect has.
            let spans: Vec<_> = frames().filter_map(|f| f.timing.encode).collect();
            let (gap, len): (Vec<f32>, Vec<f32>) = spans
                .iter()
                .map(|e| {
                    (
                        e.start_us as f32,
                        (i64::from(e.end_us) - i64::from(e.start_us)).max(0) as f32,
                    )
                })
                .unzip();
            let mut tail: Vec<f32> = spans
                .iter()
                .map(|e| e.end_us.saturating_sub(e.input_end_us) as f32)
                .collect();
            end = push(steps, Stage::Encode, end, gap, len);
            if let Some(st) = steps.last_mut() {
                st.tail_us = median(&mut tail).min(st.len_us);
            }
        }
        let (gap, len): (Vec<f32>, Vec<f32>) = frames()
            .map(|f| {
                let t = f.timing;
                let before = t.encode.map_or(0, |e| e.end_us);
                // To the last chunk's arrival here, on the board's clock; never shorter than
                // the board's own send.
                let arrived = self.us(f.completed) - offset - board_us(f, t.send.start_us);
                let sent = t.send.end_us.saturating_sub(t.send.start_us) as f32;
                (
                    t.send.start_us.saturating_sub(before) as f32,
                    (arrived as f32).max(sent),
                )
            })
            .unzip();
        Some(push(steps, Stage::Send, end, gap, len))
    }

    /// Appends this PC's image steps (decode, draw) after `end`, from the frames presented in
    /// the window that are of the newest one's kind (decoded or not); returns their end,
    /// `None` if there were none.
    fn host_steps(&self, steps: &mut Vec<Step>, mut end: f32) -> Option<f32> {
        let (_, newest) = self.host.back()?;
        let decoded = newest.decode.is_some();
        let frames = || {
            self.host
                .iter()
                .map(|(_, f)| f)
                .filter(move |f| f.decode.is_some() == decoded)
        };
        if decoded {
            let (gap, len): (Vec<f32>, Vec<f32>) = frames()
                .filter_map(|f| f.decode.map(|(s, e)| (us(f.completed, s), us(s, e))))
                .unzip();
            end = push(steps, Stage::Decode, end, gap, len);
        }
        let (gap, len): (Vec<f32>, Vec<f32>) = frames()
            .map(|f| {
                let ready = f.decode.map_or(f.completed, |(_, e)| e);
                (us(ready, f.taken), us(f.taken, f.drawn))
            })
            .unzip();
        Some(push(steps, Stage::Draw, end, gap, len))
    }

    /// Appends the markers' steps: detect (from before the capture at `capture`), measure,
    /// send, and draw if a blob frame was drawn. Returns the end of the send and of the draw.
    fn marker_steps(
        &self,
        steps: &mut Vec<Step>,
        capture: f32,
        offset: Option<f64>,
    ) -> (Option<f32>, Option<f32>) {
        let Some(offset) = offset.filter(|_| !self.blobs.is_empty()) else {
            return (None, None);
        };
        let blobs = || self.blobs.iter().map(|(_, b)| b);
        let (gap, len): (Vec<f32>, Vec<f32>) = blobs()
            .map(|b| {
                let t = b.timing;
                (
                    -(t.strips_lead_us as f32),
                    t.strips_lead_us.saturating_add(t.strips_end_us) as f32,
                )
            })
            .unzip();
        let end = push(steps, Stage::Detect, capture, gap, len);
        let (gap, len): (Vec<f32>, Vec<f32>) = blobs()
            .map(|b| {
                let t = b.timing;
                (0.0, t.published_us.saturating_sub(t.strips_end_us) as f32)
            })
            .unzip();
        let end = push(steps, Stage::Measure, end, gap, len);
        let (gap, len): (Vec<f32>, Vec<f32>) = blobs()
            .map(|b| {
                let t = b.timing;
                let queued = b.timestamp_us as f64 + f64::from(t.queued_us);
                let arrived = self.us(b.arrived) - offset - queued;
                (
                    t.queued_us.saturating_sub(t.published_us) as f32,
                    arrived.max(0.0) as f32,
                )
            })
            .unzip();
        let sent = push(steps, Stage::MarkerSend, end, gap, len);
        if self.overlay.is_empty() {
            return (Some(sent), None);
        }
        let (gap, len): (Vec<f32>, Vec<f32>) = self
            .overlay
            .iter()
            .map(|(_, o)| (us(o.arrived, o.taken), us(o.taken, o.drawn)))
            .unzip();
        let drawn = push(steps, Stage::MarkerDraw, sent, gap, len);
        (Some(sent), Some(drawn))
    }

    /// Core 1's figures over the window's blob frames.
    fn core1(&self, sensor: &Sensor) -> Option<Core1> {
        let (_, newest) = self.blobs.back()?;
        let t = newest.timing;
        let longest = |f: fn(&DetectTiming) -> u16| {
            self.blobs
                .iter()
                .map(|(_, b)| f(&b.timing))
                .max()
                .map_or(0.0, f32::from)
        };
        let mut measure: Vec<f32> = self
            .blobs
            .iter()
            .map(|(_, b)| f32::from(b.timing.measure_us))
            .collect();
        let cut = match (self.cuts.front(), self.cuts.back()) {
            (Some(&(_, first)), Some(&(_, last))) => last.wrapping_sub(first),
            _ => 0,
        };
        Some(Core1 {
            strips: t.strips,
            strip_lines: t.strip_lines,
            strip_us: f32::from(t.strip_lines) * sensor.line_us,
            start_max_us: longest(|d| d.start_max_us),
            detect_max_us: longest(|d| d.detect_max_us),
            copy_wait_max_us: longest(|d| d.copy_wait_max_us),
            hold_max_us: longest(|d| d.hold_max_us),
            hold_budget_us: f32::from(t.hold_budget_us),
            measure_us: median(&mut measure),
            late_strips: self
                .blobs
                .iter()
                .map(|(_, b)| u32::from(b.timing.late_strips))
                .sum(),
            frames_cut: cut,
        })
    }
}

/// µs from `from` to `to`, 0 if `to` is earlier.
fn us(from: Instant, to: Instant) -> f32 {
    to.saturating_duration_since(from).as_secs_f32() * 1e6
}

/// Appends the measured step of `stage` after `end`: it starts the median of `gap` later and
/// lasts the median of `len`. Returns its end.
fn push(
    steps: &mut Vec<Step>,
    stage: Stage,
    end: f32,
    mut gap: Vec<f32>,
    mut len: Vec<f32>,
) -> f32 {
    let step = Step {
        stage,
        start_us: end + median(&mut gap),
        len_us: median(&mut len),
        slowest_us: len.iter().copied().fold(0.0, f32::max),
        tail_us: 0.0,
        measured: true,
    };
    steps.push(step);
    step.end_us()
}

/// The median of `v` (the upper of the two middle values of an even count); 0 when empty.
fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mid = v.len() / 2;
    *v.select_nth_unstable_by(mid, f32::total_cmp).1
}

#[cfg(test)]
mod tests {
    use espion_proto::cam::{EncodeSpan, Format, FrameTiming, Span};

    use super::*;

    const SENSOR: Sensor = Sensor {
        exposure_us: 1_800.0,
        readout_us: 7_300.0,
        line_us: 9.125,
        period_us: 8_333.0,
    };

    /// The board's clock reads this much less than the epoch's µs.
    const CLOCK_US: u64 = 5_000_000;

    fn at(epoch: Instant, us: u64) -> Instant {
        epoch + Duration::from_micros(us)
    }

    /// A JPEG frame captured at board time `ts`: encoded whole from 100 to 100 + `encode` µs
    /// after it, then as [`jpeg_span`].
    fn jpeg(
        epoch: Instant,
        frame: u32,
        ts: u64,
        encode: u32,
        send: u32,
        transit: u64,
    ) -> BoardFrame {
        let whole = EncodeSpan {
            start_us: 100,
            input_end_us: 100,
            end_us: 100 + encode,
        };
        jpeg_span(epoch, frame, ts, whole, send, transit)
    }

    /// A JPEG frame captured at board time `ts`, encoded over `encode`, sent from 50 µs after
    /// the encode's end for `send` µs, its first chunk arriving `transit` µs after the send's
    /// start and its last `transit` µs after the send's end.
    fn jpeg_span(
        epoch: Instant,
        frame: u32,
        ts: u64,
        encode: EncodeSpan,
        send: u32,
        transit: u64,
    ) -> BoardFrame {
        let start = encode.end_us + 50;
        let host = |after: u32| at(epoch, CLOCK_US + ts + u64::from(after) + transit);
        BoardFrame {
            timing: FrameTiming {
                frame,
                encode: Some(encode),
                send: Span {
                    start_us: start,
                    end_us: start + send,
                },
            },
            timestamp_us: ts,
            first_chunk: host(start),
            completed: host(start + send),
        }
    }

    /// A frame whole at `completed`: decoded from 200 µs later for `decode` µs, taken `wait` µs
    /// after that, drawn in 300 µs.
    fn shown(epoch: Instant, completed: u64, decode: Option<u64>, wait: u64) -> HostFrame {
        let done = at(epoch, completed);
        let decode = decode.map(|d| {
            (
                done + Duration::from_micros(200),
                done + Duration::from_micros(200 + d),
            )
        });
        let taken = decode.map_or(done, |(_, e)| e) + Duration::from_micros(wait);
        HostFrame {
            completed: done,
            decode,
            taken,
            drawn: taken + Duration::from_micros(300),
        }
    }

    /// A blob frame captured at board time `ts`, detected from `lead` µs before it to 400 µs
    /// after it, published at 600 µs, queued at 1000 µs, arriving `transit` µs after.
    fn blob_frame(epoch: Instant, ts: u64, lead: u32, transit: u64) -> BlobFrame {
        BlobFrame {
            seq: ts,
            frame: 0,
            generation: 0,
            timestamp_us: ts,
            format: Format::Raw8,
            width: 1280,
            height: 800,
            threshold: 100,
            detect_us: 3_000,
            timing: DetectTiming {
                strips_lead_us: lead,
                strips_end_us: 400,
                published_us: 600,
                queued_us: 1_000,
                measure_us: 150,
                strips: 13,
                strip_lines: 64,
                late_strips: 1,
                start_max_us: 20,
                detect_max_us: 300,
                copy_wait_max_us: 5,
                hold_max_us: 380,
                hold_budget_us: 1_100,
            },
            flags: 0,
            blobs: Vec::new(),
            arrived: at(epoch, CLOCK_US + ts + 1_000 + transit),
        }
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 2.0
    }

    fn stages(s: &Snapshot) -> Vec<Stage> {
        s.steps.iter().map(|s| s.stage).collect()
    }

    #[test]
    fn the_image_steps_chain_from_the_exposure_to_the_draw() {
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        for (i, encode) in [6_000, 6_200, 6_400].into_iter().enumerate() {
            let ts = 1_000_000 + i as u64 * 250_000;
            t.board(epoch, jpeg(epoch, i as u32, ts, encode, 4_000, 0));
        }
        for (i, decode) in [700, 800, 900].into_iter().enumerate() {
            t.host(
                epoch,
                shown(epoch, 20_000 + i as u64 * 8_333, Some(decode), 4_000),
            );
        }
        assert!(t.update(epoch, Some(SENSOR)));
        let s = t.snapshot().unwrap();
        let step = |stage| *s.step(stage).unwrap();
        assert_eq!(stages(s), &Stage::ALL[..6]);
        assert!(!step(Stage::Exposure).measured && !step(Stage::Readout).measured);
        // The capture is the end of the readout, 9100 µs after the start of the exposure.
        let encode = step(Stage::Encode);
        assert!(near(encode.start_us, 9_200.0), "{encode:?}");
        assert!(
            near(encode.len_us, 6_200.0) && near(encode.slowest_us, 6_400.0),
            "{encode:?}"
        );
        // Whole frames: the encode is all tail, no mark.
        assert!(near(encode.tail_us, encode.len_us), "{encode:?}");
        let send = step(Stage::Send);
        assert!(near(send.start_us, encode.end_us() + 50.0), "{send:?}");
        assert!(near(send.len_us, 4_000.0), "{send:?}");
        let decode = step(Stage::Decode);
        assert!(near(decode.start_us, send.end_us() + 200.0), "{decode:?}");
        assert!(
            near(decode.len_us, 800.0) && near(decode.slowest_us, 900.0),
            "{decode:?}"
        );
        let draw = step(Stage::Draw);
        assert!(near(draw.start_us, decode.end_us() + 4_000.0), "{draw:?}");
        assert!(near(draw.len_us, 300.0), "{draw:?}");
        assert!(s.image_complete && near(s.image_us.unwrap(), draw.end_us()));
        assert!(near(s.total_us, draw.end_us()));
        assert_eq!((s.markers_us, s.core1), (None, None));
        assert_eq!((s.board_frames, s.host_frames), (3, 3));
    }

    #[test]
    fn an_encode_fed_during_the_readout_starts_before_the_capture() {
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        for (i, end) in [450, 500, 700].into_iter().enumerate() {
            let ts = 1_000_000 + i as u64 * 250_000;
            let span = EncodeSpan {
                start_us: -6_500,
                input_end_us: 300,
                end_us: end,
            };
            t.board(epoch, jpeg_span(epoch, i as u32, ts, span, 4_000, 0));
        }
        assert!(t.update(epoch, Some(SENSOR)));
        let s = t.snapshot().unwrap();
        let encode = *s.step(Stage::Encode).unwrap();
        // The capture is at 9100 µs: the encode starts 6500 µs before it, in the readout.
        assert!(near(encode.start_us, 2_600.0), "{encode:?}");
        assert!(
            near(encode.len_us, 7_000.0) && near(encode.slowest_us, 7_200.0),
            "{encode:?}"
        );
        assert!(near(encode.tail_us, 200.0), "{encode:?}");
        let send = *s.step(Stage::Send).unwrap();
        assert!(near(send.start_us, encode.end_us() + 50.0), "{send:?}");
    }

    #[test]
    fn the_fastest_datagram_sets_the_clock_offset() {
        // Datagrams take 0, 300 and 900 µs to arrive: the send to the last chunk's arrival is
        // the board's 4000 µs plus what each took more than the fastest.
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        for (i, transit) in [300, 0, 900].into_iter().enumerate() {
            let ts = 1_000_000 + i as u64 * 250_000;
            t.board(epoch, jpeg(epoch, i as u32, ts, 6_000, 4_000, transit));
        }
        t.update(epoch, Some(SENSOR));
        let send = *t.snapshot().unwrap().step(Stage::Send).unwrap();
        assert!(near(send.len_us, 4_300.0), "{send:?}");
        assert!(near(send.slowest_us, 4_900.0), "{send:?}");
    }

    #[test]
    fn a_faster_blob_datagram_sets_the_offset_for_the_image_too() {
        // Every frame chunk took 500 µs, a blob frame none: the image's send is 500 µs longer
        // than the board's.
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        t.board(epoch, jpeg(epoch, 1, 1_000_000, 6_000, 4_000, 500));
        t.blobs(epoch, &blob_frame(epoch, 1_000_000, 6_000, 0));
        t.update(epoch, Some(SENSOR));
        let s = t.snapshot().unwrap();
        let send = *s.step(Stage::Send).unwrap();
        assert!(near(send.len_us, 4_500.0), "{send:?}");
        assert!(near(s.step(Stage::MarkerSend).unwrap().len_us, 0.0));
    }

    #[test]
    fn a_raw_stream_has_no_encode_and_no_decode() {
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        let mut f = jpeg(epoch, 1, 1_000_000, 0, 90_000, 0);
        f.timing.encode = None;
        t.board(epoch, f);
        t.host(epoch, shown(epoch, 120_000, None, 2_000));
        t.update(epoch, Some(SENSOR));
        let s = t.snapshot().unwrap();
        assert_eq!(
            stages(s),
            [Stage::Exposure, Stage::Readout, Stage::Send, Stage::Draw]
        );
        // Sent 150 µs after the capture; taken 2000 µs after it was whole.
        let send = *s.step(Stage::Send).unwrap();
        assert!(
            near(send.start_us, 9_250.0) && near(send.len_us, 90_000.0),
            "{send:?}"
        );
        let draw = *s.step(Stage::Draw).unwrap();
        assert!(near(draw.start_us, send.end_us() + 2_000.0), "{draw:?}");
    }

    #[test]
    fn with_frames_off_the_markers_chain_from_the_strips_on_their_own_clock() {
        // Blob frames only: detection starts during the readout; the fastest of the window
        // sets the clock offset, so the send is what each took more than it.
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        for (i, (lead, transit)) in [(6_000, 200), (6_200, 0), (6_400, 700)]
            .into_iter()
            .enumerate()
        {
            t.blobs(
                epoch,
                &blob_frame(epoch, 1_000_000 + i as u64 * 8_333, lead, transit),
            );
        }
        t.frames_cut(epoch, 7);
        assert!(t.update(epoch, Some(SENSOR)));
        let s = t.snapshot().unwrap();
        assert_eq!(
            stages(s),
            [
                Stage::Exposure,
                Stage::Readout,
                Stage::Detect,
                Stage::Measure,
                Stage::MarkerSend
            ]
        );
        assert_eq!((s.image_us, s.image_complete), (None, false));
        let step = |stage| *s.step(stage).unwrap();
        // The capture is at 9100 µs: detection from 6200 µs before it to 400 µs after.
        let detect = step(Stage::Detect);
        assert!(near(detect.start_us, 2_900.0), "{detect:?}");
        assert!(near(detect.end_us(), 9_500.0), "{detect:?}");
        assert!(near(detect.slowest_us, 6_800.0), "{detect:?}");
        let measure = step(Stage::Measure);
        assert!(
            near(measure.start_us, 9_500.0) && near(measure.len_us, 200.0),
            "{measure:?}"
        );
        // Queued 400 µs after the publication; the send is the transit above the fastest.
        let send = step(Stage::MarkerSend);
        assert!(near(send.start_us, 10_100.0), "{send:?}");
        assert!(
            near(send.len_us, 200.0) && near(send.slowest_us, 700.0),
            "{send:?}"
        );
        assert!(near(s.markers_us.unwrap(), send.end_us()));
        assert_eq!(s.markers_drawn_us, None);
        let c = s.core1.unwrap();
        assert_eq!((c.strips, c.strip_lines, c.late_strips), (13, 64, 3));
        assert!(near(c.strip_us, 584.0) && near(c.hold_budget_us, 1_100.0));
        // One count of cut frames is the window's baseline: none cut in it.
        assert_eq!(c.frames_cut, 0);
        assert_eq!(s.blob_frames, 3);
    }

    #[test]
    fn the_markers_end_with_their_first_draw() {
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        let b = blob_frame(epoch, 1_000_000, 6_000, 0);
        t.blobs(epoch, &b);
        let taken = b.arrived + Duration::from_micros(3_000);
        t.overlay(
            epoch,
            OverlayFrame {
                arrived: b.arrived,
                taken,
                drawn: taken + Duration::from_micros(250),
            },
        );
        t.update(epoch, Some(SENSOR));
        let s = t.snapshot().unwrap();
        let send = *s.step(Stage::MarkerSend).unwrap();
        let draw = *s.step(Stage::MarkerDraw).unwrap();
        assert!(near(draw.start_us, send.end_us() + 3_000.0), "{draw:?}");
        assert!(near(draw.len_us, 250.0), "{draw:?}");
        assert!(near(s.markers_drawn_us.unwrap(), draw.end_us()));
        assert!(near(s.markers_us.unwrap(), send.end_us()));
    }

    #[test]
    fn the_cut_frames_are_those_counted_within_the_window() {
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        t.blobs(epoch, &blob_frame(epoch, 1_000_000, 6_000, 0));
        t.frames_cut(epoch, 10);
        t.frames_cut(epoch + Duration::from_millis(100), 10);
        let later = epoch + WINDOW + INTERVAL;
        t.frames_cut(later - Duration::from_millis(500), 13);
        t.blobs(later, &blob_frame(epoch, 1_100_000, 6_000, 0));
        t.update(later, Some(SENSOR));
        assert_eq!(t.snapshot().unwrap().core1.unwrap().frames_cut, 3);
    }

    #[test]
    fn a_snapshot_comes_once_per_interval_from_the_last_window() {
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        t.board(epoch, jpeg(epoch, 1, 1_000_000, 6_000, 4_000, 0));
        assert!(t.update(epoch, Some(SENSOR)));
        let first = t.snapshot().unwrap().clone();
        // Not due again within the interval, whatever arrives.
        let soon = epoch + INTERVAL / 2;
        t.board(soon, jpeg(epoch, 2, 1_500_000, 9_000, 4_000, 0));
        assert!(!t.update(soon, Some(SENSOR)));
        assert_eq!(t.snapshot(), Some(&first));
        // Once the first frame is older than the window, only the second counts.
        let later = epoch + WINDOW + INTERVAL / 4;
        assert!(t.update(later, Some(SENSOR)));
        let s = t.snapshot().unwrap();
        assert!(s.serial > first.serial);
        assert_eq!(s.board_frames, 1);
        assert!(near(s.step(Stage::Encode).unwrap().len_us, 9_000.0));
        // With no sample left, the sensor's steps remain and neither lane is there.
        let much_later = later + WINDOW + INTERVAL;
        assert!(t.update(much_later, Some(SENSOR)));
        let s = t.snapshot().unwrap();
        assert_eq!(s.steps.len(), 2);
        assert_eq!((s.image_us, s.markers_us), (None, None));
    }

    #[test]
    fn the_state_repeating_a_frame_counts_it_once() {
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        let f = jpeg(epoch, 7, 1_000_000, 6_000, 4_000, 0);
        t.board(epoch, f);
        t.board(epoch, f);
        t.update(epoch, Some(SENSOR));
        assert_eq!(t.snapshot().unwrap().board_frames, 1);
    }

    #[test]
    fn this_pc_s_steps_need_the_board_s() {
        // Without a frame the board timed there is no clock to place the PC's steps on.
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        t.host(epoch, shown(epoch, 20_000, Some(800), 4_000));
        t.update(epoch, Some(SENSOR));
        let s = t.snapshot().unwrap();
        assert_eq!(s.steps.len(), 2);
        assert!(!s.image_complete);
    }

    #[test]
    fn the_axis_grows_at_once_and_shrinks_after_a_few_snapshots() {
        let mut a = Axis::default();
        // A round length with a twentieth to spare, in ticks of 1, 2 or 5.
        a.fit(37_400.0);
        assert_eq!(a.len_us(), 40_000.0);
        assert_eq!(Axis::tick_us(a.len_us()), 5_000.0);
        // Grows as soon as the content no longer fits.
        a.fit(41_000.0);
        assert_eq!(a.len_us(), 45_000.0);
        // Content that would fit four fifths of it shrinks it only after three snapshots in a
        // row; one that does not starts the count again.
        a.fit(30_000.0);
        a.fit(30_000.0);
        a.fit(40_000.0);
        a.fit(30_000.0);
        a.fit(30_000.0);
        assert_eq!(a.len_us(), 45_000.0);
        a.fit(30_000.0);
        assert_eq!(a.len_us(), 35_000.0);
        // 38 ms would fit 40 ms, but that is not four fifths of 50: the axis stays.
        let mut b = Axis::default();
        b.fit(45_000.0);
        for _ in 0..5 {
            b.fit(38_000.0);
        }
        assert_eq!(b.len_us(), 50_000.0);
        // 1 ms at least.
        let mut c = Axis::default();
        c.fit(10.0);
        assert_eq!(c.len_us(), 1_200.0);
    }

    #[test]
    fn a_frame_the_state_still_names_after_the_window_is_not_taken_again() {
        // With frames off the board's state names its last timed frame for ever: once it left
        // the window the image lane stays empty instead of coming back every other second.
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        let f = jpeg(epoch, 7, 1_000_000, 6_000, 4_000, 0);
        t.board(epoch, f);
        assert!(t.update(epoch, Some(SENSOR)));
        assert!(t.snapshot().unwrap().image_us.is_some());
        let mut now = epoch;
        for _ in 0..6 {
            now += INTERVAL;
            t.board(now, f);
            t.update(now, Some(SENSOR));
        }
        let s = t.snapshot().unwrap();
        assert_eq!((s.board_frames, s.image_us), (0, None));
    }

    #[test]
    fn samples_from_before_a_configuration_change_are_not_taken() {
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        t.configure(epoch, 1, Lanes::ALL);
        let old = jpeg(epoch, 1, 1_000_000, 6_000, 4_000, 0);
        let old_blobs = BlobFrame {
            generation: 1,
            ..blob_frame(epoch, 1_000_000, 6_000, 0)
        };
        // The mode changes after both arrived; the state still names the old frame, and an
        // old blob frame comes late.
        let change = old.completed.max(old_blobs.arrived) + Duration::from_millis(1);
        t.configure(change, 2, Lanes::ALL);
        t.board(change, old);
        let late = BlobFrame {
            arrived: change + Duration::from_millis(1),
            ..old_blobs
        };
        t.blobs(change, &late);
        assert!(t.update(change, Some(SENSOR)));
        let s = t.snapshot().unwrap();
        assert_eq!((s.board_frames, s.blob_frames), (0, 0));
        assert_eq!(s.collected, Duration::ZERO);
        // A blob frame of the new generation is taken.
        let new = BlobFrame {
            generation: 2,
            ..late.clone()
        };
        t.blobs(change, &new);
        let later = change + INTERVAL;
        t.update(later, Some(SENSOR));
        let s = t.snapshot().unwrap();
        assert_eq!(s.blob_frames, 1);
        assert_eq!(s.collected, INTERVAL);
    }

    #[test]
    fn a_lane_that_is_off_has_no_steps() {
        let epoch = Instant::now();
        let mut t = Timing::new(epoch);
        let lanes = Lanes {
            frames: false,
            detection: Detection::Off,
        };
        t.configure(epoch, 1, lanes);
        let f = jpeg(epoch, 1, 1_000_000, 6_000, 4_000, 0);
        t.board(epoch, f);
        t.blobs(
            epoch,
            &BlobFrame {
                generation: 1,
                ..blob_frame(epoch, 1_000_000, 6_000, 0)
            },
        );
        t.update(f.completed, Some(SENSOR));
        let s = t.snapshot().unwrap();
        assert_eq!(stages(s), [Stage::Exposure, Stage::Readout]);
        assert_eq!((s.image_us, s.markers_us, s.core1), (None, None, None));
        assert_eq!(s.lanes, lanes);
    }
}
