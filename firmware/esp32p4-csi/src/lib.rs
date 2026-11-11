//! MIPI CSI-2 receiver driver for the ESP32-P4.
//!
//! esp-hal 1.2.2 has no MIPI CSI, camera or ISP driver for the P4. This crate fills the gap on
//! the `esp32p4` PAC for exactly one use: **2 data lanes, RAW8 (or another 8-bit data type)
//! passed through the ISP unchanged, continuous capture into caller-supplied frame buffers**.
//! espion uses it for the OV9281. See `docs/architecture.md`.
//!
//! Data path: D-PHY → CSI-2 host → ISP header/tail with the ISP pipeline disabled (bypass) →
//! CSI bridge → DW-GDMA ("VDMA") channel, single block per frame, bridge as flow controller →
//! memory (PSRAM or internal RAM). The sequence follows ESP-IDF `esp_cam_ctlr_csi.c`,
//! `mipi_csi_hal.c`, `esp_isp_new_processor` (bypass) and `dw_gdma.c`, and the TRM chapters 7
//! (VDMA), 38.7 (ISP/CSI bridge) and 42.7 (MIPI CSI).
//!
//! # Peripheral ownership
//!
//! esp-hal's `Peripherals` has `VDMA_CH0`..`VDMA_CH3` singletons, and [`Csi::new`] takes one.
//! It has **no** singleton for the CSI host, the CSI bridge or the ISP: this driver takes those
//! PAC blocks unsafely (`esp32p4::MIPI_CSI_HOST::ptr()` etc.) and guards against a second
//! instance with an atomic flag. Nothing else in the firmware may touch them. The driver also
//! binds the `DMA` (DW-GDMA, shared by all four VDMA channels), `CSI` and `CSI_BRIDGE`
//! interrupts, so it cannot run next to esp-hal's MIPI-DSI driver, which binds `DMA` as well.
//!
//! # Example
//!
//! ```no_run
//! # async fn run(p: esp_hal::peripherals::Peripherals, bufs: [&'static mut [u8]; 2]) {
//! use esp32p4_csi::{Config, Csi, LdoChannel, enable_ldo};
//!
//! // VDD_MIPI_DPHY comes from LDO VO3 on the ESP32-P4-Core and on the ESP32-P4-ETH.
//! enable_ldo(LdoChannel::Vo3, 2500).unwrap();
//! embassy_time::Timer::after_micros(500).await;
//!
//! // Buffers: `Csi::buffer_len(&config)` bytes each, aligned to the cache line.
//! let config = Config::new(640, 400, 800);
//! let mut csi = Csi::new(p.VDMA_CH0, bufs, config).unwrap();
//! csi.start().unwrap();
//! // ...now tell the sensor to stream (receiver first, then sensor stream-on)...
//!
//! let mut frame = None;
//! for _ in 0..1000 {
//!     // Gives the previous frame's buffer back to the driver and waits for the next one.
//!     let f = csi.wait_frame(frame.take()).await.unwrap();
//!     let pixels: &[u8] = &f; // config.frame_bytes() bytes, row-major
//!     let _ = pixels[0];
//!     frame = Some(f);
//! }
//! drop(frame);
//!
//! // Switch mode: stop the sensor stream, then
//! csi.stop();
//! csi.set_config(Config::new(1280, 800, 800)).unwrap();
//! csi.start().unwrap();
//! # }
//! ```
//!
//! `wait_frame` never blocks; wrap it in `embassy_time::with_timeout` to detect a dead link, then
//! print [`Csi::debug_status`] and [`Csi::stats`].
//!
//! The buffer swap runs in the DMA interrupt between two frames. Anything that keeps interrupts
//! masked for longer than the sensor's vertical blanking (for example `esp-println` writing a
//! log line at 115200 baud inside its critical section) makes the next frame start before the
//! DMA is re-armed, and that frame arrives short ([`Frame::received_len`]).
//!
//! # Line strips
//!
//! [`CsiStrips`] is the second mode of the same hardware (only one of the two drivers can
//! exist at a time): the bridge cuts each frame into DMA blocks of a fixed number of rows, a
//! circular linked list of [`StripLink`]s fills a small ring of strip buffers in internal RAM,
//! and [`CsiStrips::wait_strip`] hands out [`Strip`]s in order with their frame number and
//! first row. See the [`strips`] module docs for the frame-boundary method and an example.

#![no_std]

mod dphy;
mod ldo;
pub mod strips;

use core::{
    cell::RefCell,
    future::poll_fn,
    marker::PhantomData,
    ops::Deref,
    sync::atomic::{AtomicBool, Ordering},
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
use esp32p4 as pac;
// Cache maintenance, behind the lock shared with every other cache operation of the firmware.
use esp32p4_cache as cache;

pub use dphy::{MAX_MBPS, MIN_MBPS, PhyStatus, hsfreqrange};
pub use ldo::{LdoChannel, LdoError, LdoSetting, disable_ldo, enable_ldo};
pub use strips::{CsiStrips, MAX_STRIPS, Strip, StripDebug, StripError, StripLink, StripStats};

/// Number of active data lanes. Fixed: the P4 has two and this driver uses both.
pub const LANES: u8 = 2;
/// CSI-2 data type RAW8.
pub const DATA_TYPE_RAW8: u8 = 0x2A;
/// CSI-2 data type RAW10. In memory the frame keeps the CSI-2 packed layout: per 4 pixels, 4
/// bytes with bits 9..2 of pixels 0..3, then 1 byte with their bits 1..0 (pixel 0 in bits
/// 1..0); a line is `width * 5 / 4` bytes, no padding (see [`Config::line_bytes`]).
pub const DATA_TYPE_RAW10: u8 = 0x2B;
/// Most frame buffers the driver rotates.
pub const MAX_BUFFERS: usize = 4;

/// Fixed DW-GDMA source address of the CSI bridge FIFO (`MIPI_CSI_BRG_MEM_BASE`, TRM 9.3
/// "MIPI CSI internal memory 0x5010_4000 ~ 0x5010_4FFF").
const CSI_BRG_MEM_BASE: u32 = 0x5010_4000;
/// Bridge `DMA_BURST_LEN` in 64-bit items; matches the channel's `SRC_MSIZE` = 512 items (IDF).
const BURST_ITEMS: u32 = 512;
/// One bridge DMA burst in bytes.
pub const BURST_BYTES: usize = BURST_ITEMS as usize * 8;
/// Bridge `CSI_BUF_AFULL_THRD` (IDF `mipi_csi_hal_init`).
const BRIDGE_AFULL_THRD: u32 = 960;

/// Capture geometry and link rate. Change it with [`Csi::set_config`] while stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Pixels per line. The line (`width` × bits per pixel) must be a whole number of 32-bit
    /// words: `width` a multiple of 4 for RAW8. RAW10 needs a multiple of 64 (the ISP word count
    /// `width * 10 / 32` must be a multiple of 4, TRM `ISP_HVNUM_SETTING_ERR_INT`) and an even
    /// `height`.
    pub width: u16,
    /// Lines per frame.
    pub height: u16,
    /// Bit rate per data lane in Mbit/s (selects the D-PHY `hsfreqrange`), 90..=1500.
    pub lane_bit_rate_mbps: u16,
    /// CSI-2 data type that passes the bridge filter (`DATA_TYPE_MIN = MAX`); other packets,
    /// e.g. embedded data (0x12), are dropped. Default [`DATA_TYPE_RAW8`]; [`DATA_TYPE_RAW10`]
    /// is 10 bits per pixel, every other value is treated as 8 bits per pixel.
    pub data_type: u8,
    /// The sensor sends line-start/line-end short packets (ISP `HSYNC_START/END_EXIST`).
    /// The OV9281 setup used by espion does not. Default `false`.
    pub line_sync: bool,
}

impl Config {
    /// RAW8, no line short packets.
    pub const fn new(width: u16, height: u16, lane_bit_rate_mbps: u16) -> Self {
        Self {
            width,
            height,
            lane_bit_rate_mbps,
            data_type: DATA_TYPE_RAW8,
            line_sync: false,
        }
    }

    /// RAW10 (CSI-2 packed in memory), no line short packets.
    pub const fn raw10(width: u16, height: u16, lane_bit_rate_mbps: u16) -> Self {
        Self {
            data_type: DATA_TYPE_RAW10,
            ..Self::new(width, height, lane_bit_rate_mbps)
        }
    }

    /// Bits per pixel on the link and in memory: 10 for [`DATA_TYPE_RAW10`], else 8.
    pub const fn bits_per_pixel(&self) -> u32 {
        if self.data_type == DATA_TYPE_RAW10 {
            10
        } else {
            8
        }
    }

    /// Bytes of one line in memory: `width` for RAW8, `width * 5 / 4` for RAW10 (packed).
    pub const fn line_bytes(&self) -> usize {
        self.width as usize * self.bits_per_pixel() as usize / 8
    }

    /// Bytes of one frame: `line_bytes() * height` (`width * height` for RAW8).
    pub const fn frame_bytes(&self) -> usize {
        self.line_bytes() * self.height as usize
    }

    /// Bridge bursts the frame needs.
    const fn bursts(&self) -> usize {
        self.frame_bytes().div_ceil(BURST_BYTES)
    }

    /// 32-bit Image Interface words per line: what the ISP header counts in bypass (IDF
    /// `esp_isp_new_processor`: `ceil(h_res * in_bits_per_pixel / 32)`).
    const fn line_words(&self) -> u32 {
        (self.width as u32 * self.bits_per_pixel()).div_ceil(32)
    }

    fn validate(&self) -> Result<u8, Error> {
        // Bridge HADR_NUM/VADR_NUM and ISP HADR/VADR are 12-bit fields.
        if self.width == 0 || self.height == 0 || self.width > 4095 || self.height > 4095 {
            return Err(Error::InvalidGeometry);
        }
        // DMA moves 64-bit items (IDF: frame size must be a whole number of them; TRM 38.7.3:
        // image dimensions aligned to 8 bytes). ISP bypass counts 32-bit words per line, so a
        // line must be a whole number of them (RAW8: width % 4; RAW10: width % 16).
        let line_bits = self.width as u32 * self.bits_per_pixel();
        if !self.frame_bytes().is_multiple_of(8) || !line_bits.is_multiple_of(32) {
            return Err(Error::InvalidGeometry);
        }
        // TRM 38.6 `ISP_HVNUM_SETTING_ERR_INT`: "image dimensions must be even; with the RAW10
        // input, HNUM must be a multiple of 4". In bypass HNUM is the word count.
        if self.data_type == DATA_TYPE_RAW10
            && (!self.line_words().is_multiple_of(4) || !self.height.is_multiple_of(2))
        {
            return Err(Error::InvalidGeometry);
        }
        hsfreqrange(self.lane_bit_rate_mbps).ok_or(Error::InvalidBitRate)
    }
}

impl Default for Config {
    /// 640×400 RAW8 at 800 Mbit/s per lane (the OV9281 2×2-binned mode).
    fn default() -> Self {
        Self::new(640, 400, 800)
    }
}

/// Driver errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A [`Csi`] already exists.
    AlreadyInUse,
    /// Width/height out of range or not aligned (see [`Config::width`]).
    InvalidGeometry,
    /// Lane bit rate outside [`MIN_MBPS`]..=[`MAX_MBPS`].
    InvalidBitRate,
    /// Fewer than 2 or more than [`MAX_BUFFERS`] buffers.
    BufferCount,
    /// Buffer `index` is shorter than [`Csi::buffer_len`] for the configuration.
    BufferTooSmall { index: u8, needed: usize },
    /// Buffer `index` start or length is not a multiple of `align` (the cache line).
    BufferAlignment { index: u8, align: usize },
    /// The operation needs the receiver stopped.
    Running,
    /// The receiver is stopped (also returned by a pending [`Csi::wait_frame`] on `stop`).
    NotRunning,
}

/// Event counters since [`Csi::new`] (or [`Csi::reset_stats`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// DMA transfers completed (one per frame the bridge delivered).
    pub frames_done: u32,
    /// Completed frames thrown away because no free buffer was available: the DMA was
    /// re-armed into the same buffer.
    pub frames_dropped: u32,
    /// Completed frames replaced by a newer one before [`Csi::wait_frame`] took them.
    pub frames_overwritten: u32,
    /// Bytes the DMA wrote for the last completed frame: final `DAR0` minus the buffer start.
    /// Differs from [`Config::frame_bytes`] when the sensor geometry does not match the
    /// configuration.
    pub last_frame_bytes: u32,
    /// `CMPLTD_BLK_TFR_SIZE` of the last completed frame as the channel reports it (64-bit
    /// items). Measured on the ESP32-P4 v3.2: one item less than the DMA wrote.
    pub last_block_items: u32,
    /// DW-GDMA channel error interrupts.
    pub dma_errors: u32,
    /// OR of all DW-GDMA channel `INTSTATUS` bits seen with an error.
    pub dma_error_bits: u32,
    /// CSI host error interrupts.
    pub host_errors: u32,
    /// OR of `CSI_HOST_INT_ST_MAIN` (bit 0 PHY fatal, 1 packet fatal, 2 frame boundary,
    /// 3 frame sequence, 4 frame CRC, 5 payload CRC, 6 data ID, 7 ECC corrected, 16 PHY).
    pub host_error_main: u32,
    /// OR of the per-category status registers, indexed like the `INT_ST_MAIN` bits 0..=7 and
    /// index 8 for `INT_ST_PHY` (bits 0/1 `ErrSotHS` lane 0/1, 16/17 `ErrEsc` lane 0/1).
    /// Index 0 is `INT_ST_PHY_FATAL` (bits 0/1 `ErrSotSyncHS` lane 0/1).
    pub host_error_detail: [u32; 9],
    /// CSI bridge error interrupts.
    pub bridge_errors: u32,
    /// OR of `CSI_BRIG_INT_ST` (bit 2 incomplete frame discarded, 3 buffer overrun,
    /// 4 async FIFO overflow).
    pub bridge_error_bits: u32,
}

/// Register snapshot for "no frames arrive" debugging. Reading it clears nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebugStatus {
    /// D-PHY lane states.
    pub phy: PhyStatus,
    /// `hsfreqrange` read back from D-PHY test code 0x44 at the last [`Csi::start`]
    /// (`None` before the first start). Should equal [`hsfreqrange`] of the lane rate.
    pub hsfreqrange_readback: Option<u8>,
    /// ISP `INT_RAW` (bit 28 header IDI frame, 27 tail IDI frame, 5 MIPI hnum mismatch,
    /// 3 h/v num setting error, 1 async FIFO overflow, 0 data type error).
    pub isp_int_raw: u32,
    /// ISP `CNTL` (bit 0 `MIPI_DATA_EN`, bit 1 `ISP_EN` must be 0 for bypass).
    pub isp_cntl: u32,
    /// CSI bridge `INT_RAW`.
    pub bridge_int_raw: u32,
    /// CSI bridge `CSI_BUF_DEPTH`: 64-bit words waiting in the bridge buffer.
    pub bridge_buf_depth: u16,
    /// CSI bridge enabled (`CSI_BRIG_EN`).
    pub bridge_enabled: bool,
    /// DW-GDMA channel enabled (`DMAC_CHEN0.CHn_EN`).
    pub dma_enabled: bool,
    /// DW-GDMA channel raw `INTSTATUS0`.
    pub dma_int_status: u32,
    /// DW-GDMA `CMPLTD_BLK_TFR_SIZE` of the channel (64-bit items; after a frame, one less
    /// than the DMA wrote).
    pub dma_completed_items: u32,
    /// DW-GDMA `DATA_LEFT_IN_FIFO` of the channel.
    pub dma_fifo_left: u32,
    /// Current destination address (`DAR0`): moves while a frame is being written.
    pub dma_dar: u32,
}

// ── Shared state (task ↔ ISRs) ──────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Ready {
    slot: u8,
    seq: u32,
    bytes: u32,
}

struct Shared {
    /// Bumped by every `Csi::new` so frames of an old instance are not returned to a new one.
    generation: u32,
    bufs: [(usize, usize); MAX_BUFFERS],
    count: u8,
    /// Slots the driver may arm.
    free: u8,
    /// Slot the DMA is filling.
    dma: Option<u8>,
    /// Newest completed frame not yet handed out.
    ready: Option<Ready>,
    running: bool,
    channel: u8,
    frame_items: u32,
    stats: Stats,
}

impl Shared {
    const fn new() -> Self {
        Self {
            generation: 0,
            bufs: [(0, 0); MAX_BUFFERS],
            count: 0,
            free: 0,
            dma: None,
            ready: None,
            running: false,
            channel: 0,
            frame_items: 0,
            stats: Stats {
                frames_done: 0,
                frames_dropped: 0,
                frames_overwritten: 0,
                last_frame_bytes: 0,
                last_block_items: 0,
                dma_errors: 0,
                dma_error_bits: 0,
                host_errors: 0,
                host_error_main: 0,
                host_error_detail: [0; 9],
                bridge_errors: 0,
                bridge_error_bits: 0,
            },
        }
    }

    fn take_free(&mut self) -> Option<u8> {
        if self.free == 0 {
            return None;
        }
        let slot = self.free.trailing_zeros() as u8;
        self.free &= !(1 << slot);
        Some(slot)
    }
}

static SHARED: Mutex<RefCell<Shared>> = Mutex::new(RefCell::new(Shared::new()));
static WAKER: AtomicWaker = AtomicWaker::new();
static TAKEN: AtomicBool = AtomicBool::new(false);

// ── Register blocks ─────────────────────────────────────────────────────────────────────

fn host() -> &'static pac::mipi_csi_host::RegisterBlock {
    unsafe { &*pac::MIPI_CSI_HOST::ptr() }
}
fn bridge() -> &'static pac::mipi_csi_bridge::RegisterBlock {
    unsafe { &*pac::MIPI_CSI_BRIDGE::ptr() }
}
fn isp() -> &'static pac::isp::RegisterBlock {
    unsafe { &*pac::ISP::ptr() }
}
fn dma() -> &'static pac::dma::RegisterBlock {
    unsafe { &*pac::DMA::ptr() }
}
fn clkrst() -> &'static pac::hp_sys_clkrst::RegisterBlock {
    unsafe { &*pac::HP_SYS_CLKRST::ptr() }
}

// DW-GDMA channel interrupt bits (`dw_gdma_ll.h` / hw_ver3 `dw_gdma_struct.h`).
const DMA_TFR_DONE: u32 = 1 << 1;
/// Decode/slave/LLI/shadow errors (bits 5..=14), slave-interface errors (16..=21, 25).
const DMA_ERR_MASK: u32 = 0x0000_7FE0 | 0x003F_0000 | 0x0200_0000;

// DW-GDMA CTL0 (hw_ver3 `dw_gdma_struct.h`, same layout esp-hal's `vdma.rs` uses).
const CTL0_SMS_CSI: u32 = 0; // bit 0: source on master 0 (reaches the CSI bridge)
const CTL0_DMS_MEM: u32 = 1 << 2; // bit 2: destination on master 1 (L2MEM, PSRAM)
const CTL0_SINC_FIXED: u32 = 1 << 4;
const CTL0_DINC_INCREMENT: u32 = 0 << 6;
const CTL0_SRC_TR_WIDTH_64: u32 = 3 << 8;
const CTL0_DST_TR_WIDTH_64: u32 = 3 << 11;
const CTL0_SRC_MSIZE_512: u32 = 8 << 14;
const CTL0_DST_MSIZE_512: u32 = 8 << 18;
const CTL0: u32 = CTL0_SMS_CSI
    | CTL0_DMS_MEM
    | CTL0_SINC_FIXED
    | CTL0_DINC_INCREMENT
    | CTL0_SRC_TR_WIDTH_64
    | CTL0_DST_TR_WIDTH_64
    | CTL0_SRC_MSIZE_512
    | CTL0_DST_MSIZE_512;
/// CTL1: ARLEN_EN, ARLEN = 16, AWLEN_EN, AWLEN = 16 (IDF `axi_burst_len = 16`); no status
/// fetch, no block markers.
const CTL1: u32 = (1 << 6) | (16 << 7) | (1 << 15) | (16 << 16);

fn channel_enabled(ch: u8) -> bool {
    dma().chen0().read().bits() & (1 << ch) != 0
}

fn channel_enable(ch: u8, en: bool) {
    // CHn_EN with its write-enable bit (`dw_gdma_ll_channel_enable`).
    let v = if en { 0x101 << ch } else { 0x100 << ch };
    dma().chen0().write(|w| unsafe { w.bits(v) });
}

/// Programs one single-block transfer bridge FIFO → `dst` and enables the channel
/// (`dw_gdma_channel_config_transfer` + `dw_gdma_channel_enable_ctrl`, IDF CSI ISR).
fn arm(ch: u8, dst: usize, items: u32) {
    let c = dma().ch(ch as usize);
    c.sar0().write(|w| unsafe { w.bits(CSI_BRG_MEM_BASE) });
    c.sar1().write(|w| unsafe { w.bits(0) });
    c.dar0().write(|w| unsafe { w.bits(dst as u32) });
    c.dar1().write(|w| unsafe { w.bits(0) });
    c.block_ts0().write(|w| unsafe { w.bits(items - 1) });
    c.ctl0().write(|w| unsafe { w.bits(CTL0) });
    c.ctl1().write(|w| unsafe { w.bits(CTL1) });
    channel_enable(ch, true);
}

// ── Interrupt handlers ──────────────────────────────────────────────────────────────────

/// DW-GDMA: a frame (one DMA transfer) is complete. Swaps buffers and wakes `wait_frame`.
#[esp_hal::ram]
#[esp_hal::handler(priority = Priority::Priority2)]
fn dma_isr() {
    critical_section::with(|cs| {
        let mut s = SHARED.borrow_ref_mut(cs);
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
        let items = s.frame_items;
        if st & DMA_TFR_DONE != 0 {
            let Some(done) = s.dma else { return };
            let bytes = c
                .dar0()
                .read()
                .bits()
                .wrapping_sub(s.bufs[done as usize].0 as u32);
            s.stats.frames_done = s.stats.frames_done.wrapping_add(1);
            s.stats.last_frame_bytes = bytes;
            s.stats.last_block_items = c.status0().read().bits() & 0x3F_FFFF;
            // Next buffer: a free one, else the older unclaimed frame (keep the newest).
            let next = match s.take_free() {
                Some(slot) => Some(slot),
                None => s.ready.take().map(|old| {
                    s.stats.frames_overwritten = s.stats.frames_overwritten.wrapping_add(1);
                    old.slot
                }),
            };
            if let Some(next) = next {
                arm(ch, s.bufs[next as usize].0, items);
                s.dma = Some(next);
                let seq = s.stats.frames_done;
                if let Some(old) = s.ready.replace(Ready {
                    slot: done,
                    seq,
                    bytes,
                }) {
                    s.free |= 1 << old.slot;
                    s.stats.frames_overwritten = s.stats.frames_overwritten.wrapping_add(1);
                }
                WAKER.wake();
            } else {
                // Every other buffer is held by the application: overwrite this frame.
                arm(ch, s.bufs[done as usize].0, items);
                s.stats.frames_dropped = s.stats.frames_dropped.wrapping_add(1);
            }
        } else if st & DMA_ERR_MASK != 0 && !channel_enabled(ch) {
            // The channel stopped on an error: restart it on the same buffer.
            if let Some(slot) = s.dma {
                arm(ch, s.bufs[slot as usize].0, items);
                s.stats.frames_dropped = s.stats.frames_dropped.wrapping_add(1);
            }
        }
    });
}

/// CSI host error interrupt (`s_csi_default_isr`): reading the per-category status registers
/// clears them (RC).
#[esp_hal::handler(priority = Priority::Priority1)]
fn host_isr() {
    let (main, detail) = read_host_errors();
    critical_section::with(|cs| {
        let mut s = SHARED.borrow_ref_mut(cs);
        s.stats.host_errors = s.stats.host_errors.wrapping_add(1);
        s.stats.host_error_main |= main;
        for (acc, d) in s.stats.host_error_detail.iter_mut().zip(detail) {
            *acc |= d;
        }
    });
}

/// Reads (and so clears) `INT_ST_MAIN` and the per-category status registers it flags, indexed
/// as [`Stats::host_error_detail`].
fn read_host_errors() -> (u32, [u32; 9]) {
    let h = host();
    let main = h.int_st_main().read().bits();
    let mut detail = [0u32; 9];
    if main & (1 << 0) != 0 {
        detail[0] = h.int_st_phy_fatal().read().bits();
    }
    if main & (1 << 1) != 0 {
        detail[1] = h.int_st_pkt_fatal().read().bits();
    }
    if main & (1 << 2) != 0 {
        detail[2] = h.int_st_bndry_frame_fatal().read().bits();
    }
    if main & (1 << 3) != 0 {
        detail[3] = h.int_st_seq_frame_fatal().read().bits();
    }
    if main & (1 << 4) != 0 {
        detail[4] = h.int_st_crc_frame_fatal().read().bits();
    }
    if main & (1 << 5) != 0 {
        detail[5] = h.int_st_pld_crc_fatal().read().bits();
    }
    if main & (1 << 6) != 0 {
        detail[6] = h.int_st_data_id().read().bits();
    }
    if main & (1 << 7) != 0 {
        detail[7] = h.int_st_ecc_corrected().read().bits();
    }
    if main & (1 << 16) != 0 {
        detail[8] = h.int_st_phy().read().bits();
    }
    (main, detail)
}

/// Bridge interrupts enabled by the driver: discard (2), buffer overrun (3), async FIFO
/// overflow (4).
const BRIDGE_ERR_MASK: u32 = 0b1_1100;

/// CSI bridge error interrupt.
#[esp_hal::handler(priority = Priority::Priority1)]
fn bridge_isr() {
    let b = bridge();
    let st = b.int_st().read().bits();
    b.int_clr().write(|w| unsafe { w.bits(st) });
    critical_section::with(|cs| {
        let mut s = SHARED.borrow_ref_mut(cs);
        s.stats.bridge_errors = s.stats.bridge_errors.wrapping_add(1);
        s.stats.bridge_error_bits |= st;
    });
}

// ── Frame ───────────────────────────────────────────────────────────────────────────────

/// A completed frame. Derefs to the [`Config::frame_bytes`] pixel bytes (RAW10 packed, see
/// [`DATA_TYPE_RAW10`]), cache-coherent.
///
/// Dropping it (or passing it to the next [`Csi::wait_frame`]) gives the buffer back to the
/// driver. Holding more frames than `buffers - 2` makes the DMA drop frames
/// ([`Stats::frames_dropped`]).
pub struct Frame {
    data: &'static [u8],
    slot: u8,
    generation: u32,
    seq: u32,
    received: u32,
}

impl Frame {
    /// Frame number: [`Stats::frames_done`] when this frame completed. Gaps mean frames were
    /// dropped or overwritten.
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// Bytes the DMA actually wrote for this frame. Equal to `len()` when the sensor geometry
    /// matches the configuration.
    pub fn received_len(&self) -> usize {
        self.received as usize
    }
}

impl Deref for Frame {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.data
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        critical_section::with(|cs| {
            let mut s = SHARED.borrow_ref_mut(cs);
            if s.generation == self.generation {
                s.free |= 1 << self.slot;
            }
        });
    }
}

// ── Driver ──────────────────────────────────────────────────────────────────────────────

/// The MIPI CSI-2 receiver.
pub struct Csi<'d> {
    config: Config,
    hsfreq: u8,
    hsfreq_readback: Option<u8>,
    channel: u8,
    generation: u32,
    running: bool,
    chip_rev: u16,
    _vdma: PhantomData<&'d mut ()>,
}

impl<'d> Csi<'d> {
    /// Buffer length [`Csi::new`] requires for `config`: the frame rounded up to whole bridge
    /// bursts plus one burst. The bridge is the DMA flow controller and ends a block after at
    /// most `bursts + 1` bursts (`CSI_BRIG_DMABLK_SIZE`), so a sensor sending a larger frame
    /// than configured cannot write past this length.
    pub const fn buffer_len(config: &Config) -> usize {
        (config.bursts() + 1) * BURST_BYTES
    }

    /// Sets up clocks, resets and the DMA channel, and takes ownership of `buffers`
    /// (2..=[`MAX_BUFFERS`]). Does not start capture.
    ///
    /// Each buffer must be at least [`Csi::buffer_len`] long for every configuration it will be
    /// used with, and start and length must be multiples of the cache line (64 B in internal
    /// RAM; in PSRAM the L2 line the bootloader set, 64 or 128 B: align to 128 to be safe).
    /// Buffers may be in PSRAM or internal RAM.
    ///
    /// Takes the CSI host, bridge and ISP PAC blocks unsafely (esp-hal has no singleton for
    /// them); returns [`Error::AlreadyInUse`] if a `Csi` already exists.
    pub fn new<const N: usize>(
        vdma_channel: impl VdmaDmaChannel + 'd,
        buffers: [&'static mut [u8]; N],
        config: Config,
    ) -> Result<Self, Error> {
        let hsfreq = config.validate()?;
        if !(2..=MAX_BUFFERS).contains(&N) {
            return Err(Error::BufferCount);
        }
        let mut table = [(0usize, 0usize); MAX_BUFFERS];
        for (i, b) in buffers.iter().enumerate() {
            let (addr, len) = (b.as_ptr() as usize, b.len());
            let align = cache::line_size(addr);
            if addr % align != 0 || len % align != 0 {
                return Err(Error::BufferAlignment {
                    index: i as u8,
                    align,
                });
            }
            table[i] = (addr, len);
        }
        check_buffers(&table[..N], &config)?;
        if TAKEN.swap(true, Ordering::Acquire) {
            return Err(Error::AlreadyInUse);
        }
        let channel = vdma_channel.channel_id();
        // The buffers now belong to the driver (shared with frames as `&'static [u8]`).
        let _ = buffers;

        let generation = critical_section::with(|cs| {
            let mut s = SHARED.borrow_ref_mut(cs);
            s.generation = s.generation.wrapping_add(1);
            s.bufs = table;
            s.count = N as u8;
            s.free = ((1u16 << N) - 1) as u8;
            s.dma = None;
            s.ready = None;
            s.running = false;
            s.channel = channel;
            s.frame_items = (config.frame_bytes() / 8) as u32;
            s.stats = Stats::default();
            s.generation
        });

        let chip_rev = chip_revision();
        enable_clocks();
        init_dma_channel(channel, 0, DMA_TFR_DONE | DMA_ERR_MASK);

        interrupt::bind_handler(Interrupt::DMA, dma_isr);
        interrupt::bind_handler(Interrupt::CSI, host_isr);
        interrupt::bind_handler(Interrupt::CSI_BRIDGE, bridge_isr);

        Ok(Self {
            config,
            hsfreq,
            hsfreq_readback: None,
            channel,
            generation,
            running: false,
            chip_rev,
            _vdma: PhantomData,
        })
    }

    /// Current configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Changes geometry and/or lane rate. Only while stopped; every buffer must be long enough
    /// for the new configuration.
    pub fn set_config(&mut self, config: Config) -> Result<(), Error> {
        if self.running {
            return Err(Error::Running);
        }
        let hsfreq = config.validate()?;
        critical_section::with(|cs| {
            let mut s = SHARED.borrow_ref_mut(cs);
            let n = s.count as usize;
            check_buffers(&s.bufs[..n], &config)?;
            s.frame_items = (config.frame_bytes() / 8) as u32;
            Ok(())
        })?;
        self.config = config;
        self.hsfreq = hsfreq;
        Ok(())
    }

    /// Brings up the D-PHY, CSI-2 host, ISP bypass and bridge for the current configuration and
    /// arms the DMA. Call it before the sensor's stream-on: with a continuous clock lane the
    /// sensor enters HS at stream start, with a gated one it returns to LP-11 between packets;
    /// in both cases the receiver must be out of reset when the first frame starts.
    ///
    /// Needs at least one buffer not held by a [`Frame`].
    pub fn start(&mut self) -> Result<(), Error> {
        if self.running {
            return Err(Error::Running);
        }
        let cfg = self.config;
        let ch = self.channel;
        self.hsfreq_readback = Some(configure_link(
            &cfg,
            self.hsfreq,
            self.chip_rev,
            // TRM 38.5.7: for one block per frame of n bursts, DMABLK_SIZE > n.
            (cfg.bursts() + 1) as u16,
        ));

        // ── DMA: arm the first buffer, then enable the bridge (IDF `s_ctlr_csi_start`) ──
        let armed = critical_section::with(|cs| {
            let mut s = SHARED.borrow_ref_mut(cs);
            // A frame finished before the last stop is stale now.
            if let Some(r) = s.ready.take() {
                s.free |= 1 << r.slot;
            }
            if let Some(d) = s.dma.take() {
                s.free |= 1 << d;
            }
            let slot = s.take_free()?;
            let c = dma().ch(ch as usize);
            c.intclear0().write(|w| unsafe { w.bits(u32::MAX) });
            arm(ch, s.bufs[slot as usize].0, s.frame_items);
            s.dma = Some(slot);
            s.running = true;
            Some(())
        });
        if armed.is_none() {
            return Err(Error::BufferCount);
        }
        bridge().csi_en().modify(|_, w| w.csi_brig_en().set_bit());
        self.running = true;
        Ok(())
    }

    /// Stops capture (TRM 42.7.2): bridge off, DMA channel disabled (aborted if it does not
    /// stop), CSI host and PHY held in reset. Stop the sensor stream first. A pending
    /// [`Csi::wait_frame`] returns [`Error::NotRunning`]; frames already handed out stay valid.
    pub fn stop(&mut self) {
        let ch = self.channel;
        bridge().csi_en().modify(|_, w| w.csi_brig_en().clear_bit());
        critical_section::with(|cs| SHARED.borrow_ref_mut(cs).running = false);
        halt_channel(ch);
        reset_link();
        self.running = false;
        WAKER.wake();
    }

    /// `true` between [`Csi::start`] and [`Csi::stop`].
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Gives `previous` back to the driver and waits for the next completed frame (the newest
    /// one if several completed meanwhile). Invalidates the frame's cache lines before
    /// returning it. Never blocks the executor; returns [`Error::NotRunning`] if the receiver
    /// is (or gets) stopped.
    pub async fn wait_frame(&mut self, previous: Option<Frame>) -> Result<Frame, Error> {
        drop(previous);
        let (ready, addr) = poll_fn(|cx| {
            WAKER.register(cx.waker());
            critical_section::with(|cs| {
                let mut s = SHARED.borrow_ref_mut(cs);
                if let Some(r) = s.ready.take() {
                    return Poll::Ready(Ok((r, s.bufs[r.slot as usize].0)));
                }
                if !s.running {
                    return Poll::Ready(Err(Error::NotRunning));
                }
                Poll::Pending
            })
        })
        .await?;
        let len = self.config.frame_bytes();
        let line = cache::line_size(addr);
        cache::invalidate(addr, len.div_ceil(line) * line);
        Ok(Frame {
            data: unsafe { core::slice::from_raw_parts(addr as *const u8, len) },
            slot: ready.slot,
            generation: self.generation,
            seq: ready.seq,
            received: ready.bytes,
        })
    }

    /// Event counters.
    pub fn stats(&self) -> Stats {
        critical_section::with(|cs| SHARED.borrow_ref(cs).stats)
    }

    /// Zeroes the counters.
    pub fn reset_stats(&mut self) {
        critical_section::with(|cs| SHARED.borrow_ref_mut(cs).stats = Stats::default());
    }

    /// D-PHY lane state.
    pub fn phy_status(&self) -> PhyStatus {
        dphy::status(host())
    }

    /// Register snapshot of the whole path, for debugging. Clears nothing.
    pub fn debug_status(&self) -> DebugStatus {
        snapshot(self.channel, self.hsfreq_readback)
    }

    /// Chip revision ×100 from eFuse (e.g. 302 for v3.2).
    pub fn chip_revision(&self) -> u16 {
        self.chip_rev
    }
}

impl Drop for Csi<'_> {
    fn drop(&mut self) {
        if self.running {
            self.stop();
        }
        let cpu = Cpu::current();
        interrupt::disable(cpu, Interrupt::DMA);
        interrupt::disable(cpu, Interrupt::CSI);
        interrupt::disable(cpu, Interrupt::CSI_BRIDGE);
        TAKEN.store(false, Ordering::Release);
    }
}

fn check_buffers(bufs: &[(usize, usize)], config: &Config) -> Result<(), Error> {
    let needed = Csi::buffer_len(config);
    for (i, &(_, len)) in bufs.iter().enumerate() {
        if len < needed {
            return Err(Error::BufferTooSmall {
                index: i as u8,
                needed,
            });
        }
    }
    Ok(())
}

/// Reads every RC host status register once.
fn clear_host_status() {
    let h = host();
    let _ = h.int_st_phy_fatal().read();
    let _ = h.int_st_pkt_fatal().read();
    let _ = h.int_st_bndry_frame_fatal().read();
    let _ = h.int_st_seq_frame_fatal().read();
    let _ = h.int_st_crc_frame_fatal().read();
    let _ = h.int_st_pld_crc_fatal().read();
    let _ = h.int_st_data_id().read();
    let _ = h.int_st_ecc_corrected().read();
    let _ = h.int_st_phy().read();
}

/// eFuse wafer version (`efuse_ll_get_chip_wafer_version_major/minor`), ×100.
fn chip_revision() -> u16 {
    let sys2 = unsafe { &*pac::EFUSE::ptr() }.rd_mac_sys2().read().bits();
    let major = (((sys2 >> 23) & 1) << 2) | ((sys2 >> 4) & 0x3);
    let minor = sys2 & 0xF;
    (major * 100 + minor) as u16
}

/// Clock gates and resets (TRM 42.7.1.1 and 38.7.1; IDF `s_csi_claim_controller`,
/// `mipi_csi_brg_claim`, PHY clock in `esp_cam_new_csi_ctlr`, ISP clock in
/// `esp_isp_new_processor`, DW-GDMA in `dw_gdma_acquire_group_handle`).
fn enable_clocks() {
    critical_section::with(|_| {
        let c = clkrst();
        // CSI host bus clock, reset pulse.
        c.soc_clk_ctrl1()
            .modify(|_, w| w.csi_host_sys_clk_en().set_bit());
        c.hp_rst_en0().modify(|_, w| w.rst_en_csi_host().set_bit());
        c.hp_rst_en0()
            .modify(|_, w| w.rst_en_csi_host().clear_bit());
        // CSI bridge (and ISP) bus clock; the reset pulse is done by `start`.
        c.soc_clk_ctrl1()
            .modify(|_, w| w.csi_brg_sys_clk_en().set_bit());
        // D-PHY configuration clock: PLL_F20M (`MIPI_CSI_PHY_CLK_SRC_DEFAULT`), gate 0 -> 1.
        c.ref_clk_ctrl2()
            .modify(|_, w| w.ref_20m_clk_en().set_bit());
        c.peri_clk_ctrl03().modify(|_, w| unsafe {
            w.mipi_csi_dphy_clk_src_sel().bits(0);
            w.mipi_csi_dphy_cfg_clk_en().clear_bit()
        });
        c.peri_clk_ctrl03()
            .modify(|_, w| w.mipi_csi_dphy_cfg_clk_en().set_bit());
        // ISP function clock: PLL_F240M (`ISP_CLK_SRC_DEFAULT`), divider 1. The ISP_Header and
        // ISP_Tail stay in the path in bypass and need it.
        c.ref_clk_ctrl1()
            .modify(|_, w| w.ref_240m_clk_en().set_bit());
        c.peri_clk_ctrl25().modify(|_, w| unsafe {
            w.isp_clk_src_sel().bits(1);
            w.isp_clk_en().set_bit()
        });
        c.peri_clk_ctrl26()
            .modify(|_, w| unsafe { w.isp_clk_div_num().bits(0) });
        // DW-GDMA bus clocks.
        c.soc_clk_ctrl0()
            .modify(|_, w| w.gdma_cpu_clk_en().set_bit());
        c.soc_clk_ctrl1()
            .modify(|_, w| w.gdma_sys_clk_en().set_bit());
    });
}

/// One-time DW-GDMA controller and channel setup (`dw_gdma_hal_init`, `dw_gdma_new_channel`
/// with the IDF CSI allocation config). The controller is reset only if nobody enabled it yet.
fn init_dma_channel(ch: u8, multblk_type: u8, signals: u32) {
    let d = dma();
    if d.cfg0().read().dmac_en().bit_is_clear() {
        critical_section::with(|_| {
            let c = clkrst();
            c.hp_rst_en0().modify(|_, w| w.rst_en_gdma().set_bit());
            c.hp_rst_en0().modify(|_, w| w.rst_en_gdma().clear_bit());
        });
        d.reset0().write(|w| w.dmac_rst().set_bit());
        while d.reset0().read().dmac_rst().bit_is_set() {}
        d.cfg0()
            .modify(|_, w| w.dmac_en().set_bit().int_en().set_bit());
    }
    channel_enable(ch, false);
    let c = d.ch(ch as usize);
    // Multi-block type for source and destination: 0 = contiguous (one block per transfer),
    // 3 = linked list.
    c.cfg0().write(|w| unsafe {
        w.ch1_src_multblk_type().bits(multblk_type);
        w.ch1_dst_multblk_type().bits(multblk_type)
    });
    // TT_FC = 4: peripheral to memory, source peripheral is flow controller (TRM 38.7.3).
    // Hardware handshake both sides (IDF), SRC_PER = 1 (CSI), priority 1, 5 outstanding.
    c.cfg1().write(|w| unsafe {
        w.ch1_tt_fc().bits(4);
        w.ch1_hs_sel_src().clear_bit();
        w.ch1_hs_sel_dst().clear_bit();
        w.ch1_src_per().bits(1);
        w.ch1_ch_prior().bits(1);
        w.ch1_src_osr_lmt().bits(4);
        w.ch1_dst_osr_lmt().bits(4)
    });
    c.sstatar0().write(|w| unsafe { w.bits(CSI_BRG_MEM_BASE) });
    c.dstatar0().write(|w| unsafe { w.bits(0) });
    // Status for every event, interrupt signal for `signals`.
    c.intclear0().write(|w| unsafe { w.bits(u32::MAX) });
    c.intstatus_enable0().write(|w| unsafe { w.bits(u32::MAX) });
    c.intsignal_enable0().write(|w| unsafe { w.bits(signals) });
}

/// Brings up the D-PHY, CSI-2 host, ISP bypass and bridge for `cfg` (everything of
/// [`Csi::start`] except arming the DMA and enabling the bridge), with the bridge ending a DMA
/// block after `dmablk_bursts` bursts or at frame end. Returns the `hsfreqrange` read back
/// from the D-PHY.
fn configure_link(cfg: &Config, hsfreq: u8, chip_rev: u16, dmablk_bursts: u16) -> u8 {
    let h = host();
    let b = bridge();

    // Bridge + ISP back to reset values (also clears stale FIFO content after a stop).
    // `mipi_csi_brg_claim`: module clock on, reset pulse, CSI_ENABLECLK.
    critical_section::with(|_| {
        let c = clkrst();
        c.hp_rst_en0().modify(|_, w| w.rst_en_csi_brg().set_bit());
        c.hp_rst_en0().modify(|_, w| w.rst_en_csi_brg().clear_bit());
        c.hp_rst_en0().modify(|_, w| w.rst_en_isp().set_bit());
        c.hp_rst_en0().modify(|_, w| w.rst_en_isp().clear_bit());
    });
    b.host_ctrl().modify(|_, w| w.csi_enableclk().set_bit());

    // ── D-PHY + host (TRM 42.7.1.2; IDF `mipi_csi_hal_init`) ──
    // Hold host and PHY in reset/shutdown.
    h.csi2_resetn().write(|w| unsafe { w.bits(0) });
    h.phy_shutdownz().write(|w| unsafe { w.bits(0) });
    h.dphy_rstz().write(|w| unsafe { w.bits(0) });
    // Test-code logic reset, then hsfreqrange into test code 0x44 (data bits [6:1]).
    dphy::test_clear(h);
    dphy::write_test_code(h, dphy::TEST_CODE_HSFREQRANGE, hsfreq << 1);
    let rb = dphy::read_test_code(h, dphy::TEST_CODE_HSFREQRANGE);
    // Lanes, no virtual-channel extension (VCX = 1 disables it), no descrambler.
    h.n_lanes().write(|w| unsafe { w.bits((LANES - 1) as u32) });
    h.vc_extension().write(|w| unsafe { w.bits(1) });
    h.scrambling().write(|w| unsafe { w.bits(0) });
    // Release the PHY.
    h.phy_shutdownz().write(|w| unsafe { w.bits(1) });
    h.dphy_rstz().write(|w| unsafe { w.bits(1) });
    // Error interrupts: clear stale status (RC), unmask everything
    // (`mipi_csi_host_ll_enable_intr(ERR_ALL)` masks values).
    clear_host_status();
    h.int_msk_phy_fatal().write(|w| unsafe { w.bits(0x3) });
    h.int_msk_pkt_fatal().write(|w| unsafe { w.bits(0x3) });
    h.int_msk_bndry_frame_fatal()
        .write(|w| unsafe { w.bits(0xFFFF) });
    h.int_msk_seq_frame_fatal()
        .write(|w| unsafe { w.bits(0xFFFF) });
    h.int_msk_crc_frame_fatal()
        .write(|w| unsafe { w.bits(0xFFFF) });
    h.int_msk_pld_crc_fatal()
        .write(|w| unsafe { w.bits(0xFFFF) });
    h.int_msk_data_id().write(|w| unsafe { w.bits(0xFFFF) });
    h.int_msk_ecc_corrected()
        .write(|w| unsafe { w.bits(0xFFFF) });
    h.int_msk_phy().write(|w| unsafe { w.bits(0x3_0003) });
    // Release the host controller.
    h.csi2_resetn().write(|w| unsafe { w.bits(1) });

    configure_isp(cfg);
    configure_bridge(cfg, chip_rev, dmablk_bursts);
    (rb >> 1) & 0x3F
}

/// The ISP part of [`configure_link`]: ISP in bypass (IDF `esp_isp_new_processor` with
/// `bypass_isp`), input from the CSI host.
fn configure_isp(cfg: &Config) {
    let i = isp();
    // `isp_ll_init`: CNTL = 0 (ISP_EN = 0 is the bypass), clear and mask interrupts.
    i.cntl().write(|w| unsafe { w.bits(0) });
    i.int_clr().write(|w| unsafe { w.bits(u32::MAX) });
    i.int_ena().write(|w| unsafe { w.bits(0) });
    i.clk_en().modify(|_, w| w.clk_en().set_bit());
    // Input from the CSI host.
    i.cntl()
        .modify(|_, w| unsafe { w.isp_in_src().bits(0).mipi_data_en().set_bit() });
    // Bypass: HADR counts 32-bit IDI words per line (`ceil(width * bpp / 32)`, IDF
    // `esp_isp_new_processor`); registers hold value - 1.
    let words = cfg.line_words();
    i.frame_cfg().modify(|_, w| unsafe {
        w.hadr_num().bits((words - 1) as u16);
        w.vadr_num().bits(cfg.height - 1);
        w.hsync_start_exist().bit(cfg.line_sync);
        w.hsync_end_exist().bit(cfg.line_sync)
    });
    // ISP_SHADOW_MODE_UPDATE_ONLY_NEXT_VSYNC.
    i.shadow_reg_ctrl()
        .modify(|_, w| unsafe { w.shadow_update_sel().bits(2) });
}

/// The CSI bridge part of [`configure_link`] (TRM 38.7.2; IDF `mipi_csi_hal_init` +
/// `esp_cam_new_csi_ctlr`): geometry, buffer threshold, byte order, DMA burst and block size,
/// data type filter, colour mode, error interrupts. Leaves the bridge disabled.
fn configure_bridge(cfg: &Config, chip_rev: u16, dmablk_bursts: u16) {
    let b = bridge();
    b.frame_cfg().modify(|_, w| unsafe {
        // IDF writes pixels (`h_res`) here; TRM 38.7.2 says 64-bit units. With
        // VADR_NUM_CHECK off (reset value) the bridge does not use these for flow control.
        w.hadr_num().bits(cfg.width);
        w.vadr_num().bits(cfg.height)
    });
    b.buf_flow_ctl()
        .modify(|_, w| unsafe { w.csi_buf_afull_thrd().bits(BRIDGE_AFULL_THRD as u16) });
    b.endian_mode()
        .modify(|_, w| w.byte_endian_order().clear_bit());
    b.dma_req_cfg()
        .modify(|_, w| unsafe { w.dma_burst_len().bits(BURST_ITEMS as u16) });
    b.dmablk_size()
        .write(|w| unsafe { w.dmablk_size().bits(dmablk_bursts) });
    b.data_type_cfg().modify(|_, w| unsafe {
        w.data_type_min().bits(cfg.data_type);
        w.data_type_max().bits(cfg.data_type)
    });
    if chip_rev >= 300 {
        // Color-mode block present from v3.0: enabled but bypassed (same in/out format).
        b.host_cm_ctrl().modify(|_, w| {
            w.csi_host_cm_en().set_bit();
            w.csi_host_cm_bypass().set_bit()
        });
    }
    b.int_clr().write(|w| unsafe { w.bits(u32::MAX) });
    b.int_ena().write(|w| unsafe { w.bits(BRIDGE_ERR_MASK) });
}

/// Resets the ISP and the CSI bridge (the reset pulses of [`configure_link`]), programs both
/// again for `cfg` and enables the bridge, with the CSI host and the D-PHY left running: the
/// stall recovery of [`strips`] (see "Stalls" there). Measured: the bridge then cuts blocks
/// as after `start`, beginning with the next frame. Call it only while the DMA channel waits
/// between blocks (halted on an item, or stopped): the bridge is the DMA's flow controller.
/// Call it inside a critical section: `HP_RST_EN0` holds other modules' resets too.
fn restart_isp_bridge(cfg: &Config, chip_rev: u16, dmablk_bursts: u16) {
    let c = clkrst();
    c.hp_rst_en0().modify(|_, w| w.rst_en_csi_brg().set_bit());
    c.hp_rst_en0().modify(|_, w| w.rst_en_csi_brg().clear_bit());
    c.hp_rst_en0().modify(|_, w| w.rst_en_isp().set_bit());
    c.hp_rst_en0().modify(|_, w| w.rst_en_isp().clear_bit());
    bridge()
        .host_ctrl()
        .modify(|_, w| w.csi_enableclk().set_bit());
    configure_isp(cfg);
    configure_bridge(cfg, chip_rev, dmablk_bursts);
    bridge().csi_en().modify(|_, w| w.csi_brig_en().set_bit());
}

/// Disables DMA channel `ch` (TRM 7.5.7.2), aborting it if it does not stop, and clears its
/// interrupt status.
fn halt_channel(ch: u8) {
    channel_enable(ch, false);
    let mut spins = 0u32;
    while channel_enabled(ch) && spins < 100_000 {
        spins += 1;
    }
    if channel_enabled(ch) {
        // `dw_gdma_ll_channel_abort`: the abort bit clears itself when done.
        dma().chen1().write(|w| unsafe { w.bits(0x101 << ch) });
        let mut spins = 0u32;
        while dma().chen1().read().bits() & (0x101 << ch) != 0 && spins < 100_000 {
            spins += 1;
        }
    }
    dma()
        .ch(ch as usize)
        .intclear0()
        .write(|w| unsafe { w.bits(u32::MAX) });
}

/// ISP input off, CSI host and PHY held in reset (TRM 42.7.2).
fn reset_link() {
    isp().cntl().modify(|_, w| w.mipi_data_en().clear_bit());
    let h = host();
    h.csi2_resetn().write(|w| unsafe { w.bits(0) });
    h.phy_shutdownz().write(|w| unsafe { w.bits(0) });
    h.dphy_rstz().write(|w| unsafe { w.bits(0) });
    critical_section::with(|_| {
        let c = clkrst();
        c.hp_rst_en0().modify(|_, w| w.rst_en_csi_host().set_bit());
        c.hp_rst_en0()
            .modify(|_, w| w.rst_en_csi_host().clear_bit());
    });
}

/// Register snapshot of the whole path for DMA channel `ch`. Clears nothing.
fn snapshot(ch: u8, hsfreqrange_readback: Option<u8>) -> DebugStatus {
    let b = bridge();
    let c = dma().ch(ch as usize);
    DebugStatus {
        phy: dphy::status(host()),
        hsfreqrange_readback,
        isp_int_raw: isp().int_raw().read().bits(),
        isp_cntl: isp().cntl().read().bits(),
        bridge_int_raw: b.int_raw().read().bits(),
        bridge_buf_depth: ((b.buf_flow_ctl().read().bits() >> 16) & 0x3FFF) as u16,
        bridge_enabled: b.csi_en().read().bits() & 1 != 0,
        dma_enabled: channel_enabled(ch),
        dma_int_status: c.intstatus0().read().bits(),
        dma_completed_items: c.status0().read().bits() & 0x3F_FFFF,
        dma_fifo_left: c.status1().read().bits() & 0x7FFF,
        dma_dar: c.dar0().read().bits(),
    }
}
