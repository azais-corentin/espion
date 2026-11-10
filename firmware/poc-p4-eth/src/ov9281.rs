//! OV9281 register settings for the PoC: free-running modes, RAW8 or RAW10, two MIPI CSI-2 data
//! lanes at 800 Mbit/s each, 24 MHz external clock (XVCLK). `core` only.
//!
//! [`MODES`] indexes 0..=3 are the 120 fps RAW8 modes (1280x800, 1280x720 crop, 640x480 crop,
//! 640x400 binned). Indexes 4..=11 cover the Inno-Maker CAM-MIPIOV9281 V2 mode list: RAW8
//! 1280x800 at 144 fps, 1280x720 at 144, 640x400 binned at 210 and 253, 320x200 at 453; RAW10
//! 1280x800 at 120, 1280x720 at 120, 640x400 binned at 210. Indexes 4 and 5 run the system
//! clock at 96 MHz, above the datasheet's 80 MHz; every other mode stays at 80 MHz.
//!
//! The unbinned 1280x720 window needs VTS >= 814 (measured on the board at 80 MHz, also with
//! the window moved to rows 0..=735): VTS 763 gives a dark image whose bright areas read
//! lowest, VTS 780..=810 a flat black-level frame, VTS 814..=820 the normal image, while the
//! frames themselves stay whole and on time [INFERENCE: the unbinned readout needs about the
//! full 816 array rows]. The binned modes read two rows per line and run at VTS 434.
//!
//! Sources and derivations are in the PoC notes (`poc-ov9281.md`). In short: the common block
//! is Espressif's `esp_cam_sensor` OV9281 table (RAW8, 2 lanes, 24 MHz, 800 Mbit/s), the windows
//! come from the Linux `ov9282.c` and Raspberry Pi `ov9281.c` drivers (which also give the RAW10
//! and 96 MHz register values), and HTS/VTS, the 640x480 and 320x200 windows and the 8-bit
//! register choices are derived from the OV9281 datasheet v1.1.
//!
//! Sequence for the caller (SCCB: 16-bit register address, 8-bit value):
//! 1. Read [`CHIP_ID_HIGH`] and [`CHIP_ID_LOW`] and compare with [`CHIP_ID`].
//! 2. Write every pair of [`Mode::regs`] in order. A pair whose address is [`DELAY`] is not a
//!    register: wait its value in milliseconds instead. The list starts with a software reset,
//!    leaves the sensor in software standby, and sets exposure [`Mode::default_exposure_lines`]
//!    and gain [`DEFAULT_GAIN`].
//! 3. Optionally write [`Mode::exposure_regs`], [`gain_regs`], [`TEST_PATTERN_ON`] or
//!    [`MIPI_CLOCK_GATED`] (the last only in standby).
//! 4. Start the CSI receiver (data type [`Mode::data_type`]), then write [`STREAM_ON`]. The
//!    datasheet allows up to 10 ms from streaming to the first frame.
//!
//! The sensor free-runs: FSIN/XVS trigger mode stays off (0x3030 bit 2 = 0) and the STROBE output
//! stays disabled (0x3006 bit 3 = 0).

/// Marker address in a register list: the pair `(DELAY, n)` means "wait `n` ms".
pub const DELAY: u16 = 0xFFFF;

/// 7-bit SCCB address with SID low (8-bit write address 0xC0, datasheet v1.1 p.1-9).
pub const SCCB_ADDR: u8 = 0x60;
/// 7-bit SCCB address with SID high (8-bit write address 0x20).
pub const SCCB_ADDR_ALT: u8 = 0x10;

/// Chip ID high byte register (reads 0x92).
pub const CHIP_ID_HIGH: u16 = 0x300A;
/// Chip ID low byte register (reads 0x81).
pub const CHIP_ID_LOW: u16 = 0x300B;
/// Expected `{0x300A, 0x300B}`.
pub const CHIP_ID: u16 = 0x9281;

/// SC_MODE_SELECT = 1: streaming.
pub const STREAM_ON: (u16, u8) = (0x0100, 0x01);
/// SC_MODE_SELECT = 0: software standby (the current frame completes first, 0x0106 = 0).
pub const STREAM_OFF: (u16, u8) = (0x0100, 0x00);
/// SC_SOFTWARE_RESET: resets every register to its default; follow it with [`RESET_DELAY_MS`].
pub const SOFTWARE_RESET: (u16, u8) = (0x0103, 0x01);
/// Wait after [`SOFTWARE_RESET`] (Espressif's driver waits 5 ms; the datasheet gives no figure).
pub const RESET_DELAY_MS: u8 = 5;

/// PRE_CTRL00 (0x5E00): standard test pattern bar on (bit 7). Other values of this register:
/// 0x82 grey squares, 0x92 black-and-white squares, 0xC0 rolling bars, 0x81 random data.
pub const TEST_PATTERN_ON: (u16, u8) = (0x5E00, 0x80);
/// PRE_CTRL00 (0x5E00): test pattern off, sensor image.
pub const TEST_PATTERN_OFF: (u16, u8) = (0x5E00, 0x00);

/// MIPI_CTRL00 (0x4800) as the modes set it: clock lane free-running (continuous HS clock),
/// no line short packets.
pub const MIPI_CLOCK_CONTINUOUS: (u16, u8) = (0x4800, 0x00);
/// MIPI_CTRL00 (0x4800) as Espressif sets it: clock lane gated between packets (bit 5; bit 6 is
/// reserved in the datasheet but set by Espressif). Write in standby only.
pub const MIPI_CLOCK_GATED: (u16, u8) = (0x4800, 0x60);

/// System clock (PLL2 sys clk) of the 80 MHz modes in Hz: 24 MHz / 3 x 80 / 4 / 2. HTS counts
/// these cycles. Each mode's own clock is [`Mode::sclk_mhz`].
pub const SCLK_HZ: u32 = 80_000_000;
/// MIPI bit rate per data lane in Mbit/s: 24 MHz / 1.5 x 50 / 1 (PLL1), the same in every mode.
pub const LANE_MBPS: u16 = 800;

/// HTS register value (SCLK cycles per line) of every mode: 728 / 80 MHz = 9.1 us per line,
/// 728 / 96 MHz = 7.583 us.
pub const HTS: u16 = 728;
/// A frame lasts `(VTS - 1) * HTS + FRAME_LONG_LINE` SCLK cycles, not `VTS * HTS`: as if one
/// line per frame lasted this many cycles instead of HTS (an inference). Measured on the board
/// (2026-11-10): 21 runs of 10 s over HTS 728..=2912, VTS 434..=1830, SCLK 80 and 96 MHz,
/// RAW8, RAW10, cropped and binned, exposure 100..=800 lines fit this within 1.4 cycles, with
/// 1730.2 cycles and the sensor clock 28 ppm fast against the ESP32-P4's timer. The datasheet
/// does not give it. At HTS 728 the frame is 1.38 lines longer than VTS (0.15 % at VTS 915,
/// 0.32 % at VTS 434); with HTS above 1730 it is shorter.
pub const FRAME_LONG_LINE: u32 = 1730;
/// VTS register value (lines per frame) for 120 fps: 80 MHz / (914 x 728 + 1730) =
/// 119.918 fps.
pub const VTS_120FPS: u16 = 915;

/// CSI-2 data type of RAW8 long packets: one byte per pixel.
pub const DT_RAW8: u8 = 0x2A;
/// CSI-2 data type of RAW10 long packets. Packing (CSI-2 RAW10): every 4 pixels P0..P3 take
/// 5 bytes, `P0[9:2] P1[9:2] P2[9:2] P3[9:2]`, then `P3[1:0] << 6 | P2[1:0] << 4 |
/// P1[1:0] << 2 | P0[1:0]`; a line is `width * 5 / 4` bytes (1600 for 1280, 800 for 640).
pub const DT_RAW10: u8 = 0x2B;

/// Exposure must stay this many lines below VTS (Linux, Raspberry Pi and Espressif drivers;
/// datasheet v1.1 says 12).
pub const EXPOSURE_MAX_OFFSET: u16 = 25;
/// Shortest exposure in lines (datasheet: 1 row period).
pub const EXPOSURE_MIN_LINES: u16 = 1;
/// Default exposure of the 80 MHz modes: 110 lines x 9.1 us = 1.001 ms. Each mode's own default
/// is [`Mode::default_exposure_lines`].
pub const DEFAULT_EXPOSURE_LINES: u16 = 110;
/// Default exposure of the 96 MHz modes: 132 lines x 7.583 us = 1.001 ms.
pub const DEFAULT_EXPOSURE_LINES_96MHZ: u16 = 132;

/// Lowest gain, 1x (real gain format, 4 fraction bits).
pub const GAIN_MIN: u8 = 0x10;
/// Highest gain used by the Raspberry Pi and Espressif drivers, 15.5x.
pub const GAIN_MAX: u8 = 0xF8;
/// Default gain, 1x.
pub const DEFAULT_GAIN: u8 = GAIN_MIN;

/// One sensor mode.
pub struct Mode {
    pub name: &'static str,
    /// Output width in pixels.
    pub width: u16,
    /// Output height in lines.
    pub height: u16,
    /// Nominal frame rate in whole frames per second, as in the mode lists (120, 144, ...);
    /// the exact one is [`Mode::frame_rate_mhz`] (119918 mHz for "120", 143901 for "144").
    pub fps: u16,
    /// MIPI bit rate per data lane, for the receiver's D-PHY setup.
    pub lane_mbps: u16,
    /// HTS register value, in SCLK cycles per line (Linux counts it as 2 x this, in pixels).
    pub hts: u16,
    /// VTS register value, in lines per frame.
    pub vts: u16,
    /// Full register list from software reset to standby, ready to stream.
    pub regs: &'static [(u16, u8)],
    /// Bits per pixel on the wire: 8 (RAW8) or 10 (RAW10).
    pub bits_per_pixel: u8,
    /// CSI-2 data type of the image packets, [`DT_RAW8`] or [`DT_RAW10`], on virtual channel 0.
    pub data_type: u8,
    /// Sensor system clock (PLL2 sys clk) in MHz at XVCLK 24 MHz: 80 (datasheet maximum) or 96.
    /// HTS counts its cycles; the array's pixel rate is twice it.
    pub sclk_mhz: u8,
    /// Exposure that `regs` sets, in lines: about 1 ms, within [`Mode::max_exposure_lines`].
    pub default_exposure_lines: u16,
}

impl Mode {
    /// Line period in nanoseconds.
    pub const fn line_ns(&self) -> u32 {
        (self.hts as u64 * 1000 / self.sclk_mhz as u64) as u32
    }

    /// SCLK cycles per frame: `(VTS - 1) * HTS + FRAME_LONG_LINE`, as measured.
    pub const fn frame_cycles(&self) -> u64 {
        (self.vts as u64 - 1) * self.hts as u64 + FRAME_LONG_LINE as u64
    }

    /// Frame period in nanoseconds.
    pub const fn frame_ns(&self) -> u32 {
        (self.frame_cycles() * 1000 / self.sclk_mhz as u64) as u32
    }

    /// Frame rate in millihertz.
    pub const fn frame_rate_mhz(&self) -> u32 {
        (self.sclk_mhz as u64 * 1_000_000_000 / self.frame_cycles()) as u32
    }

    /// Bytes of image data per line on the wire (RAW10: 5 bytes per 4 pixels).
    pub const fn line_bytes(&self) -> u32 {
        self.width as u32 * self.bits_per_pixel as u32 / 8
    }

    /// Bytes of image data per frame on the wire.
    pub const fn frame_bytes(&self) -> u32 {
        self.line_bytes() * self.height as u32
    }

    /// Longest exposure in lines for this mode's VTS.
    pub const fn max_exposure_lines(&self) -> u16 {
        self.vts - EXPOSURE_MAX_OFFSET
    }

    /// [`exposure_regs`] with `lines` clamped to
    /// [`EXPOSURE_MIN_LINES`]..=[`Mode::max_exposure_lines`], so the exposure never stretches
    /// the frame. The shortest maximum is 217 lines = 1.97 ms, in the 320x200 mode.
    pub const fn exposure_regs(&self, lines: u16) -> [(u16, u8); 3] {
        let max = self.max_exposure_lines();
        let l = if lines < EXPOSURE_MIN_LINES {
            EXPOSURE_MIN_LINES
        } else if lines > max {
            max
        } else {
            lines
        };
        exposure_regs(l)
    }
}

/// Exposure in lines (one line = [`Mode::line_ns`]), not clamped. Valid range
/// [`EXPOSURE_MIN_LINES`]..=[`Mode::max_exposure_lines`], which depends on the mode: 890 lines at
/// VTS 915 down to 217 at VTS 242. [`Mode::exposure_regs`] clamps to it.
/// Writes EXPO {0x3500[3:0], 0x3501, 0x3502}, whose low 4 bits are a fraction of a line and are
/// left at 0. Takes effect one frame later (0x3503 bits 0 and 1 = 0).
pub const fn exposure_regs(lines: u16) -> [(u16, u8); 3] {
    [
        (0x3500, (lines >> 12) as u8 & 0x0F),
        (0x3501, (lines >> 4) as u8),
        (0x3502, (lines << 4) as u8),
    ]
}

/// Gain in real gain format (0x3503 bit 2 = 0): 0x10 = 1x, 1/16 steps, so 0x28 = 2.5x.
/// Clamped to [`GAIN_MIN`]..=[`GAIN_MAX`] (1x to 15.5x). Writes GAIN (0x3509); takes effect one
/// frame later (0x3503 bits 4 to 6 = 0).
pub const fn gain_regs(gain: u8) -> [(u16, u8); 1] {
    let g = if gain < GAIN_MIN {
        GAIN_MIN
    } else if gain > GAIN_MAX {
        GAIN_MAX
    } else {
        gain
    };
    [(0x3509, g)]
}

const DEFAULT_EXPOSURE: [(u16, u8); 3] = exposure_regs(DEFAULT_EXPOSURE_LINES);

/// Reset, PLL, 8-bit output, analog and ISP settings shared by every mode. Espressif
/// `esp_cam_sensor` table `MIPI_2lane_24Minput_RAW8_640x400_100fps` without its mode-specific
/// registers; changed: delay after reset, 0x4003 (BLC target for no ISP), 0x4800 (continuous
/// clock), default exposure and gain.
const COMMON: &[(u16, u8)] = &[
    SOFTWARE_RESET,
    (DELAY, RESET_DELAY_MS),
    (0x0106, 0x00), // fast standby off: a frame completes before standby
    // PLL (datasheet table 2-10 defaults, rewritten): PLL1 mult 50 -> MIPI 800 Mbit/s per lane,
    // PLL2 mult 80 -> SCLK 80 MHz (datasheet maximum). Linux uses 0x030D = 0x60 for RAW8 (96 MHz).
    (0x0302, 0x32),
    (0x030D, 0x50),
    (0x030E, 0x02),
    (0x3001, 0x00),
    (0x3004, 0x00),
    (0x3005, 0x00),
    (0x3006, 0x04), // pad outputs: ILPWM only; STROBE (bit 3) off
    (0x3011, 0x0A),
    (0x3013, 0x18),
    (0x301C, 0xF0),
    (0x3022, 0x01),
    (0x3030, 0x10), // SC_LP_CTRL4 default: FSIN wake-up (bit 2) off, free-running
    (0x3039, 0x32),
    (0x303A, 0x00),
    DEFAULT_EXPOSURE[0],
    DEFAULT_EXPOSURE[1],
    DEFAULT_EXPOSURE[2],
    (0x3503, 0x08), // gain_prec16_en, real gain format, exposure/gain delayed one frame
    (0x3505, 0x8C),
    (0x3507, 0x03),
    (0x3508, 0x00),
    (0x3509, DEFAULT_GAIN),
    (0x3610, 0x80),
    (0x3611, 0xA0),
    (0x3620, 0x6E),
    (0x3632, 0x56),
    (0x3633, 0x78),
    (0x3662, 0x07), // ANA_CORE_2: RAW8 (0x05 is RAW10)
    (0x3666, 0x00),
    (0x366F, 0x5A),
    (0x3680, 0x84),
    (0x3707, 0x56),
    (0x370D, 0x00),
    (0x370E, 0xFA),
    (0x3712, 0x80),
    (0x372D, 0x22),
    (0x3731, 0x80),
    (0x3732, 0x30),
    (0x377D, 0x22),
    (0x3788, 0x02),
    (0x3789, 0xA4),
    (0x378A, 0x00),
    (0x378B, 0x4A),
    (0x3799, 0x20),
    (0x379C, 0x01),
    (0x382B, 0x3A),
    (0x382C, 0x06),
    (0x382D, 0xC2),
    (0x389D, 0x00),
    (0x3881, 0x42),
    (0x3882, 0x02),
    (0x3883, 0x12),
    (0x3885, 0x07),
    (0x38A8, 0x02),
    (0x38A9, 0x80),
    (0x38B1, 0x03),
    (0x38B3, 0x07),
    (0x38C4, 0x00),
    (0x38C5, 0xC0),
    (0x38C6, 0x04),
    (0x38C7, 0x80),
    (0x3920, 0xFF),
    (0x4003, 0x10), // BLC target 16 (10-bit) = 4 (8-bit); Espressif uses 0x40 with its ISP
    (0x400C, 0x01),
    (0x4010, 0xF0),
    (0x4011, 0x3B),
    (0x4042, 0x01),
    (0x4043, 0x40),
    (0x4307, 0x30),
    (0x4317, 0x00),
    (0x4501, 0x00),
    (0x450A, 0x08),
    (0x4601, 0x30), // VFIFO read start (Espressif's RAW8 value; 0x04 in the RAW10 tables)
    (0x470F, 0x00),
    (0x4F07, 0x00),
    MIPI_CLOCK_CONTINUOUS,
    (0x4837, 0x14), // MIPI pclk period, 1 fraction bit: 10 ns = byte clock at 800 Mbit/s
    (0x5000, 0x9F),
    (0x5001, 0x00),
    TEST_PATTERN_OFF,
    (0x5D00, 0x07),
    (0x5D01, 0x00),
    (0x4F00, 0x0C),
    (0x4F10, 0x00),
    (0x4F11, 0x88),
    (0x4F12, 0x0F),
    (0x4F13, 0xC4),
    // Line and frame length, the same in every mode.
    (0x380C, (HTS >> 8) as u8),
    (0x380D, HTS as u8),
    (0x380E, (VTS_120FPS >> 8) as u8),
    (0x380F, VTS_120FPS as u8),
];

// Pixel array: 1296 x 816 (x 0..=1295, y 0..=815), the 1280 x 800 active area starting at (8, 8).
// 0x3800-0x3807: array window read out; 0x3808-0x380B: output size; 0x3810-0x3813: ISP window
// offset inside the read-out window; 0x3814/0x3815: skip increments; 0x3820/0x3821: binning.

/// 1280x800 full frame (Raspberry Pi / Linux / Espressif full-resolution window).
const MODE_1280X800: &[(u16, u8)] = &[
    (0x3778, 0x00),
    (0x3800, 0x00), // x start 0
    (0x3801, 0x00),
    (0x3802, 0x00), // y start 0
    (0x3803, 0x00),
    (0x3804, 0x05), // x end 1295
    (0x3805, 0x0F),
    (0x3806, 0x03), // y end 815
    (0x3807, 0x2F),
    (0x3808, 0x05), // width 1280
    (0x3809, 0x00),
    (0x380A, 0x03), // height 800
    (0x380B, 0x20),
    (0x3810, 0x00), // ISP x offset 8
    (0x3811, 0x08),
    (0x3812, 0x00), // ISP y offset 8
    (0x3813, 0x08),
    (0x3814, 0x11),
    (0x3815, 0x11),
    (0x3820, 0x40),
    (0x3821, 0x00),
    (0x4008, 0x04),
    (0x4009, 0x0B),
    (0x400D, 0x07),
    (0x4507, 0x00),
    (0x4509, 0x00),
];

/// 1280x720 centred: active rows 40..=759 (Raspberry Pi / Espressif 1280x720 window).
const MODE_1280X720: &[(u16, u8)] = &[
    (0x3778, 0x00),
    (0x3800, 0x00), // x start 0
    (0x3801, 0x00),
    (0x3802, 0x00), // y start 40
    (0x3803, 0x28),
    (0x3804, 0x05), // x end 1295
    (0x3805, 0x0F),
    (0x3806, 0x03), // y end 775 = 40 + 720 + 16 - 1
    (0x3807, 0x07),
    (0x3808, 0x05), // width 1280
    (0x3809, 0x00),
    (0x380A, 0x02), // height 720
    (0x380B, 0xD0),
    (0x3810, 0x00), // ISP x offset 8
    (0x3811, 0x08),
    (0x3812, 0x00), // ISP y offset 8
    (0x3813, 0x08),
    (0x3814, 0x11),
    (0x3815, 0x11),
    (0x3820, 0x40),
    (0x3821, 0x00),
    (0x4008, 0x04),
    (0x4009, 0x0B),
    (0x400D, 0x07),
    (0x4507, 0x00),
    (0x4509, 0x00),
];

/// 640x480 centred, no binning: active columns 320..=959, rows 160..=639. Derived: the
/// 1280x720 crop method (array window = output + 16, ISP offset 8) applied on both axes.
const MODE_640X480: &[(u16, u8)] = &[
    (0x3778, 0x00),
    (0x3800, 0x01), // x start 320
    (0x3801, 0x40),
    (0x3802, 0x00), // y start 160
    (0x3803, 0xA0),
    (0x3804, 0x03), // x end 975 = 320 + 640 + 16 - 1
    (0x3805, 0xCF),
    (0x3806, 0x02), // y end 655 = 160 + 480 + 16 - 1
    (0x3807, 0x8F),
    (0x3808, 0x02), // width 640
    (0x3809, 0x80),
    (0x380A, 0x01), // height 480
    (0x380B, 0xE0),
    (0x3810, 0x00), // ISP x offset 8
    (0x3811, 0x08),
    (0x3812, 0x00), // ISP y offset 8
    (0x3813, 0x08),
    (0x3814, 0x11),
    (0x3815, 0x11),
    (0x3820, 0x40),
    (0x3821, 0x00),
    (0x4008, 0x04),
    (0x4009, 0x0B),
    (0x400D, 0x07),
    (0x4507, 0x00),
    (0x4509, 0x00),
];

/// 640x400 2x2 binned, whole array (identical in the Linux, Raspberry Pi and Espressif drivers).
const MODE_640X400: &[(u16, u8)] = &[
    (0x3778, 0x10),
    (0x3800, 0x00), // x start 0
    (0x3801, 0x00),
    (0x3802, 0x00), // y start 0
    (0x3803, 0x00),
    (0x3804, 0x05), // x end 1295
    (0x3805, 0x0F),
    (0x3806, 0x03), // y end 815
    (0x3807, 0x2F),
    (0x3808, 0x02), // width 640
    (0x3809, 0x80),
    (0x380A, 0x01), // height 400
    (0x380B, 0x90),
    (0x3810, 0x00), // ISP x offset 4 (binned pixels)
    (0x3811, 0x04),
    (0x3812, 0x00), // ISP y offset 4
    (0x3813, 0x04),
    (0x3814, 0x31), // x odd/even increment 3/1
    (0x3815, 0x22), // y odd/even increment 2/2
    (0x3820, 0x60),
    (0x3821, 0x01), // horizontal binning
    (0x4008, 0x02),
    (0x4009, 0x05),
    (0x400D, 0x03),
    (0x4507, 0x03),
    (0x4509, 0x80),
];

const fn concat<const N: usize>(a: &[(u16, u8)], b: &[(u16, u8)]) -> [(u16, u8); N] {
    assert!(a.len() + b.len() == N);
    let mut out = [(0u16, 0u8); N];
    let mut i = 0;
    while i < a.len() {
        out[i] = a[i];
        i += 1;
    }
    let mut j = 0;
    while j < b.len() {
        out[i + j] = b[j];
        j += 1;
    }
    out
}

const REGS_1280X800: [(u16, u8); COMMON.len() + MODE_1280X800.len()] =
    concat(COMMON, MODE_1280X800);
const REGS_1280X720: [(u16, u8); COMMON.len() + MODE_1280X720.len()] =
    concat(COMMON, MODE_1280X720);
const REGS_640X480: [(u16, u8); COMMON.len() + MODE_640X480.len()] = concat(COMMON, MODE_640X480);
const REGS_640X400: [(u16, u8); COMMON.len() + MODE_640X400.len()] = concat(COMMON, MODE_640X400);

/// 320x200: centred crop of the 2x2-binned readout, i.e. the centre 640x400 sensor pixels
/// (active columns 320..=959, rows 200..=599), half the field of view. Derived: the 640x400
/// binned block with its array window cut like the 640x480 crop (array window = 2 x (output +
/// 2 x ISP offset 4) = output in sensor pixels + 16). Reading only 416 array rows (208 after the
/// row skip) is what lets VTS drop to 241. Two common-block values are reset to the datasheet
/// defaults (also the Linux `ov9282.c` values) in [`REGS_320X200_RAW8_453`], both measured on
/// the board:
/// - VFIFO read start 0x4601 = 0x04. It counts 8-byte units and must not exceed the line: at
///   320 B per line 0x28 runs, 0x29 and the common 0x30 (384 B) deliver no frame (one CSI-2
///   frame start/end pairing error per frame; the frame's last 2560 B stay in the CSI bridge).
/// - PSV_CTRL 0x4F00 = 0x00. With the common 0x0C (power-saving mode on) the frames arrive but
///   at VTS 241 rows 0..=187 and at VTS 260 every row are flat at the black level.
const MODE_320X200: &[(u16, u8)] = &[
    (0x3778, 0x10),
    (0x3800, 0x01), // x start 320
    (0x3801, 0x40),
    (0x3802, 0x00), // y start 200
    (0x3803, 0xC8),
    (0x3804, 0x03), // x end 975 = 320 + 640 + 16 - 1
    (0x3805, 0xCF),
    (0x3806, 0x02), // y end 615 = 200 + 400 + 16 - 1
    (0x3807, 0x67),
    (0x3808, 0x01), // width 320
    (0x3809, 0x40),
    (0x380A, 0x00), // height 200
    (0x380B, 0xC8),
    (0x3810, 0x00), // ISP x offset 4 (binned pixels)
    (0x3811, 0x04),
    (0x3812, 0x00), // ISP y offset 4
    (0x3813, 0x04),
    (0x3814, 0x31),
    (0x3815, 0x22),
    (0x3820, 0x60),
    (0x3821, 0x01),
    (0x4008, 0x02),
    (0x4009, 0x05),
    (0x400D, 0x03),
    (0x4507, 0x03),
    (0x4509, 0x80),
];

/// PLL2 multiplier 96: SCLK 96 MHz (VCO 768 MHz), above the datasheet's 80 MHz. Linux and
/// Raspberry Pi RAW8 value.
const SCLK_96MHZ: (u16, u8) = (0x030D, 0x60);
/// ANA_CORE_2 RAW10 (Linux, Raspberry Pi).
const ANA_RAW10: (u16, u8) = (0x3662, 0x05);
/// VFIFO read start of the RAW10 tables (Linux, Raspberry Pi; Espressif's original before its
/// RAW8 change to 0x30).
const VFIFO_RAW10: (u16, u8) = (0x4601, 0x04);

/// VTS for 640x400 binned at 210 fps, 80 MHz: 80e6 / (522 x 728 + 1730) = 209.56 fps (minimum
/// 422).
const VTS_210FPS: u16 = 523;
/// VTS for 640x400 binned at 253 fps, 80 MHz: 80e6 / (433 x 728 + 1730) = 252.40 fps.
const VTS_253FPS: u16 = 434;
/// VTS for 320x200, 80 MHz: 80e6 / (728 x 241) = 455.98 fps by HTS x VTS; measured 453.40 fps
/// ((VTS - 1) x HTS + 1730 SCLK cycles per frame), the rate closest to Inno-Maker's 453.
/// Measured limits at the exposure maximum (VTS - 25): 232, 234, 236, 238, 240 and 241 give
/// whole, correct pictures; 222 and 230 whole frames with a nearly black picture (230 also at
/// 100 lines); 214, 215, 216, 218 and 220 stop the frames (CSI-2 frame start/end errors;
/// measured with 0x4F00 still at 0x0C).
const VTS_453FPS: u16 = 241;

const fn vts_regs(vts: u16) -> [(u16, u8); 2] {
    [(0x380E, (vts >> 8) as u8), (0x380F, vts as u8)]
}

const EXP_96MHZ: [(u16, u8); 3] = exposure_regs(DEFAULT_EXPOSURE_LINES_96MHZ);
const VTS_REGS_210: [(u16, u8); 2] = vts_regs(VTS_210FPS);
const VTS_REGS_253: [(u16, u8); 2] = vts_regs(VTS_253FPS);
const VTS_REGS_453: [(u16, u8); 2] = vts_regs(VTS_453FPS);

/// `COMMON` + `mode`, then each pair of `over` replaces the value of the one entry with its
/// address (the build fails if there is none or more than one). Keeps the write order.
const fn build<const N: usize>(mode: &[(u16, u8)], over: &[(u16, u8)]) -> [(u16, u8); N] {
    let mut out: [(u16, u8); N] = concat(COMMON, mode);
    let mut k = 0;
    while k < over.len() {
        let mut hits = 0;
        let mut i = 0;
        while i < N {
            if out[i].0 == over[k].0 {
                out[i].1 = over[k].1;
                hits += 1;
            }
            i += 1;
        }
        assert!(hits == 1);
        k += 1;
    }
    out
}

const N_1280X800: usize = COMMON.len() + MODE_1280X800.len();
const N_1280X720: usize = COMMON.len() + MODE_1280X720.len();
const N_640X400: usize = COMMON.len() + MODE_640X400.len();
const N_320X200: usize = COMMON.len() + MODE_320X200.len();

const REGS_1280X800_RAW8_144: [(u16, u8); N_1280X800] = build(
    MODE_1280X800,
    &[SCLK_96MHZ, EXP_96MHZ[0], EXP_96MHZ[1], EXP_96MHZ[2]],
);
/// 1280x720 at 144 fps: the 1280x720 window at 96 MHz and VTS 915, as the 1280x800 144 fps
/// mode (80 MHz needs VTS >= 814, at most 134.8 fps).
const REGS_1280X720_RAW8_144: [(u16, u8); N_1280X720] = build(
    MODE_1280X720,
    &[SCLK_96MHZ, EXP_96MHZ[0], EXP_96MHZ[1], EXP_96MHZ[2]],
);
const REGS_640X400_RAW8_210: [(u16, u8); N_640X400] =
    build(MODE_640X400, &[VTS_REGS_210[0], VTS_REGS_210[1]]);
const REGS_640X400_RAW8_253: [(u16, u8); N_640X400] =
    build(MODE_640X400, &[VTS_REGS_253[0], VTS_REGS_253[1]]);
const REGS_320X200_RAW8_453: [(u16, u8); N_320X200] = build(
    MODE_320X200,
    &[
        VTS_REGS_453[0],
        VTS_REGS_453[1],
        (0x4601, 0x04), // VFIFO read start: see `MODE_320X200`
        (0x4F00, 0x00), // PSV_CTRL: power-saving mode off, see `MODE_320X200`
    ],
);
const REGS_1280X800_RAW10: [(u16, u8); N_1280X800] =
    build(MODE_1280X800, &[ANA_RAW10, VFIFO_RAW10]);
const REGS_1280X720_RAW10: [(u16, u8); N_1280X720] =
    build(MODE_1280X720, &[ANA_RAW10, VFIFO_RAW10]);
const REGS_640X400_RAW10_210: [(u16, u8); N_640X400] = build(
    MODE_640X400,
    &[ANA_RAW10, VFIFO_RAW10, VTS_REGS_210[0], VTS_REGS_210[1]],
);

/// A RAW8 mode at SCLK 80 MHz, HTS 728 and the default 1 ms exposure.
const fn raw8(
    name: &'static str,
    width: u16,
    height: u16,
    fps: u16,
    vts: u16,
    regs: &'static [(u16, u8)],
) -> Mode {
    Mode {
        name,
        width,
        height,
        fps,
        lane_mbps: LANE_MBPS,
        hts: HTS,
        vts,
        regs,
        bits_per_pixel: 8,
        data_type: DT_RAW8,
        sclk_mhz: 80,
        default_exposure_lines: DEFAULT_EXPOSURE_LINES,
    }
}

/// A RAW10 mode at SCLK 80 MHz, HTS 728 and the default 1 ms exposure.
const fn raw10(
    name: &'static str,
    width: u16,
    height: u16,
    fps: u16,
    vts: u16,
    regs: &'static [(u16, u8)],
) -> Mode {
    Mode {
        bits_per_pixel: 10,
        data_type: DT_RAW10,
        ..raw8(name, width, height, fps, vts, regs)
    }
}

/// Every mode, all at HTS 728 and 800 Mbit/s per lane. 0..=3: the 120 fps RAW8 modes under test
/// (VTS 915, 9.1 us per line, 119.918 fps). 4..=11: the Inno-Maker mode list.
pub const MODES: [Mode; 12] = [
    raw8("1280x800", 1280, 800, 120, VTS_120FPS, &REGS_1280X800),
    raw8("1280x720 crop", 1280, 720, 120, VTS_120FPS, &REGS_1280X720),
    raw8("640x480 crop", 640, 480, 120, VTS_120FPS, &REGS_640X480),
    raw8("640x400 bin2", 640, 400, 120, VTS_120FPS, &REGS_640X400),
    Mode {
        sclk_mhz: 96,
        default_exposure_lines: DEFAULT_EXPOSURE_LINES_96MHZ,
        ..raw8(
            "1280x800 RAW8 144fps 96MHz",
            1280,
            800,
            144,
            VTS_120FPS,
            &REGS_1280X800_RAW8_144,
        )
    },
    Mode {
        sclk_mhz: 96,
        default_exposure_lines: DEFAULT_EXPOSURE_LINES_96MHZ,
        ..raw8(
            "1280x720 crop RAW8 144fps 96MHz",
            1280,
            720,
            144,
            VTS_120FPS,
            &REGS_1280X720_RAW8_144,
        )
    },
    raw8(
        "640x400 bin2 RAW8 210fps",
        640,
        400,
        210,
        VTS_210FPS,
        &REGS_640X400_RAW8_210,
    ),
    raw8(
        "640x400 bin2 RAW8 253fps",
        640,
        400,
        253,
        VTS_253FPS,
        &REGS_640X400_RAW8_253,
    ),
    raw8(
        "320x200 bin2 crop RAW8 455fps",
        320,
        200,
        455,
        VTS_453FPS,
        &REGS_320X200_RAW8_453,
    ),
    raw10(
        "1280x800 RAW10",
        1280,
        800,
        120,
        VTS_120FPS,
        &REGS_1280X800_RAW10,
    ),
    raw10(
        "1280x720 crop RAW10",
        1280,
        720,
        120,
        VTS_120FPS,
        &REGS_1280X720_RAW10,
    ),
    raw10(
        "640x400 bin2 RAW10 210fps",
        640,
        400,
        210,
        VTS_210FPS,
        &REGS_640X400_RAW10_210,
    ),
];
