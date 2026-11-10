//! The OV9281 and the CSI-2 receiver, set up as `poc-p4-eth` does: SCCB on I2C0 (GPIO7 SDA,
//! GPIO8 SCL, 100 kHz), the D-PHY supply from LDO VO3 at 2.5 V, whole frames of [`MODE`] into
//! [`BUFFERS`] buffers at the start of PSRAM (`esp32p4-csi`), the sensor free-running.

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
    Address, DeviceError, Exposure, Gain, Mode, Ov9281, Rate as FrameRate, Tolerance, limits,
};
use static_cell::StaticCell;

/// 1280x800 RAW8 at 120 fps +- 0.5 Hz (the `poc-p4-eth` mode 0).
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
/// Frame buffers the receiver rotates: one held by the sender, one being written, one ready.
pub const BUFFERS: usize = 3;
/// Bytes per buffer (a multiple of the 128-byte cache-line alignment the driver wants).
pub const BUFFER_LEN: usize = Csi::buffer_len(&csi_config());
/// Exposure in lines (as `poc-p4-eth`: indoor scene without the IR ring).
const EXPOSURE_LINES: u16 = 800;
/// Analog gain 2x.
const GAIN: Gain = Gain::from_sixteenths(0x20);
/// SCCB pins: net ESP_I2C_SDA (GPIO7, CSI connector J1 pin 2) and ESP_I2C_SCL (GPIO8, J1 pin 3).
const SCCB_SDA_GPIO: u8 = 7;
const SCCB_SCL_GPIO: u8 = 8;
const SCCB_FREQ_KHZ: u32 = 100;
/// Wait for the first frame after stream-on.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_millis(500);

pub type Sensor = Ov9281<I2c<'static, esp_hal::Async>>;
pub type SensorError = DeviceError<I2cError>;

/// The sensor and the receiver.
pub struct Camera {
    sensor: Sensor,
    csi: Csi<'static>,
}

static CAMERA: StaticCell<Camera> = StaticCell::new();

const fn csi_config() -> Config {
    Config::new(MODE.width(), MODE.height(), limits::LANE_MBPS)
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
    let (b2, _) = rest.split_at_mut(BUFFER_LEN);
    let csi = match Csi::new(vdma, [b0, b1, b2], csi_config()) {
        Ok(c) => c,
        Err(e) => {
            fail("Csi::new", &e);
            return None;
        }
    };
    log_ready(address);
    Some(CAMERA.init(Camera {
        sensor: Ov9281::new(i2c, address),
        csi,
    }))
}

#[inline(never)]
fn fail(what: &str, detail: &dyn core::fmt::Debug) {
    error!("camera: {what}: {detail:?}");
}

#[inline(never)]
fn log_ready(address: Address) {
    info!(
        "camera: OV9281 at {:#04x}, mode {}x{} RAW8 {} (requested 120 Hz +- 0.5), HTS {} VTS {}, {} buffers of {} B in PSRAM",
        address.bits(),
        MODE.width(),
        MODE.height(),
        MODE.frame_rate(),
        MODE.hts(),
        MODE.vts(),
        BUFFERS,
        BUFFER_LEN
    );
}

impl Camera {
    /// Writes the mode, starts the receiver, then the sensor stream, and waits for the first
    /// frame. `false` (logged) on failure; the receiver is stopped again then.
    pub async fn start(&mut self) -> bool {
        if let Err(e) = self.setup().await {
            fail("sensor set-up", &e);
            return false;
        }
        if let Err(e) = self.csi.start() {
            fail("receiver start", &e);
            return false;
        }
        Timer::after_millis(1).await;
        if let Err(e) = self.sensor.start().await {
            fail("stream on", &e);
        }
        match with_timeout(FIRST_FRAME_TIMEOUT, self.csi.wait_frame(None)).await {
            Ok(Ok(_)) => return true,
            Ok(Err(e)) => fail("first frame", &e),
            Err(_) => fail(
                "no first frame within (ms)",
                &FIRST_FRAME_TIMEOUT.as_millis(),
            ),
        }
        self.stop().await;
        false
    }

    async fn setup(&mut self) -> Result<(), SensorError> {
        self.sensor.configure(&MODE, &mut Delay).await?;
        self.sensor
            .set_exposure(Exposure::lines(
                EXPOSURE_LINES.min(MODE.max_exposure_lines()),
            ))
            .await?;
        self.sensor.set_gain(GAIN).await
    }

    /// Stream off, let the frame finish, receiver off.
    pub async fn stop(&mut self) {
        if let Err(e) = self.sensor.stop().await {
            fail("stream off", &e);
        }
        Timer::after_millis(20).await;
        self.csi.stop();
    }

    /// Gives `previous` back and waits (up to `timeout`) for the newest completed frame.
    pub async fn next(&mut self, previous: Option<Frame>, timeout: Duration) -> Option<Frame> {
        match with_timeout(timeout, self.csi.wait_frame(previous)).await {
            Ok(Ok(f)) => Some(f),
            _ => None,
        }
    }

    pub fn stats(&self) -> Stats {
        self.csi.stats()
    }

    pub fn reset_stats(&mut self) {
        self.csi.reset_stats();
    }

    /// Bytes per frame.
    pub const fn frame_bytes() -> usize {
        csi_config().frame_bytes()
    }
}
