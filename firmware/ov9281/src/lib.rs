//! OV9281 driver: computes a sensor mode from a request and drives the sensor over SCCB.
//!
//! The caller asks for a format (RAW8 or RAW10), an output size, a readout (crop or 2x2
//! binning, with an optional window origin) and a frame rate with a tolerance. The
//! [`ModeBuilder`] checks the request against [`limits`], picks the sensor clock, HTS and VTS
//! and returns a [`Mode`], or an [`Error`] saying what is wrong. [`Mode::registers`] generates
//! the register writes; [`Ov9281`] writes them over any `embedded-hal-async` I2C bus and sets
//! exposure, gain and test patterns. `no_std`, no heap, no esp dependency: it builds and tests
//! on the host.
//!
//! Fixed set-up: 2 MIPI CSI-2 data lanes at 800 Mbit/s each ([`limits::LANE_MBPS`]), 24 MHz
//! XVCLK, continuous clock lane, free-running (FSIN/XVS trigger off, STROBE output off).
//!
//! # Frame rate
//!
//! A frame lasts `(VTS - 1) x HTS + 1730` SCLK cycles ([`frame_cycles`], measured). The solver
//! ([`ModeBuilder::build`]) takes 80 MHz and the shortest allowed line ([`limits::hts_min`],
//! 728) first, lengthens the line only as far as the tolerance needs (up to
//! [`limits::HTS_MAX`]), and uses 96 MHz (above the datasheet's 80 MHz, flagged by
//! [`Mode::exceeds_datasheet_clock`]) only when the requested rate is above the fastest 80 MHz
//! rate of the window and format. VTS is the one nearest the request, at or above the window's
//! minimum ([`limits::min_vts`]). [`Mode::frame_rate`] is that rate at an exact 24 MHz XVCLK;
//! the characterised sensor ran 24..34 ppm fast against the ESP32-P4's crystal, so measure
//! against the real oscillator, not this figure.
//!
//! The limits are measured on one sensor (2026-11-10); `README.md` holds the measurements and
//! the same example as below (compiled as a doc test).
//!
//! # Example
//!
//! ```
//! use ov9281::{Address, Exposure, Format, Gain, Mode, Ov9281, Rate, Readout, TestPattern, Tolerance};
//! # async fn run<I>(
//! #     i2c: I,
//! #     mut delay: impl embedded_hal_async::delay::DelayNs,
//! # ) -> Result<(), Box<dyn core::error::Error>>
//! # where
//! #     I: embedded_hal_async::i2c::I2c,
//! #     I::Error: 'static,
//! # {
//!
//! let mode = Mode::builder()
//!     .format(Format::Raw10)                           // Raw8 | Raw10
//!     .size(640, 400)                                  // output pixels
//!     .readout(Readout::Binned)                        // optional, default Readout::Crop
//!     .origin(0, 0)                                    // optional, default centred (sensor pixels)
//!     .frame_rate(Rate::hz(210.0), Tolerance::hz(0.1)) // also Rate::millihertz(209_563)
//!     .build()?;
//! assert_eq!(mode.frame_rate(), Rate::millihertz(209_964)); // VTS 522 at 80 MHz
//! assert!(!mode.exceeds_datasheet_clock());
//!
//! let mut cam = Ov9281::new(i2c, Address::Primary);
//! cam.identify().await?;                               // chip ID
//! cam.configure(&mode, &mut delay).await?;             // writes the registers, stays in standby
//! cam.set_exposure(Exposure::lines(400)).await?;       // error above mode.max_exposure_lines()
//! cam.set_gain(Gain::from_sixteenths(0x20)).await?;    // 2x
//! cam.set_test_pattern(TestPattern::Bars).await?;
//! // ...start the CSI receiver (data type mode.format().data_type()), then:
//! cam.start().await?;
//! cam.stop().await?;
//! # Ok(())
//! # }
//! ```
//!
//! The builder works in a `const`:
//!
//! ```
//! use ov9281::{Format, Mode, Rate, Readout, Tolerance};
//!
//! const MODE: Mode = match Mode::builder()
//!     .format(Format::Raw8)
//!     .size(320, 200)
//!     .readout(Readout::Binned)
//!     .frame_rate(Rate::millihertz(453_386), Tolerance::millihertz(0))
//!     .build()
//! {
//!     Ok(m) => m,
//!     Err(_) => panic!("320x200 at 453 Hz"),
//! };
//! assert_eq!(MODE.vts(), 241);
//! ```
//!
//! `build` exists only once format, size and frame rate are set; this does not compile:
//!
//! ```compile_fail
//! let _ = ov9281::Mode::builder().format(ov9281::Format::Raw8).size(640, 400).build();
//! ```

#![no_std]

mod device;
pub mod limits;
mod mode;
mod rate;
mod regs;

pub use device::{Address, DeviceError, Exposure, Gain, Ov9281, TestPattern};
pub use mode::{Error, Format, Mode, ModeBuilder, Readout, SensorClock, Set, Unset, frame_cycles};
pub use rate::{Rate, Tolerance};
pub use regs::{CHIP_ID, Registers, Write};

/// Compiles and runs the code blocks of `README.md` as doc tests, so its example stays in step
/// with the one above.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
