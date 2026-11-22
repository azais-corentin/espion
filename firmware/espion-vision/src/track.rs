//! Identities for the blobs across frames: a blob keeps its ID for as long as it is the same
//! marker, whatever the other blobs do. `core` only, no heap, integer arithmetic (the board
//! and the fake camera give the same IDs for the same blobs).
//!
//! [`Tracker::update`] takes one frame's measured blobs ([`Spot`]) with the frame's capture
//! time and gives each an ID. An ID is never 0 and is given once (the count wraps after
//! 2^32): a marker that comes back after its track ended is a new marker.
//!
//! How:
//!
//! - **Tracks.** One per marker in view: its ID, position, velocity and when it was last
//!   seen. Each frame a track is predicted to the frame's time at constant velocity.
//! - **Gate.** A blob can be a track's when it is within the track's gate of the prediction:
//!   [`Params::gate`], plus what a change of velocity moves it: [`Params::speed_gain`] of its
//!   own speed over the frame interval (a marker may stop or turn back between two frames),
//!   [`Params::speed_slack`] over the time since it was last seen, and
//!   [`Params::miss_gain`] of what the prediction missed last (a marker that turned is still
//!   turning). A track seen once has no velocity yet and has not moved since: its gate is
//!   [`Params::gate`] plus what [`Params::speed_new`] covers in the time since it was seen.
//!   No gate is wider than [`Params::gate_max`].
//! - **Confirmed tracks.** A track is confirmed once it was seen [`Params::confirm`] more
//!   times away from the blobs of the confirmed tracks. Until then it may be a second blob
//!   of a marker that has a track already (a marker in motion or at the detector's splitting
//!   limit comes out as two blobs in some frames): it gets an ID like any blob, but it does
//!   not coast for long, never hides and never takes a confirmed track's blob.
//! - **Association.** Nearest first: a track and a blob that are each other's nearest (within
//!   the gate, among those still free) are paired, again and again until none is left; the
//!   confirmed tracks first, then the others with the blobs left. A blob stays with the
//!   track that had it in the frame before unless another track is nearer by
//!   [`Params::stick`]: also when that track is not confirmed yet and the other one is a
//!   confirmed track that lost its blob (a marker that went dark while a new one lit up
//!   near it).
//! - **A paired track** moves to its blob; its velocity takes a share of what the prediction
//!   missed (an alpha-beta filter with alpha 1: the blobs are measured to a small fraction
//!   of a pixel, markers accelerate).
//! - **An unpaired track** coasts on its prediction, for [`Params::coast_us`] at most
//!   ([`Params::coast_new_us`] until it is confirmed), so a marker missed for a few frames
//!   comes back with its ID. It ends at once when the prediction leaves the frame: a marker
//!   that went out of view.
//! - **Merged markers.** A confirmed track without a blob whose prediction is within reach
//!   ([`Params::reach_widths`]) of a blob another track has is **hidden** in it: two markers
//!   whose light merged into one blob. Its clock does not run, for [`Params::hidden_us`] at
//!   most, and its prediction is kept inside the blob ([`Params::hold_widths`]). The track
//!   that has the blob follows it but keeps its velocity: the blob is the light of both,
//!   and their velocities are what tells them apart when they part. So two markers that
//!   cross come out with their IDs.
//! - **An unpaired blob** starts a track with the next ID.
//!
//! Frames may be missing (the detector skips or cuts some): the prediction and the gates go
//! by the capture times. A capture time that does not advance (the board restarted) ends
//! every track; so does [`Tracker::reset`] (another sensor window). The IDs go on.

use crate::refine::Spot;

/// Units per pixel of the positions ([`Spot::x`]).
const SUB: i32 = 256;
/// Distances are compared in 1/16 pixel.
const DIST_SHIFT: u32 = 4;
/// The widest gate there can be, pixels: its square in 1/16 pixel fits `u32` twice.
const GATE_LIMIT: u32 = 2047;
/// No blob, no track.
const NONE: u8 = u8::MAX;
/// Rounds of mutual-nearest pairing per pass; each pairs at least the nearest pair left, and
/// nearly always all of them in two or three.
const ROUNDS: usize = 8;

/// The tracker's constants. Lengths in pixels, speeds in pixels per second, times in µs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    /// Gate of a track at rest.
    pub gate: u16,
    /// Share of a track's own speed its velocity may change by from one frame to the next, in
    /// sixteenths (32: it may turn back at the same speed).
    pub speed_gain: u16,
    /// Speed change allowed to any track while it is not seen.
    pub speed_slack: u16,
    /// Share of the prediction's last miss (halved by every frame since) added to the gate,
    /// in sixteenths.
    pub miss_gain: u16,
    /// The fastest marker a track seen once is still paired with in the next frame.
    pub speed_new: u16,
    /// The widest gate.
    pub gate_max: u16,
    /// Sightings after the first, away from the confirmed tracks' blobs, that confirm a
    /// track.
    pub confirm: u8,
    /// How long a confirmed track coasts without a blob.
    pub coast_us: u32,
    /// How long a track not confirmed yet does.
    pub coast_new_us: u32,
    /// How long a track may stay hidden in another marker's blob.
    pub hidden_us: u32,
    /// A blob's reach: this many of its widths (sigma), in sixteenths, plus
    /// [`Params::gate`]. A track whose prediction is that near a blob another track has is
    /// hidden in it; a new blob that near a confirmed track's blob does not count towards
    /// its own track's confirmation.
    pub reach_widths: u16,
    /// A hidden track's prediction is kept within this many widths of the centre of the
    /// blob it is hidden in, in sixteenths.
    pub hold_widths: u16,
    /// How much nearer to a blob (sixteenths of a pixel) a track that had no blob in the
    /// frame before must be to take it from the track that had one.
    pub stick: u16,
    /// Smallest share of the prediction's miss the velocity takes, in 1/256 (the first
    /// sightings take more: all of it, half, a third, ...).
    pub beta: u16,
}

impl Params {
    /// The values the board and the fake camera use.
    pub const DEFAULT: Self = Self {
        gate: 6,
        speed_gain: 32,
        speed_slack: 400,
        miss_gain: 32,
        speed_new: 6000,
        gate_max: 160,
        confirm: 3,
        coast_us: 250_000,
        coast_new_us: 80_000,
        hidden_us: 5_000_000,
        reach_widths: 40,
        hold_widths: 16,
        stick: 32,
        beta: 128,
    };
}

#[derive(Clone, Copy, Debug)]
struct Track {
    /// 0: a free slot.
    id: u32,
    /// Position at the tracker's time, 1/256 px: its blob's when it has one, else predicted.
    x: i32,
    y: i32,
    /// Velocity, 1/256 px per second.
    vx: i32,
    vy: i32,
    /// Capture time of the last frame with its blob.
    seen_us: u64,
    /// Capture time of the last frame with its blob or with it hidden in another's.
    held_us: u64,
    /// Frames with its blob, saturating; of them, after the first, those away from the
    /// confirmed tracks' blobs.
    hits: u8,
    apart: u8,
    confirmed: bool,
    /// What the prediction missed its blob by, 1/16 px: the largest of the last miss and
    /// half the value before.
    miss: u32,
    /// This frame: the gate and what its distances are raised by (1/16 px), its nearest free
    /// blob, the blob paired, the blob it is hidden in.
    gate: u32,
    bias: u32,
    near: u8,
    spot: u8,
    host: u8,
    /// The ID of the track that had the blob this one was hidden in at the last frame (0: it
    /// was not hidden).
    under: u32,
}

impl Track {
    const FREE: Self = Self {
        id: 0,
        x: 0,
        y: 0,
        vx: 0,
        vy: 0,
        seen_us: 0,
        held_us: 0,
        hits: 0,
        apart: 0,
        confirmed: false,
        miss: 0,
        gate: 0,
        bias: 0,
        near: NONE,
        spot: NONE,
        host: NONE,
        under: 0,
    };
}

impl Track {
    /// The distance from the track's prediction to blob `s`, 1/16 px, if within its gate.
    fn distance(&self, s: &Spot) -> Option<u32> {
        let dx = ((s.x as i32 - self.x) >> DIST_SHIFT).unsigned_abs();
        let dy = ((s.y as i32 - self.y) >> DIST_SHIFT).unsigned_abs();
        if dx > self.gate || dy > self.gate {
            return None;
        }
        let d2 = dx * dx + dy * dy;
        (d2 <= self.gate * self.gate).then(|| d2.isqrt())
    }
}

/// A blob of the frame being paired.
#[derive(Clone, Copy, Debug)]
struct Seat {
    /// No track has it yet.
    free: bool,
    /// The ID of the track that has it.
    owner: u32,
    /// Its nearest free track this round, and that track's distance (raised by its bias).
    track: u8,
    dist: u32,
    /// A track is hidden in it; one of them was seen so lately that it still goes by its
    /// own velocity.
    hides: bool,
    crossing: bool,
    /// How near the nearest track that is not confirmed yet and had a blob in the frame
    /// before predicts it (`u32::MAX`: none within its gate).
    held: u32,
}

impl Seat {
    const FREE: Self = Self {
        free: true,
        owner: 0,
        track: NONE,
        dist: u32::MAX,
        hides: false,
        crossing: false,
        held: u32::MAX,
    };
}

/// `us` (at most a minute counts) as a multiplier of a per-second quantity, in 1/2^20.
fn span(us: u64) -> i64 {
    // 2^40 / 10^6, rounded.
    ((us.min(60_000_000) * 1_099_512) >> 20) as i64
}

/// `per_s` over a span ([`span`]).
fn over(per_s: i32, span: i64) -> i32 {
    ((i64::from(per_s) * span) >> 20).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// The length of `(x, y)` within 7 %, without a square root.
fn length(x: i32, y: i32) -> u32 {
    let (a, b) = (x.unsigned_abs(), y.unsigned_abs());
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    hi.saturating_add(lo / 8 * 3)
}

/// Whether `(x, y)` (1/256 px) is within the reach of blob `s`.
fn reaches(p: &Params, s: &Spot, x: i32, y: i32) -> bool {
    let r = (i64::from(s.width) * i64::from(p.reach_widths) / 16
        + i64::from(p.gate) * i64::from(SUB))
    .min(i64::from(p.gate_max) * i64::from(SUB));
    let (dx, dy) = (i64::from(s.x as i32 - x), i64::from(s.y as i32 - y));
    dx.abs() <= r && dy.abs() <= r && dx * dx + dy * dy <= r * r
}

/// What [`Tracker::update`] did, for the counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Update {
    /// Blobs that went on with a track's ID.
    pub kept: u16,
    /// Blobs that started a track.
    pub new: u16,
    /// Tracks that ended.
    pub ended: u16,
    /// Tracks alive after the frame (with a blob, coasting or hidden).
    pub tracks: u16,
}

/// Gives the blobs their identities, frame after frame, with at most `N` tracks (`N` at most
/// 254): the markers in view and those coasting or hidden. A frame's blobs beyond `N` get
/// IDs but no track. Lives in a static (`new` is `const`; about 64 bytes per track).
#[derive(Clone, Debug)]
pub struct Tracker<const N: usize> {
    params: Params,
    tracks: [Track; N],
    seats: [Seat; N],
    /// Capture time of the last frame.
    t_us: u64,
    /// The ID before the next one.
    last_id: u32,
}

/// The tracker of the board and of the fake camera: the 64 blobs a frame carries on the wire
/// and half as many tracks without a blob.
pub type DefaultTracker = Tracker<96>;

impl<const N: usize> Default for Tracker<N> {
    fn default() -> Self {
        Self::new(Params::DEFAULT)
    }
}

impl<const N: usize> Tracker<N> {
    /// A tracker without tracks; its first ID is 1.
    pub const fn new(params: Params) -> Self {
        assert!(N < NONE as usize);
        Self {
            params,
            tracks: [Track::FREE; N],
            seats: [Seat::FREE; N],
            t_us: 0,
            last_id: 0,
        }
    }

    /// Ends every track: what comes next is another scene (another sensor window). The IDs
    /// go on from where they were.
    pub fn reset(&mut self) {
        self.tracks = [Track::FREE; N];
    }

    /// The tracks alive.
    pub fn tracks(&self) -> usize {
        self.tracks.iter().filter(|t| t.id != 0).count()
    }

    /// The last ID given (0: none yet).
    pub const fn last_id(&self) -> u32 {
        self.last_id
    }

    fn next_id(&mut self) -> u32 {
        self.last_id = match self.last_id.wrapping_add(1) {
            0 => 1,
            id => id,
        };
        self.last_id
    }

    /// One frame of `width` x `height` pixels captured at `t_us`: `ids[i]` becomes the ID of
    /// `spots[i]` (as many as both hold). Every ID of a frame is different and none is 0.
    pub fn update(
        &mut self,
        t_us: u64,
        width: u16,
        height: u16,
        spots: &[Spot],
        ids: &mut [u32],
    ) -> Update {
        let n = spots.len().min(ids.len());
        let (spots, ids) = (&spots[..n], &mut ids[..n]);
        ids.fill(0);
        if t_us <= self.t_us {
            self.reset();
        }
        // The blobs beyond the tracker's capacity are not tracked.
        let tracked = &spots[..n.min(N)];
        self.seats[..tracked.len()].fill(Seat::FREE);
        self.predict(t_us);
        self.hold(tracked);
        self.pair(tracked, true);
        self.pair(tracked, false);
        self.keep(tracked);
        self.hide(t_us, tracked);
        let mut out = Update::default();
        self.follow(t_us, tracked, ids, &mut out);
        self.end(t_us, width, height, tracked, &mut out);
        // The blobs left start tracks.
        for (j, s) in spots.iter().enumerate() {
            if ids[j] != 0 {
                continue;
            }
            let id = self.next_id();
            ids[j] = id;
            out.new += 1;
            if let Some(slot) = self.free_slot() {
                out.ended += u16::from(self.tracks[slot].id != 0);
                self.tracks[slot] = Track {
                    id,
                    x: s.x as i32,
                    y: s.y as i32,
                    seen_us: t_us,
                    held_us: t_us,
                    hits: 1,
                    // Paired, for `free_slot`: this frame's tracks are not given away.
                    spot: j.min(usize::from(NONE) - 1) as u8,
                    ..Track::FREE
                };
            }
        }
        self.t_us = t_us;
        out.tracks = self.tracks() as u16;
        out
    }

    /// Moves every track to its prediction at `t_us` and opens its gate.
    fn predict(&mut self, t_us: u64) {
        let p = self.params;
        let frame = span(t_us - self.t_us.min(t_us));
        let gate_max = u32::from(p.gate_max).min(GATE_LIMIT) << DIST_SHIFT;
        for t in self.tracks.iter_mut().filter(|t| t.id != 0) {
            t.x = t.x.saturating_add(over(t.vx, frame));
            t.y = t.y.saturating_add(over(t.vy, frame));
            let (since, late) = if t.seen_us == self.t_us {
                (frame, false)
            } else {
                (span(t_us - t.seen_us), true)
            };
            // A track seen once has not moved since: the marker may be anywhere it can have
            // gone to in the time. One with a velocity is where it was going, but for what
            // its velocity changed by.
            let reach = if t.hits < 2 {
                over(i32::from(p.speed_new) << DIST_SHIFT, since)
            } else {
                let v = (length(t.vx, t.vy) / SUB as u32).min(1 << 20) as i32;
                let own = v * i32::from(p.speed_gain) / 16;
                over(own << DIST_SHIFT, frame)
                    .saturating_add(over(i32::from(p.speed_slack) << DIST_SHIFT, since))
            }
            .max(0) as u32;
            let reach = reach.saturating_add(t.miss.saturating_mul(u32::from(p.miss_gain)) / 16);
            t.gate = (u32::from(p.gate) << DIST_SHIFT)
                .saturating_add(reach)
                .min(gate_max);
            // A track that lost its blob does not take another track's unless it is clearly
            // nearer; one hidden in a blob has its place in it and takes what is nearest.
            t.bias = if late && t.under == 0 {
                u32::from(p.stick)
            } else {
                0
            };
            (t.near, t.spot, t.host) = (NONE, NONE, NONE);
        }
    }

    /// Notes, for each blob, how near a track that is not confirmed yet and had a blob in the
    /// frame before predicts it: a new marker's track, which a confirmed track that lost its
    /// own blob (and looks for it in a gate that widens with time) must not take the blob
    /// from, though the confirmed tracks pair first.
    fn hold(&mut self, spots: &[Spot]) {
        for t in self.tracks.iter().filter(|t| t.id != 0 && !t.confirmed) {
            if t.seen_us != self.t_us {
                continue;
            }
            for (s, seat) in spots.iter().zip(&mut self.seats) {
                if let Some(d) = t.distance(s) {
                    seat.held = seat.held.min(d);
                }
            }
        }
    }

    /// Pairs the free tracks that are confirmed (or, with `confirmed` false, that are not)
    /// with the free blobs, nearest first. A track that lost its blob takes one only if it is
    /// nearer to it, by [`Params::stick`], than the track that had it in the frame before.
    fn pair(&mut self, spots: &[Spot], confirmed: bool) {
        for _ in 0..ROUNDS {
            for s in &mut self.seats[..spots.len()] {
                (s.track, s.dist) = (NONE, u32::MAX);
            }
            for (i, t) in self.tracks.iter_mut().enumerate() {
                t.near = NONE;
                if t.id == 0 || t.spot != NONE || t.confirmed != confirmed {
                    continue;
                }
                let mut best = u32::MAX;
                for (j, (s, seat)) in spots.iter().zip(&mut self.seats).enumerate() {
                    if !seat.free {
                        continue;
                    }
                    let Some(d) = t.distance(s) else {
                        continue;
                    };
                    let d = d + t.bias;
                    if t.bias != 0 && d >= seat.held {
                        continue;
                    }
                    if d < best {
                        best = d;
                        t.near = j as u8;
                    }
                    if d < seat.dist {
                        (seat.track, seat.dist) = (i as u8, d);
                    }
                }
            }
            let mut any = false;
            for (i, t) in self.tracks.iter_mut().enumerate() {
                if t.near == NONE {
                    continue;
                }
                let seat = &mut self.seats[usize::from(t.near)];
                if usize::from(seat.track) == i {
                    t.spot = t.near;
                    (seat.free, seat.owner) = (false, t.id);
                    any = true;
                }
            }
            if !any {
                break;
            }
        }
    }

    /// Leaves a merged blob with the track that had it: a track that was hidden in another
    /// track's blob and took a blob that the other one, left without, is within reach of
    /// gives it back and stays hidden.
    fn keep(&mut self, spots: &[Spot]) {
        let p = self.params;
        for i in 0..N {
            let h = self.tracks[i];
            if h.id == 0 || h.under == 0 || h.spot == NONE {
                continue;
            }
            let Some(o) = self.tracks.iter().position(|o| o.id == h.under) else {
                continue;
            };
            let owner = self.tracks[o];
            if owner.spot == NONE && reaches(&p, &spots[usize::from(h.spot)], owner.x, owner.y) {
                self.tracks[o].spot = h.spot;
                self.tracks[i].spot = NONE;
                self.seats[usize::from(h.spot)].owner = owner.id;
            }
        }
    }

    /// Hides the confirmed tracks without a blob whose prediction is within reach of a blob
    /// another track has.
    fn hide(&mut self, t_us: u64, spots: &[Spot]) {
        let p = self.params;
        for t in &mut self.tracks {
            t.under = 0;
            let unseen = t_us - t.seen_us;
            if t.id == 0 || t.spot != NONE || !t.confirmed || unseen > u64::from(p.hidden_us) {
                continue;
            }
            let host = spots
                .iter()
                .zip(&self.seats)
                .enumerate()
                .filter(|(_, (s, seat))| !seat.free && reaches(&p, s, t.x, t.y))
                .min_by_key(|(_, (s, _))| {
                    let (dx, dy) = (i64::from(t.x - s.x as i32), i64::from(t.y - s.y as i32));
                    dx * dx + dy * dy
                });
            if let Some((j, _)) = host {
                let seat = &mut self.seats[j];
                (t.host, t.under) = (j as u8, seat.owner);
                seat.hides = true;
                seat.crossing |= unseen <= u64::from(p.coast_us);
            }
        }
    }

    /// Moves the paired tracks to their blobs and gives the blobs their IDs.
    fn follow(&mut self, t_us: u64, spots: &[Spot], ids: &mut [u32], out: &mut Update) {
        let p = self.params;
        let limit = i32::from(p.speed_new).saturating_mul(4 * SUB);
        for i in 0..N {
            let t = self.tracks[i];
            if t.id == 0 || t.spot == NONE {
                continue;
            }
            let j = usize::from(t.spot);
            let (sx, sy) = (spots[j].x as i32, spots[j].y as i32);
            ids[j] = t.id;
            out.kept += 1;
            // A blob within reach of the blob of a confirmed or older track may be that
            // marker's. The tracks confirmed in this frame are marked after this pass
            // (`end`).
            let apart = t.confirmed
                || !self.tracks.iter().any(|o| {
                    o.id != 0
                        && o.spot != NONE
                        && (o.confirmed || o.id < t.id)
                        && reaches(&p, &spots[usize::from(o.spot)], sx, sy)
                });
            // The velocity takes all of the miss at the second sighting, then half, a
            // third, ... down to beta; none from a blob in which another marker is
            // crossing this one: it is the light of both, and their velocities are what
            // tells them apart when they part.
            let seat = self.seats[j];
            let share = if seat.crossing {
                0
            } else {
                (256 / u32::from(t.hits)).max(u32::from(p.beta))
            };
            // In 1/256 per second per unit of miss, times 256.
            let since = (t_us - t.seen_us).clamp(1, u64::from(u32::MAX)) as u32;
            let rate = i64::from(share * 1_000_000 / since);
            let gain = |miss: i32| ((i64::from(miss) * rate) >> 8) as i32;
            let miss = (sx - t.x, sy - t.y);
            let t = &mut self.tracks[i];
            t.miss = (length(miss.0, miss.1) >> DIST_SHIFT).max(t.miss / 2);
            t.vx = t.vx.saturating_add(gain(miss.0)).clamp(-limit, limit);
            t.vy = t.vy.saturating_add(gain(miss.1)).clamp(-limit, limit);
            (t.x, t.y) = (sx, sy);
            (t.seen_us, t.held_us) = (t_us, t_us);
            t.apart = t.apart.saturating_add(u8::from(apart));
            t.hits = t.hits.saturating_add(1);
            // The tracks hidden in its blob for longer than a velocity is believed go
            // with the blob: they keep their place in it.
            if seat.hides {
                let v = (t.vx, t.vy);
                for h in &mut self.tracks {
                    let stale = t_us - h.seen_us > u64::from(p.coast_us);
                    if h.id != 0 && usize::from(h.host) == j && stale {
                        h.x = h.x.saturating_add(miss.0);
                        h.y = h.y.saturating_add(miss.1);
                        (h.vx, h.vy) = v;
                    }
                }
            }
        }
    }

    /// Confirms the paired tracks seen apart often enough and keeps the hidden ones inside
    /// their blobs; the others coast or end.
    fn end(&mut self, t_us: u64, width: u16, height: u16, spots: &[Spot], out: &mut Update) {
        let p = self.params;
        let (w, h) = (i32::from(width) * SUB, i32::from(height) * SUB);
        for t in self.tracks.iter_mut().filter(|t| t.id != 0) {
            if t.spot != NONE {
                t.confirmed = t.apart >= p.confirm;
                continue;
            }
            if t.host != NONE {
                let s = &spots[usize::from(t.host)];
                let hold = i64::from(s.width) * i64::from(p.hold_widths) / 16;
                let (dx, dy) = (i64::from(t.x - s.x as i32), i64::from(t.y - s.y as i32));
                let d = ((dx * dx + dy * dy) as u64).isqrt() as i64;
                if d > hold {
                    t.x = s.x as i32 + (dx * hold / d) as i32;
                    t.y = s.y as i32 + (dy * hold / d) as i32;
                }
                t.held_us = t_us;
                continue;
            }
            let inside = t.x >= 0 && t.y >= 0 && t.x < w && t.y < h;
            let coast = if t.confirmed {
                p.coast_us
            } else {
                p.coast_new_us
            };
            if !inside || t_us - t.held_us > u64::from(coast) {
                t.id = 0;
                out.ended += 1;
            }
        }
    }

    /// A slot for a new track: a free one, else the one of a track without a blob in this
    /// frame, one not confirmed before a confirmed one, the longest unseen first; none when
    /// every track has a blob.
    fn free_slot(&self) -> Option<usize> {
        if let Some(i) = self.tracks.iter().position(|t| t.id == 0) {
            return Some(i);
        }
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.spot == NONE)
            .min_by_key(|(_, t)| (t.confirmed, t.seen_us))
            .map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests;
