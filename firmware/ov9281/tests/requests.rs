//! The requests of the limits characterisation (2026-11-10, `README.md`, "Requests verified on
//! the board"): each verified request builds, and the requests the characterisation says must
//! be refused are, with the bound recomputed for the 4-line VTS margin.
//!
//! Where the report gives no tolerance, the request's tolerance is the report setting's own
//! distance from the requested rate, so the report's setting is inside it.

use ov9281::{Error, Format, Mode, Rate, Readout, SensorClock, Tolerance, frame_cycles, limits};

use Format::{Raw8, Raw10};
use Readout::{Binned, Crop};
use SensorClock::{Mhz80, Mhz96};

fn rate_at(clock: SensorClock, hts: u16, vts: u16) -> u32 {
    let c = frame_cycles(hts, vts);
    ((clock.hz() as u64 * 2000 + c) / (2 * c)) as u32
}

/// Clock, HTS, VTS, PSV.
type Setting = (SensorClock, u16, u16, bool);

/// What the solver does with a verified request.
enum Choice {
    /// It builds this setting. The text says why it differs from the report's; empty when it
    /// is the report's.
    Builds(Setting, &'static str),
    /// It refuses the request at its tolerance (the report's setting is outside the rules);
    /// with the tolerance widened to `wider` it builds the setting.
    Refused(Error, u32, Setting, &'static str),
}

struct Verified {
    id: &'static str,
    format: Format,
    readout: Readout,
    size: (u16, u16),
    origin: Option<(u16, u16)>,
    /// Requested rate and tolerance in millihertz.
    req: u32,
    tol: u32,
    /// The report's setting, its formula rate and the rate measured over 2 s (millihertz).
    report: Setting,
    report_rate: u32,
    measured: u32,
    choice: Choice,
}

use Choice::{Builds, Refused};

const NEARER: &str = "a VTS nearer the request than the report's";
const PSV_MARGIN: &str = "PSV off: the VTS is above the measured h + 71 but below h + 75";

#[rustfmt::skip]
const VERIFIED: [Verified; 22] = [
    Verified { id: "rq01", format: Raw8, readout: Crop, size: (1280, 800), origin: None, req: 120_000, tol: 100,
        report: (Mhz80, 728, 915, true), report_rate: 119_918, measured: 119_921,
        choice: Builds((Mhz80, 728, 914, true), NEARER) },
    Verified { id: "rq02", format: Raw8, readout: Crop, size: (1280, 800), origin: None, req: 128_000, tol: 21,
        report: (Mhz80, 728, 857, false), report_rate: 128_021, measured: 128_024,
        choice: Builds((Mhz80, 728, 857, false), "") },
    Verified { id: "rq03", format: Raw8, readout: Crop, size: (1280, 800), origin: None, req: 150_000, tol: 44,
        report: (Mhz96, 728, 878, false), report_rate: 149_956, measured: 149_961,
        choice: Builds((Mhz96, 728, 878, false), "") },
    Verified { id: "rq04", format: Raw8, readout: Crop, size: (1280, 720), origin: None, req: 140_000, tol: 80,
        report: (Mhz80, 728, 784, false), report_rate: 139_920, measured: 139_924,
        choice: Builds((Mhz80, 728, 784, false), "") },
    Verified { id: "rq05", format: Raw10, readout: Crop, size: (1280, 720), origin: None, req: 120_000, tol: 82,
        report: (Mhz80, 728, 915, true), report_rate: 119_918, measured: 119_921,
        choice: Builds((Mhz80, 728, 914, true), NEARER) },
    Verified { id: "rq06", format: Raw8, readout: Crop, size: (640, 480), origin: Some((0, 0)), req: 200_000, tol: 27,
        report: (Mhz80, 728, 548, false), report_rate: 200_027, measured: 200_033,
        choice: Builds((Mhz80, 728, 548, false), "") },
    Verified { id: "rq07", format: Raw8, readout: Binned, size: (640, 400), origin: None, req: 250_000, tol: 105,
        report: (Mhz80, 728, 438, false), report_rate: 250_105, measured: 250_112,
        choice: Builds((Mhz80, 728, 438, false), "") },
    Verified { id: "rq08", format: Raw10, readout: Binned, size: (640, 400), origin: None, req: 210_000, tol: 437,
        report: (Mhz80, 728, 523, true), report_rate: 209_563, measured: 209_569,
        choice: Builds((Mhz80, 728, 522, true), NEARER) },
    Verified { id: "rq09", format: Raw8, readout: Binned, size: (320, 200), origin: None, req: 470_000, tol: 871,
        report: (Mhz80, 728, 232, false), report_rate: 470_871, measured: 470_884,
        choice: Builds((Mhz96, 728, 279, true),
            "VTS 232 = h + 32 is below h + 35, so 470 Hz is above the 80 MHz maximum (464.895 Hz)") },
    Verified { id: "rq10", format: Raw8, readout: Binned, size: (320, 200), origin: Some((96, 36)), req: 400_000, tol: 509,
        report: (Mhz80, 728, 273, true), report_rate: 400_509, measured: 400_520,
        choice: Builds((Mhz80, 728, 273, false), PSV_MARGIN) },
    Verified { id: "rq11", format: Raw8, readout: Crop, size: (1280, 64), origin: None, req: 1_000_000, tol: 5_000,
        report: (Mhz96, 728, 131, false), report_rate: 996_161, measured: 996_188,
        choice: Builds((Mhz96, 728, 130, false), NEARER) },
    Verified { id: "rq12", format: Raw10, readout: Crop, size: (1280, 800), origin: None, req: 125_000, tol: 36,
        report: (Mhz80, 728, 878, false), report_rate: 124_964, measured: 124_967,
        choice: Builds((Mhz80, 728, 878, false), "") },
    Verified { id: "rq13", format: Raw8, readout: Crop, size: (1280, 800), origin: None, req: 30_000, tol: 11,
        report: (Mhz80, 728, 3663, true), report_rate: 29_989, measured: 29_990,
        choice: Builds((Mhz80, 728, 3662, true), NEARER) },
    Verified { id: "rq14b", format: Raw10, readout: Crop, size: (640, 480), origin: None, req: 240_000, tol: 32,
        report: (Mhz96, 728, 548, false), report_rate: 240_032, measured: 240_039,
        choice: Builds((Mhz96, 728, 548, false), "") },
    Verified { id: "rq15", format: Raw10, readout: Binned, size: (320, 200), origin: None, req: 400_000, tol: 509,
        report: (Mhz80, 728, 273, true), report_rate: 400_509, measured: 400_520,
        choice: Builds((Mhz80, 728, 273, false), PSV_MARGIN) },
    Verified { id: "rq16b", format: Raw8, readout: Crop, size: (640, 240), origin: Some((200, 160)), req: 300_000, tol: 62,
        report: (Mhz80, 728, 365, true), report_rate: 299_938, measured: 299_946,
        choice: Builds((Mhz80, 728, 365, true), "") },
    Verified { id: "rq17", format: Raw10, readout: Binned, size: (320, 200), origin: Some((400, 160)), req: 560_000, tol: 244,
        report: (Mhz96, 728, 234, false), report_rate: 560_244, measured: 560_259,
        choice: Refused(Error::TooFast { max: Rate::millihertz(557_874) }, 2_126, (Mhz96, 728, 235, false),
            "VTS 234 = h + 34 is below h + 35; the fastest 96 MHz rate is 557.874 Hz") },
    Verified { id: "rq18", format: Raw8, readout: Crop, size: (1280, 400), origin: Some((0, 200)), req: 15_000, tol: 1,
        report: (Mhz80, 728, 7325, true), report_rate: 14_999, measured: 15_000,
        choice: Builds((Mhz80, 728, 7325, true), "") },
    Verified { id: "rq19", format: Raw10, readout: Crop, size: (1216, 704), origin: Some((32, 48)), req: 130_000, tol: 200,
        report: (Mhz80, 728, 845, true), report_rate: 129_836, measured: 129_839,
        choice: Builds((Mhz80, 728, 844, true), NEARER) },
    Verified { id: "rq20", format: Raw8, readout: Binned, size: (640, 400), origin: None, req: 300_000, tol: 2_000,
        report: (Mhz96, 728, 440, false), report_rate: 298_766, measured: 298_774,
        choice: Builds((Mhz96, 728, 438, false), NEARER) },
    Verified { id: "rq21", format: Raw8, readout: Crop, size: (1280, 800), origin: None, req: 120_000, tol: 1,
        report: (Mhz80, 738, 902, true), report_rate: 120_000, measured: 120_003,
        choice: Builds((Mhz80, 738, 902, true), "") },
    Verified { id: "rq16", format: Raw8, readout: Crop, size: (320, 240), origin: Some((32, 501)), req: 364_600, tol: 27,
        report: (Mhz80, 728, 300, false), report_rate: 364_627, measured: 364_637,
        choice: Builds((Mhz80, 728, 300, false), "") },
];

fn build(v: &Verified, tol: u32) -> Result<Mode, Error> {
    let b = Mode::builder()
        .format(v.format)
        .size(v.size.0, v.size.1)
        .readout(v.readout);
    let b = match v.origin {
        Some((x, y)) => b.origin(x, y),
        None => b,
    };
    b.frame_rate(Rate::millihertz(v.req), Tolerance::millihertz(tol))
        .build()
}

fn setting(m: &Mode) -> Setting {
    (m.sensor_clock(), m.hts(), m.vts(), m.psv())
}

/// Whether the report's clock, HTS and VTS are inside the crate's rules.
fn report_timing_allowed(v: &Verified) -> bool {
    let (clock, hts, vts, _) = v.report;
    let line = v.format.line_bytes(v.size.0);
    vts >= limits::min_vts(v.readout, v.size.1)
        && (limits::hts_min(line, clock)..=limits::HTS_MAX).contains(&hts)
}

#[test]
fn the_report_rates_follow_the_frame_period_formula() {
    for v in &VERIFIED {
        let (clock, hts, vts, _) = v.report;
        assert_eq!(rate_at(clock, hts, vts), v.report_rate, "{}", v.id);
        // The sensor ran 24..34 ppm fast against the P4; the logged rates are rounded to 1 mHz.
        let lo = v.report_rate as f64 * (1.0 + 20e-6) - 1.0;
        let hi = v.report_rate as f64 * (1.0 + 40e-6) + 1.0;
        assert!(
            (lo..=hi).contains(&(v.measured as f64)),
            "{}: measured {} against {}",
            v.id,
            v.measured,
            v.report_rate
        );
        assert!(
            v.report_rate.abs_diff(v.req) <= v.tol,
            "{}: the report's setting is inside the request's tolerance",
            v.id
        );
    }
}

#[test]
fn verified_requests_build() {
    for v in &VERIFIED {
        let allowed = report_timing_allowed(v);
        match v.choice {
            Builds(expected, why) => {
                let m = build(v, v.tol).unwrap_or_else(|e| panic!("{}: {e:?}", v.id));
                assert!(m.rate_error().unsigned_abs() <= v.tol, "{}", v.id);
                assert_eq!(setting(&m), expected, "{}", v.id);
                assert_eq!(why.is_empty(), expected == v.report, "{}: {why}", v.id);
                if let Some(origin) = v.origin {
                    assert_eq!(m.origin(), origin, "{}", v.id);
                }
                // A nearer VTS than the report's when the report's timing is allowed.
                if allowed && why == NEARER {
                    assert!(m.rate_error().unsigned_abs() < v.report_rate.abs_diff(v.req));
                }
                if !allowed {
                    assert!(!why.is_empty(), "{}", v.id);
                }
            }
            Refused(error, wider, expected, why) => {
                assert!(!allowed && !why.is_empty(), "{}: {why}", v.id);
                assert_eq!(build(v, v.tol), Err(error), "{}", v.id);
                let m = build(v, wider).unwrap_or_else(|e| panic!("{}: {e:?}", v.id));
                assert_eq!(setting(&m), expected, "{}", v.id);
                assert_eq!(
                    build(v, wider - 1),
                    Err(error),
                    "{}: {wider} mHz is the least tolerance that builds",
                    v.id
                );
            }
        }
    }
}

#[test]
fn the_verified_timing_is_kept_unless_a_rule_moves_it() {
    // Every request whose report timing is allowed keeps the report's clock: the clock rule
    // and the report agree.
    for v in VERIFIED.iter().filter(|v| report_timing_allowed(v)) {
        let m = build(v, v.tol).unwrap();
        assert_eq!(m.sensor_clock(), v.report.0, "{}", v.id);
    }
}

struct Rejected {
    id: &'static str,
    format: Format,
    readout: Readout,
    size: (u16, u16),
    origin: Option<(u16, u16)>,
    req: u32,
    tol: u32,
    error: Error,
}

/// Any rate the window could run at, for the geometry refusals (the report gives none).
const ANY: u32 = 30_000;

fn rejected() -> Vec<Rejected> {
    let alignment = |width_multiple, height_multiple| Error::Alignment {
        width_multiple,
        height_multiple,
    };
    let crop_size = Error::Size {
        max_width: 1280,
        max_height: 800,
    };
    let r = |id, format, size, origin, req, tol, error| Rejected {
        id,
        format,
        readout: Crop,
        size,
        origin,
        req,
        tol,
        error,
    };
    vec![
        // The report's bound at VTS h + 51 (154.706 Hz) becomes h + 55.
        r(
            "x1",
            Raw8,
            (1280, 800),
            None,
            160_000,
            0,
            Error::TooFast {
                max: Rate::millihertz(rate_at(Mhz96, 728, 855)),
            },
        ),
        // 96 MHz needs HTS 851 for a 1600-byte line; report 132.399 Hz at VTS 851.
        r(
            "x2",
            Raw10,
            (1280, 800),
            None,
            140_000,
            0,
            Error::TooFast {
                max: Rate::millihertz(rate_at(Mhz96, 851, 855)),
            },
        ),
        // Report 247.697 Hz at VTS 531.
        r(
            "x3",
            Raw10,
            (640, 480),
            None,
            250_000,
            0,
            Error::TooFast {
                max: Rate::millihertz(rate_at(Mhz96, 728, 535)),
            },
        ),
        r("x4", Raw8, (1276, 800), None, ANY, 1_000, alignment(8, 1)),
        r("x5", Raw10, (1248, 720), None, ANY, 1_000, alignment(64, 2)),
        r("x6", Raw10, (640, 401), None, ANY, 1_000, alignment(64, 2)),
        r("x7", Raw8, (1296, 800), None, ANY, 1_000, crop_size),
        r("x8", Raw8, (0, 0), None, ANY, 1_000, crop_size),
        r(
            "x9a",
            Raw8,
            (640, 480),
            Some((700, 0)),
            ANY,
            1_000,
            Error::Window,
        ),
        r(
            "x9b",
            Raw8,
            (640, 480),
            Some((0, 400)),
            ANY,
            1_000,
            Error::Window,
        ),
        r("x10", Raw8, (48, 200), None, ANY, 1_000, crop_size),
        r(
            "x12",
            Raw8,
            (1280, 800),
            None,
            100,
            0,
            Error::TooSlow {
                min: Rate::millihertz(rate_at(Mhz80, 2912, 65535)),
            },
        ),
        r(
            "x13",
            Raw8,
            (640, 480),
            Some((101, 37)),
            ANY,
            1_000,
            Error::Window,
        ),
    ]
}

#[test]
fn must_reject_requests_are_refused() {
    for x in rejected() {
        let b = Mode::builder()
            .format(x.format)
            .size(x.size.0, x.size.1)
            .readout(x.readout);
        let b = match x.origin {
            Some((ox, oy)) => b.origin(ox, oy),
            None => b,
        };
        let got = b
            .frame_rate(Rate::millihertz(x.req), Tolerance::millihertz(x.tol))
            .build();
        assert_eq!(got, Err(x.error), "{}", x.id);
    }
    // The recomputed bounds, in hertz.
    assert_eq!(rate_at(Mhz96, 728, 855), 153_984);
    assert_eq!(rate_at(Mhz96, 851, 855), 131_781);
    assert_eq!(rate_at(Mhz96, 728, 535), 245_850);
    assert_eq!(rate_at(Mhz80, 2912, 65535), 419);
}

#[test]
fn x11_is_below_the_rate_resolution() {
    // 1280x800 at 120 Hz +- 0.0001 Hz: the tolerance rounds to 0 mHz, and HTS 738, VTS 902
    // (119.99976 Hz) rounds to 120.000 Hz, so the request builds instead of failing with
    // `Tolerance { nearest: 119.99976 }`. The crate resolves rates to 1 mHz.
    assert_eq!(Tolerance::hz(0.0001), Tolerance::millihertz(0));
    let m = Mode::builder()
        .format(Raw8)
        .size(1280, 800)
        .frame_rate(Rate::hz(120.0), Tolerance::hz(0.0001))
        .build()
        .unwrap();
    assert_eq!(setting(&m), (Mhz80, 738, 902, true));
    assert_eq!(m.frame_rate(), Rate::millihertz(120_000));
    let exact = 80e9 / frame_cycles(738, 902) as f64;
    assert!((exact - 119_999.76).abs() < 0.01, "{exact}");
}
