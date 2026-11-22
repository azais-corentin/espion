//! How stable the blobs' identities are: each drawn marker's blob should carry one ID
//! ([`espion_proto::cam::Blob::id`]) for as long as the page draws that marker, and no other
//! marker should ever carry it.
//!
//! [`Identities`] follows the check's matches frame by frame. A marker's **observation** is a
//! frame where it has a blob of its own. A blob two markers share (their light merged, so one
//! of them has the blob and the other, within its gate of it, has none) is neither's: it may
//! carry either ID, and both markers count as not seen in that frame. A marker the detector
//! split into several blobs (blobs of no marker within its gate) keeps its ID when any of
//! them carries it.

use std::collections::HashMap;

use super::check::{FrameMatch, Marker, Place, Seen, gate};

/// Identity figures since the scene started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdCounts {
    /// Markers observed at least once.
    pub markers: u64,
    /// Observations: a marker with a blob of its own in a frame.
    pub observations: u64,
    /// Distinct IDs each marker carried, summed over the markers: `markers` when every marker
    /// kept one.
    pub ids: u64,
    /// Observations whose ID is not the one of the marker's observation before.
    pub switches: u64,
    /// Of them, right after frames in which the marker was not observed.
    pub switches_after_gap: u64,
    /// Observations whose ID was last on another marker.
    pub transfers: u64,
    /// Times a marker was observed again after frames without (missed, merged with another,
    /// out of view or not drawn), and of them those where it came back with the ID it had.
    pub gaps: u64,
    pub gaps_kept: u64,
    /// Observations carrying the ID their marker carried most.
    pub main: u64,
}

impl IdCounts {
    /// IDs beyond the first per marker: how many times a marker's track broke into a new ID.
    pub const fn fragmentations(&self) -> u64 {
        self.ids - self.markers
    }

    /// Share of the observations that carry their marker's main ID, 0..=1 (1 with none).
    pub fn purity(&self) -> f64 {
        if self.observations == 0 {
            1.0
        } else {
            self.main as f64 / self.observations as f64
        }
    }
}

impl std::fmt::Display for IdCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let per = |n: u64| n as f64 / self.markers.max(1) as f64;
        write!(
            f,
            "{} markers, {} observations; switches {} ({:.2} per marker; {} after a gap), \
             fragmentations {} ({:.2} per marker), transfers {}, gaps kept {} of {}, purity \
             {:.2} %",
            self.markers,
            self.observations,
            self.switches,
            per(self.switches),
            self.switches_after_gap,
            self.fragmentations(),
            per(self.fragmentations()),
            self.transfers,
            self.gaps_kept,
            self.gaps,
            self.purity() * 100.0
        )
    }
}

/// A matched frame as one line of JSON, to record a scene and replay it through a tracker
/// (`tests/board.rs` `ids` writes them, `tests/ids.rs` `replay` reads them): the frame, its
/// blobs (`[id, x, y, sigma, flux]`) and its markers (`[id, u, v, sigma, speed, place,
/// blob]`; place 0 inside, 1 at the edge, 2 outside; blob -1: none).
pub fn to_line(m: &FrameMatch) -> String {
    let blobs: Vec<_> = m
        .blobs
        .iter()
        .map(|b| serde_json::json!([b.id, b.x, b.y, b.sigma, b.flux]))
        .collect();
    let markers: Vec<_> = m
        .markers
        .iter()
        .map(|(k, p, b)| {
            let place = match p {
                Place::Inside => 0,
                Place::Edge => 1,
                Place::Outside => 2,
            };
            let blob = b.map_or(-1, |j| j as i64);
            serde_json::json!([k.id, k.u, k.v, k.sigma, k.speed, place, blob])
        })
        .collect();
    serde_json::json!({
        "generation": m.generation,
        "frame": m.frame,
        "timestamp_us": m.timestamp_us,
        "period_us": m.period_us,
        "blobs": blobs,
        "markers": markers,
    })
    .to_string()
}

/// The matched frame of a line of [`to_line`] (its times on the viewer's clock are not
/// recorded: 0).
pub fn from_line(line: &str) -> Option<FrameMatch> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let blobs = v["blobs"]
        .as_array()?
        .iter()
        .map(|b| {
            Some(Seen {
                id: b[0].as_u64()? as u32,
                x: b[1].as_f64()?,
                y: b[2].as_f64()?,
                sigma: b[3].as_f64()?,
                flux: b[4].as_f64()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let markers = v["markers"]
        .as_array()?
        .iter()
        .map(|k| {
            let m = Marker {
                id: k[0].as_u64()? as u32,
                u: k[1].as_f64()?,
                v: k[2].as_f64()?,
                sigma: k[3].as_f64()?,
                speed: k[4].as_f64()?,
            };
            let place = match k[5].as_u64()? {
                0 => Place::Inside,
                1 => Place::Edge,
                _ => Place::Outside,
            };
            let blob = usize::try_from(k[6].as_i64()?).ok();
            Some((m, place, blob.filter(|&j| j < blobs.len())))
        })
        .collect::<Option<Vec<_>>>()?;
    let extra = (0..blobs.len())
        .filter(|j| !markers.iter().any(|(_, _, b)| *b == Some(*j)))
        .count();
    Some(FrameMatch {
        generation: v["generation"].as_u64()? as u32,
        frame: v["frame"].as_u64()? as u32,
        timestamp_us: v["timestamp_us"].as_u64()?,
        mid_us: 0.0,
        drawn_us: 0.0,
        markers,
        blobs,
        extra,
        period_us: v["period_us"].as_f64()?,
    })
}

#[derive(Debug, Default)]
struct MarkerIds {
    /// The ID of its last observation, and those of the other blobs it was split into then.
    last: u32,
    others: Vec<u32>,
    /// Not observed since, in a frame that drew it in view.
    gap: bool,
    /// The last frame ([`Identities::frames`]) that drew it in view.
    drawn: u64,
    /// Observations per ID.
    seen: HashMap<u32, u64>,
}

/// A marker whose ID changed, or took an ID another marker had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switch {
    /// The frame, and the marker.
    pub frame: u32,
    pub marker: u32,
    /// The ID it had (`None`: its first) and the one it has now.
    pub from: Option<u32>,
    pub to: u32,
    /// It was not observed in the frames between.
    pub after_gap: bool,
    /// The marker the new ID was last on, when another.
    pub taken_from: Option<u32>,
}

/// The identities of a scene's markers, from its matches in frame order.
#[derive(Debug, Default)]
pub struct Identities {
    markers: HashMap<u32, MarkerIds>,
    /// The marker each ID was last observed on.
    owner: HashMap<u32, u32>,
    counts: IdCounts,
    /// Frames added.
    frames: u64,
    /// Every switch, when asked for ([`Identities::logging`]).
    switches: Option<Vec<Switch>>,
}

impl Identities {
    /// As [`Identities::default`], keeping every switch for [`Identities::switches`].
    pub fn logging() -> Self {
        Self {
            switches: Some(Vec::new()),
            ..Self::default()
        }
    }

    /// The switches so far (none unless made by [`Identities::logging`]).
    pub fn switches(&self) -> &[Switch] {
        self.switches.as_deref().unwrap_or_default()
    }

    /// One matched frame; `id` gives the ID of the frame's blob `j`.
    pub fn add(&mut self, m: &FrameMatch, id: impl Fn(usize) -> u32) {
        let unmatched = |j: usize| !m.markers.iter().any(|(_, _, b)| *b == Some(j));
        self.frames += 1;
        let frame = self.frames;
        for (i, (marker, place, blob)) in m.markers.iter().enumerate() {
            if *place == Place::Outside {
                continue;
            }
            // Out of view or not drawn in the frames between is a gap too.
            if let Some(s) = self.markers.get_mut(&marker.id) {
                s.gap |= s.drawn + 1 < frame;
                s.drawn = frame;
            }
            // A blob another marker without one is within its gate of is shared.
            let own = blob.filter(|&j| {
                let b = &m.blobs[j];
                !m.markers.iter().enumerate().any(|(k, (o, p, ob))| {
                    k != i
                        && *p != Place::Outside
                        && ob.is_none()
                        && (b.x - o.u).hypot(b.y - o.v) <= gate(o, m.period_us)
                })
            });
            let Some(j) = own else {
                if let Some(s) = self.markers.get_mut(&marker.id) {
                    s.gap = true;
                }
                continue;
            };
            // Split into several blobs, the marker is the one that carries its ID.
            let last = self.markers.get(&marker.id).map(|s| s.last);
            let split = |k: usize| {
                let b = &m.blobs[k];
                k != j
                    && unmatched(k)
                    && (b.x - marker.u).hypot(b.y - marker.v) <= gate(marker, m.period_us)
            };
            let others: Vec<u32> = (0..m.blobs.len()).filter(|&k| split(k)).map(&id).collect();
            let id = match last {
                Some(l) if others.contains(&l) => l,
                _ => id(j),
            };
            let c = &mut self.counts;
            c.observations += 1;
            let (from, after_gap) = match self.markers.get_mut(&marker.id) {
                None => {
                    c.markers += 1;
                    c.ids += 1;
                    self.markers.insert(
                        marker.id,
                        MarkerIds {
                            last: id,
                            others,
                            gap: false,
                            drawn: frame,
                            seen: HashMap::from([(id, 1)]),
                        },
                    );
                    (None, false)
                }
                Some(s) => {
                    // Split in its last observation, it goes on as another of those blobs:
                    // that one was the marker.
                    if s.last != id && s.others.contains(&id) {
                        if let Some(n) = s.seen.get_mut(&s.last) {
                            *n -= 1;
                            if *n == 0 {
                                s.seen.remove(&s.last);
                                c.ids -= 1;
                            }
                        }
                        let n = s.seen.entry(id).or_insert(0);
                        c.ids += u64::from(*n == 0);
                        *n += 1;
                        s.last = id;
                    }
                    let was = (Some(s.last).filter(|l| *l != id), s.gap);
                    c.gaps += u64::from(s.gap);
                    c.gaps_kept += u64::from(s.gap && was.0.is_none());
                    c.switches += u64::from(was.0.is_some());
                    c.switches_after_gap += u64::from(was.0.is_some() && s.gap);
                    let n = s.seen.entry(id).or_insert(0);
                    c.ids += u64::from(*n == 0);
                    *n += 1;
                    s.last = id;
                    s.others = others;
                    s.gap = false;
                    was
                }
            };
            let taken_from = self.owner.insert(id, marker.id).filter(|o| *o != marker.id);
            c.transfers += u64::from(taken_from.is_some());
            if let Some(log) = &mut self.switches
                && (from.is_some() || taken_from.is_some())
            {
                log.push(Switch {
                    frame: m.frame,
                    marker: marker.id,
                    from,
                    to: id,
                    after_gap,
                    taken_from,
                });
            }
        }
    }

    /// The figures so far.
    pub fn counts(&self) -> IdCounts {
        IdCounts {
            main: self
                .markers
                .values()
                .map(|s| s.seen.values().copied().max().unwrap_or(0))
                .sum(),
            ..self.counts
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen_target::check::{Marker, Seen};

    /// A frame with markers at `(id, u)` on one row and blobs at `(u, blob id)`, each marker
    /// matched to the blob at its place.
    fn frame(markers: &[(u32, f64)], blobs: &[(f64, u32)]) -> FrameMatch {
        let seen: Vec<Seen> = blobs
            .iter()
            .map(|&(x, id)| Seen {
                id,
                x,
                y: 100.0,
                sigma: 2.0,
                flux: 1000.0,
            })
            .collect();
        let mut used = vec![false; seen.len()];
        let markers = markers
            .iter()
            .map(|&(id, u)| {
                let j = seen
                    .iter()
                    .position(|b| (b.x - u).abs() < 1.0)
                    .filter(|&j| !std::mem::replace(&mut used[j], true));
                let m = Marker {
                    id,
                    u,
                    v: 100.0,
                    sigma: 2.0,
                    speed: 0.0,
                };
                (m, Place::Inside, j)
            })
            .collect();
        FrameMatch {
            generation: 1,
            frame: 0,
            timestamp_us: 0,
            mid_us: 0.0,
            drawn_us: 0.0,
            markers,
            extra: 0,
            blobs: seen,
            period_us: 8333.0,
        }
    }

    fn run(frames: &[FrameMatch]) -> IdCounts {
        let mut ids = Identities::default();
        for f in frames {
            ids.add(f, |j| f.blobs[j].id);
        }
        ids.counts()
    }

    #[test]
    fn markers_that_keep_their_ids_count_nothing() {
        let f = frame(&[(1, 100.0), (2, 300.0)], &[(100.0, 7), (300.0, 9)]);
        let c = run(&[f.clone(), f.clone(), f]);
        assert_eq!(
            c,
            IdCounts {
                markers: 2,
                observations: 6,
                ids: 2,
                main: 6,
                ..IdCounts::default()
            }
        );
        assert_eq!(c.fragmentations(), 0);
        assert_eq!(c.purity(), 1.0);
    }

    #[test]
    fn ids_by_frame_index_switch_when_a_marker_goes() {
        // Three markers labelled by their index; the first one disappears: the others' labels
        // shift, and each takes a label another marker had.
        let a = frame(
            &[(1, 100.0), (2, 300.0), (3, 500.0)],
            &[(100.0, 1), (300.0, 2), (500.0, 3)],
        );
        let b = frame(&[(2, 300.0), (3, 500.0)], &[(300.0, 1), (500.0, 2)]);
        let c = run(&[a, b]);
        assert_eq!((c.switches, c.switches_after_gap), (2, 0));
        assert_eq!(c.transfers, 2);
        assert_eq!(c.fragmentations(), 2);
    }

    #[test]
    fn a_gap_is_kept_or_not() {
        let seen = frame(&[(1, 100.0)], &[(100.0, 4)]);
        let missed = frame(&[(1, 100.0)], &[]);
        let back = frame(&[(1, 100.0)], &[(100.0, 4)]);
        let other = frame(&[(1, 100.0)], &[(100.0, 5)]);
        let c = run(&[seen.clone(), missed.clone(), back, missed, other]);
        assert_eq!((c.gaps, c.gaps_kept), (2, 1));
        assert_eq!((c.switches, c.switches_after_gap), (1, 1));
        assert_eq!((c.markers, c.ids, c.observations, c.main), (1, 2, 3, 2));
    }

    #[test]
    fn a_blob_two_markers_share_is_neither_s() {
        // Markers 1 and 2 merge into one blob, which carries 2's ID while the check gives it
        // to marker 1; then they part with their own IDs.
        let apart = frame(&[(1, 100.0), (2, 140.0)], &[(100.0, 1), (140.0, 2)]);
        let mut merged = frame(&[(1, 118.0), (2, 122.0)], &[(120.0, 2)]);
        merged.markers[0].2 = Some(0);
        merged.markers[1].2 = None;
        let c = run(&[apart.clone(), merged, apart]);
        assert_eq!((c.switches, c.transfers), (0, 0));
        assert_eq!((c.gaps, c.gaps_kept), (2, 2));
        assert_eq!(c.observations, 4);
    }
}
