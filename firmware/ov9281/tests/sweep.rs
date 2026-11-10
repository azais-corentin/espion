//! Property-style sweeps over formats, readouts, sizes, rates and tolerances: every mode the
//! solver returns keeps the invariants, and every refusal is justified.

use std::collections::HashMap;

use ov9281::{
    Error, Format, Mode, Rate, Readout, SensorClock, Tolerance, Write, frame_cycles, limits,
};

const FORMATS: [Format; 2] = [Format::Raw8, Format::Raw10];
const READOUTS: [Readout; 2] = [Readout::Crop, Readout::Binned];
const WIDTHS: [u16; 10] = [0, 28, 32, 64, 100, 318, 320, 636, 640, 1280];
const HEIGHTS: [u16; 10] = [0, 1, 7, 100, 200, 399, 400, 480, 720, 800];
const RATES_MHZ: [u32; 17] = [
    0, 1_000, 1_676, 1_677, 30_000, 60_000, 119_918, 127_000, 143_901, 166_000, 209_563, 250_000,
    300_000, 453_386, 470_000, 480_000, 1_000_000,
];
const TOLERANCES_MHZ: [u32; 4] = [0, 1, 100, 10_000];

fn rate_at(clock: SensorClock, hts: u16, vts: u16) -> u32 {
    let c = frame_cycles(hts, vts);
    ((clock.hz() as u64 * 2000 + c) / (2 * c)) as u32
}

fn build(
    format: Format,
    readout: Readout,
    w: u16,
    h: u16,
    req: u32,
    tol: u32,
) -> Result<Mode, Error> {
    Mode::builder()
        .format(format)
        .size(w, h)
        .readout(readout)
        .frame_rate(Rate::millihertz(req), Tolerance::millihertz(tol))
        .build()
}

/// Some setting at `clock` (any HTS of the range, VTS at or above the minimum) within `tol`.
fn reachable(clock: SensorClock, m: &Mode, req: u32, tol: u32) -> bool {
    let vmin = limits::min_vts(m.readout(), m.rows_read(), clock, false).unwrap();
    (limits::HTS_MIN..=limits::HTS_MAX)
        .filter(|&hts| limits::link_fits(m.bytes_per_line(), hts, clock))
        .any(|hts| {
            (vmin..=limits::VTS_MAX)
                .map(|v| rate_at(clock, hts, v))
                .take_while(|&r| r as u64 + tol as u64 >= req as u64)
                .any(|r| r.abs_diff(req) <= tol)
        })
}

fn check_mode(m: &Mode, req: u32, tol: u32) {
    let ctx = format!("{m:?} req {req} tol {tol}");
    let clock = m.sensor_clock();
    // Rate.
    assert_eq!(
        m.frame_rate().as_millihertz(),
        rate_at(clock, m.hts(), m.vts()),
        "{ctx}"
    );
    assert!(m.rate_error().unsigned_abs() <= tol, "{ctx}");
    // Timing limits.
    let vmin = limits::min_vts(m.readout(), m.rows_read(), clock, false).unwrap();
    assert!(m.vts() >= vmin, "{ctx}");
    assert!(
        (limits::HTS_MIN..=limits::HTS_MAX).contains(&m.hts()),
        "{ctx}"
    );
    assert_eq!(
        m.psv(),
        limits::psv_on(m.readout(), m.rows_read(), clock, m.vts()),
        "{ctx}"
    );
    if m.psv() {
        let psv_min = limits::min_vts(m.readout(), m.rows_read(), clock, true).unwrap();
        assert!(m.vts() >= psv_min, "{ctx}");
    }
    assert!(
        limits::link_fits(m.bytes_per_line(), m.hts(), clock),
        "{ctx}"
    );
    assert_eq!(m.max_exposure_lines(), m.vts() - 25, "{ctx}");
    assert!(
        (1..=m.max_exposure_lines()).contains(&m.default_exposure_lines()),
        "{ctx}"
    );
    // Nearest VTS at the chosen clock and HTS.
    let err = |v: u16| rate_at(clock, m.hts(), v).abs_diff(req);
    if m.vts() > vmin {
        assert!(err(m.vts() - 1) >= err(m.vts()), "{ctx}");
    }
    if m.vts() < limits::VTS_MAX {
        assert!(err(m.vts() + 1) >= err(m.vts()), "{ctx}");
    }
    // 96 MHz only when 80 MHz cannot meet the request.
    assert_eq!(
        m.exceeds_datasheet_clock(),
        clock == SensorClock::Mhz96,
        "{ctx}"
    );
    if clock == SensorClock::Mhz96 {
        assert!(!reachable(SensorClock::Mhz80, m, req, tol), "{ctx}");
    }

    // Registers.
    let writes: Vec<Write> = m.registers().collect();
    assert_eq!(writes.len(), 120, "{ctx}");
    assert_eq!(writes[0], Write::Reg(0x0103, 0x01), "{ctx}");
    let mut regs = HashMap::new();
    for w in &writes {
        if let Write::Reg(a, v) = *w {
            assert!(regs.insert(a, v).is_none(), "{ctx}: {a:#06x} written twice");
        }
    }
    let r16 = |a: u16| u16::from_be_bytes([regs[&a], regs[&(a + 1)]]);
    assert_eq!(r16(0x380C), m.hts(), "{ctx}");
    assert_eq!(r16(0x380E), m.vts(), "{ctx}");
    // 12-bit window fields.
    for a in [0x3800, 0x3802, 0x3804, 0x3806, 0x3808, 0x380A] {
        assert!(regs[&a] <= 0x0F, "{ctx}: {a:#06x}");
    }
    let s = limits::scale(m.readout());
    let (x, y) = m.origin();
    assert_eq!((r16(0x3800), r16(0x3802)), (x, y), "{ctx}");
    let (x_end, y_end) = (r16(0x3804), r16(0x3806));
    assert_eq!(x_end, x + m.width() * s + 15, "{ctx}");
    assert_eq!(y_end, y + m.height() * s + 15, "{ctx}");
    assert!(x_end <= 1295 && y_end <= 815, "{ctx}");
    assert_eq!((r16(0x3808), r16(0x380A)), (m.width(), m.height()), "{ctx}");
    assert!(regs[&0x4601] as u32 * 8 <= m.bytes_per_line(), "{ctx}");
    assert!(regs[&0x4601] >= 1, "{ctx}");
    assert!(regs[&0x3500] <= 0x0F, "{ctx}");
    let exposure =
        (regs[&0x3500] as u32) << 12 | (regs[&0x3501] as u32) << 4 | regs[&0x3502] as u32 >> 4;
    assert_eq!(exposure, m.default_exposure_lines() as u32, "{ctx}");
    let pll = if clock == SensorClock::Mhz80 {
        0x50
    } else {
        0x60
    };
    assert_eq!(regs[&0x030D], pll, "{ctx}");
    assert_eq!(regs[&0x4F00], if m.psv() { 0x0C } else { 0x00 }, "{ctx}");
    assert_eq!(
        regs[&0x3662],
        if m.format() == Format::Raw8 {
            0x07
        } else {
            0x05
        },
        "{ctx}"
    );
}

fn check_refusal(e: Error, format: Format, readout: Readout, w: u16, h: u16, req: u32, tol: u32) {
    let ctx = format!("{e:?}: {format:?} {readout:?} {w}x{h} req {req} tol {tol}");
    let (max_w, max_h) = limits::max_size(readout);
    let size_bad = w < limits::MIN_WIDTH || h == 0 || w > max_w || h > max_h;
    let misaligned = !w.is_multiple_of(limits::width_multiple(format) as u16);
    match e {
        Error::Size {
            max_width,
            max_height,
        } => {
            assert!(size_bad, "{ctx}");
            assert_eq!((max_width, max_height), (max_w, max_h), "{ctx}");
        }
        Error::Alignment { width_multiple } => {
            assert!(!size_bad && misaligned, "{ctx}");
            assert_eq!(width_multiple, limits::width_multiple(format), "{ctx}");
        }
        Error::Window => panic!("a centred window is always inside: {ctx}"),
        Error::TooFast { max } => {
            assert!(!size_bad && !misaligned, "{ctx}");
            assert!(req > max.as_millihertz() + tol, "{ctx}");
            let m = build(format, readout, w, h, max.as_millihertz(), 0).unwrap();
            assert_eq!(m.frame_rate(), max, "{ctx}");
        }
        Error::TooSlow { min } => {
            assert!(!size_bad && !misaligned, "{ctx}");
            assert!(req + tol < min.as_millihertz(), "{ctx}");
            let m = build(format, readout, w, h, min.as_millihertz(), 0).unwrap();
            assert_eq!(m.frame_rate(), min, "{ctx}");
        }
        Error::Tolerance { nearest } => {
            assert!(!size_bad && !misaligned, "{ctx}");
            let off = nearest.as_millihertz().abs_diff(req);
            assert!(off > tol, "{ctx}");
            let m = build(format, readout, w, h, req, off).unwrap();
            assert!(m.rate_error().unsigned_abs() <= off, "{ctx}");
        }
    }
}

#[test]
fn sweep_requests() {
    let (mut built, mut refused) = (0, 0);
    for format in FORMATS {
        for readout in READOUTS {
            for w in WIDTHS {
                for h in HEIGHTS {
                    for req in RATES_MHZ {
                        for tol in TOLERANCES_MHZ {
                            match build(format, readout, w, h, req, tol) {
                                Ok(m) => {
                                    check_mode(&m, req, tol);
                                    built += 1;
                                }
                                Err(e) => {
                                    check_refusal(e, format, readout, w, h, req, tol);
                                    refused += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        built > 1000 && refused > 1000,
        "built {built}, refused {refused}"
    );
}

/// Every reachable rate of a window, requested exactly, comes back exactly.
#[test]
fn sweep_exact_rates() {
    for (format, readout, w, h) in [
        (Format::Raw8, Readout::Crop, 1280, 800),
        (Format::Raw10, Readout::Crop, 640, 480),
        (Format::Raw8, Readout::Binned, 320, 200),
        (Format::Raw10, Readout::Binned, 640, 400),
    ] {
        let rows = limits::rows_read(readout, h);
        let vmin = limits::min_vts(readout, rows, SensorClock::Mhz80, false).unwrap();
        for vts in vmin..vmin + 2000 {
            let req = rate_at(SensorClock::Mhz80, limits::HTS_PREFERRED, vts);
            let m = build(format, readout, w, h, req, 0).unwrap();
            assert_eq!((m.sensor_clock(), m.vts()), (SensorClock::Mhz80, vts));
            check_mode(&m, req, 0);
        }
    }
}
