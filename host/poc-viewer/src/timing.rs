//! Where a frame's time goes, from the start of its exposure to its hand-off to the GPU.
//!
//! The steps of a frame's way come from three places:
//!
//! - the sensor's exposure and readout are computed from the applied mode ([`Sensor`]): they
//!   end at the frame's capture timestamp;
//! - the board's encode and send come from the state ([`BoardFrame`]: the board's times of one
//!   frame, joined by the network thread with that frame's arrival here), read at the poll
//!   rate, so they are a sample of the frames;
//! - this PC's decode and draw come from every frame presented ([`HostFrame`]).
//!
//! The board and this PC have different clocks. Each [`BoardFrame`] bounds their offset: a
//! chunk arrives here no earlier than the board sent it, so the smallest "arrival minus board
//! time" of the window is taken as the offset. The send step then runs from the board taking
//! the frame to its last chunk arriving here. What stays unknown is the smallest transit of
//! one datagram (wire time, the adapter): the PC's steps sit that much too early.
//!
//! [`Timing::update`] makes a [`Snapshot`] once per [`INTERVAL`] from the samples of the last
//! [`WINDOW`]: per step the median length and the slowest sample, chained in order (a step
//! starts after the one before it plus the median wait between them). The window draws
//! snapshots only, so the diagram changes once a second however fast frames come.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use espion_proto::cam::State;

use crate::net::BoardFrame;

/// Samples older than this are forgotten.
pub const WINDOW: Duration = Duration::from_secs(2);
/// A new snapshot is made this often.
pub const INTERVAL: Duration = Duration::from_secs(1);

/// A step of a frame's way, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// The sensor's exposure.
    Exposure,
    /// The sensor's readout into the board's frame buffer.
    Readout,
    /// The board's JPEG encode (JPEG streams only).
    Encode,
    /// From the board taking the frame for sending to its last chunk arriving here.
    Send,
    /// The JPEG decode here (JPEG streams only).
    Decode,
    /// The window's pass that took the picture, up to the end of its texture upload.
    Draw,
}

impl Stage {
    /// Every stage, in order.
    pub const ALL: [Self; 6] = [
        Self::Exposure,
        Self::Readout,
        Self::Encode,
        Self::Send,
        Self::Decode,
        Self::Draw,
    ];

    /// The stage's name in the window.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Exposure => "Exposure",
            Self::Readout => "Readout",
            Self::Encode => "Encode",
            Self::Send => "Send",
            Self::Decode => "Decode",
            Self::Draw => "Draw",
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
            period_us: 1e9 / state.mode.frame_rate_mhz.max(1) as f32,
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

/// The typical frame of the last [`WINDOW`].
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Increases with every snapshot.
    pub serial: u64,
    /// The stages measured or computed, in order. The time between a step's end and the next
    /// one's start is a wait (for the encoder, the link, the decoder, the window's pass).
    pub steps: Vec<Step>,
    /// End of the last step, µs after the start of the exposure.
    pub total_us: f32,
    /// The steps reach the hand-off to the GPU (the board's and this PC's samples were there).
    pub complete: bool,
    /// Frame period, µs.
    pub period_us: f32,
    /// Length of a time axis that shows three consecutive frames, µs ([`axis_us`]).
    pub axis_us: f32,
    /// Frames the board timed in the window.
    pub board_frames: u32,
    /// Frames presented in the window.
    pub host_frames: u32,
}

impl Snapshot {
    /// The step of `stage`, if the snapshot has it.
    pub fn step(&self, stage: Stage) -> Option<&Step> {
        self.steps.iter().find(|s| s.stage == stage)
    }
}

/// Axis lengths, µs: 1, 2, 5 steps from 5 ms.
const AXES_US: [f32; 12] = [5e3, 1e4, 2e4, 5e4, 1e5, 2e5, 5e5, 1e6, 2e6, 5e6, 1e7, 2e7];

/// The length of a time axis for `needed_us` of content when the axis is `current_us` long
/// (0: none yet). It grows to the smallest step that holds the content as soon as the content
/// no longer fits, and shrinks only to a step that holds it with a tenth to spare, so content
/// near a step's end does not make the axis flip.
pub fn axis_us(current_us: f32, needed_us: f32) -> f32 {
    let fit = |needed: f32| {
        AXES_US
            .iter()
            .copied()
            .find(|&a| a >= needed)
            .unwrap_or(AXES_US[AXES_US.len() - 1])
    };
    if current_us <= 0.0 || needed_us > current_us {
        fit(needed_us)
    } else {
        fit(needed_us * 1.1).min(current_us)
    }
}

/// The samples of the last [`WINDOW`] and the latest [`Snapshot`].
#[derive(Debug)]
pub struct Timing {
    /// Instants are counted from here, µs.
    epoch: Instant,
    board: VecDeque<(Instant, BoardFrame)>,
    host: VecDeque<(Instant, HostFrame)>,
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
            due: now,
            snapshot: None,
            serial: 0,
        }
    }

    /// Forgets every sample and the snapshot (the frame layout changed).
    pub fn reset(&mut self, now: Instant) {
        self.board.clear();
        self.host.clear();
        self.snapshot = None;
        self.due = now;
    }

    /// A frame the board timed. The state repeats it until the board sent another, so a frame
    /// already known is ignored.
    pub fn board(&mut self, now: Instant, f: BoardFrame) {
        if self
            .board
            .back()
            .is_none_or(|(_, b)| b.timing.frame != f.timing.frame)
        {
            self.board.push_back((now, f));
        }
    }

    /// A frame this PC presented.
    pub fn host(&mut self, now: Instant, f: HostFrame) {
        self.host.push_back((now, f));
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
        while self.board.front().is_some_and(|&(at, _)| !fresh(at)) {
            self.board.pop_front();
        }
        while self.host.front().is_some_and(|&(at, _)| !fresh(at)) {
            self.host.pop_front();
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
                measured: false,
            },
            Step {
                stage: Stage::Readout,
                start_us: sensor.exposure_us,
                len_us: sensor.readout_us,
                slowest_us: sensor.readout_us,
                measured: false,
            },
        ];
        let board = self.board_steps(&mut steps);
        let host = board && self.host_steps(&mut steps);
        let total_us = steps.last().map_or(0.0, Step::end_us);
        let axis = self.snapshot.as_ref().map_or(0.0, |s| s.axis_us);
        self.snapshot = Some(Snapshot {
            serial: self.serial,
            steps,
            total_us,
            complete: host,
            period_us: sensor.period_us,
            axis_us: axis_us(axis, total_us + 2.0 * sensor.period_us),
            board_frames: self.board.len() as u32,
            host_frames: self.host.len() as u32,
        });
        true
    }

    /// µs from the epoch to `t`.
    fn us(&self, t: Instant) -> f64 {
        t.saturating_duration_since(self.epoch).as_secs_f64() * 1e6
    }

    /// Appends the board's steps (encode, send) from the board's frames of the window that are
    /// of the newest one's kind (encoded or not); returns whether there were any.
    fn board_steps(&self, steps: &mut Vec<Step>) -> bool {
        let Some((_, newest)) = self.board.back() else {
            return false;
        };
        let encoded = newest.timing.encode.is_some();
        let frames = || {
            self.board
                .iter()
                .map(|(_, f)| f)
                .filter(move |f| f.timing.encode.is_some() == encoded)
        };
        // Board time of a frame's capture, and the smallest "arrival here minus board time":
        // the clock offset, as near as the fastest datagram of the window shows it.
        let board_us = |f: &BoardFrame, after_us: u32| f.timestamp_us as f64 + f64::from(after_us);
        let offset = frames()
            .map(|f| {
                let first = self.us(f.first_chunk) - board_us(f, f.timing.send.start_us);
                let last = self.us(f.completed) - board_us(f, f.timing.send.end_us);
                first.min(last)
            })
            .fold(f64::INFINITY, f64::min);
        let mut end = steps.last().map_or(0.0, Step::end_us);
        if encoded {
            let (gap, len): (Vec<f32>, Vec<f32>) = frames()
                .filter_map(|f| f.timing.encode)
                .map(|e| {
                    (
                        e.start_us as f32,
                        e.end_us.saturating_sub(e.start_us) as f32,
                    )
                })
                .unzip();
            end = push(steps, Stage::Encode, end, gap, len);
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
        push(steps, Stage::Send, end, gap, len);
        true
    }

    /// Appends this PC's steps (decode, draw) from the frames presented in the window that are
    /// of the newest one's kind (decoded or not); returns whether there were any.
    fn host_steps(&self, steps: &mut Vec<Step>) -> bool {
        let Some((_, newest)) = self.host.back() else {
            return false;
        };
        let decoded = newest.decode.is_some();
        let frames = || {
            self.host
                .iter()
                .map(|(_, f)| f)
                .filter(move |f| f.decode.is_some() == decoded)
        };
        let us =
            |from: Instant, to: Instant| to.saturating_duration_since(from).as_secs_f32() * 1e6;
        let mut end = steps.last().map_or(0.0, Step::end_us);
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
        push(steps, Stage::Draw, end, gap, len);
        true
    }
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
    use espion_proto::cam::{FrameTiming, Span};

    use super::*;

    const SENSOR: Sensor = Sensor {
        exposure_us: 1_800.0,
        readout_us: 7_300.0,
        period_us: 8_333.0,
    };

    /// The board's clock reads this much less than the epoch's µs.
    const CLOCK_US: u64 = 5_000_000;

    fn at(epoch: Instant, us: u64) -> Instant {
        epoch + Duration::from_micros(us)
    }

    /// A JPEG frame captured at board time `ts`: encoded from 100 to 100 + `encode` µs after
    /// it, sent from 50 µs later for `send` µs, its first chunk arriving `transit` µs after the
    /// send's start and its last `transit` µs after the send's end.
    fn jpeg(
        epoch: Instant,
        frame: u32,
        ts: u64,
        encode: u32,
        send: u32,
        transit: u64,
    ) -> BoardFrame {
        let start = 100 + encode + 50;
        let host = |after: u32| at(epoch, CLOCK_US + ts + u64::from(after) + transit);
        BoardFrame {
            timing: FrameTiming {
                frame,
                encode: Some(Span {
                    start_us: 100,
                    end_us: 100 + encode,
                }),
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

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 2.0
    }

    #[test]
    fn the_steps_chain_from_the_exposure_to_the_draw() {
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
        assert_eq!(
            s.steps.iter().map(|s| s.stage).collect::<Vec<_>>(),
            Stage::ALL
        );
        assert!(!step(Stage::Exposure).measured && !step(Stage::Readout).measured);
        // The capture is the end of the readout, 9100 µs after the start of the exposure.
        let encode = step(Stage::Encode);
        assert!(near(encode.start_us, 9_200.0), "{encode:?}");
        assert!(
            near(encode.len_us, 6_200.0) && near(encode.slowest_us, 6_400.0),
            "{encode:?}"
        );
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
        assert!(s.complete && near(s.total_us, draw.end_us()));
        assert_eq!((s.board_frames, s.host_frames), (3, 3));
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
            s.steps.iter().map(|s| s.stage).collect::<Vec<_>>(),
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
        // With no sample left, the sensor's steps remain and the snapshot says it is partial.
        let much_later = later + WINDOW + INTERVAL;
        assert!(t.update(much_later, Some(SENSOR)));
        let s = t.snapshot().unwrap();
        assert_eq!(s.steps.len(), 2);
        assert!(!s.complete);
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
        assert!(!s.complete);
    }

    #[test]
    fn the_axis_grows_at_once_and_shrinks_with_a_margin() {
        assert_eq!(axis_us(0.0, 41_500.0), 50_000.0);
        // Grows as soon as the content no longer fits.
        assert_eq!(axis_us(50_000.0, 50_001.0), 100_000.0);
        // 48 ms would fit 50 ms again, but not with a tenth to spare: the axis stays.
        assert_eq!(axis_us(100_000.0, 48_000.0), 100_000.0);
        assert_eq!(axis_us(100_000.0, 45_000.0), 50_000.0);
        // Never below the smallest step, never above the largest.
        assert_eq!(axis_us(0.0, 10.0), 5_000.0);
        assert_eq!(axis_us(0.0, 1e9), 2e7);
    }
}
