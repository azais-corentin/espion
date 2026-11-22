//! The blobs' identities (`espion_vision::track`, [`espion_proto::cam::Blob::id`]) against
//! what the screen target's page drew.
//!
//! The `filmed` tests need nothing: a camera in code ([`film`]) renders a random walk's
//! markers frame by frame on its own clock and runs the board's detector, measure and tracker
//! on each frame, and the screen target's check counts the identities against the markers
//! drawn. The same frames every run, so the counts are the same.
//!
//! `replay` (ignored) needs scenes recorded on the board by `tests/board.rs` `ids`
//! (`POC_IDS_LOG=DIR`): it runs the tracker again over each scene's blobs and prints the
//! identity figures, so a change of the tracker is measured on real blobs without the board.
//! `POC_IDS_SHOW=N` also prints the first N switches of each scene, each with the marker and
//! the blobs around it in the frames before (`POC_IDS_RECORDED=1`: the switches of the IDs as
//! recorded, not of the tracker run again). `POC_IDS_PARAMS` changes the tracker's constants
//! (`espion_vision::track::Params`), e.g. `coast_us=300000,speed_slack=300`. The screen
//! target test (`tests/target.rs`) records its scenes the same way.
//!
//! ```sh
//! POC_IDS_LOG=/tmp/ids cargo test -p poc-viewer --release --test ids -- --ignored --nocapture replay
//! ```

use espion_proto::cam::{AUTO_MIN_AREA, FRAME_BLOBS_MAX, SUBPIXEL};
use espion_vision::{
    Blob, DefaultDetector, Split,
    refine::{self, Spot},
    track::{DefaultTracker, Params, Tracker},
};
use poc_viewer::screen_target::{
    check::{Check, FrameMatch, Seen},
    ids::{self, IdCounts, Identities},
    link::{Drawn, FWHM_PER_SIGMA},
    sim::{self, Walk},
};

/// The filmed camera: its window, frame period (µs), dark level and threshold (8-bit).
const FILM_SIZE: (u16, u16) = (640, 400);
const FILM_PERIOD_US: u64 = 8_333;
const FILM_BLACK: u8 = 8;
const FILM_THRESHOLD: u8 = 24;

/// Films `secs` of the random walk of `seed` (markers of about 6 px that cross, touch, leave
/// at the edge and are replaced) and returns the identity counts by the tracker's IDs and by
/// each blob's place in its frame. `cut(frame)`: the frame is not detected (as when the board
/// cuts or skips one). `hidden(id, secs)`: the marker is not lit.
fn film(
    seed: u32,
    secs: f64,
    cut: impl Fn(u32) -> bool,
    hidden: impl Fn(u32, f64) -> bool,
) -> (IdCounts, IdCounts) {
    let (w, h) = FILM_SIZE;
    let (wide, high) = (usize::from(w), usize::from(h));
    let model = sim::model();
    let mut walk = Walk::new(seed, f64::from(w), f64::from(h), 6.0);
    let mut check = Check::new(model, FILM_SIZE);
    let mut detector: Box<DefaultDetector> = Box::default();
    let mut tracker: Box<DefaultTracker> = Box::default();
    let mut frame = vec![0u8; wide * high];
    let mut found = vec![Blob::default(); 256];
    let (mut spots, mut given) = (Vec::new(), Vec::new());
    let mut by_index = Identities::default();
    let mut taken = 0;
    for n in 0..(secs * 1e6 / FILM_PERIOD_US as f64) as u32 {
        let t_us = u64::from(n) * FILM_PERIOD_US;
        let t = t_us as f64 / 1e6;
        let lit: Vec<_> = walk
            .step(t)
            .into_iter()
            .filter(|&(id, ..)| !hidden(id, t))
            .collect();
        let drawn: Vec<Drawn> = lit
            .iter()
            .map(|&(id, u, v, size)| {
                let (x, y) = model.to_screen(u, v);
                Drawn {
                    id,
                    x,
                    y,
                    sigma: size / FWHM_PER_SIGMA / model.scale_at(x, y),
                    level: 1.0,
                }
            })
            .collect();
        check.drawn(t_us as f64, &drawn);
        if cut(n) {
            continue;
        }
        // The frame: each marker a Gaussian spot of peak 200 on the dark level.
        frame.fill(FILM_BLACK);
        for &(_, u, v, size) in &lit {
            let sigma = size / FWHM_PER_SIGMA;
            let reach = (4.0 * sigma).ceil();
            let rows = (v - reach).max(0.0) as usize..=((v + reach) as usize).min(high - 1);
            let cols = (u - reach).max(0.0) as usize..=((u + reach) as usize).min(wide - 1);
            if v + reach < 0.0 || u + reach < 0.0 {
                continue;
            }
            for row in rows {
                for col in cols.clone() {
                    let r2 = (col as f64 - u).powi(2) + (row as f64 - v).powi(2);
                    let light = 200.0 * (-r2 / (2.0 * sigma * sigma)).exp();
                    let p = &mut frame[row * wide + col];
                    *p = (f64::from(*p) + light).round().min(255.0) as u8;
                }
            }
        }
        // As the board and the fake camera do: blobs split by their peaks, the automatic
        // area filter, each measured on the frame, then the identities.
        detector.set_split(Some(Split {
            background: u16::from(FILM_BLACK),
            min_area: AUTO_MIN_AREA,
        }));
        let count = detector.process_frame(&frame, w, FILM_THRESHOLD, &mut found);
        let kept = found[..count]
            .iter()
            .filter(|b| b.area >= AUTO_MIN_AREA)
            .take(FRAME_BLOBS_MAX);
        spots.clear();
        spots.extend(kept.map(|b| refine::refine_raw8(&frame, w, h, b)));
        given.clear();
        given.resize(spots.len(), 0);
        tracker.update(t_us, w, h, &spots, &mut given);
        let px = |v: u32| f64::from(v) / f64::from(SUBPIXEL);
        let seen = spots
            .iter()
            .zip(&given)
            .map(|(s, &id)| Seen {
                id,
                x: px(s.x),
                y: px(s.y),
                sigma: px(u32::from(s.width)),
                flux: f64::from(s.flux),
            })
            .collect();
        check.detected(1, n, t_us, t_us as f64, FILM_PERIOD_US as f64, seen);
        // The check matches the frames (and makes its figures) now and then, as in the viewer.
        if n % 12 != 0 {
            continue;
        }
        check.update(t_us as f64);
        for m in check.matches_since(taken) {
            by_index.add(m, |j| j as u32);
        }
        taken = check.report().total.frames;
    }
    (check.report().ids, by_index.counts())
}

/// Markers that cross, touch, leave the view and enter it, every frame detected: a marker
/// keeps its ID, but for the crossings so slow that positions do not tell who came out
/// where. Their place in the frame, as an ID, changes all the time.
#[test]
fn a_filmed_random_walk_keeps_its_ids() {
    let (mut ids, mut index) = (IdCounts::default(), IdCounts::default());
    for seed in 1..=3 {
        let (i, x) = film(seed, 20.0, |_| false, |_, _| false);
        println!("seed {seed}, tracker IDs:    {i}");
        println!("seed {seed}, by frame index: {x}");
        assert!(i.markers >= 15, "the walk replaced few markers: {i}");
        assert!(i.observations > 10_000, "{i}");
        assert!(x.switches > 5 * i.markers, "{x}");
        ids.markers += i.markers;
        ids.switches += i.switches;
        ids.transfers += i.transfers;
        index.switches += x.switches;
    }
    assert!(ids.switches * 20 <= ids.markers, "{ids}");
    assert!(ids.transfers * 20 <= ids.markers, "{ids}");
    assert!(index.switches > 100 * ids.switches.max(1), "{index}");
}

/// The same walk through what a camera does to it: one frame in ten is not detected, and two
/// frames in a row now and then; every third marker goes dark for 100 ms each second. A
/// marker missed for that long comes back with its ID (but one that had just entered, or
/// that the dark took across the edge or into another marker).
#[test]
fn a_filmed_walk_with_missing_frames_and_blinking_markers_keeps_its_ids() {
    let cut = |n: u32| n % 10 == 3 || n % 97 == 4;
    let hidden = |id: u32, t: f64| id.is_multiple_of(3) && (t + f64::from(id) * 0.13).fract() < 0.1;
    let (mut markers, mut switches, mut transfers) = (0, 0, 0);
    let (mut gaps, mut kept) = (0, 0);
    for seed in 1..=3 {
        let (i, _) = film(seed, 20.0, cut, hidden);
        println!("seed {seed}: {i}");
        markers += i.markers;
        switches += i.switches;
        transfers += i.transfers;
        gaps += i.gaps;
        kept += i.gaps_kept;
    }
    assert!(gaps >= 60, "{gaps} gaps");
    assert!(kept * 8 >= gaps * 7, "{kept} of {gaps} gaps bridged");
    assert!(
        switches * 10 <= markers,
        "{switches} switches, {markers} markers"
    );
    assert!(
        transfers * 10 <= markers,
        "{transfers} transfers, {markers} markers"
    );
}

/// The IDs a tracker gives the blobs of recorded `frames` (frame by frame, blob by blob), the
/// frame of `window`; and the tracks it started.
fn tracked(frames: &[FrameMatch], window: (u16, u16), params: Params) -> (Vec<Vec<u32>>, u32) {
    let mut tracker: Tracker<128> = Tracker::new(params);
    let mut spots = Vec::new();
    let mut started = 0;
    let given = frames
        .iter()
        .map(|m| {
            let q = |v: f64| (v * f64::from(SUBPIXEL)).round();
            spots.clear();
            spots.extend(m.blobs.iter().map(|b| Spot {
                x: q(b.x) as u32,
                y: q(b.y) as u32,
                width: q(b.sigma) as u16,
                flux: b.flux as u32,
            }));
            let mut given = vec![0; spots.len()];
            let u = tracker.update(m.timestamp_us, window.0, window.1, &spots, &mut given);
            started += u32::from(u.new);
            given
        })
        .collect();
    (given, started)
}

/// The tracker's constants, changed by `POC_IDS_PARAMS` (`name=value,...`).
fn params() -> Params {
    let mut p = Params::DEFAULT;
    let Ok(set) = std::env::var("POC_IDS_PARAMS") else {
        return p;
    };
    for kv in set.split(',').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').expect("name=value");
        let n: u32 = v.parse().expect("a number");
        match k {
            "gate" => p.gate = n as u16,
            "speed_gain" => p.speed_gain = n as u16,
            "speed_slack" => p.speed_slack = n as u16,
            "miss_gain" => p.miss_gain = n as u16,
            "speed_new" => p.speed_new = n as u16,
            "gate_max" => p.gate_max = n as u16,
            "confirm" => p.confirm = n as u8,
            "coast_us" => p.coast_us = n,
            "coast_new_us" => p.coast_new_us = n,
            "hidden_us" => p.hidden_us = n,
            "reach_widths" => p.reach_widths = n as u16,
            "hold_widths" => p.hold_widths = n as u16,
            "stick" => p.stick = n as u16,
            "beta" => p.beta = n as u16,
            _ => panic!("no constant {k}"),
        }
    }
    p
}

#[test]
#[ignore = "needs scenes recorded on the board (POC_IDS_LOG)"]
fn replay() {
    let dir = std::env::var("POC_IDS_LOG").expect("POC_IDS_LOG: the directory of the scenes");
    let show: usize = std::env::var("POC_IDS_SHOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("the directory")
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("a scene");
        let frames: Vec<FrameMatch> = text.lines().filter_map(ids::from_line).collect();
        // The board's start-up window.
        let (given, started) = tracked(&frames, (1280, 800), params());
        // The switches shown: the replayed tracker's, or with `POC_IDS_RECORDED` those of the
        // IDs as recorded.
        let as_recorded = std::env::var_os("POC_IDS_RECORDED").is_some();
        let mut by_index = Identities::default();
        let mut recorded = Identities::logging();
        let mut replayed = Identities::logging();
        for (m, given) in frames.iter().zip(&given) {
            by_index.add(m, |j| j as u32);
            recorded.add(m, |j| m.blobs[j].id);
            replayed.add(m, |j| given[j]);
        }
        println!("== {} ({} frames)", path.display(), frames.len());
        println!("   by frame index: {}", by_index.counts());
        println!("   as recorded:    {}", recorded.counts());
        println!("   replayed:       {}; {started} tracks", replayed.counts());
        let shown = if as_recorded { &recorded } else { &replayed };
        for s in shown.switches().iter().take(show) {
            println!("   {s:?}");
            let at = frames
                .iter()
                .position(|m| m.frame == s.frame)
                .expect("the switch's frame");
            for (m, given) in frames.iter().zip(&given).take(at + 1).skip(at.max(6) - 6) {
                let Some((k, _, b)) = m.markers.iter().find(|(k, _, _)| k.id == s.marker) else {
                    println!("     {}: not drawn", m.frame);
                    continue;
                };
                let near: Vec<String> = m
                    .blobs
                    .iter()
                    .enumerate()
                    .filter(|(_, o)| (o.x - k.u).hypot(o.y - k.v) < 150.0)
                    .map(|(j, o)| {
                        format!(
                            "{}#{} ({:+.1}, {:+.1}) s{:.1} f{:.0}",
                            if *b == Some(j) { "*" } else { "" },
                            if as_recorded { o.id } else { given[j] },
                            o.x - k.u,
                            o.y - k.v,
                            o.sigma,
                            o.flux
                        )
                    })
                    .collect();
                println!(
                    "     {} t{}: ({:.0}, {:.0}) s{:.1} v{:.0}: {}",
                    m.frame,
                    m.timestamp_us % 10_000_000 / 100,
                    k.u,
                    k.v,
                    k.sigma,
                    k.speed,
                    near.join(" | ")
                );
            }
        }
    }
}
