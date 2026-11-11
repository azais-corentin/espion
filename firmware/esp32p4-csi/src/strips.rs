//! Line-strip capture: every frame arrives as a sequence of strips of `lines` rows in a small
//! ring of buffers (internal RAM), so a row-incremental consumer can work on a frame while the
//! sensor is still sending it and never touches PSRAM.
//!
//! # How it works
//!
//! - The CSI bridge ends a DW-GDMA block after `DMABLK_SIZE` = `line_bytes * lines / 4096` bursts
//!   and at the end of every frame (TRM 38.5.7, 38.7.2 step 4).
//! - The channel runs a circular linked list (TRM 7.5.4.2 "Linked List", 7.7.3) of one
//!   [`StripLink`] per strip buffer. Every item asks for an interrupt at block end
//!   (`IOC_BLKTFR`). The DMA writes each item's `CTL` and `LLP_STATUS` back when its block
//!   ends, which clears the item's `VALID` bit (TRM 7.5.4.2, figure 7.5-6).
//! - Back-pressure: an item is valid only while its strip buffer belongs to the driver.
//!   Handing a strip to the application leaves the item invalid. Returning the strip (drop it
//!   or pass it to the next [`CsiStrips::wait_strip`]) sets `VALID` again. If the DMA reaches
//!   an item that is still invalid, it halts with `SHADOWREG_OR_LLI_INVALID_ERR` until the
//!   driver writes `BLK_TFR_RESUMEREQ` (TRM 7.5.4.2). Before that write the driver resets and
//!   reprograms the ISP and the bridge (see "Stalls"), counts a stall and resynchronises as
//!   below.
//!
//! # Frame boundaries
//!
//! A frame ends with the only block shorter than a full strip. The bridge cuts blocks at
//! `DMABLK_SIZE` bursts and at frame end, and [`CsiStrips::new`] requires that `height` is
//! **not** a multiple of `lines`, so the last strip of every frame is short. This marker is
//! carried in the data path itself: it is written by the same DMA, in the same order, as the
//! pixels, and it needs no second interrupt source that could race with the block interrupts.
//! (Counting blocks drifts silently after a lost block; the ISP `TAIL_IDI_FRAME` interrupt comes
//! from a different block and can arrive before or after the last block's interrupt.)
//!
//! The DMA interrupt walks the finished items in ring order and keeps a row counter:
//! - a full block where a full one is expected is the next strip of the frame;
//! - a short block of exactly the expected size at the expected position ends the frame;
//! - anything else (short too early, full where the short one was due, wrong size), a bridge
//!   overrun, discard or FIFO overflow, a fatal CSI host error, a DMA error or a stall marks
//!   the frame damaged.
//!
//! After damage the driver **discards every block until the next short one**, then keeps
//! discarding for one **probation frame**: a whole frame whose every block has the size its
//! position predicts and whose short last block arrives exactly where it should. Delivery
//! starts with the block after that. One short block alone is not enough: a frame end found
//! after damage says nothing about whether the next frame's blocks line up with its rows, and
//! only a frame that checks out from end to end proves the alignment. A failed check restarts
//! the search.
//!
//! So a strip handed out always carries rows that belong to its frame number. Cost: a damaged
//! frame loses its remaining strips plus one to three more frames.
//!
//! After [`CsiStrips::start`] the driver is in the same state, so the first two frames (one to
//! find a frame end, one probation frame) are never delivered.
//!
//! A frame was delivered whole when a strip with [`Strip::is_last`] arrives. A strip with
//! `first_row() == 0` starts a frame; if one arrives before the previous frame's last strip,
//! that previous frame was truncated ([`StripStats::frames_truncated`]).
//!
//! # Stalls
//!
//! The bridge, not the DMA, is the flow controller (`TT_FC` = 4, `CSI_DMA_FLOW_CONTROLLER`;
//! TRM 38.7.2, 7.5.5): it alone decides where a block ends, and the item's `BLOCK_TS` does not
//! bound it (TRM 7.5.1: "the flow controller controls the block length"). While the DMA halts
//! on an invalid item the bridge fills up and discards the rest of the frame
//! (`CSI_BRIG_DISCARD`). Measured on the board with the DMA simply resumed afterwards (as the
//! driver did before 2026-11-10): the bridge flushed what it had buffered (961 items) as one
//! block, and its block cuts were then off for the following blocks. In RAW8 the next block
//! was 448 items short and the frame end 449 items late, which the probation frame absorbs.
//! In RAW10 (1280x800, 2 lanes, a strip returned while the bridge was empty) the bridge never
//! ended the next block at all: the DMA wrote on from the strip's start through the rest of
//! the ring, the statics and the stack up to the end of L2MEM (365248 bytes, `DST_DEC_ERR` at
//! `0x4FFC_0100`) and the firmware froze or faulted. No list item can stop that: the DMA
//! fetches the next one only when the bridge ends the block.
//!
//! So the driver never resumes a halted channel on the old bridge state. Every resume (and
//! every restart after a DMA error) first pulses the ISP and bridge resets, programs both as
//! [`CsiStrips::start`] did and enables the bridge again; the CSI host and the D-PHY keep
//! running. This happens only while the channel waits between blocks, so no bridge transfer
//! is in flight. Measured: after the restart the first block is a full strip at the start of
//! the next frame and every block has its normal size; halts of 1 to 100 ms, and 8 or 20 halts
//! in a row, recover in RAW8 and RAW10 with every strip's rows correct, at most one frame
//! skipped and one truncated per halt, the next whole frame 25 to 33 ms (120 fps modes) or 14
//! to 19 ms (640x400 at 210 fps) after the strip is returned, and guard areas after the ring
//! and around the list items unchanged (`poc-p4-eth` strip stall test, 2026-11-10). A bridge
//! discard while the DMA runs (the DMA starved, not halted) does not pass through this path;
//! it was not seen in any soak.
//!
//! # Interrupt handler
//!
//! Unlike the whole-frame path, the DMA does not wait for the handler: it runs on through
//! every valid item on its own. A late handler only delays the wake-up and the re-validation
//! of blocks the driver discards while resynchronising. Data is lost only when the DMA catches
//! up with a strip that is queued or held by the application.
//!
//! Worst case per run, all inside one critical section:
//! - 4 register accesses (interrupt status read and clear, bridge raw status);
//! - per finished item (at most the ring size, [`MAX_STRIPS`] = 16): 2 non-cacheable reads,
//!   and for a discarded block 3 non-cacheable writes;
//! - to resume a halted channel: about 40 register accesses for the ISP and bridge restart,
//!   then at most 3 DMA register writes (only after a stall);
//! - if a `BLOCK_TFR_DONE` interrupt finds no written-back item, up to 64 more reads of one
//!   status word (`WRITEBACK_SPINS`) before giving up.
//!
//! That is under 200 memory accesses for a 16-strip ring. Measured on the board (400 MHz, RAW8,
//! all four 120 fps modes): 1.4–3.3 µs on average and under 4 µs in steady state, with rare
//! peaks of 10–17 µs and up to 81 µs on the first blocks after `new` (cold caches). See
//! `StripStats::isr_cycles_max`. The strip buffers themselves are never touched in the handler.
//!
//! # Memory
//!
//! - **Strip buffers:** `line_bytes * lines` bytes each ([`CsiStrips::strip_bytes`]), start and
//!   length aligned to the cache line, in internal RAM (any address the DMA reaches works;
//!   PSRAM defeats the purpose).
//! - **List items:** a `[StripLink; N]` in internal L2MEM (a `static`). The driver writes and
//!   reads them through the non-cacheable L2MEM alias (+0x4000_0000, as IDF `dw_gdma.c`
//!   does), so no cache maintenance is needed per item.
//! - **Cache:** [`CsiStrips::wait_strip`] invalidates a strip's lines (`Cache_Invalidate_Addr`)
//!   before handing it out.
//!
//! # Two cores
//!
//! [`CsiStrips::split`] divides the driver into a [`StripControl`] (start, stop, errors and
//! stalls, on the core that created it) and a [`StripReader`] for a bare second core, which
//! polls the list items itself: no interrupt per strip, no critical section and no waker in
//! the data path. See the [`split`] module docs. Not run on the board yet.
//!
//! # Example: a row-incremental consumer
//!
//! ```no_run
//! # struct Detector;
//! # impl Detector {
//! #     fn begin_frame(&mut self, _width: u16, _threshold: u8) {}
//! #     fn push_rows(&mut self, _rows: &[u8], _first_row: u16) {}
//! #     fn finish(&mut self, _out: &mut [u32]) -> usize { 0 }
//! # }
//! # async fn run(p: esp_hal::peripherals::Peripherals) {
//! use esp32p4_csi::{Config, CsiStrips, StripLink};
//!
//! const W: usize = 1280;
//! const LINES: u16 = 64;
//! #[repr(C, align(64))]
//! struct StripMem([u8; W * LINES as usize]);
//! static mut STRIPS: [StripMem; 4] = [const { StripMem([0; W * LINES as usize]) }; 4];
//! static mut LINKS: [StripLink; 4] = [StripLink::NEW; 4];
//!
//! // SAFETY: taken once, here.
//! let (strips, links) = unsafe {
//!     let [a, b, c, d] = &mut *core::ptr::addr_of_mut!(STRIPS);
//!     ([&mut a.0[..], &mut b.0[..], &mut c.0[..], &mut d.0[..]],
//!      &mut *core::ptr::addr_of_mut!(LINKS))
//! };
//! // 1280x800: 12 strips of 64 rows and a short last one of 32 rows per frame.
//! let mut csi = CsiStrips::new(p.VDMA_CH0, strips, links, Config::new(1280, 800, 800), LINES)
//!     .unwrap();
//! csi.start().unwrap();
//! // ...sensor stream-on...
//!
//! let mut detector = Detector;
//! let mut blobs = [0u32; 64];
//! let mut frame = None; // frame being assembled
//! let mut strip = None;
//! loop {
//!     // Returns the previous strip to the DMA and waits for the next one, in order.
//!     let s = csi.wait_strip(strip.take()).await.unwrap();
//!     if s.first_row() == 0 {
//!         detector.begin_frame(W as u16, 128);
//!         frame = Some(s.frame());
//!     }
//!     if frame == Some(s.frame()) {
//!         detector.push_rows(&s, s.first_row()); // s.rows() * W bytes, row-major
//!         if s.is_last() {
//!             let n = detector.finish(&mut blobs);
//!             let _ = n; // frame s.frame() complete
//!             frame = None;
//!         }
//!     }
//!     strip = Some(s);
//! }
//! # }
//! ```

use core::{
    cell::RefCell,
    future::poll_fn,
    marker::PhantomData,
    ops::Deref,
    ptr,
    sync::atomic::{Ordering, fence},
    task::Poll,
};

use critical_section::Mutex;
use esp_hal::{
    asynch::AtomicWaker,
    interrupt::{self, Priority},
    mipi_dsi::VdmaDmaChannel,
    peripherals::Interrupt,
    system::Cpu,
};

use crate::{
    BRIDGE_ERR_MASK, BURST_BYTES, CSI_BRG_MEM_BASE, CTL0, CTL1, Config, DMA_ERR_MASK, DMA_TFR_DONE,
    DebugStatus, Error, PhyStatus, TAKEN, bridge, cache, channel_enable, channel_enabled,
    chip_revision, configure_link, dma, dphy, enable_clocks, halt_channel, host, init_dma_channel,
    read_host_errors, reset_link, restart_isp_bridge, snapshot,
};

pub mod split;
pub use split::{StripControl, StripReader, StripRef};

/// Most strip buffers in the ring.
pub const MAX_STRIPS: usize = 16;

/// Reads of an item's status word the DMA handler spends waiting for a write-back that its
/// interrupt overtook.
const WRITEBACK_SPINS: u32 = 64;
/// DW-GDMA channel `BLOCK_TFR_DONE` interrupt bit (`dw_gdma_ll.h`).
const BLOCK_TFR_DONE: u32 = 1 << 0;
/// `SHADOWREG_OR_LLI_INVALID_ERR`: the DMA fetched an invalid item and halted.
const LLI_INVALID: u32 = 1 << 13;
/// `CTL1.IOC_BLKTFR`: interrupt at the end of this block.
const CTL1_IOC_BLKTFR: u32 = 1 << 26;
/// `CTL1.SHADOWREG_OR_LLI_VALID`.
const CTL1_LLI_VALID: u32 = 1 << 31;
/// `LLP.LMS`: the list items are on master 1 (memory), as IDF sets it.
const LLP_LMS_MEMORY: u32 = 1;
/// `LLP_STATUS[62]` (bit 30 of the upper word): `BLOCK_TFR_DONE`, written last (TRM figure
/// 7.5-6).
const STATUS_HI_BLOCK_DONE: u32 = 1 << 30;
/// CSI host `INT_ST_MAIN` bits that mean data was lost or corrupted: PHY fatal, packet fatal,
/// frame boundary, frame sequence, frame CRC, payload CRC, data ID, PHY (ErrSotHS/ErrEsc). Bit 7
/// (ECC corrected) is not a loss.
const HOST_LOSS_MASK: u32 = 0x7F | (1 << 16);

// Word offsets in a list item (TRM figure 7.5-5; IDF `dw_gdma_link_list_item_t`).
const LLI_SAR: usize = 0;
const LLI_DAR: usize = 2;
const LLI_BLOCK_TS: usize = 4;
const LLI_LLP: usize = 6;
const LLI_CTL0: usize = 8;
const LLI_CTL1: usize = 9;
const LLI_STATUS_LO: usize = 12;
const LLI_STATUS_HI: usize = 13;

/// One DW-GDMA linked-list item (64 bytes, 64-byte aligned: one L1 cache line). Provide one
/// per strip buffer, in internal L2MEM, and never touch them while the driver owns them.
#[repr(C, align(64))]
pub struct StripLink([u32; 16]);

impl StripLink {
    /// An all-zero item, for `static` arrays: `[StripLink::NEW; N]`.
    pub const NEW: Self = Self([0; 16]);
}

/// Errors of the strip path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripError {
    /// A shared condition of the receiver ([`Error::AlreadyInUse`], [`Error::InvalidGeometry`],
    /// [`Error::InvalidBitRate`], [`Error::Running`], [`Error::NotRunning`]).
    Csi(Error),
    /// `lines` is not below `height`, the strip needs more than 8191 bursts, or `height` is a
    /// multiple of `lines` (the frame-end marker needs a short last strip).
    StripGeometry,
    /// `lines` is 0 or not a multiple of `step` ([`CsiStrips::line_step`]): a strip must be a
    /// whole number of bridge bursts ([`BURST_BYTES`]).
    StripLines { step: u16 },
    /// Fewer than 2 or more than [`MAX_STRIPS`] strip buffers.
    StripCount,
    /// Strip buffer `index` is shorter than [`CsiStrips::strip_bytes`].
    StripTooSmall { index: u8, needed: usize },
    /// Strip buffer `index` start or length is not a multiple of `align` (the cache line).
    StripAlignment { index: u8, align: usize },
    /// The list items are not in internal L2MEM (0x4FF0_0000..0x4FFC_0000).
    LinksNotInternal,
    /// [`CsiStrips::start`] with strips still held by the application.
    StripsHeld,
    /// [`StripControl::start`]: the split reader stayed inside one poll for 10 ms, so core 1
    /// is stopped or stuck. The list items were not touched.
    ReaderBusy,
}

impl From<Error> for StripError {
    fn from(e: Error) -> Self {
        Self::Csi(e)
    }
}

/// Event counters of the strip path since [`CsiStrips::new`] (or [`CsiStrips::reset_stats`]).
///
/// Frame accounting: every frame end the DMA delivered counts once in `frames_done` and once
/// in exactly one of `frames_complete`, `frames_truncated` and `frames_skipped`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StripStats {
    /// Blocks the DMA finished (written back).
    pub strips_done: u32,
    /// Strips handed to the application.
    pub strips_delivered: u32,
    /// Finished blocks thrown away while resynchronising (or with an unexpected size).
    pub strips_discarded: u32,
    /// Frame ends seen: short blocks.
    pub frames_done: u32,
    /// Frames of which every strip was queued for the application, the last one with
    /// [`Strip::is_last`].
    pub frames_complete: u32,
    /// Frames abandoned after some of their strips were queued: damage was detected mid-frame.
    pub frames_truncated: u32,
    /// Frames of which nothing was delivered: the first two frames after `start`, and frames
    /// that ended while the driver was resynchronising (seeking a frame end or checking the
    /// probation frame).
    pub frames_skipped: u32,
    /// Times the DMA reached an item whose strip was not returned and halted
    /// (`SHADOWREG_OR_LLI_INVALID_ERR`).
    pub dma_stalls: u32,
    /// Bridge buffer overruns (`CSI_BUF_OVERRUN`): data lost.
    pub bridge_overruns: u32,
    /// Blocks whose size did not fit their position in the frame.
    pub length_errors: u32,
    /// `BLOCK_TFR_DONE` interrupts without a written-back item. Non-zero means the DMA does
    /// not write the items back (assumption A13).
    pub writeback_missing: u32,
    /// `LLP_STATUS` completed-items field of the last finished block, as written back.
    pub last_block_items: u32,
    /// DMA handler runs, and their CPU cycles (critical section included): total and longest.
    pub isr_runs: u32,
    pub isr_cycles_total: u64,
    pub isr_cycles_max: u32,
    /// CPU cycles of the cache invalidation in [`CsiStrips::wait_strip`] (one per strip
    /// delivered): total and longest.
    pub invalidate_cycles_total: u64,
    pub invalidate_cycles_max: u32,
    /// DW-GDMA channel error interrupts.
    pub dma_errors: u32,
    /// OR of all DW-GDMA channel `INTSTATUS` bits seen with an error.
    pub dma_error_bits: u32,
    /// CSI host error interrupts.
    pub host_errors: u32,
    /// OR of `CSI_HOST_INT_ST_MAIN`, as [`crate::Stats::host_error_main`].
    pub host_error_main: u32,
    /// OR of the per-category host status registers, as [`crate::Stats::host_error_detail`].
    pub host_error_detail: [u32; 9],
    /// CSI bridge error interrupts.
    pub bridge_errors: u32,
    /// OR of `CSI_BRIG_INT_ST`, as [`crate::Stats::bridge_error_bits`].
    pub bridge_error_bits: u32,
}

impl StripStats {
    const ZERO: Self = Self {
        strips_done: 0,
        strips_delivered: 0,
        strips_discarded: 0,
        frames_done: 0,
        frames_complete: 0,
        frames_truncated: 0,
        frames_skipped: 0,
        dma_stalls: 0,
        bridge_overruns: 0,
        length_errors: 0,
        writeback_missing: 0,
        last_block_items: 0,
        isr_runs: 0,
        isr_cycles_total: 0,
        isr_cycles_max: 0,
        invalidate_cycles_total: 0,
        invalidate_cycles_max: 0,
        dma_errors: 0,
        dma_error_bits: 0,
        host_errors: 0,
        host_error_main: 0,
        host_error_detail: [0; 9],
        bridge_errors: 0,
        bridge_error_bits: 0,
    };
}

/// Register and ring snapshot for bring-up. Reading it clears nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StripDebug {
    /// The same snapshot as [`crate::Csi::debug_status`].
    pub link: DebugStatus,
    /// Channel `LLP0`: address of the item the DMA works on, plus `LMS` in bit 0.
    pub dma_llp: u32,
    /// Ring slot whose block the driver expects to finish next.
    pub dma_next: u8,
    /// Slots the DMA may write (item valid), one bit per slot.
    pub armed: u16,
    /// Slots handed to the application.
    pub held: u16,
    /// Finished strips waiting for [`CsiStrips::wait_strip`].
    pub queued: u8,
    /// `false` while discarding blocks up to the next frame end.
    pub synced: bool,
    /// The channel waits for an item to become valid (or stopped on an error).
    pub halted: bool,
}

// ── Shared state (task ↔ ISRs) ──────────────────────────────────────────────────────────

/// Frame alignment of the block stream.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sync {
    /// Discarding up to the next short block (frame end).
    Seeking,
    /// Discarding one whole frame while checking that every block has the size its position
    /// predicts; delivery starts after its last block.
    Probation,
    /// Delivering.
    Synced,
}

#[derive(Clone, Copy)]
struct Meta {
    frame: u32,
    first_row: u16,
    rows: u16,
    last: bool,
    /// `mcycle` when the handler queued the strip.
    queued_at: u32,
}

impl Meta {
    const EMPTY: Self = Self {
        frame: 0,
        first_row: 0,
        rows: 0,
        last: false,
        queued_at: 0,
    };
}

/// CPU cycle counter (`mcycle`, low 32 bits).
fn cycles() -> u32 {
    let c: u32;
    // SAFETY: reads a counter CSR; no side effects.
    unsafe { core::arch::asm!("csrr {0}, mcycle", out(reg) c) };
    c
}

/// What classifying a block needs to know about the frame.
#[derive(Clone, Copy)]
struct Geom {
    /// Bytes per line in memory ([`Config::line_bytes`]).
    line_bytes: u32,
    height: u16,
    /// Rows per full strip.
    lines: u16,
}

impl Geom {
    const ZERO: Self = Self {
        line_bytes: 0,
        height: 0,
        lines: 0,
    };

    /// 64-bit items of a full strip.
    fn strip_items(&self) -> u32 {
        (self.line_bytes * self.lines as u32) / 8
    }
}

/// Frame alignment and row position of the block stream: the classification described under
/// "Frame boundaries". The interrupt path and the split reader ([`split`]) both run it on
/// every finished block, in ring order.
#[derive(Clone, Copy)]
struct Tracker {
    sync: Sync,
    /// First row of the next block (Probation and Synced).
    row: u16,
    /// Frame number of the frame in progress: frame ends seen since `start`.
    frame: u32,
    /// The frame being discarded had strips delivered already (counted as truncated).
    partial: bool,
}

impl Tracker {
    /// The state after `start`: seeking the first frame end.
    const NEW: Self = Self {
        sync: Sync::Seeking,
        row: 0,
        frame: 0,
        partial: false,
    };

    /// Classifies one finished block of `reported` items (`LLP_STATUS` completed size).
    /// `damaged`: data was lost since the previous block. Returns the strip to deliver, or
    /// `None` when the block is discarded (counted in `strips_discarded`).
    fn complete(
        &mut self,
        g: &Geom,
        reported: u32,
        damaged: bool,
        st: &mut StripStats,
    ) -> Option<Meta> {
        st.strips_done = st.strips_done.wrapping_add(1);
        st.last_block_items = reported;
        // On the P4 v3.2 the completed size reads one item short (A6); accept both.
        let items = reported + 1;
        let full = items >= g.strip_items();
        if damaged {
            self.abandon(st);
        }
        let meta = if self.sync == Sync::Seeking {
            if !full {
                // A frame end, but alignment is unproven: check one whole frame first.
                self.frame_end_unused(st);
                self.sync = Sync::Probation;
            }
            None
        } else {
            // Probation and Synced track rows the same way; only Synced delivers.
            let deliver = self.sync == Sync::Synced;
            let rows_left = g.height - self.row;
            let expect_last = rows_left < g.lines;
            let expected_items = (rows_left as u32 * g.line_bytes) / 8;
            if full && !expect_last {
                let meta = deliver.then(|| Meta {
                    frame: self.frame,
                    first_row: self.row,
                    rows: g.lines,
                    last: false,
                    queued_at: cycles(),
                });
                self.row += g.lines;
                meta
            } else if !full && expect_last && items.abs_diff(expected_items) <= 1 {
                let meta = if deliver {
                    let meta = Meta {
                        frame: self.frame,
                        first_row: self.row,
                        rows: rows_left,
                        last: true,
                        queued_at: cycles(),
                    };
                    st.frames_complete = st.frames_complete.wrapping_add(1);
                    self.frame_end(st);
                    Some(meta)
                } else {
                    // A whole frame with the expected block sizes: aligned from the next block on.
                    self.frame_end_unused(st);
                    None
                };
                self.sync = Sync::Synced;
                meta
            } else {
                st.length_errors = st.length_errors.wrapping_add(1);
                self.abandon(st);
                if !full {
                    // A frame end at the wrong place: rows were lost or shifted. Prove the next
                    // frame before delivering again.
                    self.frame_end_unused(st);
                    self.sync = Sync::Probation;
                }
                None
            }
        };
        if meta.is_none() {
            st.strips_discarded = st.strips_discarded.wrapping_add(1);
        }
        meta
    }

    /// Gives up the frame in progress: discard blocks up to the next frame end, then check a
    /// whole frame before delivering again.
    fn abandon(&mut self, st: &mut StripStats) {
        if self.sync == Sync::Synced && self.row > 0 {
            st.frames_truncated = st.frames_truncated.wrapping_add(1);
            self.partial = true;
        }
        self.sync = Sync::Seeking;
    }

    /// End of a frame that was not delivered (counted truncated already, or skipped now).
    fn frame_end_unused(&mut self, st: &mut StripStats) {
        if self.partial {
            self.partial = false;
        } else {
            st.frames_skipped = st.frames_skipped.wrapping_add(1);
        }
        self.frame_end(st);
    }

    fn frame_end(&mut self, st: &mut StripStats) {
        self.frame = self.frame.wrapping_add(1);
        self.row = 0;
        st.frames_done = st.frames_done.wrapping_add(1);
    }
}

/// Non-cacheable pointer to word `word` of the list item at `link` (its cached L2MEM address,
/// as the DMA sees it).
fn link_word(link: usize, word: usize) -> *mut u32 {
    (cache::non_cacheable(link) as *mut u32).wrapping_add(word)
}

/// Makes the list item at `link` valid again after a write-back (which changes only `CTL` and
/// the status words): status cleared, then `CTL1` with `VALID`. Only the item's owner calls
/// it: the interrupt path, or the split reader for items it has taken from the DMA.
fn rearm_link(link: usize) {
    // SAFETY: the item belongs to the caller and the DMA does not use it (not valid).
    unsafe {
        ptr::write_volatile(link_word(link, LLI_STATUS_LO), 0);
        ptr::write_volatile(link_word(link, LLI_STATUS_HI), 0);
        fence(Ordering::SeqCst);
        ptr::write_volatile(
            link_word(link, LLI_CTL1),
            CTL1 | CTL1_IOC_BLKTFR | CTL1_LLI_VALID,
        );
    }
    fence(Ordering::SeqCst);
}

/// The DMA has written the item at `link` back: its `BLOCK_TFR_DONE` status bit, which it
/// writes last, is set.
fn link_done(link: usize) -> bool {
    // SAFETY: a read of one word of a list item; reading changes nothing.
    unsafe { ptr::read_volatile(link_word(link, LLI_STATUS_HI)) & STATUS_HI_BLOCK_DONE != 0 }
}

/// Completed-items field of the item at `link`, as written back. Read it after
/// [`link_done`] and a fence.
fn link_items(link: usize) -> u32 {
    // SAFETY: as in `link_done`.
    unsafe { ptr::read_volatile(link_word(link, LLI_STATUS_LO)) & 0x3F_FFFF }
}

struct State {
    /// Bumped by every `CsiStrips::new` so strips of an old instance are not returned to it.
    generation: u32,
    running: bool,
    /// [`CsiStrips::split`] ran: the split reader takes the finished blocks, the interrupt
    /// handles errors and stalls only.
    split: bool,
    channel: u8,
    count: u8,
    /// Strip buffer addresses and lengths.
    strips: [usize; MAX_STRIPS],
    lens: [usize; MAX_STRIPS],
    /// List item addresses as the DMA sees them (cached L2MEM addresses).
    links: [usize; MAX_STRIPS],
    geom: Geom,
    /// Slots whose item is valid (owned by the DMA).
    armed: u16,
    /// Slots handed out.
    held: u16,
    /// Finished strips in delivery order.
    queue: [u8; MAX_STRIPS],
    q_head: u8,
    q_len: u8,
    meta: [Meta; MAX_STRIPS],
    /// Ring slot whose block finishes next.
    dma_next: u8,
    track: Tracker,
    /// Loss seen since the last block was processed.
    damaged: bool,
    halted: bool,
    stats: StripStats,
    /// What [`restart_isp_bridge`] needs to program the ISP and the bridge again (set by
    /// `start`): geometry and data type, chip revision, bridge bursts per block.
    config: Config,
    chip_rev: u16,
    bursts: u16,
}

impl State {
    const fn new() -> Self {
        Self {
            generation: 0,
            running: false,
            split: false,
            channel: 0,
            count: 0,
            strips: [0; MAX_STRIPS],
            lens: [0; MAX_STRIPS],
            links: [0; MAX_STRIPS],
            geom: Geom::ZERO,
            armed: 0,
            held: 0,
            queue: [0; MAX_STRIPS],
            q_head: 0,
            q_len: 0,
            meta: [Meta::EMPTY; MAX_STRIPS],
            dma_next: 0,
            track: Tracker::NEW,
            damaged: false,
            halted: false,
            stats: StripStats::ZERO,
            config: Config::new(0, 0, 0),
            chip_rev: 0,
            bursts: 0,
        }
    }

    /// Writes every field of slot `slot`'s item and makes it valid (`VALID` last).
    fn write_link(&mut self, slot: u8) {
        let next = (slot as usize + 1) % self.count as usize;
        let link = self.links[slot as usize];
        let fields = [
            (LLI_SAR, CSI_BRG_MEM_BASE),
            (LLI_SAR + 1, 0),
            (LLI_DAR, self.strips[slot as usize] as u32),
            (LLI_DAR + 1, 0),
            (LLI_BLOCK_TS, self.geom.strip_items() - 1),
            (LLI_LLP, self.links[next] as u32 | LLP_LMS_MEMORY),
            (LLI_LLP + 1, 0),
            (LLI_CTL0, CTL0),
        ];
        for (word, value) in fields {
            // SAFETY: the item belongs to the driver and the DMA does not use it (not valid).
            unsafe { ptr::write_volatile(link_word(link, word), value) };
        }
        self.rearm(slot);
    }

    /// Makes slot `slot`'s item valid again ([`rearm_link`]).
    fn rearm(&mut self, slot: u8) {
        rearm_link(self.links[slot as usize]);
        self.armed |= 1 << slot;
    }

    /// Slot `slot` is armed and the DMA has written its item back.
    fn written_back(&self, slot: u8) -> bool {
        self.armed & (1 << slot) != 0 && link_done(self.links[slot as usize])
    }

    /// Walks the finished items from `dma_next` in ring order. Returns how many it found.
    fn collect(&mut self) -> u32 {
        let mut found = 0;
        for _ in 0..self.count {
            let slot = self.dma_next;
            if !self.written_back(slot) {
                break;
            }
            fence(Ordering::SeqCst);
            let items = link_items(self.links[slot as usize]);
            self.armed &= !(1 << slot);
            self.dma_next = (slot + 1) % self.count;
            found += 1;
            self.complete(slot, items);
        }
        found
    }

    /// Classifies one finished block of `reported` items: queued for the application, or
    /// given back to the DMA at once.
    fn complete(&mut self, slot: u8, reported: u32) {
        let damaged = core::mem::take(&mut self.damaged);
        let geom = self.geom;
        match self
            .track
            .complete(&geom, reported, damaged, &mut self.stats)
        {
            Some(meta) => self.push(slot, meta),
            None => self.rearm(slot),
        }
    }

    fn push(&mut self, slot: u8, meta: Meta) {
        self.meta[slot as usize] = meta;
        let tail = (self.q_head + self.q_len) as usize % MAX_STRIPS;
        self.queue[tail] = slot;
        self.q_len += 1;
    }

    fn pop(&mut self) -> Option<u8> {
        if self.q_len == 0 {
            return None;
        }
        let slot = self.queue[self.q_head as usize];
        self.q_head = (self.q_head + 1) % MAX_STRIPS as u8;
        self.q_len -= 1;
        Some(slot)
    }

    /// Restarts a halted channel once the item it waits for is valid: the ISP and the bridge
    /// reset and programmed again ([`restart_isp_bridge`], see "Stalls" in the module docs),
    /// then `BLK_TFR_RESUMEREQ` after a stall (TRM 7.5.4.2), or `LLP0` + enable after the
    /// channel stopped on an error.
    fn kick(&mut self) {
        let slot = self.dma_next;
        if !self.halted || !self.running || self.armed & (1 << slot) == 0 {
            return;
        }
        self.restart_channel(slot);
    }

    /// The restart of [`State::kick`], at slot `slot` if the channel stopped on an error.
    fn restart_channel(&mut self, slot: u8) {
        let ch = self.channel;
        let c = dma().ch(ch as usize);
        // The channel waits between blocks, so no bridge transfer is in flight.
        restart_isp_bridge(&self.config, self.chip_rev, self.bursts);
        if channel_enabled(ch) {
            c.blk_tfr_resumereq0().write(|w| unsafe { w.bits(1) });
        } else {
            c.llp0()
                .write(|w| unsafe { w.bits(self.links[slot as usize] as u32 | LLP_LMS_MEMORY) });
            c.llp1().write(|w| unsafe { w.bits(0) });
            channel_enable(ch, true);
        }
        self.halted = false;
    }
}

static STATE: Mutex<RefCell<State>> = Mutex::new(RefCell::new(State::new()));
static WAKER: AtomicWaker = AtomicWaker::new();

// ── Interrupt handlers ──────────────────────────────────────────────────────────────────

/// DW-GDMA: one or more blocks finished, or the channel halted. See the module docs for the
/// worst-case work.
#[esp_hal::ram]
#[esp_hal::handler(priority = Priority::Priority2)]
fn dma_isr() {
    let t0 = cycles();
    critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        let ch = s.channel;
        let c = dma().ch(ch as usize);
        let st = c.intstatus0().read().bits();
        c.intclear0().write(|w| unsafe { w.bits(st) });
        if st & DMA_ERR_MASK != 0 {
            s.stats.dma_errors = s.stats.dma_errors.wrapping_add(1);
            s.stats.dma_error_bits |= st;
        }
        if !s.running {
            return;
        }
        if s.split {
            // The split reader takes the finished blocks; this handler sees errors and stalls
            // only (`BLOCK_TFR_DONE` no longer signals).
            s.split_event(st);
        } else {
            // Loss the bridge reported before these blocks finished (its own handler may not
            // have run yet).
            if bridge().int_raw().read().bits() & BRIDGE_ERR_MASK != 0 {
                s.damaged = true;
            }
            let queued = s.q_len;
            let mut found = s.collect();
            if st & BLOCK_TFR_DONE != 0 && found == 0 {
                // The interrupt may beat the item's write-back (order not documented): give the
                // write-back a bounded moment instead of leaving the strip until the next block.
                for _ in 0..WRITEBACK_SPINS {
                    if s.written_back(s.dma_next) {
                        break;
                    }
                }
                found = s.collect();
                if found == 0 {
                    s.stats.writeback_missing = s.stats.writeback_missing.wrapping_add(1);
                }
            }
            if st & LLI_INVALID != 0 {
                s.stats.dma_stalls = s.stats.dma_stalls.wrapping_add(1);
                s.damaged = true;
                s.halted = true;
            } else if st & (DMA_ERR_MASK | DMA_TFR_DONE) != 0 && !channel_enabled(ch) {
                // Stopped on an error (or ran off the list): restart at the expected item.
                s.damaged = true;
                s.halted = true;
            }
            s.kick();
            if s.q_len != queued {
                WAKER.wake();
            }
        }
        let dt = cycles().wrapping_sub(t0);
        s.stats.isr_runs = s.stats.isr_runs.wrapping_add(1);
        s.stats.isr_cycles_total = s.stats.isr_cycles_total.wrapping_add(dt as u64);
        s.stats.isr_cycles_max = s.stats.isr_cycles_max.max(dt);
    });
}

/// CSI host error interrupt. Above the DMA priority so that a loss is flagged before the
/// block holding it is classified.
#[esp_hal::handler(priority = Priority::Priority3)]
fn host_isr() {
    let (main, detail) = read_host_errors();
    if main & HOST_LOSS_MASK != 0 {
        split::note_loss();
    }
    critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        s.stats.host_errors = s.stats.host_errors.wrapping_add(1);
        s.stats.host_error_main |= main;
        for (acc, d) in s.stats.host_error_detail.iter_mut().zip(detail) {
            *acc |= d;
        }
        if main & HOST_LOSS_MASK != 0 && s.running {
            s.damaged = true;
        }
    });
}

/// CSI bridge error interrupt (discard, buffer overrun, async FIFO overflow): data lost.
#[esp_hal::handler(priority = Priority::Priority3)]
fn bridge_isr() {
    let b = bridge();
    let st = b.int_st().read().bits();
    // Before the clear: the split reader reads the raw status and then the loss count, so a
    // loss it misses in one it sees in the other.
    split::note_loss();
    b.int_clr().write(|w| unsafe { w.bits(st) });
    critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        s.stats.bridge_errors = s.stats.bridge_errors.wrapping_add(1);
        s.stats.bridge_error_bits |= st;
        if st & (1 << 3) != 0 {
            s.stats.bridge_overruns = s.stats.bridge_overruns.wrapping_add(1);
        }
        if s.running {
            s.damaged = true;
        }
    });
}

// ── Strip ───────────────────────────────────────────────────────────────────────────────

/// A finished strip: [`Strip::rows`] full rows of one frame, cache-coherent. Derefs to the
/// `rows * line_bytes` pixel bytes, row-major (RAW10 packed, see [`crate::DATA_TYPE_RAW10`]).
///
/// Dropping it (or passing it to the next [`CsiStrips::wait_strip`]) gives the buffer back to
/// the DMA. The DMA halts and the frame is lost when it reaches a strip that is still held.
pub struct Strip {
    data: &'static [u8],
    slot: u8,
    generation: u32,
    meta: Meta,
}

impl Strip {
    /// Frame number: frame ends seen since [`CsiStrips::start`]. Gaps mean frames were lost.
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

    /// CPU cycle counter (`mcycle`, low 32 bits) when the DMA handler queued this strip, for
    /// latency measurements against the caller's own `mcycle` reads.
    pub fn queued_at_cycles(&self) -> u32 {
        self.meta.queued_at
    }
}

impl Deref for Strip {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.data
    }
}

impl Drop for Strip {
    fn drop(&mut self) {
        critical_section::with(|cs| {
            let mut s = STATE.borrow_ref_mut(cs);
            let bit = 1 << self.slot;
            if s.generation != self.generation || s.held & bit == 0 {
                return;
            }
            s.held &= !bit;
            if s.running {
                s.rearm(self.slot);
                s.kick();
            }
        });
    }
}

// ── Driver ──────────────────────────────────────────────────────────────────────────────

/// The MIPI CSI-2 receiver in line-strip mode. Shares the hardware with [`crate::Csi`]: only one
/// of the two can exist at a time.
pub struct CsiStrips<'d> {
    config: Config,
    lines: u16,
    hsfreq: u8,
    hsfreq_readback: Option<u8>,
    channel: u8,
    generation: u32,
    running: bool,
    /// [`CsiStrips::split`] ran: `self` lives on inside a [`StripControl`].
    split: bool,
    chip_rev: u16,
    _vdma: PhantomData<&'d mut ()>,
}

impl<'d> CsiStrips<'d> {
    /// Bytes of one full strip: [`Config::line_bytes`] × `lines`.
    pub const fn strip_bytes(config: &Config, lines: u16) -> usize {
        config.line_bytes() * lines as usize
    }

    /// Smallest valid strip height: strips must be whole bridge bursts of [`BURST_BYTES`], so
    /// `lines` must be a multiple of `4096 / gcd(line_bytes, 4096)`. RAW8: 1280 wide → 16,
    /// 640 → 32, 320 → 64. RAW10 (packed): 1280 wide (1600 B) → 64, 640 (800 B) → 128,
    /// 320 (400 B) → 256.
    pub const fn line_step(config: &Config) -> u16 {
        let line = config.line_bytes();
        // gcd(line, BURST_BYTES) with BURST_BYTES a power of two: the lowest set bit of line,
        // capped at BURST_BYTES.
        let common = if line == 0 {
            BURST_BYTES
        } else {
            let low = 1usize << line.trailing_zeros();
            if low < BURST_BYTES { low } else { BURST_BYTES }
        };
        (BURST_BYTES / common) as u16
    }

    /// Strips per frame: `ceil(height / lines)`, the last one short.
    pub const fn strips_per_frame(config: &Config, lines: u16) -> usize {
        (config.height as usize).div_ceil(lines as usize)
    }

    /// Sets up clocks, resets and the DMA channel in linked-list mode and takes ownership of
    /// the strip buffers (2..=[`MAX_STRIPS`]) and their list items. Does not start capture.
    ///
    /// - `strips[i]`: at least [`CsiStrips::strip_bytes`] long for every configuration used,
    ///   start and length multiples of the cache line (64 B in internal RAM).
    /// - `links`: one item per strip, in internal L2MEM (a `static`). The driver writes back
    ///   and drops their cache lines once and from then on uses the non-cacheable alias.
    /// - `lines`: rows per strip, a multiple of [`CsiStrips::line_step`] (whole bridge bursts),
    ///   below `height`, and `height` must not be a multiple of `lines` (see the module docs).
    ///   RAW8: 64 rows suit 1280x800, 1280x720, 640x480, 640x400 and 320x200. RAW10: 64 rows
    ///   for 1280x800 and 1280x720, 128 for 640x400 (strips of 102 400 B).
    pub fn new<const N: usize>(
        vdma_channel: impl VdmaDmaChannel + 'd,
        strips: [&'static mut [u8]; N],
        links: &'static mut [StripLink; N],
        config: Config,
        lines: u16,
    ) -> Result<Self, StripError> {
        let hsfreq = config.validate()?;
        check_geometry(&config, lines)?;
        if !(2..=MAX_STRIPS).contains(&N) {
            return Err(StripError::StripCount);
        }
        let links_addr = links.as_ptr() as usize;
        let links_len = core::mem::size_of_val(links);
        if !cache::in_l2mem(links_addr, links_len) {
            return Err(StripError::LinksNotInternal);
        }
        let mut table = [(0usize, 0usize); MAX_STRIPS];
        for (i, b) in strips.iter().enumerate() {
            let (addr, len) = (b.as_ptr() as usize, b.len());
            let align = cache::line_size(addr);
            if addr % align != 0 || len % align != 0 {
                return Err(StripError::StripAlignment {
                    index: i as u8,
                    align,
                });
            }
            table[i] = (addr, len);
        }
        check_lengths(&table[..N], &config, lines)?;
        if TAKEN.swap(true, core::sync::atomic::Ordering::Acquire) {
            return Err(Error::AlreadyInUse.into());
        }
        let channel = vdma_channel.channel_id();
        // Stale dirty lines must never land on what the DMA and the non-cacheable accesses
        // write later.
        cache::writeback_invalidate(links_addr, links_len);
        for &(addr, len) in &table[..N] {
            cache::invalidate(addr, len);
        }
        // The buffers and items now belong to the driver.
        let _ = (strips, links);

        let generation = critical_section::with(|cs| {
            let mut s = STATE.borrow_ref_mut(cs);
            let generation = s.generation.wrapping_add(1);
            *s = State::new();
            s.generation = generation;
            s.channel = channel;
            s.count = N as u8;
            for (i, &entry) in table[..N].iter().enumerate() {
                (s.strips[i], s.lens[i]) = entry;
                s.links[i] = links_addr + i * core::mem::size_of::<StripLink>();
            }
            s.geom = Geom {
                line_bytes: config.line_bytes() as u32,
                height: config.height,
                lines,
            };
            generation
        });

        let chip_rev = chip_revision();
        enable_clocks();
        // TRM 38.7.3: linked list for source and destination.
        init_dma_channel(channel, 3, BLOCK_TFR_DONE | DMA_TFR_DONE | DMA_ERR_MASK);

        interrupt::bind_handler(Interrupt::DMA, dma_isr);
        interrupt::bind_handler(Interrupt::CSI, host_isr);
        interrupt::bind_handler(Interrupt::CSI_BRIDGE, bridge_isr);

        Ok(Self {
            config,
            lines,
            hsfreq,
            hsfreq_readback: None,
            channel,
            generation,
            running: false,
            split: false,
            chip_rev,
            _vdma: PhantomData,
        })
    }

    /// Current configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Rows per strip.
    pub fn strip_lines(&self) -> u16 {
        self.lines
    }

    /// Changes geometry, lane rate and/or rows per strip. Only while stopped; every strip
    /// buffer must be long enough for the new strip size.
    pub fn set_config(&mut self, config: Config, lines: u16) -> Result<(), StripError> {
        if self.running {
            return Err(Error::Running.into());
        }
        let hsfreq = config.validate()?;
        check_geometry(&config, lines)?;
        critical_section::with(|cs| {
            let mut s = STATE.borrow_ref_mut(cs);
            let n = s.count as usize;
            let mut table = [(0usize, 0usize); MAX_STRIPS];
            for (i, t) in table[..n].iter_mut().enumerate() {
                *t = (s.strips[i], s.lens[i]);
            }
            check_lengths(&table[..n], &config, lines)?;
            s.geom = Geom {
                line_bytes: config.line_bytes() as u32,
                height: config.height,
                lines,
            };
            Ok::<(), StripError>(())
        })?;
        self.config = config;
        self.lines = lines;
        self.hsfreq = hsfreq;
        Ok(())
    }

    /// Brings up the link as [`crate::Csi::start`] does, with the bridge cutting blocks of
    /// `lines` rows, writes every list item and starts the channel on the first one. Call it
    /// before the sensor's stream-on. All strips must have been returned. The first two frames
    /// are used to find and check the frame boundary and are not delivered.
    pub fn start(&mut self) -> Result<(), StripError> {
        if self.running {
            return Err(Error::Running.into());
        }
        if self.split {
            // A poll the reader began before the last stop may still write a list item; the
            // items are written below only after it has left.
            if !split::wait_reader() {
                return Err(StripError::ReaderBusy);
            }
        } else {
            let held = critical_section::with(|cs| STATE.borrow_ref(cs).held);
            if held != 0 {
                return Err(StripError::StripsHeld);
            }
        }
        let split = self.split;
        let cfg = self.config;
        let ch = self.channel;
        let bursts = (Self::strip_bytes(&cfg, self.lines) / BURST_BYTES) as u16;
        self.hsfreq_readback = Some(configure_link(&cfg, self.hsfreq, self.chip_rev, bursts));

        critical_section::with(|cs| {
            let mut s = STATE.borrow_ref_mut(cs);
            s.q_head = 0;
            s.q_len = 0;
            s.armed = 0;
            for slot in 0..s.count {
                s.write_link(slot);
            }
            s.dma_next = 0;
            s.track = Tracker::NEW;
            s.damaged = false;
            s.halted = false;
            s.running = true;
            s.config = cfg;
            s.chip_rev = self.chip_rev;
            s.bursts = bursts;
            if split {
                // Before the channel runs: the reader takes the new geometry with this run.
                split::publish_start(&s);
            }
            let c = dma().ch(ch as usize);
            c.intclear0().write(|w| unsafe { w.bits(u32::MAX) });
            c.llp0()
                .write(|w| unsafe { w.bits(s.links[0] as u32 | LLP_LMS_MEMORY) });
            c.llp1().write(|w| unsafe { w.bits(0) });
            channel_enable(ch, true);
        });
        bridge().csi_en().modify(|_, w| w.csi_brig_en().set_bit());
        self.running = true;
        Ok(())
    }

    /// Stops capture as [`crate::Csi::stop`] does. Strips not yet handed out are dropped;
    /// strips held by the application stay valid. A pending [`CsiStrips::wait_strip`] returns
    /// `NotRunning`.
    pub fn stop(&mut self) {
        let ch = self.channel;
        if self.split {
            // First of all: from here on the reader writes no list item and reports every
            // strip it handed out as not intact.
            split::publish_stop();
        }
        bridge().csi_en().modify(|_, w| w.csi_brig_en().clear_bit());
        critical_section::with(|cs| {
            let mut s = STATE.borrow_ref_mut(cs);
            s.running = false;
            s.q_len = 0;
        });
        halt_channel(ch);
        reset_link();
        self.running = false;
        WAKER.wake();
    }

    /// `true` between [`CsiStrips::start`] and [`CsiStrips::stop`].
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Gives `previous` back to the DMA and waits for the next strip, strictly in capture
    /// order. Invalidates the strip's cache lines before returning it. Never blocks the
    /// executor; returns `NotRunning` if the receiver is (or gets) stopped. Wrap it in
    /// `embassy_time::with_timeout` to detect a dead link.
    pub async fn wait_strip(&mut self, previous: Option<Strip>) -> Result<Strip, StripError> {
        drop(previous);
        let (slot, meta, addr) = poll_fn(|cx| {
            WAKER.register(cx.waker());
            critical_section::with(|cs| {
                let mut s = STATE.borrow_ref_mut(cs);
                if let Some(slot) = s.pop() {
                    s.held |= 1 << slot;
                    s.stats.strips_delivered = s.stats.strips_delivered.wrapping_add(1);
                    return Poll::Ready(Ok((slot, s.meta[slot as usize], s.strips[slot as usize])));
                }
                if !s.running {
                    return Poll::Ready(Err(StripError::Csi(Error::NotRunning)));
                }
                Poll::Pending
            })
        })
        .await?;
        let len = meta.rows as usize * self.config.line_bytes();
        let line = cache::line_size(addr);
        let t0 = cycles();
        cache::invalidate(addr, len.div_ceil(line) * line);
        let dt = cycles().wrapping_sub(t0);
        critical_section::with(|cs| {
            let st = &mut STATE.borrow_ref_mut(cs).stats;
            st.invalidate_cycles_total = st.invalidate_cycles_total.wrapping_add(dt as u64);
            st.invalidate_cycles_max = st.invalidate_cycles_max.max(dt);
        });
        Ok(Strip {
            // SAFETY: the slot is held (its item invalid): the DMA does not write it until
            // the strip is dropped.
            data: unsafe { core::slice::from_raw_parts(addr as *const u8, len) },
            slot,
            generation: self.generation,
            meta,
        })
    }

    /// Event counters. After [`CsiStrips::split`] the block and frame counters come from the
    /// reader, as it last published them (at the end of each of its polls).
    pub fn stats(&self) -> StripStats {
        let mut st = critical_section::with(|cs| STATE.borrow_ref(cs).stats);
        if self.split {
            split::reader_stats(&mut st);
        }
        st
    }

    /// Zeroes the counters. After [`CsiStrips::split`] the reader zeroes its own at its next
    /// poll.
    pub fn reset_stats(&mut self) {
        critical_section::with(|cs| STATE.borrow_ref_mut(cs).stats = StripStats::ZERO);
        if self.split {
            split::request_stats_reset();
        }
    }

    /// D-PHY lane state.
    pub fn phy_status(&self) -> PhyStatus {
        dphy::status(host())
    }

    /// Register and ring snapshot, for debugging. Clears nothing.
    pub fn debug_status(&self) -> StripDebug {
        let link = snapshot(self.channel, self.hsfreq_readback);
        let dma_llp = dma().ch(self.channel as usize).llp0().read().bits();
        critical_section::with(|cs| {
            let s = STATE.borrow_ref(cs);
            let mut d = StripDebug {
                link,
                dma_llp,
                dma_next: s.dma_next,
                armed: s.armed,
                held: s.held,
                queued: s.q_len,
                synced: s.track.sync == Sync::Synced,
                halted: s.halted,
            };
            if s.split {
                split::debug(&s, &mut d);
            }
            d
        })
    }

    /// Chip revision ×100 from eFuse (e.g. 302 for v3.2).
    pub fn chip_revision(&self) -> u16 {
        self.chip_rev
    }
}

impl Drop for CsiStrips<'_> {
    fn drop(&mut self) {
        if self.running {
            self.stop();
        }
        if self.split {
            split::detach();
        }
        let cpu = Cpu::current();
        interrupt::disable(cpu, Interrupt::DMA);
        interrupt::disable(cpu, Interrupt::CSI);
        interrupt::disable(cpu, Interrupt::CSI_BRIDGE);
        TAKEN.store(false, core::sync::atomic::Ordering::Release);
    }
}

fn check_geometry(config: &Config, lines: u16) -> Result<(), StripError> {
    let step = CsiStrips::line_step(config);
    if lines == 0 || !lines.is_multiple_of(step) {
        return Err(StripError::StripLines { step });
    }
    let bytes = CsiStrips::strip_bytes(config, lines);
    let ok = lines < config.height
        && bytes / BURST_BYTES <= 0x1FFF
        && !config.height.is_multiple_of(lines);
    if ok {
        Ok(())
    } else {
        Err(StripError::StripGeometry)
    }
}

fn check_lengths(strips: &[(usize, usize)], config: &Config, lines: u16) -> Result<(), StripError> {
    let needed = CsiStrips::strip_bytes(config, lines);
    for (i, &(_, len)) in strips.iter().enumerate() {
        if len < needed {
            return Err(StripError::StripTooSmall {
                index: i as u8,
                needed,
            });
        }
    }
    Ok(())
}
