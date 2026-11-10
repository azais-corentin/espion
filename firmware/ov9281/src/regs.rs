//! Register writes of a mode, generated from one sequence shared by every mode.
//!
//! SCCB: 16-bit register address, 8-bit value. The sequence is the common block (reset, PLL,
//! analog and ISP settings, line and frame length), then the window block (array window,
//! output size, ISP offset, skip increments, binning). Fixed values come from these sources:
//! - the common block is Espressif's `esp_cam_sensor` OV9281 table
//!   `MIPI_2lane_24Minput_RAW8_640x400_100fps` (RAW8, 2 lanes, 24 MHz, 800 Mbit/s) without its
//!   mode-specific registers, changed in: the delay after reset, 0x4003 (black-level target
//!   for no ISP), 0x4800 (continuous clock lane), default exposure and gain;
//! - the windows follow the Linux `ov9282.c` and Raspberry Pi `ov9281.c` drivers (the
//!   1280x800 full frame, the 1280x720 crop and the 640x400 binned mode, identical in the
//!   Linux, Raspberry Pi and Espressif drivers), which also give the RAW10 and 96 MHz values;
//! - the crop and binned-crop windows at any origin, HTS and VTS, and the 8-bit choices are
//!   derived from the OV9281 datasheet v1.1.
//!
//! The values that depend on the mode are marked `Var` below. The PSV and VFIFO values follow
//! the measured rules in [`crate::limits`] ([`limits::psv_on`], [`limits::vfifo_read_start`]),
//! so they differ from the fixed values of the source tables: 0x4601 is `width / 16` (the
//! tables had 0x30 or 0x04), and 0x4F00 is 0x0C only from crop `h + 95` / binned `h + 75` up.

use crate::{Format, Mode, Readout, limits};

/// One step of a mode's register list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Write {
    /// Write `value` to register `address`.
    Reg(u16, u8),
    /// Wait this many milliseconds before the next step.
    DelayMs(u8),
}

/// 7-bit SCCB address with SID low (8-bit write address 0xC0, datasheet v1.1).
pub(crate) const ADDRESS_PRIMARY: u8 = 0x60;
/// 7-bit SCCB address with SID high (8-bit write address 0x20).
pub(crate) const ADDRESS_ALTERNATE: u8 = 0x10;
/// Chip ID high byte register (reads 0x92); 0x300B holds the low byte (0x81).
pub(crate) const CHIP_ID_REG: u16 = 0x300A;
/// Expected `{0x300A, 0x300B}`.
pub const CHIP_ID: u16 = 0x9281;
/// SC_MODE_SELECT: 1 streaming, 0 software standby (the current frame completes first,
/// 0x0106 = 0).
pub(crate) const MODE_SELECT: u16 = 0x0100;
/// SC_SOFTWARE_RESET: resets every register to its default.
const SOFTWARE_RESET: u16 = 0x0103;
/// Wait after the software reset (Espressif's driver waits 5 ms; the datasheet gives no figure).
const RESET_DELAY_MS: u8 = 5;
/// EXPO {0x3500[3:0], 0x3501, 0x3502}: exposure in 1/16 lines, written with the fraction 0.
/// Takes effect one frame later (0x3503 bits 0 and 1 = 0).
pub(crate) const EXPOSURE_REG: u16 = 0x3500;
/// GAIN (0x3509), real gain format (0x3503 bit 2 = 0): 0x10 = 1x, 1/16 steps. Takes effect one
/// frame later (0x3503 bits 4 to 6 = 0).
pub(crate) const GAIN_REG: u16 = 0x3509;
/// PRE_CTRL00: test pattern select (bit 7 enable).
pub(crate) const TEST_PATTERN_REG: u16 = 0x5E00;
/// PRE_CTRL01: bit 4 "same seed enable" restarts the random pattern every frame.
pub(crate) const TEST_PATTERN_SEED_REG: u16 = 0x5E01;

/// EXPO register values for `lines` of exposure (fraction 0).
pub(crate) const fn exposure_bytes(lines: u16) -> [u8; 3] {
    [
        (lines >> 12) as u8 & 0x0F,
        (lines >> 4) as u8,
        (lines << 4) as u8,
    ]
}

/// A mode-dependent value.
#[derive(Clone, Copy)]
enum Var {
    /// PLL2 multiplier (0x030D).
    Sclk,
    /// Byte `n` of the default exposure ([`exposure_bytes`]).
    Exposure(u8),
    /// ANA_CORE_2 (0x3662): 0x07 RAW8 (Espressif), 0x05 RAW10 (Linux, Raspberry Pi).
    Analog,
    /// VFIFO read start (0x4601): `max(1, width / 16)`, [`limits::vfifo_read_start`].
    Vfifo,
    /// PSV_CTRL (0x4F00): 0x0C power saving on (Espressif's value), 0x00 off (datasheet
    /// default), by [`limits::psv_on`].
    Psv,
    /// High byte of a field.
    Hi(Field),
    /// Low byte of a field.
    Lo(Field),
    /// One value for the crop readout, another for the binned one.
    ByReadout { crop: u8, binned: u8 },
}

/// A 16-bit value written as two registers.
#[derive(Clone, Copy)]
enum Field {
    /// HTS, SCLK cycles per line (Linux counts it as 2 x this, in pixels).
    Hts,
    /// VTS, lines per frame.
    Vts,
    /// First array column read: the window origin.
    XStart,
    /// First array row read.
    YStart,
    /// Last array column read: origin + window width in sensor pixels + 16 - 1.
    XEnd,
    /// Last array row read.
    YEnd,
    /// Output width.
    Width,
    /// Output height.
    Height,
}

#[derive(Clone, Copy)]
enum Step {
    Reg(u16, u8),
    Var(u16, Var),
    DelayMs(u8),
}

use Field::*;
use Step::{DelayMs, Reg, Var as V};
use Var::{ByReadout, Hi, Lo};

/// Every mode's register list, in write order.
const SEQUENCE: [Step; 120] = [
    // Common block.
    Reg(SOFTWARE_RESET, 0x01),
    DelayMs(RESET_DELAY_MS),
    Reg(0x0106, 0x00), // fast standby off: a frame completes before standby
    // PLL (datasheet table 2-10 defaults, rewritten): PLL1 multiplier 50 -> MIPI 800 Mbit/s
    // per lane; PLL2 multiplier -> SCLK.
    Reg(0x0302, 0x32),
    V(0x030D, Var::Sclk),
    Reg(0x030E, 0x02),
    Reg(0x3001, 0x00),
    Reg(0x3004, 0x00),
    Reg(0x3005, 0x00),
    Reg(0x3006, 0x04), // pad outputs: ILPWM only; STROBE (bit 3) off
    Reg(0x3011, 0x0A),
    Reg(0x3013, 0x18),
    Reg(0x301C, 0xF0),
    Reg(0x3022, 0x01),
    Reg(0x3030, 0x10), // SC_LP_CTRL4 default: FSIN wake-up (bit 2) off, free-running
    Reg(0x3039, 0x32),
    Reg(0x303A, 0x00),
    V(EXPOSURE_REG, Var::Exposure(0)),
    V(EXPOSURE_REG + 1, Var::Exposure(1)),
    V(EXPOSURE_REG + 2, Var::Exposure(2)),
    Reg(0x3503, 0x08), // gain_prec16_en, real gain format, exposure/gain delayed one frame
    Reg(0x3505, 0x8C),
    Reg(0x3507, 0x03),
    Reg(0x3508, 0x00),
    Reg(GAIN_REG, limits::GAIN_MIN), // 1x
    Reg(0x3610, 0x80),
    Reg(0x3611, 0xA0),
    Reg(0x3620, 0x6E),
    Reg(0x3632, 0x56),
    Reg(0x3633, 0x78),
    V(0x3662, Var::Analog),
    Reg(0x3666, 0x00),
    Reg(0x366F, 0x5A),
    Reg(0x3680, 0x84),
    Reg(0x3707, 0x56),
    Reg(0x370D, 0x00),
    Reg(0x370E, 0xFA),
    Reg(0x3712, 0x80),
    Reg(0x372D, 0x22),
    Reg(0x3731, 0x80),
    Reg(0x3732, 0x30),
    Reg(0x377D, 0x22),
    Reg(0x3788, 0x02),
    Reg(0x3789, 0xA4),
    Reg(0x378A, 0x00),
    Reg(0x378B, 0x4A),
    Reg(0x3799, 0x20),
    Reg(0x379C, 0x01),
    Reg(0x382B, 0x3A),
    Reg(0x382C, 0x06),
    Reg(0x382D, 0xC2),
    Reg(0x389D, 0x00),
    Reg(0x3881, 0x42),
    Reg(0x3882, 0x02),
    Reg(0x3883, 0x12),
    Reg(0x3885, 0x07),
    Reg(0x38A8, 0x02),
    Reg(0x38A9, 0x80),
    Reg(0x38B1, 0x03),
    Reg(0x38B3, 0x07),
    Reg(0x38C4, 0x00),
    Reg(0x38C5, 0xC0),
    Reg(0x38C6, 0x04),
    Reg(0x38C7, 0x80),
    Reg(0x3920, 0xFF),
    Reg(0x4003, 0x10), // BLC target 16 (10-bit) = 4 (8-bit); Espressif uses 0x40 with its ISP
    Reg(0x400C, 0x01),
    Reg(0x4010, 0xF0),
    Reg(0x4011, 0x3B),
    Reg(0x4042, 0x01),
    Reg(0x4043, 0x40),
    Reg(0x4307, 0x30),
    Reg(0x4317, 0x00),
    Reg(0x4501, 0x00),
    Reg(0x450A, 0x08),
    V(0x4601, Var::Vfifo),
    Reg(0x470F, 0x00),
    Reg(0x4F07, 0x00),
    Reg(0x4800, 0x00), // MIPI_CTRL00: clock lane free-running, no line short packets
    Reg(0x4837, 0x14), // MIPI pclk period, 1 fraction bit: 10 ns = byte clock at 800 Mbit/s
    Reg(0x5000, 0x9F),
    Reg(0x5001, 0x00),
    Reg(TEST_PATTERN_REG, 0x00), // test pattern off
    Reg(0x5D00, 0x07),
    Reg(0x5D01, 0x00),
    V(0x4F00, Var::Psv),
    Reg(0x4F10, 0x00),
    Reg(0x4F11, 0x88),
    Reg(0x4F12, 0x0F),
    Reg(0x4F13, 0xC4),
    V(0x380C, Hi(Hts)),
    V(0x380D, Lo(Hts)),
    V(0x380E, Hi(Vts)),
    V(0x380F, Lo(Vts)),
    // Window block. 0x3800-0x3807: array window read out; 0x3808-0x380B: output size;
    // 0x3810-0x3813: ISP window offset inside the read-out window (8 sensor pixels);
    // 0x3814/0x3815: odd/even skip increments; 0x3820/0x3821: binning.
    V(
        0x3778,
        ByReadout {
            crop: 0x00,
            binned: 0x10,
        },
    ),
    V(0x3800, Hi(XStart)),
    V(0x3801, Lo(XStart)),
    V(0x3802, Hi(YStart)),
    V(0x3803, Lo(YStart)),
    V(0x3804, Hi(XEnd)),
    V(0x3805, Lo(XEnd)),
    V(0x3806, Hi(YEnd)),
    V(0x3807, Lo(YEnd)),
    V(0x3808, Hi(Width)),
    V(0x3809, Lo(Width)),
    V(0x380A, Hi(Height)),
    V(0x380B, Lo(Height)),
    Reg(0x3810, 0x00),
    V(
        0x3811,
        ByReadout {
            crop: 0x08,
            binned: 0x04,
        },
    ), // ISP x offset, output pixels
    Reg(0x3812, 0x00),
    V(
        0x3813,
        ByReadout {
            crop: 0x08,
            binned: 0x04,
        },
    ), // ISP y offset
    V(
        0x3814,
        ByReadout {
            crop: 0x11,
            binned: 0x31,
        },
    ), // x odd/even increment
    V(
        0x3815,
        ByReadout {
            crop: 0x11,
            binned: 0x22,
        },
    ), // y odd/even increment
    V(
        0x3820,
        ByReadout {
            crop: 0x40,
            binned: 0x60,
        },
    ),
    V(
        0x3821,
        ByReadout {
            crop: 0x00,
            binned: 0x01,
        },
    ), // horizontal binning
    V(
        0x4008,
        ByReadout {
            crop: 0x04,
            binned: 0x02,
        },
    ),
    V(
        0x4009,
        ByReadout {
            crop: 0x0B,
            binned: 0x05,
        },
    ),
    V(
        0x400D,
        ByReadout {
            crop: 0x07,
            binned: 0x03,
        },
    ),
    V(
        0x4507,
        ByReadout {
            crop: 0x00,
            binned: 0x03,
        },
    ),
    V(
        0x4509,
        ByReadout {
            crop: 0x00,
            binned: 0x80,
        },
    ),
];

impl Mode {
    const fn field(&self, f: Field) -> u16 {
        let (x, y) = self.origin();
        let s = limits::scale(self.readout());
        match f {
            Hts => self.hts(),
            Vts => self.vts(),
            XStart => x,
            YStart => y,
            XEnd => x + self.width() * s + limits::WINDOW_MARGIN - 1,
            YEnd => y + self.height() * s + limits::WINDOW_MARGIN - 1,
            Width => self.width(),
            Height => self.height(),
        }
    }

    const fn value(&self, v: Var) -> u8 {
        match v {
            Var::Sclk => self.sensor_clock().pll2_multiplier(),
            Var::Exposure(n) => exposure_bytes(self.default_exposure_lines())[n as usize],
            Var::Analog => match self.format() {
                Format::Raw8 => 0x07,
                Format::Raw10 => 0x05,
            },
            Var::Vfifo => limits::vfifo_read_start(self.width()),
            Var::Psv => {
                if self.psv() {
                    0x0C
                } else {
                    0x00
                }
            }
            Hi(f) => (self.field(f) >> 8) as u8,
            Lo(f) => self.field(f) as u8,
            ByReadout { crop, binned } => match self.readout() {
                Readout::Crop => crop,
                Readout::Binned => binned,
            },
        }
    }
}

/// Iterator over a mode's register writes, from [`Mode::registers`]. The list starts with a
/// software reset and leaves the sensor in software standby, with the default exposure
/// ([`Mode::default_exposure_lines`]), gain 1x and the test pattern off.
#[derive(Debug, Clone)]
pub struct Registers<'a> {
    mode: &'a Mode,
    next: usize,
}

impl<'a> Registers<'a> {
    pub(crate) const fn new(mode: &'a Mode) -> Self {
        Self { mode, next: 0 }
    }
}

impl Iterator for Registers<'_> {
    type Item = Write;

    fn next(&mut self) -> Option<Write> {
        let step = *SEQUENCE.get(self.next)?;
        self.next += 1;
        Some(match step {
            Reg(a, v) => Write::Reg(a, v),
            V(a, v) => Write::Reg(a, self.mode.value(v)),
            DelayMs(ms) => Write::DelayMs(ms),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = SEQUENCE.len() - self.next;
        (n, Some(n))
    }
}

impl ExactSizeIterator for Registers<'_> {}
impl core::iter::FusedIterator for Registers<'_> {}
