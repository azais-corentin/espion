//! The OV9281 and the CSI-2 receiver, set up as `poc-p4-eth` does: SCCB on I2C0 (GPIO7 SDA,
//! GPIO8 SCL, 100 kHz), the D-PHY supply from LDO VO3 at 2.5 V, whole frames into [`BUFFERS`]
//! buffers at the start of PSRAM (`esp32p4-csi`), each sized for the largest mode (1280x800
//! RAW10), the sensor free-running.
//!
//! [`Camera::start`] runs any mode of the `ov9281` crate: sensor registers and controls, receiver
//! geometry, receiver start, the lanes checked in their stop state, stream on, the first frame.
//! [`Camera::stop`] is stream off, the frame in flight let out, receiver off.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_time::{Delay, Duration, Timer, with_timeout};
use esp_hal::gpio::AnyPin;
use esp_hal::i2c::master::{Config as I2cConfig, Error as I2cError, I2c};
use esp_hal::peripherals::{I2C0, VDMA_CH0};
use esp_hal::time::Rate;
use esp32p4_csi::{Config, Csi, Frame, LdoChannel, Stats, enable_ldo};
use log::{error, info};
use ov9281::Format::Raw8;
use ov9281::Readout::Crop;
use ov9281::{
    Address, DeviceError, Exposure, Gain, Mode, Ov9281, Rate as FrameRate, TestPattern, Tolerance,
    limits,
};
use static_cell::StaticCell;

/// 1280x800 RAW8 at 120 fps +- 0.5 Hz (the `poc-p4-eth` mode 0): the bench's mode and the
/// receiver's geometry at [`init`].
pub const MODE: Mode = match Mode::builder()
    .format(Raw8)
    .size(1280, 800)
    .readout(Crop)
    .frame_rate(FrameRate::hz(120.0), Tolerance::hz(0.5))
    .build()
{
    Ok(m) => m,
    Err(_) => panic!("the camera mode is refused by the mode builder"),
};
/// Frame buffers the receiver rotates: one held by the sender, one being written, one ready,
/// one spare for the moment the capture task holds a frame it hands on or drops.
pub const BUFFERS: usize = 4;
/// Bytes per buffer: the largest frame, 1280x800 RAW10 (a multiple of the 128-byte cache-line
/// alignment the driver wants).
pub const BUFFER_LEN: usize = Csi::buffer_len(&Config::raw10(1280, 800, limits::LANE_MBPS));
/// The bench's exposure in lines (as `poc-p4-eth`: indoor scene without the IR ring).
pub const BENCH_CONTROLS: Controls = Controls {
    exposure_lines: 800,
    gain: Gain::from_sixteenths(0x20),
    pattern: TestPattern::Off,
};
/// SCCB pins: net ESP_I2C_SDA (GPIO7, CSI connector J1 pin 2) and ESP_I2C_SCL (GPIO8, J1 pin 3).
const SCCB_SDA_GPIO: u8 = 7;
const SCCB_SCL_GPIO: u8 = 8;
const SCCB_FREQ_KHZ: u32 = 100;
/// Wait for the first frame after stream-on, beyond two frame periods of the mode.
const FIRST_FRAME_MARGIN: Duration = Duration::from_millis(500);
/// How long the lanes may take to reach their stop state after the receiver starts.
const LANES_STOP_TIMEOUT: Duration = Duration::from_millis(10);
/// Shortest wait after stream off before the receiver is reset.
const STOP_MIN: Duration = Duration::from_millis(20);

pub type Sensor = Ov9281<Bus>;
pub type SensorError = DeviceError<I2cError>;

/// 7-bit SCCB address no device on the bus answers: [`Bus::misaddress`] sends there.
const NO_DEVICE: u8 = 0x11;

/// The SCCB bus, with a fault switch for the stream service's error-path test: while
/// [`MISADDRESS`] is set every transfer goes to [`NO_DEVICE`], so the sensor calls fail with
/// the bus's real NACK error.
pub struct Bus {
    i2c: I2c<'static, esp_hal::Async>,
}

/// The [`Bus`] fault switch (the driver owns the bus, so the switch lives outside it).
static MISADDRESS: AtomicBool = AtomicBool::new(false);

impl embedded_hal::i2c::ErrorType for Bus {
    type Error = I2cError;
}

impl embedded_hal_async::i2c::I2c for Bus {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [embedded_hal::i2c::Operation<'_>],
    ) -> Result<(), Self::Error> {
        let address = if MISADDRESS.load(Ordering::Relaxed) {
            NO_DEVICE
        } else {
            address
        };
        embedded_hal_async::i2c::I2c::transaction(&mut self.i2c, address, operations).await
    }
}

/// A fault [`Camera::start`] injects once (the stream service's error-path test).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Every sensor write of the start goes to an address nobody answers: a sensor bus error.
    SensorBus,
    /// The sensor stream is not turned on: no first frame, a receiver error.
    NoStream,
}

/// Exposure, gain and test pattern: what changes without a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    /// Exposure in lines; clamped to the mode's range by [`Camera::start`].
    pub exposure_lines: u16,
    pub gain: Gain,
    pub pattern: TestPattern,
}

/// Why [`Camera::start`] failed. The receiver is stopped again in every case.
#[derive(Clone, Copy, Debug)]
pub enum StartError {
    /// A sensor write (mode registers, controls, stream on) failed.
    Sensor(SensorError),
    /// The receiver refused the configuration or the start.
    Receiver(esp32p4_csi::Error),
    /// The lanes were not all in their stop state before stream on.
    LanesNotStopped,
    /// No frame within two frame periods plus [`FIRST_FRAME_MARGIN`] of stream on.
    NoFirstFrame,
}

/// The sensor and the receiver.
pub struct Camera {
    sensor: Sensor,
    csi: Csi<'static>,
    /// The mode running (between a successful [`Camera::start`] and [`Camera::stop`]).
    running: Option<Mode>,
}

static CAMERA: StaticCell<Camera> = StaticCell::new();

/// Receiver configuration for `mode`: its format's data type, its output size, the sensor's
/// lane rate.
pub const fn csi_config(mode: &Mode) -> Config {
    Config {
        data_type: mode.format().data_type(),
        ..Config::new(mode.width(), mode.height(), limits::LANE_MBPS)
    }
}

/// How long a frame of `mode` may take: two frame periods plus [`FIRST_FRAME_MARGIN`] (a 2 Hz
/// mode's frames are 500 ms apart).
pub fn frame_timeout(mode: &Mode) -> Duration {
    FIRST_FRAME_MARGIN + Duration::from_micros(2 * u64::from(mode.frame_ns()) / 1000)
}

/// Finds the sensor, turns on the D-PHY supply and creates the receiver on the first
/// `BUFFERS * BUFFER_LEN` bytes of `psram` (cache-line aligned). `None` (logged) on failure.
pub async fn init(
    i2c0: I2C0<'static>,
    vdma: VDMA_CH0<'static>,
    psram: &'static mut [u8],
) -> Option<&'static mut Camera> {
    let mut i2c = make_bus(i2c0);
    let mut address = None;
    for a in [Address::Primary, Address::Alternate] {
        if Ov9281::new(&mut i2c, a).identify().await.is_ok() {
            address = Some(a);
            break;
        }
    }
    let Some(address) = address else {
        fail("no OV9281 on SCCB", &());
        return None;
    };
    if let Err(e) = enable_ldo(LdoChannel::Vo3, 2500) {
        fail("LDO VO3", &e);
        return None;
    }
    Timer::after_millis(1).await;
    make_camera(vdma, psram, i2c, address)
}

#[inline(never)]
fn make_bus(i2c0: I2C0<'static>) -> I2c<'static, esp_hal::Async> {
    I2c::new(
        i2c0,
        I2cConfig::default().with_frequency(Rate::from_khz(SCCB_FREQ_KHZ)),
    )
    .expect("I2C0 config")
    // SAFETY: GPIO7/GPIO8 are used only by this bus.
    .with_sda(unsafe { AnyPin::steal(SCCB_SDA_GPIO) })
    .with_scl(unsafe { AnyPin::steal(SCCB_SCL_GPIO) })
    .into_async()
}

#[inline(never)]
fn make_camera(
    vdma: VDMA_CH0<'static>,
    psram: &'static mut [u8],
    i2c: I2c<'static, esp_hal::Async>,
    address: Address,
) -> Option<&'static mut Camera> {
    let (b0, rest) = psram.split_at_mut(BUFFER_LEN);
    let (b1, rest) = rest.split_at_mut(BUFFER_LEN);
    let (b2, rest) = rest.split_at_mut(BUFFER_LEN);
    let (b3, _) = rest.split_at_mut(BUFFER_LEN);
    let csi = match Csi::new(vdma, [b0, b1, b2, b3], csi_config(&MODE)) {
        Ok(c) => c,
        Err(e) => {
            fail("Csi::new", &e);
            return None;
        }
    };
    crate::irq::install(&[
        crate::irq::Source::CsiDma,
        crate::irq::Source::CsiHost,
        crate::irq::Source::CsiBridge,
    ]);
    log_ready(address);
    Some(CAMERA.init(Camera {
        sensor: Ov9281::new(Bus { i2c }, address),
        csi,
        running: None,
    }))
}

#[inline(never)]
fn fail(what: &str, detail: &dyn core::fmt::Debug) {
    error!("camera: {what}: {detail:?}");
}

#[inline(never)]
fn log_ready(address: Address) {
    info!(
        "camera: OV9281 at {:#04x}, {} buffers of {} B in PSRAM",
        address.bits(),
        BUFFERS,
        BUFFER_LEN
    );
}

impl Camera {
    /// Writes `mode` and `controls`, configures and starts the receiver, checks the lanes are
    /// in their stop state, turns the sensor stream on and waits for the first frame (dropped).
    /// Stops first if a mode runs. On failure the receiver is stopped again.
    /// `fault` is injected into this start only.
    pub async fn start(
        &mut self,
        mode: &Mode,
        controls: &Controls,
        fault: Option<Fault>,
    ) -> Result<(), StartError> {
        if self.running.is_some() {
            let _ = self.stop().await;
        }
        MISADDRESS.store(fault == Some(Fault::SensorBus), Ordering::Relaxed);
        let setup = self.setup(mode, controls).await;
        MISADDRESS.store(false, Ordering::Relaxed);
        setup.map_err(StartError::Sensor)?;
        self.csi
            .set_config(csi_config(mode))
            .and_then(|()| self.csi.start())
            .map_err(StartError::Receiver)?;
        if !self.lanes_stopped().await {
            self.csi.stop();
            return Err(StartError::LanesNotStopped);
        }
        if fault != Some(Fault::NoStream)
            && let Err(e) = self.sensor.start().await
        {
            self.csi.stop();
            return Err(StartError::Sensor(e));
        }
        self.running = Some(*mode);
        match with_timeout(frame_timeout(mode), self.csi.wait_frame(None)).await {
            Ok(Ok(_)) => Ok(()),
            _ => {
                let _ = self.stop().await;
                Err(StartError::NoFirstFrame)
            }
        }
    }

    async fn setup(&mut self, mode: &Mode, c: &Controls) -> Result<(), SensorError> {
        self.sensor.configure(mode, &mut Delay).await?;
        self.set_controls(mode, c).await
    }

    /// Exposure (clamped to `mode`'s range), gain and test pattern, while streaming or not.
    pub async fn set_controls(&mut self, mode: &Mode, c: &Controls) -> Result<(), SensorError> {
        let lines = c
            .exposure_lines
            .clamp(limits::EXPOSURE_MIN, mode.max_exposure_lines());
        self.sensor.set_exposure(Exposure::lines(lines)).await?;
        self.sensor.set_gain(c.gain).await?;
        self.sensor.set_test_pattern(c.pattern).await
    }

    /// Polls the D-PHY until the clock and both data lanes are in their stop state (LP-11),
    /// at most [`LANES_STOP_TIMEOUT`].
    async fn lanes_stopped(&mut self) -> bool {
        for _ in 0..LANES_STOP_TIMEOUT.as_millis() * 4 {
            let phy = self.csi.phy_status();
            if phy.clock_stop && phy.data_stop[0] && phy.data_stop[1] {
                return true;
            }
            Timer::after_micros(250).await;
        }
        false
    }

    /// Stream off, then the receiver once the frame in flight is out (one frame period plus
    /// 2 ms, at least [`STOP_MIN`]). A failed stream-off write is returned after the receiver
    /// is stopped anyway.
    pub async fn stop(&mut self) -> Result<(), SensorError> {
        let r = self.sensor.stop().await;
        let frame = self.running.map_or(0, |m| u64::from(m.frame_ns()) / 1000);
        Timer::after(STOP_MIN.max(Duration::from_micros(frame + 2000))).await;
        self.csi.stop();
        self.running = None;
        r
    }

    /// Sensor stream off behind the receiver's back (the stream service's stall test): frames
    /// stop, the receiver stays started.
    pub async fn stall(&mut self) -> Result<(), SensorError> {
        self.sensor.stop().await
    }

    /// Gives `previous` back and waits (up to `timeout`) for the newest completed frame.
    pub async fn next(&mut self, previous: Option<Frame>, timeout: Duration) -> Option<Frame> {
        match with_timeout(timeout, self.csi.wait_frame(previous)).await {
            Ok(Ok(f)) => Some(f),
            _ => None,
        }
    }

    /// Waits for the newest completed frame; `Err` if the receiver is (or gets) stopped.
    pub async fn wait_frame(&mut self) -> Result<Frame, esp32p4_csi::Error> {
        self.csi.wait_frame(None).await
    }

    /// The mode running, if any.
    pub const fn running(&self) -> Option<&Mode> {
        self.running.as_ref()
    }

    pub fn stats(&self) -> Stats {
        self.csi.stats()
    }

    pub fn reset_stats(&mut self) {
        self.csi.reset_stats();
    }

    /// Bytes per frame of the bench's [`MODE`].
    pub const fn frame_bytes() -> usize {
        csi_config(&MODE).frame_bytes()
    }
}
