//! Camera settings checked with the `ov9281` driver, in the order the board checks them.
//!
//! The fake camera applies [`validate`] to every `SetCamera`; the viewer runs the same checks
//! as the user edits, so it shows the driver's verdict before anything is sent. The board's
//! answer stays authoritative.

use espion_proto::cam::{CamError, CameraSettings, Encoding, Format, ModeInfo, Origin, Readout};
use ov9281::{Mode, Rate, SensorClock, Tolerance, limits};

/// The driver's format.
pub const fn format(f: Format) -> ov9281::Format {
    match f {
        Format::Raw8 => ov9281::Format::Raw8,
        Format::Raw10 => ov9281::Format::Raw10,
    }
}

/// The driver's readout.
pub const fn readout(r: Readout) -> ov9281::Readout {
    match r {
        Readout::Crop => ov9281::Readout::Crop,
        Readout::Binned => ov9281::Readout::Binned,
    }
}

/// The driver's test pattern.
pub const fn test_pattern(p: espion_proto::cam::TestPattern) -> ov9281::TestPattern {
    use espion_proto::cam::TestPattern as P;
    match p {
        P::Off => ov9281::TestPattern::Off,
        P::Bars => ov9281::TestPattern::Bars,
        P::RollingBars => ov9281::TestPattern::RollingBars,
        P::GreySquares => ov9281::TestPattern::GreySquares,
        P::BlackWhiteSquares => ov9281::TestPattern::BlackWhiteSquares,
        P::Random => ov9281::TestPattern::Random,
    }
}

/// The wire mirror of a builder error.
pub const fn from_mode_error(e: ov9281::Error) -> CamError {
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

/// The ESP32-P4 CSI-2 receiver's geometry rules (`esp32p4-csi` `Config::validate`): 12-bit
/// sizes, whole 64-bit DMA items per frame, whole 32-bit words per line, and for RAW10 a word
/// count per line that is a multiple of 4 and an even height.
pub fn receiver_accepts(f: Format, width: u16, height: u16) -> bool {
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

/// The sensor mode of the settings' mode part (format, size, readout, origin, rate and
/// tolerance), then the receiver's check.
pub fn build_mode(c: &CameraSettings) -> Result<Mode, CamError> {
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
pub fn check_controls(c: &CameraSettings, mode: &Mode) -> Result<(), CamError> {
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

/// Every check of a `SetCamera`, in the board's order: the mode (builder, then receiver),
/// the stream's encoding against the format, then exposure and gain. Nothing is applied
/// unless all pass.
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

/// The window's fastest rates (millihertz) at 80 MHz (the datasheet's clock) and at 96 MHz:
/// the minimum VTS at the shortest line.
pub fn fastest_rates(f: Format, width: u16, height: u16, r: Readout) -> (u32, u32) {
    let vts = limits::min_vts(readout(r), height);
    let line_bytes = format(f).line_bytes(width);
    let rate = |clock: SensorClock| {
        let cycles = ov9281::frame_cycles(limits::hts_min(line_bytes, clock), vts);
        ((clock.hz() as u64 * 2000 + cycles) / (2 * cycles)) as u32
    };
    (rate(SensorClock::Mhz80), rate(SensorClock::Mhz96))
}

/// Slowest rate of every window (millihertz): 80 MHz at the longest line and frame.
pub fn slowest_rate() -> u32 {
    let cycles = ov9281::frame_cycles(limits::HTS_MAX, limits::VTS_MAX);
    ((SensorClock::Mhz80.hz() as u64 * 2000 + cycles) / (2 * cycles)) as u32
}

#[cfg(test)]
mod tests {
    use espion_proto::cam::TestPattern;

    use super::*;

    fn settings() -> CameraSettings {
        CameraSettings {
            format: Format::Raw8,
            width: 1280,
            height: 800,
            readout: Readout::Crop,
            origin: None,
            rate_mhz: 120_000,
            tolerance_mhz: 100,
            exposure_lines: 100,
            gain_sixteenths: 16,
            test_pattern: TestPattern::Off,
        }
    }

    #[test]
    fn the_driver_decides_the_mode() {
        // rq01 of the board's request verification: 80 MHz, HTS 728, VTS 914, PSV on.
        let m = validate(&settings(), Encoding::Raw8).unwrap();
        let info = mode_info(&m);
        assert_eq!((info.sensor_clock_mhz, info.hts, info.vts), (80, 728, 914));
        assert!(info.psv);
        assert_eq!(info.frame_rate_mhz, 120_049);
        assert_eq!(applied(&settings(), &m).origin, Some(Origin { x: 0, y: 0 }));
    }

    #[test]
    fn refusals_carry_the_drivers_values() {
        let c = CameraSettings {
            rate_mhz: 160_000,
            ..settings()
        };
        assert_eq!(
            validate(&c, Encoding::Raw8),
            Err(CamError::TooFast { max_mhz: 153_984 })
        );
        let c = CameraSettings {
            width: 1276,
            ..settings()
        };
        assert_eq!(
            validate(&c, Encoding::Raw8),
            Err(CamError::Alignment {
                width_multiple: 8,
                height_multiple: 1
            })
        );
        assert_eq!(
            validate(&settings(), Encoding::Raw10),
            Err(CamError::Encoding {
                format: Format::Raw8,
                encoding: Encoding::Raw10
            })
        );
        let c = CameraSettings {
            exposure_lines: 890,
            ..settings()
        };
        assert_eq!(
            validate(&c, Encoding::Raw8),
            Err(CamError::Exposure { max_lines: 889 })
        );
        let c = CameraSettings {
            gain_sixteenths: 15,
            ..settings()
        };
        assert_eq!(
            validate(&c, Encoding::Raw8),
            Err(CamError::Gain { min: 16, max: 248 })
        );
    }

    #[test]
    fn fastest_rates_match_the_measured_table() {
        // ov9281 README, "Fastest and slowest rates the crate gives".
        assert_eq!(
            fastest_rates(Format::Raw8, 1280, 800, Readout::Crop),
            (128_320, 153_984)
        );
        assert_eq!(
            fastest_rates(Format::Raw10, 1280, 800, Readout::Crop),
            (128_320, 131_781)
        );
        assert_eq!(
            fastest_rates(Format::Raw8, 640, 400, Readout::Binned),
            (251_824, 302_189)
        );
        assert_eq!(slowest_rate(), 419);
    }

    #[test]
    fn every_mode_the_driver_accepts_passes_the_receiver() {
        let mut accepted = 0;
        for f in [Format::Raw8, Format::Raw10] {
            for w in (128..=1280).step_by(8) {
                for h in 2..=800 {
                    let ok = ov9281::Mode::builder()
                        .format(format(f))
                        .size(w, h)
                        .frame_rate(Rate::hz(10.0), Tolerance::hz(10.0))
                        .build()
                        .is_ok();
                    if ok {
                        accepted += 1;
                        assert!(receiver_accepts(f, w, h), "{f:?} {w}x{h}");
                    }
                }
            }
        }
        // RAW8: 145 widths x 799 heights; RAW10: 19 widths x 400 even heights.
        assert_eq!(accepted, 145 * 799 + 19 * 400);
    }
}
