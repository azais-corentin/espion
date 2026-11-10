//! Every limit the mode solver applies, each a named constant or a small function.
//!
//! The rules come from a board characterisation of one sensor at room temperature on
//! 2026-11-10: a Waveshare ESP32-P4-ETH dev board with an OV9281 module, two MIPI lanes at
//! 800 Mbit/s, 24 MHz XVCLK, the sensor free-running. A setting passed when its frames arrived
//! whole, on time and without receiver errors **and** a scene picture matched the reference of
//! the same window (no flat rows, block-mean correlation >= 0.95, brightness within
//! 0.8..1.25). A wrong VTS, PSV, VFIFO or HTS setting never showed in the link counters or in
//! the test pattern, only in a scene picture. The measurements, condensed, are in the crate's
//! `README.md` ("Measured limits"); each rule below gives its boundary and the range tested.
//! Every "measured" below means: on that one sensor, at room temperature, on 2026-11-10; no
//! temperature or part-to-part spread was measured.
//!
//! h, w below are the output height and width. Every timing rule is in lines and was the same
//! at 80 and 96 MHz and in RAW8 and RAW10. What was not measured is kept conservative and says
//! so. Each rule is one constant or function here; the solver and the register generator only
//! call these.

use crate::{Format, Readout, SensorClock};

// Geometry. Pixel array: 1296 x 816 (x 0..=1295, y 0..=815), the 1280 x 800 active area
// starting at (8, 8) (datasheet v1.1).

/// Width of the active area in sensor pixels. Measured: windows reaching past it deliver
/// frames (the sensor does not refuse them) with flat rows (crop 640x480 at y 400: 72 flat
/// rows) or content that was not checked, so the solver refuses them ([`crate::Error::Window`]).
pub const ACTIVE_WIDTH: u16 = 1280;
/// Height of the active area in sensor pixels. See [`ACTIVE_WIDTH`].
pub const ACTIVE_HEIGHT: u16 = 800;
/// Array columns (and rows) read beyond the output window, in sensor pixels: the array window
/// (0x3800..=0x3807) is the output area plus 16, and the ISP offset (0x3810..=0x3813) skips
/// the first 8 (8 output pixels unbinned, 4 binned). This is the full-frame window of the
/// Linux `ov9282.c`, Raspberry Pi `ov9281.c` and Espressif `esp_cam_sensor` drivers; every
/// window of the characterisation was built this way and its still found at the expected
/// place in the full frame.
pub const WINDOW_MARGIN: u16 = 16;

/// Sensor pixels per output pixel on each axis: 1 cropped, 2 binned (2x2).
pub const fn scale(readout: Readout) -> u16 {
    match readout {
        Readout::Crop => 1,
        Readout::Binned => 2,
    }
}

/// Largest output size `(width, height)` of a readout: the active area, halved when binned.
pub const fn max_size(readout: Readout) -> (u16, u16) {
    let s = scale(readout);
    (ACTIVE_WIDTH / s, ACTIVE_HEIGHT / s)
}

/// Array rows read out per frame for an output `height`: the window in sensor rows plus
/// [`WINDOW_MARGIN`].
pub const fn rows_read(readout: Readout, height: u16) -> u16 {
    height * scale(readout) + WINDOW_MARGIN
}

/// Narrowest output in pixels, crop and binned. Measured (RAW8, h 200 crop, h 100 binned):
/// 120 and 136 gave the right picture at the right place; 112, 100, 96 and 80 deliver whole
/// frames with a wrong, dark picture (and an exact test pattern); 48 and narrower deliver no
/// frames. Conservative: 128, the first width above the tested good 120 that is a multiple of
/// both [`width_multiple`]s; 113..=119 were not tested.
pub const MIN_WIDTH: u16 = 128;

/// Lowest output height, crop and binned. Measured: heights 2..=800 crop and 2..=400 binned
/// have the right picture at [`min_vts`]; height 1 delivered a right picture at a long VTS
/// (640x1 crop at VTS 915) but its minimum VTS for a picture was never measured, so it is
/// refused.
pub const MIN_HEIGHT: u16 = 2;

/// The output width must be a multiple of this.
/// - RAW8: 8. Every width of 0 mod 8 from 120 to 1280 tested gave the right picture; of 12
///   widths of 4 mod 8, 8 delivered no frames at all (1268, 1260, 1020, 1004, 996, 644, 636,
///   324) and no rule separated the 4 that worked.
/// - RAW10: 64. The ESP32-P4 CSI receiver needs a multiple of 64 in RAW10 (`esp32p4-csi`,
///   confirmed: 1248 is refused with `InvalidGeometry`), so no other RAW10 width could be
///   verified; 1280, 1216, 1152, 640, 576 and 320 were.
pub const fn width_multiple(format: Format) -> u8 {
    match format {
        Format::Raw8 => 8,
        Format::Raw10 => 64,
    }
}

/// The output height must be a multiple of this. RAW8: 1, odd heights deliver the right
/// picture at the right place (1280x799, 1280x401, 640x3, binned 640x101). RAW10: 2, the
/// ESP32-P4 CSI receiver needs an even RAW10 height (640x201 refused with `InvalidGeometry`).
pub const fn height_multiple(format: Format) -> u8 {
    match format {
        Format::Raw8 => 1,
        Format::Raw10 => 2,
    }
}

/// The window's x origin (sensor pixels, crop and binned) must be a multiple of this.
/// Measured: the sensor truncates the window x start to a multiple of 8 (asked x 1, 2, 4, 6 ->
/// found 0; 12 -> 8; 20 -> 16; 140 -> 136; 101 -> 96), so other values are refused rather than
/// silently moved.
pub const ORIGIN_X_MULTIPLE: u16 = 8;

/// The window's y origin (sensor pixels) must be a multiple of this: 1 cropped (y is exact:
/// asked 1, 5, 37, 161, 501 -> found the same row), 2 binned (an odd y is floored to the even
/// row: asked 1 and 3 -> found 0 and 2 sensor rows).
pub const fn origin_y_multiple(readout: Readout) -> u16 {
    match readout {
        Readout::Crop => 1,
        Readout::Binned => 2,
    }
}

// Line and frame timing. A frame lasts `(VTS - 1) * HTS + FRAME_LONG_LINE` SCLK cycles
// (`crate::frame_cycles`).

/// SCLK cycles of the one line per frame that does not last HTS: a frame lasts
/// `(VTS - 1) * HTS + FRAME_LONG_LINE` cycles. Measured: 1493 probes over HTS 659..=65535 and
/// VTS 21..=65535, at 80 and 96 MHz, RAW8 and RAW10, crop and binned, all within -23.5..-33.9
/// ppm of the formula, which is the sensor's clock offset against the ESP32-P4's timer (the
/// same offset at every setting). Exposure has no effect. That one line lasts 1730 cycles is
/// an inference: only the whole frame period was measured. The datasheet does not give it.
pub const FRAME_LONG_LINE: u32 = 1730;

/// Smallest HTS the sensor itself is given. Measured, scene picture against HTS 728: 723 when
/// the array window is 1296 columns wide (crop w 1280, binned w 640; 722 has flat rows and a
/// 10x too bright picture), 671 for array widths 80..=976 (670 too bright; 671 already reads
/// 11..16 % brighter than 728), 677 at array width 80. Conservative: 728 (the Linux `ov9282.c`
/// line length), giving up up to 8 % of the rate in narrow windows.
pub const HTS_SENSOR_MIN: u16 = 728;

/// Largest HTS the solver may use. Measured: the period formula holds up to HTS 65535 and the
/// picture matches up to 8000, but brightens with long lines at the same exposure (1.04x at
/// 2912, 1.09x at 8000, failing from about 16000). 2912 (4 x 728) is the range in which the
/// picture, the formula and [`min_vts`] were all tested.
pub const HTS_MAX: u16 = 2912;

/// Data lanes.
pub const LANES: u8 = 2;
/// Bit rate per data lane in Mbit/s: PLL1 24 MHz / 1.5 x 50 = 800 (0x0302 = 0x32), the same
/// in every mode.
pub const LANE_MBPS: u16 = 800;
/// Image bytes the link carries per microsecond: [`LANES`] x [`LANE_MBPS`] / 8 = 200.
pub const LINK_BYTES_PER_US: u32 = LANES as u32 * LANE_MBPS as u32 / 8;

/// SCLK cycles a line needs beyond its data time on the link. Measured (RAW10 at 96 MHz,
/// h 200, scene picture): lines of 1600, 1520, 1440 and 1280 bytes (768, 730, 691 and 614
/// cycles of data) need HTS 843, 803, 765 and 688, i.e. data + 73..75 cycles (0.77 us per line;
/// `[INFERENCE]` the D-PHY's HS entry and exit). Just below it the frames are whole, on time
/// and error-free but the picture is broken; further below no frames arrive (1600 B at HTS
/// 787 and below).
pub const LINK_LINE_OVERHEAD: u32 = 75;

/// Cycles added to the measured link minimum ([`link_min_hts`]) for margin.
pub const HTS_LINK_MARGIN: u16 = 8;

/// Smallest HTS whose line carries `line_bytes` over the link at `clock`, as measured:
/// `ceil(line_bytes x sclk_mhz / 200) + 75` ([`LINK_BYTES_PER_US`], [`LINK_LINE_OVERHEAD`]).
pub const fn link_min_hts(line_bytes: u32, clock: SensorClock) -> u32 {
    (line_bytes * clock.mhz() as u32).div_ceil(LINK_BYTES_PER_US) + LINK_LINE_OVERHEAD
}

/// Smallest HTS the solver may use for a line of `line_bytes` at `clock`:
/// `max(`[`HTS_SENSOR_MIN`]`, `[`link_min_hts`]` + `[`HTS_LINK_MARGIN`]`)`. The link term
/// binds only for RAW10 wider than 1075 pixels at 96 MHz (1280: 851); RAW8 and every 80 MHz
/// line stay at 728.
pub const fn hts_min(line_bytes: u32, clock: SensorClock) -> u16 {
    let link = link_min_hts(line_bytes, clock) + HTS_LINK_MARGIN as u32;
    if link > HTS_SENSOR_MIN as u32 {
        link as u16
    } else {
        HTS_SENSOR_MIN
    }
}

/// Lines added to every measured VTS boundary ([`min_vts`], [`psv_min_vts`]).
pub const VTS_MARGIN: u16 = 4;

/// The measured smallest VTS with a right picture, PSV off: crop `h + 51`, binned `h + 31`.
/// Below it the frames stay whole, on time and error-free but the picture is dark and inverted
/// (mean below the black level, correlation about -0.4, some all-zero rows); further below
/// (crop h + 31..33, binned h + 18..22) no frames arrive. Measured last good / first bad at
/// every height tested (crop 2..=800, binned 2..=400), at widths crop 320..=1280 and binned
/// 160..=640, at the top, centre and bottom of the array, at several exposures, at 80 and
/// 96 MHz, in RAW8 and RAW10. It binds at HTS 728..=1000 (crop) and 728..=850 (binned); at
/// longer lines only the lower frame limit was found, so the rule is safe over the whole HTS
/// range 728..=2912.
pub const fn measured_min_vts(readout: Readout, height: u16) -> u16 {
    match readout {
        Readout::Crop => height + 51,
        Readout::Binned => height + 31,
    }
}

/// Smallest VTS the solver uses for an output `height`: [`measured_min_vts`] plus
/// [`VTS_MARGIN`], the same at both clocks, in both formats and at every HTS in range.
pub const fn min_vts(readout: Readout, height: u16) -> u16 {
    measured_min_vts(readout, height) + VTS_MARGIN
}

/// Largest VTS: the register width (0x380E, 0x380F). Measured: right pictures and the period
/// formula at VTS 32767, 32768, 54946 and 65535 (HTS 728, 80 MHz).
pub const VTS_MAX: u16 = 0xFFFF;

/// The measured smallest VTS from which the power-saving mode (PSV_CTRL 0x4F00 = 0x0C) leaves
/// the picture alone: crop `h + 91`, binned `h + 71` (the edge was h + 90 in some windows and
/// boots, h + 91 passed in all). Below it, sweeping VTS - h up from the PSV-off minimum, PSV
/// on is not monotonic: crop 51..54 right, 55..57 **no frames**, 58 right, 59..90 rows flat at
/// the black level, then right; binned 31..34 right, 35..37 no frames, 38 right, 39..70 flat.
/// The narrow good windows sit next to the no-frame lines, so no "on" window below this is
/// used. The bands are in lines and the same at HTS 728, 1000, 1456 and 2912, at both clocks,
/// in both formats, at every width, position and exposure tested. Where PSV is safe it changes
/// nothing measurable (picture, black level, noise, rate); power was not measured.
pub const fn measured_psv_min_vts(readout: Readout, height: u16) -> u16 {
    match readout {
        Readout::Crop => height + 91,
        Readout::Binned => height + 71,
    }
}

/// Smallest VTS with PSV on: [`measured_psv_min_vts`] plus [`VTS_MARGIN`] (crop h + 95,
/// binned h + 75).
pub const fn psv_min_vts(readout: Readout, height: u16) -> u16 {
    measured_psv_min_vts(readout, height) + VTS_MARGIN
}

/// Whether the driver turns PSV on: from [`psv_min_vts`] up, off below.
pub const fn psv_on(readout: Readout, height: u16, vts: u16) -> bool {
    vts >= psv_min_vts(readout, height)
}

// Exposure and gain.

/// Exposure must stay this many lines below VTS. Measured: VTS - 26 and VTS - 13 gave a right
/// picture at an unchanged period; an exposure longer than VTS gives black frames (the frame
/// is never stretched). VTS - 13..VTS was not tested; 25 is the Linux, Raspberry Pi and
/// Espressif drivers' margin.
pub const EXPOSURE_MARGIN: u16 = 25;
/// Shortest exposure in lines (datasheet: 1 row period).
pub const EXPOSURE_MIN: u16 = 1;
/// Lowest gain, 1x (real gain format, 4 fraction bits).
pub const GAIN_MIN: u8 = 0x10;
/// Highest gain, 15.5x: the largest the Raspberry Pi and Espressif drivers use.
pub const GAIN_MAX: u8 = 0xF8;

// Output FIFO.

/// VFIFO read start (0x4601) for an output `width`: `max(1, w / 16)`. The register counts
/// 8 **pixels** (RAW10 1280 wide: largest good 160, not 1600 B / 8). Measured with the bar
/// pattern (rows must repeat exactly): good `low..=w / 8` with `low` = `ceil(w / 40) - 4`
/// (1280: 28, 640: 12, 320: 4, 128: 1) in RAW8 at 80 MHz and 1 in every other format and
/// clock; too low shifts or smears lines, too high delivers no frames. No fixed value serves
/// every width (Espressif's 0x30 gives no frames below 384 pixels, 0x04 breaks RAW8 at 80 MHz
/// above about 320). `w / 16` lies inside every measured range and was used by every probe of
/// the characterisation (widths 64..=1280, both clocks and formats, HTS 659..=65535).
pub const fn vfifo_read_start(width: u16) -> u8 {
    let v = width / 16;
    if v < 1 { 1 } else { v as u8 }
}
