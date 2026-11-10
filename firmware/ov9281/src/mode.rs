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
    /// value). Measured on one sensor: the same VTS, PSV and VFIFO limits in lines as at
    /// 80 MHz; at gain 15.5x an earlier run showed vertical column stripes and a raised black
    /// floor (not re-examined; the characterisation used gain 8x or less at 96 MHz).
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
    /// The width is outside [`limits::MIN_WIDTH`]`..=max_width` or the height outside
    /// [`limits::MIN_HEIGHT`]`..=max_height` (the largest of the readout are given).
    Size {
        /// Largest output width of the readout.
        max_width: u16,
        /// Largest output height of the readout.
        max_height: u16,
    },
    /// The width is not a multiple of `width_multiple` ([`limits::width_multiple`]) or the
    /// height not a multiple of `height_multiple` ([`limits::height_multiple`]); both are the
    /// format's.
    Alignment {
        /// Required width multiple for the format: 8 RAW8, 64 RAW10.
        width_multiple: u8,
        /// Required height multiple for the format: 1 RAW8, 2 RAW10.
        height_multiple: u8,
    },
    /// The window does not lie inside the active area (binned: `2 x width` by `2 x height`
    /// sensor pixels), or its origin is off the grid the sensor reads exactly: x a multiple
    /// of [`limits::ORIGIN_X_MULTIPLE`], y of [`limits::origin_y_multiple`].
    Window,
    /// The request is above the fastest rate of this window and format, 96 MHz at
    /// [`limits::min_vts`] and [`limits::hts_min`], by more than the tolerance.
    TooFast {
        /// Fastest achievable rate.
        max: Rate,
    },
    /// The request is below the slowest rate, 80 MHz at [`limits::VTS_MAX`] and
    /// [`limits::HTS_MAX`], by more than the tolerance.
    TooSlow {
        /// Slowest achievable rate.
        min: Rate,
    },
    /// No setting of the clock the request selects ([`ModeBuilder::build`]) is within the
    /// tolerance; `nearest` is the rate nearest the request over every HTS and VTS allowed at
    /// that clock.
    Tolerance {
        /// Achievable rate nearest the request at that clock.
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
            Self::Alignment {
                width_multiple,
                height_multiple,
            } => write!(
                f,
                "width must be a multiple of {width_multiple}, height of {height_multiple}"
            ),
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
    rate_of_cycles(clock, frame_cycles(hts, vts))
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

    /// `true` when the mode runs at 96 MHz, above the datasheet's 80 MHz: the requested rate
    /// is above the fastest 80 MHz rate of the window and format.
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
    /// readouts: x a multiple of [`limits::ORIGIN_X_MULTIPLE`], y of
    /// [`limits::origin_y_multiple`]. Default: the window centred, each coordinate rounded
    /// down to its multiple.
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
    /// The request is checked in this order: the size ([`Error::Size`]), the width and height
    /// multiples ([`Error::Alignment`]), then the window ([`Error::Window`]; without
    /// [`origin`](Self::origin) it is centred, rounded down to the origin grid).
    ///
    /// The solver then decides, in this order:
    /// 1. **Clock**: 80 MHz, unless the requested rate is above the fastest 80 MHz rate of the
    ///    window and format ([`limits::min_vts`] at [`limits::hts_min`]); then 96 MHz. The
    ///    tolerance does not move the clock: a request at or below the 80 MHz maximum stays at
    ///    80 MHz even when a 96 MHz setting would be nearer.
    /// 2. **HTS**: the clock's [`limits::hts_min`] (728, longer only for wide RAW10 lines at
    ///    96 MHz) when a VTS there is within the tolerance; otherwise the smallest longer HTS,
    ///    up to [`limits::HTS_MAX`], that has one.
    /// 3. **VTS**: in [`limits::min_vts`]`..=`[`limits::VTS_MAX`], the one whose rate is nearest
    ///    the request at that HTS (on a tie the longer frame).
    /// 4. **PSV** on from [`limits::psv_min_vts`] up, off below ([`limits::psv_on`]).
    ///
    /// Rates are compared rounded to 1 mHz, as [`Mode::frame_rate`] gives them. When no
    /// setting of the selected clock is within the tolerance: [`Error::TooFast`] if the request
    /// is above the 96 MHz maximum, [`Error::TooSlow`] if it is below the slowest 80 MHz rate,
    /// otherwise [`Error::Tolerance`] with the nearest rate over every HTS and VTS of the
    /// selected clock.
    ///
    /// Cost: at most one pass over the HTS range, 2185 steps (728..=2912) in the worst case (a
    /// request no HTS meets), each one 32-bit division and a few comparisons, plus about ten
    /// 64-bit divisions in all. No table, no allocation.
    pub const fn build(self) -> Result<Mode, Error> {
        let (format, readout, width, height) = (self.format, self.readout, self.width, self.height);
        let (max_width, max_height) = limits::max_size(readout);
        if width < limits::MIN_WIDTH
            || height < limits::MIN_HEIGHT
            || width > max_width
            || height > max_height
        {
            return Err(Error::Size {
                max_width,
                max_height,
            });
        }
        let width_multiple = limits::width_multiple(format);
        let height_multiple = limits::height_multiple(format);
        if !width.is_multiple_of(width_multiple as u16)
            || !height.is_multiple_of(height_multiple as u16)
        {
            return Err(Error::Alignment {
                width_multiple,
                height_multiple,
            });
        }
        let scale = limits::scale(readout);
        let (sensor_w, sensor_h) = (width * scale, height * scale);
        let y_multiple = limits::origin_y_multiple(readout);
        let (origin_x, origin_y) = match self.origin {
            Some(o) => o,
            None => (
                centred(limits::ACTIVE_WIDTH, sensor_w, limits::ORIGIN_X_MULTIPLE),
                centred(limits::ACTIVE_HEIGHT, sensor_h, y_multiple),
            ),
        };
        if !origin_x.is_multiple_of(limits::ORIGIN_X_MULTIPLE)
            || !origin_y.is_multiple_of(y_multiple)
            || origin_x as u32 + sensor_w as u32 > limits::ACTIVE_WIDTH as u32
            || origin_y as u32 + sensor_h as u32 > limits::ACTIVE_HEIGHT as u32
        {
            return Err(Error::Window);
        }

        let s = match solve(
            readout,
            height,
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
            psv: limits::psv_on(readout, height, s.vts),
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

/// Origin that centres `size` in `active`, rounded down to a multiple of `multiple`.
const fn centred(active: u16, size: u16, multiple: u16) -> u16 {
    let o = active.saturating_sub(size) / 2;
    o - o % multiple
}

/// The setting the solver chose.
#[derive(Clone, Copy)]
struct Setting {
    clock: SensorClock,
    hts: u16,
    vts: u16,
}

/// Rate in millihertz of a frame of `cycles` SCLK cycles at `clock`, rounded to the nearest.
const fn rate_of_cycles(clock: SensorClock, cycles: u64) -> u32 {
    ((clock.hz() as u64 * 2000 + cycles) / (2 * cycles)) as u32
}

/// The fastest rate of a window at `clock`: [`limits::min_vts`] (`vmin`) at
/// [`limits::hts_min`].
const fn fastest(clock: SensorClock, line_bytes: u32, vmin: u16) -> u32 {
    rate_mhz(clock, limits::hts_min(line_bytes, clock), vmin)
}

/// The frame lengths in SCLK cycles, `lo..=hi`, whose rate at `clock` ([`rate_of_cycles`]) is
/// within `tol` of `req`, saturated to `u32`.
const fn cycle_window(clock: SensorClock, req: u32, tol: u32) -> (u32, u32) {
    // rate = floor((K + c) / 2c) with K = 2000 x clock: rate <= r for c > K / (2r + 1), and
    // rate >= r for c <= K / (2r - 1).
    let k = clock.hz() as u64 * 2000;
    let lo = k / (2 * (req as u64 + tol as u64) + 1) + 1;
    let slowest = req.saturating_sub(tol) as u64;
    let hi = if slowest == 0 {
        u64::MAX
    } else {
        k / (2 * slowest - 1)
    };
    (saturate(lo), saturate(hi))
}

const fn saturate(v: u64) -> u32 {
    if v > u32::MAX as u64 {
        u32::MAX
    } else {
        v as u32
    }
}

/// The VTS in `vmin..=VTS_MAX` whose rate at `clock` and `hts` is nearest `req` (on a tie the
/// longer frame).
const fn nearest_vts(clock: SensorClock, hts: u16, vmin: u16, req: u32) -> u16 {
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
    if ra.abs_diff(req) < rb.abs_diff(req) {
        a
    } else {
        b
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

/// See [`ModeBuilder::build`].
const fn solve(
    readout: Readout,
    height: u16,
    line_bytes: u32,
    req: u32,
    tol: u32,
) -> Result<Setting, Error> {
    let vmin = limits::min_vts(readout, height);
    let clock = if req > fastest(SensorClock::Mhz80, line_bytes, vmin) {
        SensorClock::Mhz96
    } else {
        SensorClock::Mhz80
    };
    let (lo, hi) = cycle_window(clock, req, tol);
    // A frame is `k * hts + FRAME_LONG_LINE` cycles with k = VTS - 1 in kmin..=kmax; every
    // product fits a u32 (65534 x 2912 + 1730 < 2^28).
    let long = FRAME_LONG_LINE;
    let (kmin, kmax) = (vmin as u32 - 1, limits::VTS_MAX as u32 - 1);
    // The nearest frame lengths outside the window: the longest one shorter than `lo` (a
    // faster rate) and the shortest one longer than `hi`.
    let (mut faster, mut slower) = (0u32, u32::MAX);
    let mut hts = limits::hts_min(line_bytes, clock);
    loop {
        let h = hts as u32;
        // The smallest k whose frame is at least `lo` cycles.
        let k = if lo <= kmin * h + long {
            kmin
        } else {
            (lo - long).div_ceil(h)
        };
        if k > kmax {
            let c = kmax * h + long;
            if c > faster {
                faster = c;
            }
        } else {
            let c = k * h + long;
            if c <= hi {
                return Ok(Setting {
                    clock,
                    hts,
                    vts: nearest_vts(clock, hts, vmin, req),
                });
            }
            if c < slower {
                slower = c;
            }
            if k > kmin && c - h > faster {
                faster = c - h;
            }
        }
        if hts == limits::HTS_MAX {
            break;
        }
        hts += 1;
    }
    if let SensorClock::Mhz96 = clock {
        let max = fastest(SensorClock::Mhz96, line_bytes, vmin);
        if req > max {
            return Err(Error::TooFast {
                max: Rate::millihertz(max),
            });
        }
    } else {
        let min = rate_mhz(SensorClock::Mhz80, limits::HTS_MAX, limits::VTS_MAX);
        if req < min {
            return Err(Error::TooSlow {
                min: Rate::millihertz(min),
            });
        }
    }
    // Between the bounds a faster frame exists (the fastest setting is at or above the
    // request) and a slower one (the slowest is at or below it); on a tie the slower frame.
    let slow = rate_of_cycles(clock, slower as u64);
    let fast = if faster == 0 {
        u32::MAX
    } else {
        rate_of_cycles(clock, faster as u64)
    };
    let nearest = if fast.abs_diff(req) < slow.abs_diff(req) {
        fast
    } else {
        slow
    };
    Err(Error::Tolerance {
        nearest: Rate::millihertz(nearest),
    })
}
