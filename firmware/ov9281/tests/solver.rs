//! Requests that must fail with each error, rate quantisation and tolerance boundaries, the
//! clock and PSV choices, and `const` use.

use ov9281::{Error, Format, Mode, Rate, Readout, SensorClock, Tolerance, Write, frame_cycles};

/// Nominal rate at `mhz`, `hts`, `vts` in millihertz, rounded (the formula of the crate docs).
fn rate_at(mhz: u64, hts: u16, vts: u16) -> u32 {
    let c = frame_cycles(hts, vts);
    ((mhz * 1_000_000_000 * 2 + c) / (2 * c)) as u32
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

fn reg(mode: &Mode, addr: u16) -> u8 {
    mode.registers()
        .find_map(|w| match w {
            Write::Reg(a, v) if a == addr => Some(v),
            _ => None,
        })
        .unwrap()
}

#[test]
fn rate_constructors_round_to_the_millihertz() {
    assert_eq!(Rate::hz(210.0), Rate::millihertz(210_000));
    assert_eq!(Rate::hz(209.563), Rate::millihertz(209_563));
    assert_eq!(Tolerance::hz(0.1), Tolerance::millihertz(100));
    assert_eq!(Rate::hz(-1.0), Rate::millihertz(0));
    assert_eq!(Rate::hz(f32::NAN), Rate::millihertz(0));
    assert_eq!(Rate::hz(1e12), Rate::millihertz(u32::MAX));
}

#[test]
fn size_errors() {
    let crop = Err(Error::Size {
        max_width: 1280,
        max_height: 800,
    });
    for size in [(0, 400), (640, 0), (1284, 800), (1280, 801), (28, 400)] {
        assert_eq!(
            build(Format::Raw8, Readout::Crop, size, 120_000, 1_000_000),
            crop,
            "{size:?}"
        );
    }
    let binned = Err(Error::Size {
        max_width: 640,
        max_height: 400,
    });
    for size in [(644, 400), (640, 401), (1280, 800)] {
        assert_eq!(
            build(Format::Raw10, Readout::Binned, size, 120_000, 1_000_000),
            binned,
            "{size:?}"
        );
    }
}

#[test]
fn alignment_errors() {
    for format in [Format::Raw8, Format::Raw10] {
        assert_eq!(
            build(format, Readout::Crop, (642, 400), 120_000, 1_000_000),
            Err(Error::Alignment { width_multiple: 4 })
        );
    }
}

#[test]
fn window_errors() {
    let req = |readout, size: (u16, u16), x, y| {
        Mode::builder()
            .format(Format::Raw8)
            .size(size.0, size.1)
            .readout(readout)
            .origin(x, y)
            .frame_rate(Rate::hz(100.0), Tolerance::hz(50.0))
            .build()
    };
    assert_eq!(req(Readout::Crop, (640, 480), 642, 0), Err(Error::Window));
    assert_eq!(req(Readout::Crop, (640, 480), 0, 322), Err(Error::Window));
    assert_eq!(req(Readout::Crop, (640, 480), 1, 0), Err(Error::Window));
    assert_eq!(req(Readout::Binned, (640, 400), 2, 0), Err(Error::Window));
    assert_eq!(req(Readout::Binned, (320, 200), 642, 0), Err(Error::Window));
    // The far corner fits exactly.
    let m = req(Readout::Binned, (320, 200), 640, 400).unwrap();
    assert_eq!(m.origin(), (640, 400));
    assert_eq!(reg(&m, 0x3804), 0x05); // x end 1295
    assert_eq!(reg(&m, 0x3805), 0x0F);
    assert_eq!(reg(&m, 0x3806), 0x03); // y end 815
    assert_eq!(reg(&m, 0x3807), 0x2F);
}

#[test]
fn default_origin_is_centred() {
    let m = build(Format::Raw8, Readout::Crop, (640, 480), 120_000, 1_000).unwrap();
    assert_eq!(m.origin(), (320, 160));
    let m = build(Format::Raw8, Readout::Binned, (320, 200), 120_000, 1_000).unwrap();
    assert_eq!(m.origin(), (320, 200));
    let m = build(Format::Raw8, Readout::Crop, (1276, 798), 60_000, 1_000).unwrap();
    assert_eq!(m.origin(), (2, 0));
}

#[test]
fn too_fast_carries_the_fastest_rate_of_the_window() {
    let size = (320, 200);
    let Err(Error::TooFast { max }) = build(Format::Raw8, Readout::Binned, size, 1_000_000, 0)
    else {
        panic!("expected TooFast")
    };
    // The 320x200 binned window: VTS 232 at 80 MHz (the 96 MHz floor is longer).
    assert_eq!(max.as_millihertz(), rate_at(80, 728, 232));
    let m = build(Format::Raw8, Readout::Binned, size, max.as_millihertz(), 0).unwrap();
    assert_eq!((m.vts(), m.sensor_clock()), (232, SensorClock::Mhz80));
    assert_eq!(
        build(
            Format::Raw8,
            Readout::Binned,
            size,
            max.as_millihertz() + 1,
            0
        ),
        Err(Error::TooFast { max })
    );
    // A VTS below the minimum is never used, even when it would be nearer.
    let below = rate_at(80, 728, 231);
    assert_eq!(
        build(Format::Raw8, Readout::Binned, size, below, 0),
        Err(Error::TooFast { max })
    );
}

#[test]
fn too_fast_at_96_mhz_when_the_link_cannot_carry_the_line() {
    // RAW10 1280 wide (1600 B) does not fit a 96 MHz line: the fastest is at 80 MHz, VTS 860.
    let r = build(Format::Raw10, Readout::Crop, (1280, 800), 143_901, 100);
    let max = rate_at(80, 728, 816 + 44);
    assert_eq!(
        r,
        Err(Error::TooFast {
            max: Rate::millihertz(max)
        })
    );
    // RAW8 fits, so the same request runs at 96 MHz.
    let m = build(Format::Raw8, Readout::Crop, (1280, 800), 143_901, 100).unwrap();
    assert!(m.exceeds_datasheet_clock());
}

#[test]
fn too_slow_carries_the_slowest_rate() {
    let Err(Error::TooSlow { min }) = build(Format::Raw8, Readout::Crop, (640, 480), 500, 0) else {
        panic!("expected TooSlow")
    };
    assert_eq!(min.as_millihertz(), rate_at(80, 728, 0xFFFF));
    let m = build(
        Format::Raw8,
        Readout::Crop,
        (640, 480),
        min.as_millihertz(),
        0,
    )
    .unwrap();
    assert_eq!(m.frame_rate(), min);
    assert_eq!(
        build(
            Format::Raw8,
            Readout::Crop,
            (640, 480),
            min.as_millihertz() - 1,
            0
        ),
        Err(Error::TooSlow { min })
    );
    assert_eq!(
        build(Format::Raw8, Readout::Crop, (640, 480), 0, 0),
        Err(Error::TooSlow { min })
    );
}

#[test]
fn tolerance_boundary() {
    let req = 210_000;
    let size = (640, 400);
    let Err(Error::Tolerance { nearest }) = build(Format::Raw8, Readout::Binned, size, req, 0)
    else {
        panic!("expected Tolerance")
    };
    assert_eq!(nearest.as_millihertz(), rate_at(80, 728, 522));
    let off = nearest.as_millihertz().abs_diff(req);
    let m = build(Format::Raw8, Readout::Binned, size, req, off).unwrap();
    assert_eq!(m.frame_rate(), nearest);
    assert_eq!(m.rate_error(), nearest.as_millihertz() as i32 - req as i32);
    assert_eq!(
        build(Format::Raw8, Readout::Binned, size, req, off - 1),
        Err(Error::Tolerance { nearest })
    );
}

#[test]
fn the_nearest_vts_is_chosen() {
    let size = (640, 400);
    let (fast, slow) = (rate_at(80, 728, 522), rate_at(80, 728, 523));
    let vts = |req| {
        build(Format::Raw8, Readout::Binned, size, req, 1_000)
            .unwrap()
            .vts()
    };
    assert_eq!(vts(fast), 522);
    assert_eq!(vts(slow), 523);
    let mid = (fast + slow) / 2;
    assert_eq!(vts(mid + 1), 522);
    assert_eq!(vts(mid - 1), 523);
}

#[test]
fn clock_is_80_mhz_unless_only_96_mhz_meets_the_request() {
    // Within 80 MHz reach.
    let m = build(Format::Raw8, Readout::Crop, (1280, 800), 127_000, 1_000).unwrap();
    assert_eq!(m.sensor_clock(), SensorClock::Mhz80);
    assert!(!m.exceeds_datasheet_clock());
    // Above it.
    let m = build(Format::Raw8, Readout::Crop, (1280, 800), 143_901, 1).unwrap();
    assert_eq!((m.sensor_clock(), m.vts()), (SensorClock::Mhz96, 915));
    assert_eq!(reg(&m, 0x030D), 0x60);
    // Below the 80 MHz maximum, but between two 80 MHz steps with a tight tolerance: 96 MHz
    // has a step there (VTS 1000), so it is used.
    let at96 = rate_at(96, 728, 1000);
    assert!(at96 < rate_at(80, 728, 780));
    let m = build(Format::Raw8, Readout::Crop, (1280, 720), at96, 10).unwrap();
    assert_eq!((m.sensor_clock(), m.vts()), (SensorClock::Mhz96, 1000));
    // A wide tolerance keeps 80 MHz.
    let m = build(Format::Raw8, Readout::Crop, (1280, 720), at96, 1_000).unwrap();
    assert_eq!(m.sensor_clock(), SensorClock::Mhz80);
    // 96 MHz never goes below VTS 915, the only VTS measured there.
    let r = build(
        Format::Raw8,
        Readout::Crop,
        (1280, 800),
        rate_at(96, 728, 914),
        0,
    );
    assert_eq!(
        r,
        Err(Error::TooFast {
            max: Rate::millihertz(rate_at(96, 728, 915))
        })
    );
}

#[test]
fn psv_is_on_only_where_measured_safe() {
    let cases = [
        // (readout, size, vts, psv)
        (Readout::Crop, (1280, 720), 813, false),
        (Readout::Crop, (1280, 720), 814, true),
        (Readout::Crop, (1280, 720), 780, false),
        (Readout::Binned, (640, 400), 433, false),
        (Readout::Binned, (640, 400), 434, true),
        (Readout::Binned, (320, 200), 241, false),
        (Readout::Binned, (320, 200), 260, false),
    ];
    for (readout, size, vts, psv) in cases {
        let m = build(Format::Raw8, readout, size, rate_at(80, 728, vts), 0).unwrap();
        assert_eq!((m.vts(), m.psv()), (vts, psv), "{size:?}");
        assert_eq!(reg(&m, 0x4F00), if psv { 0x0C } else { 0x00 });
    }
    // Binned at 96 MHz was never measured with PSV on.
    let at96 = rate_at(96, 728, 1000);
    let m = build(Format::Raw8, Readout::Binned, (640, 400), at96, 0).unwrap();
    assert_eq!((m.sensor_clock(), m.vts()), (SensorClock::Mhz96, 1000));
    assert!(!m.psv());
}

#[test]
fn minimum_vts_follows_the_rows_read() {
    // 1280x720 crop: 736 rows; fastest at 80 MHz is VTS 780, at 96 MHz VTS 915.
    let m = build(
        Format::Raw8,
        Readout::Crop,
        (1280, 720),
        rate_at(80, 728, 780),
        0,
    )
    .unwrap();
    assert_eq!((m.sensor_clock(), m.vts()), (SensorClock::Mhz80, 780));
    let r = build(Format::Raw8, Readout::Crop, (1280, 720), 200_000, 0);
    assert_eq!(
        r,
        Err(Error::TooFast {
            max: Rate::millihertz(rate_at(96, 728, 915))
        })
    );
}

#[test]
fn vfifo_read_start_fits_the_line() {
    let m = build(Format::Raw8, Readout::Binned, (320, 200), 453_386, 0).unwrap();
    assert_eq!(reg(&m, 0x4601), 0x04);
    let m = build(Format::Raw8, Readout::Crop, (384, 200), 120_000, 1_000).unwrap();
    assert_eq!(reg(&m, 0x4601), 0x30);
    let m = build(Format::Raw8, Readout::Crop, (380, 200), 120_000, 1_000).unwrap();
    assert_eq!(reg(&m, 0x4601), 0x04);
    let m = build(Format::Raw10, Readout::Crop, (1280, 800), 119_918, 1).unwrap();
    assert_eq!(reg(&m, 0x4601), 0x04);
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

const REFUSED: Result<Mode, Error> = Mode::builder()
    .format(Format::Raw8)
    .size(640, 400)
    .readout(Readout::Binned)
    .origin(320, 200)
    .frame_rate(Rate::millihertz(209_563), Tolerance::millihertz(100))
    .build();

#[test]
fn builds_in_const() {
    let runtime = build(Format::Raw10, Readout::Binned, (640, 400), 210_000, 100).unwrap();
    assert_eq!(MODE, runtime);
    assert_eq!(MODE.vts(), 522);
    assert_eq!(REFUSED, Err(Error::Window));
}

#[test]
fn derived_figures() {
    let m = build(Format::Raw10, Readout::Crop, (1280, 720), 119_918, 1).unwrap();
    assert_eq!(m.bytes_per_line(), 1600);
    assert_eq!(m.frame_bytes(), 1_152_000);
    assert_eq!(m.line_ns(), 9100);
    assert_eq!(m.max_exposure_lines(), 890);
    assert_eq!(m.default_exposure_lines(), 110);
    assert_eq!(m.format().data_type(), 0x2B);
    let m = build(Format::Raw8, Readout::Crop, (1280, 720), 143_901, 1).unwrap();
    assert_eq!(m.line_ns(), 7583);
    assert_eq!(m.default_exposure_lines(), 132);
    assert_eq!(m.format().data_type(), 0x2A);
}
