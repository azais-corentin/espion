use std::{collections::BTreeMap, vec::Vec};

use super::*;

/// Frame period at 120 Hz, µs.
const PERIOD: u64 = 8333;
const W: u16 = 1280;
const H: u16 = 800;
/// Markers nearer than this come out as one blob, pixels.
const MERGE: f64 = 8.0;

/// A marker of a simulated frame: who it is and where.
#[derive(Clone, Copy, Debug)]
struct Truth {
    id: u32,
    x: f64,
    y: f64,
}

fn at(id: u32, x: f64, y: f64) -> Truth {
    Truth { id, x, y }
}

fn spot(x: f64, y: f64, sigma: f64) -> Spot {
    Spot {
        x: (x * 256.0).round() as u32,
        y: (y * 256.0).round() as u32,
        width: (sigma * 256.0).round() as u16,
        flux: 2000,
    }
}

/// A tracker fed simulated frames, and what became of each marker's identity.
struct Sim {
    tracker: Tracker<32>,
    t_us: u64,
    frame: u32,
    /// The ID each marker had when it last had a blob of its own.
    last: BTreeMap<u32, u32>,
    /// The marker each ID was last on.
    owner: BTreeMap<u32, u32>,
    /// Every ID ever given.
    given: Vec<u32>,
    /// Markers whose ID changed, and IDs that went to another marker.
    switches: u32,
    transfers: u32,
}

impl Sim {
    fn new() -> Self {
        Self {
            tracker: Tracker::default(),
            t_us: 1_000_000,
            frame: 0,
            last: BTreeMap::new(),
            owner: BTreeMap::new(),
            given: Vec::new(),
            switches: 0,
            transfers: 0,
        }
    }

    /// Lets `frames` frame periods pass without a frame (the detector skipped them).
    fn skip(&mut self, frames: u64) {
        self.t_us += frames * PERIOD;
    }

    /// One frame with `markers` in view and `extra` blobs of no marker. Markers nearer than
    /// [`MERGE`] make one blob between them. The blobs go to the tracker in an order that
    /// changes with every frame. Returns each blob's ID with the markers in it.
    fn step(&mut self, markers: &[Truth], extra: &[(f64, f64)]) -> Vec<(u32, Vec<u32>)> {
        // Group the markers that merge (chains included).
        let mut group: Vec<usize> = (0..markers.len()).collect();
        for i in 0..markers.len() {
            for j in 0..i {
                let (a, b) = (markers[i], markers[j]);
                if (a.x - b.x).hypot(a.y - b.y) < MERGE {
                    let (from, to) = (group[i], group[j]);
                    group
                        .iter_mut()
                        .filter(|g| **g == from)
                        .for_each(|g| *g = to);
                }
            }
        }
        let mut blobs: Vec<(Spot, Vec<u32>)> = Vec::new();
        for g in 0..markers.len() {
            let of: Vec<Truth> = (0..markers.len())
                .filter(|&i| group[i] == g)
                .map(|i| markers[i])
                .collect();
            if of.is_empty() {
                continue;
            }
            let n = of.len() as f64;
            let (x, y) = (
                of.iter().map(|m| m.x).sum::<f64>() / n,
                of.iter().map(|m| m.y).sum::<f64>() / n,
            );
            let spread = of
                .iter()
                .map(|m| (m.x - x).hypot(m.y - y))
                .fold(0.0, f64::max);
            blobs.push((
                spot(x, y, (2.5f64 * 2.5 + spread * spread).sqrt()),
                of.iter().map(|m| m.id).collect(),
            ));
        }
        blobs.extend(extra.iter().map(|&(x, y)| (spot(x, y, 1.0), Vec::new())));
        // The detector's order is no identity: another one every frame.
        let turn = self.frame as usize % blobs.len().max(1);
        blobs.rotate_left(turn);
        if self.frame % 3 == 1 {
            blobs.reverse();
        }
        let spots: Vec<Spot> = blobs.iter().map(|b| b.0).collect();
        let mut ids = std::vec![0; spots.len()];
        self.t_us += PERIOD;
        self.frame += 1;
        self.tracker.update(self.t_us, W, H, &spots, &mut ids);
        // Every ID of a frame is different and none is 0.
        for (k, id) in ids.iter().enumerate() {
            assert_ne!(*id, 0, "frame {}", self.frame);
            assert!(!ids[..k].contains(id), "frame {}: {ids:?}", self.frame);
        }
        self.given.extend(&ids);
        for (id, (_, of)) in ids.iter().zip(&blobs) {
            // A merged blob is the marker of none: it may carry either ID.
            let [marker] = of[..] else {
                continue;
            };
            if self.last.insert(marker, *id).is_some_and(|l| l != *id) {
                self.switches += 1;
            }
            if self.owner.insert(*id, marker).is_some_and(|o| o != marker) {
                self.transfers += 1;
            }
        }
        ids.into_iter()
            .zip(blobs.into_iter().map(|b| b.1))
            .collect()
    }

    /// The ID marker `m` had when it last had a blob of its own.
    fn id(&self, m: u32) -> u32 {
        self.last[&m]
    }

    fn stable(&self) -> bool {
        self.switches == 0 && self.transfers == 0
    }
}

#[test]
fn still_markers_keep_their_ids_whatever_their_order() {
    let mut s = Sim::new();
    let markers: Vec<Truth> = (0..9)
        .map(|i| {
            at(
                i,
                100.0 + 120.0 * f64::from(i % 3),
                100.0 + 200.0 * f64::from(i / 3),
            )
        })
        .collect();
    for _ in 0..500 {
        s.step(&markers, &[]);
    }
    assert!(s.stable());
    // One ID each, the first nine.
    let mut ids: Vec<u32> = (0..9).map(|m| s.id(m)).collect();
    ids.sort_unstable();
    assert_eq!(ids, (1..=9).collect::<Vec<u32>>());
    assert_eq!(s.tracker.last_id(), 9);
}

#[test]
fn a_marker_that_goes_or_comes_leaves_the_others_their_ids() {
    let mut s = Sim::new();
    let all: Vec<Truth> = (0..5)
        .map(|i| at(i, 200.0 + 150.0 * f64::from(i), 300.0))
        .collect();
    for _ in 0..50 {
        s.step(&all, &[]);
    }
    let before: Vec<u32> = (0..5).map(|m| s.id(m)).collect();
    // The first one in the detector's order goes: with the blobs' index as their identity,
    // every other marker would change.
    for _ in 0..100 {
        s.step(&all[1..], &[]);
    }
    // A new one comes, and one in the middle goes.
    let mut now: Vec<Truth> = all[1..].to_vec();
    now.remove(1);
    now.push(at(9, 50.0, 100.0));
    for _ in 0..100 {
        s.step(&now, &[]);
    }
    assert!(s.stable());
    for m in [1, 3, 4] {
        assert_eq!(s.id(m), before[m as usize]);
    }
    assert!(!before.contains(&s.id(9)), "the new marker has a new ID");
    // The first marker comes back long after: it is a new marker.
    for _ in 0..10 {
        s.step(&all, &[]);
    }
    assert!(!s.given[..s.given.len() - 50].contains(&s.id(0)));
    assert_eq!(s.id(1), before[1]);
}

/// A marker on a curved path at about `speed` px/s, at frame `k`.
fn path(id: u32, k: u32, speed: f64) -> Truth {
    let t = f64::from(k) * PERIOD as f64 / 1e6;
    let w = speed / 300.0;
    at(
        id,
        640.0 + 300.0 * (w * t).sin(),
        400.0 + 200.0 * (0.7 * w * t).cos(),
    )
}

#[test]
fn a_marker_missed_for_a_few_frames_comes_back_with_its_id() {
    for missed in [1, 2, 5, 12, 25] {
        let mut s = Sim::new();
        let still = at(2, 100.0, 100.0);
        for k in 0..2000 {
            // Missed for `missed` frames (up to 208 ms) every 100.
            if k % 100 >= 50 && k % 100 < 50 + missed {
                s.step(&[still], &[]);
            } else {
                s.step(&[path(1, k, 600.0), still], &[]);
            }
        }
        assert!(s.stable(), "missed for {missed} frames");
    }
}

#[test]
fn a_marker_gone_for_long_is_a_new_marker() {
    let mut s = Sim::new();
    let still = at(2, 100.0, 100.0);
    for k in 0..100 {
        s.step(&[path(1, k, 300.0), still], &[]);
    }
    let first = s.id(1);
    // Half a second without it.
    for _ in 0..60 {
        s.step(&[still], &[]);
    }
    s.step(&[path(1, 160, 300.0), still], &[]);
    assert_ne!(s.id(1), first);
    assert_eq!(s.transfers, 0);
    assert_eq!(s.id(2), 2.min(s.id(2)), "the other marker kept its ID");
    assert_eq!(s.given.iter().filter(|i| **i == first).count(), 100);
}

#[test]
fn a_marker_that_lights_up_near_one_that_went_dark_keeps_its_new_id() {
    // At every frame, and with only one frame in five detected (a busy detector): the dark
    // marker's track looks for its blob farther and farther out while the new one, 46 px
    // away, is not confirmed yet.
    for every in [1, 5] {
        let mut s = Sim::new();
        let still = at(3, 900.0, 600.0);
        for _ in 0..40 {
            s.step(&[at(1, 300.0, 200.0), still], &[]);
            s.skip(every - 1);
        }
        let old = s.id(1);
        for _ in 0..40 {
            s.step(&[at(2, 340.0, 224.0), still], &[]);
            s.skip(every - 1);
        }
        assert!(s.stable(), "one frame in {every}");
        assert_ne!(s.id(2), old);
    }
}

#[test]
fn frames_the_detector_skips_break_no_identity() {
    let mut s = Sim::new();
    for k in 0..3000u32 {
        // One frame in three is not detected, at times two in a row.
        if k % 3 == 2 || k % 50 == 10 {
            s.skip(1);
            continue;
        }
        s.step(
            &[
                path(1, k, 1000.0),
                path(2, k + 700, 500.0),
                at(3, 30.0, 30.0),
            ],
            &[],
        );
    }
    assert!(s.stable());
}

#[test]
fn a_marker_that_leaves_the_view_ends_and_the_one_that_enters_is_new() {
    let mut s = Sim::new();
    let still = at(9, 640.0, 400.0);
    // Marker 1 leaves through the right edge at 480 px/s.
    let mut x = 1200.0;
    while x < f64::from(W) - 1.0 {
        s.step(&[at(1, x, 300.0), still], &[]);
        x += 4.0;
    }
    let gone = s.id(1);
    // Two frames later marker 2 enters through the same edge, 20 px lower.
    s.step(&[still], &[]);
    s.step(&[still], &[]);
    let mut x = f64::from(W) - 2.0;
    while x > 1100.0 {
        s.step(&[at(2, x, 320.0), still], &[]);
        x -= 4.0;
    }
    assert!(s.stable());
    assert_ne!(s.id(2), gone);
    // A marker held at the edge, missed for a few frames, stays itself.
    let mut s = Sim::new();
    for k in 0..300 {
        if k % 20 < 4 {
            s.step(&[still], &[]);
        } else {
            s.step(&[at(1, 1278.0, 300.0), still], &[]);
        }
    }
    assert!(s.stable());
}

#[test]
fn markers_that_cross_keep_their_ids() {
    // Two markers on straight lines through the same point, at speeds from 60 to 1500 px/s
    // and angles from head-on to nearly alongside; they merge into one blob while they are
    // nearer than MERGE. A velocity is believed for `coast_us`: markers that stay merged for
    // longer are not crossing any more (the next test).
    let believed = (u64::from(Params::DEFAULT.coast_us) / PERIOD) as u32;
    let mut crossings = 0;
    for (va, vb) in [
        (60.0, 60.0),
        (200.0, 120.0),
        (500.0, 300.0),
        (1500.0, 400.0),
        (900.0, 0.0),
    ] {
        for angle in [180.0f64, 150.0, 90.0, 45.0, 20.0] {
            let (ca, sa) = (angle.to_radians().cos(), angle.to_radians().sin());
            let mut s = Sim::new();
            let dt = PERIOD as f64 / 1e6;
            let mut merged = 0;
            for k in -150i32..150 {
                let t = (f64::from(k) + 0.3) * dt;
                let a = at(1, 640.0 + va * t, 400.0);
                let b = at(2, 640.0 + vb * t * ca, 400.0 + vb * t * sa);
                let out = s.step(&[a, b, at(3, 100.0, 100.0)], &[]);
                merged += u32::from(out.iter().any(|(_, of)| of.len() == 2));
            }
            assert!(merged > 0, "{va} {vb} {angle}: they never merged");
            if merged >= believed {
                continue;
            }
            assert!(
                s.stable(),
                "{va} and {vb} px/s at {angle} degrees: {} switches, {} transfers",
                s.switches,
                s.transfers
            );
            crossings += 1;
        }
    }
    assert_eq!(crossings, 23);
}

#[test]
fn markers_that_travel_together_merged_come_apart_with_their_ids() {
    // Two markers side by side close in until they are one blob, stay so for half a second,
    // then part again, each on its own side.
    let mut s = Sim::new();
    for k in 0..400 {
        let gap = match k {
            0..150 => 30.0 - 0.19 * f64::from(k),
            150..210 => 1.5,
            _ => 1.5 + 0.15 * f64::from(k - 210),
        };
        let x = 200.0 + 2.0 * f64::from(k);
        s.step(
            &[at(1, x, 400.0 - gap / 2.0), at(2, x, 400.0 + gap / 2.0)],
            &[],
        );
    }
    assert!(
        s.stable(),
        "{} switches, {} transfers",
        s.switches,
        s.transfers
    );
}

#[test]
fn fast_markers_keep_their_ids() {
    // Straight at 6000 px/s (50 px a frame), next to a still marker and a slow one.
    let mut s = Sim::new();
    for k in 0..24 {
        let x = 20.0 + 50.0 * f64::from(k);
        s.step(
            &[
                at(1, x, 400.0),
                at(2, 600.0, 430.0),
                at(3, 300.0 + f64::from(k), 380.0),
            ],
            &[],
        );
    }
    assert!(s.stable());
    // Back and forth between two walls, turning back at once, at 300 to 4800 px/s.
    for speed in [300.0, 1200.0, 2400.0, 4800.0] {
        let mut s = Sim::new();
        let (mut x, mut dir) = (200.0, 1.0);
        for _ in 0..1500 {
            x += dir * speed * PERIOD as f64 / 1e6;
            if x > 1100.0 {
                (x, dir) = (2200.0 - x, -1.0);
            } else if x < 200.0 {
                (x, dir) = (400.0 - x, 1.0);
            }
            s.step(&[at(1, x, 400.0), at(2, 640.0, 440.0)], &[]);
        }
        assert!(s.stable(), "{speed} px/s: {} switches", s.switches);
    }
    // In circles of 100 px radius at 3000 px/s: 9 g at a pixel per millimetre.
    let mut s = Sim::new();
    for k in 0..1000 {
        let a = 30.0 * f64::from(k) * PERIOD as f64 / 1e6;
        s.step(
            &[
                at(1, 640.0 + 100.0 * a.cos(), 400.0 + 100.0 * a.sin()),
                at(2, 640.0, 400.0),
            ],
            &[],
        );
    }
    assert!(s.stable());
}

#[test]
fn a_second_blob_of_a_marker_never_takes_its_id() {
    // A marker in motion that comes out as two blobs in some frames (a second one a few
    // pixels behind it, or at its very place), as a marker on a slow screen does.
    let mut s = Sim::new();
    for k in 0..3000u32 {
        let m = path(1, k, 800.0);
        let behind = path(1, k.saturating_sub(1), 800.0);
        // Not from its first frames on: two blobs that start together are two markers.
        let extra: &[(f64, f64)] = match k % 11 {
            _ if k < 20 => &[],
            0..=2 => &[(behind.x, behind.y)],
            5 => &[(m.x + 0.8, m.y - 0.6)],
            7 => &[(m.x - 1.0, m.y + 0.5), (behind.x, behind.y)],
            _ => &[],
        };
        s.step(&[m, at(2, 640.0, 400.0)], extra);
    }
    assert!(s.stable(), "{} switches", s.switches);
}

#[test]
fn blobs_of_noise_take_no_marker_s_id() {
    // One blob of noise per frame, anywhere, some right next to the markers.
    let mut s = Sim::new();
    let mut rng = 0x2545_f491u32;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        f64::from(rng) / f64::from(u32::MAX)
    };
    for k in 0..3000u32 {
        let m = path(1, k, 400.0);
        let noise = if k % 5 == 0 {
            (m.x + 30.0 * (next() - 0.5), m.y + 30.0 * (next() - 0.5))
        } else {
            (1279.0 * next(), 799.0 * next())
        };
        s.step(&[m, at(2, 200.0, 200.0)], &[noise]);
    }
    assert!(
        s.stable(),
        "{} switches, {} transfers",
        s.switches,
        s.transfers
    );
}

#[test]
fn a_reset_or_a_clock_that_goes_back_ends_the_tracks_and_the_ids_go_on() {
    let mut s = Sim::new();
    let markers = [at(1, 100.0, 100.0), at(2, 500.0, 300.0)];
    s.step(&markers, &[]);
    s.step(&markers, &[]);
    assert_eq!(s.tracker.tracks(), 2);
    let before = [s.id(1), s.id(2)];
    s.tracker.reset();
    assert_eq!(s.tracker.tracks(), 0);
    s.step(&markers, &[]);
    assert!(!before.contains(&s.id(1)) && !before.contains(&s.id(2)));
    // The board restarted: its clock starts again.
    let before = [s.id(1), s.id(2)];
    s.t_us = 5000;
    s.step(&markers, &[]);
    assert!(!before.contains(&s.id(1)) && !before.contains(&s.id(2)));
    assert_eq!(s.tracker.last_id(), 6);
}

#[test]
fn more_blobs_than_tracks_all_get_ids() {
    let mut t: Tracker<4> = Tracker::default();
    let spots: Vec<Spot> = (0..7)
        .map(|i| spot(100.0 + 50.0 * f64::from(i), 200.0, 2.0))
        .collect();
    let mut first = [0; 7];
    let u = t.update(1000, W, H, &spots, &mut first);
    assert_eq!((u.new, u.kept, u.tracks), (7, 0, 4));
    let mut second = [0; 7];
    let u = t.update(1000 + PERIOD, W, H, &spots, &mut second);
    // The four tracked keep their IDs; the others get new ones each frame.
    assert_eq!((u.new, u.kept), (3, 4));
    assert_eq!(first[..4], second[..4]);
    let mut all: Vec<u32> = first.iter().chain(&second[4..]).copied().collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), 10);
    // Fewer IDs than blobs asked for: only those are given.
    let mut two = [0; 2];
    assert_eq!(t.update(1000 + 2 * PERIOD, W, H, &spots, &mut two).kept, 2);
    assert_eq!(two, [first[0], first[1]]);
}

/// A scene like the screen target's random walk: markers on smoothed random walks that
/// leave and enter the view, merge when they meet, are missed now and then, among blobs of
/// noise. Returns the simulation after `frames` frames.
fn walk(seed: u32, frames: u32) -> Sim {
    struct Walker {
        id: u32,
        x: f64,
        y: f64,
        vx: f64,
        vy: f64,
    }
    let mut rng = seed.wrapping_mul(0x9e37_79b9) | 1;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        f64::from(rng) / f64::from(u32::MAX)
    };
    let dt = PERIOD as f64 / 1e6;
    let mut s = Sim::new();
    let mut live: Vec<Walker> = Vec::new();
    let mut next_id = 1;
    let mut missing: BTreeMap<u32, u32> = BTreeMap::new();
    for _ in 0..frames {
        // Six markers in view: one that left is replaced by one entering elsewhere.
        while live.len() < 6 {
            let speed = 100.0 + 500.0 * next();
            let (x, y, vx, vy) = match (next() * 4.0) as u32 {
                0 => (1.0, 799.0 * next(), speed, speed * (next() - 0.5)),
                1 => (1278.0, 799.0 * next(), -speed, speed * (next() - 0.5)),
                2 => (1279.0 * next(), 1.0, speed * (next() - 0.5), speed),
                _ => (1279.0 * next(), 798.0, speed * (next() - 0.5), -speed),
            };
            live.push(Walker {
                id: next_id,
                x,
                y,
                vx,
                vy,
            });
            next_id += 1;
        }
        for m in &mut live {
            // A push every frame, a pull back to 400 px/s at most.
            m.vx += 4000.0 * dt * (next() - 0.5) * 2.0;
            m.vy += 4000.0 * dt * (next() - 0.5) * 2.0;
            let v = m.vx.hypot(m.vy);
            if v > 800.0 {
                (m.vx, m.vy) = (m.vx * 800.0 / v, m.vy * 800.0 / v);
            }
            m.x += m.vx * dt;
            m.y += m.vy * dt;
        }
        live.retain(|m| m.x >= 0.0 && m.y >= 0.0 && m.x <= 1279.0 && m.y <= 799.0);
        // A marker is missed for 1 to 12 frames, about once every 2 s.
        for m in &live {
            if !missing.contains_key(&m.id) && next() < 1.0 / 240.0 {
                missing.insert(m.id, 1 + (next() * 12.0) as u32);
            }
        }
        missing.retain(|_, left| {
            *left -= 1;
            *left > 0
        });
        let seen: Vec<Truth> = live
            .iter()
            .filter(|m| !missing.contains_key(&m.id))
            .map(|m| at(m.id, m.x, m.y))
            .collect();
        let noise = [(1279.0 * next(), 799.0 * next())];
        s.step(&seen, if next() < 0.2 { &noise } else { &[] });
    }
    s
}

#[test]
fn a_random_walk_keeps_nearly_every_identity() {
    // Two minutes at 120 Hz per seed: about 200 markers come and go, each missed for up to
    // 100 ms now and then, some meeting. What is left: markers that merge and part again
    // otherwise than their velocities said, and markers missed right where another one is.
    let (mut switches, mut transfers, mut markers) = (0, 0, 0);
    for seed in [1, 2, 3, 4, 5] {
        let s = walk(seed, 14_400);
        std::eprintln!(
            "seed {seed}: {} markers, {} switches, {} transfers, {} IDs",
            s.last.len(),
            s.switches,
            s.transfers,
            s.tracker.last_id()
        );
        switches += s.switches;
        transfers += s.transfers;
        markers += s.last.len();
    }
    assert!(markers > 500, "{markers} markers");
    assert!(
        switches * 50 <= markers as u32,
        "{switches} switches of {markers} markers"
    );
    assert!(
        transfers * 100 <= markers as u32,
        "{transfers} transfers of {markers} markers"
    );
}

#[test]
fn the_same_blobs_give_the_same_ids() {
    // Integer arithmetic only: the board and the fake camera agree.
    let (a, b) = (walk(7, 2000), walk(7, 2000));
    assert_eq!(a.given, b.given);
    assert!(a.given.len() > 10_000);
}
