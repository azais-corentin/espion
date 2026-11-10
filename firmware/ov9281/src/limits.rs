//! Every limit the mode solver applies, each with its evidence and status.
//!
//! The bench is a Waveshare ESP32-P4-ETH dev board with an OV9281 module: two MIPI lanes at
//! 800 Mbit/s, 24 MHz XVCLK, the sensor free-running (`firmware/poc-p4-eth/README.md` has the
//! set-up, method and results; runs of 2026-11-10). "Right picture" below means a scene still
//! matching the full-frame reference of the same boot; a wrong VTS or PSV setting never showed
//! in the link counters or in the test pattern, only in a scene picture.
//!
//! Values marked **PROVISIONAL** rest on a few measured points (conservative between them) or
//! on an inference, and are expected to change with a board characterisation. Each rule is
//! one constant or function here, so changing it is local; the solver and the register
//! generator only call these.

use crate::{Format, Readout, SensorClock};

// Geometry. Pixel array: 1296 x 816 (x 0..=1295, y 0..=815), the 1280 x 800 active area
// starting at (8, 8) (datasheet v1.1).

/// Width of the active area in sensor pixels.
pub const ACTIVE_WIDTH: u16 = 1280;
/// Height of the active area in sensor pixels.
pub const ACTIVE_HEIGHT: u16 = 800;
/// Array columns (and rows) read beyond the output window, in sensor pixels: the array window
/// (0x3800..=0x3807) is the output area plus 16, and the ISP offset (0x3810..=0x3813) skips
/// the first 8 (8 output pixels unbinned, 4 binned). This is the full-frame window of the
/// Linux `ov9282.c`, Raspberry Pi `ov9281.c` and Espressif `esp_cam_sensor` drivers; the
/// cropped 1280x720, 640x480 and 320x200 windows built the same way gave right pictures at
/// the expected place in the array.
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

/// Array rows per sensor line (per HTS period): 2 when binned (the vertical skip 0x3815 = 0x22
/// reads two rows per line).
pub const fn rows_per_line(readout: Readout) -> u16 {
    scale(readout)
}

/// Narrowest output in pixels. **PROVISIONAL**: the VFIFO read start of 4 units
/// ([`vfifo_read_start`]) needs a line of at least 32 B. The narrowest width tested is 320.
pub const MIN_WIDTH: u16 = 32;

/// The output width must be a multiple of this. RAW10 packs 4 pixels in 5 bytes (CSI-2), so
/// 4. RAW8 4 as well, **PROVISIONAL**: a whole number of 32-bit words per line is what the
/// ESP32-P4 receiver needs; every width tested (320, 640, 1280) is a multiple of 64. The
/// receiver driver has stricter rules of its own (`esp32p4-csi`: RAW10 a multiple of 64 and
/// an even height) and checks them itself.
pub const fn width_multiple(format: Format) -> u8 {
    match format {
        Format::Raw8 | Format::Raw10 => 4,
    }
}

/// Window origin coordinates (sensor pixels) must be a multiple of this. **PROVISIONAL**:
/// every origin tested (x 0, 320; y 0, 40, 160, 200) is a multiple of 8; an odd origin would
/// shift the 2x2 binning phase (an inference) and was never tried.
pub const ORIGIN_MULTIPLE: u16 = 2;

// Line and frame timing. A frame lasts `(VTS - 1) * HTS + FRAME_LONG_LINE` SCLK cycles
// (`crate::frame_cycles`).

/// SCLK cycles of the one line per frame that does not last HTS: a frame lasts
/// `(VTS - 1) * HTS + FRAME_LONG_LINE` cycles, not `VTS * HTS`. Measured: 21 runs of 10 s over
/// HTS 728..=2912, VTS 434..=1830, SCLK 80 and 96 MHz, RAW8 and RAW10, cropped and binned,
/// exposure 100..=800 lines fit this within 1.4 cycles (fit 1730.2), with the sensor clock
/// 28 ppm fast against the ESP32-P4's timer; the 320x200 mode at VTS 241 measured 5 cycles
/// under it. Exposure has no effect. That one line lasts 1730 cycles is an inference: only
/// the whole frame period was measured. The datasheet does not give it.
pub const FRAME_LONG_LINE: u32 = 1730;

/// HTS the solver tries first at each clock, and the one every measured picture used (Linux
/// `ov9282.c` 0x2D8; 9.1 us per line at 80 MHz).
pub const HTS_PREFERRED: u16 = 728;
/// Smallest HTS the solver may use. **PROVISIONAL**: fixed at [`HTS_PREFERRED`]; the frame
/// period formula holds over 728..=2912 (measured), but no picture was checked at another
/// HTS and a shorter line was never tried.
pub const HTS_MIN: u16 = 728;
/// Largest HTS the solver may use. **PROVISIONAL**: see [`HTS_MIN`]. The solver searches the
/// whole `HTS_MIN..=HTS_MAX` range when [`HTS_PREFERRED`] cannot meet a request.
pub const HTS_MAX: u16 = 728;

const _: () = assert!(HTS_MIN <= HTS_PREFERRED && HTS_PREFERRED <= HTS_MAX);

/// Largest VTS: the register width (0x380E, 0x380F). **PROVISIONAL**: the frame period formula
/// is fitted up to VTS 1830 only.
pub const VTS_MAX: u16 = 0xFFFF;

/// Lines a frame needs beyond the lines read out ([`rows_read`] / [`rows_per_line`]) for a
/// right picture, power-saving mode (PSV) off. Below it the frames stay whole and on time but
/// the picture is dark or nearly black, and PSV off does not help. **PROVISIONAL**:
/// - cropped, 80 MHz: 44. 1280x720 window (736 rows read): VTS 763 (+27) dark, inverted-looking
///   at every exposure; VTS 780 (+44) and 800 right. 764..=779 untested.
/// - binned, 80 MHz: 24. 320x200 window (416 rows, 208 lines): VTS 222 (+14) and 230 (+22)
///   nearly black; 232 (+24), 234, 236, 238, 240 and 241 right. 231 untested.
/// - 96 MHz: nothing below VTS 915 was measured ([`vts_floor`] keeps 96 MHz frames there).
///   The 80 MHz margins scaled by 96/80 and rounded up (53 and 29), which is conservative if
///   the margin is a time (an inference).
pub const fn vts_margin(readout: Readout, clock: SensorClock) -> u16 {
    let lines = match readout {
        Readout::Crop => 44,
        Readout::Binned => 24,
    };
    scale_80_to(clock, lines)
}

/// Smallest VTS whatever the window. **PROVISIONAL**:
/// - 80 MHz: 232, the shortest frame that gave a right picture (320x200 binned, 208 lines
///   read); frames stopped altogether at VTS 214..=220 in that window (PSV on). No smaller
///   window was measured.
/// - 96 MHz: 915, the only VTS measured at 96 MHz (1280x800 and 1280x720 right at gain 2x).
///   A wrong VTS shows only in the scene picture, so the solver does not go below it.
pub const fn vts_floor(clock: SensorClock) -> u16 {
    match clock {
        SensorClock::Mhz80 => 232,
        SensorClock::Mhz96 => 915,
    }
}

/// Smallest VTS for a window reading `rows_read` array rows. With `psv` (PSV on), also at
/// least [`psv_min_vts`]; `None` when PSV must not be on at that readout and clock.
pub const fn min_vts(
    readout: Readout,
    rows_read: u16,
    clock: SensorClock,
    psv: bool,
) -> Option<u16> {
    let lines = rows_read.div_ceil(rows_per_line(readout));
    let picture = lines.saturating_add(vts_margin(readout, clock));
    let floor = vts_floor(clock);
    let off = if picture > floor { picture } else { floor };
    if !psv {
        return Some(off);
    }
    match psv_min_vts(readout, clock) {
        Some(p) if p > off => Some(p),
        Some(_) => Some(off),
        None => None,
    }
}

/// Smallest VTS at which the power-saving mode (PSV_CTRL 0x4F00 = 0x0C, from Espressif's
/// table; datasheet default 0x00, Linux never writes it) is known to leave the picture alone;
/// `None` where it was never measured. With PSV on and a shorter frame the frames arrive but
/// rows are flat at the black level; with PSV off the same settings give whole pictures, and
/// PSV off made no visible difference in picture, black level, noise or rate where PSV on
/// works. **PROVISIONAL**, the lowest measured good values:
/// - cropped, 80 MHz: 814. 1280x720: VTS 780..=810 flat (717 of 720 rows at 780), 814..=820
///   right; 1280x800, 1280x720 and 640x480 right at 915.
/// - cropped, 96 MHz: 915, the only VTS measured (1280x800 and 1280x720 right at gain 2x).
/// - binned, 80 MHz: 434. 640x400 (whole array) right at 434, 523 and 915; the 320x200
///   window flat at 241 (188 of 200 rows) and 260 (all rows).
/// - binned, 96 MHz: never measured.
///
/// The bad points sit below the whole array's lines (816, binned 408) and the good ones at or
/// above it, so the threshold may follow the array rather than the window (an inference).
pub const fn psv_min_vts(readout: Readout, clock: SensorClock) -> Option<u16> {
    match (readout, clock) {
        (Readout::Crop, SensorClock::Mhz80) => Some(814),
        (Readout::Crop, SensorClock::Mhz96) => Some(915),
        (Readout::Binned, SensorClock::Mhz80) => Some(434),
        (Readout::Binned, SensorClock::Mhz96) => None,
    }
}

/// Whether the driver turns PSV on: where it is known to be safe ([`psv_min_vts`]), off
/// elsewhere.
pub const fn psv_on(readout: Readout, rows_read: u16, clock: SensorClock, vts: u16) -> bool {
    match min_vts(readout, rows_read, clock, true) {
        Some(min) => vts >= min,
        None => false,
    }
}

/// `lines` at 80 MHz scaled to `clock`, rounded up.
const fn scale_80_to(clock: SensorClock, lines: u16) -> u16 {
    let mhz = clock.mhz() as u32;
    (lines as u32 * mhz).div_ceil(80) as u16
}

// Exposure and gain.

/// Exposure must stay this many lines below VTS (Linux, Raspberry Pi and Espressif drivers;
/// datasheet v1.1 says 12).
pub const EXPOSURE_MARGIN: u16 = 25;
/// Shortest exposure in lines (datasheet: 1 row period).
pub const EXPOSURE_MIN: u16 = 1;
/// Lowest gain, 1x (real gain format, 4 fraction bits).
pub const GAIN_MIN: u8 = 0x10;
/// Highest gain, 15.5x: the largest the Raspberry Pi and Espressif drivers use.
pub const GAIN_MAX: u8 = 0xF8;

// MIPI CSI-2 link.

/// Data lanes.
pub const LANES: u8 = 2;
/// Bit rate per data lane in Mbit/s: PLL1 24 MHz / 1.5 x 50 = 800 (0x0302 = 0x32), the same
/// in every mode.
pub const LANE_MBPS: u16 = 800;
/// Line time the link needs beyond the image data (packet header and footer, HS entry and
/// exit). **PROVISIONAL**: the fullest lines that ran are 1600 B (8.0 us of data) in a
/// 9.1 us line (RAW10 1280 wide, HTS 728 at 80 MHz) and 1280 B (6.4 us) in 7.58 us (RAW8
/// 1280 wide at 96 MHz); 1.1 us is what the first leaves. The real overhead is not measured.
pub const LINK_LINE_OVERHEAD_NS: u32 = 1100;

/// Whether a line of `line_bytes` fits the link in one line period (`hts` cycles of `clock`):
/// data time at `LANES x LANE_MBPS` plus [`LINK_LINE_OVERHEAD_NS`] within the line.
pub const fn link_fits(line_bytes: u32, hts: u16, clock: SensorClock) -> bool {
    let data_ns = line_bytes as u64 * 8000 / (LANES as u64 * LANE_MBPS as u64);
    // line_ns = hts * 1000 / mhz, compared without rounding.
    (data_ns + LINK_LINE_OVERHEAD_NS as u64) * clock.mhz() as u64 <= hts as u64 * 1000
}

// Output FIFO.

/// VFIFO read start (0x4601), in 8-byte units. It must not exceed the line: at 320 B per line
/// 0x04, 0x20 and 0x28 run, 0x29, 0x2F and Espressif's RAW8 0x30 (384 B) deliver no frame
/// (one frame start/end pairing error per frame). RAW8 keeps Espressif's 0x30 where the line
/// holds it (verified at 640 and 1280 wide), otherwise the datasheet default 0x04 (also Linux
/// `ov9282.c`; verified at 320 wide). RAW10 uses 0x04 (Linux, Raspberry Pi; verified at 640
/// and 1280 wide).
pub const fn vfifo_read_start(format: Format, line_bytes: u32) -> u8 {
    const ESPRESSIF_RAW8: u8 = 0x30;
    const DEFAULT: u8 = 0x04;
    match format {
        Format::Raw8 if line_bytes >= ESPRESSIF_RAW8 as u32 * 8 => ESPRESSIF_RAW8,
        Format::Raw8 | Format::Raw10 => DEFAULT,
    }
}
