//! The board's blobs as the window shows them: the blob frames of the last seconds, the frame
//! that goes with the picture shown, the single-marker statistics and the trail.
//!
//! Time here is the board's (`timestamp_us`, the frames' capture times), so the windows hold
//! the same frames whatever the network or the window did. A new generation (another mode or
//! encoding) or a board clock that goes back (a restart) starts over.

use std::collections::VecDeque;

use espion_proto::cam::SUBPIXEL;

use crate::net::BlobFrame;

/// Most blob frames held: a minute at the sensor's fastest rate.
pub const MAX_FRAMES: usize = 40_000;

/// A position or width of a blob ([`Blob::x`], [`Blob::y`], [`Blob::width`]) in pixels;
/// positions have pixel centres at integers.
pub fn px(q: u32) -> f64 {
    f64::from(q) / f64::from(SUBPIXEL)
}

/// The blob frames of the last seconds, oldest first.
#[derive(Debug, Default)]
pub struct Markers {
    frames: VecDeque<BlobFrame>,
}

/// What the statistics window says about the marker (frames with exactly one blob).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MarkerStats {
    /// Blob frames in the window: frames the board detected.
    pub frames: u32,
    /// Of them, frames with at least one blob.
    pub found: u32,
    /// Of them, frames with exactly one blob: the frames the figures below are of.
    pub single: u32,
    /// Mean centroid, pixels.
    pub mean_x: f64,
    pub mean_y: f64,
    /// Standard deviation of the centroid (population), pixels.
    pub sigma_x: f64,
    pub sigma_y: f64,
    /// Mean peak (8-bit), area (pixels), width (pixels) and flux (frame units).
    pub mean_peak: f64,
    pub mean_area: f64,
    pub mean_width: f64,
    pub mean_flux: f64,
}

impl MarkerStats {
    /// Frames with at least one blob over frames detected, 0..=1 (0 with no frame).
    pub fn detection_rate(&self) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            f64::from(self.found) / f64::from(self.frames)
        }
    }
}

/// One centroid of the trail.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrailPoint {
    /// Pixels.
    pub x: f64,
    pub y: f64,
    /// Its age over the trail's length: 0 newest, up to 1.
    pub age: f32,
}

impl Markers {
    /// Adds a blob frame (newer than those held). Another generation, or a capture time before
    /// the newest held, starts over.
    pub fn push(&mut self, f: BlobFrame) {
        if self
            .frames
            .back()
            .is_some_and(|b| b.generation != f.generation || f.timestamp_us < b.timestamp_us)
        {
            self.frames.clear();
        }
        if self.frames.len() == MAX_FRAMES {
            self.frames.pop_front();
        }
        self.frames.push_back(f);
    }

    /// Drops the frames captured more than `keep_us` before the newest.
    pub fn prune(&mut self, keep_us: u64) {
        let Some(newest) = self.frames.back().map(|b| b.timestamp_us) else {
            return;
        };
        while self
            .frames
            .front()
            .is_some_and(|b| newest - b.timestamp_us > keep_us)
        {
            self.frames.pop_front();
        }
    }

    /// The arrival number of the newest frame held (0: none).
    pub fn seq(&self) -> u64 {
        self.frames.back().map_or(0, |b| b.seq)
    }

    /// The newest blob frame.
    pub fn newest(&self) -> Option<&BlobFrame> {
        self.frames.back()
    }

    /// The blob frame of frame `frame` of `generation` (the picture shown), if held.
    pub fn of_frame(&self, generation: u32, frame: u32) -> Option<&BlobFrame> {
        self.frames
            .iter()
            .rev()
            .find(|b| (b.generation, b.frame) == (generation, frame))
    }

    /// The frames captured within `span_us` of the newest, oldest first.
    fn window(&self, span_us: u64) -> impl Iterator<Item = &BlobFrame> {
        let newest = self.frames.back().map_or(0, |b| b.timestamp_us);
        self.frames
            .iter()
            .filter(move |b| newest - b.timestamp_us <= span_us)
    }

    /// The statistics of the frames captured within `span_us` of the newest.
    pub fn stats(&self, span_us: u64) -> MarkerStats {
        let mut s = MarkerStats::default();
        // Welford's running mean and sum of squared deviations: no cancellation at sub-pixel
        // spreads of positions a thousand pixels from the origin.
        let (mut mx, mut my, mut m2x, mut m2y) = (0.0, 0.0, 0.0, 0.0);
        let (mut speak, mut sarea, mut swidth, mut sflux) = (0.0, 0.0, 0.0, 0.0);
        for b in self.window(span_us) {
            s.frames += 1;
            if b.blobs.is_empty() {
                continue;
            }
            s.found += 1;
            let [one] = b.blobs[..] else {
                continue;
            };
            s.single += 1;
            let (x, y) = (px(one.x), px(one.y));
            let n = f64::from(s.single);
            let (dx, dy) = (x - mx, y - my);
            (mx, my) = (mx + dx / n, my + dy / n);
            (m2x, m2y) = (m2x + dx * (x - mx), m2y + dy * (y - my));
            speak += f64::from(one.peak);
            sarea += f64::from(one.area);
            swidth += px(u32::from(one.width));
            sflux += f64::from(one.flux);
        }
        if s.single > 0 {
            let n = f64::from(s.single);
            (s.mean_x, s.mean_y) = (mx, my);
            s.sigma_x = (m2x / n).sqrt();
            s.sigma_y = (m2y / n).sqrt();
            (s.mean_peak, s.mean_area) = (speak / n, sarea / n);
            (s.mean_width, s.mean_flux) = (swidth / n, sflux / n);
        }
        s
    }

    /// Every centroid of the frames captured within `span_us` of the newest, oldest first,
    /// with its age.
    pub fn trail(&self, span_us: u64) -> impl Iterator<Item = TrailPoint> {
        let newest = self.frames.back().map_or(0, |b| b.timestamp_us);
        let span = span_us.max(1) as f64;
        self.window(span_us).flat_map(move |b| {
            let age = ((newest - b.timestamp_us) as f64 / span) as f32;
            b.blobs.iter().map(move |o| TrailPoint {
                x: px(o.x),
                y: px(o.y),
                age,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use espion_proto::cam::{Blob, Format};

    use super::*;

    fn frame(generation: u32, number: u32, t_us: u64, blobs: &[(f64, f64)]) -> BlobFrame {
        BlobFrame {
            seq: u64::from(number) + 1,
            frame: number,
            generation,
            timestamp_us: t_us,
            format: Format::Raw8,
            width: 640,
            height: 400,
            threshold: 100,
            detect_us: 500,
            timing: Default::default(),
            flags: 0,
            blobs: blobs
                .iter()
                .map(|&(x, y)| Blob {
                    x: (x * f64::from(SUBPIXEL)).round() as u32,
                    y: (y * f64::from(SUBPIXEL)).round() as u32,
                    width: 384,
                    flux: 3000,
                    area: 10,
                    peak: 200,
                })
                .collect(),
            arrived: Instant::now(),
        }
    }

    #[test]
    fn the_shown_frame_finds_its_blobs() {
        let mut m = Markers::default();
        for n in 0..10 {
            m.push(frame(3, n, u64::from(n) * 1000, &[(f64::from(n), 5.0)]));
        }
        let b = m.of_frame(3, 4).expect("frame 4");
        assert_eq!(px(b.blobs[0].x), 4.0);
        assert!(m.of_frame(2, 4).is_none());
        assert!(m.of_frame(3, 10).is_none());
        assert_eq!(m.newest().map(|b| b.frame), Some(9));
        assert_eq!(m.seq(), 10);
    }

    #[test]
    fn statistics_take_the_frames_with_one_blob_in_the_window() {
        let mut m = Markers::default();
        // 1 ms apart: one blob alternating between x 10 and 12 (mean 11, sigma 1), y 20;
        // every fifth frame empty, every seventh with two blobs.
        for n in 0u32..100 {
            let blobs: Vec<(f64, f64)> = if n % 5 == 0 {
                vec![]
            } else if n % 7 == 0 {
                vec![(1.0, 1.0), (2.0, 2.0)]
            } else {
                vec![(if n % 2 == 0 { 10.0 } else { 12.0 }, 20.0)]
            };
            m.push(frame(1, n, u64::from(n) * 1000, &blobs));
        }
        let all = m.stats(1_000_000);
        assert_eq!((all.frames, all.found), (100, 80));
        let two = (0u32..100).filter(|n| n % 5 != 0 && n % 7 == 0).count() as u32;
        assert_eq!(all.single, 80 - two);
        assert!((all.mean_x - 11.0).abs() < 0.05, "{all:?}");
        assert!((all.sigma_x - 1.0).abs() < 0.01, "{all:?}");
        assert_eq!((all.mean_y, all.sigma_y), (20.0, 0.0));
        assert_eq!((all.mean_peak, all.mean_area), (200.0, 10.0));
        assert!((all.detection_rate() - 0.8).abs() < 1e-12);
        // The last 9 ms: frames 90..=99.
        let last = m.stats(9_000);
        assert_eq!((last.frames, last.found), (10, 8));
        assert_eq!(Markers::default().stats(1000), MarkerStats::default());
    }

    #[test]
    fn the_trail_fades_with_age_and_ends_at_its_length() {
        let mut m = Markers::default();
        for n in 0u32..11 {
            m.push(frame(1, n, u64::from(n) * 100, &[(f64::from(n), 0.0)]));
        }
        let t: Vec<TrailPoint> = m.trail(500).collect();
        assert_eq!(t.len(), 6);
        assert_eq!((t[0].x, t[0].age), (5.0, 1.0));
        assert_eq!((t[5].x, t[5].age), (10.0, 0.0));
        m.prune(200);
        assert_eq!(m.trail(10_000).count(), 3);
    }

    #[test]
    fn a_new_generation_or_a_restart_starts_over() {
        let mut m = Markers::default();
        m.push(frame(1, 0, 1000, &[(1.0, 1.0)]));
        m.push(frame(1, 1, 2000, &[(1.0, 1.0)]));
        m.push(frame(2, 2, 3000, &[(1.0, 1.0)]));
        assert_eq!(m.stats(u64::MAX).frames, 1);
        m.push(frame(2, 0, 10, &[(1.0, 1.0)]));
        assert_eq!(m.stats(u64::MAX).frames, 1);
        assert_eq!(m.newest().map(|b| b.timestamp_us), Some(10));
    }
}
