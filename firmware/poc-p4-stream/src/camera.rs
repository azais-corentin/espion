//! The OV9281 and the CSI-2 receiver, set up as `poc-p4-eth` does: SCCB on I2C0 (GPIO7 SDA,
//! GPIO8 SCL, 100 kHz), the D-PHY supply from LDO VO3 at 2.5 V, the sensor free-running. One
//! receiver at a time (`esp32p4-csi`), chosen at each start:
//!
//! - **Whole frames** ([`Csi`]): into [`BUFFERS`] buffers at the start of PSRAM, each sized
//!   for the largest mode (1280x800 RAW10). The bench's receiver, and the stream's for the
//!   modes the strip path cannot run.
//! - **Strips** ([`CsiStrips`], split; the stream only, once [`Camera::enable_strips`] gave it
//!   core 1 and its frame buffers, and while [`Camera::set_strips_allowed`] says so): each
//!   frame as strips into a ring in internal RAM ([`RING_BYTES`]), the reader on core 1
//!   (`espion-core1`'s stream job), which copies each strip into a frame buffer in PSRAM. Each
//!   mode cuts the ring into the strips [`strip_plan`] chooses. The receiver and core 1's
//!   reader are made afresh at each start.
//!
//! [`Camera::start`] runs any mode of the `ov9281` crate: sensor registers and controls, receiver
//! geometry, receiver start, the lanes checked in their stop state, stream on, the first frame.
//! [`Camera::stop`] is stream off, the frame in flight let out, receiver off (strips: core 1
//! drops what it holds, a bounded wait).

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_time::{Delay, Duration, Instant, Timer, with_timeout};
use esp_hal::gpio::AnyPin;
use esp_hal::i2c::master::{Config as I2cConfig, Error as I2cError, I2c};
use esp_hal::peripherals::{I2C0, VDMA_CH0};
use esp_hal::time::Rate;
use esp32p4_csi::{
    BURST_BYTES, Config, Csi, CsiStrips, Frame, LdoChannel, MAX_STRIPS, Stats, StripControl,
    StripLink, enable_ldo,
};
use espion_core1::{Core1, FrameBuffers, StreamRun};
use log::{error, info};
use ov9281::Format::Raw8;
use ov9281::Readout::Crop;
use ov9281::{
    Address, DeviceError, Exposure, Gain, Mode, Ov9281, Rate as FrameRate, TestPattern, Tolerance,
    limits,
};
use static_cell::{ConstStaticCell, StaticCell};

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
/// Frame buffers the whole-frame receiver rotates: one held by the sender (or two by the
/// encode stage), one being written, one ready.
pub const BUFFERS: usize = 4;
/// Bytes per buffer: the largest frame, 1280x800 RAW10 (a multiple of the 128-byte cache-line
/// alignment the driver wants).
pub const BUFFER_LEN: usize = Csi::buffer_len(&Config::raw10(1280, 800, limits::LANE_MBPS));
/// Bytes of the strip ring in internal RAM: three 64-row strips of 1280-wide RAW10 (the
/// largest strip mode); every strip mode cuts it into its own strips ([`strip_plan`]).
pub const RING_BYTES: usize = 3 * 64 * 1600;
/// The shortest strip [`strip_plan`] chooses, in sensor time: core 1's fixed work per strip
/// (poll, invalidate, copy start, release; about 20 us) stays a small part of it.
const MIN_STRIP_NS: u64 = 200_000;
/// Fewest strips in the ring: a strip may be held for (count - 1) strip periods.
const MIN_STRIPS: usize = 3;
/// How long [`Camera::stop`] waits for core 1 to drop what its stream job holds.
const CORE1_ABORT: Duration = Duration::from_millis(20);
/// How long a start waits for core 1 to take the previous strip reader.
const CORE1_MAILBOX: Duration = Duration::from_millis(10);
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
/// Feature `markers`: OUTPUT_ENABLE6 (0x3006) with the STROBE pad's output on (bit 3) beside
/// ILPWM (bit 2, the mode's 0x04), the one write Linux's `ov9282` driver makes for
/// `V4L2_CID_FLASH_STROBE_OE`. The mode already has the rest of that driver's strobe set-up
/// (0x3920 = 0xFF); it does not write the pulse width (0x3925 to 0x3928, left at their reset
/// values). Written in standby, after the mode. README, "Marker pins".
#[cfg(feature = "markers")]
const STROBE_ON: (u16, u8) = (0x3006, 0x0C);

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
    /// The whole-frame receiver refused the configuration or the start.
    Receiver(esp32p4_csi::Error),
    /// The strip receiver refused the configuration or the start, or core 1 did not take its
    /// reader.
    Strips(Option<esp32p4_csi::StripError>),
    /// The lanes were not all in their stop state before stream on.
    LanesNotStopped,
    /// No frame within two frame periods plus [`FIRST_FRAME_MARGIN`] of stream on.
    NoFirstFrame,
}

/// How the strip path cuts a mode's frames ([`strip_plan`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripPlan {
    /// Rows per strip (the last strip of a frame is shorter).
    pub lines: u16,
    /// Strips in the ring.
    pub count: u8,
    /// Bytes per strip.
    pub bytes: usize,
}

/// The strip path for `mode`, or `None` if it cannot run it. The strip's rows are a multiple of
/// the receiver's step (whole bridge bursts), below the frame's height, not a divisor of it (the
/// short last strip marks the frame's end), and last at least [`MIN_STRIP_NS`] of sensor time;
/// the shortest such strip is taken, and the ring holds as many as fit [`RING_BYTES`] (at most
/// `MAX_STRIPS`, at least [`MIN_STRIPS`]). Cannot run: 128x2 (no row count below the height),
/// RAW10 320x200 and 1280x64 (the step is not below the height), RAW10 1216x704 (one strip of
/// the step, 380 KiB, does not fit the ring).
pub const fn strip_plan(mode: &Mode) -> Option<StripPlan> {
    let cfg = csi_config(mode);
    let line = cfg.line_bytes();
    let height = mode.height();
    let step = CsiStrips::line_step(&cfg);
    let line_ns = mode.line_ns() as u64;
    let mut lines = step;
    while lines < height {
        let bytes = line * lines as usize;
        if bytes == 0 || RING_BYTES / bytes < MIN_STRIPS {
            return None;
        }
        if !height.is_multiple_of(lines)
            && bytes / BURST_BYTES <= 0x1FFF
            && lines as u64 * line_ns >= MIN_STRIP_NS
        {
            let fit = RING_BYTES / bytes;
            let count = if fit < MAX_STRIPS { fit } else { MAX_STRIPS };
            return Some(StripPlan {
                lines,
                count: count as u8,
                bytes,
            });
        }
        lines += step;
    }
    None
}

#[repr(C, align(64))]
struct Ring([u8; RING_BYTES]);

static RING: ConstStaticCell<Ring> = ConstStaticCell::new(Ring([0; RING_BYTES]));
static LINKS: ConstStaticCell<[StripLink; MAX_STRIPS]> =
    ConstStaticCell::new([StripLink::NEW; MAX_STRIPS]);

/// Internal RAM the strip ring and its list items take.
pub const RING_RAM_BYTES: usize = RING_BYTES + MAX_STRIPS * core::mem::size_of::<StripLink>();

/// The receiver in use.
enum Rx {
    Frames(Csi<'static>),
    Strips(StripControl<'static>),
    /// Between dropping one and making the other.
    None,
}

/// What the strip path needs: core 1, the ring, the frame buffers.
struct StripPath {
    core1: &'static mut Core1,
    /// The ring's and the list items' addresses (statics taken once; each receiver gets them
    /// anew once the one before is dropped).
    ring: usize,
    links: usize,
    frames: FrameBuffers,
    /// Strips may be used (core 1 healthy).
    allowed: bool,
    /// L1 data autoload over each strip core 1's detector reads, for the next start.
    autoload: bool,
    /// The run number of the strips started last (core 1's results carry it).
    run: u32,
    /// The plan of the mode running on strips.
    plan: Option<StripPlan>,
}

/// The sensor and the receiver.
pub struct Camera {
    sensor: Sensor,
    rx: Rx,
    /// The whole-frame receiver's buffers' start ([`BUFFERS`] x [`BUFFER_LEN`]), to make the
    /// receiver again after the strip path ran.
    frames: usize,
    strips: Option<StripPath>,
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

/// Finds the sensor, turns on the D-PHY supply and creates the whole-frame receiver on the
/// first `BUFFERS * BUFFER_LEN` bytes of `psram` (cache-line aligned). `None` (logged) on
/// failure.
pub async fn init(
    i2c0: I2C0<'static>,
    vdma: VDMA_CH0<'static>,
    psram: &'static mut [u8],
) -> Option<&'static mut Camera> {
    let (sensor, address) = sensor(i2c0).await?;
    make_camera(vdma, psram, sensor, address)
}

/// Finds the sensor on SCCB and turns on the D-PHY supply. `None` (logged) on failure.
pub async fn sensor(i2c0: I2C0<'static>) -> Option<(Sensor, Address)> {
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
    Some((Ov9281::new(Bus { i2c }, address), address))
}

/// Writes `mode` (software reset, registers, standby) and the controls `c` (with feature
/// `markers`, the STROBE output on).
pub async fn setup(sensor: &mut Sensor, mode: &Mode, c: &Controls) -> Result<(), SensorError> {
    sensor.configure(mode, &mut Delay).await?;
    #[cfg(feature = "markers")]
    sensor.write_register(STROBE_ON.0, STROBE_ON.1).await?;
    set_controls(sensor, mode, c).await
}

/// Exposure (clamped to `mode`'s range), gain and test pattern, while streaming or not.
pub async fn set_controls(
    sensor: &mut Sensor,
    mode: &Mode,
    c: &Controls,
) -> Result<(), SensorError> {
    let lines = c
        .exposure_lines
        .clamp(limits::EXPOSURE_MIN, mode.max_exposure_lines());
    sensor.set_exposure(Exposure::lines(lines)).await?;
    sensor.set_gain(c.gain).await?;
    sensor.set_test_pattern(c.pattern).await
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

/// The whole-frame receiver on the [`BUFFERS`] buffers from `base`.
///
/// # Safety
///
/// `base..base + BUFFERS * BUFFER_LEN` is PSRAM given to the camera for good, and no other
/// receiver exists (nor any frame of an earlier whole-frame receiver that is still read).
unsafe fn make_csi(base: usize, config: Config) -> Result<Csi<'static>, esp32p4_csi::Error> {
    // SAFETY: the caller's contract; each buffer is a distinct part of that range.
    let buf = |i: usize| unsafe {
        core::slice::from_raw_parts_mut((base + i * BUFFER_LEN) as *mut u8, BUFFER_LEN)
    };
    // SAFETY: VDMA channel 0 is the receiver's, and only one receiver exists at a time.
    let vdma = unsafe { VDMA_CH0::steal() };
    let csi = Csi::new(vdma, [buf(0), buf(1), buf(2), buf(3)], config)?;
    crate::irq::install(&[
        crate::irq::Source::CsiDma,
        crate::irq::Source::CsiHost,
        crate::irq::Source::CsiBridge,
    ]);
    Ok(csi)
}

/// A strip receiver on the ring cut for `plan`, its reader handed to core 1 (which took the
/// one before) with the stream job's parameters, started. The receiver before is dropped.
#[allow(
    clippy::large_stack_frames,
    reason = "once per strip start, on the main stack: the strip receiver is built here by value \
    and split"
)]
#[inline(never)]
fn make_strips(
    p: &mut StripPath,
    mode: &Mode,
    plan: StripPlan,
) -> Result<StripControl<'static>, StartError> {
    let config = csi_config(mode);
    let count = usize::from(plan.count);
    let mut strips: [&'static mut [u8]; MAX_STRIPS] = Default::default();
    for (i, s) in strips[..count].iter_mut().enumerate() {
        // SAFETY: distinct parts of the ring, taken for good by `enable_strips`; the receiver
        // that had them is dropped.
        *s = unsafe {
            core::slice::from_raw_parts_mut((p.ring + i * plan.bytes) as *mut u8, plan.bytes)
        };
    }
    // SAFETY: as above, the list items.
    let links = unsafe { core::slice::from_raw_parts_mut(p.links as *mut StripLink, count) };
    // SAFETY: VDMA channel 0 is the receiver's, and only one receiver exists at a time.
    let vdma = unsafe { VDMA_CH0::steal() };
    let cs = CsiStrips::from_slices(vdma, &mut strips[..count], links, config, plan.lines)
        .map_err(|e| StartError::Strips(Some(e)))?;
    crate::irq::install(&[
        crate::irq::Source::CsiDma,
        crate::irq::Source::CsiHost,
        crate::irq::Source::CsiBridge,
    ]);
    let mut control = p.core1.hand_over(cs).ok_or(StartError::Strips(None))?;
    p.run = p.run.wrapping_add(1);
    let mhz = esp_hal::clock::cpu_clock().as_mhz();
    let strip_ns = u64::from(plan.lines) * u64::from(mode.line_ns());
    p.core1.set_stream(&StreamRun {
        width: mode.width(),
        height: mode.height(),
        raw10: mode.format() == ov9281::Format::Raw10,
        line_bytes: config.line_bytes() as u32,
        strip_lines: plan.lines,
        frames: FrameBuffers {
            frame_bytes: config.frame_bytes(),
            ..p.frames
        },
        late_cycles: (strip_ns * u64::from(mhz) / 1000) as u32,
        // A strip is safe for (strips - 1) periods after the DMA wrote it (`esp32p4-csi`
        // README, "A reader that falls behind"); seven eighths of that for the detector, the
        // rest for the copy's end and the release.
        budget_cycles: (u64::from(plan.count.saturating_sub(1)) * strip_ns * 7 / 8 * u64::from(mhz)
            / 1000)
            .min(u64::from(u32::MAX)) as u32,
        autoload: p.autoload,
        run: p.run,
    });
    p.plan = Some(plan);
    control.start().map_err(|e| StartError::Strips(Some(e)))?;
    Ok(control)
}

#[inline(never)]
fn make_camera(
    // The channel is stolen again for each receiver (one at a time); taken here as the proof
    // that the camera owns it.
    _vdma: VDMA_CH0<'static>,
    psram: &'static mut [u8],
    sensor: Sensor,
    address: Address,
) -> Option<&'static mut Camera> {
    if psram.len() < BUFFERS * BUFFER_LEN {
        fail("PSRAM for the frame buffers", &psram.len());
        return None;
    }
    let base = psram.as_mut_ptr() as usize;
    // SAFETY: the range was given for good; no receiver exists yet.
    let csi = match unsafe { make_csi(base, csi_config(&MODE)) } {
        Ok(c) => c,
        Err(e) => {
            fail("Csi::new", &e);
            return None;
        }
    };
    log_ready(address);
    Some(CAMERA.init(Camera {
        sensor,
        rx: Rx::Frames(csi),
        frames: base,
        strips: None,
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
    /// Gives the camera the strip path: core 1 (its stream job reads the strips) and the frame
    /// buffers its copies fill. Once; the strip ring is taken here.
    pub fn enable_strips(&mut self, core1: &'static mut Core1, frames: FrameBuffers) {
        let (Some(ring), Some(links)) = (RING.try_take(), LINKS.try_take()) else {
            return;
        };
        self.strips = Some(StripPath {
            core1,
            ring: ring.0.as_mut_ptr() as usize,
            links: links.as_mut_ptr() as usize,
            frames,
            allowed: true,
            autoload: false,
            run: 0,
            plan: None,
        });
    }

    /// Whether the next starts may use strips (and core 1's L1 data autoload over each strip),
    /// for the modes [`strip_plan`] accepts.
    pub fn set_strips_allowed(&mut self, allowed: bool, autoload: bool) {
        if let Some(p) = &mut self.strips {
            p.allowed = allowed;
            p.autoload = autoload;
        }
    }

    /// The strip plan a start of `mode` would use (`None`: whole frames).
    pub fn plan_for(&self, mode: &Mode) -> Option<StripPlan> {
        self.strips
            .as_ref()
            .filter(|p| p.allowed)
            .and_then(|_| strip_plan(mode))
    }

    /// The whole-frame receiver exists now.
    pub fn has_frame_receiver(&self) -> bool {
        matches!(self.rx, Rx::Frames(_))
    }

    /// Writes `mode` and `controls`, configures and starts the receiver (strips if
    /// [`Camera::plan_for`] has a plan, else whole frames), checks the lanes are in their stop
    /// state, turns the sensor stream on and waits for the first frame (dropped; strips: the
    /// first frame end). Stops first if a mode runs. On failure the receiver is stopped again.
    /// The receiver's counters ([`Camera::stats`]) start from zero. `fault` is injected into
    /// this start only.
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
        match self.plan_for(mode) {
            Some(plan) => self.start_strips(mode, plan).await?,
            None => self.start_frames(mode)?,
        }
        if !self.lanes_stopped().await {
            self.stop_receiver().await;
            return Err(StartError::LanesNotStopped);
        }
        if fault != Some(Fault::NoStream)
            && let Err(e) = self.sensor.start().await
        {
            self.stop_receiver().await;
            return Err(StartError::Sensor(e));
        }
        self.running = Some(*mode);
        if self.first_frame(mode).await {
            Ok(())
        } else {
            let _ = self.stop().await;
            Err(StartError::NoFirstFrame)
        }
    }

    /// The whole-frame receiver, configured for `mode` and started (made again if the strips
    /// ran last).
    fn start_frames(&mut self, mode: &Mode) -> Result<(), StartError> {
        let config = csi_config(mode);
        if !matches!(self.rx, Rx::Frames(_)) {
            // The strip receiver first: one receiver at a time.
            self.rx = Rx::None;
            // SAFETY: the range was given for good; the strip receiver is dropped, and the
            // stream gives back every frame of the earlier whole-frame receiver before the
            // strips start (`stream::frames`).
            self.rx =
                Rx::Frames(unsafe { make_csi(self.frames, config) }.map_err(StartError::Receiver)?);
        }
        if let Some(p) = &mut self.strips {
            p.plan = None;
        }
        let Rx::Frames(csi) = &mut self.rx else {
            return Err(StartError::Receiver(esp32p4_csi::Error::NotRunning));
        };
        csi.set_config(config).map_err(StartError::Receiver)?;
        csi.reset_stats();
        csi.start().map_err(StartError::Receiver)
    }

    /// A new strip receiver on the ring cut for `plan`, its reader handed to core 1 with the
    /// stream job's parameters, started.
    async fn start_strips(&mut self, mode: &Mode, plan: StripPlan) -> Result<(), StartError> {
        // The receiver before goes first (a strip receiver's drop detaches its reader).
        self.rx = Rx::None;
        let Some(p) = &mut self.strips else {
            return Err(StartError::Strips(None));
        };
        if !p.core1.wait_mailbox(CORE1_MAILBOX).await {
            return Err(StartError::Strips(None));
        }
        self.rx = Rx::Strips(make_strips(p, mode, plan)?);
        Ok(())
    }

    /// Waits for the first frame of the mode just started: whole frames, a frame (dropped);
    /// strips, a frame end counted (serviced every millisecond meanwhile).
    async fn first_frame(&mut self, mode: &Mode) -> bool {
        let timeout = frame_timeout(mode);
        match &mut self.rx {
            Rx::Frames(csi) => {
                matches!(with_timeout(timeout, csi.wait_frame(None)).await, Ok(Ok(_)))
            }
            Rx::Strips(control) => {
                let end = Instant::now() + timeout;
                while Instant::now() < end {
                    Timer::after_millis(1).await;
                    control.service();
                    if control.stats().frames_done > 0 {
                        return true;
                    }
                }
                false
            }
            Rx::None => false,
        }
    }

    async fn setup(&mut self, mode: &Mode, c: &Controls) -> Result<(), SensorError> {
        setup(&mut self.sensor, mode, c).await
    }

    /// Exposure (clamped to `mode`'s range), gain and test pattern, while streaming or not.
    pub async fn set_controls(&mut self, mode: &Mode, c: &Controls) -> Result<(), SensorError> {
        set_controls(&mut self.sensor, mode, c).await
    }

    /// Polls the D-PHY until the clock and both data lanes are in their stop state (LP-11),
    /// at most [`LANES_STOP_TIMEOUT`].
    async fn lanes_stopped(&mut self) -> bool {
        for _ in 0..LANES_STOP_TIMEOUT.as_millis() * 4 {
            let phy = match &self.rx {
                Rx::Frames(csi) => csi.phy_status(),
                Rx::Strips(control) => control.phy_status(),
                Rx::None => return false,
            };
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
        self.stop_receiver().await;
        self.running = None;
        r
    }

    /// The receiver off; on strips also core 1's stream job: it drops the frame it is
    /// assembling and the one waiting for its measure, with their buffers, once its copy in
    /// flight ended (at most [`CORE1_ABORT`]; a core 1 that does not answer is left to the
    /// heartbeat watch).
    async fn stop_receiver(&mut self) {
        match &mut self.rx {
            Rx::Frames(csi) => csi.stop(),
            Rx::Strips(control) => {
                control.stop();
                if let Some(p) = &mut self.strips {
                    p.core1.note_stop();
                    let req = p.core1.abort_stream();
                    let end = Instant::now() + CORE1_ABORT;
                    while !p.core1.stream_idle(req) && Instant::now() < end {
                        Timer::after_micros(100).await;
                    }
                }
            }
            Rx::None => {}
        }
    }

    /// Sensor stream off behind the receiver's back (the stream service's stall test): frames
    /// stop, the receiver stays started.
    pub async fn stall(&mut self) -> Result<(), SensorError> {
        self.sensor.stop().await
    }

    /// Whole frames: gives `previous` back and waits (up to `timeout`) for the newest
    /// completed frame. `None` on strips.
    pub async fn next(&mut self, previous: Option<Frame>, timeout: Duration) -> Option<Frame> {
        let Rx::Frames(csi) = &mut self.rx else {
            return None;
        };
        match with_timeout(timeout, csi.wait_frame(previous)).await {
            Ok(Ok(f)) => Some(f),
            _ => None,
        }
    }

    /// Whole frames: waits for the newest completed frame; `Err` if the receiver is (or gets)
    /// stopped, or runs strips.
    pub async fn wait_frame(&mut self) -> Result<Frame, esp32p4_csi::Error> {
        match &mut self.rx {
            Rx::Frames(csi) => csi.wait_frame(None).await,
            _ => Err(esp32p4_csi::Error::NotRunning),
        }
    }

    /// The mode running, if any.
    pub const fn running(&self) -> Option<&Mode> {
        self.running.as_ref()
    }

    /// The strip path runs the mode: its plan, and the run number core 1's results carry.
    pub fn strips_running(&self) -> Option<(StripPlan, u32)> {
        match (&self.rx, &self.strips, self.running) {
            (Rx::Strips(_), Some(p), Some(_)) => p.plan.map(|plan| (plan, p.run)),
            _ => None,
        }
    }

    /// The strip receiver's control half (strips only): [`StripControl::service`] every
    /// millisecond, its counters.
    pub fn strip_control(&mut self) -> Option<&mut StripControl<'static>> {
        match &mut self.rx {
            Rx::Strips(c) => Some(c),
            _ => None,
        }
    }

    /// Core 1, once [`Camera::enable_strips`] gave it.
    pub fn core1(&mut self) -> Option<&mut Core1> {
        self.strips.as_mut().map(|p| &mut *p.core1)
    }

    /// The receiver's counters. On strips: frames done (frame ends), the DMA's errors and
    /// stalls as `dma_errors`, the host's and the bridge's errors, and as `frames_dropped` the
    /// frames lost to the strips (lapped strips: core 1 too late; blocks of the wrong length).
    pub fn stats(&self) -> Stats {
        match &self.rx {
            Rx::Frames(csi) => csi.stats(),
            Rx::Strips(control) => {
                let s = control.stats();
                Stats {
                    frames_done: s.frames_done,
                    frames_dropped: control.strips_lapped().wrapping_add(s.length_errors),
                    frames_overwritten: 0,
                    last_frame_bytes: 0,
                    last_block_items: s.last_block_items,
                    dma_errors: s.dma_errors.wrapping_add(s.dma_stalls),
                    dma_error_bits: s.dma_error_bits,
                    host_errors: s.host_errors,
                    host_error_main: s.host_error_main,
                    host_error_detail: s.host_error_detail,
                    bridge_errors: s.bridge_errors,
                    bridge_error_bits: s.bridge_error_bits,
                }
            }
            Rx::None => Stats::default(),
        }
    }

    pub fn reset_stats(&mut self) {
        match &mut self.rx {
            Rx::Frames(csi) => csi.reset_stats(),
            Rx::Strips(control) => control.reset_stats(),
            Rx::None => {}
        }
    }

    /// Bytes per frame of the bench's [`MODE`].
    pub const fn frame_bytes() -> usize {
        csi_config(&MODE).frame_bytes()
    }
}
