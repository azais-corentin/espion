//! The PoC's 12 sensor modes, as requests to the `ov9281` crate's mode builder, solved at
//! compile time: an invalid request fails the build.
//!
//! [`MODES`] 0..=3 are the 120 fps RAW8 modes of question 2 (1280x800, 1280x720 crop, 640x480
//! crop, 640x400 binned). 4..=11 are the Inno-Maker CAM-MIPIOV9281 V2 mode list: RAW8 1280x800
//! and 1280x720 at 144 fps, 640x400 binned at 210 and 253, 320x200 binned at 453; RAW10
//! 1280x800 and 1280x720 at 120, 640x400 binned at 210. Each asks for the nominal rate with a
//! tolerance; the solver picks clock, HTS, VTS and PSV (the camera section logs its choice).

use ov9281::Format::{Raw8, Raw10};
use ov9281::Readout::{Binned, Crop};
use ov9281::{Format, Mode, Rate, Readout, Tolerance};

/// A sensor mode and the name the logs and result tables use for it.
#[derive(Clone, Copy)]
pub struct CamMode {
    pub name: &'static str,
    pub mode: Mode,
}

impl CamMode {
    /// The requested rate rounded to whole frames per second (120 for 120 Hz +- 0.5): the
    /// stills' names and the strip soak's budget.
    pub const fn nominal_fps(&self) -> u16 {
        let requested =
            self.mode.frame_rate().as_millihertz() as i64 - self.mode.rate_error() as i64;
        ((requested + 500) / 1000) as u16
    }

    /// RAW10 (else RAW8).
    pub const fn raw10(&self) -> bool {
        matches!(self.mode.format(), Format::Raw10)
    }
}

/// `format`, `width` x `height` with `readout`, centred, at `hz` +- `tol_hz`.
pub const fn request(
    name: &'static str,
    format: Format,
    (width, height): (u16, u16),
    readout: Readout,
    hz: f32,
    tol_hz: f32,
) -> CamMode {
    let mode = match Mode::builder()
        .format(format)
        .size(width, height)
        .readout(readout)
        .frame_rate(Rate::hz(hz), Tolerance::hz(tol_hz))
        .build()
    {
        Ok(m) => m,
        Err(_) => panic!("a PoC mode request is refused by the mode builder"),
    };
    CamMode { name, mode }
}

/// Every mode, by index. +-0.5 Hz lets the solver keep the standard line length (HTS 728) in
/// every mode: the nearest VTS there misses the rate by at most 0.39 Hz (453 Hz). The clock is
/// the solver's: 96 MHz where the rate is above the window's 80 MHz maximum (144 and 253 Hz).
pub const MODES: [CamMode; 12] = [
    request("1280x800", Raw8, (1280, 800), Crop, 120.0, 0.5),
    request("1280x720 crop", Raw8, (1280, 720), Crop, 120.0, 0.5),
    request("640x480 crop", Raw8, (640, 480), Crop, 120.0, 0.5),
    request("640x400 bin2", Raw8, (640, 400), Binned, 120.0, 0.5),
    request("1280x800 RAW8 144fps", Raw8, (1280, 800), Crop, 144.0, 0.5),
    request(
        "1280x720 crop RAW8 144fps",
        Raw8,
        (1280, 720),
        Crop,
        144.0,
        0.5,
    ),
    request(
        "640x400 bin2 RAW8 210fps",
        Raw8,
        (640, 400),
        Binned,
        210.0,
        0.5,
    ),
    request(
        "640x400 bin2 RAW8 253fps",
        Raw8,
        (640, 400),
        Binned,
        253.0,
        0.5,
    ),
    request(
        "320x200 bin2 RAW8 453fps",
        Raw8,
        (320, 200),
        Binned,
        453.0,
        0.5,
    ),
    request("1280x800 RAW10", Raw10, (1280, 800), Crop, 120.0, 0.5),
    request("1280x720 crop RAW10", Raw10, (1280, 720), Crop, 120.0, 0.5),
    request(
        "640x400 bin2 RAW10 210fps",
        Raw10,
        (640, 400),
        Binned,
        210.0,
        0.5,
    ),
];
