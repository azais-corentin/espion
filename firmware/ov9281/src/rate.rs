//! Frame rate and frame-rate tolerance, both exact to the millihertz.

use core::fmt;

/// A frame rate in millihertz.
///
/// [`Rate::hz`] rounds to the nearest millihertz; [`Rate::millihertz`] is exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rate(u32);

impl Rate {
    /// `mhz` millihertz, exactly.
    pub const fn millihertz(mhz: u32) -> Self {
        Self(mhz)
    }

    /// `hz` hertz, rounded to the nearest millihertz. Negative values and NaN give 0 Hz, values
    /// above `u32::MAX` millihertz saturate.
    pub const fn hz(hz: f32) -> Self {
        Self(milli(hz))
    }

    /// The rate in millihertz.
    pub const fn as_millihertz(self) -> u32 {
        self.0
    }

    /// The rate in hertz (for display; the driver computes in millihertz).
    pub const fn as_hz(self) -> f32 {
        (self.0 as f64 / 1000.0) as f32
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:03} Hz", self.0 / 1000, self.0 % 1000)
    }
}

/// The largest acceptable difference between the requested and the achievable frame rate, in
/// millihertz, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tolerance(u32);

impl Tolerance {
    /// `mhz` millihertz, exactly.
    pub const fn millihertz(mhz: u32) -> Self {
        Self(mhz)
    }

    /// `hz` hertz, rounded to the nearest millihertz. Negative values and NaN give 0 (only an
    /// exact rate is accepted), values above `u32::MAX` millihertz saturate.
    pub const fn hz(hz: f32) -> Self {
        Self(milli(hz))
    }

    /// The tolerance in millihertz.
    pub const fn as_millihertz(self) -> u32 {
        self.0
    }
}

/// `v * 1000` rounded to the nearest integer; 0 for negative values and NaN (a float-to-integer
/// `as` cast saturates, and maps NaN to 0).
const fn milli(v: f32) -> u32 {
    let m = v as f64 * 1000.0;
    if m > 0.0 { (m + 0.5) as u32 } else { 0 }
}
