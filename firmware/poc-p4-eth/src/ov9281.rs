//! OV9281 register settings for the PoC: four free-running modes at 120 fps, RAW8, two MIPI
//! CSI-2 data lanes at 800 Mbit/s each, 24 MHz external clock (XVCLK). `core` only.
//!
//! Sources and derivations are in the PoC notes (`poc-ov9281.md`). In short: the common block
//! is Espressif's `esp_cam_sensor` OV9281 table (RAW8, 2 lanes, 24 MHz, 800 Mbit/s), the windows
//! come from the Linux `ov9282.c` and Raspberry Pi `ov9281.c` drivers, and HTS/VTS for 120 fps,
//! the 640x480 window and the 8-bit register choices are derived from the OV9281 datasheet v1.1.
//!
//! Sequence for the caller (SCCB: 16-bit register address, 8-bit value):
//! 1. Read [`CHIP_ID_HIGH`] and [`CHIP_ID_LOW`] and compare with [`CHIP_ID`].
//! 2. Write every pair of [`Mode::regs`] in order. A pair whose address is [`DELAY`] is not a
//!    register: wait its value in milliseconds instead. The list starts with a software reset,
//!    leaves the sensor in software standby, and sets exposure [`DEFAULT_EXPOSURE_LINES`] and
//!    gain [`DEFAULT_GAIN`].
//! 3. Optionally write [`exposure_regs`], [`gain_regs`], [`TEST_PATTERN_ON`] or
//!    [`MIPI_CLOCK_GATED`] (the last only in standby).
//! 4. Start the CSI receiver, then write [`STREAM_ON`]. The datasheet allows up to 10 ms from
//!    streaming to the first frame.
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

/// System clock (PLL2 sys clk) in Hz: 24 MHz / 3 x 80 / 4 / 2. HTS counts these cycles.
pub const SCLK_HZ: u32 = 80_000_000;
/// MIPI bit rate per data lane in Mbit/s: 24 MHz / 1.5 x 50 / 1 (PLL1).
pub const LANE_MBPS: u16 = 800;

/// HTS register value (SCLK cycles per line): 728 / 80 MHz = 9.1 us per line.
pub const HTS: u16 = 728;
/// VTS register value (lines per frame) for 120 fps: 80 MHz / (728 x 915) = 120.10 fps.
pub const VTS_120FPS: u16 = 915;

/// Exposure must stay this many lines below VTS (Linux, Raspberry Pi and Espressif drivers;
/// datasheet v1.1 says 12).
pub const EXPOSURE_MAX_OFFSET: u16 = 25;
/// Shortest exposure in lines (datasheet: 1 row period).
pub const EXPOSURE_MIN_LINES: u16 = 1;
/// Default exposure: 110 lines x 9.1 us = 1.001 ms.
pub const DEFAULT_EXPOSURE_LINES: u16 = 110;

/// Lowest gain, 1x (real gain format, 4 fraction bits).
pub const GAIN_MIN: u8 = 0x10;
/// Highest gain used by the Raspberry Pi and Espressif drivers, 15.5x.
pub const GAIN_MAX: u8 = 0xF8;
/// Default gain, 1x.
pub const DEFAULT_GAIN: u8 = GAIN_MIN;

/// One sensor mode.
pub struct Mode {
    pub name: &'static str,
    /// Output width in pixels (RAW8: bytes per line).
    pub width: u16,
    /// Output height in lines.
    pub height: u16,
    /// Nominal frame rate; the exact one is [`Mode::frame_rate_mhz`].
    pub fps: u16,
    /// MIPI bit rate per data lane, for the receiver's D-PHY setup.
    pub lane_mbps: u16,
    /// HTS register value, in SCLK cycles per line (Linux counts it as 2 x this, in pixels).
    pub hts: u16,
    /// VTS register value, in lines per frame.
    pub vts: u16,
    /// Full register list from software reset to standby, ready to stream.
    pub regs: &'static [(u16, u8)],
}

impl Mode {
    /// Line period in nanoseconds.
    pub const fn line_ns(&self) -> u32 {
        (self.hts as u64 * 1_000_000_000 / SCLK_HZ as u64) as u32
    }

    /// Frame rate in millihertz.
    pub const fn frame_rate_mhz(&self) -> u32 {
        (SCLK_HZ as u64 * 1000 / (self.hts as u64 * self.vts as u64)) as u32
    }

    /// Longest exposure in lines for this mode's VTS.
    pub const fn max_exposure_lines(&self) -> u16 {
        self.vts - EXPOSURE_MAX_OFFSET
    }
}

/// Exposure in lines (one line = [`Mode::line_ns`]). Valid range
/// [`EXPOSURE_MIN_LINES`]..=[`Mode::max_exposure_lines`] (890 lines = 8.1 ms at VTS 915).
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

/// The four modes under test, all at HTS 728, VTS 915: 9.1 us per line, 120.10 fps.
pub const MODES: [Mode; 4] = [
    Mode {
        name: "1280x800",
        width: 1280,
        height: 800,
        fps: 120,
        lane_mbps: LANE_MBPS,
        hts: HTS,
        vts: VTS_120FPS,
        regs: &REGS_1280X800,
    },
    Mode {
        name: "1280x720 crop",
        width: 1280,
        height: 720,
        fps: 120,
        lane_mbps: LANE_MBPS,
        hts: HTS,
        vts: VTS_120FPS,
        regs: &REGS_1280X720,
    },
    Mode {
        name: "640x480 crop",
        width: 640,
        height: 480,
        fps: 120,
        lane_mbps: LANE_MBPS,
        hts: HTS,
        vts: VTS_120FPS,
        regs: &REGS_640X480,
    },
    Mode {
        name: "640x400 bin2",
        width: 640,
        height: 400,
        fps: 120,
        lane_mbps: LANE_MBPS,
        hts: HTS,
        vts: VTS_120FPS,
        regs: &REGS_640X400,
    },
];
