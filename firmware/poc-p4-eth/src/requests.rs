//! Request verification: mode requests built with the `ov9281` builder at run time on the P4
//! (the list is data, every `build()` runs on the chip and is timed with the cycle counter),
//! each accepted one captured against a reference of the same window.
//!
//! Per request of [`REQUESTS`]:
//! 1. `build()` five times, timed (first call and fastest); the result or the error is logged.
//!    [`Ask::Fastest`] first asks for 1 MHz and takes the bound of the `TooFast` error, then
//!    asks for exactly that rate: the highest rate the builder allows for the window.
//! 2. If accepted and expected to be captured: the reference, the same window at
//!    [`REFERENCE_RATE`] (a long VTS) with the power-saving mode forced off and the request's
//!    exposure time, 1 s of capture and a scene still named `<request>-ref-...`.
//! 3. The request's mode: the capture measurement (the plan's window, at least
//!    [`MIN_PERIODS`] frame periods; rate against the mode's prediction, sizes, errors), a scene
//!    still and a bar-pattern still named `<request>-...`.
//!
//! Exposure: [`EXPOSURE_NS`] (800 lines at HTS 728 and 80 MHz), clamped to the request's
//! longest; the reference gets the same time in its own lines, so brightness compares at equal
//! exposure whatever the clock and line length.
//!
//! The pictures are judged on the host: the scene still against its reference (correlation,
//! flat rows, brightness), and its location in the full frame of the nearest 1280x800 still.

use core::hint::black_box;

use static_cell::ConstStaticCell;

use embassy_time::Duration;
use esp32p4_csi::Csi;
use log::info;
use ov9281::{Error, Format, Mode, Rate, Readout, TestPattern, Tolerance};

use crate::camera::{self, Cam, Plan, Sensor};
use crate::mcycle;
use crate::modes::CamMode;

/// What a request asks for.
#[derive(Clone, Copy)]
pub enum Ask {
    /// This rate within this tolerance.
    Rate(Rate, Tolerance),
    /// The highest rate the builder allows for the window (from a 1 MHz request's `TooFast`
    /// bound), exactly.
    Fastest,
}

/// What the request should do.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Build, then be captured.
    Accept,
    /// Be refused.
    Reject,
    /// Build; not captured (its setting is captured as another request).
    BuildOnly,
}

/// One request.
pub struct Request {
    pub name: &'static str,
    pub format: Format,
    pub size: (u16, u16),
    pub readout: Readout,
    /// `None`: the builder's default, centred.
    pub origin: Option<(u16, u16)>,
    pub ask: Ask,
    pub expect: Expect,
}

/// Rate of the reference capture: a long VTS for every window (VTS 3662 at HTS 728, 80 MHz).
const REFERENCE_RATE: Rate = Rate::hz(30.0);
/// The reference may run anywhere near it.
const REFERENCE_TOLERANCE: Tolerance = Tolerance::hz(1.0);
/// Capture of the reference: only its frames' sizes and errors matter.
const REFERENCE_MEASURE: Duration = Duration::from_secs(1);
/// Shortest measurement of a request, in its frame periods (14 s at 0.42 Hz).
const MIN_PERIODS: u64 = 6;
/// Exposure time of every capture: 800 lines of 9.1 us, as in the other plans.
const EXPOSURE_NS: u32 = 7_280_000;
/// Timed `build()` calls per request.
const BUILDS: usize = 5;

use Ask::{Fastest, Rate as Hz};
use Expect::{Accept, BuildOnly, Reject};
use Format::{Raw8, Raw10};
use Readout::{Binned, Crop};

const fn hz(rate: f32, tolerance: f32) -> Ask {
    Hz(Rate::hz(rate), Tolerance::hz(tolerance))
}

/// Exactly `rate` millihertz.
const fn exactly(rate: u32) -> Ask {
    Hz(Rate::millihertz(rate), Tolerance::millihertz(0))
}

const fn rq(
    name: &'static str,
    format: Format,
    size: (u16, u16),
    readout: Readout,
    origin: Option<(u16, u16)>,
    ask: Ask,
    expect: Expect,
) -> Request {
    Request {
        name,
        format,
        size,
        readout,
        origin,
        ask,
        expect,
    }
}

/// The requests, in order: first the ones that must be refused (and `x11`, which builds), then
/// the characterisation's verified requests (`rqNN`, with the tolerances of the crate's
/// `tests/requests.rs`), low rates, the narrowest window, and the highest rates the builder
/// allows per window at 80 MHz (`fNNN-80`, the crate's 80 MHz maximum, exactly) and 96 MHz
/// (`fNNN-96`, [`Ask::Fastest`]). 1280x800 requests recur so every still has a full frame taken
/// close in time to be located in.
#[rustfmt::skip]
pub const REQUESTS: &[Request] = &[
    rq("x1", Raw8, (1280, 800), Crop, None, hz(160.0, 0.1), Reject),
    rq("x2", Raw10, (1280, 800), Crop, None, hz(140.0, 0.1), Reject),
    rq("x3", Raw10, (640, 480), Crop, None, hz(250.0, 0.1), Reject),
    rq("x4", Raw8, (1276, 800), Crop, None, hz(120.0, 0.1), Reject),
    rq("x5", Raw10, (1248, 720), Crop, None, hz(120.0, 0.1), Reject),
    rq("x6", Raw10, (640, 401), Crop, None, hz(120.0, 0.1), Reject),
    rq("x7", Raw8, (1296, 800), Crop, None, hz(120.0, 0.1), Reject),
    rq("x8", Raw8, (0, 0), Crop, None, hz(120.0, 0.1), Reject),
    rq("x9a", Raw8, (640, 480), Crop, Some((700, 0)), hz(120.0, 0.1), Reject),
    rq("x9b", Raw8, (640, 480), Crop, Some((0, 400)), hz(120.0, 0.1), Reject),
    rq("x10", Raw8, (48, 200), Crop, None, hz(120.0, 0.1), Reject),
    rq("x11", Raw8, (1280, 800), Crop, None, hz(120.0, 0.0001), BuildOnly),
    rq("x11b", Raw8, (1280, 800), Crop, None, exactly(120_001), Reject),
    rq("x12", Raw8, (1280, 800), Crop, None, hz(0.1, 0.01), Reject),
    rq("x13", Raw8, (640, 480), Crop, Some((101, 37)), hz(120.0, 0.1), Reject),
    rq("rq01", Raw8, (1280, 800), Crop, None, hz(120.0, 0.1), Accept),
    rq("rq04", Raw8, (1280, 720), Crop, None, hz(140.0, 0.08), Accept),
    rq("rq05", Raw10, (1280, 720), Crop, None, hz(120.0, 0.082), Accept),
    rq("rq06", Raw8, (640, 480), Crop, Some((0, 0)), hz(200.0, 0.027), Accept),
    rq("rq16b", Raw8, (640, 240), Crop, Some((200, 160)), hz(300.0, 0.062), Accept),
    rq("rq02", Raw8, (1280, 800), Crop, None, hz(128.0, 0.021), Accept),
    rq("rq07", Raw8, (640, 400), Binned, None, hz(250.0, 0.105), Accept),
    rq("rq08", Raw10, (640, 400), Binned, None, hz(210.0, 0.1), Accept),
    rq("rq20", Raw8, (640, 400), Binned, None, hz(300.0, 2.0), Accept),
    rq("rq09", Raw8, (320, 200), Binned, None, hz(470.0, 0.871), Accept),
    rq("rq10", Raw8, (320, 200), Binned, Some((96, 36)), hz(400.0, 0.509), Accept),
    rq("rq15", Raw10, (320, 200), Binned, None, hz(400.0, 0.509), Accept),
    rq("rq17", Raw10, (320, 200), Binned, Some((400, 160)), hz(560.0, 2.126), Accept),
    rq("rq03", Raw8, (1280, 800), Crop, None, hz(150.0, 0.044), Accept),
    rq("rq12", Raw10, (1280, 800), Crop, None, hz(125.0, 0.036), Accept),
    rq("rq21", Raw8, (1280, 800), Crop, None, hz(120.0, 0.001), Accept),
    rq("rq11", Raw8, (1280, 64), Crop, None, hz(1000.0, 5.0), Accept),
    rq("rq14b", Raw10, (640, 480), Crop, None, hz(240.0, 0.032), Accept),
    rq("rq18", Raw8, (1280, 400), Crop, Some((0, 200)), hz(15.0, 0.001), Accept),
    rq("rq19", Raw10, (1216, 704), Crop, Some((32, 48)), hz(130.0, 0.2), Accept),
    rq("rq16", Raw8, (320, 240), Crop, Some((32, 501)), hz(364.6, 0.027), Accept),
    rq("rq13", Raw8, (1280, 800), Crop, None, hz(30.0, 0.011), Accept),
    rq("lo2", Raw8, (1280, 800), Crop, None, hz(2.0, 0.01), Accept),
    rq("lo042", Raw8, (1280, 800), Crop, None, hz(0.42, 0.001), Accept),
    rq("narrow", Raw8, (128, 16), Crop, None, hz(1000.0, 5.0), Accept),
    rq("f800-80", Raw8, (1280, 800), Crop, None, exactly(128_320), Accept),
    rq("f800-96", Raw8, (1280, 800), Crop, None, Fastest, Accept),
    rq("f720-80", Raw8, (1280, 720), Crop, None, exactly(141_542), Accept),
    rq("f720-96", Raw8, (1280, 720), Crop, None, Fastest, Accept),
    rq("f480-80", Raw8, (640, 480), Crop, None, exactly(204_875), Accept),
    rq("f480-96", Raw8, (640, 480), Crop, None, Fastest, Accept),
    rq("f400-80", Raw8, (640, 400), Binned, None, exactly(251_824), Accept),
    rq("f400-96", Raw8, (640, 400), Binned, None, Fastest, Accept),
    rq("f200-80", Raw8, (320, 200), Binned, None, exactly(464_895), Accept),
    rq("f200-96", Raw8, (320, 200), Binned, None, Fastest, Accept),
    rq("f800r10-80", Raw10, (1280, 800), Crop, None, exactly(128_320), Accept),
    rq("f800r10-96", Raw10, (1280, 800), Crop, None, Fastest, Accept),
    rq("f400r10-80", Raw10, (640, 400), Binned, None, exactly(251_824), Accept),
    rq("f400r10-96", Raw10, (640, 400), Binned, None, Fastest, Accept),
    rq("rq01b", Raw8, (1280, 800), Crop, None, hz(120.0, 0.1), Accept),
];

/// One capture of a request: its reference or the request's mode.
struct Job {
    prefix: [&'static str; 2],
    mode: CamMode,
    sensor: Sensor,
    window: Duration,
    patterns: &'static [TestPattern],
}

/// The captures of the current request, out of [`run`]'s future (which also holds the
/// measurement's, close to the stack-frame limit).
static JOBS: ConstStaticCell<[Option<Job>; 2]> = ConstStaticCell::new([None, None]);

/// Runs every request of `plan`.
pub async fn run(cam: &mut Cam, csi: &mut Csi<'static>, plan: &Plan) {
    let jobs = JOBS.take();
    for r in plan.requests {
        *jobs = make_jobs(r, plan);
        // One `capture` call site: each one would add its future to this function's.
        for job in jobs.iter().flatten() {
            note(r.name, job.prefix[1]);
            capture(cam, csi, job).await;
        }
    }
    note("requests", "done");
}

/// The captures of `r`: the reference (if it builds), then the request's mode; none if the
/// request is not to be captured or the builder did not do what was expected.
#[inline(never)]
fn make_jobs(r: &'static Request, plan: &Plan) -> [Option<Job>; 2] {
    let Some(mode) = build_logged(r) else {
        return [None, None];
    };
    let lines = clamp_lines(EXPOSURE_NS / mode.line_ns(), &mode);
    let exposure_ns = u32::from(lines) * mode.line_ns();
    let reference = reference(r).map(|m| Job {
        prefix: [r.name, "-ref-"],
        mode: CamMode {
            name: "reference",
            mode: m,
        },
        sensor: Sensor {
            exposure_lines: clamp_lines((exposure_ns + m.line_ns() / 2) / m.line_ns(), &m),
            psv_off: true,
            ..plan.sensor
        },
        window: REFERENCE_MEASURE,
        patterns: &[],
    });
    let periods = Duration::from_micros(MIN_PERIODS * u64::from(mode.frame_ns()) / 1000);
    let test = Job {
        prefix: [r.name, "-"],
        mode: CamMode { name: r.name, mode },
        sensor: Sensor {
            exposure_lines: lines,
            ..plan.sensor
        },
        window: plan.measure.max(periods),
        patterns: &[TestPattern::Bars],
    };
    [reference, Some(test)]
}

/// `lines` within `mode`'s exposure range.
fn clamp_lines(lines: u32, mode: &Mode) -> u16 {
    lines.clamp(1, u32::from(mode.max_exposure_lines())) as u16
}

/// Start, measurement, the scene still (and one per pattern), stop.
async fn capture(cam: &mut Cam, csi: &mut Csi<'static>, job: &Job) {
    if camera::start_mode(cam, csi, &job.mode, &job.sensor).await {
        camera::measure(csi, &job.mode, job.window).await;
        camera::stills(
            cam,
            csi,
            job.prefix,
            &job.mode,
            job.sensor.pattern,
            job.patterns,
        )
        .await;
    }
    camera::stop(cam, csi).await;
}

/// Builds `r` (timed, logged); the mode if it is accepted and should be.
#[inline(never)]
fn build_logged(r: &Request) -> Option<Mode> {
    let ask = match r.ask {
        Fastest => {
            let probe = Hz(Rate::millihertz(1_000_000_000), Tolerance::millihertz(0));
            let (result, timing) = timed(r, probe);
            log_built(r, None, probe, &result, timing);
            match result {
                Err(Error::TooFast { max }) => Hz(max, Tolerance::millihertz(0)),
                _ => return None,
            }
        }
        ask => ask,
    };
    let (result, timing) = timed(r, ask);
    log_built(r, Some(r.expect), ask, &result, timing);
    match (result, r.expect) {
        (Ok(m), Accept) => Some(m),
        _ => None,
    }
}

/// `BUILDS` builds of `r` with `ask`: the result, and the cycles of the first and the
/// fastest call.
#[inline(never)]
fn timed(r: &Request, ask: Ask) -> (Result<Mode, Error>, (u32, u32)) {
    let mut first = 0;
    let mut best = u32::MAX;
    let mut result = Err(Error::Window);
    for k in 0..BUILDS {
        let (req, ask) = black_box((r, ask));
        let t0 = mcycle();
        result = build(req, ask);
        let cycles = mcycle().wrapping_sub(t0);
        black_box(&result);
        if k == 0 {
            first = cycles;
        }
        best = best.min(cycles);
    }
    (result, (first, best))
}

/// The builder call itself, as an application writes it.
#[inline(never)]
fn build(r: &Request, ask: Ask) -> Result<Mode, Error> {
    let Hz(rate, tolerance) = ask else {
        return Err(Error::Window);
    };
    let (w, h) = r.size;
    let b = Mode::builder()
        .format(r.format)
        .size(w, h)
        .readout(r.readout);
    let b = match r.origin {
        Some((x, y)) => b.origin(x, y),
        None => b,
    };
    b.frame_rate(rate, tolerance).build()
}

/// The reference for `r`: the same window at [`REFERENCE_RATE`].
#[inline(never)]
fn reference(r: &Request) -> Option<Mode> {
    let reference = build(r, Hz(REFERENCE_RATE, REFERENCE_TOLERANCE));
    if let Err(e) = &reference {
        log_error(r.name, "reference", e);
    }
    reference.ok()
}

#[inline(never)]
fn note(name: &str, what: &str) {
    info!("requests: {name}: {what}");
}

#[inline(never)]
fn log_error(name: &str, what: &str, e: &Error) {
    info!("requests: {name}: {what}: error {e:?}: {e}");
}

/// One build result: the request (`expect`: `None` for the probe of [`Ask::Fastest`]), the
/// timing and the solver's choice or the error.
#[inline(never)]
fn log_built(
    r: &Request,
    expect: Option<Expect>,
    ask: Ask,
    result: &Result<Mode, Error>,
    (first, best): (u32, u32),
) {
    let mhz = esp_hal::clock::cpu_clock().as_mhz();
    let (w, h) = r.size;
    let (rate, tolerance) = match ask {
        Hz(rate, tolerance) => (rate.as_millihertz(), tolerance.as_millihertz()),
        Fastest => (0, 0),
    };
    let readout = match r.readout {
        Crop => "crop",
        Binned => "binned",
    };
    let verdict = match (result.is_ok(), expect) {
        (_, None) => "probe",
        (true, Some(Accept)) => "accepted (expected)",
        (true, Some(BuildOnly)) => "accepted (expected, not captured)",
        (false, Some(Reject)) => "rejected (expected)",
        (true, Some(Reject)) => "accepted (UNEXPECTED)",
        (false, Some(_)) => "rejected (UNEXPECTED)",
    };
    info!(
        "requests: {}: RAW{} {w}x{h} {readout}, origin {:?}, {}.{:03} Hz +- {}.{:03}: {verdict}; build {} cycles = {} us (first call {} cycles)",
        r.name,
        r.format.bits_per_pixel(),
        r.origin,
        rate / 1000,
        rate % 1000,
        tolerance / 1000,
        tolerance % 1000,
        best,
        best / mhz,
        first
    );
    match result {
        Ok(m) => camera::log_solved(&CamMode {
            name: r.name,
            mode: *m,
        }),
        Err(e) => log_error(r.name, "build", e),
    }
}
