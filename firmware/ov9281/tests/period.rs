//! The frame period formula against the periods measured on the bench (2026-11-10, Waveshare
//! ESP32-P4-ETH, first-to-last over 10 s, timed by the ESP32-P4). The sensor's oscillator ran
//! 28 ppm fast against the P4's, and the fit was within 1.4 SCLK cycles.

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

#[test]
fn frame_rate_is_nominal_and_within_the_stated_ppm_of_the_bench() {
    // (format, readout, size, measured fps on the bench) for modes the solver reproduces.
    let cases = [
        (Format::Raw8, Readout::Crop, (1280, 800), 143_901, 143.905),
        (Format::Raw8, Readout::Binned, (640, 400), 209_563, 209.569),
        (Format::Raw8, Readout::Binned, (640, 400), 252_402, 252.409),
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
