//! Sensor modes: the typestate builder, the solver that picks clock, HTS and VTS, and the mode's
//! derived figures.

use core::{fmt, marker::PhantomData};

use crate::{
    Rate, Tolerance,
    limits::{self, FRAME_LONG_LINE},
    regs::Registers,
};

/// Pixel format on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// 8 bits per pixel, CSI-2 data type 0x2A.
    Raw8,
    /// 10 bits per pixel, CSI-2 data type 0x2B, CSI-2 packed: every 4 pixels P0..P3 take
    /// 5 bytes, `P0[9:2] P1[9:2] P2[9:2] P3[9:2]`, then
    /// `P3[1:0] << 6 | P2[1:0] << 4 | P1[1:0] << 2 | P0[1:0]`; no padding (measured).
    Raw10,
}

impl Format {
    /// Bits per pixel: 8 or 10.
    pub const fn bits_per_pixel(self) -> u8 {
        match self {
            Self::Raw8 => 8,
            Self::Raw10 => 10,
        }
    }

    /// CSI-2 data type of the image long packets (virtual channel 0).
    pub const fn data_type(self) -> u8 {
        match self {
            Self::Raw8 => 0x2A,
            Self::Raw10 => 0x2B,
        }
    }

    /// Bytes of a `width`-pixel line on the wire.
    pub const fn line_bytes(self, width: u16) -> u32 {
        width as u32 * self.bits_per_pixel() as u32 / 8
    }
}

/// How the output window is read from the pixel array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Readout {
    /// One output pixel per sensor pixel: a window of the array, up to 1280x800.
    #[default]
    Crop,
    /// 2x2 binning: one output pixel per 2x2 sensor pixels, up to 640x400 (whole array).
    Binned,
}

/// Sensor system clock (PLL2 SCLK). HTS counts its cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensorClock {
    /// 80 MHz, the datasheet's maximum: 24 MHz / 3 x 80 / 4 / 2 (0x030D = 0x50).
    Mhz80,
    /// 96 MHz, above the datasheet's maximum (0x030D = 0x60, the Linux and Raspberry Pi RAW8
    /// value). Measured: at gain 15.5x the picture has vertical column stripes and a raised
    /// black floor; normal at gain 2x.
    Mhz96,
}

impl SensorClock {
    /// Clock in MHz.
    pub const fn mhz(self) -> u8 {
        match self {
            Self::Mhz80 => 80,
            Self::Mhz96 => 96,
        }
    }

    /// Clock in Hz.
    pub const fn hz(self) -> u32 {
        self.mhz() as u32 * 1_000_000
    }

    /// PLL2 multiplier (0x030D): SCLK = 24 MHz / 3 x multiplier / 4 / 2.
    pub(crate) const fn pll2_multiplier(self) -> u8 {
        match self {
            Self::Mhz80 => 0x50,
            Self::Mhz96 => 0x60,
        }
    }
}

/// Why [`ModeBuilder::build`] refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The size is zero, narrower than [`limits::MIN_WIDTH`], or larger than the readout
    /// allows (the largest is given).
    Size {
        /// Largest output width of the readout.
        max_width: u16,
        /// Largest output height of the readout.
        max_height: u16,
    },
    /// The width is not a multiple of `width_multiple` ([`limits::width_multiple`]).
    Alignment {
        /// Required width multiple for the format.
        width_multiple: u8,
    },
    /// The origin puts the window outside the active area, or is not a multiple of
    /// [`limits::ORIGIN_MULTIPLE`].
    Window,
    /// The request is above the fastest rate any valid setting gives for this window and
    /// format, even at 96 MHz (`max`; 0 Hz when no setting suits the window at all).
    TooFast {
        /// Fastest achievable rate.
        max: Rate,
    },
    /// The request is below the slowest achievable rate (`min`).
    TooSlow {
        /// Slowest achievable rate.
        min: Rate,
    },
    /// The nearest achievable rate (`nearest`) is outside the tolerance.
    Tolerance {
        /// Achievable rate nearest the request.
        nearest: Rate,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Size {
                max_width,
                max_height,
            } => write!(f, "size not supported (largest {max_width}x{max_height})"),
            Self::Alignment { width_multiple } => {
                write!(f, "width must be a multiple of {width_multiple}")
            }
            Self::Window => f.write_str("window outside the pixel array or off the origin grid"),
            Self::TooFast { max } => write!(f, "frame rate above the maximum {max}"),
            Self::TooSlow { min } => write!(f, "frame rate below the minimum {min}"),
            Self::Tolerance { nearest } => {
                write!(
                    f,
                    "nearest achievable frame rate {nearest} is outside the tolerance"
                )
            }
        }
    }
}

impl core::error::Error for Error {}

/// SCLK cycles per frame: `(vts - 1) * hts + FRAME_LONG_LINE` ([`limits::FRAME_LONG_LINE`]).
pub const fn frame_cycles(hts: u16, vts: u16) -> u64 {
    (vts as u64).saturating_sub(1) * hts as u64 + FRAME_LONG_LINE as u64
}

/// Frame rate in millihertz at `clock`, rounded to the nearest millihertz.
const fn rate_mhz(clock: SensorClock, hts: u16, vts: u16) -> u32 {
    let cycles = frame_cycles(hts, vts);
    ((clock.hz() as u64 * 2000 + cycles) / (2 * cycles)) as u32
}

/// A complete sensor mode: format, window, clock and line/frame timing.
///
/// Built by [`Mode::builder`]; a small `Copy` value. [`Mode::registers`] generates the register
/// writes from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mode {
    format: Format,
    readout: Readout,
    clock: SensorClock,
    psv: bool,
    width: u16,
    height: u16,
    origin_x: u16,
    origin_y: u16,
    hts: u16,
    vts: u16,
    requested: Rate,
}

impl Mode {
    /// Starts a mode request. [`ModeBuilder::build`] exists once
    /// [`format`](ModeBuilder::format), [`size`](ModeBuilder::size) and
    /// [`frame_rate`](ModeBuilder::frame_rate) are set.
    pub const fn builder() -> ModeBuilder<Unset, Unset, Unset> {
        ModeBuilder {
            format: Format::Raw8,
            width: 0,
            height: 0,
            readout: Readout::Crop,
            origin: None,
            rate: Rate::millihertz(0),
            tolerance: Tolerance::millihertz(0),
            state: PhantomData,
        }
    }

    /// The rate the sensor runs at, from the frame period formula ([`frame_cycles`]) at the
    /// nominal clock, i.e. with XVCLK exactly 24 MHz. The bench sensor's oscillator ran 28 ppm
    /// fast against the ESP32-P4's crystal, so a real sensor differs by its oscillator's error.
    pub const fn frame_rate(&self) -> Rate {
        Rate::millihertz(rate_mhz(self.clock, self.hts, self.vts))
    }

    /// [`Mode::frame_rate`] minus the requested rate, in millihertz.
    pub const fn rate_error(&self) -> i32 {
        self.frame_rate().as_millihertz() as i32 - self.requested.as_millihertz() as i32
    }

    /// The sensor clock the mode runs at.
    pub const fn sensor_clock(&self) -> SensorClock {
        self.clock
    }

    /// `true` when the mode runs at 96 MHz, above the datasheet's 80 MHz: no 80 MHz setting met
    /// the request within its tolerance.
    pub const fn exceeds_datasheet_clock(&self) -> bool {
        matches!(self.clock, SensorClock::Mhz96)
    }

    /// Output width in pixels.
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Output height in lines.
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Pixel format.
    pub const fn format(&self) -> Format {
        self.format
    }

    /// Readout (crop or 2x2 binning).
    pub const fn readout(&self) -> Readout {
        self.readout
    }

    /// Top-left corner of the window in sensor pixels of the active area: the first output
    /// pixel is active pixel `origin`.
    pub const fn origin(&self) -> (u16, u16) {
        (self.origin_x, self.origin_y)
    }

    /// HTS: SCLK cycles per line.
    pub const fn hts(&self) -> u16 {
        self.hts
    }

    /// VTS: lines per frame.
    pub const fn vts(&self) -> u16 {
        self.vts
    }

    /// Whether the power-saving mode (PSV_CTRL 0x4F00) is on ([`limits::psv_on`]).
    pub const fn psv(&self) -> bool {
        self.psv
    }

    /// Bytes of image data per line on the wire.
    pub const fn bytes_per_line(&self) -> u32 {
        self.format.line_bytes(self.width)
    }

    /// Bytes of image data per frame on the wire.
    pub const fn frame_bytes(&self) -> u32 {
        self.bytes_per_line() * self.height as u32
    }

    /// Line period in nanoseconds, rounded (nominal clock).
    pub const fn line_ns(&self) -> u32 {
        let mhz = self.clock.mhz() as u32;
        (self.hts as u32 * 1000 + mhz / 2) / mhz
    }

    /// Frame period in nanoseconds, rounded (nominal clock).
    pub const fn frame_ns(&self) -> u32 {
        let mhz = self.clock.mhz() as u64;
        ((frame_cycles(self.hts, self.vts) * 1000 + mhz / 2) / mhz) as u32
    }

    /// Longest exposure in lines: VTS - [`limits::EXPOSURE_MARGIN`].
    pub const fn max_exposure_lines(&self) -> u16 {
        self.vts - limits::EXPOSURE_MARGIN
    }

    /// Exposure the register list sets, in lines: about 1 ms (SCLK / 1000 / HTS, rounded),
    /// within the mode's range.
    pub const fn default_exposure_lines(&self) -> u16 {
        let hts = self.hts as u32;
        let lines = ((self.clock.hz() / 1000 + hts / 2) / hts) as u16;
        let max = self.max_exposure_lines();
        if lines > max { max } else { lines }
    }

    /// Array rows read out per frame ([`limits::rows_read`]).
    pub const fn rows_read(&self) -> u16 {
        limits::rows_read(self.readout, self.height)
    }

    /// The register writes that set the mode, from the software reset to standby: write them in
    /// order, waiting at each [`Write::DelayMs`](crate::Write::DelayMs).
    pub const fn registers(&self) -> Registers<'_> {
        Registers::new(self)
    }
}

/// Typestate marker: the builder setting is still missing.
#[derive(Debug)]
pub struct Unset;
/// Typestate marker: the builder setting is given.
#[derive(Debug)]
pub struct Set;

/// Request for a [`Mode`], from [`Mode::builder`]. The type parameters record whether the
/// format, the size and the frame rate are set ([`Unset`] or [`Set`]); `build` exists only
/// when all three are. Every method is `const`.
#[derive(Debug)]
#[must_use]
pub struct ModeBuilder<F, S, R> {
    format: Format,
    width: u16,
    height: u16,
    readout: Readout,
    origin: Option<(u16, u16)>,
    rate: Rate,
    tolerance: Tolerance,
    state: PhantomData<(F, S, R)>,
}

impl<F, S, R> ModeBuilder<F, S, R> {
    const fn with<F2, S2, R2>(self) -> ModeBuilder<F2, S2, R2> {
        ModeBuilder {
            format: self.format,
            width: self.width,
            height: self.height,
            readout: self.readout,
            origin: self.origin,
            rate: self.rate,
            tolerance: self.tolerance,
            state: PhantomData,
        }
    }

    /// Crop (default) or 2x2 binning.
    pub const fn readout(mut self, readout: Readout) -> Self {
        self.readout = readout;
        self
    }

    /// Top-left corner of the window in sensor pixels of the active area (1280x800), for both
    /// readouts. Default: the window centred, rounded down to [`limits::ORIGIN_MULTIPLE`].
    pub const fn origin(mut self, x: u16, y: u16) -> Self {
        self.origin = Some((x, y));
        self
    }
}

impl<S, R> ModeBuilder<Unset, S, R> {
    /// Pixel format. Required.
    pub const fn format(mut self, format: Format) -> ModeBuilder<Set, S, R> {
        self.format = format;
        self.with()
    }
}

impl<F, R> ModeBuilder<F, Unset, R> {
    /// Output size in pixels (binned: binned pixels). Required.
    pub const fn size(mut self, width: u16, height: u16) -> ModeBuilder<F, Set, R> {
        self.width = width;
        self.height = height;
        self.with()
    }
}

impl<F, S> ModeBuilder<F, S, Unset> {
    /// Requested frame rate and the largest acceptable difference from it. Required.
    pub const fn frame_rate(mut self, rate: Rate, tolerance: Tolerance) -> ModeBuilder<F, S, Set> {
        self.rate = rate;
        self.tolerance = tolerance;
        self.with()
    }
}

impl ModeBuilder<Set, Set, Set> {
    /// Checks the request against [`limits`] and solves for clock, HTS and VTS.
    ///
    /// Clock: 80 MHz, or 96 MHz when no valid 80 MHz setting is within the tolerance. At each
    /// clock the solver tries [`limits::HTS_PREFERRED`], then the whole
    /// [`limits::HTS_MIN`]`..=`[`limits::HTS_MAX`] range; for each HTS that the line fits
    /// ([`limits::link_fits`]) it takes the VTS in [`limits::min_vts`]`..=`[`limits::VTS_MAX`]
    /// whose rate is nearest the request (on a tie the longer frame). PSV is then on where
    /// [`limits::psv_on`] allows it.
    pub const fn build(self) -> Result<Mode, Error> {
        let (format, readout, width, height) = (self.format, self.readout, self.width, self.height);
        let (max_width, max_height) = limits::max_size(readout);
        if width < limits::MIN_WIDTH || height == 0 || width > max_width || height > max_height {
            return Err(Error::Size {
                max_width,
                max_height,
            });
        }
        let width_multiple = limits::width_multiple(format);
        if !width.is_multiple_of(width_multiple as u16) {
            return Err(Error::Alignment { width_multiple });
        }
        let scale = limits::scale(readout);
        let (sensor_w, sensor_h) = (width * scale, height * scale);
        let (origin_x, origin_y) = match self.origin {
            Some(o) => o,
            None => (
                centred(limits::ACTIVE_WIDTH, sensor_w),
                centred(limits::ACTIVE_HEIGHT, sensor_h),
            ),
        };
        if !origin_x.is_multiple_of(limits::ORIGIN_MULTIPLE)
            || !origin_y.is_multiple_of(limits::ORIGIN_MULTIPLE)
            || origin_x as u32 + sensor_w as u32 > limits::ACTIVE_WIDTH as u32
            || origin_y as u32 + sensor_h as u32 > limits::ACTIVE_HEIGHT as u32
        {
            return Err(Error::Window);
        }

        let rows = limits::rows_read(readout, height);
        let s = match solve(
            readout,
            rows,
            format.line_bytes(width),
            self.rate.as_millihertz(),
            self.tolerance.as_millihertz(),
        ) {
            Ok(s) => s,
            Err(e) => return Err(e),
        };
        Ok(Mode {
            format,
            readout,
            clock: s.clock,
            psv: limits::psv_on(readout, rows, s.clock, s.vts),
            width,
            height,
            origin_x,
            origin_y,
            hts: s.hts,
            vts: s.vts,
            requested: self.rate,
        })
    }
}

/// Origin that centres `size` in `active`, rounded down to the origin grid.
const fn centred(active: u16, size: u16) -> u16 {
    let o = active.saturating_sub(size) / 2;
    o - o % limits::ORIGIN_MULTIPLE
}

/// One achievable setting.
#[derive(Clone, Copy)]
struct Setting {
    clock: SensorClock,
    hts: u16,
    vts: u16,
    rate: u32,
}

/// What the solver has seen so far for one request, over every clock and HTS tried.
struct Search {
    req: u32,
    line_bytes: u32,
    /// Nearest setting overall.
    nearest: Option<Setting>,
    /// Fastest achievable rate.
    max: u32,
    /// Slowest achievable rate.
    min: u32,
}

impl Search {
    /// The nearest setting at `clock` and `hts` with VTS at least `vmin`, recorded in the bounds
    /// and the overall nearest; `None` when the line does not fit the link at that HTS.
    const fn consider(&mut self, clock: SensorClock, hts: u16, vmin: u16) -> Option<Setting> {
        if !limits::link_fits(self.line_bytes, hts, clock) {
            return None;
        }
        let fastest = rate_mhz(clock, hts, vmin);
        let slowest = rate_mhz(clock, hts, limits::VTS_MAX);
        if fastest > self.max {
            self.max = fastest;
        }
        if slowest < self.min {
            self.min = slowest;
        }
        let s = nearest_vts(clock, hts, vmin, self.req);
        self.nearest = closer(self.nearest, s, self.req);
        Some(s)
    }

    const fn within(&self, s: &Setting, tol: u32) -> bool {
        s.rate.abs_diff(self.req) <= tol
    }
}

/// `s` if it is strictly nearer `req` than `best`.
const fn closer(best: Option<Setting>, s: Setting, req: u32) -> Option<Setting> {
    match best {
        Some(b) if b.rate.abs_diff(req) <= s.rate.abs_diff(req) => Some(b),
        _ => Some(s),
    }
}

/// The VTS in `vmin..=VTS_MAX` whose rate at `clock` and `hts` is nearest `req` (on a tie the
/// longer frame).
const fn nearest_vts(clock: SensorClock, hts: u16, vmin: u16, req: u32) -> Setting {
    let (lo, hi) = (vmin as u64, limits::VTS_MAX as u64);
    // Rate falls as VTS grows. `v0` is the largest VTS whose exact rate is at or above `req`
    // (the inner floor keeps it exact: floor((floor(x) - L) / h) = floor((x - L) / h) for
    // integers L and h), so the nearest is `v0` or `v0 + 1`.
    let v0 = if req == 0 {
        hi
    } else {
        let target = clock.hz() as u64 * 1000 / req as u64;
        if target <= FRAME_LONG_LINE as u64 {
            1
        } else {
            (target - FRAME_LONG_LINE as u64) / hts as u64 + 1
        }
    };
    let a = clamp(v0, lo, hi) as u16;
    let b = clamp(v0 + 1, lo, hi) as u16;
    let (ra, rb) = (rate_mhz(clock, hts, a), rate_mhz(clock, hts, b));
    let (vts, rate) = if ra.abs_diff(req) < rb.abs_diff(req) {
        (a, ra)
    } else {
        (b, rb)
    };
    Setting {
        clock,
        hts,
        vts,
        rate,
    }
}

const fn clamp(v: u64, lo: u64, hi: u64) -> u64 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// Clocks in the order the solver tries them.
const CLOCKS: [SensorClock; 2] = [SensorClock::Mhz80, SensorClock::Mhz96];

/// See [`ModeBuilder::build`].
const fn solve(
    readout: Readout,
    rows: u16,
    line_bytes: u32,
    req: u32,
    tol: u32,
) -> Result<Setting, Error> {
    let mut search = Search {
        req,
        line_bytes,
        nearest: None,
        max: 0,
        min: u32::MAX,
    };
    let mut c = 0;
    while c < CLOCKS.len() {
        let clock = CLOCKS[c];
        c += 1;
        let Some(vmin) = limits::min_vts(readout, rows, clock, false) else {
            continue;
        };
        if vmin as u32 > limits::VTS_MAX as u32 {
            continue;
        }
        if let Some(s) = search.consider(clock, limits::HTS_PREFERRED, vmin)
            && search.within(&s, tol)
        {
            return Ok(s);
        }
        // The preferred HTS cannot meet the request: the nearest over the rest of the range.
        let mut best: Option<Setting> = None;
        let mut hts = limits::HTS_MIN;
        loop {
            if hts != limits::HTS_PREFERRED
                && let Some(s) = search.consider(clock, hts, vmin)
            {
                best = closer(best, s, req);
            }
            if hts == limits::HTS_MAX {
                break;
            }
            hts += 1;
        }
        if let Some(b) = best
            && search.within(&b, tol)
        {
            return Ok(b);
        }
    }
    match search.nearest {
        None => Err(Error::TooFast {
            max: Rate::millihertz(0),
        }),
        Some(_) if req > search.max => Err(Error::TooFast {
            max: Rate::millihertz(search.max),
        }),
        Some(_) if req < search.min => Err(Error::TooSlow {
            min: Rate::millihertz(search.min),
        }),
        Some(n) => Err(Error::Tolerance {
            nearest: Rate::millihertz(n.rate),
        }),
    }
}
