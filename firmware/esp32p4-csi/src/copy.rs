//! Memory-to-memory copies on a DW-GDMA ("VDMA") channel the receiver does not use: one
//! contiguous block per copy, the DMA its own flow controller, started and polled with plain
//! register accesses.
//!
//! Made for the strip path: a strip in internal RAM copied into a frame buffer in PSRAM while
//! the CSI keeps channel 0. [`DmaCopy::new`] runs on core 0 (it may enable the DMA's clocks
//! and the controller, behind esp-hal's critical section); after that, [`DmaCopy::start`],
//! [`DmaCopy::poll`], [`DmaCopy::wait`] and [`DmaCopy::abort`] use no critical section, no
//! interrupt, no lock and no esp-hal call, so a bare second core may own the copier.
//!
//! # Set-up (TRM 7.5, ESP-IDF `dw_gdma.c` and its "M2M Test: Contiguous Mode")
//!
//! - `CFG0`: contiguous source and destination (multi-block type 0): one block per transfer.
//! - `CFG1`: `TT_FC` = 0 (memory to memory, the DMA is the flow controller), priority 0 (the
//!   CSI's channel has 1, and a bigger number wins: the copy never takes the bus from the
//!   receiver), four outstanding requests each side as the receiver's channel.
//! - `CTL0`: both ends on master 1 (L2MEM, PSRAM), both incrementing, the widest transfer
//!   width the addresses and the length allow (64, 32, 16 or 8 bits), 16-item bursts.
//! - `CTL1`: AXI read and write bursts of 16 beats, as the receiver's channel.
//! - Interrupts: every event sets its status bit, none signals. The DW-GDMA has one interrupt
//!   line for its four channels, which the receiver binds; a copy channel that signalled would
//!   call the receiver's handler for nothing. Completion is the channel's `TFR_DONE` status
//!   bit, polled.
//!
//! The controller is reset only if it is not enabled yet, as the receiver's own set-up does:
//! a reset would kill a capture in progress. Do not create esp-hal's DSI `VdmaChannel`, which
//! resets it unconditionally.
//!
//! # Memory and cache
//!
//! Master 1 reaches internal L2MEM and PSRAM only (IDF `dw_gdma_ll.h`); [`DmaCopy::start`]
//! refuses anything else. The DMA reads and writes memory, not the caches: the source must
//! have no dirty cache lines (write it back first if the CPU wrote it), and the CPU must
//! invalidate the destination's lines after the copy before it reads them
//! (`esp32p4-cache`). The copier does no cache maintenance itself.

use core::marker::PhantomData;

use esp_hal::mipi_dsi::VdmaDmaChannel;

use crate::strips::cycles;
use crate::{DMA_ERR_MASK, DMA_TFR_DONE, channel_enable, dma, enable_controller, halt_channel};

/// PSRAM's address window (the cached one, which the DMA uses as well).
const PSRAM: core::ops::Range<usize> = 0x4800_0000..0x4C00_0000;
/// Largest block: `BLOCK_TS` is 22 bits of items minus one.
const MAX_ITEMS: usize = 1 << 22;

// CTL0 fields (the receiver's `CTL0_*` layout).
const CTL0_SMS_MEM: u32 = 1; // bit 0: source on master 1
const CTL0_DMS_MEM: u32 = 1 << 2; // bit 2: destination on master 1
const CTL0_SRC_TR_WIDTH: u32 = 8;
const CTL0_DST_TR_WIDTH: u32 = 11;
/// MSIZE code 3: 16 items per burst, both sides.
const CTL0_MSIZE_16: u32 = (3 << 14) | (3 << 18);
/// CTL1: ARLEN_EN, ARLEN = 16, AWLEN_EN, AWLEN = 16.
const CTL1: u32 = (1 << 6) | (16 << 7) | (1 << 15) | (16 << 16);

/// Why a copy did not start or did not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyError {
    /// A copy is in progress (not yet seen complete by [`DmaCopy::poll`]).
    Busy,
    /// An end of the source or destination is outside L2MEM and PSRAM, or the length is 0 or
    /// beyond one block.
    Range,
    /// The channel reported an error: its `INTSTATUS` bits.
    Dma(u32),
    /// [`DmaCopy::wait`] gave up.
    Timeout,
}

/// One DW-GDMA channel used for memory-to-memory copies, one at a time. `Send`: create it on
/// core 0, use it anywhere.
pub struct DmaCopy<'d> {
    ch: u8,
    busy: bool,
    _channel: PhantomData<&'d mut ()>,
}

impl<'d> DmaCopy<'d> {
    /// Takes `vdma_channel` (not the receiver's), enables the DMA's clocks and the controller
    /// if nobody did yet, and sets the channel up for copies. Call on core 0.
    pub fn new(vdma_channel: impl VdmaDmaChannel + 'd) -> Self {
        let ch = vdma_channel.channel_id();
        enable_controller();
        channel_enable(ch, false);
        let c = dma().ch(ch as usize);
        c.cfg0().write(|w| unsafe {
            w.ch1_src_multblk_type().bits(0);
            w.ch1_dst_multblk_type().bits(0)
        });
        c.cfg1().write(|w| unsafe {
            w.ch1_tt_fc().bits(0);
            w.ch1_hs_sel_src().clear_bit();
            w.ch1_hs_sel_dst().clear_bit();
            w.ch1_ch_prior().bits(0);
            w.ch1_src_osr_lmt().bits(4);
            w.ch1_dst_osr_lmt().bits(4)
        });
        c.intclear0().write(|w| unsafe { w.bits(u32::MAX) });
        c.intstatus_enable0().write(|w| unsafe { w.bits(u32::MAX) });
        c.intsignal_enable0().write(|w| unsafe { w.bits(0) });
        Self {
            ch,
            busy: false,
            _channel: PhantomData,
        }
    }

    /// Starts copying `len` bytes from `src` to `dst`. The widest transfer width that divides
    /// both addresses and the length is used (8 bytes for strips and frame buffers).
    ///
    /// # Safety
    ///
    /// Until [`DmaCopy::poll`] or [`DmaCopy::wait`] reports the end (or [`DmaCopy::abort`]
    /// returned), `src..src + len` must stay valid and unchanged, and `dst..dst + len` must be
    /// writable and touched by nobody else. Cache: see the module docs.
    pub unsafe fn start(&mut self, src: usize, dst: usize, len: usize) -> Result<(), CopyError> {
        if self.busy {
            return Err(CopyError::Busy);
        }
        let width = (src | dst | len).trailing_zeros().min(3);
        let items = len >> width;
        let reachable = |a: usize| {
            esp32p4_cache::in_l2mem(a, len) || (PSRAM.contains(&a) && a + len <= PSRAM.end)
        };
        if len == 0 || items > MAX_ITEMS || !reachable(src) || !reachable(dst) {
            return Err(CopyError::Range);
        }
        let c = dma().ch(self.ch as usize);
        c.intclear0().write(|w| unsafe { w.bits(u32::MAX) });
        c.sar0().write(|w| unsafe { w.bits(src as u32) });
        c.sar1().write(|w| unsafe { w.bits(0) });
        c.dar0().write(|w| unsafe { w.bits(dst as u32) });
        c.dar1().write(|w| unsafe { w.bits(0) });
        c.block_ts0().write(|w| unsafe { w.bits(items as u32 - 1) });
        c.ctl0().write(|x| unsafe {
            x.bits(
                CTL0_SMS_MEM
                    | CTL0_DMS_MEM
                    | width << CTL0_SRC_TR_WIDTH
                    | width << CTL0_DST_TR_WIDTH
                    | CTL0_MSIZE_16,
            )
        });
        c.ctl1().write(|x| unsafe { x.bits(CTL1) });
        channel_enable(self.ch, true);
        self.busy = true;
        Ok(())
    }

    /// `None` while the copy runs (or none was started); `Some(Ok(()))` once, when it
    /// completed; `Some(Err(_))` once, if the channel reported an error (the channel is
    /// stopped then).
    pub fn poll(&mut self) -> Option<Result<(), CopyError>> {
        if !self.busy {
            return None;
        }
        let c = dma().ch(self.ch as usize);
        let st = c.intstatus0().read().bits();
        if st & DMA_ERR_MASK != 0 {
            self.abort();
            return Some(Err(CopyError::Dma(st)));
        }
        if st & DMA_TFR_DONE == 0 {
            return None;
        }
        c.intclear0().write(|w| unsafe { w.bits(u32::MAX) });
        self.busy = false;
        Some(Ok(()))
    }

    /// Spins until the copy ends, at most `max_cycles` CPU cycles (then the copy is aborted).
    /// `Ok(())` if no copy was in progress.
    pub fn wait(&mut self, max_cycles: u32) -> Result<(), CopyError> {
        let t0 = cycles();
        while self.busy {
            if let Some(r) = self.poll() {
                return r;
            }
            if cycles().wrapping_sub(t0) > max_cycles {
                self.abort();
                return Err(CopyError::Timeout);
            }
            core::hint::spin_loop();
        }
        Ok(())
    }

    /// A copy is in progress.
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Stops the channel (aborting a copy in progress: the destination is then partly
    /// written) and clears its status.
    pub fn abort(&mut self) {
        halt_channel(self.ch);
        self.busy = false;
    }
}

impl Drop for DmaCopy<'_> {
    fn drop(&mut self) {
        if self.busy {
            self.abort();
        }
    }
}
