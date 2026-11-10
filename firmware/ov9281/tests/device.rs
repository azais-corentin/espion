//! The device layer against a mock SCCB sensor.

mod common;

use common::{BusError, MockDelay, MockSensor, block_on};
use ov9281::{
    Address, DeviceError, Exposure, Format, Gain, Mode, Ov9281, Rate, Readout, TestPattern,
    Tolerance, Write,
};

fn mode() -> Mode {
    Mode::builder()
        .format(Format::Raw10)
        .size(640, 400)
        .readout(Readout::Binned)
        .origin(0, 0)
        .frame_rate(Rate::hz(210.0), Tolerance::hz(0.1))
        .build()
        .unwrap()
}

fn register_writes(mode: &Mode) -> Vec<(u16, u8)> {
    mode.registers()
        .filter_map(|w| match w {
            Write::Reg(a, v) => Some((a, v)),
            Write::DelayMs(_) => None,
        })
        .collect()
}

/// The approved example, run against the mock.
#[test]
fn approved_example_writes_the_mode_then_the_controls() {
    let mode = mode();
    let mut cam = Ov9281::new(MockSensor::new(0x60), Address::Primary);
    let mut delay = MockDelay::default();
    block_on(async {
        cam.identify().await?;
        cam.configure(&mode, &mut delay).await?;
        cam.set_exposure(Exposure::lines(400)).await?;
        cam.set_gain(Gain::from_sixteenths(0x20)).await?;
        cam.set_test_pattern(TestPattern::Bars).await?;
        cam.start().await?;
        cam.stop().await
    })
    .unwrap();
    assert_eq!(cam.mode(), Some(&mode));
    let mut expected = register_writes(&mode);
    let delays: Vec<u32> = mode
        .registers()
        .filter_map(|w| match w {
            Write::DelayMs(ms) => Some(ms as u32),
            Write::Reg(..) => None,
        })
        .collect();
    expected.extend([
        (0x3500, 0x00), // 400 lines = 0x1900 sixteenths
        (0x3501, 0x19),
        (0x3502, 0x00),
        (0x3509, 0x20),
        (0x5E00, 0x80),
        (0x0100, 0x01),
        (0x0100, 0x00),
    ]);
    assert_eq!(cam.release().writes, expected);
    assert_eq!(delay.ms, delays);
    assert_eq!(delays, [5]);
}

#[test]
fn identify_checks_the_chip_id() {
    let mut sensor = MockSensor::new(0x10);
    sensor.chip_id = 0x9282;
    let mut cam = Ov9281::new(sensor, Address::Alternate);
    assert_eq!(
        block_on(cam.identify()),
        Err(DeviceError::ChipId { found: 0x9282 })
    );
}

#[test]
fn bus_errors_are_reported() {
    let mut cam = Ov9281::new(MockSensor::new(0x10), Address::Primary);
    assert_eq!(block_on(cam.identify()), Err(DeviceError::Bus(BusError)));
    let mut delay = MockDelay::default();
    assert_eq!(
        block_on(cam.configure(&mode(), &mut delay)),
        Err(DeviceError::Bus(BusError))
    );
    assert_eq!(cam.mode(), None);
}

#[test]
fn exposure_is_checked_against_the_configured_mode() {
    let mode = mode();
    let max = mode.max_exposure_lines();
    let mut cam = Ov9281::new(MockSensor::new(0x60), Address::Primary);
    assert_eq!(
        block_on(cam.set_exposure(Exposure::lines(10))),
        Err(DeviceError::NotConfigured)
    );
    block_on(cam.configure(&mode, &mut MockDelay::default())).unwrap();
    let configured = register_writes(&mode).len();
    for bad in [0, max + 1, u16::MAX] {
        assert_eq!(
            block_on(cam.set_exposure(Exposure::lines(bad))),
            Err(DeviceError::Exposure { max })
        );
    }
    block_on(cam.set_exposure(Exposure::lines(1))).unwrap();
    block_on(cam.set_exposure(Exposure::lines(max))).unwrap();
    let writes = cam.release().writes;
    let [hi, mid, lo] = [(max >> 12) as u8, (max >> 4) as u8, (max << 4) as u8];
    assert_eq!(
        writes[configured..],
        [
            (0x3500, 0x00),
            (0x3501, 0x00),
            (0x3502, 0x10),
            (0x3500, hi),
            (0x3501, mid),
            (0x3502, lo),
        ]
    );
}

#[test]
fn gain_outside_1x_to_15_5x_is_refused() {
    let mut cam = Ov9281::new(MockSensor::new(0x60), Address::Primary);
    for bad in [0x00, 0x0F, 0xF9, 0xFF] {
        assert_eq!(
            block_on(cam.set_gain(Gain::from_sixteenths(bad))),
            Err(DeviceError::Gain)
        );
    }
    block_on(cam.set_gain(Gain::from_sixteenths(0x10))).unwrap();
    block_on(cam.set_gain(Gain::from_sixteenths(0xF8))).unwrap();
    assert_eq!(cam.release().writes, [(0x3509, 0x10), (0x3509, 0xF8)]);
}

#[test]
fn random_pattern_sets_the_same_seed() {
    let mut cam = Ov9281::new(MockSensor::new(0x60), Address::Primary);
    block_on(cam.set_test_pattern(TestPattern::Random)).unwrap();
    block_on(cam.set_test_pattern(TestPattern::Off)).unwrap();
    assert_eq!(
        cam.release().writes,
        [(0x5E01, 0x51), (0x5E00, 0x81), (0x5E00, 0x00)]
    );
}
