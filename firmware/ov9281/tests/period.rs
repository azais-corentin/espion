//! The frame period formula against the periods measured on the bench (2026-11-10, Waveshare
//! ESP32-P4-ETH, timed by the ESP32-P4): the PoC's timing sweeps (first-to-last over 10 s) and
//! the limits characterisation (least-squares fits over 2 s). The sensor's oscillator ran
//! 24..34 ppm fast against the P4's.

use ov9281::{Format, Mode, Rate, Readout, Tolerance, frame_cycles};

/// `(SCLK MHz, HTS, VTS, measured period in ns)` from the timing sweeps
/// (`TMP_TIMING_PLAN.log`, `TMP_HTS_PLAN.log`) and the 320x200 mode (2205.56 us).
const MEASURED: [(u32, u16, u16, f64); 15] = [
    (80, 728, 915, 8_338_793.9),
    (80, 1456, 915, 16_655_966.6),
    (80, 728, 1830, 16_665_066.6),
    (80, 1456, 1830, 33_308_483.3),
    (80, 728, 434, 3_961_816.5),
    (96, 728, 915, 6_948_988.8),
    (80, 728, 763, 6_955_636.9),
    (80, 728, 523, 4_771_696.5),
    (80, 728, 915, 8_338_800.6), // RAW10
    (80, 1092, 915, 12_497_368.7),
    (80, 1820, 915, 20_814_552.0),
    (80, 2184, 915, 24_973_137.5),
    (80, 2912, 915, 33_290_310.0),
    (80, 1456, 915, 16_655_970.0), // 640x400 binned
    (80, 728, 241, 2_205_560.0),
];

/// Sensor clock offset against the P4's timer, from the fit.
const SENSOR_FAST_PPM: f64 = 28.0;

#[test]
fn period_formula_matches_the_measured_periods() {
    for (mhz, hts, vts, ns) in MEASURED {
        // The measured period in cycles of the sensor's own clock.
        let cycles = ns * mhz as f64 / 1000.0 * (1.0 + SENSOR_FAST_PPM * 1e-6);
        let predicted = frame_cycles(hts, vts) as f64;
        let residual = cycles - predicted;
        // 1.4 cycles of fit residual plus the rounding of the logged figures (0.1 ns; 10 ns
        // for the 320x200 period, 0.8 cycles).
        assert!(
            residual.abs() <= 2.5,
            "HTS {hts} VTS {vts} {mhz} MHz: {residual:.2} cycles"
        );
    }
}

/// `(SCLK MHz, HTS, VTS, measured period in SCLK cycles of the P4's timer)` from the limits
/// characterisation (`README.md`, "Frame period"): long lines (crop 1280x400) and low rates
/// (crop 1280x800), over the whole HTS and VTS register range.
const MEASURED_CYCLES: [(u32, u16, u16, f64); 20] = [
    (80, 728, 500, 364_992.41),
    (80, 800, 500, 400_919.48),
    (80, 1000, 500, 500_716.89),
    (80, 1456, 500, 728_254.96),
    (80, 2912, 500, 1_454_780.05),
    (96, 2912, 500, 1_454_779.48),
    (80, 4000, 500, 1_997_677.91),
    (80, 8000, 500, 3_993_625.39),
    (80, 16000, 500, 7_985_520.00),
    (80, 32760, 500, 16_348_540.66),
    (80, 65535, 500, 32_702_832.97),
    (80, 728, 1831, 1_333_935.26),
    (80, 728, 3663, 2_667_595.88),
    (80, 728, 10990, 8_001_489.67),
    (80, 728, 21979, 16_001_269.45),
    (80, 728, 54946, 40_000_595.16),
    (80, 728, 32767, 23_854_717.36),
    (80, 728, 65535, 47_709_173.63),
    (80, 1456, 916, 1_333_933.68),
    (80, 2912, 916, 2_666_137.04),
];

#[test]
fn period_formula_holds_over_the_register_range() {
    for (mhz, hts, vts, cycles) in MEASURED_CYCLES {
        let predicted = frame_cycles(hts, vts) as f64;
        // The same clock offset at every setting: the measured period is 23.5..33.9 ppm short.
        let ppm = (cycles / predicted - 1.0) * 1e6;
        assert!(
            (-34.0..=-23.5).contains(&ppm),
            "HTS {hts} VTS {vts} {mhz} MHz: {ppm:.1} ppm"
        );
    }
}

#[test]
fn frame_rate_is_nominal_and_within_the_stated_ppm_of_the_bench() {
    // (format, readout, size, measured fps on the bench) for modes the solver reproduces.
    let cases = [
        (Format::Raw8, Readout::Crop, (1280, 800), 143_901, 143.905),
        (Format::Raw8, Readout::Binned, (640, 400), 209_563, 209.569),
        (Format::Raw8, Readout::Binned, (640, 400), 250_105, 250.112),
        (Format::Raw8, Readout::Binned, (320, 200), 453_386, 453.399),
        (Format::Raw10, Readout::Crop, (1280, 800), 119_918, 119.921),
    ];
    for (format, readout, (w, h), req, measured) in cases {
        let mode = Mode::builder()
            .format(format)
            .size(w, h)
            .readout(readout)
            .frame_rate(Rate::millihertz(req), Tolerance::millihertz(1))
            .build()
            .unwrap();
        let nominal = mode.frame_rate().as_millihertz() as f64 / 1000.0;
        let ppm = (measured / nominal - 1.0) * 1e6;
        // 28 ppm fast, the logged rates rounded to 1 mHz (up to 8 ppm at 120 Hz).
        assert!(
            (ppm - SENSOR_FAST_PPM).abs() <= 10.0,
            "{w}x{h}: {ppm:.1} ppm"
        );
        let mhz = mode.sensor_clock().mhz() as f64;
        let period = frame_cycles(mode.hts(), mode.vts()) as f64 * 1000.0 / mhz;
        assert!((mode.frame_ns() as f64 - period).abs() <= 0.5);
    }
}
