//! Test and benchmark of the hardware JPEG encoder (`esp32p4-jpeg`), greyscale, selected with
//! `JPEG_PLAN` in `main.rs`. Phases, each switched by [`Plan`]:
//!
//! 1. **Correctness, synthetic**: a test picture ([`draw_test`]: horizontal gradient over the
//!    top half, discs on noise below, a white border and a diagonal) at 1280x800, 640x400 and
//!    320x200, encoded at each quality of [`QUALITIES`]; every JPEG and the source are dumped
//!    over the UART (`@@JPG` / `@@IMG`, decoded on the host by `tools/still.py`, checked by
//!    `tools/jpeg_check.py`).
//! 2. **Correctness, camera**: one captured frame per mode of [`Plan::camera_stills`], encoded
//!    at each quality from the CSI buffer in PSRAM, dumped with its source.
//! 3. **Speed alone**: [`SPEED_SIZES`] x [`QUALITIES`], [`Plan::speed_frames`] encodes each
//!    of the test picture from PSRAM into PSRAM, timed with the cycle counter (the whole
//!    `encode` call); then 320x200 with input and output in internal RAM, and the variants of
//!    [`Plan::variants`].
//! 4. **Speed with the camera**: per mode of [`Plan::capture`], capture only for
//!    [`Plan::baseline`] (`camera::measure`), then every delivered frame encoded while the DMA
//!    captures the next, for the run's window; nothing is logged inside the window.
//! 5. **Robustness**: a too-small output (error, then the next frame), a dropped encode
//!    (timeout), then [`Plan::soak`] of capture and encoding with every output checked on the
//!    board, and the last frame dumped.
//!
//! Each log line is formatted in its own `#[inline(never)]` function (the crate denies
//! `clippy::large_stack_frames`).

use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_println::println;
use esp32p4_csi::{Csi, Frame};
use esp32p4_jpeg::{Config as JpegConfig, Encoder, Error as JpegError, HEADER_LEN, Stats};
use log::{info, warn};
use ov9281::Format::Raw8;
use ov9281::Readout::{Binned, Crop};
use static_cell::{ConstStaticCell, StaticCell};

use crate::camera::{self, Cam, Sensor};
use crate::mcycle;
use crate::modes::{self, CamMode};
use crate::synth::Synth;

/// What the JPEG plan runs.
#[derive(Clone, Copy)]
pub struct Plan {
    pub synthetic: bool,
    /// Modes of one captured, encoded and dumped frame each.
    pub camera_stills: &'static [CamMode],
    /// Encodes per size and quality in the speed test; 0 skips it.
    pub speed_frames: u32,
    /// Extra speed variants at 1280x800 q90 (memory placement).
    pub variants: bool,
    /// Capture-only window before each capture run.
    pub baseline: Duration,
    pub capture: &'static [CaptureRun],
    pub robustness: bool,
    /// Capture-and-encode soak of the robustness phase (1280x800 at 120 Hz).
    pub soak: Duration,
}

/// One capture-and-encode run.
#[derive(Clone, Copy)]
pub struct CaptureRun {
    pub mode: CamMode,
    pub window: Duration,
    pub quality: u8,
}

/// Qualities of the correctness and speed tests.
pub const QUALITIES: [u8; 3] = [50, 75, 90];
/// Sizes of the correctness test.
const CHECK_SIZES: [(u16, u16); 3] = [(1280, 800), (640, 400), (320, 200)];
/// Sizes of the speed test.
pub const SPEED_SIZES: [(u16, u16); 5] =
    [(1280, 800), (1280, 720), (640, 480), (640, 400), (320, 200)];

/// Camera modes of this plan, as requests to the `ov9281` mode builder (solved at compile
/// time). 128.3 and 141.5 Hz are the 80 MHz maxima of their windows; 150, 153.9 (the 96 MHz
/// maximum of 1280x800), 169.8 and 300 Hz need 96 MHz; 464.8 Hz is the 80 MHz maximum of
/// 320x200 binned.
pub mod cam_modes {
    use super::*;
    pub const M1280X800_120: CamMode = modes::MODES[0];
    pub const M1280X800_128: CamMode =
        modes::request("1280x800@128.3", Raw8, (1280, 800), Crop, 128.3, 0.05);
    pub const M1280X800_154: CamMode =
        modes::request("1280x800@153.9", Raw8, (1280, 800), Crop, 153.9, 0.1);
    pub const M1280X800_150: CamMode =
        modes::request("1280x800@150", Raw8, (1280, 800), Crop, 150.0, 0.5);
    pub const M1280X720_141: CamMode =
        modes::request("1280x720@141.5", Raw8, (1280, 720), Crop, 141.5, 0.1);
    pub const M1280X720_170: CamMode =
        modes::request("1280x720@169.8", Raw8, (1280, 720), Crop, 169.8, 0.1);
    pub const M640X400_250: CamMode =
        modes::request("640x400b@250", Raw8, (640, 400), Binned, 250.0, 0.5);
    pub const M640X400_300: CamMode =
        modes::request("640x400b@300", Raw8, (640, 400), Binned, 300.0, 2.0);
    pub const M320X200_465: CamMode =
        modes::request("320x200b@464.8", Raw8, (320, 200), Binned, 464.8, 0.2);
    pub const M640X400_120: CamMode = modes::MODES[3];
    pub const M320X200_453: CamMode = modes::MODES[8];
}

/// The camera stills of the correctness test: 1280x800, 640x400 and 320x200 (binned).
pub const CAMERA_STILLS: &[CamMode] = &[
    cam_modes::M1280X800_120,
    cam_modes::M640X400_120,
    cam_modes::M320X200_453,
];

const fn cap(mode: CamMode) -> CaptureRun {
    CaptureRun {
        mode,
        window: Duration::from_secs(20),
        quality: 90,
    }
}

/// The capture-and-encode runs: 20 s each at quality 90. 1280x800 up to the sensor's fastest
/// rate (153.98 Hz at 96 MHz, VTS 855).
pub const CAPTURE_RUNS: &[CaptureRun] = &[
    cap(cam_modes::M1280X800_120),
    cap(cam_modes::M1280X800_128),
    cap(cam_modes::M1280X800_150),
    cap(cam_modes::M1280X800_154),
    cap(cam_modes::M1280X720_141),
    cap(cam_modes::M1280X720_170),
    cap(cam_modes::M640X400_250),
    cap(cam_modes::M640X400_300),
    cap(cam_modes::M320X200_465),
];

/// PSRAM areas after the capture buffers: the test picture, two outputs.
const SRC_LEN: usize = 1024 * 1024;
const OUT_LEN: usize = 1024 * 1024;
/// Internal RAM: 64 KiB input, 64 KiB output (of the 128 KiB `internal` buffer).
const INTERNAL_HALF: usize = 64 * 1024;
/// Deadline of one encode.
const ENCODE_TIMEOUT: Duration = Duration::from_millis(200);

static ENCODER: StaticCell<Encoder> = StaticCell::new();
static SYNTH: ConstStaticCell<Synth> = ConstStaticCell::new(Synth::NEW);

struct Areas {
    src: &'static mut [u8],
    out: &'static mut [u8],
    out2: &'static mut [u8],
    internal: &'static mut [u8],
    synth: &'static mut Synth,
}

/// Runs `plan` with the receiver `csi` (created, stopped) and `rest`, the PSRAM after the
/// capture buffers (cache-line aligned).
pub async fn run(
    cam: &mut Cam,
    csi: &mut Csi<'static>,
    rest: &'static mut [u8],
    internal: &'static mut [u8],
    sensor: &Sensor,
    plan: &Plan,
) {
    let Some(mut a) = split(rest, internal) else {
        return;
    };
    let Some(enc) = make_encoder() else {
        return;
    };
    if plan.synthetic {
        synthetic(enc, &mut a).await;
    }
    for m in plan.camera_stills {
        camera_still(cam, csi, enc, &mut a, m, sensor).await;
    }
    if plan.speed_frames > 0 {
        speed(enc, &mut a, plan.speed_frames, plan.variants).await;
    }
    for r in plan.capture {
        capture(cam, csi, enc, &mut a, r, sensor, plan.baseline).await;
    }
    if plan.robustness {
        robustness(cam, csi, enc, &mut a, sensor, plan.soak).await;
    }
    log_stats("end", &enc.stats());
}

#[inline(never)]
fn split(rest: &'static mut [u8], internal: &'static mut [u8]) -> Option<Areas> {
    if rest.len() < SRC_LEN + 2 * OUT_LEN || internal.len() < 2 * INTERNAL_HALF {
        warn!("jpeg: memory too small");
        return None;
    }
    let (src, r) = rest.split_at_mut(SRC_LEN);
    let (out, r) = r.split_at_mut(OUT_LEN);
    let (out2, _) = r.split_at_mut(OUT_LEN);
    Some(Areas {
        src,
        out,
        out2,
        internal: &mut internal[..2 * INTERNAL_HALF],
        synth: SYNTH.take(),
    })
}

#[inline(never)]
fn make_encoder() -> Option<&'static mut Encoder> {
    let t = mcycle();
    match Encoder::new(JpegConfig::new(1280, 800, 90)) {
        Ok(e) => {
            let dt = mcycle().wrapping_sub(t);
            info!("jpeg: encoder ready in {} cycles", dt);
            let e = ENCODER.init(e);
            log_debug("after new", e);
            Some(e)
        }
        Err(e) => {
            warn!("jpeg: Encoder::new: {e:?}");
            None
        }
    }
}

/// The correctness test picture into `buf`, written back to memory.
#[inline(never)]
fn draw_test(buf: &mut [u8], w: u16, h: u16, synth: &mut Synth) {
    let (w, h) = (w as usize, h as usize);
    synth.fill(buf, w, h, 16, 0x5EED_0001 ^ (w as u32));
    for y in 0..h {
        let row = &mut buf[y * w..(y + 1) * w];
        if y < h / 2 {
            for (x, p) in row.iter_mut().enumerate() {
                *p = (x * 255 / (w - 1)) as u8;
            }
        }
        // Border.
        if y < 2 || y >= h - 2 {
            row.fill(255);
        }
        row[0] = 255;
        row[1] = 255;
        row[w - 1] = 255;
        row[w - 2] = 255;
        // Diagonal from top-left to bottom-right, 3 px wide, black over the gradient.
        let x = y * w / h;
        let v = if y < h / 2 { 0 } else { 255 };
        for p in row.iter_mut().skip(x.saturating_sub(1)).take(3) {
            *p = v;
        }
    }
    esp32p4_jpeg::write_back(&buf[..w * h]);
}

/// One timed encode, `None` (logged) on error or timeout.
async fn encode_timed(
    enc: &mut Encoder,
    input: &[u8],
    out: &mut [u8],
) -> Option<(Result<usize, JpegError>, u32)> {
    let t = mcycle();
    let r = with_timeout(ENCODE_TIMEOUT, enc.encode(input, out)).await;
    let dt = mcycle().wrapping_sub(t);
    match r {
        Ok(r) => Some((r, dt)),
        Err(_) => {
            log_timeout(enc);
            None
        }
    }
}

// ── 1. Synthetic correctness ──────────────────────────────────────────────────────────────

async fn synthetic(enc: &mut Encoder, a: &mut Areas) {
    for &(w, h) in &CHECK_SIZES {
        draw_test(a.src, w, h, a.synth);
        for &q in &QUALITIES {
            if !set(enc, w, h, q) {
                continue;
            }
            if let Some((r, dt)) = encode_timed(enc, a.src, a.out).await {
                report_and_dump("synth", w, h, q, r, dt, a.out);
            }
        }
        dump_pixels("synth", w, h, &a.src[..w as usize * h as usize]);
    }
}

#[inline(never)]
fn set(enc: &mut Encoder, w: u16, h: u16, q: u8) -> bool {
    match enc.set_config(JpegConfig::new(w, h, q)) {
        Ok(()) => true,
        Err(e) => {
            warn!("jpeg: set_config {w}x{h} q{q}: {e:?}");
            false
        }
    }
}

#[inline(never)]
fn report_and_dump(
    what: &str,
    w: u16,
    h: u16,
    q: u8,
    r: Result<usize, JpegError>,
    dt: u32,
    out: &[u8],
) {
    match r {
        Ok(len) => {
            let j = &out[..len];
            info!(
                "jpeg: {what} {w}x{h} q{q}: {len} B in {} us, head {:02x?} tail {:02x?}",
                dt / mhz(),
                &j[..4],
                &j[len - 2..]
            );
            dump_jpeg(what, w, h, q, j);
        }
        Err(e) => warn!("jpeg: {what} {w}x{h} q{q}: {e:?}"),
    }
}

// ── 2. Camera correctness ─────────────────────────────────────────────────────────────────

async fn camera_still(
    cam: &mut Cam,
    csi: &mut Csi<'static>,
    enc: &mut Encoder,
    a: &mut Areas,
    mode: &CamMode,
    sensor: &Sensor,
) {
    if !camera::start_mode(cam, csi, mode, sensor).await {
        camera::stop(cam, csi).await;
        return;
    }
    // A few frames to settle exposure, then hold one.
    let timeout = camera::frame_timeout(&mode.mode);
    for _ in 0..5 {
        let _ = with_timeout(timeout, csi.wait_frame(None)).await;
    }
    let f = match with_timeout(timeout, csi.wait_frame(None)).await {
        Ok(Ok(f)) => f,
        _ => {
            log_warn("no frame for the camera still");
            camera::stop(cam, csi).await;
            return;
        }
    };
    camera::stop(cam, csi).await;
    let (w, h) = (mode.mode.width(), mode.mode.height());
    encode_and_dump(enc, "cam", w, h, &QUALITIES, &f, a.out).await;
    drop(f);
}

/// Encodes `input` at each quality and dumps the files, then the source.
async fn encode_and_dump(
    enc: &mut Encoder,
    what: &str,
    w: u16,
    h: u16,
    qualities: &[u8],
    input: &[u8],
    out: &mut [u8],
) {
    for &q in qualities {
        if !set(enc, w, h, q) {
            continue;
        }
        if let Some((r, dt)) = encode_timed(enc, input, out).await {
            report_and_dump(what, w, h, q, r, dt, out);
        }
    }
    dump_pixels(what, w, h, input);
}

// ── 3. Speed alone ────────────────────────────────────────────────────────────────────────

/// Encode times and sizes over a run.
#[derive(Clone, Copy)]
struct Acc {
    n: u32,
    errors: u32,
    first_error: Option<JpegError>,
    t_min: u32,
    t_max: u32,
    t_sum: u64,
    s_min: u32,
    s_max: u32,
    s_sum: u64,
}

impl Acc {
    const NEW: Self = Self {
        n: 0,
        errors: 0,
        first_error: None,
        t_min: u32::MAX,
        t_max: 0,
        t_sum: 0,
        s_min: u32::MAX,
        s_max: 0,
        s_sum: 0,
    };

    fn add(&mut self, r: Result<usize, JpegError>, dt: u32) {
        match r {
            Ok(len) => {
                let len = len as u32;
                self.n += 1;
                self.t_min = self.t_min.min(dt);
                self.t_max = self.t_max.max(dt);
                self.t_sum += u64::from(dt);
                self.s_min = self.s_min.min(len);
                self.s_max = self.s_max.max(len);
                self.s_sum += u64::from(len);
            }
            Err(e) => {
                self.errors += 1;
                self.first_error.get_or_insert(e);
            }
        }
    }
}

async fn speed(enc: &mut Encoder, a: &mut Areas, frames: u32, variants: bool) {
    for &(w, h) in &SPEED_SIZES {
        // The test picture of this size (its own layout, not a crop).
        draw_test(a.src, w, h, a.synth);
        for &q in &QUALITIES {
            speed_run(enc, "psram->psram", a.src, a.out, (w, h, q), frames).await;
        }
    }
    // Internal RAM: 320x200 in and out.
    let (inp, out) = a.internal.split_at_mut(INTERNAL_HALF);
    draw_test(inp, 320, 200, a.synth);
    for &q in &QUALITIES {
        speed_run(enc, "sram->sram", inp, out, (320, 200, q), frames).await;
    }
    // Same picture from PSRAM into internal RAM and from internal RAM into PSRAM.
    a.src[..64000].copy_from_slice(&inp[..64000]);
    esp32p4_jpeg::write_back(&a.src[..64000]);
    speed_run(enc, "psram->sram", a.src, out, (320, 200, 90), frames).await;
    speed_run(enc, "sram->psram", inp, a.out, (320, 200, 90), frames).await;
    if variants {
        // 1280x800 from PSRAM into internal RAM: only q50 (35 kB) fits the 64 KiB output;
        // q75 and q90 end in `OutputTooSmall` (seen on the board).
        draw_test(a.src, 1280, 800, a.synth);
        speed_run(enc, "psram->sram", a.src, out, (1280, 800, 50), frames).await;
    }
}

/// `frames` encodes of `input` at `(width, height, quality)`, then their log line.
async fn speed_run(
    enc: &mut Encoder,
    path: &str,
    input: &[u8],
    out: &mut [u8],
    (w, h, q): (u16, u16, u8),
    frames: u32,
) {
    if !set(enc, w, h, q) {
        return;
    }
    let mut acc = Acc::NEW;
    for _ in 0..frames {
        match encode_timed(enc, input, out).await {
            Some((r, dt)) => acc.add(r, dt),
            None => acc.errors += 1,
        }
    }
    log_speed(path, w, h, q, &acc);
}

// ── 4. Capture and encode ─────────────────────────────────────────────────────────────────

/// Counters of one capture-and-encode window.
#[derive(Clone, Copy)]
struct CapAcc {
    enc: Acc,
    delivered: u32,
    /// Frames completed while the encoder was busy (sequence steps > 1 between two frames the
    /// encoder took).
    skipped: u32,
    wrong_size: u32,
    bad_output: u32,
    timeouts: u32,
    first_seq: u32,
    last_seq: u32,
    window_us: u64,
}

async fn capture(
    cam: &mut Cam,
    csi: &mut Csi<'static>,
    enc: &mut Encoder,
    a: &mut Areas,
    run: &CaptureRun,
    sensor: &Sensor,
    baseline: Duration,
) {
    let mode = &run.mode;
    if !camera::start_mode(cam, csi, mode, sensor).await {
        camera::stop(cam, csi).await;
        return;
    }
    if baseline.as_ticks() > 0 {
        camera::measure(csi, mode, baseline).await;
    }
    let (w, h) = (mode.mode.width(), mode.mode.height());
    if set(enc, w, h, run.quality) {
        encode_window(csi, enc, a.out, mode, run.quality, run.window).await;
    }
    camera::stop(cam, csi).await;
}

/// Encodes every delivered frame for `window`, then logs the counters; nothing is logged
/// inside the window.
async fn encode_window(
    csi: &mut Csi<'static>,
    enc: &mut Encoder,
    out: &mut [u8],
    mode: &CamMode,
    quality: u8,
    window: Duration,
) {
    let timeout = camera::frame_timeout(&mode.mode);
    let expected = mode.mode.width() as usize * mode.mode.height() as usize;
    Timer::after_millis(20).await;
    let _ = with_timeout(timeout, csi.wait_frame(None)).await;
    csi.reset_stats();
    let mut c = CapAcc {
        enc: Acc::NEW,
        delivered: 0,
        skipped: 0,
        wrong_size: 0,
        bad_output: 0,
        timeouts: 0,
        first_seq: 0,
        last_seq: 0,
        window_us: 0,
    };
    let t0 = Instant::now();
    let mut frame: Option<Frame> = None;
    while t0.elapsed() < window {
        let f = match with_timeout(timeout, csi.wait_frame(frame.take())).await {
            Ok(Ok(f)) => f,
            _ => {
                c.timeouts += 1;
                break;
            }
        };
        c.arrived(&f, expected);
        encode_counted(enc, &f, out, &mut c).await;
        frame = Some(f);
    }
    drop(frame);
    c.window_us = t0.elapsed().as_micros();
    log_capture(csi, mode, quality, &c);
    log_capture_errors(csi, mode);
}

/// One encode of the capture window, counted in `c`.
async fn encode_counted(enc: &mut Encoder, input: &[u8], out: &mut [u8], c: &mut CapAcc) {
    let t = mcycle();
    match with_timeout(ENCODE_TIMEOUT, enc.encode(input, out)).await {
        Ok(r) => {
            let dt = mcycle().wrapping_sub(t);
            if let Ok(len) = r
                && !plausible(&out[..len])
            {
                c.bad_output += 1;
            }
            c.enc.add(r, dt);
        }
        Err(_) => c.timeouts += 1,
    }
}

impl CapAcc {
    fn arrived(&mut self, f: &Frame, expected: usize) {
        if self.delivered == 0 {
            self.first_seq = f.seq();
        } else {
            self.skipped += f.seq().wrapping_sub(self.last_seq).wrapping_sub(1);
        }
        self.last_seq = f.seq();
        self.delivered += 1;
        if f.received_len() != expected {
            self.wrong_size += 1;
        }
    }
}

/// SOI at the start, EOI at the end, more than the header plus one block.
fn plausible(j: &[u8]) -> bool {
    j.len() > HEADER_LEN + 8 && j[..2] == [0xFF, 0xD8] && j[j.len() - 2..] == [0xFF, 0xD9]
}

// ── 5. Robustness ─────────────────────────────────────────────────────────────────────────

async fn robustness(
    cam: &mut Cam,
    csi: &mut Csi<'static>,
    enc: &mut Encoder,
    a: &mut Areas,
    sensor: &Sensor,
    soak: Duration,
) {
    if error_cases(enc, a).await {
        soak_run(cam, csi, enc, a, sensor, soak).await;
    }
}

/// Bad settings, too small an output, a dropped encode; each recovery checked against a
/// reference file. False if the reference encode failed.
async fn error_cases(enc: &mut Encoder, a: &mut Areas) -> bool {
    // Geometry errors come back before the hardware is touched.
    let e = enc.set_config(JpegConfig::new(1284, 800, 90));
    log_result("set_config 1284x800", &e);
    let e = enc.set_config(JpegConfig::new(1280, 800, 0));
    log_result("set_config q0", &e);

    draw_test(a.src, 1280, 800, a.synth);
    if !set(enc, 1280, 800, 90) {
        return false;
    }
    // Reference.
    let good = encode_timed(enc, a.src, a.out).await;
    let reference = match good {
        Some((Ok(len), _)) => (len, crc32(&a.out[..len])),
        _ => {
            log_warn("robustness: reference encode failed");
            return false;
        }
    };
    log_reference(reference);
    // Too small an output: the header and 4 KiB.
    let small = &mut a.out2[..HEADER_LEN + 4096];
    let r = encode_timed(enc, a.src, small).await;
    log_small(r.map(|(r, dt)| (r.map(|_| ()), dt)), enc);
    let r = encode_timed(enc, a.src, a.out).await;
    log_after("after the small output", r, reference, a.out, enc);
    dropped_case(enc, a, reference).await;
    true
}

/// An encode dropped before it ends (a 300 us timeout on a frame that takes milliseconds),
/// then the next frame compared with `reference` (length, CRC).
async fn dropped_case(enc: &mut Encoder, a: &mut Areas, reference: (usize, u32)) {
    let r = with_timeout(Duration::from_micros(300), enc.encode(a.src, a.out)).await;
    log_dropped(r.is_err());
    let r = encode_timed(enc, a.src, a.out).await;
    log_after("after the dropped encode", r, reference, a.out, enc);
}

/// Soak: 1280x800 at 120 Hz, every output checked, then one more frame encoded and dumped
/// with its source.
async fn soak_run(
    cam: &mut Cam,
    csi: &mut Csi<'static>,
    enc: &mut Encoder,
    a: &mut Areas,
    sensor: &Sensor,
    soak: Duration,
) {
    let mode = &cam_modes::M1280X800_120;
    if soak.as_ticks() == 0 || !camera::start_mode(cam, csi, mode, sensor).await {
        camera::stop(cam, csi).await;
        return;
    }
    encode_window(csi, enc, a.out, mode, 90, soak).await;
    let timeout = camera::frame_timeout(&mode.mode);
    let f = with_timeout(timeout, csi.wait_frame(None)).await;
    camera::stop(cam, csi).await;
    if let Ok(Ok(f)) = f {
        encode_and_dump(enc, "soak-last", 1280, 800, &[90], &f, a.out).await;
    }
}

// ── Dumps ─────────────────────────────────────────────────────────────────────────────────

/// `@@JPG <what>-<w>x<h>-q<q> <w> <h> <len>` and the bytes (`tools/still.py` writes the .jpg).
#[inline(never)]
fn dump_jpeg(what: &str, w: u16, h: u16, q: u8, j: &[u8]) {
    println!("@@JPG {what}-{w}x{h}-q{q} {w} {h} {}", j.len());
    camera::dump_body(j);
}

/// `@@IMG <what>-<w>x<h> <w> <h> <len> 8` and the pixels, as a still.
#[inline(never)]
fn dump_pixels(what: &str, w: u16, h: u16, px: &[u8]) {
    println!("@@IMG {what}-{w}x{h} {w} {h} {} 8", px.len());
    camera::dump_body(px);
}

fn crc32(data: &[u8]) -> u32 {
    !camera::crc32_update(!0, data)
}

// ── Log lines ─────────────────────────────────────────────────────────────────────────────

fn mhz() -> u32 {
    esp_hal::clock::cpu_clock().as_mhz()
}

#[inline(never)]
fn log_debug(what: &str, enc: &Encoder) {
    let d = enc.debug_status();
    info!("jpeg: debug {what}: {d:x?}");
}

#[inline(never)]
fn log_timeout(enc: &Encoder) {
    warn!("jpeg: encode timed out");
    log_debug("timeout", enc);
    log_stats("timeout", &enc.stats());
}

#[inline(never)]
fn log_stats(what: &str, s: &Stats) {
    info!("jpeg: stats {what}: {s:x?}");
}

#[inline(never)]
fn log_speed(path: &str, w: u16, h: u16, q: u8, a: &Acc) {
    let m = u64::from(mhz());
    let n = u64::from(a.n.max(1));
    let mean = a.t_sum / n;
    // Frames per second and megapixels per second from the mean, in thousandths.
    let fps_milli = m * 1_000_000_000 / mean.max(1);
    let mpx_milli = fps_milli * u64::from(w) * u64::from(h) / 1_000_000;
    info!(
        "jpeg: speed {path} {w}x{h} q{q}: {} frames, {} errors (first {:?}), us min {} mean {} max {}, fps {}.{:03}, Mpx/s {}.{:03}, bytes min {} mean {} max {}",
        a.n,
        a.errors,
        a.first_error,
        u64::from(a.t_min) / m,
        mean / m,
        u64::from(a.t_max) / m,
        fps_milli / 1000,
        fps_milli % 1000,
        mpx_milli / 1000,
        mpx_milli % 1000,
        a.s_min,
        a.s_sum / n,
        a.s_max
    );
}

#[inline(never)]
fn log_capture(csi: &Csi<'static>, mode: &CamMode, q: u8, c: &CapAcc) {
    let s = csi.stats();
    let m = u64::from(mhz());
    let n = u64::from(c.enc.n.max(1));
    let periods = u64::from(c.last_seq.wrapping_sub(c.first_seq).max(1));
    info!(
        "jpeg: capture {} q{q}: {} ms, captured {} (seq span {}), delivered {}, encoded {}, busy-skipped {}, encode errors {} (first {:?}), timeouts {}, bad outputs {}, wrong size {}, us min {} mean {} max {}, bytes min {} mean {} max {}",
        mode.name,
        c.window_us / 1000,
        s.frames_done,
        periods,
        c.delivered,
        c.enc.n,
        c.skipped,
        c.enc.errors,
        c.enc.first_error,
        c.timeouts,
        c.bad_output,
        c.wrong_size,
        u64::from(c.enc.t_min) / m,
        c.enc.t_sum / n / m,
        u64::from(c.enc.t_max) / m,
        c.enc.s_min,
        c.enc.s_sum / n,
        c.enc.s_max
    );
}

#[inline(never)]
fn log_capture_errors(csi: &Csi<'static>, mode: &CamMode) {
    let s = csi.stats();
    info!(
        "jpeg: capture {} errors: dropped {}, overwritten {}, dma {} (bits {:#x}), host {} (main {:#x}), bridge {} (bits {:#x})",
        mode.name,
        s.frames_dropped,
        s.frames_overwritten,
        s.dma_errors,
        s.dma_error_bits,
        s.host_errors,
        s.host_error_main,
        s.bridge_errors,
        s.bridge_error_bits
    );
}

#[inline(never)]
fn log_warn(what: &str) {
    warn!("jpeg: {what}");
}

#[inline(never)]
fn log_result(what: &str, r: &Result<(), JpegError>) {
    info!("jpeg: robustness: {what}: {r:?}");
}

#[inline(never)]
fn log_reference((len, crc): (usize, u32)) {
    info!("jpeg: robustness: reference 1280x800 q90: {len} B, crc {crc:08x}");
}

#[inline(never)]
fn log_small(r: Option<(Result<(), JpegError>, u32)>, enc: &Encoder) {
    match r {
        Some((r, dt)) => info!(
            "jpeg: robustness: output of {} B: {r:?} after {} us",
            HEADER_LEN + 4096,
            dt / mhz()
        ),
        None => info!("jpeg: robustness: output too small: timed out"),
    }
    log_stats("after small", &enc.stats());
}

#[inline(never)]
fn log_after(
    what: &str,
    r: Option<(Result<usize, JpegError>, u32)>,
    (len0, crc0): (usize, u32),
    out: &[u8],
    enc: &Encoder,
) {
    match r {
        Some((Ok(len), dt)) => {
            let crc = crc32(&out[..len]);
            info!(
                "jpeg: robustness: {what}: {len} B in {} us, crc {crc:08x} -> {}",
                dt / mhz(),
                if len == len0 && crc == crc0 {
                    "identical to the reference"
                } else {
                    "DIFFERENT"
                }
            );
        }
        Some((Err(e), _)) => warn!("jpeg: robustness: {what}: {e:?}"),
        None => warn!("jpeg: robustness: {what}: timed out"),
    }
    log_stats(what, &enc.stats());
}

#[inline(never)]
fn log_dropped(timed_out: bool) {
    info!("jpeg: robustness: encode with a 300 us timeout: dropped {timed_out}");
}
