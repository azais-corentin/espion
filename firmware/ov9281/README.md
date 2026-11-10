# ov9281

Driver for the OmniVision OV9281 global-shutter sensor (1280x800, monochrome) on two MIPI CSI-2
lanes. It turns a request (format, output size, crop or 2x2 binning, window origin, frame rate
with a tolerance) into a sensor mode, or refuses it with the reason, then writes the mode and
the exposure, gain and test-pattern controls over any `embedded-hal-async` I2C bus.

- `no_std`, no heap, no esp dependency: it builds and tests on the host.
- The builder is a typestate: `build` exists only once format, size and frame rate are set,
  and every builder method is a `const fn`, so a mode can be a `const`.
- The limits it applies (`src/limits.rs`) are the measured limits of one sensor, described
  below.

Fixed set-up: 2 data lanes at 800 Mbit/s, 24 MHz XVCLK, continuous clock lane, free-running
(FSIN trigger and STROBE off).

## Usage

```rust
use ov9281::{Address, Exposure, Format, Gain, Mode, Ov9281, Rate, Readout, TestPattern, Tolerance};

async fn run<I>(
    i2c: I,
    mut delay: impl embedded_hal_async::delay::DelayNs,
) -> Result<(), Box<dyn core::error::Error>>
where
    I: embedded_hal_async::i2c::I2c,
    I::Error: 'static,
{
    let mode = Mode::builder()
        .format(Format::Raw10)                           // Raw8 | Raw10
        .size(640, 400)                                  // output pixels
        .readout(Readout::Binned)                        // optional, default Readout::Crop
        .origin(0, 0)                                    // optional, default centred (sensor pixels)
        .frame_rate(Rate::hz(210.0), Tolerance::hz(0.1)) // also Rate::millihertz(209_563)
        .build()?;
    assert_eq!(mode.frame_rate(), Rate::millihertz(209_964)); // VTS 522 at 80 MHz
    assert!(!mode.exceeds_datasheet_clock());

    let mut cam = Ov9281::new(i2c, Address::Primary);
    cam.identify().await?;                               // chip ID
    cam.configure(&mode, &mut delay).await?;             // writes the registers, stays in standby
    cam.set_exposure(Exposure::lines(400)).await?;       // error above mode.max_exposure_lines()
    cam.set_gain(Gain::from_sixteenths(0x20)).await?;    // 2x
    cam.set_test_pattern(TestPattern::Bars).await?;
    // ...start the CSI receiver (data type mode.format().data_type()), then:
    cam.start().await?;
    cam.stop().await?;
    Ok(())
}
```

The same example is the crate-level doc test; this one is compiled as a doc test too.

How the solver chooses (`ModeBuilder::build`):

1. Checks the size, then the width and height multiples, then the window.
2. Clock: 80 MHz, unless the requested rate is above the fastest 80 MHz rate of the window and
   format; then 96 MHz (above the datasheet's 80 MHz; `Mode::exceeds_datasheet_clock`).
3. HTS: 728 (longer only for wide RAW10 lines at 96 MHz) when a VTS there is within the
   tolerance, otherwise the smallest longer HTS up to 2912 that has one.
4. VTS: the one nearest the request at that HTS. PSV on where the PSV rule allows it.
5. Nothing within the tolerance: `TooFast` above the 96 MHz maximum, `TooSlow` below
   0.419 Hz, otherwise `Tolerance` with the nearest rate of the selected clock.

Worst case: 2185 HTS steps (a request no HTS meets), each one 32-bit division and a few
comparisons.

Checks: `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test`, `cargo doc`, and for
the ESP32-P4 `cargo build --release --target riscv32imafc-unknown-none-elf`.

## Measured limits

Measured on 2026-11-10 on one sensor at room temperature: a Waveshare ESP32-P4-ETH board with
an OV9281 module, 2 lanes at 800 Mbit/s, XVCLK 24 MHz, 16 boots of a measurement harness on the
board (`poc-p4-eth`, removed afterwards). h, w are the output height and width; "crop" is the
unbinned window, "binned" the 2x2 binned readout. HTS 728 and PSV off unless stated.
`[INFERENCE]` marks what was not measured.

### Method

- Each probe: software reset, the full register list with the values under test, receiver
  start, stream on, 4 frames to settle, then a measurement over max(250 ms, 12 frame periods):
  frame count, size, drops, overwrites, sequence gaps and a least-squares fit of arrival time
  against sequence number (the period in SCLK cycles). Statistics on 4 held frames.
- **The scene picture decides.** A wrong VTS, PSV, VFIFO or HTS setting delivers whole, on-time
  frames without any receiver error, and the bar test pattern passes where the scene is broken.
  So every limit was judged on a lit scene against a reference of the same window and exposure
  at a long VTS, taken just before each search (the lamp drifted up to 1.5x within minutes).
- **Pass**: frames of the right size, no DMA, host or bridge error, no flat row in any of the
  4 frames, block-mean correlation with the reference >= 0.95, and brightness
  `(mean - 4) / (ref - 4)` within 0.8..1.25 (or means within 1.5). Searches bisect VTS, HTS or
  0x4601 to one unit; sweeps probe every value. Gain 8x (15.5x for windows of 2..24 lines at
  80 MHz, 8x or less at 96 MHz).

### Summary

| Limit | Measured boundary | Crate rule (`src/limits.rs`) |
|---|---|---|
| Min VTS, PSV off | crop `h + 51`, binned `h + 31`; same at 80/96 MHz, RAW8/RAW10 | `+ 4` margin: crop `h + 55`, binned `h + 35` |
| PSV on (0x4F00 = 0x0C) | from crop `h + 91`, binned `h + 71` (edge h + 90 or h + 91) | on from crop `h + 95`, binned `h + 75`; off below |
| Max VTS | 65535 (register width) | 65535 |
| Min HTS | 723 at 1296 array columns, 671..677 narrower; link: `ceil(line_bytes x MHz / 200) + 75` | `max(728, link + 8)`: 728, RAW10 at 96 MHz from w 1088 (1280: 851) |
| Max HTS | formula to 65535, picture to about 8000 | 2912 |
| Frame period | `(VTS - 1) x HTS + 1730` SCLK cycles | the same |
| Width | RAW8 multiple of 8, 120..1280 (binned ..640); RAW10 multiple of 64 (receiver) | RAW8 x8, RAW10 x64; 128..=1280 crop, 128..=640 binned |
| Height | 1..800 (binned 1..400), RAW10 even (receiver); minimum VTS measured from 2 | 2..=800 crop, 2..=400 binned; RAW10 even |
| Origin x | the sensor truncates it to a multiple of 8 | multiple of 8, else `Window` |
| Origin y | crop exact; binned floored to even | crop any; binned even, else `Window` |
| Window | must lie inside 1280x800 (binned 2w x 2h); outside it the sensor still streams | `Window` |
| VFIFO 0x4601 | good `low..=w/8`, `low` = 1, or `ceil(w/40) - 4` in RAW8 at 80 MHz | `max(1, w/16)` |
| Exposure | VTS - 13 still right; above VTS black frames | at most VTS - 25 lines |

### Minimum VTS, PSV off

Below the rule the frames stay whole and on time with no error, but the picture is dark and
inverted (mean below the black level 4, correlation about -0.4, some all-zero rows). Further
down (crop h + 31..33, binned h + 18..22) no frames arrive at all (CSI host frame errors).
Just above the frame limit some HTS values deliver every 2nd or 4th frame. The dark failure
needs light: at 1 line of exposure the same frames read a normal black level.

| Window (centred) | Last good | First bad | Rule | Last VTS with frames |
|---|---|---|---|---|
| crop 1280x800 | 851 | 850 (mean 0.07, 35 zero rows) | h + 51 | 832 |
| crop 1280x720 (80, 96 MHz, RAW10) | 771 | 770 | h + 51 | 752 |
| crop 1280x480 | 531 | 530 | h + 51 | 511 |
| crop 1280x240 (top, centre, bottom) | 291 | 290 | h + 51 | 271 |
| crop 1280x64 | 115 | 114 | h + 51 | 95 |
| crop 1280x2 (gain 15.5x) | 53 | 52 | h + 51 | 32 |
| crop 640x480, 320x480 | 531 | 530 | h + 51 | 512 |
| binned 640x400 | 431 | 430 | h + 31 | 422 |
| binned 640x200 (top, centre, bottom) | 231 | 230 | h + 31 | 222 |
| binned 320x200, 160x200 | 231 | 230 | h + 31 | 222 |
| binned 640x50 | 81 | 80 | h + 31 | 72 |
| binned 640x2 (gain 15.5x) | 33 | 32 | h + 31 | 22 |

The same boundaries at 96 MHz (1280x800, 1280x720, 640x480, 1280x240, 1280x64, binned 640x400,
320x200, 640x100) and in RAW10 at both clocks; at exposures of 100, 300 and 466 lines (crop) and
100 and 178 lines (binned). The rule binds at HTS 728..1000 (crop) and 728..850 (binned); at
longer lines only the frame limit was found (crop 1280x480 at HTS 1100..2912: 511..512; binned
320x200 at HTS 900..2912: 221..222), so the rule is safe over 728..2912. Height 1 reached frames
from VTS 31 (crop) and 21 (binned) but its picture limit was not measured.

### PSV (0x4F00 = 0x0C)

Sweeping VTS up from the PSV-off minimum with PSV on (step 2, two boots, 80 and 96 MHz, HTS 728
and 1456), the result is **not monotonic**:

| VTS - h, crop | VTS - h, binned | PSV on gives |
|---|---|---|
| 51..54 | 31..34 | right picture |
| 55..57 | 35..37 | **no frames** |
| 58 | 38 | right picture |
| 59..89 or 90 | 39..70 | rows flat at the black level (crop: h - 4 rows at 59, then all; binned: h - 14 rows at 39, all from 53) |
| from 90 or 91 | from 71 | right picture |

The narrow good windows sit next to the no-frame lines, so the crate never turns PSV on below
`h + 95` / `h + 75`. The bands are in lines of VTS - h, the same at HTS 728, 1000, 1456 and 2912,
both clocks, both formats, at the top and centre, and at every exposure tried.

| Window | Last good | First bad | Rule |
|---|---|---|---|
| crop 1280x800 | 891 | 890 (some frames entirely flat) | h + 91 |
| crop 1280x720 | 810 or 811 (4 boots) | 809 (all rows flat) | h + 91 |
| crop 1280x480 (also HTS 1000, 1456, 2912, 96 MHz) | 571 | 570 | h + 91 |
| crop 1280x64 | 155 | 154 | h + 91 |
| binned 640x400 | 471 | 470 | h + 71 |
| binned 320x200 (also 96 MHz, RAW10, HTS 900, 1456) | 271 | 270 | h + 71 |
| binned 640x100 | 171 | 170 | h + 71 |

Where PSV is safe it changes nothing measurable: same picture (correlation 1.0000 / 0.9999),
black level, row and column noise, and frame period. Power with PSV off was not measured.

### HTS

Shortest line, scene against the same window at HTS 728 (VTS rows + 1200):

| Window (array columns) | Format, clock | Last good | First bad: how |
|---|---|---|---|
| crop 1280 (1296), binned 640 (1296) | RAW8 80/96, RAW10 80 | 723 | 722: flat rows, rest 10x too bright |
| crop 960 (976), 640 (656), 320 (336) | RAW8 80 | 671 | 670: 2.7x too bright |
| crop 64 (80) | RAW8 80 | 677 | 676: near black |
| crop 1280, 1600 B | RAW10 96 | 840 (pattern 843) | broken picture; no frames at 787 and below |
| crop 1216, 1520 B | RAW10 96 | 803 | broken picture |
| crop 1152, 1440 B | RAW10 96 | 765 | broken picture |
| crop 1024, 1280 B | RAW10 96 | 688 | broken picture |

The link minimum is the data time on 200 MB/s plus 73..75 cycles (0.77 us per line;
`[INFERENCE]` the D-PHY's per-line HS entry and exit): `ceil(line_bytes x MHz / 200) + 75`.
At HTS 671 narrow windows already read 11..16 % brighter than at 728, so the crate keeps 728
as the floor and adds 8 cycles to the link term (RAW10 1280 at 96 MHz: 851). Long lines
brighten the picture at the same exposure (1.04x at HTS 2912, 1.09x at 8000, failing from about
16000) `[INFERENCE: charge collected outside the exposure]`; the crate stops at 2912.

### Frame period

A frame lasts `(VTS - 1) x HTS + 1730` SCLK cycles. 1493 probes over HTS 659..65535 and VTS
21..65535, both clocks, both formats, crop and binned: every measured period is 23.5..33.9 ppm
below the formula, the sensor's clock offset against the P4's timer. Selected points (P4-timer
cycles):

| HTS | VTS | Measured | Formula | ppm |
|---|---|---|---|---|
| 728 | 500 | 364 992.41 | 365 002 | -26 |
| 1456 | 500 | 728 254.96 | 728 274 | -26 |
| 2912 | 500 | 1 454 780.05 (96 MHz: .48) | 1 454 818 | -26 |
| 16000 | 500 | 7 985 520.00 | 7 985 730 | -26 |
| 65535 | 500 | 32 702 832.97 | 32 703 695 | -26 |
| 728 | 3663 | 2 667 595.88 | 2 667 666 | -26 |
| 728 | 21979 | 16 001 269.45 | 16 001 714 | -28 |
| 728 | 65535 | 47 709 173.63 | 47 710 482 | -27 |
| 2912 | 916 | 2 666 137.04 | 2 666 210 | -27 |

`Mode::frame_rate` is the formula at an exact 24 MHz XVCLK; this sensor ran 24..34 ppm faster.

### Sizes, origins, VFIFO

The ESP32-P4 receiver needs RAW10 widths that are multiples of 64 and even RAW10 heights
(1248x200 and 640x201 are refused with `InvalidGeometry`), so no other RAW10 size could be
verified. RAW8 widths (crop, h 200):

| Width | Result |
|---|---|
| 1280, 1272, 1264, 1248, 1216, 1000, 640, 320, 256, 192, 160, 136, 128, 120 (0 mod 8) | right picture at the right place |
| 112, 100, 96, 80 | whole frames, wrong dark picture (the bar pattern still exact) |
| 48 and narrower | no frames |
| 1276, 1012, 652 (4 mod 8) | frames |
| 1268, 1260, 1020, 1004, 996, 644, 636, 324 (4 mod 8) | no frames |

Binned: 640..120 right (636 and 316 too), 100..64 wrong dark picture, 48 and narrower no frames.
Odd RAW8 heights (1280x799, 640x3, 640x1, binned 640x101) are right and correctly placed. A
window past the array still streams (crop 640x480 at y 400: 72 flat rows), so the crate refuses
it.

Origin, each still located in the full frame of the same boot (correlation 0.95..0.99):

| Window | Asked | Found (sensor pixels) |
|---|---|---|
| crop 640x480 | (1,1), (3,5), (321,161), (100,37) | (0,1), (0,5), (320,161), (96,37) |
| crop 640x200 | x 1, 2, 4, 6, 8, 12, 16, 20, 24 | x 0, 0, 0, 0, 8, 8, 16, 16, 24 |
| crop 1216x704 RAW10 | (32,48) | (32,48) |
| binned 320x200 | (1,1), (2,2), (3,3), (100,37) | (0,0), (0,2), (0,2), (96,36) |

VFIFO read start 0x4601 (bar pattern, rows must repeat exactly); it counts 8 **pixels**:

| Window | Format, clock | Lowest good (first bad) | Highest good |
|---|---|---|---|
| crop 1280 | RAW8 80 | 28 (27) | 160 (161: no frames) |
| crop 1280 | RAW8 96, RAW10 80 | 1 (0) | 160 |
| crop 640, binned 640 | RAW8 80 | 12 (11) | 80 |
| crop 640 | RAW8 96, RAW10 80/96 | 1 (0) | 80 |
| crop 320, binned 320 | RAW8 80 | 4 (3) | 40 |
| crop 128 | RAW8 80 | 1 (0) | 16 |

Too low shifts or smears lines; too high gives no frames. No fixed value serves every width
(0x30 gives no frames below 384 pixels, 0x04 breaks RAW8 at 80 MHz above about 320); `w / 16`
lies inside every range and ran in every probe from the third boot on (widths 64..1280, both
clocks and formats, HTS 659..65535, up to 1133 fps).

### Exposure

At VTS 3663: exposures of VTS - 26 and VTS - 13 lines give a right, brighter picture at an
unchanged period; 4000 lines (beyond VTS) give black frames at the same period. The frame is
never stretched. The crate allows at most VTS - 25.

### Fastest and slowest rates the crate gives

Computed by the crate from the formula at the nominal clock, with the margin; HTS 728 except
RAW10 1280 at 96 MHz (HTS 851). `[INFERENCE]` these exact settings were not run: each VTS is
the measured rule plus 4 lines (the "Minimum VTS" and "PSV" tables).

| Window | 80 MHz (VTS) | 96 MHz | with PSV on: 80 / 96 MHz |
|---|---|---|---|
| crop 1280x800 RAW8 | 128.320 Hz (855) | 153.984 Hz | 122.594 / 147.112 Hz (895) |
| crop 1280x800 RAW10 | 128.320 Hz (855) | 131.781 Hz | 122.594 / 125.898 Hz |
| crop 1280x720 RAW8 | 141.542 Hz (775) | 169.851 Hz | 134.607 / 161.529 Hz (815) |
| crop 1280x720 RAW10 | 141.542 Hz (775) | 145.366 Hz | 134.607 / 138.240 Hz |
| crop 640x480 | 204.875 Hz (535) | 245.850 Hz | 190.657 / 228.788 Hz (575) |
| binned 640x400 | 251.824 Hz (435) | 302.189 Hz | 230.679 / 276.815 Hz (475) |
| binned 320x200 | 464.895 Hz (235) | 557.874 Hz | 397.610 / 477.132 Hz (275) |

The 96 MHz column is `TooFast { max }`. The slowest rate of every window is 0.419 Hz (80 MHz,
HTS 2912, VTS 65535; `TooSlow { min }`); at HTS 728, VTS 65535 gives 1.677 Hz (measured).

### Requests verified on the board

Each ran with a scene still that passed and every frame delivered. The tolerance is the
report's where given (`+-`), else the report setting's own distance from the request. Report
setting and crate choice as clock MHz / HTS / VTS / PSV:

| # | Request | Report setting | Measured (formula) | Crate |
|---|---|---|---|---|
| rq01 | RAW8 1280x800, 120 Hz +- 0.1 | 80/728/915/on | 119.921 (119.918) | 80/728/914/on: nearer, 120.049 |
| rq02 | RAW8 1280x800, 128 Hz | 80/728/857/off | 128.024 (128.021) | same |
| rq03 | RAW8 1280x800, 150 Hz | 96/728/878/off | 149.961 (149.956) | same |
| rq04 | RAW8 1280x720, 140 Hz | 80/728/784/off | 139.924 (139.920) | same |
| rq05 | RAW10 1280x720, 120 Hz | 80/728/915/on | 119.921 (119.918) | 80/728/914/on: nearer |
| rq06 | RAW8 640x480 at (0,0), 200 Hz | 80/728/548/off | 200.033 (200.027) | same |
| rq07 | RAW8 640x400 binned, 250 Hz | 80/728/438/off | 250.112 (250.105) | same |
| rq08 | RAW10 640x400 binned, 210 Hz | 80/728/523/on | 209.569 (209.563) | 80/728/522/on: nearer, 209.964 |
| rq09 | RAW8 320x200 binned, 470 Hz | 80/728/232/off | 470.884 (470.871) | 96/728/279/on, 470.325: 232 < h + 35 |
| rq10 | RAW8 320x200 binned at (96,36), 400 Hz | 80/728/273/on | 400.520 (400.509) | PSV off (273 < h + 75) |
| rq11 | RAW8 1280x64, 1000 Hz +- 5 | 96/728/131/off | 996.188 (996.161) | 96/728/130/off: nearer, 1003.743 |
| rq12 | RAW10 1280x800, 125 Hz | 80/728/878/off | 124.967 (124.964) | same |
| rq13 | RAW8 1280x800, 30 Hz | 80/728/3663/on | 29.990 (29.989) | 80/728/3662/on: nearer, 29.997 |
| rq14b | RAW10 640x480, 240 Hz | 96/728/548/off | 240.039 (240.032) | same |
| rq15 | RAW10 320x200 binned, 400 Hz | 80/728/273/on | 400.520 (400.509) | PSV off (273 < h + 75) |
| rq16b | RAW8 640x240 at (200,160), 300 Hz | 80/728/365/on | 299.946 (299.938) | same |
| rq17 | RAW10 320x200 binned at (400,160), 560 Hz | 96/728/234/off | 560.259 (560.244) | `TooFast { 557.874 }`: 234 < h + 35; builds from +- 2.126 Hz |
| rq18 | RAW8 1280x400 at (0,200), 15 Hz | 80/728/7325/on | 15.000 (14.999) | same |
| rq19 | RAW10 1216x704 at (32,48), 130 Hz +- 0.2 | 80/728/845/on | 129.839 (129.836) | 80/728/844/on: nearer, 129.990 |
| rq20 | RAW8 640x400 binned, 300 Hz +- 2 | 96/728/440/off | 298.774 (298.766) | 96/728/438/off: nearer, 300.126 |
| rq21 | RAW8 1280x800, 120 Hz +- 0.001 | 80/738/902/on | 120.003 (120.000) | same |
| rq16 | RAW8 320x240 at (32,501), 364.6 Hz | 80/728/300/off | 364.637 (364.627) | same |

`tests/requests.rs` holds these and the requests that must be refused, with the bounds
recomputed for the margin.

### Not measured, kept conservative

- Height 1: its minimum VTS for a picture; the crate starts at 2.
- Widths 113..119 (crop) and 104..112 (binned); the crate starts at 128. Why some widths of
  4 mod 8 stream is unknown; the crate refuses them.
- HTS below 728 (the sensor's own minimum is 671..723) and above 2912.
- PSV bands were swept at HTS 728 and 1456 only (the upper edge also at 1000, 2912, 96 MHz).
- The dark-VTS limit at HTS 1000..1100 (crop) and 850..900 (binned) was not bisected; the
  crate uses the 728 rule at every HTS, which is never too low.
- Exposures between VTS - 13 and VTS; the crate stops at VTS - 25.
- Content of windows reaching outside the array; the crate refuses them.
- 96 MHz at gain above 8x (column stripes were seen at 15.5x in an earlier run).
- The fastest rates at 96 MHz with RAW10 1280 (HTS 851) and the slowest rates at HTS 2912 are
  computed from the formula, not run at that setting.
- The causes of the dark-picture limit, the PSV bands and the long-line brightening.
- One sensor, one board, room temperature; no temperature or part-to-part spread.
