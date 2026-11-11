//! Hardware JPEG encoder driver for the ESP32-P4, greyscale.
//!
//! esp-hal 1.2.2 has no driver for the P4's JPEG codec or for the 2D-DMA that feeds it, and the
//! `esp32p4` 0.3 PAC has the `JPEG` block (0x5008_6000) but no 2D-DMA block. This crate fills the
//! gap for exactly one use: **8-bit greyscale pictures (one byte per pixel, row by row, as the
//! CSI receiver writes a RAW8 frame) encoded to baseline JPEG files**, one at a time, from a
//! caller's buffer into a caller's buffer. espion uses it to stream the OV9281's frames. See
//! `docs/architecture.md`.
//!
//! Data path: picture in memory (PSRAM or internal RAM) → 2D-DMA TX channel 0 in 2D-MOD1 mode
//! (blocks of 128 x 8 pixels, macroblock reordering into 8 x 8 blocks) → JPEG codec (DCT,
//! quantisation, Huffman coding with the Annex K tables, stuffing bytes, EOI) → 2D-DMA RX
//! channel 1 in 1D mode → output buffer after the header the CPU wrote. The sequence follows
//! the TRM v0.7 (§37.7.1 "JPEG Encoder", chapter 8 "2D-DMA") and ESP-IDF's
//! `esp_driver_jpeg/jpeg_encode.c`, `jpeg_emit_marker.c`, `jpeg_param.c`,
//! `esp_hal_jpeg/esp32p4/include/hal/jpeg_ll.h`, `jpeg_hal.c`,
//! `esp_driver_dma/src/dma2d.c` and `esp_hal_dma/esp32p4/include/hal/dma2d_ll.h` (Apache-2.0).
//!
//! # Peripheral ownership
//!
//! esp-hal has no singleton for the JPEG codec or the 2D-DMA: [`Encoder::new`] takes both
//! unsafely and guards against a second instance with an atomic flag. Nothing else in the
//! firmware may touch them (the PPA uses the 2D-DMA too). The driver binds the `JPEG`
//! interrupt, at [`INTERRUPT_PRIORITY`], on the core that calls [`Encoder::new`].
//!
//! # Example
//!
//! ```no_run
//! # async fn run(frame: &[u8], out: &mut [u8]) {
//! use esp32p4_jpeg::{Config, Encoder};
//!
//! // 1280x800 greyscale, quality 80 (IJG scale). Builds the header and programs the tables.
//! let mut enc = Encoder::new(Config::new(1280, 800, 80)).unwrap();
//! // `frame`: 1 024 000 bytes in memory (written by a DMA, or written back from the cache),
//! // start 16-byte aligned. `out`: start and length multiples of the cache line.
//! let len = embassy_time::with_timeout(
//!     embassy_time::Duration::from_millis(100),
//!     enc.encode(frame, out),
//! )
//! .await
//! .unwrap()
//! .unwrap();
//! let jpeg: &[u8] = &out[..len]; // FF D8 ... FF D9, cache-coherent
//! # let _ = jpeg;
//! # }
//! ```
//!
//! # Bring-up sequence
//!
//! [`Encoder::new`]:
//! 1. Clock gates and resets (TRM §37.7.1 step 1, §8.7): `HP_SYS_CLKRST_JPEG_SYS_CLK_EN` and
//!    `HP_SYS_CLKRST_DMA2D_SYS_CLK_EN` on, `HP_SYS_CLKRST_RST_EN_JPEG` and `_RST_EN_DMA2D`
//!    pulsed (ESP-IDF `jpeg_acquire_codec_handle`, `dma2d_acquire_pool`). The codec's function
//!    clock is SYS_CLK (TRM §37.2): 200 MHz with esp-hal's 400 MHz preset (CPU /1, MEM /2,
//!    SYS /1); no clock source or divider of its own.
//! 2. 2D-DMA common set-up (`dma2d_ll_hw_enable`): register clock on, AXI read and write FIFOs
//!    reset, arbiter weights off. The accessible address ranges (TRM §8.4.11) keep their reset
//!    values, 0x3010_0000..=0x8FFF_FFFF, which hold internal RAM and PSRAM.
//! 3. Channels (TRM §8.7.3, §8.7.4, §37.7.1 step 3; `dma2d_connect`,
//!    `jpeg_enc_config_dma_trans_ability`): each reset (§8.7.7: `CMD_DISABLE`, wait for
//!    `RESET_AVAIL`, `RST` pulse), connected to the JPEG codec (`PERI_SEL` 0), 128-byte bursts,
//!    page-bound wrap, descriptor bursts. TX channel 0: EOF mode 1, macroblock 8 x 8,
//!    reordering on (only channel 0 has it), no colour conversion (input 7, output 2: the reset
//!    value of the output selector is RGB888 → RGB565). RX channel 1: no macroblock, owner
//!    written back as 0. 2D-DMA interrupts stay disabled (see "Interrupt" below).
//! 4. Codec (TRM §37.7.1 step 2, §37.5.1.3): `SOFT_RST` pulse, encoder mode, colour space 3
//!    (GRAY), EOI added, `00` stuffed after `FF`, 8-bit quantisation tables in FIFO mode, all
//!    four tables written with the luminance table of the configured quality, luminance table
//!    0, picture size. Interrupts DONE, RLE error and frame-EOF error enabled.
//! 5. The 384-byte file header is built for the size and quality.
//!
//! [`Encoder::encode`], per frame (TRM §37.7.1 steps 3 to 7):
//! 1. Header copied to the output, written back; the output after it invalidated.
//! 2. Descriptors written (internal RAM, through the non-cacheable alias): TX 2D-MOD1, the whole
//!    picture in blocks of 128 x 8 (`enc_hb_tbl` for GRAY), 1 byte per pixel; RX 1D, the
//!    output after the header.
//! 3. Interrupt status cleared, link addresses set, TX then RX started (`LINK_START`), codec
//!    started (`JPEG_START`).
//! 4. Wait for `DMA2D_IN_SUC_EOF` on RX (all of the bitstream in memory; TRM §37.7.1 step 7),
//!    or an error interrupt. The length is the RX descriptor's written-back length.
//! 5. The written part of the output invalidated.
//!
//! Changing size or quality ([`Encoder::set_config`]) rebuilds the header and reprograms the
//! codec (step 4 of `new`). A frame with unchanged settings does no per-pixel and no per-table
//! work on the CPU: about 20 register writes, a 384-byte copy and three ROM cache calls.
//!
//! # Interrupts
//!
//! Three sources wake the waiting [`Encoder::encode`], all at [`INTERRUPT_PRIORITY`] on the
//! core that called [`Encoder::new`]: the codec's `JPEG` (DONE and its errors), the 2D-DMA RX
//! channel's `DMA2D_IN_CH1` (`SUC_EOF`: the end of a frame; `DSCR_EMPTY`: output too small;
//! descriptor errors) and the TX channel's `DMA2D_OUT_CH0` (descriptor errors). The 2D-DMA
//! sources (104..=108, 133, 134 in ESP-IDF's `soc/esp32p4/include/soc/interrupts.h`) are
//! missing from the PAC's `Interrupt` enum, so esp-hal's `bind_handler` cannot take them: the
//! driver binds `JPEG` with esp-hal, then maps sources 105 and 106 to the same CPU interrupt
//! (it copies `INTERRUPT_CORE0/1_*_INTR_MAP` of `JPEG`) and writes its handler into the PAC's
//! vector table entries 105 and 106, as `bind_handler` does for a known source. The RX channel
//! is 1 because the vector table ends at 127 (RX channel 2 is source 133). The codec stalls
//! and raises nothing on a too-small output (seen on the board), so without the RX interrupt
//! nothing would wake the task before the caller's timeout [INFERENCE].
//!
//! # Cache and buffers
//!
//! - **Input**: `width * height` bytes, start 16-byte aligned ([`INPUT_ALIGN`], ESP-IDF's
//!   `JPEG_DMA2D_BUFFER_ALIGN`), in PSRAM or internal RAM. The DMA reads memory, not the cache:
//!   the picture must be in memory. A frame written by a DMA (the CSI receiver) is; a picture
//!   the CPU wrote must be written back first ([`write_back`]). The driver does no cache
//!   operation on the input.
//! - **Output**: start and length multiples of the cache line of its memory (64 B in internal
//!   RAM; in PSRAM the L2 line, 64 or 128 B: align to 128 to be safe), at least
//!   [`HEADER_LEN`] + 1 bytes. Size it with [`Config::max_output_len`] for any content, or
//!   smaller for a known scene (too small gives [`Error::OutputTooSmall`]). On return the whole
//!   file is in memory and in no stale cache line: the CPU may read it and another DMA (the
//!   EMAC) may send it. The CPU must not write the output while an encode runs.
//! - Two RX/TX descriptors (64 bytes) live in a driver static in internal RAM.
//!
//! # Errors and recovery
//!
//! Every error is a value: [`Error::Geometry`], [`Error::Quality`] and the buffer checks are
//! returned before the hardware is touched. [`Error::OutputTooSmall`], [`Error::Codec`] and
//! [`Error::Dma`] come from the hardware; a timeout is the caller's (wrap [`Encoder::encode`]
//! in `embassy_time::with_timeout`). After any of them, and whenever an `encode` future is
//! dropped before it completes, the driver stops both DMA channels at once (so nothing more is
//! written into the caller's buffer), and the next `encode` runs the reset sequence first:
//! channel resets and the codec reset and programming of `new`, steps 3 and 4
//! ([`Stats::resets`]). The frame after an error encodes normally.
//!
//! # What was learned on hardware
//!
//! Waveshare ESP32-P4-ETH, chip v3.2, CPU 400 MHz, SYS_CLK 200 MHz, PSRAM 250 MHz; measured in
//! `poc-p4-eth` (`JPEG_PLAN`, README section "JPEG encoder"):
//! - About 171 Mpixel/s at 1280x800 and 1280x720 (5.99 ms and 5.39 ms a frame), 169 at
//!   640x480 and 640x400, 161 at 320x200 (0.40 ms: the per-frame overhead shows), whatever the
//!   quality (50, 75, 90) and whether the picture and file are in PSRAM or internal RAM. The
//!   same while the CSI receiver writes 1280x800 frames into PSRAM at up to 154 Hz.
//! - Frames back to back need no codec reset: the codec is programmed once.
//! - The file decodes with libjpeg-turbo at the PSNR of libjpeg-turbo's own encoder at the same
//!   quality, with 0.1 to 4.4 % fewer entropy-coded bytes; the decoded pixels differ from
//!   libjpeg-turbo's by a few levels [INFERENCE: different DCT and quantiser rounding].
//! - An output too small stalls the codec: no DONE, no error from the codec; only the RX
//!   channel's `DSCR_EMPTY` tells (hence the RX interrupt).
//! - The RX descriptor's written-back length is the file length, stuffing bytes and the EOI
//!   the hardware adds included.
//! - The PAC's `T0QNR`..`T3QNR` are read-only in the SVD but are written (raw pointer).
//!
//! # Debugging "no interrupt / no data"
//!
//! Read [`Encoder::debug_status`] after a timeout, in this order:
//! 1. `date` reads 0x0241_2190 and `jpeg_version` non-zero: the blocks are clocked and out of
//!    reset (`clk_ctrl1` has `JPEG_SYS_CLK_EN` (bit 27) and `DMA2D_SYS_CLK_EN` (bit 25) set).
//! 2. `tx_int_raw`/`rx_int_raw` bit `DSCR_ERR`: a descriptor failed its check (address outside
//!    the accessible ranges, a zero size field, owner bit with owner check on).
//! 3. `tx_state`/`rx_state`: state 0 and `tx_link_addr` = the TX descriptor means the channel
//!    never started (`LINK_START`); `tx_int_raw` `DONE`/`EOF` set means the picture was read.
//! 4. `jpeg_int_raw`: `EN_FRAME_EOF_ERR` (bit 16) means fewer blocks arrived than the size
//!    implies (TX descriptor HA/VA or hb/vb wrong); nothing set and `jpeg_status2` bits 0..23
//!    (pixels fetched) at 0 means the codec saw no data (TX `PERI_SEL` not 0).
//! 5. `rx_int_raw` `DSCR_EMPTY` (bit 10): the output was too small; nothing on RX with the codec
//!    done: RX `PERI_SEL` not 0 or RX not started.
//! 6. `axi_err` non-zero: an AXI error response (memory not reachable).

#![no_std]

pub mod dma2d;
mod header;

use core::{
    cell::UnsafeCell,
    future::poll_fn,
    sync::atomic::{AtomicBool, AtomicU32, Ordering, fence},
    task::Poll,
};

use esp_hal::{
    asynch::AtomicWaker,
    interrupt::{self, Priority},
    peripherals::Interrupt,
    system::Cpu,
};
use esp32p4 as pac;
// Cache maintenance, behind the lock shared with every other cache operation of the firmware.
use esp32p4_cache as cache;

use dma2d::{Descriptor, common, in_reg, out_reg};
pub use header::{HEADER_LEN, quant_table};

/// 2D-DMA TX channel that reads the picture: the only one with macroblock reordering.
pub const TX_CHANNEL: u8 = 0;
/// 2D-DMA RX channel that writes the bitstream (ESP-IDF picks the highest free one; 1 exists
/// on every chip revision and needs neither reordering nor colour conversion).
pub const RX_CHANNEL: u8 = 1;
/// Priority of the `JPEG` interrupt handler.
pub const INTERRUPT_PRIORITY: Priority = Priority::Priority1;
/// Alignment of the input picture's start.
pub const INPUT_ALIGN: usize = 16;
/// Largest width and height: the 14-bit fields of the codec's `PIC_SIZE` and the descriptor,
/// rounded down to a multiple of 8.
pub const MAX_SIDE: u16 = 16376;
/// Picture block the TX channel reads at a time: 128 x 8 pixels (TRM table 8.4-4 for 1 byte per
/// pixel and 8 x 8 macroblocks; ESP-IDF `enc_hb_tbl[GRAY][GRAY]`).
const BLOCK_W: u32 = 128;
const BLOCK_H: u32 = 8;
/// 2D-DMA burst length code: 4 = 128 bytes (ESP-IDF `data_burst_length = 128`).
const BURST_128: u32 = 4;
/// `IN_/OUT_MACRO_BLOCK_SIZE`: 8 x 8 and none.
const MB_8X8: u32 = 0;
const MB_NONE: u32 = 3;

// JPEG interrupt bits (PAC `jpeg::int_raw`, TRM §37.6).
const JPEG_DONE: u32 = 1 << 0;
const JPEG_RLE_PARALLEL_ERR: u32 = 1 << 1;
const JPEG_EN_FRAME_EOF_ERR: u32 = 1 << 16;
/// Encoder errors (ESP-IDF `JPEG_LL_ENCODER_EVENT_INTR`).
const JPEG_ERRORS: u32 = JPEG_RLE_PARALLEL_ERR | JPEG_EN_FRAME_EOF_ERR;
const JPEG_INT_ALL: u32 = (1 << 25) - 1;

/// Bounded spin for a 2D-DMA channel's `RESET_AVAIL` (it is immediate when the channel is idle).
const RESET_SPIN: u32 = 100_000;

/// Encoder settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Picture width in pixels, a multiple of 8, 8..=[`MAX_SIDE`].
    pub width: u16,
    /// Picture height in pixels, a multiple of 8, 8..=[`MAX_SIDE`].
    pub height: u16,
    /// Quality 1..=100, IJG scaling of the Annex K luminance table ([`quant_table`]).
    pub quality: u8,
}

impl Config {
    pub const fn new(width: u16, height: u16, quality: u8) -> Self {
        Self {
            width,
            height,
            quality,
        }
    }

    /// Input bytes: one per pixel.
    pub const fn pixels(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// An output length that holds the file whatever the picture: the header, 2 bytes per pixel
    /// (a baseline 8 x 8 block codes to at most about 1.2 bytes per pixel plus its stuffing
    /// bytes [INFERENCE: from the Annex K code lengths, not measured]), rounded up to 128.
    pub const fn max_output_len(&self) -> usize {
        (HEADER_LEN + 2 * self.pixels()).div_ceil(128) * 128
    }

    fn validate(&self) -> Result<(), Error> {
        let ok = |v: u16| (8..=MAX_SIDE).contains(&v) && v.is_multiple_of(8);
        if !ok(self.width) || !ok(self.height) {
            return Err(Error::Geometry {
                width: self.width,
                height: self.height,
            });
        }
        if !(1..=100).contains(&self.quality) {
            return Err(Error::Quality(self.quality));
        }
        Ok(())
    }
}

/// Driver errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Width or height not a multiple of 8 in 8..=[`MAX_SIDE`].
    Geometry { width: u16, height: u16 },
    /// Quality outside 1..=100.
    Quality(u8),
    /// An [`Encoder`] exists already.
    AlreadyInUse,
    /// The driver's descriptors are not in internal RAM (L2MEM): a linker layout this driver does
    /// not support.
    DescriptorMemory,
    /// The input is shorter than `width * height`.
    InputSize { needed: usize, len: usize },
    /// The input's start is not a multiple of [`INPUT_ALIGN`].
    InputAlignment,
    /// The output's start or length is not a multiple of `align`, its cache line.
    OutputAlignment { align: usize },
    /// The output cannot hold the file (`capacity` bytes): the RX descriptor ran out
    /// (`DMA2D_IN_DSCR_EMPTY`), or it is not longer than the header.
    OutputTooSmall { capacity: usize },
    /// The codec reported an error: its raw interrupt bits (bit 1 RLE error, bit 16 fewer blocks
    /// than the size implies).
    Codec { status: u32 },
    /// The 2D-DMA reported an error: raw interrupt bits of the TX and RX channels
    /// (`DSCR_ERR`, `ERR_EOF`).
    Dma { tx: u32, rx: u32 },
}

/// Event counters since [`Encoder::new`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames encoded.
    pub frames: u32,
    /// Encodes that ended in an error from the hardware, or were dropped unfinished.
    pub errors: u32,
    /// Reset sequences run (after an error or a dropped encode).
    pub resets: u32,
    /// Raw status of the last frame: JPEG interrupt bits, TX and RX channel interrupt bits.
    pub last_jpeg: u32,
    pub last_tx: u32,
    pub last_rx: u32,
}

/// Register snapshot for "no interrupt / no data" debugging. Reading it clears nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebugStatus {
    /// `HP_SYS_CLKRST_SOC_CLK_CTRL1` (JPEG bit 27, 2D-DMA bit 25) and `_HP_RST_EN0`.
    pub clk_ctrl1: u32,
    pub rst_en0: u32,
    /// `HP_SYS_CLKRST_ROOT_CLK_CTRL0..2`: CPU, MEM, SYS and APB dividers.
    pub root_clk: [u32; 3],
    pub jpeg_config: u32,
    pub jpeg_pic_size: u32,
    pub jpeg_int_raw: u32,
    pub jpeg_int_ena: u32,
    pub jpeg_status0: u32,
    /// Bits 0..23: pixels fetched from the DMA.
    pub jpeg_status2: u32,
    pub jpeg_version: u32,
    pub date: u32,
    pub axi_err: u32,
    pub intr_mem: (u32, u32),
    pub extr_mem: (u32, u32),
    pub tx_conf0: u32,
    pub tx_int_raw: u32,
    pub tx_state: u32,
    pub tx_link_addr: u32,
    pub tx_eof_des_addr: u32,
    pub rx_conf0: u32,
    pub rx_int_raw: u32,
    pub rx_state: u32,
    pub rx_link_addr: u32,
    pub rx_suc_eof_des_addr: u32,
    /// The RX descriptor's first two words as in memory.
    pub rx_desc: (u32, u32),
}

// ── Shared state ────────────────────────────────────────────────────────────────────────────

static TAKEN: AtomicBool = AtomicBool::new(false);
/// JPEG, TX and RX interrupt bits seen by the handlers since the frame started.
static EVENTS: AtomicU32 = AtomicU32::new(0);
static TX_EVENTS: AtomicU32 = AtomicU32::new(0);
static RX_EVENTS: AtomicU32 = AtomicU32::new(0);
static WAKER: AtomicWaker = AtomicWaker::new();

/// The file header of the current settings. Kept out of [`Encoder`] so the encoder is a few
/// words (it often lives in an async task's state).
struct HeaderBuf(UnsafeCell<[u8; HEADER_LEN]>);
// SAFETY: only the one `Encoder` (see `TAKEN`) touches it, through `&self` / `&mut self`.
unsafe impl Sync for HeaderBuf {}
static HEADER: HeaderBuf = HeaderBuf(UnsafeCell::new([0; HEADER_LEN]));
/// The hardware needs the reset sequence before the next frame.
static DIRTY: AtomicBool = AtomicBool::new(false);

/// The TX and RX descriptor, one cache line, accessed by the CPU only through the non-cacheable
/// alias (its cached line is written back and dropped once in `new`).
#[repr(C, align(64))]
struct Descs {
    tx: Descriptor,
    _pad: [u32; 2],
    rx: Descriptor,
    _pad2: [u32; 2],
}

const _: () = assert!(core::mem::size_of::<Descs>() == 64);

static mut DESCS: Descs = Descs {
    tx: Descriptor {
        dw0: 0,
        dw1: 0,
        dw2: 0,
        buffer: 0,
        next: 0,
    },
    _pad: [0; 2],
    rx: Descriptor {
        dw0: 0,
        dw1: 0,
        dw2: 0,
        buffer: 0,
        next: 0,
    },
    _pad2: [0; 2],
};

fn descs_addr() -> usize {
    (&raw const DESCS) as usize
}

/// Bus address of the TX descriptor (the DMA's view: the cached address).
fn tx_desc_addr() -> u32 {
    descs_addr() as u32
}

fn rx_desc_addr() -> u32 {
    (descs_addr() + core::mem::offset_of!(Descs, rx)) as u32
}

/// Writes a descriptor through the non-cacheable alias.
fn write_desc(bus_addr: u32, d: &Descriptor) {
    let p = cache::non_cacheable(bus_addr as usize) as *mut u32;
    for (i, w) in [d.dw0, d.dw1, d.dw2, d.buffer, d.next]
        .into_iter()
        .enumerate()
    {
        // SAFETY: `p` is the non-cacheable alias of a descriptor in the driver's static.
        unsafe { core::ptr::write_volatile(p.add(i), w) };
    }
}

fn read_desc(bus_addr: u32) -> (u32, u32) {
    let p = cache::non_cacheable(bus_addr as usize) as *const u32;
    // SAFETY: as in `write_desc`.
    unsafe {
        (
            core::ptr::read_volatile(p),
            core::ptr::read_volatile(p.add(1)),
        )
    }
}

fn jpeg() -> &'static pac::jpeg::RegisterBlock {
    unsafe { &*pac::JPEG::ptr() }
}

fn clkrst() -> &'static pac::hp_sys_clkrst::RegisterBlock {
    unsafe { &*pac::HP_SYS_CLKRST::ptr() }
}

/// JPEG interrupt: records the status bits and wakes the waiting `encode`.
#[esp_hal::ram]
#[esp_hal::handler(priority = Priority::Priority1)]
fn jpeg_isr() {
    let j = jpeg();
    let st = j.int_st().read().bits();
    j.int_clr().write(|w| unsafe { w.bits(st) });
    EVENTS.fetch_or(st, Ordering::Release);
    WAKER.wake();
}

/// 2D-DMA TX and RX channel interrupts: records the status bits and wakes `encode`.
#[esp_hal::ram]
#[esp_hal::handler(priority = Priority::Priority1)]
fn dma2d_isr() {
    let t = dma2d::out(TX_CHANNEL, out_reg::INT_ST);
    let st = t.read();
    dma2d::out(TX_CHANNEL, out_reg::INT_CLR).write(st);
    TX_EVENTS.fetch_or(st, Ordering::Release);
    let r = dma2d::inp(RX_CHANNEL, in_reg::INT_ST);
    let st = r.read();
    dma2d::inp(RX_CHANNEL, in_reg::INT_CLR).write(st);
    RX_EVENTS.fetch_or(st, Ordering::Release);
    WAKER.wake();
}

/// Interrupt sources of the 2D-DMA channels used (ESP-IDF `soc/esp32p4/include/soc/interrupts.h`:
/// `ETS_DMA2D_IN_CH0_INTR_SOURCE` = 104, `IN_CH1` = 105, `OUT_CH0` = 106).
const SOURCE_RX: usize = 104 + RX_CHANNEL as usize;
const SOURCE_TX: usize = 106 + TX_CHANNEL as usize;
const _: () = assert!(RX_CHANNEL < 2 && TX_CHANNEL < 3);

/// Routes the 2D-DMA sources to the CPU interrupt `JPEG` is mapped to, with `dma2d_isr` as
/// their handler (see "Interrupts").
fn bind_dma2d(cpu: Cpu) {
    unsafe extern "Rust" {
        static __EXTERNAL_INTERRUPTS: pac::Vector;
    }
    let table = (&raw const __EXTERNAL_INTERRUPTS).cast_mut();
    let map = read_map(cpu, Interrupt::JPEG as usize);
    for source in [SOURCE_RX, SOURCE_TX] {
        // SAFETY: the PAC's vector table has 128 entries in RAM (`.rwtext.interrupt`); entries
        // 105 and 106 are unused by the PAC. esp-hal's `bind_handler` writes it the same way.
        unsafe {
            table
                .add(source)
                .cast::<usize>()
                .write_volatile(dma2d_isr.handler().address());
        }
        write_map(cpu, source, map);
    }
}

fn read_map(cpu: Cpu, source: usize) -> u32 {
    match cpu {
        Cpu::ProCpu => unsafe { &*pac::INTERRUPT_CORE0::ptr() }
            .core_0_intr_map(source)
            .read()
            .bits(),
        Cpu::AppCpu => unsafe { &*pac::INTERRUPT_CORE1::ptr() }
            .core_1_intr_map(source)
            .read()
            .bits(),
    }
}

fn write_map(cpu: Cpu, source: usize, map: u32) {
    match cpu {
        Cpu::ProCpu => unsafe { &*pac::INTERRUPT_CORE0::ptr() }
            .core_0_intr_map(source)
            .write(|w| unsafe { w.bits(map) }),
        Cpu::AppCpu => unsafe { &*pac::INTERRUPT_CORE1::ptr() }
            .core_1_intr_map(source)
            .write(|w| unsafe { w.bits(map) }),
    };
}
// ── Driver ──────────────────────────────────────────────────────────────────────────────────

/// The JPEG encoder.
pub struct Encoder {
    config: Config,
    stats: Stats,
    /// Core whose interrupt matrix routes the interrupts.
    cpu: Cpu,
}

impl Encoder {
    /// Enables and resets the JPEG codec and the 2D-DMA, programs them for `config` and binds
    /// the `JPEG` interrupt on the current core. See "Bring-up sequence".
    pub fn new(config: Config) -> Result<Self, Error> {
        config.validate()?;
        let d = descs_addr();
        if !cache::in_l2mem(d, 64) {
            return Err(Error::DescriptorMemory);
        }
        if TAKEN.swap(true, Ordering::Acquire) {
            return Err(Error::AlreadyInUse);
        }
        // Nothing but the non-cacheable alias touches the descriptors from here on.
        cache::write_back(d, 64);
        cache::invalidate(d, 64);

        enable_clocks();
        init_dma();
        configure_channels();
        let mut enc = Self {
            config,
            stats: Stats::default(),
            cpu: Cpu::current(),
        };
        enc.build_header();
        program_codec(&config);
        DIRTY.store(false, Ordering::Relaxed);
        EVENTS.store(0, Ordering::Relaxed);
        interrupt::bind_handler(Interrupt::JPEG, jpeg_isr);
        bind_dma2d(enc.cpu);
        Ok(enc)
    }

    /// Current settings.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Changes size and/or quality: rebuilds the header and reprograms the codec (tables,
    /// picture size).
    pub fn set_config(&mut self, config: Config) -> Result<(), Error> {
        config.validate()?;
        self.config = config;
        self.build_header();
        program_codec(&config);
        Ok(())
    }

    /// The file header for the current settings: SOI to SOS, [`HEADER_LEN`] bytes.
    pub fn header(&self) -> &[u8; HEADER_LEN] {
        // SAFETY: written only through `&mut self` (`build_header`).
        unsafe { &*HEADER.0.get() }
    }

    fn build_header(&mut self) {
        let quant = quant_table(self.config.quality);
        // SAFETY: `&mut self` of the only encoder: no other reference to the header.
        let header = unsafe { &mut *HEADER.0.get() };
        header::build(header, self.config.width, self.config.height, &quant);
    }

    /// Encodes the `width * height` pixels at the start of `input` into `output` and returns
    /// the length of the JPEG file there (`output[..len]`, from `FF D8` to `FF D9`). See "Cache
    /// and buffers" for what `input` and `output` need. Never blocks: waits on the codec's
    /// interrupt. Dropping the future before it completes stops the DMA (see "Errors and
    /// recovery").
    pub async fn encode(&mut self, input: &[u8], output: &mut [u8]) -> Result<usize, Error> {
        let cfg = self.config;
        let needed = cfg.pixels();
        if input.len() < needed {
            return Err(Error::InputSize {
                needed,
                len: input.len(),
            });
        }
        let in_addr = input.as_ptr() as usize;
        if !in_addr.is_multiple_of(INPUT_ALIGN) {
            return Err(Error::InputAlignment);
        }
        let out_addr = output.as_mut_ptr() as usize;
        let line = cache::line_size(out_addr);
        let out_len = output.len();
        if !out_addr.is_multiple_of(line) || !output.len().is_multiple_of(line) {
            return Err(Error::OutputAlignment { align: line });
        }
        if output.len() <= HEADER_LEN {
            return Err(Error::OutputTooSmall {
                capacity: output.len(),
            });
        }
        let capacity = output.len() - HEADER_LEN;
        let payload = out_addr + HEADER_LEN;

        if DIRTY.load(Ordering::Acquire) {
            configure_channels();
            program_codec(&cfg);
            DIRTY.store(false, Ordering::Release);
            self.stats.resets += 1;
        }

        output[..HEADER_LEN].copy_from_slice(self.header());
        cache::write_back(out_addr, HEADER_LEN);
        cache::invalidate(payload, capacity);

        write_desc(
            tx_desc_addr(),
            &Descriptor::picture(
                in_addr as u32,
                u32::from(cfg.width),
                u32::from(cfg.height),
                BLOCK_W,
                BLOCK_H,
            ),
        );
        write_desc(
            rx_desc_addr(),
            &Descriptor::receive(payload as u32, capacity as u32),
        );

        let j = jpeg();
        EVENTS.store(0, Ordering::Relaxed);
        TX_EVENTS.store(0, Ordering::Relaxed);
        RX_EVENTS.store(0, Ordering::Relaxed);
        j.int_clr().write(|w| unsafe { w.bits(JPEG_INT_ALL) });
        let (tx, rx) = (TX_CHANNEL, RX_CHANNEL);
        dma2d::out(tx, out_reg::INT_CLR).write(out_reg::INT_ALL);
        dma2d::inp(rx, in_reg::INT_CLR).write(in_reg::INT_ALL);
        dma2d::out(tx, out_reg::LINK_ADDR).write(tx_desc_addr());
        dma2d::inp(rx, in_reg::LINK_ADDR).write(rx_desc_addr());
        fence(Ordering::SeqCst);

        // From here an unfinished frame must be stopped.
        let mut guard = InFlight { armed: true };
        dma2d::out(tx, out_reg::LINK_CONF).modify(0, out_reg::LINK_START);
        dma2d::inp(rx, in_reg::LINK_CONF).modify(0, in_reg::LINK_START);
        j.config().modify(|_, w| w.jpeg_start().set_bit());

        let result = poll_fn(|cx| {
            WAKER.register(cx.waker());
            let ev = EVENTS.load(Ordering::Acquire);
            let txs = TX_EVENTS.load(Ordering::Acquire);
            let rxs = RX_EVENTS.load(Ordering::Acquire);
            if ev & JPEG_ERRORS != 0 {
                return Poll::Ready(Err((Error::Codec { status: ev }, ev, txs, rxs)));
            }
            if rxs & in_reg::INT_DSCR_EMPTY != 0 {
                return Poll::Ready(Err((
                    Error::OutputTooSmall { capacity: out_len },
                    ev,
                    txs,
                    rxs,
                )));
            }
            if txs & out_reg::INT_DSCR_ERR != 0
                || rxs & (in_reg::INT_DSCR_ERR | in_reg::INT_ERR_EOF) != 0
            {
                return Poll::Ready(Err((Error::Dma { tx: txs, rx: rxs }, ev, txs, rxs)));
            }
            if rxs & in_reg::INT_SUC_EOF != 0 {
                return Poll::Ready(Ok((ev, txs, rxs)));
            }
            Poll::Pending
        })
        .await;

        match result {
            Ok((ev, txs, rxs)) => {
                let (dw0, dw1) = read_desc(rx_desc_addr());
                let n = Descriptor::received_len(dw0, dw1) as usize;
                self.stats.last_jpeg = ev;
                self.stats.last_tx = txs;
                self.stats.last_rx = rxs;
                if n == 0 || n > capacity {
                    // Only a broken descriptor write-back could do this.
                    drop(guard);
                    self.stats.errors += 1;
                    return Err(Error::Dma { tx: txs, rx: rxs });
                }
                guard.armed = false;
                cache::invalidate(payload, n.div_ceil(line) * line);
                self.stats.frames += 1;
                Ok(HEADER_LEN + n)
            }
            Err((e, ev, txs, rxs)) => {
                self.stats.last_jpeg = ev;
                self.stats.last_tx = txs;
                self.stats.last_rx = rxs;
                self.stats.errors += 1;
                // `guard` stops the DMA and marks the hardware for the reset sequence.
                drop(guard);
                Err(e)
            }
        }
    }

    /// Event counters.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Register snapshot, for debugging. Clears nothing.
    pub fn debug_status(&self) -> DebugStatus {
        let j = jpeg();
        let c = clkrst();
        let (tx, rx) = (TX_CHANNEL, RX_CHANNEL);
        DebugStatus {
            clk_ctrl1: c.soc_clk_ctrl1().read().bits(),
            rst_en0: c.hp_rst_en0().read().bits(),
            root_clk: [
                c.root_clk_ctrl0().read().bits(),
                c.root_clk_ctrl1().read().bits(),
                c.root_clk_ctrl2().read().bits(),
            ],
            jpeg_config: j.config().read().bits(),
            jpeg_pic_size: j.pic_size().read().bits(),
            jpeg_int_raw: j.int_raw().read().bits(),
            jpeg_int_ena: j.int_ena().read().bits(),
            jpeg_status0: j.status0().read().bits(),
            jpeg_status2: j.status2().read().bits(),
            jpeg_version: j.version().read().bits(),
            date: dma2d::common(common::DATE).read(),
            axi_err: dma2d::common(common::AXI_ERR).read(),
            intr_mem: (
                dma2d::common(common::INTR_MEM_START).read(),
                dma2d::common(common::INTR_MEM_END).read(),
            ),
            extr_mem: (
                dma2d::common(common::EXTR_MEM_START).read(),
                dma2d::common(common::EXTR_MEM_END).read(),
            ),
            tx_conf0: dma2d::out(tx, out_reg::CONF0).read(),
            tx_int_raw: dma2d::out(tx, out_reg::INT_RAW).read(),
            tx_state: dma2d::out(tx, out_reg::STATE).read(),
            tx_link_addr: dma2d::out(tx, out_reg::LINK_ADDR).read(),
            tx_eof_des_addr: dma2d::out(tx, out_reg::EOF_DES_ADDR).read(),
            rx_conf0: dma2d::inp(rx, in_reg::CONF0).read(),
            rx_int_raw: dma2d::inp(rx, in_reg::INT_RAW).read(),
            rx_state: dma2d::inp(rx, in_reg::STATE).read(),
            rx_link_addr: dma2d::inp(rx, in_reg::LINK_ADDR).read(),
            rx_suc_eof_des_addr: dma2d::inp(rx, in_reg::SUC_EOF_DES_ADDR).read(),
            rx_desc: read_desc(rx_desc_addr()),
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        interrupt::disable(self.cpu, Interrupt::JPEG);
        // `JPEG`'s map register now holds the disabled CPU interrupt: copy it.
        let off = read_map(self.cpu, Interrupt::JPEG as usize);
        write_map(self.cpu, SOURCE_RX, off);
        write_map(self.cpu, SOURCE_TX, off);
        halt();
        TAKEN.store(false, Ordering::Release);
    }
}

/// Stops the hardware if an `encode` ends without its frame (error or dropped future).
struct InFlight {
    armed: bool,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if self.armed {
            halt();
        }
    }
}

/// Writes the CPU's dirty lines of `buf` back to memory, so a DMA reads what the CPU wrote (for
/// an input picture the CPU drew).
pub fn write_back(buf: &[u8]) {
    cache::write_back(buf.as_ptr() as usize, buf.len());
}

/// Stops both channels and the codec and marks the hardware for the reset sequence.
fn halt() {
    dma2d::out(TX_CHANNEL, out_reg::LINK_CONF).modify(0, out_reg::LINK_STOP);
    dma2d::inp(RX_CHANNEL, in_reg::LINK_CONF).modify(0, in_reg::LINK_STOP);
    reset_tx(TX_CHANNEL);
    reset_rx(RX_CHANNEL);
    let j = jpeg();
    j.config().modify(|_, w| w.soft_rst().set_bit());
    j.config().modify(|_, w| w.soft_rst().clear_bit());
    DIRTY.store(true, Ordering::Release);
}

/// Clock gates on and reset pulses (TRM §37.7.1 step 1, §8.7; ESP-IDF
/// `jpeg_ll_enable_bus_clock`, `jpeg_ll_reset_module_register`, `dma2d_ll_enable_bus_clock`,
/// `dma2d_ll_reset_register`). `HP_RST_EN0` also holds the CSI's and the DW-GDMA's resets:
/// read-modify-write in a critical section.
fn enable_clocks() {
    critical_section::with(|_| {
        let c = clkrst();
        c.soc_clk_ctrl1().modify(|_, w| {
            w.jpeg_sys_clk_en().set_bit();
            w.dma2d_sys_clk_en().set_bit()
        });
        c.hp_rst_en0().modify(|_, w| {
            w.rst_en_jpeg().set_bit();
            w.rst_en_dma2d().set_bit()
        });
        c.hp_rst_en0().modify(|_, w| {
            w.rst_en_jpeg().clear_bit();
            w.rst_en_dma2d().clear_bit()
        });
    });
}

/// 2D-DMA common set-up (`dma2d_ll_hw_enable`).
fn init_dma() {
    let r = dma2d::common(common::RST_CONF);
    r.write(common::CLK_EN);
    r.write(common::CLK_EN | common::AXIM_RD_RST | common::AXIM_WR_RST);
    r.write(common::CLK_EN);
    dma2d::common(common::OUT_ARB_CONFIG).write(0);
    dma2d::common(common::IN_ARB_CONFIG).write(0);
}

/// TRM §8.7.7: disable, wait until the reset is safe, pulse the reset, enable.
fn reset_tx(ch: u8) {
    let conf = dma2d::out(ch, out_reg::CONF0);
    conf.modify(0, out_reg::CMD_DISABLE);
    let state = dma2d::out(ch, out_reg::STATE);
    for _ in 0..RESET_SPIN {
        if state.read() & out_reg::RESET_AVAIL != 0 {
            break;
        }
    }
    conf.modify(0, out_reg::RST);
    conf.modify(out_reg::RST, 0);
    conf.modify(out_reg::CMD_DISABLE, 0);
}

fn reset_rx(ch: u8) {
    let conf = dma2d::inp(ch, in_reg::CONF0);
    conf.modify(0, in_reg::CMD_DISABLE);
    let state = dma2d::inp(ch, in_reg::STATE);
    for _ in 0..RESET_SPIN {
        if state.read() & in_reg::RESET_AVAIL != 0 {
            break;
        }
    }
    conf.modify(0, in_reg::RST);
    conf.modify(in_reg::RST, 0);
    conf.modify(in_reg::CMD_DISABLE, 0);
}

/// Both channels reset and configured for the JPEG encoder (`dma2d_connect`,
/// `dma2d_set_transfer_ability` as `jpeg_enc_config_dma_trans_ability` calls it).
fn configure_channels() {
    let (tx, rx) = (TX_CHANNEL, RX_CHANNEL);
    dma2d::out(tx, out_reg::LINK_CONF).modify(0, out_reg::LINK_STOP);
    dma2d::inp(rx, in_reg::LINK_CONF).modify(0, in_reg::LINK_STOP);
    reset_tx(tx);
    dma2d::out(tx, out_reg::PERI_SEL).write(0);
    dma2d::out(tx, out_reg::CONF0).write(
        out_reg::EOF_MODE
            | out_reg::DSCR_BURST_EN
            | (BURST_128 << out_reg::BURST_SHIFT)
            | (MB_8X8 << out_reg::MB_SHIFT)
            | out_reg::PAGE_BOUND_EN
            | out_reg::REORDER_EN,
    );
    dma2d::out(tx, out_reg::COLOR_CONVERT).write(out_reg::CSC_NONE);
    dma2d::out(tx, out_reg::INT_ENA).write(out_reg::INT_DSCR_ERR);
    dma2d::out(tx, out_reg::INT_CLR).write(out_reg::INT_ALL);

    reset_rx(rx);
    dma2d::inp(rx, in_reg::PERI_SEL).write(0);
    dma2d::inp(rx, in_reg::CONF0).write(
        in_reg::DSCR_BURST_EN
            | (BURST_128 << in_reg::BURST_SHIFT)
            | (MB_NONE << in_reg::MB_SHIFT)
            | in_reg::PAGE_BOUND_EN,
    );
    // Owner written back as 0 (CPU); start/stop bits are self-clearing.
    dma2d::inp(rx, in_reg::LINK_CONF).modify(in_reg::AUTO_RET, 0);
    dma2d::inp(rx, in_reg::INT_ENA).write(
        in_reg::INT_SUC_EOF | in_reg::INT_ERR_EOF | in_reg::INT_DSCR_ERR | in_reg::INT_DSCR_EMPTY,
    );
    dma2d::inp(rx, in_reg::INT_CLR).write(in_reg::INT_ALL);
}

/// Codec reset and set-up for `cfg`, with the quantisation table of its quality (natural
/// order) (TRM §37.7.1 step 2, §37.5.1.3; ESP-IDF `jpeg_encoder_process` up to the header,
/// `jpeg_hal_set_quantization_coefficient`).
fn program_codec(cfg: &Config) {
    let quant = quant_table(cfg.quality);
    let j = jpeg();
    j.int_ena().write(|w| unsafe { w.bits(0) });
    j.config().modify(|_, w| w.soft_rst().set_bit());
    j.config().modify(|_, w| w.soft_rst().clear_bit());
    j.config().modify(|_, w| unsafe {
        w.mode().clear_bit(); // encoder
        w.color_space().bits(3); // GRAY
        w.pixel_rev().clear_bit();
        w.tailer_en().set_bit(); // EOI at the end
        w.ff_check_en().set_bit(); // 00 after FF
        w.qnr_precision().clear_bit(); // 8-bit tables
        w.qnr_fifo_en().set_bit();
        w.lqnr_tbl_sel().bits(0);
        w.cqnr_tbl_sel().bits(1);
        w.pause_en().clear_bit()
    });
    j.extd_config().modify(|_, w| {
        w.extd_color_space_en().clear_bit();
        w.extd_color_space().clear_bit()
    });
    j.pic_size().write(|w| unsafe {
        w.va().bits(cfg.height);
        w.ha().bits(cfg.width)
    });
    j.dqt_info().write(|w| unsafe {
        w.t0_dqt_info().bits(0);
        w.t1_dqt_info().bits(1);
        w.t2_dqt_info().bits(2);
        w.t3_dqt_info().bits(3)
    });
    // The PAC marks `T0QNR`..`T3QNR` read-only; the TRM (§37.5.1.3, FIFO mode) and ESP-IDF
    // (`jpeg_ll_write_quantization_coefficient_t0`..`t3`) write them: 64 values each.
    for reg in [
        j.t0qnr().as_ptr(),
        j.t1qnr().as_ptr(),
        j.t2qnr().as_ptr(),
        j.t3qnr().as_ptr(),
    ] {
        for &q in &quant {
            // SAFETY: a codec register, written as the TRM describes.
            unsafe { core::ptr::write_volatile(reg, u32::from(q)) };
        }
    }
    j.int_clr().write(|w| unsafe { w.bits(JPEG_INT_ALL) });
    j.int_ena()
        .write(|w| unsafe { w.bits(JPEG_DONE | JPEG_ERRORS) });
}
