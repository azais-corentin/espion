//! The sensor on an I2C (SCCB) bus, over `embedded-hal-async`.

use core::fmt;

use embedded_hal_async::{delay::DelayNs, i2c::I2c};

use crate::{
    Mode, Write, limits,
    regs::{
        ADDRESS_ALTERNATE, ADDRESS_PRIMARY, CHIP_ID, CHIP_ID_REG, EXPOSURE_REG, GAIN_REG,
        MODE_SELECT, TEST_PATTERN_REG, TEST_PATTERN_SEED_REG, exposure_bytes,
    },
};

/// SCCB address, set by the SID pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Address {
    /// SID low: 7-bit 0x60 (8-bit write address 0xC0).
    Primary,
    /// SID high: 7-bit 0x10 (8-bit write address 0x20).
    Alternate,
}

impl Address {
    /// The 7-bit address.
    pub const fn bits(self) -> u8 {
        match self {
            Self::Primary => ADDRESS_PRIMARY,
            Self::Alternate => ADDRESS_ALTERNATE,
        }
    }
}

/// Exposure time in lines (one line = [`Mode::line_ns`]). Valid:
/// [`limits::EXPOSURE_MIN`]`..=`[`Mode::max_exposure_lines`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Exposure(u16);

impl Exposure {
    /// `lines` lines.
    pub const fn lines(lines: u16) -> Self {
        Self(lines)
    }

    /// The exposure in lines.
    pub const fn as_lines(self) -> u16 {
        self.0
    }
}

/// Analog gain in sixteenths (real gain format): 0x10 = 1x, 0x20 = 2x. Valid:
/// [`limits::GAIN_MIN`]`..=`[`limits::GAIN_MAX`] (1x to 15.5x).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Gain(u8);

impl Gain {
    /// `sixteenths` / 16 times.
    pub const fn from_sixteenths(sixteenths: u8) -> Self {
        Self(sixteenths)
    }

    /// The gain in sixteenths.
    pub const fn as_sixteenths(self) -> u8 {
        self.0
    }
}

/// Sensor test pattern (PRE_CTRL00, 0x5E00), replacing the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestPattern {
    /// The sensor image (0x00).
    Off,
    /// Standard colour bar pattern (0x80): 8 vertical bars, scaled to the output width.
    Bars,
    /// Rolling bars (0xC0).
    RollingBars,
    /// Grey squares (0x82).
    GreySquares,
    /// Black-and-white squares (0x92).
    BlackWhiteSquares,
    /// Random data (0x81) from the same seed every frame (0x5E01 = 0x51, "same seed enable"):
    /// every frame is identical and every row different, which is how the strip path was
    /// checked against whole frames. Without the seed bit every frame differs.
    Random,
}

impl TestPattern {
    const fn value(self) -> u8 {
        match self {
            Self::Off => 0x00,
            Self::Bars => 0x80,
            Self::RollingBars => 0xC0,
            Self::GreySquares => 0x82,
            Self::BlackWhiteSquares => 0x92,
            Self::Random => 0x81,
        }
    }
}

/// PRE_CTRL01 with "same seed enable" (bit 4), as the board checks wrote it.
const SAME_SEED: u8 = 0x51;

/// Error of an [`Ov9281`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceError<E> {
    /// The I2C bus failed.
    Bus(E),
    /// The chip ID registers read `found` instead of 0x9281.
    ChipId {
        /// What {0x300A, 0x300B} read.
        found: u16,
    },
    /// The call needs a configured mode ([`Ov9281::configure`]).
    NotConfigured,
    /// Exposure outside 1..=`max` lines, `max` being the configured mode's
    /// [`Mode::max_exposure_lines`].
    Exposure {
        /// Longest exposure of the configured mode.
        max: u16,
    },
    /// Gain outside [`limits::GAIN_MIN`]`..=`[`limits::GAIN_MAX`].
    Gain,
}

impl<E: fmt::Debug> fmt::Display for DeviceError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(e) => write!(f, "I2C bus error: {e:?}"),
            Self::ChipId { found } => write!(f, "chip ID {found:#06x}, expected {CHIP_ID:#06x}"),
            Self::NotConfigured => f.write_str("no mode configured"),
            Self::Exposure { max } => write!(f, "exposure outside 1..={max} lines"),
            Self::Gain => f.write_str("gain outside 1x..=15.5x"),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for DeviceError<E> {}

/// An OV9281 on an I2C bus.
///
/// Remembers the last mode written by [`Ov9281::configure`] (a [`Mode`] copy, no heap) to
/// check exposures against it.
#[derive(Debug)]
pub struct Ov9281<I2C> {
    i2c: I2C,
    address: u8,
    mode: Option<Mode>,
}

impl<I2C: I2c> Ov9281<I2C> {
    /// The sensor at `address` on `i2c`. Talks to nothing yet.
    pub const fn new(i2c: I2C, address: Address) -> Self {
        Self {
            i2c,
            address: address.bits(),
            mode: None,
        }
    }

    /// Gives the bus back.
    pub fn release(self) -> I2C {
        self.i2c
    }

    /// The mode last written by [`Ov9281::configure`].
    pub const fn mode(&self) -> Option<&Mode> {
        self.mode.as_ref()
    }

    /// Reads the chip ID (0x300A, 0x300B in one read) and checks it is 0x9281.
    pub async fn identify(&mut self) -> Result<(), DeviceError<I2C::Error>> {
        let mut id = [0u8; 2];
        self.i2c
            .write_read(self.address, &CHIP_ID_REG.to_be_bytes(), &mut id)
            .await
            .map_err(DeviceError::Bus)?;
        match u16::from_be_bytes(id) {
            CHIP_ID => Ok(()),
            found => Err(DeviceError::ChipId { found }),
        }
    }

    /// Writes `mode`'s registers ([`Mode::registers`]), waiting on `delay` where the list says.
    /// The list starts with a software reset (which also undoes earlier exposure, gain and
    /// test-pattern settings) and leaves the sensor in software standby: start the CSI
    /// receiver, then [`Ov9281::start`].
    pub async fn configure(
        &mut self,
        mode: &Mode,
        delay: &mut impl DelayNs,
    ) -> Result<(), DeviceError<I2C::Error>> {
        self.mode = None;
        for w in mode.registers() {
            match w {
                Write::Reg(reg, value) => self.write(reg, value).await?,
                Write::DelayMs(ms) => delay.delay_ms(ms as u32).await,
            }
        }
        self.mode = Some(*mode);
        Ok(())
    }

    /// Sets the exposure, effective one frame later. Three register writes, not atomic
    /// against the frame boundary.
    pub async fn set_exposure(
        &mut self,
        exposure: Exposure,
    ) -> Result<(), DeviceError<I2C::Error>> {
        let max = match &self.mode {
            Some(m) => m.max_exposure_lines(),
            None => return Err(DeviceError::NotConfigured),
        };
        let lines = exposure.as_lines();
        if !(limits::EXPOSURE_MIN..=max).contains(&lines) {
            return Err(DeviceError::Exposure { max });
        }
        let [a, b, c] = exposure_bytes(lines);
        self.write(EXPOSURE_REG, a).await?;
        self.write(EXPOSURE_REG + 1, b).await?;
        self.write(EXPOSURE_REG + 2, c).await
    }

    /// Sets the analog gain, effective one frame later.
    pub async fn set_gain(&mut self, gain: Gain) -> Result<(), DeviceError<I2C::Error>> {
        if !(limits::GAIN_MIN..=limits::GAIN_MAX).contains(&gain.as_sixteenths()) {
            return Err(DeviceError::Gain);
        }
        self.write(GAIN_REG, gain.as_sixteenths()).await
    }

    /// Selects a test pattern, or the image with [`TestPattern::Off`]. Works while streaming.
    pub async fn set_test_pattern(
        &mut self,
        pattern: TestPattern,
    ) -> Result<(), DeviceError<I2C::Error>> {
        if let TestPattern::Random = pattern {
            self.write(TEST_PATTERN_SEED_REG, SAME_SEED).await?;
        }
        self.write(TEST_PATTERN_REG, pattern.value()).await
    }

    /// Starts streaming (SC_MODE_SELECT = 1). Start the CSI receiver first; the datasheet
    /// allows up to 10 ms to the first frame.
    pub async fn start(&mut self) -> Result<(), DeviceError<I2C::Error>> {
        self.write(MODE_SELECT, 0x01).await
    }

    /// Software standby (SC_MODE_SELECT = 0); the current frame completes first.
    pub async fn stop(&mut self) -> Result<(), DeviceError<I2C::Error>> {
        self.write(MODE_SELECT, 0x00).await
    }

    async fn write(&mut self, reg: u16, value: u8) -> Result<(), DeviceError<I2C::Error>> {
        let [hi, lo] = reg.to_be_bytes();
        self.i2c
            .write(self.address, &[hi, lo, value])
            .await
            .map_err(DeviceError::Bus)
    }
}
