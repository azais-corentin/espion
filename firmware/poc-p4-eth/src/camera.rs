//! OV9281 capture over MIPI CSI-2 with `esp32p4-csi`: sensor set-up through the `ov9281`
//! crate, receiver start, frame rate and error counters per mode, cache-invalidation timing
//! and coherence check, and an optional still dump over the log UART (decoded on the host by
//! `tools/still.py`).
//!
//! The async functions keep their state small (the crate denies `clippy::large_stack_frames`):
//! every log line is formatted in a separate `#[inline(never)]` function.

use embassy_time::{Delay, Duration, Instant as EInstant, Timer, with_timeout};
use esp_hal::i2c::master::{Error as I2cError, I2c};
use esp_hal::peripherals::VDMA_CH0;
use esp_println::println;
use esp32p4_cache as cache;
use esp32p4_csi::{
    Config, Csi, CsiStrips, Frame, LdoChannel, LdoSetting, PhyStatus, StripError, enable_ldo,
};
use espion_vision::{Blob, DefaultDetector, synth};
use log::{error, info, warn};
use ov9281::{DeviceError, Exposure, Format, Gain, Mode, Ov9281, Readout, TestPattern, limits};
use static_cell::StaticCell;

use crate::modes::{self, CamMode};
use crate::{DarkSkip, jpeg_bench, requests, soak, strip_soak};

/// The sensor on the camera's I2C bus.
pub type Cam = Ov9281<I2c<'static, esp_hal::Async>>;
/// Error of a sensor call.
pub type SensorError = DeviceError<I2cError>;

/// The sensor modes, as a promoted `&'static` slice: indexing the `const` array itself would
/// copy all of it onto the stack (or into the future) first.
const MODES: &[CamMode] = &modes::MODES;
/// Frame buffers the driver rotates.
const BUFFERS: usize = 3;
/// Every buffer is sized for the largest configuration: 1280x800 RAW10 (1 286 144 B).
const BUFFER_LEN: usize = Csi::buffer_len(&Config::raw10(1280, 800, limits::LANE_MBPS));
/// How long to wait for a frame beyond two frame periods of the mode ([`frame_timeout`])
/// before printing the debugging checklist.
const FRAME_TIMEOUT: Duration = Duration::from_millis(500);
/// Fill value of the frame buffers before the first capture.
const SENTINEL: u8 = 0x5A;
/// MIPI_CTRL00 (0x4800) with the clock lane gated between packets (bit 5; bit 6 is reserved in
/// the datasheet but set by Espressif), for [`Plan::clock_gated`]. The mode's own value is
/// 0x00, a continuous clock. Written in standby, after the mode.
const MIPI_CLOCK_GATED: (u16, u8) = (0x4800, 0x60);

/// What one run of the camera section does.
pub struct Plan {
    /// Indexes into [`modes::MODES`], in order.
    pub modes: &'static [usize],
    /// With entries: the request verification ([`requests`]) instead of `modes`, on the
    /// whole-frame path.
    pub requests: &'static [requests::Request],
    /// Sensor settings after each mode's registers.
    pub sensor: Sensor,
    /// Frame-rate measurement window per mode.
    pub measure: Duration,
    /// Time the cache invalidation and run the cache coherence check per mode.
    pub cache_check: bool,
    /// Dump one frame per mode over the log UART.
    pub still_dump: bool,
    /// With `still_dump`: one more still per test pattern, selected while streaming. The
    /// plan's own setting (`sensor.pattern`) is restored afterwards.
    pub still_patterns: &'static [TestPattern],
    /// Run the processing soak per mode after the checks above (`soak`). `None`: capture only.
    pub soak: Option<soak::Plan>,
    /// Run the strip path instead (`strip_soak`: the strip soak, or with `check` the strip
    /// check, whose references alone use a whole-frame receiver), and `measure`,
    /// `cache_check`, `still_dump` and `soak` are ignored.
    pub strips: Option<strip_soak::Plan>,
    /// With a plan: the JPEG encoder test and benchmark ([`jpeg_bench`]) instead of `modes`,
    /// on the whole-frame path, with `sensor` for its camera runs.
    pub jpeg: Option<jpeg_bench::Plan>,
}

/// Runs `plan`. `psram` must hold at least `BUFFERS * BUFFER_LEN` bytes (plus one frame
/// of the largest mode for the synthetic frames); its start is cache-line aligned (PSRAM
/// maps at 0x4800_0000). `internal` (internal RAM, cache-line aligned) holds the strip soak's
/// synthetic strip. `STRIPS` must be `plan.strips.is_some()`: a constant, so a build without
/// the strip soak does not link its ring (320 KiB of internal RAM). The frame soak and the
/// strip soak run the detector with `skip`.
pub async fn run<const STRIPS: bool>(
    cam: &mut Cam,
    vdma: VDMA_CH0<'static>,
    psram: &'static mut [u8],
    internal: &'static mut [u8],
    plan: &Plan,
    skip: DarkSkip<'_>,
) {
    if power_up().await {
        match plan.strips {
            Some(strips) => {
                strip_modes::<STRIPS>(cam, vdma, psram, internal, plan, strips, skip).await;
            }
            None => run_frames(cam, vdma, psram, internal, plan, skip).await,
        }
    }
    note("done");
}

// The receiver and the soaks live in statics: kept in the futures they would sit next to the
// SCCB futures (about 380 B each) and push `main`'s frame over the stack limit.
static CSI: StaticCell<Csi<'static>> = StaticCell::new();
static SOAK: StaticCell<soak::Soak> = StaticCell::new();

/// The whole-frame path: one `Csi` for every mode of the plan, then per mode the frame soak
/// or, in a capture-only plan, the detector check; or the request verification.
async fn run_frames(
    cam: &mut Cam,
    vdma: VDMA_CH0<'static>,
    psram: &'static mut [u8],
    internal: &'static mut [u8],
    plan: &Plan,
    skip: DarkSkip<'_>,
) {
    let split = (BUFFERS * BUFFER_LEN).min(psram.len());
    let (buffers, rest) = psram.split_at_mut(split);
    let first = plan.modes.first().map_or(&MODES[0], |&i| &MODES[i]);
    let Some(csi) = make_receiver(vdma, buffers, &first.mode) else {
        return;
    };
    if !plan.requests.is_empty() {
        requests::run(cam, csi, plan).await;
        return;
    }
    if let Some(j) = &plan.jpeg {
        jpeg_bench::run(cam, csi, rest, internal, &plan.sensor, j).await;
        return;
    }
    let (mut soak, mut check) = match plan.soak {
        Some(p) => (make_soak(p, rest), None),
        None => (None, make_check(rest)),
    };
    for &index in plan.modes {
        let mode = &MODES[index];
        let (s, c) = (soak.as_deref_mut(), check.as_mut());
        capture_mode(cam, csi, mode, plan, s, c, skip).await;
    }
    if let Some(s) = &soak {
        s.log_table();
    }
}

/// The soak's synthetic frames go right after the capture buffers (a multiple of 4096 B, so
/// the area starts on a cache line).
#[inline(never)]
fn make_soak(plan: soak::Plan, rest: &'static mut [u8]) -> Option<&'static mut soak::Soak> {
    const _: () = assert!(BUFFER_LEN.is_multiple_of(128));
    let need = Csi::buffer_len(&Config::new(1280, 800, limits::LANE_MBPS));
    if rest.len() < need {
        fail("PSRAM too small for the soak's synthetic frame", &need);
        return None;
    }
    Some(SOAK.init(soak::Soak::new(plan, &mut rest[..need])))
}

/// The detector check's synthetic frame goes where the soak's would (cache-line aligned).
#[inline(never)]
fn make_check(rest: &'static mut [u8]) -> Option<DetectorCheck> {
    if rest.len() < BUFFER_LEN {
        fail(
            "PSRAM too small for the detector check's synthetic frame",
            &BUFFER_LEN,
        );
        return None;
    }
    let (det, out, synth) = soak::take_detector();
    Some(DetectorCheck {
        det,
        out,
        synth,
        area: &mut rest[..BUFFER_LEN],
        mhz: esp_hal::clock::cpu_clock().as_mhz(),
    })
}

/// Turns on LDO VO3 at 2.5 V (`VDD_MIPI_DPHY` on this board) and lets it settle.
async fn power_up() -> bool {
    if !enable_vo3() {
        return false;
    }
    // Settling time for VO3 (esp-hal's DSI driver waits 500 us).
    Timer::after_millis(1).await;
    true
}

#[inline(never)]
fn enable_vo3() -> bool {
    match enable_ldo(LdoChannel::Vo3, 2500) {
        Ok(s) => {
            log_ldo(&s);
            true
        }
        Err(e) => {
            fail("LDO VO3", &e);
            false
        }
    }
}

/// Fills the frame buffers with [`SENTINEL`] and creates the receiver.
#[inline(never)]
fn make_receiver(
    vdma: VDMA_CH0<'static>,
    psram: &'static mut [u8],
    first: &Mode,
) -> Option<&'static mut Csi<'static>> {
    if psram.len() < BUFFERS * BUFFER_LEN {
        fail(
            "PSRAM too small for the frame buffers",
            &(BUFFERS * BUFFER_LEN),
        );
        return None;
    }
    // Sentinel so a tail the DMA did not write shows up in the first frames.
    psram[..BUFFERS * BUFFER_LEN].fill(SENTINEL);
    cache::write_back(psram.as_ptr() as usize, BUFFERS * BUFFER_LEN);
    let (b0, rest) = psram.split_at_mut(BUFFER_LEN);
    let (b1, rest) = rest.split_at_mut(BUFFER_LEN);
    let (b2, _) = rest.split_at_mut(BUFFER_LEN);
    match Csi::new(vdma, [b0, b1, b2], csi_config(first)) {
        Ok(c) => {
            log_ready(&c);
            Some(CSI.init(c))
        }
        Err(e) => {
            fail("Csi::new", &e);
            None
        }
    }
}

/// Receiver configuration for `mode`: its format's data type, its output size, the sensor's
/// lane rate.
pub fn csi_config(mode: &Mode) -> Config {
    Config {
        data_type: mode.format().data_type(),
        ..Config::new(mode.width(), mode.height(), limits::LANE_MBPS)
    }
}

/// How long to wait for a frame of `mode`: [`FRAME_TIMEOUT`] plus two frame periods (a 2 Hz
/// mode's frames are 500 ms apart).
pub fn frame_timeout(mode: &Mode) -> Duration {
    FRAME_TIMEOUT + Duration::from_micros(2 * u64::from(mode.frame_ns()) / 1000)
}

/// The sensor settings applied after a mode's registers.
#[derive(Clone, Copy)]
pub struct Sensor {
    /// Test pattern instead of the scene ([`TestPattern::Off`]: the scene).
    pub pattern: TestPattern,
    /// Clock lane gated between packets ([`MIPI_CLOCK_GATED`]) instead of continuous.
    pub clock_gated: bool,
    /// Exposure in lines, clamped to the mode's longest ([`Mode::max_exposure_lines`]).
    pub exposure_lines: u16,
    /// Analog gain.
    pub gain: Gain,
    /// Turn the power-saving mode off ([`PSV_OFF`]) whatever the mode chose: the request
    /// verification's long-VTS reference, as in the characterisation.
    pub psv_off: bool,
}

impl Sensor {
    /// The exposure the set-up writes for `mode`.
    pub const fn exposure(&self, mode: &Mode) -> u16 {
        let max = mode.max_exposure_lines();
        if self.exposure_lines > max {
            max
        } else if self.exposure_lines < limits::EXPOSURE_MIN {
            limits::EXPOSURE_MIN
        } else {
            self.exposure_lines
        }
    }
}

/// PSV_CTRL (0x4F00) = 0x00: the power-saving mode off, for [`Sensor::psv_off`].
const PSV_OFF: (u16, u8) = (0x4F00, 0x00);

async fn capture_mode(
    cam: &mut Cam,
    csi: &mut Csi<'static>,
    mode: &CamMode,
    plan: &Plan,
    soak: Option<&mut soak::Soak>,
    check: Option<&mut DetectorCheck>,
    skip: DarkSkip<'_>,
) {
    if start_mode(cam, csi, mode, &plan.sensor).await {
        measure(csi, mode, plan.measure).await;
        if plan.cache_check {
            cache_check(csi).await;
        }
        if plan.still_dump {
            stills(
                cam,
                csi,
                ["", ""],
                mode,
                plan.sensor.pattern,
                plan.still_patterns,
            )
            .await;
        }
        if let Some(c) = check {
            detector_check(csi, mode, c).await;
        }
        match soak {
            Some(s) if !mode.raw10() => s.run_mode(csi, mode, skip).await,
            Some(_) => note("soak skipped: RAW8 modes only"),
            None => {}
        }
    }
    stop(cam, csi).await;
}

/// Sensor set-up, receiver start, stream-on and the first frame. `false` if no frame arrived.
pub async fn start_mode(
    cam: &mut Cam,
    csi: &mut Csi<'static>,
    mode: &CamMode,
    sensor: &Sensor,
) -> bool {
    log_mode(mode, sensor);
    if let Err(e) = sensor_setup(cam, &mode.mode, sensor).await {
        fail("sensor set-up", &e);
        return false;
    }
    log_sensor_regs(cam).await;
    if let Err(e) = csi
        .set_config(csi_config(&mode.mode))
        .and_then(|()| csi.start())
    {
        fail("receiver start", &e);
        return false;
    }
    Timer::after_millis(1).await;
    log_phy_before_stream(&csi.phy_status());
    if let Err(e) = cam.start().await {
        fail("stream on", &e);
    }
    first_frame(csi, &mode.mode).await
}

async fn first_frame(csi: &mut Csi<'static>, mode: &Mode) -> bool {
    let timeout = frame_timeout(mode);
    match with_timeout(timeout, csi.wait_frame(None)).await {
        Ok(Ok(f)) => {
            log_first_frame(&f);
            true
        }
        Ok(Err(e)) => {
            fail("wait_frame", &e);
            false
        }
        Err(_) => {
            fail("no frame within (ms)", &timeout.as_millis());
            checklist(csi).await;
            false
        }
    }
}

/// The still of the setting `own`, then one per test pattern of `patterns`, after which `own`
/// is restored. Each still's name starts with the two parts of `prefix`.
pub async fn stills(
    cam: &mut Cam,
    csi: &mut Csi<'static>,
    prefix: [&str; 2],
    mode: &CamMode,
    own: TestPattern,
    patterns: &[TestPattern],
) {
    still(csi, prefix, mode, own).await;
    for &p in patterns {
        if pattern(cam, csi, &mode.mode, p).await {
            still(csi, prefix, mode, p).await;
        }
    }
    if !patterns.is_empty() {
        pattern(cam, csi, &mode.mode, own).await;
    }
}

/// Frames dropped after a test pattern change before a still: the one in flight may change
/// pattern part-way, and the write may only take effect at the next frame start.
const PATTERN_SETTLE_FRAMES: usize = 3;

/// Selects `p` while streaming and lets [`PATTERN_SETTLE_FRAMES`] pass.
async fn pattern(cam: &mut Cam, csi: &mut Csi<'static>, mode: &Mode, p: TestPattern) -> bool {
    if let Err(e) = cam.set_test_pattern(p).await {
        fail("test pattern", &e);
        return false;
    }
    for _ in 0..PATTERN_SETTLE_FRAMES {
        let _ = with_timeout(frame_timeout(mode), csi.wait_frame(None)).await;
    }
    true
}

/// Dumps the next frame; `pattern` is what it was taken with (part of its name).
async fn still(csi: &mut Csi<'static>, prefix: [&str; 2], mode: &CamMode, pattern: TestPattern) {
    match with_timeout(frame_timeout(&mode.mode), csi.wait_frame(None)).await {
        Ok(Ok(f)) => dump_still(prefix, mode, pattern, &f),
        _ => note("no frame for the still"),
    }
}

/// Stream off, then the receiver.
pub async fn stop(cam: &mut Cam, csi: &mut Csi<'static>) {
    if let Err(e) = cam.stop().await {
        fail("stream off", &e);
    }
    // Let the current frame finish so the lanes go back to LP-11.
    Timer::after_millis(20).await;
    csi.stop();
}

/// The detector on the pixels of a capture-only mode: borrowed from the soak (which does not
/// run in such a plan), plus a PSRAM area for one synthetic frame of the largest mode.
struct DetectorCheck {
    det: &'static mut DefaultDetector,
    out: &'static mut [Blob; 256],
    synth: &'static mut synth::Synth,
    area: &'static mut [u8],
    mhz: u32,
}

/// Detector threshold of the check on RAW8 frames.
const CHECK_THRESHOLD_RAW8: u8 = 128;
/// Detector threshold of the check on RAW10 frames, in 10-bit units (the same level).
const CHECK_THRESHOLD_RAW10: u16 = 512;
/// Discs in the check's synthetic RAW10 frame, and its seed.
const CHECK_DISCS: u16 = 16;
const CHECK_SEED: u32 = 0x5EED_0010;

/// One detector call of the check.
struct Detected {
    blobs: usize,
    overflow: u32,
    cycles: u32,
}

/// After the measurement window: the detector once over a captured frame, held while it
/// runs (the DMA keeps filling the other buffers), and for a RAW10 mode once more over a
/// synthetic frame of the mode's size in PSRAM with [`CHECK_DISCS`] discs, written back from
/// the cache first so it is read from PSRAM like a captured one. Logs after each call.
async fn detector_check(csi: &mut Csi<'static>, mode: &CamMode, c: &mut DetectorCheck) {
    let Ok(Ok(f)) = with_timeout(frame_timeout(&mode.mode), csi.wait_frame(None)).await else {
        note("no frame for the detector check");
        return;
    };
    let r = detect_frame(c.det, c.out, &f, &mode.mode);
    drop(f);
    log_detected(mode, "captured frame", &r, c.mhz);
    if mode.raw10() {
        let area = &mut c.area[..mode.mode.frame_bytes() as usize];
        let (w, h) = (
            usize::from(mode.mode.width()),
            usize::from(mode.mode.height()),
        );
        c.synth.fill_raw10(area, w, h, CHECK_DISCS, CHECK_SEED);
        crate::flush_cache(area);
        let r = detect_frame(c.det, c.out, area, &mode.mode);
        log_synthetic();
        log_detected(mode, "synthetic frame", &r, c.mhz);
    }
}

/// `process_frame` (RAW8) or `process_frame_raw10` over `frame`, timed with the cycle counter.
#[inline(never)]
fn detect_frame(
    det: &mut DefaultDetector,
    out: &mut [Blob],
    frame: &[u8],
    mode: &Mode,
) -> Detected {
    let t0 = crate::mcycle();
    let blobs = match mode.format() {
        Format::Raw10 => det.process_frame_raw10(frame, mode.width(), CHECK_THRESHOLD_RAW10, out),
        Format::Raw8 => det.process_frame(frame, mode.width(), CHECK_THRESHOLD_RAW8, out),
    };
    let cycles = crate::mcycle().wrapping_sub(t0);
    Detected {
        blobs,
        overflow: det.stats().overflow,
        cycles,
    }
}

#[inline(never)]
fn log_synthetic() {
    info!(
        "camera: synthetic RAW10 frame: {CHECK_DISCS} discs (peak {}), background below {} (10-bit), seed {CHECK_SEED:#x}",
        synth::PEAK_RAW10,
        synth::NOISE_MAX_RAW10
    );
}

#[inline(never)]
fn log_detected(mode: &CamMode, what: &str, r: &Detected, mhz: u32) {
    info!(
        "camera: {}: detector on {what} ({} B, RAW{}, threshold {}): {} blobs, overflow {}, {} us ({} cycles), frame period {} us",
        mode.name,
        mode.mode.frame_bytes(),
        mode.mode.format().bits_per_pixel(),
        if mode.raw10() {
            CHECK_THRESHOLD_RAW10
        } else {
            CHECK_THRESHOLD_RAW8 as u16
        },
        r.blobs,
        r.overflow,
        r.cycles / mhz,
        r.cycles,
        mode.mode.frame_ns() / 1000
    );
}

/// Frame counts and sizes over one measurement window.
struct Measurement {
    expected: usize,
    window_us: u64,
    delivered: u32,
    wrong_size: u32,
    first_wrong: usize,
    /// First and last delivered frame: arrival time (us since `t0`) and sequence number.
    first: (u32, u32),
    last: (u32, u32),
    /// Longest time between two consecutive deliveries (us), and sequence-number steps other
    /// than 1 between them.
    max_gap_us: u32,
    seq_gaps: u32,
}

/// Counts frames for `window` and checks every frame's size.
pub async fn measure(csi: &mut Csi<'static>, mode: &CamMode, window: Duration) {
    let timeout = frame_timeout(&mode.mode);
    // Drop what completed while the previous log lines went out (esp-println holds a critical
    // section per line, about 0.5 ms at 2 Mbaud, so the swap interrupt can be late and a frame
    // cut short; at 453 fps a frame lasts 2.2 ms). Nothing logs from here to the end of the
    // window.
    Timer::after_millis(20).await;
    let _ = with_timeout(timeout, csi.wait_frame(None)).await;
    csi.reset_stats();
    let t0 = EInstant::now();
    let mut m = Measurement {
        expected: csi_config(&mode.mode).frame_bytes(),
        window_us: 0,
        delivered: 0,
        wrong_size: 0,
        first_wrong: 0,
        first: (0, 0),
        last: (0, 0),
        max_gap_us: 0,
        seq_gaps: 0,
    };
    let mut frame: Option<Frame> = None;
    while t0.elapsed() < window {
        match with_timeout(timeout, csi.wait_frame(frame.take())).await {
            Ok(Ok(f)) => {
                m.arrived(&f, t0.elapsed().as_micros() as u32);
                frame = Some(f);
            }
            Ok(Err(e)) => {
                fail("wait_frame", &e);
                break;
            }
            Err(_) => {
                note("frames stopped during the measurement");
                checklist(csi).await;
                break;
            }
        }
    }
    drop(frame);
    m.window_us = t0.elapsed().as_micros().max(1);
    log_measurement(csi, mode, &m);
    log_errors(csi);
}

impl Measurement {
    /// Frame `f` delivered `at_us` after the window started.
    #[inline(never)]
    fn arrived(&mut self, f: &Frame, at_us: u32) {
        let at = (at_us, f.seq());
        if self.delivered == 0 {
            self.first = at;
        } else {
            self.max_gap_us = self.max_gap_us.max(at.0 - self.last.0);
            if at.1 != self.last.1.wrapping_add(1) {
                self.seq_gaps += 1;
            }
        }
        self.last = at;
        self.delivered += 1;
        if f.received_len() != self.expected {
            if self.wrong_size == 0 {
                self.first_wrong = f.received_len();
            }
            self.wrong_size += 1;
        }
    }
}

/// The driver's "no frames arrive" checklist (`poc-csi.md`), in order, plus a second register
/// snapshot 5 ms later to see what moves.
async fn checklist(csi: &Csi<'static>) {
    log_checklist(csi);
    Timer::after_millis(5).await;
    log_snapshot(csi);
}

/// Times the ROM call `wait_frame` makes (`Cache_Invalidate_Addr` over the frame, L1 + L2) on
/// a delivered frame, once with the frame's lines not cached and once after reading every
/// line. Then checks it is needed and works: the first `PROBE` bytes of a buffer are read (so
/// they sit in the caches); after 40 ms of DMA without `wait_frame`, reading them through the
/// cache must show the old frame and reading them after an invalidation the new one. When the
/// buffer is next delivered, the bytes as `wait_frame` hands them out must equal what an
/// explicit invalidation shows.
async fn cache_check(csi: &mut Csi<'static>) {
    if let Some((addr, cached)) = cache_control(csi).await {
        coherence(csi, addr, cached).await;
    }
}

/// Size of the probe the coherence checks read.
const PROBE: usize = 16 * 1024;

/// Timing, then the control: returns the buffer address and the probe sum now in the caches.
async fn cache_control(csi: &mut Csi<'static>) -> Option<(u32, u32)> {
    let Ok(Ok(f)) = with_timeout(FRAME_TIMEOUT, csi.wait_frame(None)).await else {
        note("no frame for the cache check");
        return None;
    };
    let addr = f.as_ptr() as u32;
    time_invalidation(&f);
    let old = byte_sum(&f[..PROBE]);
    drop(f);
    // No `wait_frame` here (it would invalidate the buffer): let the DMA cycle through the
    // free buffers for a few frames.
    Timer::after_millis(40).await;
    Some((addr, probe_control(addr, old)))
}

/// Reads the probe through the caches and after an invalidation; returns the cached sum.
#[inline(never)]
fn probe_control(addr: u32, old: u32) -> u32 {
    // SAFETY: read-only peek at a driver-owned buffer for this test.
    let peek = unsafe { core::slice::from_raw_parts(addr as *const u8, PROBE) };
    let cached = byte_sum(peek);
    cache::invalidate(addr as usize, PROBE);
    let memory = byte_sum(peek);
    log_control(addr, old, cached, memory);
    cached
}

/// Waits until buffer `addr` is delivered again and compares what `wait_frame` handed out with
/// a fresh invalidation.
async fn coherence(csi: &mut Csi<'static>, addr: u32, cached: u32) {
    let mut frame = None;
    for _ in 0..8 {
        let Ok(Ok(g)) = with_timeout(FRAME_TIMEOUT, csi.wait_frame(frame.take())).await else {
            note("no frame for the coherence check");
            return;
        };
        if g.as_ptr() as u32 == addr {
            probe_delivered(&g, cached);
            return;
        }
        frame = Some(g);
    }
    fail("buffer did not come back within 8 frames", &addr);
}

#[inline(never)]
fn probe_delivered(g: &Frame, cached: u32) {
    let addr = g.as_ptr() as u32;
    let delivered = byte_sum(&g[..PROBE]);
    cache::invalidate(addr as usize, PROBE);
    let fresh = byte_sum(&g[..PROBE]);
    log_coherence(addr, cached, delivered, fresh);
}

#[inline(never)]
fn time_invalidation(f: &Frame) {
    let (addr, len) = (f.as_ptr() as usize, f.len().div_ceil(64) * 64);
    let t = esp_hal::time::Instant::now();
    cache::invalidate(addr, len);
    let cold = t.elapsed().as_micros();
    let sum = line_sum(f);
    core::hint::black_box(sum);
    let t = esp_hal::time::Instant::now();
    cache::invalidate(addr, len);
    let warm = t.elapsed().as_micros();
    info!(
        "camera: cache invalidation of {len} B at {addr:#010x}: {cold} us uncached, {warm} us after reading the frame"
    );
}

fn line_sum(buf: &[u8]) -> u32 {
    let (lines, _) = buf.as_chunks::<64>();
    lines.iter().fold(0u32, |a, c| a.wrapping_add(c[0] as u32))
}

fn byte_sum(buf: &[u8]) -> u32 {
    buf.iter().fold(0u32, |a, &b| a.wrapping_add(b as u32))
}

// ── Strip soak ──────────────────────────────────────────────────────────────────────────

static STRIP_SOAK: StaticCell<strip_soak::StripSoak> = StaticCell::new();
static STRIP_CHECK: StaticCell<strip_soak::StripCheck> = StaticCell::new();

/// What the strip path runs per mode.
enum StripJob {
    Soak(&'static mut strip_soak::StripSoak),
    Check(&'static mut strip_soak::StripCheck),
}

impl StripJob {
    fn log_table(&self) {
        match self {
            StripJob::Soak(s) => s.log_table(),
            StripJob::Check(c) => c.log_table(),
        }
    }

    /// Runs per mode on `rx`: the soak's runs or the stall cases on strips, else one.
    fn runs(&self, rx: &Rx<'_>) -> usize {
        match (self, rx) {
            (StripJob::Soak(s), Rx::Strips(_)) => s.runs(),
            (StripJob::Check(c), Rx::Strips(_)) => c.runs(),
            _ => 1,
        }
    }

    /// Before run `k` of `mode` (plan slot `slot`) on `rx`, with the receiver stopped.
    #[inline(never)]
    fn prepare(&mut self, rx: &Rx<'_>, mode: &CamMode, slot: usize, k: usize) {
        match (self, rx) {
            (StripJob::Soak(s), Rx::Strips(_)) => {
                s.prepare(mode, k, strip_soak::lines(&csi_config(&mode.mode)));
            }
            (StripJob::Check(c), Rx::Frames(_)) => c.begin_reference(slot, mode),
            (StripJob::Check(c), Rx::Strips(_)) => c.begin_run(k),
            _ => {}
        }
    }

    /// The result of the run just done on `rx`, with the receiver stopped.
    #[inline(never)]
    fn log_run(&mut self, rx: &Rx<'_>, mode: &CamMode, slot: usize) {
        match (self, rx) {
            (StripJob::Soak(s), Rx::Strips(_)) => s.log_last(),
            (StripJob::Check(c), Rx::Frames(_)) => c.log_reference(slot),
            (StripJob::Check(c), Rx::Strips(_)) => c.log_run(slot, mode),
            (StripJob::Soak(_), Rx::Frames(_)) => {}
        }
    }
}

/// The receiver of one group of modes on the strip path.
enum Rx<'d> {
    /// Whole frames in PSRAM: the strip check's references.
    Frames(Csi<'d>),
    Strips(CsiStrips<'d>),
}

impl Rx<'_> {
    /// Configures the receiver for `mode` and starts it.
    fn start(&mut self, mode: &Mode) -> Result<(), StripError> {
        let config = csi_config(mode);
        match self {
            Rx::Frames(csi) => Ok(csi.set_config(config).and_then(|()| csi.start())?),
            Rx::Strips(cs) => cs
                .set_config(config, strip_soak::lines(&config))
                .and_then(|()| cs.start()),
        }
    }

    fn phy_status(&self) -> PhyStatus {
        match self {
            Rx::Frames(csi) => csi.phy_status(),
            Rx::Strips(cs) => cs.phy_status(),
        }
    }

    fn stop(&mut self) {
        match self {
            Rx::Frames(csi) => csi.stop(),
            Rx::Strips(cs) => cs.stop(),
        }
    }
}

/// The strip path (does nothing unless `ON`): the strip soak, or with `strips.check` the strip
/// check. The check first captures its whole-frame references with a `Csi` on the PSRAM
/// frame buffers, for every mode, and drops it; the soak never creates one. Both need the DMA
/// channel and the driver allows one receiver at a time. Consecutive modes with the same
/// strip size share one `CsiStrips` on the ring split for it (4 strips at 1280 RAW8, 8 at 640
/// RAW8, 3 in RAW10); at a change the receiver is dropped and the ring split again.
async fn strip_modes<const ON: bool>(
    cam: &mut Cam,
    mut vdma: VDMA_CH0<'static>,
    psram: &'static mut [u8],
    internal: &'static mut [u8],
    plan: &Plan,
    strips: strip_soak::Plan,
    skip: DarkSkip<'_>,
) {
    // Before the first await: with `ON` false the compiler drops the set-up below, and with it
    // every reference to the ring.
    if !ON {
        return;
    }
    let Some((mut job, mut frame_buffers, links)) = make_strip_job(psram, internal, strips) else {
        return;
    };
    let mut ring = strip_soak::Ring::take(links);
    let mut rest = plan.modes;
    let mut slot = 0;
    // One call site for every group (references, then strips): each `strip_group` future
    // constructed elsewhere would take its own room in this function's stack frame.
    loop {
        let (rx, group, first_slot) = if let Some(buffers) = frame_buffers.take() {
            (
                make_reference_receiver(vdma.reborrow(), buffers, &MODES[plan.modes[0]].mode),
                plan.modes,
                0,
            )
        } else {
            let Some(&first) = rest.first() else {
                break;
            };
            let config = csi_config(&MODES[first].mode);
            let bytes = strip_soak::strip_bytes(&config);
            let n = rest
                .iter()
                .take_while(|&&i| strip_soak::strip_bytes(&csi_config(&MODES[i].mode)) == bytes)
                .count();
            let (group, next) = rest.split_at(n);
            rest = next;
            slot += n;
            // SAFETY: the receiver of the previous group, which held the ring, was dropped at
            // the end of its `strip_group`.
            let rx = unsafe { make_strip_receiver(vdma.reborrow(), &mut ring, &config) };
            if let StripJob::Check(c) = &mut job {
                c.guard_ring_tail(ring.tail(&config));
            }
            (rx, group, slot - n)
        };
        if let Some(rx) = rx {
            strip_group(cam, rx, &mut job, group, first_slot, plan, skip).await;
        }
    }
    job.log_table();
}

/// The strip job, the PSRAM frame buffers of the check's references, and the stall test's
/// area for the list items ([`strip_soak::Ring::take`]).
type StripParts = (
    StripJob,
    Option<&'static mut [u8]>,
    Option<&'static mut [u8]>,
);

/// The soak, or the check with its frame buffers (the start of PSRAM) and its reference
/// words in PSRAM right after them.
#[inline(never)]
fn make_strip_job(
    psram: &'static mut [u8],
    internal: &'static mut [u8],
    strips: strip_soak::Plan,
) -> Option<StripParts> {
    let Some(window) = strips.check else {
        let Some(soak) = strip_soak::StripSoak::new(strips, internal) else {
            fail("internal buffer too small for the synthetic strip", &());
            return None;
        };
        return Some((StripJob::Soak(STRIP_SOAK.init(soak)), None, None));
    };
    make_check_job(psram, internal, window, strips.stall)
}

/// The check (the stall test if `stall`, with its list items and guard in `internal`).
#[inline(never)]
fn make_check_job(
    psram: &'static mut [u8],
    internal: &'static mut [u8],
    window: Duration,
    stall: bool,
) -> Option<StripParts> {
    let need = BUFFERS * BUFFER_LEN + 4 * strip_soak::CHECK_WORDS;
    if psram.len() < need {
        fail("PSRAM too small for the strip check", &need);
        return None;
    }
    let (guard, links) = match stall.then(|| strip_soak::guarded_links(internal)) {
        Some(Some((guard, links))) => (Some(guard), Some(links)),
        Some(None) => {
            fail("internal buffer too small for the stall test's guard", &());
            return None;
        }
        None => (None, None),
    };
    let (buffers, rest) = psram.split_at_mut(BUFFERS * BUFFER_LEN);
    // SAFETY: every bit pattern is a valid `u32`.
    let (_, words, _) = unsafe { rest.align_to_mut::<u32>() };
    let Some(check) = strip_soak::StripCheck::new(window, words, guard) else {
        fail("PSRAM too small for the strip check", &need);
        return None;
    };
    Some((
        StripJob::Check(STRIP_CHECK.init(check)),
        Some(buffers),
        links,
    ))
}

/// The whole-frame receiver for the strip check's references, on [`BUFFERS`] buffers.
#[inline(never)]
fn make_reference_receiver<'d>(
    vdma: VDMA_CH0<'d>,
    buffers: &'static mut [u8],
    first: &Mode,
) -> Option<Rx<'d>> {
    let (b0, rest) = buffers.split_at_mut(BUFFER_LEN);
    let (b1, b2) = rest.split_at_mut(BUFFER_LEN);
    match Csi::new(vdma, [b0, b1, b2], csi_config(first)) {
        Ok(c) => Some(Rx::Frames(c)),
        Err(e) => {
            fail("Csi::new (strip check references)", &e);
            None
        }
    }
}

/// The strip receiver for `config` on the ring, split into as many strips as fit.
///
/// # Safety
///
/// No receiver built on an earlier split of `ring` may still be alive.
#[inline(never)]
unsafe fn make_strip_receiver<'d>(
    vdma: VDMA_CH0<'d>,
    ring: &mut strip_soak::Ring,
    config: &Config,
) -> Option<Rx<'d>> {
    // SAFETY (every arm): the caller's guarantee.
    let cs = match strip_soak::ring_strips(config) {
        8 => strip_receiver(vdma, unsafe { ring.split::<8>(config) }, config),
        4 => strip_receiver(vdma, unsafe { ring.split::<4>(config) }, config),
        3 => strip_receiver(vdma, unsafe { ring.split::<3>(config) }, config),
        other => {
            fail("no ring split for this strip size (strips)", &other);
            None
        }
    };
    cs.map(Rx::Strips)
}

/// Creates the strip receiver for `config` on `ring` (internal RAM).
fn strip_receiver<'d, const N: usize>(
    vdma: VDMA_CH0<'d>,
    (strips, links): strip_soak::RingSplit<N>,
    config: &Config,
) -> Option<CsiStrips<'d>> {
    match CsiStrips::new(vdma, strips, links, *config, strip_soak::lines(config)) {
        Ok(c) => {
            log_strip_ready(&c, N);
            Some(c)
        }
        Err(e) => {
            fail("CsiStrips::new", &e);
            None
        }
    }
}

/// `group` (from plan slot `slot`) on `rx`, which is dropped at the end. Per mode and run:
/// the job's preparation, the sensor set-up (with the random test pattern for the check),
/// stream-on, then the check's reference frames (whole frames), or the soak's run or the
/// check's strip run (strips); stream-off, receiver stop, and only then the result line. The
/// receiver is stopped whenever the firmware does something else, so the DMA never halts on a
/// full ring between runs (see `strip_soak`).
async fn strip_group(
    cam: &mut Cam,
    mut rx: Rx<'_>,
    job: &mut StripJob,
    group: &[usize],
    slot: usize,
    plan: &Plan,
    skip: DarkSkip<'_>,
) {
    let pattern = matches!(job, StripJob::Check(_));
    for (k, &index) in group.iter().enumerate() {
        let mode = &MODES[index];
        let slot = slot + k;
        for run in 0..job.runs(&rx) {
            job.prepare(&rx, mode, slot, run);
            let started = start_stream(cam, &mut rx, mode, plan, pattern).await;
            let ok = started
                && match (&mut rx, &mut *job) {
                    (Rx::Frames(csi), StripJob::Check(c)) => reference_frames(csi, c, slot).await,
                    (Rx::Frames(_), StripJob::Soak(_)) => true,
                    (Rx::Strips(cs), StripJob::Soak(s)) => s.run_mode(cs, mode, skip).await,
                    (Rx::Strips(cs), StripJob::Check(c)) => c.run_mode(cs, slot).await,
                };
            if let Err(e) = cam.stop().await {
                fail("stream off", &e);
            }
            // Let the current frame finish so the lanes go back to LP-11.
            Timer::after_millis(20).await;
            rx.stop();
            if ok {
                job.log_run(&rx, mode, slot);
            } else {
                if started {
                    log_no_frames(&rx);
                }
                break;
            }
        }
    }
}

/// Drops the first two frames (stream start), then feeds
/// [`strip_soak::REFERENCE_FRAMES`] whole frames to the check. Each is held while its rows are
/// summed (about 11 ms at 1280x800 RAW10), so they are not consecutive. `false` if a frame
/// did not come.
async fn reference_frames(
    csi: &mut Csi<'_>,
    check: &mut strip_soak::StripCheck,
    slot: usize,
) -> bool {
    let mut frame = None;
    for k in 0..2 + strip_soak::REFERENCE_FRAMES {
        let Ok(Ok(f)) = with_timeout(FRAME_TIMEOUT, csi.wait_frame(frame.take())).await else {
            return false;
        };
        if k >= 2 {
            check.reference_frame(slot, &f, f.received_len(), f.seq());
        }
        frame = Some(f);
    }
    true
}

/// Sensor set-up (with the random test pattern if `pattern`), receiver start and stream-on.
/// `false` if something failed.
async fn start_stream(
    cam: &mut Cam,
    rx: &mut Rx<'_>,
    mode: &CamMode,
    plan: &Plan,
    pattern: bool,
) -> bool {
    log_mode(mode, &plan.sensor);
    if let Err(e) = sensor_setup(cam, &mode.mode, &plan.sensor).await {
        fail("sensor set-up", &e);
        return false;
    }
    if pattern {
        if let Err(e) = cam.set_test_pattern(TestPattern::Random).await {
            fail("test pattern", &e);
            return false;
        }
        log_pattern_regs(
            cam.read_register(PRE_CTRL00).await.ok(),
            cam.read_register(PRE_CTRL01).await.ok(),
        );
    }
    log_sensor_regs(cam).await;
    if let Err(e) = rx.start(&mode.mode) {
        fail("receiver start", &e);
        return false;
    }
    Timer::after_millis(1).await;
    log_phy_before_stream(&rx.phy_status());
    if let Err(e) = cam.start().await {
        fail("stream on", &e);
        return false;
    }
    true
}

/// PRE_CTRL00 (test pattern select) and PRE_CTRL01 (bit 4: same seed every frame), read back
/// for the strip check's log.
const PRE_CTRL00: u16 = 0x5E00;
const PRE_CTRL01: u16 = 0x5E01;

/// The test pattern registers as read back.
#[inline(never)]
fn log_pattern_regs(pre_ctrl00: Option<u8>, pre_ctrl01: Option<u8>) {
    info!(
        "camera: random test pattern, same seed every frame: 0x5E00 = {pre_ctrl00:02x?}, 0x5E01 = {pre_ctrl01:02x?}"
    );
}

/// No (whole) frame arrived: the receiver's state.
#[inline(never)]
fn log_no_frames(rx: &Rx<'_>) {
    match rx {
        Rx::Frames(_) => note("no whole frame for the strip check reference"),
        Rx::Strips(cs) => {
            note("no whole frame of strips");
            log_strip_state(cs);
        }
    }
}

#[inline(never)]
fn log_strip_ready(cs: &CsiStrips<'_>, strips: usize) {
    info!(
        "camera: strip receiver ready: {strips} strips of {} lines ({} B each) in internal RAM, chip rev {}",
        cs.strip_lines(),
        CsiStrips::strip_bytes(cs.config(), cs.strip_lines()),
        cs.chip_revision()
    );
}

/// The strip receiver's counters and ring snapshot.
#[inline(never)]
fn log_strip_state(cs: &CsiStrips<'_>) {
    info!("camera: strip stats {:?}", cs.stats());
    info!("camera: strip debug {:?}", cs.debug_status());
}

// ── Sensor ──────────────────────────────────────────────────────────────────────────────

/// `mode`'s registers, then the exposure, the gain, and as `sensor` says the test pattern, the
/// gated clock and PSV off. Leaves the sensor in standby.
async fn sensor_setup(cam: &mut Cam, mode: &Mode, sensor: &Sensor) -> Result<(), SensorError> {
    cam.configure(mode, &mut Delay).await?;
    cam.set_exposure(Exposure::lines(sensor.exposure(mode)))
        .await?;
    cam.set_gain(sensor.gain).await?;
    if sensor.pattern != TestPattern::Off {
        cam.set_test_pattern(sensor.pattern).await?;
    }
    if sensor.clock_gated {
        cam.write_register(MIPI_CLOCK_GATED.0, MIPI_CLOCK_GATED.1)
            .await?;
    }
    if sensor.psv_off {
        cam.write_register(PSV_OFF.0, PSV_OFF.1).await?;
    }
    Ok(())
}

/// Registers that decide the link, the window, the frame timing, PSV and the exposure, read
/// back after the mode set-up.
const READBACK: [u16; 35] = [
    0x0100, 0x030D, 0x3662, 0x4601, 0x4800, 0x4F00, 0x5E00, 0x3500, 0x3501, 0x3502, 0x3509, 0x3800,
    0x3801, 0x3802, 0x3803, 0x3804, 0x3805, 0x3806, 0x3807, 0x3808, 0x3809, 0x380A, 0x380B, 0x380C,
    0x380D, 0x380E, 0x380F, 0x3810, 0x3811, 0x3812, 0x3813, 0x3814, 0x3815, 0x3820, 0x3821,
];

async fn log_sensor_regs(cam: &mut Cam) {
    let mut vals = [0u8; READBACK.len()];
    for (v, &r) in vals.iter_mut().zip(READBACK.iter()) {
        *v = cam.read_register(r).await.unwrap_or(0xEE);
    }
    log_readback(&vals);
}

/// The name of a test pattern in the logs and still names.
pub const fn pattern_name(p: TestPattern) -> &'static str {
    match p {
        TestPattern::Off => "scene",
        TestPattern::Bars => "bars",
        TestPattern::RollingBars => "rollingbars",
        TestPattern::GreySquares => "greysquares",
        TestPattern::BlackWhiteSquares => "bwsquares",
        TestPattern::Random => "random",
    }
}

// ── Log lines ───────────────────────────────────────────────────────────────────────────

#[inline(never)]
fn fail(what: &str, detail: &dyn core::fmt::Debug) {
    error!("camera: {what}: {detail:?}");
}

#[inline(never)]
fn note(what: &str) {
    info!("camera: {what}");
}

#[inline(never)]
fn log_ldo(s: &LdoSetting) {
    info!(
        "camera: LDO VO3 2.5 V: dref {} mul {} calibrated {} expected {} mV, ctrl {:#010x} ana {:#010x}",
        s.dref, s.mul, s.calibrated, s.expected_mv, s.ctrl, s.ana
    );
}

#[inline(never)]
fn log_ready(csi: &Csi<'static>) {
    info!(
        "camera: CSI receiver ready, chip rev {}, {BUFFERS} buffers of {BUFFER_LEN} B in PSRAM",
        csi.chip_revision()
    );
}

#[inline(never)]
fn log_mode(mode: &CamMode, sensor: &Sensor) {
    let m = &mode.mode;
    let lines = sensor.exposure(m);
    info!(
        "camera: mode {} ({}x{} RAW{}, SCLK {} MHz, VTS {}), {} clock, {}, exposure {} lines = {} us (asked {}, max {}), gain {:#04x}{}",
        mode.name,
        m.width(),
        m.height(),
        m.format().bits_per_pixel(),
        m.sensor_clock().mhz(),
        m.vts(),
        if sensor.clock_gated {
            "gated"
        } else {
            "continuous"
        },
        pattern_name(sensor.pattern),
        lines,
        u32::from(lines) * m.line_ns() / 1000,
        sensor.exposure_lines,
        m.max_exposure_lines(),
        sensor.gain.as_sixteenths(),
        if sensor.psv_off {
            ", PSV forced off"
        } else {
            ""
        }
    );
    log_solved(mode);
}

/// The solver's choice for `mode`.
#[inline(never)]
pub fn log_solved(mode: &CamMode) {
    let m = &mode.mode;
    let (x, y) = m.origin();
    let requested = i64::from(m.frame_rate().as_millihertz()) - i64::from(m.rate_error());
    info!(
        "camera: {}: solved: {} at ({x}, {y}), SCLK {} MHz{}, HTS {}, VTS {}, PSV {}, {} cycles/frame, predicted {} (requested {}.{:03} Hz, error {:+} mHz)",
        mode.name,
        match m.readout() {
            Readout::Crop => "crop",
            Readout::Binned => "binned",
        },
        m.sensor_clock().mhz(),
        if m.exceeds_datasheet_clock() {
            " (above the datasheet's 80 MHz)"
        } else {
            ""
        },
        m.hts(),
        m.vts(),
        if m.psv() { "on" } else { "off" },
        ov9281::frame_cycles(m.hts(), m.vts()),
        m.frame_rate(),
        requested / 1000,
        requested % 1000,
        m.rate_error()
    );
}

#[inline(never)]
fn log_readback(vals: &[u8; READBACK.len()]) {
    info!("camera: sensor regs {READBACK:04x?} = {vals:02x?}");
    let reg = |r: u16| {
        READBACK
            .iter()
            .position(|&x| x == r)
            .map_or(0, |i| vals[i] as u16)
    };
    let pair = |r: u16| reg(r) << 8 | reg(r + 1);
    log_window([
        pair(0x3800),
        pair(0x3804),
        pair(0x3802),
        pair(0x3806),
        pair(0x3808),
        pair(0x380A),
        pair(0x3810),
        pair(0x3812),
    ]);
    log_timing([
        reg(0x3814),
        reg(0x3815),
        reg(0x3820),
        reg(0x3821),
        pair(0x380C),
        pair(0x380E),
        (reg(0x3500) & 0x0F) << 12 | reg(0x3501) << 4 | reg(0x3502) >> 4,
        reg(0x3509),
        reg(0x4601),
        reg(0x4F00),
    ]);
}

#[inline(never)]
fn log_window(v: [u16; 8]) {
    info!(
        "camera: sensor readback: array x {}..{} y {}..{}, output {}x{}, offset {},{}",
        v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]
    );
}

#[inline(never)]
fn log_timing(v: [u16; 10]) {
    info!(
        "camera: sensor readback: inc {:#04x}/{:#04x}, format {:#04x}/{:#04x}, HTS {} VTS {}, exposure {} lines, gain {:#04x}, VFIFO {:#04x}, PSV {:#04x}",
        v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8], v[9]
    );
}

#[inline(never)]
fn log_phy_before_stream(phy: &PhyStatus) {
    info!("camera: before stream-on: {phy:?}");
    if !(phy.clock_stop && phy.data_stop[0] && phy.data_stop[1]) {
        warn!("camera: lanes not all in stop state before stream-on");
    }
}

#[inline(never)]
fn log_first_frame(f: &Frame) {
    let n = f.len();
    info!(
        "camera: first frame seq {}, received {} of {} B, head {:02x?} tail {:02x?} (sentinel {SENTINEL:#04x})",
        f.seq(),
        f.received_len(),
        n,
        &f[..16],
        &f[n - 16..]
    );
}

/// Thousandths as `x.yyy` (millihertz as hertz, millilines as lines).
struct Milli(u64);

impl core::fmt::Display for Milli {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{:03}", self.0 / 1000, self.0 % 1000)
    }
}

#[inline(never)]
fn log_measurement(csi: &Csi<'static>, mode: &CamMode, m: &Measurement) {
    log_rate(&mode.mode, mode.name, m);
    let s = csi.stats();
    info!(
        "camera: {}: {} frames in {} ms, delivered {}, bytes/frame {} (expected {}, block items reported {}), wrong size {} (first {}), dropped {}, overwritten {}, worst interval {} us, seq gaps {}",
        mode.name,
        s.frames_done,
        m.window_us / 1000,
        m.delivered,
        s.last_frame_bytes,
        m.expected,
        s.last_block_items,
        m.wrong_size,
        m.first_wrong,
        s.frames_dropped,
        s.frames_overwritten,
        m.max_gap_us,
        m.seq_gaps
    );
}

/// The rate from the first to the last arrival. The window starts just before a delivery and
/// ends just after one, so `frames_done` over the window counts one frame more than the
/// periods it spans (+0.17 % at 600 frames): it is not used for the rate. The ppm compare the
/// measured period with the crate's exact one ([`ov9281::frame_cycles`]), not with the
/// millihertz-rounded [`Mode::frame_rate`] (250 ppm at 2 Hz).
#[inline(never)]
fn log_rate(mode: &Mode, name: &str, m: &Measurement) {
    let span_us = u64::from((m.last.0 - m.first.0).max(1));
    let periods = u64::from((m.last.1 - m.first.1).max(1));
    let rate_mhz = periods * 1_000_000_000 / span_us;
    let mhz = u64::from(mode.sensor_clock().mhz());
    let predicted_cycles = ov9281::frame_cycles(mode.hts(), mode.vts());
    // Measured period in SCLK cycles of the mode, in thousandths, and in units of 100 ps.
    let milli_cycles = span_us * mhz * 1000 / periods;
    let ppm = (predicted_cycles as i64 * 1000 - milli_cycles as i64) * 1_000_000
        / milli_cycles.max(1) as i64;
    let period_100ps = span_us * 10_000 / periods;
    info!(
        "camera: {name}: first-to-last {} fps over {periods} periods = {}.{} ns = {} SCLK cycles (predicted {predicted_cycles} = (VTS {} - 1) x HTS {} + {}), predicted {} ({ppm:+} ppm)",
        Milli(rate_mhz),
        period_100ps / 10,
        period_100ps % 10,
        Milli(milli_cycles),
        mode.vts(),
        mode.hts(),
        limits::FRAME_LONG_LINE,
        mode.frame_rate(),
    );
}

#[inline(never)]
fn log_errors(csi: &Csi<'static>) {
    let s = csi.stats();
    info!(
        "camera: errors: dma {} (bits {:#010x}), host {} (main {:#010x}, detail {:x?}), bridge {} (bits {:#x})",
        s.dma_errors,
        s.dma_error_bits,
        s.host_errors,
        s.host_error_main,
        s.host_error_detail,
        s.bridge_errors,
        s.bridge_error_bits
    );
}

#[inline(never)]
fn log_checklist(csi: &Csi<'static>) {
    let d = csi.debug_status();
    info!("check 1: chip rev {}", csi.chip_revision());
    info!(
        "check 2: hsfreqrange readback {:?} (want {:?})",
        d.hsfreqrange_readback,
        esp32p4_csi::hsfreqrange(csi.config().lane_bit_rate_mbps)
    );
    info!("check 3/4: {:?}", d.phy);
    log_errors(csi);
    info!(
        "check 6: isp cntl {:#010x} int_raw {:#010x}",
        d.isp_cntl, d.isp_int_raw
    );
    info!(
        "check 7: bridge enabled {} int_raw {:#010x} buf_depth {}",
        d.bridge_enabled, d.bridge_int_raw, d.bridge_buf_depth
    );
    log_snapshot(csi);
}

#[inline(never)]
fn log_snapshot(csi: &Csi<'static>) {
    let d = csi.debug_status();
    info!(
        "check 8: dma enabled {} int {:#010x} completed items {} fifo left {} dar {:#010x}, bridge buf_depth {}, phy {:?}",
        d.dma_enabled,
        d.dma_int_status,
        d.dma_completed_items,
        d.dma_fifo_left,
        d.dma_dar,
        d.bridge_buf_depth,
        d.phy
    );
}

#[inline(never)]
fn log_control(addr: u32, old: u32, cached: u32, memory: u32) {
    info!(
        "camera: cache control: buffer {addr:#010x} after 40 ms of DMA, no invalidation: sum {cached} (old frame {old}), after invalidation {memory} -> {}",
        if cached == old && memory != old {
            "stale without invalidation, as expected"
        } else {
            "inconclusive"
        }
    );
}

#[inline(never)]
fn log_coherence(addr: u32, old: u32, delivered: u32, fresh: u32) {
    info!(
        "camera: coherence: first 16 KiB of buffer {addr:#010x}: cached old sum {old}, delivered {delivered}, after re-invalidation {fresh} -> {}",
        if delivered == fresh { "OK" } else { "STALE" }
    );
}

// ── Still dump ──────────────────────────────────────────────────────────────────────────

/// Raw bytes per `@@D` line (base64: 128 characters).
const DUMP_CHUNK: usize = 96;

/// Prints `frame` as `@@IMG`, numbered base64 `@@D` lines and `@@END` with a CRC-32 (IEEE,
/// as zlib) over the pixels. `tools/still.py` turns it into a PGM and PNG.
#[inline(never)]
fn dump_still(prefix: [&str; 2], mode: &CamMode, pattern: TestPattern, frame: &Frame) {
    let t = esp_hal::time::Instant::now();
    still_header(prefix, mode, pattern, frame.len());
    dump_body(frame);
    info!(
        "camera: still dumped in {} ms",
        t.elapsed().as_micros() / 1000
    );
}

/// The numbered base64 `@@D` lines of `data` and `@@END` with a CRC-32 (IEEE, as zlib), after
/// a header line (`@@IMG` here, `@@JPG` in `jpeg_bench`).
#[inline(never)]
pub fn dump_body(data: &[u8]) {
    let mut crc = !0u32;
    let mut line = [0u8; DUMP_CHUNK / 3 * 4];
    for (i, chunk) in data.chunks(DUMP_CHUNK).enumerate() {
        crc = crc32_update(crc, chunk);
        let n = base64(chunk, &mut line);
        // SAFETY: base64 output is ASCII.
        let text = unsafe { core::str::from_utf8_unchecked(&line[..n]) };
        println!("@@D {i:x} {text}");
    }
    println!("@@END {:08x} {}", !crc, data.len().div_ceil(DUMP_CHUNK));
}

/// The `@@IMG` line, named `<prefix><w>x<h>-<fps>fps-raw<bits>-<pattern>` ([`pattern_name`]:
/// `scene`, `bars`, ...), `<fps>` being the requested rate in whole frames per second. The
/// bytes go out as they are in memory: one byte per pixel for RAW8, packed CSI-2 RAW10 (5
/// bytes per 4 pixels) for RAW10; `tools/still.py` unpacks by the last field (bits per pixel).
#[inline(never)]
fn still_header([p0, p1]: [&str; 2], mode: &CamMode, pattern: TestPattern, len: usize) {
    let (w, h) = (mode.mode.width(), mode.mode.height());
    let (fps, bits) = (mode.nominal_fps(), mode.mode.format().bits_per_pixel());
    let p = pattern_name(pattern);
    println!("@@IMG {p0}{p1}{w}x{h}-{fps}fps-raw{bits}-{p} {w} {h} {len} {bits}");
}

pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    crc
}

fn base64(data: &[u8], out: &mut [u8]) -> usize {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut n = 0;
    for c in data.chunks(3) {
        let v = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out[n] = T[(v >> 18) as usize & 63];
        out[n + 1] = T[(v >> 12) as usize & 63];
        out[n + 2] = if c.len() > 1 {
            T[(v >> 6) as usize & 63]
        } else {
            b'='
        };
        out[n + 3] = if c.len() > 2 {
            T[v as usize & 63]
        } else {
            b'='
        };
        n += 4;
    }
    n
}
