//! The checks of a `SetCamera`, in the protocol's order (`docs/protocol.md`, "Camera stream"):
//! the sensor mode (the `ov9281` builder: size, alignment, window, rate), the receiver's
//! geometry, the stream's encoding against the format, then exposure and gain. The same rules
//! as the viewer's `poc_viewer::camera` (its fake camera is the reference), on the wire types
//! of `espion_proto::cam`.

use espion_proto::cam::{
    CamError, CameraSettings, Encoding, Format, ModeInfo, Origin, Readout, TestPattern,
};
use ov9281::{Gain, Mode, Rate, Tolerance, limits};

use crate::camera::Controls;

/// The driver's format.
pub const fn format(f: Format) -> ov9281::Format {
    match f {
        Format::Raw8 => ov9281::Format::Raw8,
        Format::Raw10 => ov9281::Format::Raw10,
    }
}

/// The driver's readout.
const fn readout(r: Readout) -> ov9281::Readout {
    match r {
        Readout::Crop => ov9281::Readout::Crop,
        Readout::Binned => ov9281::Readout::Binned,
    }
}

/// The driver's test pattern.
const fn test_pattern(p: TestPattern) -> ov9281::TestPattern {
    match p {
        TestPattern::Off => ov9281::TestPattern::Off,
        TestPattern::Bars => ov9281::TestPattern::Bars,
        TestPattern::RollingBars => ov9281::TestPattern::RollingBars,
        TestPattern::GreySquares => ov9281::TestPattern::GreySquares,
        TestPattern::BlackWhiteSquares => ov9281::TestPattern::BlackWhiteSquares,
        TestPattern::Random => ov9281::TestPattern::Random,
    }
}

/// The wire mirror of a builder error.
const fn from_mode_error(e: ov9281::Error) -> CamError {
    match e {
        ov9281::Error::Size {
            max_width,
            max_height,
        } => CamError::Size {
            max_width,
            max_height,
        },
        ov9281::Error::Alignment {
            width_multiple,
            height_multiple,
        } => CamError::Alignment {
            width_multiple,
            height_multiple,
        },
        ov9281::Error::Window => CamError::Window,
        ov9281::Error::TooFast { max } => CamError::TooFast {
            max_mhz: max.as_millihertz(),
        },
        ov9281::Error::TooSlow { min } => CamError::TooSlow {
            min_mhz: min.as_millihertz(),
        },
        ov9281::Error::Tolerance { nearest } => CamError::Tolerance {
            nearest_mhz: nearest.as_millihertz(),
        },
    }
}

/// The receiver's geometry rules (`esp32p4_csi::Config::validate`, which is private): 12-bit
/// sizes, whole 64-bit DMA items per frame, whole 32-bit words per line, and for RAW10 a word
/// count per line that is a multiple of 4 and an even height.
fn receiver_accepts(f: Format, width: u16, height: u16) -> bool {
    let bits = match f {
        Format::Raw8 => 8,
        Format::Raw10 => 10,
    };
    let line_bits = width as u32 * bits;
    let frame_bytes = line_bits / 8 * height as u32;
    let words = line_bits.div_ceil(32);
    (1..=4095).contains(&width)
        && (1..=4095).contains(&height)
        && frame_bytes.is_multiple_of(8)
        && line_bits.is_multiple_of(32)
        && (f == Format::Raw8 || (words.is_multiple_of(4) && height.is_multiple_of(2)))
}

/// The sensor mode of the settings' mode part, then the receiver's check.
fn build_mode(c: &CameraSettings) -> Result<Mode, CamError> {
    let b = Mode::builder()
        .format(format(c.format))
        .size(c.width, c.height)
        .readout(readout(c.readout));
    let b = match c.origin {
        Some(o) => b.origin(o.x, o.y),
        None => b,
    };
    let mode = b
        .frame_rate(
            Rate::millihertz(c.rate_mhz),
            Tolerance::millihertz(c.tolerance_mhz),
        )
        .build()
        .map_err(from_mode_error)?;
    if !receiver_accepts(c.format, c.width, c.height) {
        return Err(CamError::Receiver);
    }
    Ok(mode)
}

/// The encoding can be produced from the sensor format: RAW8 from either, RAW10 only from
/// RAW10.
pub fn check_encoding(f: Format, e: Encoding) -> Result<(), CamError> {
    match (f, e) {
        (Format::Raw8, Encoding::Raw10) => Err(CamError::Encoding {
            format: f,
            encoding: e,
        }),
        _ => Ok(()),
    }
}

/// Exposure and gain against the mode (the driver's `set_exposure` and `set_gain` ranges).
fn check_controls(c: &CameraSettings, mode: &Mode) -> Result<(), CamError> {
    let max_lines = mode.max_exposure_lines();
    if !(limits::EXPOSURE_MIN..=max_lines).contains(&c.exposure_lines) {
        return Err(CamError::Exposure { max_lines });
    }
    if !(limits::GAIN_MIN..=limits::GAIN_MAX).contains(&c.gain_sixteenths) {
        return Err(CamError::Gain {
            min: limits::GAIN_MIN,
            max: limits::GAIN_MAX,
        });
    }
    Ok(())
}

/// Every check of a `SetCamera`, in order; the mode if all pass.
pub fn validate(c: &CameraSettings, encoding: Encoding) -> Result<Mode, CamError> {
    let mode = build_mode(c)?;
    check_encoding(c.format, encoding)?;
    check_controls(c, &mode)?;
    Ok(mode)
}

/// The settings as applied: the origin the mode resolved.
pub fn applied(c: &CameraSettings, mode: &Mode) -> CameraSettings {
    let (x, y) = mode.origin();
    CameraSettings {
        origin: Some(Origin { x, y }),
        ..*c
    }
}

/// The controls of the settings, for the driver.
pub const fn controls(c: &CameraSettings) -> Controls {
    Controls {
        exposure_lines: c.exposure_lines,
        gain: Gain::from_sixteenths(c.gain_sixteenths),
        pattern: test_pattern(c.test_pattern),
    }
}

/// What the driver chose, as the wire carries it.
pub fn mode_info(m: &Mode) -> ModeInfo {
    ModeInfo {
        sensor_clock_mhz: m.sensor_clock().mhz(),
        exceeds_datasheet_clock: m.exceeds_datasheet_clock(),
        hts: m.hts(),
        vts: m.vts(),
        psv: m.psv(),
        frame_rate_mhz: m.frame_rate().as_millihertz(),
        rate_error_mhz: m.rate_error(),
        max_exposure_lines: m.max_exposure_lines(),
        line_ns: m.line_ns(),
        frame_bytes: m.frame_bytes(),
    }
}

/// The frame layout: what [`espion_proto::cam::State::generation`] follows on the camera side.
pub fn layout(c: &CameraSettings) -> (Format, u16, u16, Readout, Option<Origin>) {
    (c.format, c.width, c.height, c.readout, c.origin)
}
