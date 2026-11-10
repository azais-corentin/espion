//! Every limit at its boundary (last accepted, first rejected), the solver's order of
//! preference, rate quantisation, and `const` use.

use ov9281::{
    Error, Format, Mode, Rate, Readout, SensorClock, Tolerance, Write, frame_cycles, limits,
};

use Format::{Raw8, Raw10};
use Readout::{Binned, Crop};
use SensorClock::{Mhz80, Mhz96};

/// Nominal rate at `clock`, `hts`, `vts` in millihertz, rounded (the formula of the crate docs).
fn rate_at(clock: SensorClock, hts: u16, vts: u16) -> u32 {
    let c = frame_cycles(hts, vts);
    ((clock.hz() as u64 * 2000 + c) / (2 * c)) as u32
}

fn build(
    format: Format,
    readout: Readout,
    (w, h): (u16, u16),
    req_mhz: u32,
    tol_mhz: u32,
) -> Result<Mode, Error> {
    Mode::builder()
        .format(format)
        .size(w, h)
        .readout(readout)
        .frame_rate(Rate::millihertz(req_mhz), Tolerance::millihertz(tol_mhz))
        .build()
}

fn build_at(
    format: Format,
    readout: Readout,
    (w, h): (u16, u16),
    (x, y): (u16, u16),
) -> Result<Mode, Error> {
    Mode::builder()
        .format(format)
        .size(w, h)
        .readout(readout)
        .origin(x, y)
        .frame_rate(Rate::hz(30.0), Tolerance::hz(1.0))
        .build()
}

/// Any rate the window can run at: geometry tests only.
fn geometry(format: Format, readout: Readout, size: (u16, u16)) -> Result<Mode, Error> {
    build(format, readout, size, 30_000, 1_000)
}

fn reg(mode: &Mode, addr: u16) -> u8 {
    mode.registers()
        .find_map(|w| match w {
            Write::Reg(a, v) if a == addr => Some(v),
            _ => None,
        })
        .unwrap()
}

/// Whether some VTS at `clock` and `hts` (from the window's minimum) is within `tol` of `req`,
/// by scanning the VTS values around the exact one.
fn hts_meets(clock: SensorClock, hts: u16, vmin: u16, req: u32, tol: u32) -> bool {
    let exact = clock.hz() as f64 * 1000.0 / req as f64;
    let v = ((exact - 1730.0) / hts as f64 + 1.0) as i64;
    (v - 2..=v + 2)
        .filter(|&v| v >= vmin as i64 && v <= limits::VTS_MAX as i64)
        .any(|v| rate_at(clock, hts, v as u16).abs_diff(req) <= tol)
}

#[test]
fn rate_constructors_round_to_the_millihertz() {
    assert_eq!(Rate::hz(210.0), Rate::millihertz(210_000));
    assert_eq!(Rate::hz(209.563), Rate::millihertz(209_563));
    assert_eq!(Tolerance::hz(0.1), Tolerance::millihertz(100));
    assert_eq!(Tolerance::hz(0.0001), Tolerance::millihertz(0));
    assert_eq!(Rate::hz(-1.0), Rate::millihertz(0));
    assert_eq!(Rate::hz(f32::NAN), Rate::millihertz(0));
    assert_eq!(Rate::hz(1e12), Rate::millihertz(u32::MAX));
}

#[test]
fn size_boundaries() {
    let crop = Err(Error::Size {
        max_width: 1280,
        max_height: 800,
    });
    let binned = Err(Error::Size {
        max_width: 640,
        max_height: 400,
    });
    // (format, readout, last accepted, first rejected, error)
    let cases = [
        // Width: 128 up, multiples of the format's width multiple.
        (Raw8, Crop, (128, 200), (120, 200), crop),
        (Raw8, Crop, (1280, 200), (1288, 200), crop),
        (Raw10, Crop, (128, 200), (64, 200), crop),
        (Raw10, Crop, (1280, 200), (1344, 200), crop),
        (Raw8, Binned, (128, 200), (120, 200), binned),
        (Raw8, Binned, (640, 200), (648, 200), binned),
        (Raw10, Binned, (640, 200), (704, 200), binned),
        // Height: 2 up (1 was not characterised).
        (Raw8, Crop, (640, 2), (640, 1), crop),
        (Raw8, Crop, (640, 800), (640, 801), crop),
        (Raw10, Crop, (640, 800), (640, 802), crop),
        (Raw8, Binned, (320, 2), (320, 1), binned),
        (Raw8, Binned, (320, 400), (320, 401), binned),
        (Raw10, Binned, (320, 400), (320, 402), binned),
    ];
    for (format, readout, good, bad, err) in cases {
        let m = geometry(format, readout, good)
            .unwrap_or_else(|e| panic!("{format:?} {readout:?} {good:?}: {e:?}"));
        assert_eq!((m.width(), m.height()), good);
        assert_eq!(geometry(format, readout, bad), err, "{format:?} {bad:?}");
    }
    assert_eq!(geometry(Raw8, Crop, (0, 0)), crop);
}

#[test]
fn alignment_boundaries() {
    let raw8 = Err(Error::Alignment {
        width_multiple: 8,
        height_multiple: 1,
    });
    let raw10 = Err(Error::Alignment {
        width_multiple: 64,
        height_multiple: 2,
    });
    // (format, readout, accepted, rejected, error)
    let cases = [
        (Raw8, Crop, (1272, 800), (1276, 800), raw8),
        (Raw8, Crop, (136, 800), (132, 800), raw8),
        (Raw8, Binned, (632, 400), (636, 400), raw8),
        (Raw10, Crop, (1216, 720), (1248, 720), raw10),
        (Raw10, Crop, (192, 720), (136, 720), raw10),
        (Raw10, Crop, (640, 400), (640, 401), raw10),
        (Raw10, Binned, (320, 2), (320, 3), raw10),
    ];
    for (format, readout, good, bad, err) in cases {
        geometry(format, readout, good)
            .unwrap_or_else(|e| panic!("{format:?} {readout:?} {good:?}: {e:?}"));
        assert_eq!(geometry(format, readout, bad), err, "{format:?} {bad:?}");
    }
    // RAW8 heights need no multiple.
    geometry(Raw8, Crop, (640, 799)).unwrap();
    geometry(Raw8, Binned, (640, 3)).unwrap();
}

#[test]
fn window_boundaries() {
    // (readout, size, accepted origin, rejected origin)
    let cases = [
        // x: a multiple of 8.
        (Crop, (640, 480), (8, 0), (4, 0)),
        (Crop, (640, 480), (96, 37), (101, 37)),
        (Binned, (320, 200), (8, 0), (4, 0)),
        // y: any row in crop, even when binned.
        (Crop, (640, 480), (0, 1), (0, 321)),
        (Binned, (320, 200), (0, 2), (0, 1)),
        // The right and bottom edges of the active area.
        (Crop, (640, 480), (640, 0), (648, 0)),
        (Crop, (640, 480), (0, 320), (0, 400)),
        (Crop, (640, 480), (632, 0), (700, 0)),
        (Binned, (320, 200), (640, 400), (648, 400)),
        (Binned, (320, 200), (640, 400), (640, 402)),
        (Binned, (640, 400), (0, 0), (8, 0)),
    ];
    for (readout, size, good, bad) in cases {
        let m = build_at(Raw8, readout, size, good)
            .unwrap_or_else(|e| panic!("{readout:?} {size:?} at {good:?}: {e:?}"));
        assert_eq!(m.origin(), good);
        assert_eq!(
            build_at(Raw8, readout, size, bad),
            Err(Error::Window),
            "{readout:?} {size:?} at {bad:?}"
        );
    }
    // The far corner reads the last array column and row.
    let m = build_at(Raw8, Binned, (320, 200), (640, 400)).unwrap();
    assert_eq!(reg(&m, 0x3804), 0x05); // x end 1295
    assert_eq!(reg(&m, 0x3805), 0x0F);
    assert_eq!(reg(&m, 0x3806), 0x03); // y end 815
    assert_eq!(reg(&m, 0x3807), 0x2F);
}

#[test]
fn default_origin_is_centred_and_rounded_down_to_the_grid() {
    let cases = [
        (Crop, (640, 480), (320, 160)),
        (Crop, (1280, 64), (0, 368)),
        (Crop, (1272, 799), (0, 0)),    // x 4 -> 0
        (Crop, (624, 481), (328, 159)), // y stays odd in crop
        (Crop, (136, 200), (568, 300)), // x 572 -> 568
        (Binned, (320, 200), (320, 200)),
        (Binned, (312, 199), (328, 200)), // y 201 -> 200
    ];
    for (readout, size, origin) in cases {
        let m = geometry(Raw8, readout, size).unwrap();
        assert_eq!(m.origin(), origin, "{readout:?} {size:?}");
    }
}

#[test]
fn minimum_vts_is_the_measured_rule_plus_the_margin() {
    assert_eq!(limits::VTS_MARGIN, 4);
    for (format, readout, size, measured) in [
        (Raw8, Crop, (1280, 800), 851),
        (Raw10, Crop, (1280, 720), 771),
        (Raw8, Crop, (1280, 2), 53),
        (Raw8, Binned, (640, 400), 431),
        (Raw10, Binned, (320, 200), 231),
        (Raw8, Binned, (640, 2), 33),
    ] {
        let (w, h) = size;
        assert_eq!(limits::measured_min_vts(readout, h), measured);
        let vmin = measured + 4;
        assert_eq!(limits::min_vts(readout, h), vmin);
        let hts80 = limits::hts_min(format.line_bytes(w), Mhz80);
        let hts96 = limits::hts_min(format.line_bytes(w), Mhz96);
        // Last accepted at 80 MHz: the fastest 80 MHz rate runs at the minimum VTS.
        let max80 = rate_at(Mhz80, hts80, vmin);
        let m = build(format, readout, size, max80, 0).unwrap();
        assert_eq!((m.sensor_clock(), m.hts(), m.vts()), (Mhz80, hts80, vmin));
        // First rejected: the rate of one line less is not served at 80 MHz.
        let m = build(
            format,
            readout,
            size,
            rate_at(Mhz80, hts80, vmin - 1),
            1_000,
        )
        .unwrap();
        assert_eq!(m.sensor_clock(), Mhz96, "{size:?}");
        // The same minimum at 96 MHz bounds the fastest rate.
        let max96 = rate_at(Mhz96, hts96, vmin);
        let m = build(format, readout, size, max96, 0).unwrap();
        assert_eq!((m.sensor_clock(), m.hts(), m.vts()), (Mhz96, hts96, vmin));
        assert_eq!(
            build(format, readout, size, max96 + 1, 0),
            Err(Error::TooFast {
                max: Rate::millihertz(max96)
            })
        );
        // A rate just above the maximum is served by it when the tolerance allows.
        let m = build(format, readout, size, max96 + 5, 5).unwrap();
        assert_eq!(m.vts(), vmin);
    }
}

#[test]
fn minimum_vts_holds_at_longer_lines() {
    // A request that needs a longer line never goes below the minimum VTS: the slowest
    // frames of the window at 80 MHz are just above the fastest of a longer HTS.
    let vmin = limits::min_vts(Crop, 800);
    for hts in [729, 800, 1000, 1456, 2912] {
        let req = rate_at(Mhz80, hts, vmin);
        let m = build(Raw8, Crop, (1280, 800), req, 0).unwrap();
        assert!(m.vts() >= vmin && m.hts() <= hts, "HTS {hts}: {m:?}");
    }
}

#[test]
fn hts_minimum_follows_the_link_only_for_wide_raw10_at_96_mhz() {
    assert_eq!(limits::HTS_SENSOR_MIN, 728);
    // ceil(line bytes x MHz / 200) + 75, the measured link rule.
    assert_eq!(limits::link_min_hts(1600, Mhz96), 843);
    assert_eq!(limits::link_min_hts(1600, Mhz80), 715);
    // (format, width, clock, minimum HTS)
    for (format, w, clock, hts) in [
        (Raw10, 1280, Mhz96, 851),
        (Raw10, 1216, Mhz96, 813),
        (Raw10, 1152, Mhz96, 775),
        (Raw10, 1088, Mhz96, 736),
        (Raw10, 1024, Mhz96, 728),
        (Raw10, 1280, Mhz80, 728),
        (Raw8, 1280, Mhz96, 728),
    ] {
        assert_eq!(limits::hts_min(format.line_bytes(w), clock), hts, "{w}");
        // The fastest mode of the window uses it, at 96 MHz.
        let Err(Error::TooFast { max }) = build(format, Crop, (w, 800), u32::MAX, 0) else {
            panic!("expected TooFast")
        };
        let m = build(format, Crop, (w, 800), max.as_millihertz(), 0).unwrap();
        assert_eq!(m.sensor_clock(), Mhz96);
        assert_eq!(m.hts(), limits::hts_min(format.line_bytes(w), Mhz96));
    }
}

#[test]
fn too_slow_is_the_longest_frame_at_80_mhz() {
    let min = rate_at(Mhz80, limits::HTS_MAX, limits::VTS_MAX);
    assert_eq!(min, 419);
    for (format, readout, size) in [(Raw8, Crop, (640, 480)), (Raw10, Binned, (320, 200))] {
        // Served at 80 MHz (a slightly shorter line with VTS 65535 rounds to the same rate).
        let m = build(format, readout, size, min, 0).unwrap();
        assert_eq!(
            (m.sensor_clock(), m.frame_rate()),
            (Mhz80, Rate::millihertz(min))
        );
        let too_slow = Err(Error::TooSlow {
            min: Rate::millihertz(min),
        });
        assert_eq!(build(format, readout, size, min - 1, 0), too_slow);
        assert_eq!(build(format, readout, size, 0, 0), too_slow);
        // Within the tolerance of the slowest it is served.
        let m = build(format, readout, size, min - 3, 3).unwrap();
        assert!(m.rate_error().unsigned_abs() <= 3);
    }
    // VTS 65535 is in range: at HTS 728 its rate (1.677 Hz) is served at HTS 728 (several
    // VTS near 65535 round to the same millihertz).
    let req = rate_at(Mhz80, 728, limits::VTS_MAX);
    assert_eq!(req, 1_677);
    let m = build(Raw8, Crop, (1280, 800), req, 0).unwrap();
    assert_eq!((m.hts(), m.frame_rate()), (728, Rate::millihertz(req)));
    assert!(m.vts() > 65_500);
}

#[test]
fn psv_boundaries() {
    // (readout, size, last VTS with PSV off, first with PSV on)
    for (readout, size, off, on) in [
        (Crop, (1280, 720), 814, 815),
        (Crop, (1280, 64), 158, 159),
        (Binned, (640, 400), 474, 475),
        (Binned, (320, 200), 274, 275),
    ] {
        assert_eq!(limits::measured_psv_min_vts(readout, size.1) + 4, on);
        for format in [Raw8, Raw10] {
            for (vts, psv) in [(off, false), (on, true), (on + 1000, true)] {
                let m = build(format, readout, size, rate_at(Mhz80, 728, vts), 0).unwrap();
                assert_eq!((m.vts(), m.psv()), (vts, psv), "{size:?}");
                assert_eq!(reg(&m, 0x4F00), if psv { 0x0C } else { 0x00 });
            }
        }
        // The same rule in lines at 96 MHz, where those rates are above the 80 MHz maximum.
        if rate_at(Mhz96, 728, off) > rate_at(Mhz80, 728, limits::min_vts(readout, size.1)) {
            let m = build(Raw8, readout, size, rate_at(Mhz96, 728, off), 0).unwrap();
            assert_eq!((m.sensor_clock(), m.vts(), m.psv()), (Mhz96, off, false));
            let m = build(Raw8, readout, size, rate_at(Mhz96, 728, on), 0).unwrap();
            assert_eq!((m.sensor_clock(), m.vts(), m.psv()), (Mhz96, on, true));
        }
    }
    // Nothing below the rule turns PSV on, not even the measured narrow good windows
    // (crop h + 51..54 and h + 58).
    for vts in limits::min_vts(Crop, 720)..815 {
        assert!(!limits::psv_on(Crop, 720, vts), "{vts}");
    }
}

#[test]
fn vfifo_read_start_is_a_sixteenth_of_the_width() {
    // (format, readout, width, 0x4601)
    for (format, readout, w, vf) in [
        (Raw8, Crop, 1280, 80),
        (Raw10, Crop, 1280, 80),
        (Raw8, Crop, 1272, 79),
        (Raw10, Crop, 1216, 76),
        (Raw8, Crop, 136, 8),
        (Raw8, Crop, 128, 8),
        (Raw10, Binned, 640, 40),
        (Raw8, Binned, 320, 20),
    ] {
        assert_eq!(limits::vfifo_read_start(w), vf);
        // 30 Hz runs at 80 MHz, 1 Hz above the 80 MHz maximum at 96 MHz.
        let max80 = rate_at(Mhz80, 728, limits::min_vts(readout, 200));
        for (req, clock) in [(30_000, Mhz80), (max80 + 1_000, Mhz96)] {
            let m = build(format, readout, (w, 200), req, 1_000).unwrap();
            assert_eq!(m.sensor_clock(), clock);
            assert_eq!(reg(&m, 0x4601), vf, "{w} at {clock:?}");
        }
    }
}

#[test]
fn exposure_stays_25_lines_below_vts() {
    let m = build(Raw8, Binned, (320, 200), rate_at(Mhz80, 728, 235), 0).unwrap();
    assert_eq!(m.max_exposure_lines(), 235 - 25);
}

#[test]
fn the_preferred_line_is_kept_when_it_meets_the_tolerance() {
    // 1280x800 at 120 Hz +- 0.1: VTS 914 at HTS 728 is 49 mHz off.
    let m = build(Raw8, Crop, (1280, 800), 120_000, 100).unwrap();
    assert_eq!((m.sensor_clock(), m.hts(), m.vts()), (Mhz80, 728, 914));
    // The nearest VTS at HTS 728, even when a longer line would hit the rate exactly.
    assert_eq!(m.frame_rate(), Rate::millihertz(rate_at(Mhz80, 728, 914)));
}

#[test]
fn the_line_is_lengthened_only_as_far_as_the_tolerance_needs() {
    // (readout, size, request, tolerance, expected HTS)
    for (readout, size, req, tol, hts) in [
        (Crop, (1280, 800), 120_000, 1, 738),
        (Crop, (1280, 800), 120_000, 0, 738),
        (Binned, (320, 200), 128_300, 0, 735),
    ] {
        let vmin = limits::min_vts(readout, size.1);
        let m = build(Raw8, readout, size, req, tol).unwrap();
        assert_eq!((m.sensor_clock(), m.hts()), (Mhz80, hts), "{req}");
        assert!(m.rate_error().unsigned_abs() <= tol);
        for shorter in 728..hts {
            assert!(!hts_meets(Mhz80, shorter, vmin, req, tol), "HTS {shorter}");
        }
    }
    // 1280x720 at a 96 MHz step below the 80 MHz maximum, +- 10 mHz: 80 MHz, one cycle longer.
    let req = rate_at(Mhz96, 728, 1000);
    assert!(req < rate_at(Mhz80, 728, limits::min_vts(Crop, 720)));
    let m = build(Raw8, Crop, (1280, 720), req, 10).unwrap();
    assert_eq!((m.sensor_clock(), m.hts()), (Mhz80, 729));
    assert!(!hts_meets(Mhz80, 728, limits::min_vts(Crop, 720), req, 10));
}

#[test]
fn ninety_six_mhz_only_above_the_80_mhz_maximum() {
    for (format, readout, size) in [
        (Raw8, Crop, (1280, 800)),
        (Raw10, Crop, (1280, 800)),
        (Raw8, Binned, (320, 200)),
    ] {
        let vmin = limits::min_vts(readout, size.1);
        let max80 = rate_at(Mhz80, 728, vmin);
        // At the maximum: 80 MHz, with any tolerance.
        for tol in [0, 1_000, 100_000] {
            let m = build(format, readout, size, max80, tol).unwrap();
            assert_eq!(m.sensor_clock(), Mhz80, "{size:?} tol {tol}");
            assert!(!m.exceeds_datasheet_clock());
        }
        // 1 mHz above it: 96 MHz, even when the 80 MHz maximum is within the tolerance.
        let m = build(format, readout, size, max80 + 1, 100_000).unwrap();
        assert_eq!(m.sensor_clock(), Mhz96, "{size:?}");
        assert!(m.exceeds_datasheet_clock());
        assert_eq!(reg(&m, 0x030D), 0x60);
    }
}

#[test]
fn tolerance_error_gives_the_nearest_rate_of_the_selected_clock() {
    // (format, readout, size, request): no HTS and VTS at the clock is within 0 mHz.
    for (size, req, clock) in [
        ((320, 200), 464_000, Mhz80),
        ((320, 200), 300_000, Mhz80),
        ((320, 200), 557_000, Mhz96),
    ] {
        let Err(Error::Tolerance { nearest }) = build(Raw8, Binned, size, req, 0) else {
            panic!("{req}: expected Tolerance")
        };
        // The nearest over every HTS and VTS of that clock, by brute force.
        let vmin = limits::min_vts(Binned, size.1);
        let brute = (limits::HTS_SENSOR_MIN..=limits::HTS_MAX)
            .flat_map(|hts| {
                let exact = clock.hz() as f64 * 1000.0 / req as f64;
                let v = ((exact - 1730.0) / hts as f64 + 1.0) as u16;
                (v.saturating_sub(1).max(vmin)..=v + 2).map(move |v| rate_at(clock, hts, v))
            })
            .min_by_key(|&r| (r.abs_diff(req), r))
            .unwrap();
        assert_eq!(nearest.as_millihertz(), brute, "{req}");
        // Widening the tolerance to it serves it.
        let off = brute.abs_diff(req);
        let m = build(Raw8, Binned, size, req, off).unwrap();
        assert_eq!(
            (m.sensor_clock(), m.rate_error().unsigned_abs()),
            (clock, off)
        );
        assert_eq!(
            build(Raw8, Binned, size, req, off - 1),
            Err(Error::Tolerance { nearest })
        );
    }
}

#[test]
fn the_nearest_vts_is_chosen() {
    let size = (640, 400);
    let (fast, slow) = (rate_at(Mhz80, 728, 522), rate_at(Mhz80, 728, 523));
    let vts = |req| build(Raw8, Binned, size, req, 1_000).unwrap().vts();
    assert_eq!(vts(fast), 522);
    assert_eq!(vts(slow), 523);
    let mid = (fast + slow) / 2;
    assert_eq!(vts(mid + 1), 522);
    assert_eq!(vts(mid - 1), 523);
}

const MODE: Mode = match Mode::builder()
    .format(Format::Raw10)
    .size(640, 400)
    .readout(Readout::Binned)
    .frame_rate(Rate::hz(210.0), Tolerance::hz(0.1))
    .build()
{
    Ok(m) => m,
    Err(_) => panic!(),
};

/// The longest search: no HTS meets the request, so every one is tried.
const LONGEST: Result<Mode, Error> = Mode::builder()
    .format(Format::Raw8)
    .size(320, 200)
    .readout(Readout::Binned)
    .frame_rate(Rate::millihertz(464_000), Tolerance::millihertz(0))
    .build();

const REFUSED: Result<Mode, Error> = Mode::builder()
    .format(Format::Raw8)
    .size(640, 400)
    .readout(Readout::Binned)
    .origin(320, 200)
    .frame_rate(Rate::millihertz(209_563), Tolerance::millihertz(100))
    .build();

#[test]
fn builds_in_const() {
    let runtime = build(Raw10, Binned, (640, 400), 210_000, 100).unwrap();
    assert_eq!(MODE, runtime);
    assert_eq!(MODE.vts(), 522);
    assert!(matches!(LONGEST, Err(Error::Tolerance { .. })));
    assert_eq!(REFUSED, Err(Error::Window));
}

#[test]
fn derived_figures() {
    let m = build(Raw10, Crop, (1280, 720), 119_918, 1).unwrap();
    assert_eq!(m.bytes_per_line(), 1600);
    assert_eq!(m.frame_bytes(), 1_152_000);
    assert_eq!(m.line_ns(), 9100);
    assert_eq!(m.max_exposure_lines(), 890);
    assert_eq!(m.default_exposure_lines(), 110);
    assert_eq!(m.format().data_type(), 0x2B);
    let m = build(Raw8, Crop, (1280, 720), 143_901, 1).unwrap();
    assert_eq!(m.line_ns(), 7583);
    assert_eq!(m.default_exposure_lines(), 132);
    assert_eq!(m.format().data_type(), 0x2A);
}
