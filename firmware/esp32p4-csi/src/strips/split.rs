//! The strip path split over the two HP cores: [`CsiStrips::split`] turns the driver into a
//! [`StripControl`] that stays on the core that created it (core 0) and a [`StripReader`] that
//! moves once to a core running bare (core 1: no scheduler, no interrupts, no critical
//! section).
//!
//! **Not run on the board yet.** Everything below is from the code, the TRM and the
//! single-core measurements; the board checks are listed in the crate README.
//!
//! # Who does what
//!
//! - **Control (core 0):** `start`, `stop`, `set_config`, counters, and the DMA error and stall
//!   interrupt with the ISP and bridge restart ([`StripControl::service`]). The per-strip
//!   `BLOCK_TFR_DONE` interrupt is masked at the split (the items still ask for it with
//!   `IOC_BLKTFR`, which also governs the item write-back the reader polls [INFERENCE: not
//!   checked with it cleared, so it stays set]). The CSI host and bridge error interrupts stay.
//! - **Reader (core 1):** [`StripReader::poll`] finds the next finished block in ring order,
//!   classifies it as the interrupt path does (frame boundaries, probation, damage), makes the
//!   item valid again at once, invalidates the strip's cache lines through `esp32p4-cache` and
//!   hands out a [`StripRef`]. [`StripRef::release`] says whether the bytes stayed intact.
//!
//! # Completion detection
//!
//! The reader polls `LLP_STATUS[62]` (`BLOCK_TFR_DONE`, bit 30 of the item's word 13) through
//! the non-cacheable alias of the item, then reads the completed size (word 12). The DMA writes
//! that bit last in its write-back (TRM figure 7.5-6), and the reader clears it when it makes
//! the item valid again, before the DMA can use the item once more. So:
//! - **no miss:** a set bit stays set until the reader itself clears it, and the reader looks
//!   only at the slot it expects next, in ring order, which is the order the DMA follows;
//! - **no double report:** the reader clears the bit before setting `VALID`, and the DMA sets
//!   it again only after writing the item's next block.
//!
//! The channel's current-item register (`LLP0`) or destination address (`DAR0`) were not
//! used: they say where the DMA is now, not which blocks finished since the last look, so a
//! slow reader could not tell one lap from two, and they are channel registers that only
//! core 0 touches.
//!
//! # A reader that falls behind
//!
//! The reader gives every item back to the DMA when it takes the block, not when the
//! consumer is done with the strip. So a slow consumer never halts the DMA as long as it stays
//! less than a ring behind; instead the DMA may come round and write over a strip the
//! consumer is still reading (the reader was *lapped*). [`StripRef::release`] detects that:
//!
//! - The DMA writes strip `k` again only after it finished the item before it, `k - 1`, once
//!   more. The reader cleared `k - 1`'s done bit when it took that block, before it took `k`.
//!   So `k - 1` showing done again when `k` is released means the DMA reached `k`.
//! - \[INFERENCE\] This assumes the write-back of `k - 1` is visible before the DMA's first data
//!   write into `k`. The TRM says data writes win over the write-back inside a channel, but
//!   not whether the next block's data can overtake the previous block's write-back; a block
//!   lasts about 0.6 ms at 1280x800, 120 fps.
//!
//! On a lapped strip `release` returns `false`, counts [`StripControl::strips_lapped`] and the
//! frame as truncated, and the reader delivers nothing more until the next frame starts. The
//! block stream itself stayed aligned (the DMA never stopped), so no probation frame is needed:
//! the remaining blocks of the frame are tracked and checked without being delivered, exactly
//! as in the probation state.
//!
//! A reader more than a ring behind (one that does not poll for a whole ring) still halts the
//! DMA: it reaches an item the reader has not taken yet, which is still invalid. Core 0 then
//! counts a stall and, once the reader has taken every finished block (every item valid),
//! restarts the ISP and the bridge and resumes the channel, as the interrupt path does.
//!
//! # Stop and restart while the reader polls
//!
//! The control half keeps a run number `run`: odd while running, even while stopped, raised by
//! one at every start and stop and never reused. The reader marks itself `busy` for the whole
//! of a poll, and every poll starts by checking the run number and the instance number:
//!
//! 1. Reader: `busy = true` (SeqCst), then load `run` (SeqCst). Not its run: reload the
//!    geometry (a new start) or do nothing (stopped). The poll writes list items only for the
//!    run it saw.
//! 2. Control, `stop`: `run` even (SeqCst) first, then the bridge and the channel stop. A poll
//!    already under way may still give an item back to the stopped channel, which is harmless.
//! 3. Control, `start`: waits until `busy` reads `false` (SeqCst; at most 10 ms, else
//!    [`StripError::ReaderBusy`] and nothing is touched), then rewrites every item, publishes
//!    the geometry and the new odd `run`, and only then enables the channel.
//!
//! Both sides store first and load second, all SeqCst, so either `start` sees `busy` (and
//! waits) or the poll sees the even or new `run` (and leaves the items alone). The reader
//! therefore never writes an item while `start` writes them.
//!
//! No stale strip passes as fresh: [`StripRef::release`] checks the run number again after the
//! consumer's reads. Every DMA write of a later run follows a change of `run` (the stop that
//! precedes any start), and `run` never returns to an old value, so an unchanged `run` means no
//! later run wrote the strip. A poll that sees `run` change before it returns drops the strip.
//!
//! # Rules for the reader's core
//!
//! Every function the reader reaches is free of `critical_section`, logging, esp-hal driver
//! calls and interrupt masking other than the cache lock's own: atomics, volatile reads and
//! writes of the list items through the non-cacheable alias, a read of the bridge's raw
//! interrupt status, `mcycle`, and `esp32p4-cache` (whose lock masks the calling core's
//! interrupts only while it holds the cache-sync registers). esp-hal's `critical_section` on
//! this chip is one lock shared by both cores; a core that panics or is parked while holding it
//! freezes the other at its next critical section, so the bare core must never take it.

use core::{
    hint::spin_loop,
    ops::Deref,
    ptr,
    sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering, fence},
};

use super::{
    CTL1_LLI_VALID, CsiStrips, Geom, LLI_CTL1, LLI_INVALID, MAX_STRIPS, Meta, STATE, State,
    StripDebug, StripError, StripStats, Sync, Tracker, cycles, link_done, link_items, link_word,
    rearm_link,
};
use crate::{
    BRIDGE_ERR_MASK, Config, DMA_ERR_MASK, DMA_TFR_DONE, PhyStatus, bridge, cache, channel_enabled,
    dma,
};

/// How long `start` and the drop wait for the reader to finish a poll: 10 ms at 400 MHz. A
/// poll takes a few microseconds plus at most one wait for the cache lock.
const READER_WAIT_CYCLES: u32 = 4_000_000;

/// Words of the reader's published counters (see [`stat_words`]).
const STAT_WORDS: usize = 13;

/// What the two halves share: atomics only. Each field names its one writer.
struct Shared {
    /// Writer: control ([`CsiStrips::split`] sets it, the drop of the control half clears it).
    /// Odd while a split driver exists, 0 otherwise. The reader acts only while it equals its
    /// own number.
    instance: AtomicU32,
    /// Writer: control. Odd while running, even while stopped; +1 at every start and stop.
    run: AtomicU32,
    /// Writer: reader. `true` for the whole of [`StripReader::poll`], the only place it
    /// writes list items.
    busy: AtomicBool,
    /// Writers: the core-0 interrupt handlers (`fetch_add`): data loss events (CSI host and
    /// bridge errors, DMA errors and stalls). The reader compares it per block.
    loss: AtomicU32,
    /// Writer: reader. `run << 8 | slot`: the slot whose block the reader expects next, in the
    /// run it last saw. Stored before the item it just took becomes valid again.
    next: AtomicU32,
    /// Writer: reader. Its classification is in the delivering state.
    synced: AtomicBool,
    /// Writer: control, in `start` before `run` turns odd, while the reader cannot be in a
    /// poll. The run's geometry; the reader loads it when it sees the new `run`.
    count: AtomicU32,
    line_bytes: AtomicU32,
    height: AtomicU32,
    lines: AtomicU32,
    strips: [AtomicUsize; MAX_STRIPS],
    links: [AtomicUsize; MAX_STRIPS],
    /// Writer: control ([`StripControl::reset_stats`]): the reader zeroes its counters when it
    /// changes.
    stats_reset: AtomicU32,
    /// Writer: reader. Sequence lock over `stats`: odd while it writes them.
    stats_seq: AtomicU32,
    /// Writer: reader. Its block and frame counters ([`stat_words`]).
    stats: [AtomicU32; STAT_WORDS],
}

static SHARED: Shared = Shared {
    instance: AtomicU32::new(0),
    run: AtomicU32::new(0),
    busy: AtomicBool::new(false),
    loss: AtomicU32::new(0),
    next: AtomicU32::new(0),
    synced: AtomicBool::new(false),
    count: AtomicU32::new(0),
    line_bytes: AtomicU32::new(0),
    height: AtomicU32::new(0),
    lines: AtomicU32::new(0),
    strips: [const { AtomicUsize::new(0) }; MAX_STRIPS],
    links: [const { AtomicUsize::new(0) }; MAX_STRIPS],
    stats_reset: AtomicU32::new(0),
    stats_seq: AtomicU32::new(0),
    stats: [const { AtomicU32::new(0) }; STAT_WORDS],
};

/// `SHARED.next` for slot `slot` in run `run` (the run's low 24 bits).
fn pack(run: u32, slot: u8) -> u32 {
    run << 8 | slot as u32
}

/// Array index of a slot, masked so that no bounds check (and so no panic path) is generated.
fn idx(slot: u8) -> usize {
    slot as usize % MAX_STRIPS
}

/// The item at `link` is valid: the DMA may use it.
fn link_valid(link: usize) -> bool {
    // SAFETY: a read of one word of a list item; reading changes nothing.
    unsafe { ptr::read_volatile(link_word(link, LLI_CTL1)) & CTL1_LLI_VALID != 0 }
}

/// The reader's counters, in the order of `SHARED.stats`; `lapped` is the reader's own count
/// of lapped strips (not in [`StripStats`], whose size the firmware crates' stack-frame
/// limit depends on).
fn stat_words(st: &StripStats, lapped: u32) -> [u32; STAT_WORDS] {
    [
        st.strips_done,
        st.strips_delivered,
        st.strips_discarded,
        lapped,
        st.frames_done,
        st.frames_complete,
        st.frames_truncated,
        st.frames_skipped,
        st.length_errors,
        st.last_block_items,
        st.invalidate_cycles_total as u32,
        (st.invalidate_cycles_total >> 32) as u32,
        st.invalidate_cycles_max,
    ]
}

// ── Core-0 side, called from strips.rs ──────────────────────────────────────────────────

/// A loss event for the reader (any mode; nobody reads it without a split).
pub(super) fn note_loss() {
    SHARED.loss.fetch_add(1, Ordering::Release);
}

/// Waits until the reader is outside a poll, at most [`READER_WAIT_CYCLES`]. Returns `false`
/// on timeout.
pub(super) fn wait_reader() -> bool {
    let t0 = cycles();
    while SHARED.busy.load(Ordering::SeqCst) {
        if cycles().wrapping_sub(t0) > READER_WAIT_CYCLES {
            return false;
        }
        spin_loop();
    }
    true
}

/// Publishes the geometry of the run `start` is beginning and then the new odd run number.
/// Called with every item written and before the channel is enabled.
pub(super) fn publish_start(s: &State) {
    SHARED.count.store(s.count as u32, Ordering::Relaxed);
    SHARED
        .line_bytes
        .store(s.geom.line_bytes, Ordering::Relaxed);
    SHARED.height.store(s.geom.height as u32, Ordering::Relaxed);
    SHARED.lines.store(s.geom.lines as u32, Ordering::Relaxed);
    for i in 0..MAX_STRIPS {
        SHARED.strips[i].store(s.strips[i], Ordering::Relaxed);
        SHARED.links[i].store(s.links[i], Ordering::Relaxed);
    }
    let run = SHARED.run.load(Ordering::Relaxed);
    let run = if run & 1 == 0 { run + 1 } else { run + 2 };
    SHARED.run.store(run, Ordering::SeqCst);
}

/// Makes the run number even: the reader stops writing items and every strip it handed out
/// fails its release check.
pub(super) fn publish_stop() {
    let run = SHARED.run.load(Ordering::Relaxed);
    if run & 1 != 0 {
        SHARED.run.store(run + 1, Ordering::SeqCst);
    }
}

/// The drop of the control half: the reader becomes inert. A poll under way is waited for
/// (bounded; a reader stuck in a poll for 10 ms is dead).
pub(super) fn detach() {
    SHARED.instance.store(0, Ordering::SeqCst);
    let _ = wait_reader();
}

/// The reader's last published counters (sequence lock, bounded: a reader stopped in the
/// middle of a publication leaves the sequence odd, and then the last read is returned).
fn reader_words() -> [u32; STAT_WORDS] {
    let mut w = [0u32; STAT_WORDS];
    for _ in 0..100 {
        let s1 = SHARED.stats_seq.load(Ordering::Acquire);
        for (v, a) in w.iter_mut().zip(&SHARED.stats) {
            *v = a.load(Ordering::Relaxed);
        }
        fence(Ordering::Acquire);
        if s1 & 1 == 0 && SHARED.stats_seq.load(Ordering::Relaxed) == s1 {
            break;
        }
        spin_loop();
    }
    w
}

/// Overwrites the block and frame counters of `st` with the reader's last published ones.
pub(super) fn reader_stats(st: &mut StripStats) {
    let w = reader_words();
    st.strips_done = w[0];
    st.strips_delivered = w[1];
    st.strips_discarded = w[2];
    st.frames_done = w[4];
    st.frames_complete = w[5];
    st.frames_truncated = w[6];
    st.frames_skipped = w[7];
    st.length_errors = w[8];
    st.last_block_items = w[9];
    st.invalidate_cycles_total = w[10] as u64 | (w[11] as u64) << 32;
    st.invalidate_cycles_max = w[12];
}

/// Asks the reader to zero its counters at its next poll.
pub(super) fn request_stats_reset() {
    SHARED.stats_reset.fetch_add(1, Ordering::Release);
}

/// The ring as the reader and the DMA see it, for [`CsiStrips::debug_status`].
pub(super) fn debug(s: &State, d: &mut StripDebug) {
    d.dma_next = (SHARED.next.load(Ordering::Acquire) & 0xFF) as u8;
    d.armed = 0;
    for slot in 0..s.count {
        if link_valid(s.links[idx(slot)]) {
            d.armed |= 1 << slot;
        }
    }
    d.held = 0;
    d.queued = 0;
    d.synced = SHARED.synced.load(Ordering::Relaxed);
}

impl State {
    /// The DMA interrupt after the split: errors and stalls only.
    pub(super) fn split_event(&mut self, st: u32) {
        if st & LLI_INVALID != 0 {
            self.stats.dma_stalls = self.stats.dma_stalls.wrapping_add(1);
            note_loss();
            self.halted = true;
        } else if st & (DMA_ERR_MASK | DMA_TFR_DONE) != 0 && !channel_enabled(self.channel) {
            // Stopped on an error (or ran off the list).
            note_loss();
            self.halted = true;
        }
        self.kick_split();
    }

    /// [`State::kick`] after the split. The channel halts on the first item the reader has
    /// not taken back yet; once every item is valid again, the reader has taken every
    /// finished block, so the item the channel waits on is valid and the reader waits on the
    /// same slot (`SHARED.next`), which is where a channel stopped on an error restarts.
    fn kick_split(&mut self) {
        if !self.halted || !self.running {
            return;
        }
        for slot in 0..self.count {
            if !link_valid(self.links[idx(slot)]) {
                return;
            }
        }
        // The reader stores `next` before it makes an item valid: once every item reads
        // valid, `next` is the slot after the last block it took.
        fence(Ordering::SeqCst);
        let next = SHARED.next.load(Ordering::Acquire);
        let run = SHARED.run.load(Ordering::Relaxed);
        let slot = (next & 0xFF) as u8;
        if next >> 8 != run & 0x00FF_FFFF || slot >= self.count {
            // The reader has not taken up this run yet.
            return;
        }
        self.restart_channel(slot);
    }
}

impl Tracker {
    /// The consumer's strip `meta` was written over by the DMA before its release. The block
    /// stream stayed aligned, so the rest of the frame is tracked without delivery (the
    /// probation state does exactly that) and delivery resumes at the next frame start.
    fn lapped(&mut self, meta: &Meta, st: &mut StripStats) {
        st.frames_truncated = st.frames_truncated.wrapping_add(1);
        if meta.last {
            // The frame was counted complete and has ended already.
            st.frames_complete = st.frames_complete.wrapping_sub(1);
        } else {
            // Counted truncated now; its frame end then counts nothing more.
            self.partial = true;
            self.sync = Sync::Probation;
        }
    }
}

// ── Control half ────────────────────────────────────────────────────────────────────────

impl<'d> CsiStrips<'d> {
    /// Splits the driver into a control half for this core and a reader for a bare second
    /// core (see the [`split`](self) module docs). Stops capture first if it runs. Fails with
    /// [`StripError::StripsHeld`], handing the driver back, while a [`super::Strip`] from
    /// [`CsiStrips::wait_strip`] is still held.
    ///
    /// The reader's block and frame counters continue from the driver's.
    pub fn split(mut self) -> Result<(StripControl<'d>, StripReader), (Self, StripError)> {
        let held = critical_section::with(|cs| STATE.borrow_ref(cs).held);
        if held != 0 {
            return Err((self, StripError::StripsHeld));
        }
        if self.running {
            self.stop();
        }
        let stats = critical_section::with(|cs| {
            let mut s = STATE.borrow_ref_mut(cs);
            s.split = true;
            s.q_head = 0;
            s.q_len = 0;
            s.stats
        });
        // No interrupt per strip any more: the reader polls. Errors and stalls still signal.
        dma()
            .ch(self.channel as usize)
            .intsignal_enable0()
            .write(|w| unsafe { w.bits(DMA_TFR_DONE | DMA_ERR_MASK) });
        self.split = true;
        let instance = self.generation.wrapping_mul(2) | 1;
        let reader = StripReader {
            instance,
            run: SHARED.run.load(Ordering::Relaxed),
            count: 0,
            next: 0,
            geom: Geom::ZERO,
            strips: [0; MAX_STRIPS],
            links: [0; MAX_STRIPS],
            track: Tracker::NEW,
            loss_seen: 0,
            stats,
            stats_reset_seen: SHARED.stats_reset.load(Ordering::Relaxed),
            lapped: 0,
        };
        // Still on this core: the reader has not moved yet, so it is the only writer.
        reader.publish_stats();
        SHARED.synced.store(false, Ordering::Relaxed);
        SHARED.instance.store(instance, Ordering::SeqCst);
        Ok((StripControl { csi: self }, reader))
    }
}

/// The half of a split [`CsiStrips`] that stays on the core that created it: start, stop,
/// configuration, counters, and the DMA error and stall handling. Dropping it stops capture,
/// makes the reader inert and frees the hardware.
pub struct StripControl<'d> {
    csi: CsiStrips<'d>,
}

impl StripControl<'_> {
    /// Current configuration.
    pub fn config(&self) -> &Config {
        self.csi.config()
    }

    /// Rows per strip.
    pub fn strip_lines(&self) -> u16 {
        self.csi.strip_lines()
    }

    /// As [`CsiStrips::set_config`]: only while stopped. The reader takes the new geometry at
    /// the next start.
    pub fn set_config(&mut self, config: Config, lines: u16) -> Result<(), StripError> {
        self.csi.set_config(config, lines)
    }

    /// As [`CsiStrips::start`], with the reader's handshake (see "Stop and restart" in the
    /// module docs): waits up to 10 ms for a poll under way to end, else
    /// [`StripError::ReaderBusy`]. A strip the reader still holds from the previous run fails
    /// its release check; it does not block the start. The first two frames are not
    /// delivered.
    pub fn start(&mut self) -> Result<(), StripError> {
        self.csi.start()
    }

    /// As [`CsiStrips::stop`]. Strips the reader handed out fail their release check.
    pub fn stop(&mut self) {
        self.csi.stop()
    }

    /// `true` between [`StripControl::start`] and [`StripControl::stop`].
    pub fn is_running(&self) -> bool {
        self.csi.is_running()
    }

    /// Restarts a halted channel once the reader has taken every finished block (stall or DMA
    /// error; the interrupt tries it once itself). Call it about every millisecond from a task
    /// on this core while running. Returns `true` while the channel is still halted.
    pub fn service(&mut self) -> bool {
        critical_section::with(|cs| {
            let mut s = STATE.borrow_ref_mut(cs);
            s.kick_split();
            s.halted
        })
    }

    /// Event counters; the block and frame counters as the reader last published them.
    pub fn stats(&self) -> StripStats {
        self.csi.stats()
    }

    /// Strips the reader handed out that the DMA wrote over before their release (the reader
    /// was lapped), as it last published them. Zeroed with the other counters.
    pub fn strips_lapped(&self) -> u32 {
        reader_words()[3]
    }

    /// Zeroes the counters; the reader zeroes its own at its next poll.
    pub fn reset_stats(&mut self) {
        self.csi.reset_stats()
    }

    /// D-PHY lane state.
    pub fn phy_status(&self) -> PhyStatus {
        self.csi.phy_status()
    }

    /// Register and ring snapshot: `dma_next` is the reader's next slot, `armed` the items
    /// that read valid, `held` and `queued` are 0.
    pub fn debug_status(&self) -> StripDebug {
        self.csi.debug_status()
    }

    /// Chip revision ×100 from eFuse (e.g. 302 for v3.2).
    pub fn chip_revision(&self) -> u16 {
        self.csi.chip_revision()
    }
}

// ── Reader ──────────────────────────────────────────────────────────────────────────────

/// The half of a split [`CsiStrips`] that runs on the bare second core. `Send`: move it there
/// once. It takes no lock but the cache lock, logs nothing, allocates nothing and touches no
/// DMA, ISP or bridge register but a read of the bridge's raw interrupt status.
pub struct StripReader {
    /// `SHARED.instance` of the driver it belongs to.
    instance: u32,
    /// The run number its state belongs to.
    run: u32,
    /// The run's geometry, loaded from `SHARED` at its start.
    count: u8,
    /// Slot whose block finishes next.
    next: u8,
    geom: Geom,
    strips: [usize; MAX_STRIPS],
    links: [usize; MAX_STRIPS],
    track: Tracker,
    /// `SHARED.loss` when the last block was classified.
    loss_seen: u32,
    /// Block and frame counters; the error counters in it stay unused.
    stats: StripStats,
    stats_reset_seen: u32,
    /// Strips found lapped at their release.
    lapped: u32,
}

const _: () = {
    const fn send<T: Send>() {}
    send::<StripReader>();
};

impl StripReader {
    /// `true` while the driver runs (between start and stop) and still exists.
    pub fn is_running(&self) -> bool {
        SHARED.instance.load(Ordering::Relaxed) == self.instance
            && SHARED.run.load(Ordering::Relaxed) & 1 != 0
    }

    /// The next finished strip in capture order, or `None` if no block has finished since the
    /// last one (or the driver is stopped). Never waits: it looks at each finished block once,
    /// at most one ring of them per call, and returns at the first one to deliver. Blocks it
    /// discards (resynchronisation) go straight back to the DMA.
    ///
    /// Cost per block [INFERENCE from the code, not measured]: two non-cacheable reads, three
    /// non-cacheable writes, one bridge register read, a few atomics; per delivered strip one
    /// cache invalidation (1.9 to 2.6 µs measured for a strip in L2MEM, more if core 0 holds
    /// the cache lock). Poll at a fraction of the strip period, not in a tight loop: the polls
    /// share the L2MEM bus with the DMA.
    pub fn poll(&mut self) -> Option<StripRef<'_>> {
        SHARED.busy.store(true, Ordering::SeqCst);
        let found = self.take();
        SHARED.busy.store(false, Ordering::SeqCst);
        let (slot, meta, addr, len) = found?;
        let run = self.run;
        Some(StripRef {
            // SAFETY: the strip buffer belongs to the driver for as long as `self` exists
            // (`'static` buffers given to `CsiStrips::new`). The DMA writes it again only if
            // the reader is lapped or the driver is stopped and restarted; `release` reports
            // both, and the consumer must then discard what it read.
            data: unsafe { core::slice::from_raw_parts(addr as *const u8, len) },
            slot,
            run,
            meta,
            released: false,
            reader: self,
        })
    }

    /// The body of `poll`, inside `SHARED.busy`.
    fn take(&mut self) -> Option<(u8, Meta, usize, usize)> {
        if SHARED.instance.load(Ordering::SeqCst) != self.instance {
            return None;
        }
        let run = SHARED.run.load(Ordering::SeqCst);
        if run != self.run {
            self.resync(run);
        }
        if run & 1 == 0 || self.count < 2 {
            return None;
        }
        let reset = SHARED.stats_reset.load(Ordering::Acquire);
        if reset != self.stats_reset_seen {
            self.stats_reset_seen = reset;
            self.stats = StripStats::ZERO;
            self.lapped = 0;
            self.publish_stats();
        }
        let mut found = None;
        let mut took = false;
        for _ in 0..self.count {
            let slot = self.next;
            let link = self.links[idx(slot)];
            if !link_done(link) {
                break;
            }
            fence(Ordering::SeqCst);
            let items = link_items(link);
            let damaged = self.take_loss();
            self.next = if slot + 1 >= self.count { 0 } else { slot + 1 };
            // Before the item turns valid: see `State::kick_split`.
            SHARED.next.store(pack(run, self.next), Ordering::Release);
            rearm_link(link);
            took = true;
            if let Some(meta) = self
                .track
                .complete(&self.geom, items, damaged, &mut self.stats)
            {
                let addr = self.strips[idx(slot)];
                let len = meta.rows as usize * self.geom.line_bytes as usize;
                // Cache lines are a power of two (64 or 128 B); no division, so no panic path.
                let line = cache::line_size(addr);
                let span = len.wrapping_add(line - 1) & !(line - 1);
                let t0 = cycles();
                cache::invalidate(addr, span);
                let dt = cycles().wrapping_sub(t0);
                let st = &mut self.stats;
                st.invalidate_cycles_total = st.invalidate_cycles_total.wrapping_add(dt as u64);
                st.invalidate_cycles_max = st.invalidate_cycles_max.max(dt);
                st.strips_delivered = st.strips_delivered.wrapping_add(1);
                found = Some((slot, meta, addr, len));
                break;
            }
        }
        if took {
            SHARED
                .synced
                .store(self.track.sync == Sync::Synced, Ordering::Relaxed);
            self.publish_stats();
        }
        if found.is_some() && SHARED.run.load(Ordering::SeqCst) != run {
            // Stopped while this poll ran: the strip may belong to a run that is over.
            return None;
        }
        found
    }

    /// Takes up run `run`: its geometry if it is running, and the state `start` implies.
    fn resync(&mut self, run: u32) {
        self.run = run;
        if run & 1 == 0 {
            return;
        }
        let count = SHARED.count.load(Ordering::Relaxed) as usize;
        self.count = count.min(MAX_STRIPS) as u8;
        self.geom = Geom {
            line_bytes: SHARED.line_bytes.load(Ordering::Relaxed),
            height: SHARED.height.load(Ordering::Relaxed) as u16,
            lines: SHARED.lines.load(Ordering::Relaxed) as u16,
        };
        for i in 0..MAX_STRIPS {
            self.strips[i] = SHARED.strips[i].load(Ordering::Relaxed);
            self.links[i] = SHARED.links[i].load(Ordering::Relaxed);
        }
        self.track = Tracker::NEW;
        self.next = 0;
        self.loss_seen = SHARED.loss.load(Ordering::Acquire);
        SHARED.next.store(pack(run, 0), Ordering::Release);
        SHARED.synced.store(false, Ordering::Relaxed);
    }

    /// Data was lost since the previous block: the bridge reports an error that core 0 has
    /// not cleared yet, or core 0 counted a loss event. The bridge handler counts before it
    /// clears, so reading the status first and the count second misses neither.
    fn take_loss(&mut self) -> bool {
        let raw = bridge().int_raw().read().bits() & BRIDGE_ERR_MASK != 0;
        fence(Ordering::SeqCst);
        let loss = SHARED.loss.load(Ordering::Acquire);
        let new = loss != self.loss_seen;
        self.loss_seen = loss;
        raw || new
    }

    /// Release check of the strip in slot `slot`, handed out in run `run`.
    fn finish(&mut self, slot: u8, run: u32, meta: &Meta) -> bool {
        // The consumer's reads of the strip come before both checks.
        fence(Ordering::SeqCst);
        let prev = if slot == 0 { self.count - 1 } else { slot - 1 };
        let lapped = link_done(self.links[idx(prev)]);
        fence(Ordering::SeqCst);
        if SHARED.run.load(Ordering::SeqCst) != run
            || SHARED.instance.load(Ordering::SeqCst) != self.instance
        {
            // Stopped or restarted: the next poll takes up the new state.
            return false;
        }
        if lapped {
            self.lapped = self.lapped.wrapping_add(1);
            self.track.lapped(meta, &mut self.stats);
            SHARED.synced.store(false, Ordering::Relaxed);
            self.publish_stats();
            return false;
        }
        true
    }

    /// Writes the counters to `SHARED.stats` under the sequence lock.
    fn publish_stats(&self) {
        let seq = SHARED.stats_seq.load(Ordering::Relaxed);
        SHARED
            .stats_seq
            .store(seq.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        for (a, v) in SHARED
            .stats
            .iter()
            .zip(stat_words(&self.stats, self.lapped))
        {
            a.store(v, Ordering::Relaxed);
        }
        SHARED
            .stats_seq
            .store(seq.wrapping_add(2), Ordering::Release);
    }
}

/// A strip handed out by [`StripReader::poll`]: [`StripRef::rows`] rows of one frame, cache
/// lines invalidated. Derefs to the `rows * line_bytes` pixel bytes, row-major.
///
/// Its item is already back with the DMA: the bytes stay intact only while the reader is less
/// than a ring ahead of being lapped and the driver keeps running. [`StripRef::release`] (or
/// the drop) checks both after the consumer's reads; only a `true` from it makes what was
/// computed from the bytes valid. While it exists the reader cannot poll.
#[must_use = "release() tells whether the bytes were intact"]
pub struct StripRef<'r> {
    reader: &'r mut StripReader,
    data: &'r [u8],
    slot: u8,
    run: u32,
    meta: Meta,
    released: bool,
}

impl StripRef<'_> {
    /// Frame number: frame ends seen since the start. Gaps mean frames were lost.
    pub fn frame(&self) -> u32 {
        self.meta.frame
    }

    /// Row of the frame the strip starts with.
    pub fn first_row(&self) -> u16 {
        self.meta.first_row
    }

    /// Rows in this strip: `lines`, fewer for the last strip of the frame.
    pub fn rows(&self) -> u16 {
        self.meta.rows
    }

    /// The last strip of its frame: every strip of the frame was delivered.
    pub fn is_last(&self) -> bool {
        self.meta.last
    }

    /// `mcycle` (low 32 bits, of the reader's core) when the reader classified the block.
    pub fn queued_at_cycles(&self) -> u32 {
        self.meta.queued_at
    }

    /// Ends the use of the strip. `true`: the DMA did not write it since it was handed out and
    /// the driver was neither stopped nor restarted, so everything read from it is the strip's
    /// data. `false`: discard what was computed from this strip's frame; the reader delivers
    /// again from the next frame start (counted in [`StripControl::strips_lapped`] and
    /// `frames_truncated` if it was lapped).
    pub fn release(mut self) -> bool {
        self.released = true;
        self.reader.finish(self.slot, self.run, &self.meta)
    }
}

impl Deref for StripRef<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.data
    }
}

impl Drop for StripRef<'_> {
    fn drop(&mut self) {
        if !self.released {
            let (slot, run, meta) = (self.slot, self.run, self.meta);
            self.reader.finish(slot, run, &meta);
        }
    }
}
