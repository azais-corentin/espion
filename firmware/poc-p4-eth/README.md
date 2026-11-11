# poc-p4-eth

A throwaway proof of concept on a Waveshare ESP32-P4-ETH dev board. It is not part of the
product (that is `firmware/cam-p4` on the espion-cam board). It answers three questions before
the camera PCBs are ordered:

1. Does the OV9281 module deliver images to the ESP32-P4 over MIPI CSI-2 (2 lanes, RAW8)?
2. At which resolutions can the P4 process every pixel for marker tracking (threshold,
   connected components, weighted centroids) at 120 fps, all on the board?
3. Do the other modes of the Inno-Maker CAM-MIPIOV9281 V2 mode list work: RAW8 1280x800 and
   1280x720 at 144 fps, 640x400 binned at 210 and 253 fps, 320x200 at 453 fps, and RAW10
   1280x800 and 1280x720 at 120 fps and 640x400 at 210 fps?

The modes for question 2 are 1280x800, 1280x720 (crop), 640x480 (crop) and 640x400 (2x2
binned), all at 120 fps (`modes::MODES` 0..=3); the Inno-Maker list is `MODES` 4..=11. Each
mode is a request to the mode builder of the `firmware/ov9281` crate (format, size, readout,
rate and tolerance), solved at compile time; the crate picks clock, line length (HTS), frame
length (VTS) and the power-saving mode, and writes the sensor. The sensor free-runs (no XVS
trigger). The PoC is minimal by intent: no Ethernet, no PTP, no LEDs, no protocol.

Status, measured on the board (details below):

- Question 1: yes. The camera delivers whole frames over CSI-2 in all four modes at
  120.05 fps (119.92 with the hand-built tables), with no dropped or short frame and no
  error. It also delivers 64-line strips into internal RAM, byte-identical to whole frames.
- Question 2: all four modes sustain 120 fps with every pixel processed when the CSI DMA
  writes 64-line strips into internal RAM: every run of the strip soak passed, with at most
  5.2 ms of processing per frame at 1280x800 and the centroids ready 0.1–0.4 ms after the
  end of the frame (synthetic and dark live frames; up to 3.5 ms in a bright live scene).
  With whole frames read from PSRAM only 640x480 and 640x400 do; the 1280-wide modes fall to
  85–106 fps processed.
- Question 3: yes, all eight, with capture-only verification (no soak at the new rates): 5 s
  of capture per mode with no dropped, short or overwritten frame and no error, at the rate
  the frame-period formula predicts (+27 to +47 ppm): 144.065, 209.969, 252.930, 453.398 and
  120.052 fps; a correct bar-pattern still and a correct scene still in every mode; the RAW10
  byte layout confirmed as CSI-2 packed. With the hand-built tables two modes needed changes:
  1280x720 at 144 fps runs the 96 MHz clock (as 1280x800 at 144 does), and 320x200 resets two
  common registers; with the crate the 253 fps mode runs at 96 MHz too. The three RAW10 modes
  also passed through the strip path (strips byte-identical to whole frames, strip soak
  passed; 1280-wide RAW10 costs about 1.3x RAW8 and fits 120 fps). See "All 12 modes".
- Strip stalls: the DMA stall that crashed the board in RAW10 is root-caused and fixed in
  `esp32p4-csi` (`9d21695`). The CSI bridge, which ends the DMA blocks, overran the ring after
  a plain resume; the driver now restarts the ISP and the bridge before it resumes. The stall
  test (`3325d0c`) passes 24/24: halts of 1 to 100 ms and bursts, in RAW8 and RAW10, every
  strip's rows correct. See "The stall fault (fixed)".
- Sensor limits: measured on this board for the `firmware/ov9281` driver crate. They bound
  the dark picture at short VTS (a minimum VTS per window height; cause unknown) and the PSV
  flat rows (bands of VTS), and show that the 253 fps mode (VTS 434) worked only in a narrow
  window. See "Sensor driver crate".
- Sensor driver: the PoC's 12 modes are requests to the crate's mode builder since `e18819d`
  (the hand-built register tables are gone). All 12 run clean at the solver's settings, and
  on the P4 the builder accepted all 40 requests of the request verification with the
  setting the crate's tests expect, each a right picture at the predicted rate, and refused
  all 14 it must, with the expected error; `build()` takes 2.5–5.7 us for a mode at the
  standard line length, at most 379 us. See "Request verification on the P4".

## Hardware set-up

| Part | What |
|---|---|
| Board | [Waveshare ESP32-P4-ETH](https://docs.waveshare.com/ESP32-P4-ETH): ESP32-P4NRW32X (32 MB PSRAM in package), 32 MB flash, CH343P USB-UART bridge on the USB-C port |
| Camera | SZYTF WX-1M-9281-79 OV9281 module, Raspberry-Pi-style 15-pin FFC (pin table in [docs/hardware/camera.md](../../docs/hardware/camera.md)) |
| Cable | 22-pin to 15-pin FFC into the board's 22-pin MIPI-CSI connector J1 [INFERENCE: Pi-style mapping, J1 pin 6 to module pin 11] |
| Host link | USB-C to the CH343, a `ttyACM` device on Linux (UART0: 115200 baud for the ROM and bootloader, 2 Mbaud once the firmware runs) |

Pin map used, from the Waveshare schematic
([ESP32-P4-ETH-datasheet.pdf](https://files.waveshare.com/wiki/ESP32-P4-ETH/ESP32-P4-ETH-datasheet.pdf),
sheet 1 of 3), next to the espion-cam board ([docs/hardware/camera.md](../../docs/hardware/camera.md)):

| Signal | Dev board (this PoC) | espion-cam |
|---|---|---|
| Log UART | GPIO37 TX, GPIO38 RX, to the CH343 | USB Serial/JTAG |
| Auto-reset | CH343 RTS/DTR through U5 to EN and GPIO35 (BOOT) | — |
| SCCB SDA | GPIO7 (net ESP_I2C_SDA, J1 pin 2, 2.2 k pull-up R46) | GPIO8 |
| SCCB SCL | GPIO8 (net ESP_I2C_SCL, J1 pin 3, 2.2 k pull-up R44) | GPIO9 |
| Camera enable (module pin 11, PWDN) | no GPIO: CSI_IO0 (J1 pin 6) held at 3.3 V by R42 (10 k) | GPIO10 (CAM_EN), R43 pull-down |
| Module pin 12 | CSI_IO1 (J1 pin 5), floating (R43 not fitted), no GPIO | CAM_IO1 |
| XVS / STROBE | not connected (free-running) | GPIO7 / GPIO12 |
| MIPI D-PHY supply (VDD_MIPI_DPHY, U6 pin 41) | ESP_LDO_VO3, the P4's internal LDO channel 3 | ESP_LDO_VO3 on the ESP32-P4-Core module ([docs/architecture.md](../../docs/architecture.md)) |

The dev board's I2C bus is shared with the ES8311 audio codec and the DSI connector. GPIO7 is
SCCB data here but XVS on the espion-cam board.

## Build, flash, capture the log

From this directory, inside the dev shell (or prefix with `nix develop -c` from the repo root).
`PORT` is the CH343's serial device. It shows up as a `ttyACM` device (`/dev/ttyACM0` on the
computer of the first runs, but the number depends on what else is plugged in);
`ls /dev/serial/by-id/` lists it by name as `usb-1a86_USB_Single_Serial_<serial>-if00`, which
does not change.

```sh
PORT=/dev/serial/by-id/usb-1a86_USB_Single_Serial_<serial>-if00
cargo build --release
E=target/riscv32imafc-unknown-none-elf/release/poc-p4-eth
timeout 120 espflash flash   --chip esp32p4 --port "$PORT" --non-interactive "$E"
timeout 330 espflash monitor --chip esp32p4 --port "$PORT" --non-interactive --no-addresses \
  --monitor-baud 2000000 --elf "$E" > /tmp/poc-p4-eth/soak.log 2>&1   # exits 124 at the timeout: expected
sed 's/\x1b\[[0-9;]*m//g' /tmp/poc-p4-eth/soak.log | tr -d '\r' | grep -aE 'camera|soak|^INFO - \|'
python3 tools/still.py /tmp/poc-p4-eth/soak.log   # only with still_dump: true
```

- `monitor` resets the chip through the CH343's DTR/RTS on connect, so the log starts at the
  ROM banner. No BOOT or RESET press is needed.
- The firmware switches UART0 to 2 Mbaud right after `esp_hal::init` (`LOG_BAUD` in
  `main.rs`). The ROM and bootloader lines before that are at 115200 and look garbled in a
  2 Mbaud monitor.
- How long to let `monitor` run: the boot report is out within about 1 s; the default plan
  (frame soak, four modes) needs about 330 s; the RAW8 strip soak finishes in under 300 s;
  capture only takes about 70 s for four modes with the cache check (about 100 s with the
  still dumps too); `MODE_STILLS_PLAN` about 180 s, `REQUESTS_PLAN` about 25 min,
  `STRIP_CHECK_PLAN` about 45 s, `RAW10_STRIP_SOAK_PLAN` about 190 s, `STRIP_STALL_PLAN` about
  90 s, `JPEG_PLAN` about 8 min; the detector benchmark (`RUN_BENCH = true`) about 15 s.
  Every camera plan ends with the line `camera: done`.
- `--no-addresses` stops espflash from printing symbol lines for hex numbers in the log.
- `cargo run --release` uses the runner in `.cargo/config.toml`,
  `espflash flash --monitor ... --port /dev/ttyACM0`: interactive, and with the first
  computer's port.
- `tools/still.py <log> [out dir]` (Python standard library only) checks each still's CRC and
  line count and writes `<out dir>/<name>.pgm` and `.png` (default `/tmp/poc-p4-eth`). RAW10
  stills become a 16-bit PGM and an 8-bit PNG. It also writes the JPEG files of `JPEG_PLAN`
  (`@@JPG` lines) as `<name>.jpg`.
- `tools/jpeg_check.py <dir>` checks every `<name>-q<q>.jpg` there against its source
  `<name>.pgm`: markers (SOF0, 8-bit, one component, size, EOI), decoding with `djpeg`, PSNR
  against the source, and `cjpeg -grayscale -baseline -quality <q>` of the same source as the
  reference (size and PSNR); it writes `<name>-q<q>.decoded.png`. Run it with libjpeg-turbo:
  `nix shell nixpkgs#libjpeg_turbo -c python3 tools/jpeg_check.py /tmp/poc-p4-eth/jpeg`.

### Choosing what runs

All in constants at the top of `src/main.rs`; one plan per boot. The default selection is
`RUN_BENCH = false`, `PLAN = &CAMERA_PLAN`, `SOAK = Soak::Frames`.

| Constant | Value | Runs |
|---|---|---|
| `RUN_BENCH` | `false` (default) / `true` | Detector benchmark on synthetic frames, before the camera |
| `RUN_CAMERA` | `true` (default) | The camera section |
| `PLAN` | `&CAMERA_PLAN` (default) | Modes 0..=3 (the 120 fps RAW8 modes), then the soak chosen by `SOAK` |
| `PLAN` | `&FAST_CAPTURE_PLAN` | Capture only over modes 4..=11, 5 s each, then the detector check per mode |
| `PLAN` | `&MODE_STILLS_PLAN` | All 12 modes (order 0, 9, 1, 10, 6, 11, 4, 5, 7, 8, 2, 3, so RAW10 follows its RAW8 window and 8 follows the binned modes), gain 2x: capture only, the detector check, a scene still and a bar-pattern still per mode |
| `PLAN` | `&RAW10_PATTERNS_PLAN` | Modes 6 and 11 (640x400 RAW8 and RAW10 at 210 fps): scene still, then stills of the random-data, grey-squares and rolling-bar test patterns |
| `PLAN` | `&REQUESTS_PLAN` | Request verification (`requests::REQUESTS`): every request built at run time on the P4 and timed, the refused ones with their error; each accepted one captured after a long-VTS reference of the same window, with a scene and a bar-pattern still; gain 8x. See "Request verification" |
| `PLAN` | `&STRIP_CHECK_PLAN` | Strip check on modes 0, 9, 10, 11: whole frames of the random test pattern as the reference, then 10 s of strips per mode checked row by row against it |
| `PLAN` | `&RAW10_STRIP_SOAK_PLAN` | Strip soak on the RAW10 modes 9, 10, 11 |
| `PLAN` | `&STRIP_STALL_PLAN` | Stall test on the modes of `STRIP_CHECK_PLAN`: provoked DMA halts (1, 5, 30 and 100 ms, bursts of 20 x 2 ms and 8 x 30 ms), every strip checked against the reference, a memory guard around the list items and after the ring |
| `PLAN` | `&JPEG_PLAN` | Hardware JPEG encoder (`esp32p4-jpeg`, `src/jpeg_bench.rs`): synthetic and camera pictures at 3 sizes x 3 qualities dumped for `tools/jpeg_check.py`, speed alone, capture-and-encode runs up to the sensor's fastest rates, robustness (bad settings, output too small, dropped encode, 60 s soak). See "JPEG encoder" |
| `SOAK` | `Soak::Frames` (default) | With `CAMERA_PLAN`: frame soak, whole frames in PSRAM (`SOAK_PLAN`), RAW8 modes only |
| `SOAK` | `Soak::Strips` | With `CAMERA_PLAN`: strip soak, strips in internal RAM (`STRIP_SOAK_PLAN`) |
| `SOAK` | `Soak::None` | With `CAMERA_PLAN`: capture only, then the detector check per mode |

Every plan that uses the strip path needs `RUN_BENCH = false` (checked at compile time).
`camera::Plan` holds the modes (indexes into `modes::MODES`) or the requests, the sensor
settings (`camera::Sensor`: exposure 800 lines, clamped per mode to its longest, gain 2x,
continuous clock, scene or a test pattern), the measurement window (5 s), the `cache_check`
and `still_dump` switches, `still_patterns` (one extra still per listed `ov9281::TestPattern`,
selected while the sensor streams; three frames are dropped before each, and the plan's own
setting is restored afterwards), and the soak: `soak` (frames) or `strips`
(`strip_soak::Plan`: the soak, or with `check: Some(window)` the strip check). Stills are
named `<w>x<h>-<fps>fps-raw<bits>-<pattern>`, `<fps>` being the requested rate rounded and
`<pattern>` one of `scene`, `bars`, `random`, `greysquares`, `bwsquares`, `rollingbars`, for
example `320x200-453fps-raw8-bars`; the request verification prefixes `<request>-` and, for
its reference, `<request>-ref-`.

## What the firmware does

At boot, in order:

1. Switches the log UART to 2 Mbaud, then prints the chip revision, CPU clock, MAC, and the L1
   data and L2 cache sizes read back from the cache registers.
2. Brings up PSRAM through esp-hal, prints its size and bus clock, and runs a write/read-back
   pattern test over the last 4 MiB plus an aliasing check.
3. Measures one core's sequential read speed over 1 MiB of cold PSRAM and 128 KiB of internal
   RAM.
4. Starts I2C0 at 100 kHz, scans the bus and identifies the OV9281 (`Ov9281::identify`, chip
   ID at 0x60, then 0x10). This step and the next two run in a task of their own (`app`):
   inside `main`, the sensor futures would push `main`'s future over the stack-frame limit.
5. If `RUN_BENCH`: enables PIE on this core (`esp32p4_pie::Pie::enable`), runs the four PIE
   kernel self-tests (`bench::pie_self_tests`), then the detector benchmark on synthetic
   frames (`bench::run`).
6. If `RUN_CAMERA`: the camera section (`camera::run`). It turns on LDO VO3 at 2.5 V (the
   D-PHY supply), then for each mode of the plan:
   1. writes the mode (`Ov9281::configure`: the crate's register list from a software reset
      to standby), the exposure (clamped to the mode's maximum and printed as applied), gain,
      and optionally a test pattern, the gated clock (`write_register(0x4800, 0x60)`) or PSV
      forced off; logs the solver's choice and reads the main registers back;
   2. configures the receiver from the mode (format, width, height) and starts it, checks the
      lanes are in stop state, starts the stream, waits for the first frame (500 ms plus two
      frame periods, then the driver's checklist);
   3. measures 5 s of capture: frames, sizes against the configured frame length, error
      counters, and the measured frame rate next to the sensor's predicted one. Nothing is
      logged inside the window;
   4. optionally times the cache invalidation, checks cache coherence and dumps the stills;
   5. runs the frame soak, or in a capture-only plan the detector check (below);
   6. writes `STREAM_OFF`, waits 20 ms, stops the receiver.

   The strip path (strip soak or strip check) skips steps 3 and 4. The strip check first
   captures its reference frames with a whole-frame receiver on the PSRAM buffers, for every
   mode, and drops it; the strip soak never creates one. Consecutive modes with the same strip
   size share one strip receiver; at a change it is dropped and a new one is built on the same
   320 KiB, split for that size. The receiver and the sensor stream are started for every run
   and stopped before the firmware logs or prepares the next run.

The detector check (capture-only plans) runs after the measurement window, once per mode. The
detector processes one held captured frame: `process_frame` at threshold 128 for RAW8,
`process_frame_raw10` at 512 for RAW10. For a RAW10 mode it also processes a `Synth::fill_raw10`
frame of the mode's size with 16 discs, held in PSRAM and written back from the cache first.
Each call prints its blob count, the detector overflow and the time from the cycle counter.

| File | What |
|---|---|
| `src/main.rs` | Boot report: log baud switch, chip and cache info, PSRAM init and test, read-speed kernels, SCCB scan and sensor identification. Pin and address choices, and the plans (see "Choosing what runs"), are constants at the top |
| `src/modes.rs` | The 12 modes as named `ov9281` mode requests (`CamMode`: name and solved `ov9281::Mode`), built in `const` context so a request the builder refuses fails the build |
| `src/requests.rs` | Request verification (`REQUESTS_PLAN`): the request list, `build()` timed with the cycle counter on the P4, the long-VTS reference and the capture of each accepted request |
| `src/camera.rs` | Camera section: LDO VO3, sensor set-up through `ov9281::Ov9281`, the CSI receiver (`esp32p4-csi`) with 3 frame buffers at the start of PSRAM (each sized for 1280x800 RAW10), capture measurement against the crate's frame-period prediction, cache check, still dumps, detector check, and the strip path (receiver, mode groups, the random test pattern for the strip check) |
| `src/soak.rs` | Frame soak: per mode, 30 s on live frames, then 10 s each on synthetic frames with 0, 16 and 64 discs, while capture keeps running; result table |
| `src/strip_soak.rs` | Strip soak: the same runs on strips in a 320 KiB ring in internal RAM (RAW8: 4 strips of 80 KiB at 1280 wide, 8 of 40 KiB at 640, 64 lines; RAW10: 3 strips of 102 400 B, 64 lines at 1280 and 128 at 640), with per-frame processing and latency, per-strip hold times, and the driver's interrupt and invalidation times. `StripCheck`: row checksums of the reference frames and the strip check against them |
| `src/bench.rs` | PIE kernel self-tests; detector benchmark on synthetic frames: 4 modes x 3 blob counts x up to 3 memory paths, 50 passes each, timed with the core's counters (`esp32p4-perf`); then the cost of a dark row at 5 widths, of busy rows (a bright pixel in every row or every 8th row) and the cost of a blob at 4 disc sizes |
| `../espion-vision/src/lib.rs` (crate `espion-vision`) | Blob detector (`core` only, no heap): threshold, 8-connected runs with union-find, weighted centroids in 1/32 px. `DefaultDetector` = 256 runs per row, 256 live components, 256 blobs, about 19.5 KB. RAW8 or packed RAW10 (`begin_frame_raw10`, `process_frame_raw10`); whole frame (`process_frame`) or strips (`begin_frame` / `push_rows` / `finish`) |
| `../espion-vision/src/synth.rs` (crate `espion-vision`) | Deterministic synthetic frames: noise 4..19, discs of radius 3..5 peaking at 250. API: `Synth::NEW` (keep it in a static, it holds a 3 KiB disc list), `Synth::fill(&mut self, buf, width, height, blobs, seed)` for RAW8 and `Synth::fill_raw10` for the same scene in packed RAW10 |
| `src/jpeg_bench.rs` | `JPEG_PLAN`: test and benchmark of the hardware JPEG encoder (`esp32p4-jpeg`): test picture, camera stills, speed table, capture-and-encode runs, robustness |
| `../esp32p4-jpeg` (crate) | Hardware JPEG encoder driver (greyscale) and its 2D-DMA channels, on the PAC |
| `../ov9281` (crate) | The sensor driver: mode builder and solver, the measured limits, the register list, SCCB control (`firmware/ov9281/README.md`) |
| `tools/still.py` | Decodes the still dumps of a log into PGM and PNG files, and the JPEG dumps into `.jpg` files |
| `tools/jpeg_check.py` | Checks the board's JPEG files against libjpeg-turbo (decoding, PSNR, size against `cjpeg`) |

## Measured results

All figures are from this board on 2026-11-10. The boot report and benchmark are from the
firmware as committed then; the capture results from commit `c0a70eb` and the soak from
`08b7bef`, both with frame buffers of 1 028 096 B (1280x800 RAW8; the buffers are now sized
for RAW10). The results for all 12 modes and RAW10 are from the later code named in their
section.

### Chip and memory

| Item | Value |
|---|---|
| Chip | ESP32-P4 rev v3.2, crystal 40 MHz, flash 32 MB (`espflash board-info`) |
| ROM / bootloader | `esp32p4-eco7-20260109`; ESP-IDF v6.1 2nd-stage bootloader bundled with espflash 4.7.0 |
| CPU | 400 MHz (`CpuClock::max()`), checked with the `mcycle` counter |
| L1 data cache | 64 KiB, 64 B lines |
| L2 cache | 128 KiB, 64 B lines (set by the bootloader) |
| PSRAM | 32 MiB, auto-detected, mapped at 0x4800_0000, 250 MHz (MPLL 500 MHz / 2), 16-line DDR. Pattern test and aliasing check pass |
| Internal RAM for the app | 568 KiB (0x4FF2_0000..0x4FFA_DFC0) with the 128 KiB L2 cache |

### Read speed, one core

1 MiB in PSRAM, cache written back and invalidated before each pass; 128 KiB in internal RAM.
Four passes averaged. MB = 10^6 bytes.

| Buffer | Byte sum | Word sum (16 loads per 64 B line) | One load per 64 B line |
|---|---|---|---|
| PSRAM, cold | 47 MB/s | 120 MB/s | 163 MB/s |
| Internal RAM | 65 MB/s | 355 MB/s | 1424 MB/s (1598 MB/s in an earlier build) |

- The byte sum is limited by the CPU, not the memory: it compiles to a 4-instruction loop and
  takes about 6 cycles per byte even from internal RAM.
- The last column is the cache-fill ceiling. Its internal-RAM figure moved from 1598 to
  1424 MB/s when the buffer moved from 0x4FF2_9F18 to 0x4FF2_A320 between builds
  [INFERENCE: buffer alignment; the buffer was not cache-line aligned then]. It is now
  (`SRAM_BENCH`, 64-byte aligned): on 2026-11-11 three builds with it at 0x4FF3_1440,
  0x4FF3_15C0 and 0x4FF3_12C0 read 1598, 1579 and 1598 MB/s (32 849, 32 850 and 32 830 cycles
  per pass: the same within 0.1 %; logs `/tmp/p4perf-logs/BenchFlags/{A-unaligned,B-aligned,C-nofp}.log`).

### Detector benchmark (synthetic frames)

Threshold 128, every blob of one pixel or more kept, 50 passes per case, timed with `mcycle`
(us = cycles / 400). The 120 fps frame period is 8333 us; the verdict is on the max. Each mode
and blob count uses one synthetic frame in PSRAM, so all paths see the same content:

- **psram**: the whole frame in PSRAM through `process_frame`, its cache lines written back
  and invalidated before every pass, as after a DMA write.
- **strip**: each 64-line strip of the PSRAM frame is copied into one internal buffer and
  flushed from the cache; only `begin_frame`, the `push_rows` calls and `finish` are timed and
  summed per frame. This stands in for CSI DMA into a ring of internal strips.
- **sram**: the whole frame in internal RAM, flushed before every pass. Only for the modes
  that fit.

Final flashed build:

| Mode | Blobs | Path | Min | Mean | Max | Found | Overflow | 120 fps |
|---|---|---|---|---|---|---|---|---|
| 1280x800 | 0 | psram | 9822 | 9826 | 10045 | 0 | 0 | OVER |
| 1280x800 | 0 | strip | 4226 | 4228 | 4314 | 0 | 0 | OK |
| 1280x800 | 16 | psram | 10038 | 10044 | 10337 | 16 | 0 | OVER |
| 1280x800 | 16 | strip | 4471 | 4472 | 4551 | 16 | 0 | OK |
| 1280x800 | 64 | psram | 10737 | 10740 | 10875 | 64 | 0 | OVER |
| 1280x800 | 64 | strip | 5167 | 5168 | 5247 | 64 | 0 | OK |
| 1280x720 | 0 | psram | 8840 | 8841 | 8906 | 0 | 0 | OVER |
| 1280x720 | 0 | strip | 3804 | 3805 | 3866 | 0 | 0 | OK |
| 1280x720 | 16 | psram | 9045 | 9048 | 9170 | 16 | 0 | OVER |
| 1280x720 | 16 | strip | 4039 | 4040 | 4119 | 16 | 0 | OK |
| 1280x720 | 64 | psram | 9756 | 9759 | 9879 | 64 | 0 | OVER |
| 1280x720 | 64 | strip | 4746 | 4747 | 4826 | 64 | 0 | OK |
| 640x480 | 0 | psram | 3207 | 3209 | 3273 | 0 | 0 | OK |
| 640x480 | 0 | strip | 1530 | 1531 | 1592 | 0 | 0 | OK |
| 640x480 | 0 | sram | 1526 | 1528 | 1603 | 0 | 0 | OK |
| 640x480 | 16 | psram | 3425 | 3428 | 3556 | 16 | 0 | OK |
| 640x480 | 16 | strip | 1777 | 1778 | 1857 | 16 | 0 | OK |
| 640x480 | 16 | sram | 1742 | 1744 | 1834 | 16 | 0 | OK |
| 640x480 | 64 | psram | 4106 | 4109 | 4251 | 64 | 0 | OK |
| 640x480 | 64 | strip | 2455 | 2456 | 2535 | 64 | 0 | OK |
| 640x480 | 64 | sram | 2420 | 2422 | 2519 | 64 | 0 | OK |
| 640x400 | 0 | psram | 2674 | 2675 | 2739 | 0 | 0 | OK |
| 640x400 | 0 | strip | 1276 | 1277 | 1338 | 0 | 0 | OK |
| 640x400 | 0 | sram | 1273 | 1274 | 1331 | 0 | 0 | OK |
| 640x400 | 16 | psram | 2908 | 2911 | 3039 | 16 | 0 | OK |
| 640x400 | 16 | strip | 1539 | 1541 | 1627 | 16 | 0 | OK |
| 640x400 | 16 | sram | 1505 | 1507 | 1597 | 16 | 0 | OK |
| 640x400 | 64 | psram | 3624 | 3626 | 3768 | 64 | 0 | OK |
| 640x400 | 64 | strip | 2251 | 2253 | 2331 | 64 | 0 | OK |
| 640x400 | 64 | sram | 2217 | 2219 | 2320 | 64 | 0 | OK |

Times in us. An earlier build gave the same min and mean within 4 us for the psram and sram
paths, and the strip path with 16 or 64 blobs 27–29 us faster; no verdict changed.

Per resolution (max over 50 passes, 0 to 64 blobs):

| Mode | Whole frame from PSRAM | Internal 64-line strips | Whole frame in internal RAM |
|---|---|---|---|
| 1280x800 | 10.0–10.9 ms, over budget | 4.3–5.2 ms, fits | does not fit |
| 1280x720 | 8.9–9.9 ms, over budget (also with 0 blobs) | 3.9–4.8 ms, fits | does not fit |
| 640x480 | 3.3–4.3 ms, fits | 1.6–2.5 ms, fits | 1.6–2.5 ms, fits |
| 640x400 | 2.7–3.8 ms, fits | 1.3–2.3 ms, fits | 1.3–2.3 ms, fits |

- At 1280x800 a dark frame costs about 1.65 cycles per pixel from internal strips and about
  3.8 cycles per pixel from cold PSRAM (about 104 MB/s). PSRAM is 2.1–2.3 times slower for the
  same frame.
- Strips cost the same as a whole frame in internal RAM; the gap to the psram path is the
  cost of reading PSRAM.
- Each blob adds about 14 us at 1280 px width.
- Every case found exactly the number of discs drawn, with no overflow.

**What fits in internal RAM:** 640x400 (250 KiB) and 640x480 (300 KiB) fit next to this
firmware's other buffers. 1280x720 (900 KiB) and 1280x800 (1000 KiB) are larger than all
internal RAM. A 64-line strip at 1280 px is 80 KiB.

Limits of these numbers:

- No CSI DMA ran at the same time, on the PSRAM bus or into internal RAM. These are lower
  bounds for the live case [INFERENCE: the live PSRAM path will be slower].
- Cache maintenance is untimed in every path. The live firmware must invalidate each frame
  or strip before reading it; for whole frames that cost was measured later (17–31 us per
  frame, see "Camera capture").
- The strip path is a stand-in: DMA set-up and per-strip interrupt latency are not included.
  The strip soak below measures them under live capture.
- Synthetic frames: a dark background (4..19) and round discs. Real frames with brighter
  backgrounds take the detector's per-pixel path more often [INFERENCE].
- Between builds, detector times moved by up to 2 % and the internal-RAM line-load figure by
  11 %. For the detector, the counters later traced this to code layout, not to the address
  of its data ("Detector benchmark with counters", alignment).

### Detector benchmark with counters (2026-11-11)

The same benchmark with the core's counters read around every timed window
([`esp32p4-perf`](../esp32p4-perf/README.md), `init` once before the first case), and two
sections that the first one did not have: the cost of a dark row and the cost of a blob. Build:
the commit that adds this section, with `RUN_BENCH = true` and `RUN_CAMERA = false`, frame
pointers on (`-C force-frame-pointers`, as `.cargo/config.toml` had it then). Log
`/tmp/p4perf-logs/BenchFlags/A-unaligned.log`. The whole benchmark runs in about 15 s after
the boot report.

- Every window holds only detector calls (`window` in `src/bench.rs`); the strip path sums 15
  windows per frame (`begin_frame`, 13 `push_rows`, `finish`). Copies and cache flushes stay
  outside. Each window counts the 5 closing counter reads as instructions (75 per strip
  frame).
- Counters are per-frame means over the 50 passes; IPC = instructions / cycles. Mean us is
  from the time table of the same run.

Counters per case:

| Mode | Blobs | Path | Mean us | Cycles | Instructions | IPC | Branches | Mispredicts | Stores | l1i_miss | l1d_miss |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1280x800 | 0 | psram | 9930 | 3 972 087 | 920 336 | 0.231 | 82 670 | 1612 | 33 525 | 9427 | 2 369 793 |
| 1280x800 | 0 | strip | 4320 | 1 728 342 | 921 639 | 0.533 | 82 706 | 1603 | 33 885 | 997 | 161 759 |
| 1280x800 | 16 | psram | 10132 | 4 053 161 | 973 529 | 0.240 | 90 164 | 2931 | 36 502 | 9592 | 2 370 683 |
| 1280x800 | 16 | strip | 4575 | 1 830 331 | 974 832 | 0.532 | 90 200 | 2962 | 36 862 | 21 501 | 162 074 |
| 1280x800 | 64 | psram | 10784 | 4 313 903 | 1 145 851 | 0.265 | 115 024 | 6847 | 45 604 | 9021 | 2 371 472 |
| 1280x800 | 64 | strip | 5261 | 2 104 554 | 1 147 154 | 0.545 | 115 060 | 6915 | 45 964 | 40 475 | 162 876 |
| 1280x720 | 0 | psram | 8935 | 3 574 287 | 828 456 | 0.231 | 74 430 | 1443 | 30 205 | 7761 | 2 133 592 |
| 1280x720 | 0 | strip | 3888 | 1 555 491 | 829 651 | 0.533 | 74 463 | 1443 | 30 535 | 466 | 145 608 |
| 1280x720 | 16 | psram | 9129 | 3 651 716 | 878 731 | 0.240 | 81 536 | 2683 | 33 030 | 8697 | 2 134 035 |
| 1280x720 | 16 | strip | 4107 | 1 642 925 | 879 926 | 0.535 | 81 569 | 2702 | 33 360 | 10 826 | 146 388 |
| 1280x720 | 64 | psram | 9795 | 3 918 318 | 1 054 160 | 0.269 | 106 971 | 6704 | 42 164 | 8841 | 2 134 628 |
| 1280x720 | 64 | strip | 4834 | 1 933 945 | 1 055 355 | 0.545 | 107 004 | 6730 | 42 494 | 44 921 | 146 708 |
| 640x480 | 0 | psram | 3276 | 1 310 490 | 332 016 | 0.253 | 30 510 | 963 | 20 245 | 7645 | 711 471 |
| 640x480 | 0 | strip | 1586 | 634 619 | 332 779 | 0.524 | 30 531 | 963 | 20 455 | 472 | 49 069 |
| 640x480 | 0 | sram | 1581 | 632 572 | 332 016 | 0.524 | 30 510 | 963 | 20 245 | 531 | 48 549 |
| 640x480 | 16 | psram | 3483 | 1 393 232 | 385 797 | 0.276 | 38 193 | 2243 | 23 154 | 8882 | 711 702 |
| 640x480 | 16 | strip | 1807 | 723 186 | 386 560 | 0.534 | 38 214 | 2235 | 23 364 | 7562 | 49 462 |
| 640x480 | 16 | sram | 1786 | 714 603 | 385 797 | 0.539 | 38 193 | 2244 | 23 154 | 1386 | 48 451 |
| 640x480 | 64 | psram | 4118 | 1 647 281 | 552 757 | 0.335 | 62 508 | 6179 | 31 883 | 8722 | 712 278 |
| 640x480 | 64 | strip | 2465 | 986 335 | 553 520 | 0.561 | 62 529 | 6218 | 32 093 | 19 428 | 49 898 |
| 640x480 | 64 | sram | 2422 | 968 998 | 552 757 | 0.570 | 62 508 | 6179 | 31 883 | 1769 | 48 796 |
| 640x400 | 0 | psram | 2733 | 1 093 547 | 276 936 | 0.253 | 25 470 | 807 | 16 925 | 7646 | 592 846 |
| 640x400 | 0 | strip | 1323 | 529 420 | 277 591 | 0.524 | 25 488 | 803 | 17 105 | 564 | 40 928 |
| 640x400 | 0 | sram | 1319 | 527 612 | 276 936 | 0.524 | 25 470 | 803 | 16 925 | 531 | 40 469 |
| 640x400 | 16 | psram | 2956 | 1 182 647 | 334 920 | 0.283 | 33 683 | 2178 | 20 107 | 8740 | 593 102 |
| 640x400 | 16 | strip | 1547 | 618 984 | 335 575 | 0.542 | 33 701 | 2212 | 20 287 | 1674 | 41 279 |
| 640x400 | 16 | sram | 1540 | 616 202 | 334 920 | 0.543 | 33 683 | 2166 | 20 107 | 1387 | 40 340 |
| 640x400 | 64 | psram | 3623 | 1 449 298 | 510 763 | 0.352 | 59 521 | 6079 | 29 052 | 8786 | 593 735 |
| 640x400 | 64 | strip | 2228 | 891 258 | 511 418 | 0.573 | 59 539 | 6078 | 29 232 | 8063 | 41 773 |
| 640x400 | 64 | sram | 2207 | 882 903 | 510 763 | 0.578 | 59 521 | 6073 | 29 052 | 1768 | 40 731 |

**Row cost.** All-dark rows (bytes 0..63, so the dark skip never stops) through
`begin_frame` + `push_rows`, per row, means over 5 passes after one untimed pass. Warm: 7680 B
of rows (6 rows of 1280) pushed again and again as consecutive rows, 1200 rows per pass, so
they stay in the L1 data cache. Cold: one 80 KiB buffer flushed before each pass (2560 rows at
width 32, 64 at 1280). The fits are least squares over the five widths, `cost = per row +
per pixel x width`:

| Width | Warm cycles | Warm instructions | Warm branches | Warm mispredicts | Warm stores | Cold cycles | Cold instructions | Cold l1d_miss |
|---|---|---|---|---|---|---|---|---|
| 32 | 509.5 | 253.0 | 25.2 | 1.0 | 41.9 | 511.0 | 252.1 | 5.0 |
| 64 | 549.4 | 276.5 | 27.2 | 1.0 | 42.0 | 553.6 | 275.6 | 10.1 |
| 256 | 791.6 | 417.0 | 39.3 | 2.0 | 42.7 | 814.6 | 416.9 | 41.3 |
| 640 | 1271.7 | 698.2 | 63.5 | 2.5 | 44.2 | 1329.5 | 699.5 | 101.1 |
| 1280 | 2069.3 | 1166.8 | 103.9 | 2.5 | 46.7 | 2186.0 | 1170.6 | 201.8 |

| Fit | Per row | Per pixel | Largest residual |
|---|---|---|---|
| warm cycles | 470.3 | 1.250 | 1.4 |
| warm instructions | 229.6 | 0.732 | 0.0 |
| cold cycles | 469.1 | 1.342 | 1.9 |
| cold instructions | 228.5 | 0.736 | 0.0 |

**Blob cost.** The 1280x800 strip path with 64 discs of one radius drawn over the `synth`
background (seed 0x1234_5678): `synth`'s profile and spacing (peak 250, falling to the
background at radius + 1; bounding squares at least 2 pixels apart), drawn by `draw_discs` in
`src/bench.rs` because `synth` itself draws radius 3..5 only. Per blob = (frame - the same
background without discs) / found, from the per-frame means. Mean area is the detector's own
count of pixels at or above the threshold:

| Radius | Found | Mean area (px) | Mean us | Cycles | Instructions | Cycles per blob | Instructions per blob | Branches per blob | Mispredicts per blob | Stores per blob |
|---|---|---|---|---|---|---|---|---|---|---|
| none | 0 | - | 4324 | 1 729 705 | 921 639 | - | - | - | - | - |
| 2 | 64 | 13 | 5021 | 2 008 402 | 1 072 326 | 4354 | 2354 | 332 | 65 | 145 |
| 4 | 64 | 37 | 5291 | 2 116 426 | 1 150 289 | 6042 | 3572 | 514 | 82 | 190 |
| 8 | 64 | 121 | 6170 | 2 468 163 | 1 390 590 | 11 538 | 7327 | 1072 | 158 | 321 |
| 12 | 64 | 261 | 7254 | 2 901 792 | 1 696 702 | 18 313 | 12 110 | 1777 | 219 | 453 |

Where the cycles of a dark 1280x800 strip frame go (1.728 M cycles, 0.922 M instructions):

- **The dark skip: 1.28 to 1.37 M cycles (74 to 79 %).** 1.25 cycles per pixel with the rows
  in the L1 cache, 1.34 cold, for 1 024 000 pixels; 0.73 instructions per pixel. Line fills
  add only 7 % (cold against warm): the skip is not waiting for memory.
- **A fixed cost per row: about 470 cycles, 230 instructions, 42 stores, 25 branches and 1 to
  2.5 mispredictions (0.38 M cycles, 22 %).** Warm and cold give the same figure, so it is
  code, not memory. The two parts add up to 1.75 M cycles against 1.73 M measured.
- **Each blob** costs 4.4 k cycles at 13 pixels, 6.0 k at 37 and 18.3 k at 261; from radius 4
  to 12, about 55 cycles and 38 instructions per added foreground pixel. Mispredictions per
  blob rise from 65 to 219. The 64 `synth` discs of the case table (radius 3..5) cost
  (2 104 554 - 1 728 342) / 64 = 5878 cycles and 3524 instructions per blob.
- **PSRAM** runs the same instructions at IPC 0.23 instead of 0.53: the whole-frame path
  waits for memory.

Against the spike of 2026-11-11 (same source with its own counter code; plan document
`p4perf-spike.md`, section S1): dark strip frame 4320 against 4328 us, 1 728 342 against
1 731 750 cycles, 921 639 against 921 955 instructions; per-row fit 470.3 + 1.250 per pixel
against 470 + 1.25. Within the noise floor (2 to 3 % between builds). The 64-blob strip frame
is 1.5 % slower (2 104 554 against 2 073 575 cycles: 5878 against 5341 cycles per blob) with
the same instructions per blob (3524) and 40 475 against 1 641 L1 instruction misses per frame:
a code-layout difference [INFERENCE on the cause; both counts are measured]. Mispredictions on
the dark frame are 1603 here against 2390 in the spike, with the same code.

Counter caveats (from the `esp32p4-perf` README):

- Every counter counts on core 0 including interrupt time: the esp-rtos timer tick and any
  other interrupt that falls into a window is counted with it.
- The L1 miss counters count miss events, including retries while a line fill is pending, not
  lines: about 10 per 64-byte line read cold from internal RAM, about 148 from PSRAM. Compare
  them between runs of the same code only.
- The branch counters count conditional branches only (not jumps or calls).
- All counters are 32 bits wide; every window here is far shorter than the 10.7 s wrap of
  `mcycle`.

**Cache-line alignment of the statics: tried, not kept.** The detector state, its blob
output and the benchmark's 300 KiB buffer (and the frame soak's detector and blob output) were
put on 64-byte boundaries (`#[repr(C, align(64))]` wrapper), then each layout was rebuilt with
one unrelated static added to move them. Cycles per frame, with L1 instruction miss events in
brackets (logs `/tmp/p4perf-logs/BenchFlags/{A-unaligned,A2-unaligned-pad,B-aligned,B2-aligned-pad}.log`,
frame pointers on):

| Build | 1280x800 0 strip | 1280x800 16 strip | 1280x800 64 strip | 1280x800 0 psram | 1280x800 64 psram | 640x400 64 strip |
|---|---|---|---|---|---|---|
| unaligned | 1 728 342 (997) | 1 830 331 (21 501) | 2 104 554 (40 475) | 3 972 087 (9427) | 4 313 903 (9021) | 891 258 (8063) |
| unaligned + 36 B static | 1 729 010 (1605) | 1 833 263 (24 332) | 2 104 686 (40 480) | 3 972 091 (9425) | 4 313 911 (9037) | 891 278 (8036) |
| aligned | 1 728 338 (1074) | 1 850 132 (41 358) | 2 143 145 (73 380) | 3 972 232 (9561) | 4 313 846 (9060) | 895 663 (12 455) |
| aligned + 100 B static | 1 728 312 (1074) | 1 851 266 (42 553) | 2 143 649 (73 971) | 3 972 120 (9470) | 4 313 845 (9055) | 895 653 (12 458) |

Addresses (detector / blob output / buffer): unaligned 0x4FF2_BB08 / 0x4FF5_348C / 0x4FF5_4894,
moved by 40 / 64 / 64 bytes with the extra static; aligned 0x4FF2_BC00 / 0x4FF5_36C0 /
0x4FF5_4B40, moved by 64 bytes each.

- Moving the data changed no figure by more than 0.16 %, aligned or not.
- The aligned build was 1.1 % (16 blobs) and 1.8 % (64 blobs) slower than the unaligned one,
  with the same instructions and twice the L1 instruction misses: its code sits elsewhere.
- So the build-to-build movement of up to 2 % comes from code layout, not from the address of
  the detector's data [INFERENCE on the mechanism; the miss counts are measured]. The change
  showed no gain and was reverted.

### Frame pointers (2026-11-11)

`.cargo/config.toml` no longer passes `-C force-frame-pointers`. The same source as
"Detector benchmark with counters" built both ways (logs `/tmp/p4perf-logs/BenchFlags/A-unaligned.log`,
with, and `C-nofp.log`, without):

| Mode | Blobs | Path | Mean us | Cycles | Instructions | Mispredicts | l1i_miss | Cycles change | Instructions change |
|---|---|---|---|---|---|---|---|---|---|
| 1280x800 | 0 | strip | 4320 → 4238 | 1 728 342 → 1 695 529 | 921 639 → 909 570 | 1603 → 1618 | 997 → 1718 | -1.9 % | -1.3 % |
| 1280x800 | 0 | psram | 9930 → 9833 | 3 972 087 → 3 933 276 | 920 336 → 908 326 | 1612 → 1614 | 9427 → 2612 | -1.0 % | -1.3 % |
| 1280x800 | 16 | strip | 4575 → 4452 | 1 830 331 → 1 780 856 | 974 832 → 961 722 | 2962 → 3029 | 21 501 → 6645 | -2.7 % | -1.3 % |
| 1280x800 | 16 | psram | 10132 → 10038 | 4 053 161 → 4 015 373 | 973 529 → 960 491 | 2931 → 2999 | 9592 → 2674 | -0.9 % | -1.3 % |
| 1280x800 | 64 | strip | 5261 → 5107 | 2 104 554 → 2 042 823 | 1 147 154 → 1 130 803 | 6915 → 7360 | 40 475 → 6821 | -2.9 % | -1.4 % |
| 1280x800 | 64 | psram | 10784 → 10688 | 4 313 903 → 4 275 410 | 1 145 851 → 1 129 572 | 6847 → 7257 | 9021 → 982 | -0.9 % | -1.4 % |
| 640x400 | 0 | strip | 1323 → 1280 | 529 420 → 512 389 | 277 591 → 271 545 | 803 → 811 | 564 → 349 | -3.2 % | -2.2 % |
| 640x400 | 0 | psram | 2733 → 2676 | 1 093 547 → 1 070 742 | 276 936 → 270 926 | 807 → 808 | 7646 → 292 | -2.1 % | -2.2 % |
| 640x400 | 16 | strip | 1547 → 1514 | 618 984 → 605 877 | 335 575 → 328 434 | 2212 → 2320 | 1674 → 6561 | -2.1 % | -2.1 % |
| 640x400 | 16 | psram | 2956 → 2906 | 1 182 647 → 1 162 794 | 334 920 → 327 815 | 2178 → 2259 | 8740 → 3843 | -1.7 % | -2.1 % |
| 640x400 | 64 | strip | 2228 → 2177 | 891 258 → 871 182 | 511 418 → 500 696 | 6078 → 6532 | 8063 → 6850 | -2.3 % | -2.1 % |
| 640x400 | 64 | psram | 3623 → 3570 | 1 449 298 → 1 428 273 | 510 763 → 500 077 | 6079 → 6540 | 8786 → 3759 | -1.5 % | -2.1 % |

- Every case is faster: 1 to 3 % in cycles, 1.3 to 2.2 % fewer instructions. The
  instruction counts are the firm part; the cycle figures also hold code-layout noise (the
  L1 instruction misses moved both ways).
- The dark-row fit: 470.3 → 429.0 cycles and 229.6 → 214.6 instructions per row (warm), the
  per-pixel part unchanged (1.250 → 1.249): each row's calls no longer save and set up `s0`.
  Stores per row 46.7 → 44.4 at 1280.
- Per blob (radius 2 / 4 / 8 / 12): 4354 / 6042 / 11 538 / 18 313 → 3754 / 5513 / 10 939 /
  17 580 cycles; 2354 / 3572 / 7327 / 12 110 → 2304 / 3502 / 7199 / 11 919 instructions.
- Mispredictions per blob frame rose slightly (64 synth discs: 6915 → 7360), the dark frame
  stayed the same (1603 → 1618). The spike's drop from 2390 to 1609 on the dark frame was a
  layout effect: this baseline already had 1603.

**What a panic prints without them.** esp-backtrace 0.20's build script looks for
`force-frame-pointers` in the rustflags. With it, the panic handler walks the frame pointers.
Without it, the handler never walks them (no garbage addresses, no fault while walking):
it prints the message and location, then one `STACKDUMP:` line, the hex of up to 8 KiB of
stack (`ESP_BACKTRACE_CONFIG_STACK_DUMP_MAX_SIZE`, default `8K`). `espflash monitor --elf`
decodes that line into a backtrace from the ELF's `.eh_frame`, but only without
`--no-addresses`. No esp-config key or cargo feature of esp-backtrace changes this, and the
defaults are kept: nothing misleading is printed, and the dump is complete. For a panic hunt,
run the monitor without `--no-addresses`. `.cargo/backtrace.toml` brings the frame pointers
back: `cargo build --release --config .cargo/backtrace.toml` (cargo appends that file's
`build.rustflags` to the ones of `.cargo/config.toml`, checked with `cargo config get
--show-origin`; that build is byte-identical to the build of the previous config).

Checked on the board with a temporary `fault_outer` → `fault_inner` call after the boot report
(not committed), both builds, monitor without `--no-addresses` (logs
`/tmp/p4perf-logs/BenchFlags/{P,E}-{fp,nofp}.log`):

- Panic, frame pointers: `panicked at src/main.rs:351:14: deliberate panic (backtrace check)`,
  then `Backtrace:` with `fault_inner` (main.rs:351), `fault_outer` (main.rs:344), the `app`
  task, the embassy executor, `esp_rtos` `Executor::run`.
- Panic, no frame pointers: the same message and location, `STACKDUMP: 4004454c ...`, then
  `Backtrace (decoded from the stack dump):` with `dump_stack`, `rust_begin_unwind`,
  `panic_fmt`, `fault_inner` (main.rs:351), `(reconstructed) fault_outer` (main.rs:344), the
  `app` task, the executor with its inlined frames, `main`, `hal_main`. With
  `--no-addresses` only the `STACKDUMP` hex line follows the message.
- Illegal instruction, both builds: `panicked at .../esp-hal-1.2.2/src/exception_handler/mod.rs:117:5:
  [ProCpu] Exception 'Illegal instruction (2)' mepc=0x4001e04e, mtval=0x00000000` (0x4001e338
  with frame pointers), `mepc` resolved to `fault_inner` at main.rs:353, and the trap frame's registers.
  - With frame pointers the walk goes `ExceptionHandler`, `_start_trap_rust_hal`,
    `_dispatch_exception`, the trap entry (0x4FF2_04B4, unnamed), then the `app` task: it
    skips `fault_outer`, the caller of the faulting function.
  - Without, the decoded dump stops at `ExceptionHandler` with `backtrace stopped:
    0x40044f44 didn't save the return address of its caller`. The faulting place is in the
    message (`mepc`).
- Every case printed the panic once and then stayed in esp-backtrace's halt loop with
  interrupts off: no reset, no second fault, through the rest of the 15 s capture.

### PIE self-tests at boot (2026-11-11)

The bench build (`RUN_BENCH = true`) enables PIE on core 0 and runs the self-test of each
kernel of [`esp32p4-pie`](../esp32p4-pie/README.md) against its scalar reference: 12 288
randomised cases each (48 rounds of the 256 source and destination alignment pairs that
`self_test_copy` walks through), seed `0x5EED_0001`. First run on the board, two sessions
with the same results (logs `/tmp/p4perf-logs/DetectorOneCore/s0-base.log` and, for the
committed source, `s0b-base.log`; times of the latter):

| Kernel | Failures | Time (whole self-test, cases generated and checked byte by byte) |
|---|---|---|
| `first_bright` | 0 of 12 288 | 612 ms |
| `first_bright10` | 0 of 12 288 | 887 ms |
| `copy` | 0 of 12 288 | 816 ms |
| `sum16_be` | 0 of 12 288 | 322 ms |

The times are mostly the case generation and the byte-wise checks, not the kernels.

### Busy rows (2026-11-11)

The all-dark rows of "Row cost" stop showing the cost of rows near blobs once dark rows are
skipped in whole spans (`espion-vision`, span skip). The bench adds two row cases at 1280
wide: dark rows (bytes 0..63) with one pixel of 255 in column 643 of every row (`column`:
one run per row, linked to the run above, one component through all rows) and of every 8th
row (`every 8th`: a one-pixel blob, entered from dark rows and left again). Warm pushes the
same 8 rows again and again (1200 rows per pass), cold one 64-row buffer flushed before each
pass; means over 5 passes after one untimed pass. Same build and log (`s0b-base.log`) as the
self-tests:

| Rows | Warm cycles | Warm instructions | Warm branches | Warm mispredicts | Warm stores | Cold cycles | Cold instructions | Cold l1d_miss |
|---|---|---|---|---|---|---|---|---|
| dark (from the row-cost table) | 2026.9 | 1150.8 | 103.9 | 2.8 | 44.4 | 2142.8 | 1155.5 | 202.0 |
| column | 2626.8 | 1548.7 | 162.7 | 8.0 | 66.1 | 2754.0 | 1556.8 | 204.9 |
| every 8th | 2133.7 | 1218.2 | 113.0 | 4.0 | 48.7 | 2257.6 | 1227.0 | 202.4 |

A row with one run linked to the row above costs about 600 cycles and 400 instructions more
than a dark row. The same build reproduces the frame-pointer table: dark 1280x800 strip
frame 4235 us, 1 694 165 cycles, 909 557 instructions; 64 blobs 5098 us, 2 039 229 cycles,
1 130 803 instructions.

### Detector steps (2026-11-11)

The detector restructure of `espion-vision` and the PIE skip, one step at a time, each measured
with the bench build (`RUN_BENCH = true`, `RUN_CAMERA = false`) in its own board session; logs
under `/tmp/p4perf-logs/DetectorOneCore/`. Frames: the 1280x800 strip path (dark and 64
`synth` discs), per-frame means over 50 passes. Rows: the busy-row cases, warm, per row. Per
blob: the blob-cost section at radius 4 (64 discs, 37 pixels each), and in brackets that
section's whole-frame cycles. Two builds of the same source differ by 2 to 3 % in cycles
from code layout alone; instructions are the firm part. All figures with the `Scalar` skip.

| Step (commit on `main`) | Log | Dark frame us / cycles / instructions | 64-blob frame us / cycles / instructions | Column row cycles / instructions | Every-8th row cycles / instructions | Per blob cycles / instructions (frame cycles) | Result |
|---|---|---|---|---|---|---|---|
| baseline (`dfeddcf`, busy-row cases added) | `s0b-base.log` | 4235 / 1 694 165 / 909 557 | 5098 / 2 039 229 / 1 130 803 | 2626.8 / 1548.7 | 2133.7 / 1218.2 | 5445 / 3502 (2 043 547) | - |
| one skip interface (`9420364`) | `s1-skip.log` | 4174 / 1 669 843 / 830 770 | 5165 / 2 066 297 / 1 083 102 | 2773.4 / 1571.3 | 2110.3 / 1135.0 | 6224 / 3995 (2 072 128) | kept, judged with the next two (they build on it) |
| span skip while idle (`318e1d0`) | `s2-span.log` | 3142 / 1 257 172 / 628 187 | 4655 / 1 862 035 / 987 710 | 2776.2 / 1584.7 | 1735.5 / 957.2 | 9364 / 5566 (1 856 960) | kept |
| lean row entry (`29f50b7`) | `s3-lean.log` | 3144 / 1 257 727 / 628 317 | 4640 / 1 856 201 / 969 750 | 2685.0 / 1479.7 | 1720.2 / 942.4 | 9297 / 5288 (1 853 333) | kept |
| centroid without 64-bit division (`757a3b6`) | `s4-div.log` | 3145 / 1 258 015 / 628 315 | 4601 / 1 840 714 / 964 276 | 2702.5 / 1485.7 | 1700.8 / 928.1 | 9010 / 5200 (1 835 137) | kept |
| component table without bounds checks (`0c6888c`) | `s5-slot.log` | 3145 / 1 258 221 / 628 569 | 4578 / 1 831 278 / 960 042 | 2672.5 / 1473.9 | 1700.6 / 927.1 | 8844 / 5132 (1 825 062) | kept |

- **One skip interface** alone: the dark frame 1.4 % fewer cycles and 8.7 % fewer
  instructions (the 64-byte OR loop), but the row overhead rose (dark-row fit 428.8 → 546.1
  cycles per row, 1.249 → 1.116 per pixel), the column row by 5.6 % and the blob by 14 %
  (3502 → 3995 instructions per blob; [INFERENCE] from the code: the in-row scan now calls
  the skip after each run, each call with its own head and tail). The 64-blob frame took
  1.3 % more cycles with 4.2 % fewer instructions. It is the base of the next two steps;
  after them (`29f50b7`) the 64-blob frame is 9.0 % under the baseline and the column row
  2.2 % over it in cycles with 4.5 % fewer instructions.
- **Span skip**: dark rows cost only the skip (fit 2.2 + 1.160 per pixel warm, 0 + 1.247
  cold): the dark frame 24.7 % fewer cycles, the 64-blob frame 9.9 %, the every-8th row
  17.8 %; the column row, which is never idle, unchanged. From here the per-blob figure
  also holds the rows near the blob, which the dark frame no longer pays: compare the
  frame cycles in brackets.
- **Lean row entry**: column row 3.3 % fewer cycles and 6.6 % fewer instructions, 64-blob
  frame 1.8 % fewer instructions; dark frame unchanged.
- **Centroid division**: 88 instructions and 287 cycles less per blob at radius 4 (1.0 %
  of the frame), 64-blob frame 0.8 % fewer cycles. The column row rose by 0.7 % (6
  instructions), inside the layout noise.
- **Slot index**: per blob 1.3 % fewer instructions and 1.8 % fewer cycles, 64-blob frame
  0.5 %, column row 1.1 %; the dark frame 254 instructions more (0.04 %).
- **The series** against the baseline: dark frame 4235 → 3145 us (1 694 165 → 1 258 221
  cycles, -25.7 %; 909 557 → 628 569 instructions, -30.9 %); 64-blob frame 5098 → 4578 us
  (2 039 229 → 1 831 278 cycles, -10.2 %); column row 2626.8 → 2672.5 cycles (+1.7 %) with
  1548.7 → 1473.9 instructions (-4.8 %); every-8th row -20.3 %; the radius-4 blob frame
  -10.7 %, the radius-12 blob frame 2 816 617 → 2 780 264 cycles (-1.3 %).

### Camera capture (whole frames in PSRAM)

Set-up: three frame buffers at the start of PSRAM (0x4800_0000), filled with the sentinel
0x5A before the first frame. Continuous clock lane, exposure 800 lines, gain 0x20, indoor
scene. In every mode, before stream-on, the clock and both data lanes were in stop state.

Every mode measured **119.921–119.922 fps** first-to-last over 5 s. The
sensor's frame period is `(VTS − 1) × HTS + 1730` SCLK cycles (found later, see "Frame
period"), which predicts 119.918 fps at HTS 728, VTS 915: the measurement is +25 ppm against
it. In every mode the following were all 0: wrong size, dropped, overwritten, DMA errors,
host errors (main and all 9 detail registers), bridge errors.

| Mode | Readback 0x3808..0x380B | Bytes/frame (= w·h) | `CMPLTD_BLK_TFR_SIZE` |
|---|---|---|---|
| 1280x800 | 0500 x 0320 | 1 024 000 | 127 999 |
| 1280x720 crop | 0500 x 02d0 | 921 600 | 115 199 |
| 640x480 crop | 0280 x 01e0 | 307 200 | 38 399 |
| 640x400 2x2 binned | 0280 x 0190 | 256 000 | 31 999 |

- The first run compared this with `VTS × HTS` (120.098 fps) and put the −0.15 % down to
  XVCLK being slightly under 24 MHz. The frame-period formula explains it instead; what is
  left is +25 ppm.
- The first frame's tail never held the sentinel: the DMA writes the whole frame. The
  block-size register under-reports by one item, so the driver takes the frame length from
  the DMA destination address instead.

Stills (dumped over the UART, decoded with `tools/still.py`):

| Still | Checked |
|---|---|
| 1280x800 scene (min 4, max 255, mean 41.0, 7.3 s to dump) | Office ceiling with suspended tiles: grid lines straight across the whole frame, sharp, no torn or wrapped lines |
| 1280x720 scene (mean 47.2) | Best pixel match inside the 1280x800 frame at offset (0, 40): the centred crop |
| 640x480 scene (min 5, max 83, mean 51.7) | Best match inside the 1280x800 frame at (320, 160), the exact centre, mean absolute difference 4.6 (noise level) |
| 640x400 scene (mean 75.0) | The whole field of view: correlation 0.991 with a 2x2 average of the 1280x800 frame at (0, 0). It is 1.83x brighter in the mean (about 2x above the black level at equal exposure in the later stills: "Stills, bright scene") |
| 640x400 and 1280x800 test pattern (0x5E00 = 0x80) | 8 vertical bars, 80 px apart at 640 and 162 px at 1280; every even row equals row 0 and every odd row row 1, so no shear and no lost bytes. Inside the bars each 2x2 Bayer phase is 0 or 255: the standard colour bar of an RGB Bayer generator seen on a mono sensor |

Cache invalidation (`Cache_Invalidate_Addr` over L1 and L2, which `wait_frame` calls on each
delivered frame):

- 17–31 us in all runs, return code 0, independent of the size: 24–28 us for 256 000 B and
  for 1 024 000 B with the lines not cached; 19–26 us after reading every line.
  [INFERENCE: the cache sync engine handles a range in constant time.]
- It is needed and it works: after reading the first 16 KiB of a buffer and letting the DMA
  run 40 ms into it, a cached read returns exactly the old sum, and a read after the
  invalidation the new one. When that buffer is next delivered, the bytes `wait_frame` hands
  out equal a fresh re-invalidation, in all 4 modes.

Logging pitfall: `esp-println` holds a critical section for each log line. At 115200 baud a
line logged right after a frame kept the DMA interrupt masked past vertical blanking, and the
next frame arrived short (7680 B once). Hence the 2 Mbaud log, and the capture code drops the
frame that completes during set-up and logging before it measures, and logs nothing inside a
measurement window. With that, all frames were whole (2400 per run of four modes).

### 120 fps soak, whole frames from PSRAM

Method:

- **One core, one executor.** The esp-rtos embassy main task on core 0 waits for frames
  (`Csi::wait_frame`) and runs the detector in the same task; nothing else runs. Core 1 stays
  parked. [INFERENCE: the CSI DMA interrupt also lands on core 0, as esp-hal binds handlers on
  the core that creates the driver.]
- Per mode: a live run of 30 s, then synthetic runs of 10 s with 0, 16 and 64 discs, while
  capture keeps running. Each run first drops the frame that completed during set-up and
  logging, then resets the driver's counters. Nothing is logged while a run measures.
- Per frame: processing = `mcycle` around `DefaultDetector::process_frame` (threshold 128,
  256-blob output). The cache invalidation inside `wait_frame` is not in it. The frame is held
  while it is processed and given back by the next `wait_frame`.
- **Live:** the delivered frame is processed.
- **Synthetic:** `Synth::fill` (seed 0x1234_5678, as `bench`) builds a frame of the mode's size
  in PSRAM outside the capture buffers. Before every pass it is written back and invalidated
  (untimed), so every pass reads cold PSRAM while the DMA writes the next capture buffer.
  Frames are waited for and given back exactly as in the live run.
- PASS only if no frame was dropped, overwritten or skipped, no wrong size, no errors, no
  timeout, and max processing < 8333 us.

Results (us; budget 8333 us):

| mode | run | frames | done | dropped | overwritten | seq gaps | wrong size | errors | proc min | mean | max | > budget | fps | blobs | overflow | verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1280x800 | live | 2798 | 3598 | 0 | 799 | 798 | 0 | 0 | 10448 | 10606 | 10841 | 2798 | 93.277 | 1..5 | 0 | FAIL: overwritten |
| 1280x800 | synth 0 | 950 | 1200 | 0 | 249 | 249 | 0 | 0 | 10365 | 10455 | 10620 | 950 | 94.984 | 0..0 | 0 | FAIL: overwritten |
| 1280x800 | synth 16 | 909 | 1200 | 0 | 290 | 289 | 0 | 0 | 10755 | 10847 | 11206 | 909 | 90.908 | 16..16 | 0 | FAIL: overwritten |
| 1280x800 | synth 64 | 849 | 1199 | 0 | 349 | 349 | 0 | 0 | 11503 | 11596 | 11881 | 849 | 84.952 | 64..64 | 0 | FAIL: overwritten |
| 1280x720 crop | live | 3108 | 3597 | 0 | 488 | 488 | 0 | 0 | 9372 | 9535 | 9721 | 3108 | 103.624 | 1..4 | 0 | FAIL: overwritten |
| 1280x720 crop | synth 0 | 1059 | 1199 | 0 | 139 | 139 | 0 | 0 | 9298 | 9371 | 9499 | 1059 | 105.944 | 0..0 | 0 | FAIL: overwritten |
| 1280x720 crop | synth 16 | 1012 | 1200 | 0 | 187 | 187 | 0 | 0 | 9642 | 9736 | 9946 | 1012 | 101.156 | 16..16 | 0 | FAIL: overwritten |
| 1280x720 crop | synth 64 | 938 | 1199 | 0 | 260 | 260 | 0 | 0 | 10355 | 10492 | 10854 | 938 | 93.823 | 64..64 | 0 | FAIL: overwritten |
| 640x480 crop | live | 3598 | 3598 | 0 | 0 | 0 | 0 | 0 | 3303 | 3336 | 3430 | 0 | 119.920 | 0..0 | 0 | PASS |
| 640x480 crop | synth 0 | 1200 | 1200 | 0 | 0 | 0 | 0 | 0 | 3296 | 3345 | 3423 | 0 | 119.920 | 0..0 | 0 | PASS |
| 640x480 crop | synth 16 | 1200 | 1200 | 0 | 0 | 0 | 0 | 0 | 3729 | 3766 | 3912 | 0 | 119.920 | 16..16 | 0 | PASS |
| 640x480 crop | synth 64 | 1200 | 1200 | 0 | 0 | 0 | 0 | 0 | 4411 | 4461 | 4600 | 0 | 119.920 | 64..64 | 0 | PASS |
| 640x400 bin2 | live | 3597 | 3598 | 0 | 0 | 0 | 0 | 0 | 6193 | 7263 | 8507 | 259 | 119.920 | 182..256 | 15341 | FAIL: over budget |
| 640x400 bin2 | synth 0 | 1200 | 1200 | 0 | 0 | 0 | 0 | 0 | 2769 | 2813 | 2910 | 0 | 119.920 | 0..0 | 0 | PASS |
| 640x400 bin2 | synth 16 | 1199 | 1199 | 0 | 0 | 0 | 0 | 0 | 3213 | 3258 | 3393 | 0 | 119.920 | 16..16 | 0 | PASS |
| 640x400 bin2 | synth 64 | 1199 | 1199 | 0 | 0 | 0 | 0 | 0 | 3929 | 3982 | 4112 | 0 | 119.919 | 64..64 | 0 | PASS |

"done" = frames the DMA completed, "frames" = frames handed out and processed, "fps" = the
processed rate. Each mode's 5 s capture-only measurement before the soak: 119.921–119.922 fps,
0 errors. A first run, built against the in-progress strip driver, gave the same verdicts and
figures within about 1 % (live 640x400 aside, which depends on the scene).

Re-run on the final code (default build, `/tmp/poc-p4-eth/final/default.log`): capture
119.921 fps (+25 ppm) with 0 errors in all four modes; the same verdicts: every 1280-wide run
FAIL on overwritten frames (83.0–105.0 fps processed), every 640-wide run PASS. The live
640x400 run passed this time: the scene of that day gave 0 blobs at threshold 128. Mean
processing against the table: 1280x800 synth 0 10 539 us (+0.8 %), synth 64 11 751 (+1.3 %);
1280x720 9446 (+0.8 %), 10 625 (+1.3 %); 640x480 3498 (+4.6 %), 4511 (+1.1 %); 640x400 2955
(+5.0 %), 3975 (−0.2 %).

Findings:

- **640x480 crop and 640x400 binned sustain 120 fps** with every pixel processed from PSRAM
  under live DMA: nothing dropped, overwritten or skipped, no errors, max 4.6 ms with 64
  synthetic blobs (55 % of the budget). 640x480 also passes live (its crop of the scene has
  no pixel >= 128).
- **The 1280-wide modes do not.** Every frame takes longer than the frame period, so the
  driver overwrites the older unclaimed frame and the detector always gets the newest one.
  Nothing is dropped and there are no errors; the processed rate falls to 85–95 fps at
  1280x800 (93.3 live) and 94–106 fps at 1280x720 (103.6 live). Processing takes
  10.4–11.9 ms and 9.3–10.9 ms.
- **DMA contention costs about 4–8 %.** Mean processing with live DMA against the benchmark
  without DMA: 1280x800 0 blobs 10 455 vs 9 826 us (+6.4 %), 64 blobs 11 596 vs 10 740
  (+8.0 %); 1280x720 0 blobs 9 371 vs 8 841 (+6.0 %); 640x480 0 blobs 3 345 vs 3 209
  (+4.2 %); 640x400 0 blobs 2 813 vs 2 675 (+5.2 %).
- **The live 640x400 FAIL is the scene, not the pipeline.** The binned mode is about 1.8x
  brighter; at threshold 128 the indoor scene (lamp, white panels, no IR ring, 7.3 ms
  exposure) gives 182 to 256+ components per frame. The 256-blob output saturates (overflow
  15 341 in total) and processing reaches 8.5 ms; 259 of 3597 frames were over budget, yet
  none was lost (the third buffer absorbs single late frames). [INFERENCE] With markers on a
  dark background (short exposure, IR ring) the live cost is close to the synthetic runs. The
  product still needs a bound on per-frame work for bright scenes (exposure control, an
  ambient mask, or a cap on runs or blobs per frame).
- The interval between handed-out frames is 8338 us on average (119.92 fps) in every 640-wide
  run. In the 1280-wide runs it equals processing plus about 0.1 ms: the loop is CPU-bound.

Caveats:

- One core only; core 1 idle. No Ethernet, PTP or other bus master running.
- Cache invalidation is outside the timed processing: inside `wait_frame` for live frames,
  and the explicit write-back and invalidation for synthetic frames.
- Synthetic frames are dark (4..19) with round discs; real marker scenes may differ.
- Live blob counts depend on the bench scene at the time, so the live runs are not
  repeatable workloads (two runs gave 6..256 and 182..256 blobs at 640x400).
- The frame buffers and the synthetic area are 4 KiB aligned; the detector statics are not
  cache-line aligned (aligning them showed no gain in the benchmark: "Detector benchmark with
  counters").

### Strip capture (64-line strips into internal RAM)

Bring-up of `esp32p4_csi::CsiStrips` by the driver's author, RAW8, in all four modes, with a
scratch test firmware (not in this crate). The sensor sent its random-data test pattern with
the same seed every frame (every frame identical, every row unique), and a whole-frame `Csi`
capture in the same boot served as reference.

Correctness:

- Every delivered strip was checked against the reference rows (every 8th word of every row),
  and the sequence checked (`first_row` contiguous, a new frame at row 0, `is_last` only at
  the frame height): 0 bad rows and 0 sequence errors in every mode and ring size, including
  runs with deliberate stalls.
- A frame assembled from strips equals the whole-frame reference byte for byte (0 differing,
  missing or duplicated rows) in all four modes. The 1280-wide modes were checked at 4x HTS
  (30 fps, same geometry), because copying an 80 KiB strip to PSRAM takes longer than the
  582 us a strip lasts on the wire.
- Scene stills assembled from strips: the ceiling grid runs straight through all 13 strip
  boundaries of 1280x800; the row-to-row difference at strip boundaries equals the one inside
  strips (2.20 vs 2.13 at 1280x800, the same in every mode).

Resynchronisation rule: when the consumer holds strips too long, the DMA reaches a strip that
is still held and halts (a stall: `dma_stalls`, DMA error bits 0x2009, bridge discard 0x4 if
the halt lasts). After any damage the driver drops strips until a frame end, then checks one
whole frame's block sizes (the probation frame) before it delivers again. The first two frames
after `start` are never delivered. Before this rule, stalls produced strips labelled with rows
of the shifted next frame. In this bring-up the driver simply resumed the DMA after a stall,
and a stall cost about 4 frames (the damaged one, the shifted next one, the probation frame).
Since the stall fix the driver restarts the ISP and the bridge before it resumes, the bridge
starts cleanly at the next frame, and a halt costs at most one skipped and one truncated
frame, with the next whole frame 25–33 ms after the strip is returned (14–19 ms at 640x400,
210 fps; see "The stall fault (fixed)").

How long the consumer may hold strips (10 s runs, no processing, every strip held for a fixed
time; driver before the stall fix):

| Mode | Ring | Every strip held (sustained) | Only the first strip of a frame held (single) |
|---|---|---|---|
| 1280x800 | 4 | 608 us (640 stalls) | 1.5 ms (2.0 stalls) |
| 1280x720 | 4 | 658 us (693 stalls) | 1.5 ms (2.0 stalls) |
| 640x480 | 8 | 988 us (1040: 117 fps) | 3.0 ms (4.0 stalls) |
| 640x400 | 8 | 1129 us (1189: 117 fps) | 8 ms or more (all passed) |

- Sustained: a 64-row strip lasts 64 x 9.1 = 582 us on the wire; the consumer may average up
  to the frame period divided by the strips per frame (641 us at 1280x800, 694 at 1280x720,
  1041 at 640x480, 1190 at 640x400) if the ring absorbs the excess within a frame.
- Single: about (N - 1) x 0.5 ms with a ring of N strips.
- With a ring of 3 the limits are lower (1280x800: 576 us sustained, 1.0 ms single).

Driver timing (counters in `StripStats`):

- DMA interrupt handler: 1.4–3.3 us on average, under 4 us in steady state; 10–17 us now and
  then, 59–81 us on the first run after `new` (cold caches).
- Strip cache invalidation in `wait_strip` (L1, 80 or 40 KiB): 1.9–2.3 us on average, 2.6 us
  max; 9.7 us the first time.
- Handler queued the last strip until the task has it: 11 us on average, 25 us max. From the
  end of the frame (the ISP's frame-tail bit) to the last strip queued: 4 us (23 us at
  640x400 binned).

### 120 fps soak, 64-line strips in internal RAM

Method (`SOAK = Soak::Strips`, 2026-11-10, log `/tmp/poc-p4-eth/strip-soak4.log`; the soak
has since been changed to restart the receiver and the stream for every run, see "RAW10
through the strip path"):

- Same board, sensor settings, scene and plan as the frame soak: per mode a live run of 30 s,
  then synthetic runs of 10 s aiming at 0, 16 and 64 blobs per frame, with capture running.
  One core, one executor; the task that waits for strips runs the detector. Nothing is logged
  while a run measures.
- Ring: the same 320 KiB of internal RAM, split per width. 4 strips of 80 KiB for the
  1280-wide modes, 8 strips of 40 KiB for the 640-wide ones. The receiver is rebuilt when the
  width changes.
- Every strip from `wait_strip` goes through `begin_frame` (at row 0), `push_rows`, and
  `finish` (at the last strip), then goes back to the driver with the next `wait_strip`.
- Each run starts at a frame boundary: strips are drained until a last strip that the
  interrupt handler queued less than 200 us earlier, then the driver's counters are reset.
- **Synthetic:** the detector reads a 64-row strip held in internal RAM instead of the DMA
  strip. It is four 16-row bands, each filled by `Synth::fill` (background 4..19, discs of
  radius 3..5 peaking at 250, seed 0x1234_5678 + band) with its own disc count, so every strip
  holds whole discs only. The short last strip reads the first bands. The counts are searched
  for the total closest to the target: the blobs actually fed per frame are 0/13/64 at
  1280x800, 0/12/66 at 1280x720, 0/16/64 at 640x480 and 0/18/64 at 640x400. The strip is
  written back and invalidated before every `push_rows`, so it is read from RAM. DMA strips
  are still waited for and given back at the sensor's rate.
- Per frame: processing = cycles inside the detector calls of the frame. Latency = first
  strip handed out until `finish` returns. Frame end to centroids = the last strip queued by
  the interrupt handler (`Strip::queued_at_cycles`) until `finish` returns.
- Per strip: hold = handed out until given back (the longest, with its first row).
  Queued to consumed = queued by the handler until the detector has consumed it.
- Driver: interrupt handler and per-strip invalidation times from `StripStats`.
- PASS only with no DMA stall, bridge overrun, truncated or skipped frame, no error, and max
  processing < 8333 us.

Results (us; budget 8333 us; errors = DMA + host + bridge + length + write-back missing):

| mode | run | blobs/frame fed | frames complete | truncated | skipped | strips delivered | discarded | stalls | overruns | errors | proc min | mean | max | > budget | latency min | mean | max | hold max (row) | queued->consumed mean/max | frame end->centroids min/mean/max | ISR mean/max | invalidate mean/max | fps | blobs | overflow | verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1280x800 | live | scene | 3597 | 0 | 0 | 46768 | 0 | 0 | 0 | 0 | 4346 | 4351 | 4889 | 0 | 7021 | 7029 | 7034 | 688 (0) | 352/696 | 252/252/286 | 3.1/9.7 | 2.2/2.4 | 119.921 | 3..4 | 0 | PASS |
| 1280x800 | synth 0 | 0 | 1199 | 0 | 0 | 15588 | 0 | 0 | 0 | 0 | 4323 | 4323 | 4329 | 0 | 7034 | 7034 | 7040 | 360 (704) | 353/371 | 258/258/264 | 3.2/3.4 | 2.2/2.3 | 119.921 | 0..0 | 0 | PASS |
| 1280x800 | synth 16 | 13 | 1199 | 0 | 0 | 15588 | 0 | 0 | 0 | 0 | 4457 | 4457 | 4463 | 0 | 7055 | 7055 | 7061 | 370 (704) | 364/381 | 279/279/285 | 3.2/3.4 | 2.3/2.4 | 119.921 | 13..13 | 0 | PASS |
| 1280x800 | synth 64 | 64 | 1199 | 0 | 0 | 15588 | 0 | 0 | 0 | 0 | 5184 | 5185 | 5199 | 0 | 7154 | 7155 | 7161 | 434 (0) | 425/445 | 378/379/385 | 3.2/3.4 | 2.3/2.4 | 119.921 | 64..64 | 0 | PASS |
| 1280x720 crop | live | scene | 3597 | 0 | 0 | 43170 | 0 | 0 | 0 | 0 | 3915 | 3920 | 3929 | 0 | 6359 | 6360 | 6366 | 383 (128) | 356/394 | 312/312/318 | 3.1/3.4 | 2.2/2.4 | 119.921 | 3..4 | 0 | PASS |
| 1280x720 crop | synth 0 | 0 | 1199 | 0 | 0 | 14389 | 0 | 0 | 0 | 0 | 3892 | 3892 | 3898 | 0 | 6364 | 6365 | 6371 | 360 (640) | 358/371 | 317/317/323 | 3.1/3.4 | 2.2/2.3 | 119.921 | 0..0 | 0 | PASS |
| 1280x720 crop | synth 16 | 12 | 1199 | 0 | 0 | 14389 | 0 | 0 | 0 | 0 | 4015 | 4016 | 4022 | 0 | 6385 | 6386 | 6392 | 370 (640) | 369/381 | 338/338/344 | 3.1/3.4 | 2.3/2.4 | 119.921 | 12..12 | 0 | PASS |
| 1280x720 crop | synth 64 | 66 | 1199 | 0 | 0 | 14389 | 0 | 0 | 0 | 0 | 4833 | 4834 | 4840 | 0 | 6453 | 6453 | 6459 | 446 (640) | 444/457 | 405/406/411 | 3.2/3.5 | 2.3/2.4 | 119.921 | 66..66 | 0 | PASS |
| 640x480 crop | live | scene | 3597 | 0 | 0 | 28779 | 0 | 0 | 0 | 0 | 1585 | 1585 | 1593 | 0 | 3982 | 3982 | 3983 | 224 (0) | 210/235 | 118/118/119 | 3.2/3.5 | 2.2/2.3 | 119.921 | 0..0 | 0 | PASS |
| 640x480 crop | synth 0 | 0 | 1199 | 0 | 0 | 9593 | 0 | 0 | 0 | 0 | 1585 | 1585 | 1591 | 0 | 3984 | 3984 | 3985 | 220 (384) | 213/231 | 120/120/121 | 3.2/3.5 | 2.2/2.3 | 119.921 | 0..0 | 0 | PASS |
| 640x480 crop | synth 16 | 16 | 1199 | 0 | 0 | 9593 | 0 | 0 | 0 | 0 | 1792 | 1793 | 1816 | 0 | 4011 | 4011 | 4011 | 268 (0) | 239/279 | 147/147/147 | 3.2/3.5 | 2.3/2.4 | 119.921 | 16..16 | 0 | PASS |
| 640x480 crop | synth 64 | 64 | 1199 | 0 | 0 | 9593 | 0 | 0 | 0 | 0 | 2483 | 2483 | 2490 | 0 | 4144 | 4144 | 4145 | 334 (0) | 331/344 | 280/280/281 | 3.2/3.4 | 2.3/2.4 | 119.921 | 64..64 | 0 | PASS |
| 640x400 bin2 | live | scene | 3597 | 0 | 0 | 25181 | 0 | 0 | 0 | 0 | 4010 | 5108 | 6453 | 0 | 4505 | 5537 | 6658 | 1632 (64) | 2018/3950 | 1351/2383/3503 | 2.2/3.8 | 2.1/7.9 | 119.928 | 173..256 | 2733 | PASS |
| 640x400 bin2 | synth 0 | 0 | 1199 | 0 | 0 | 8394 | 0 | 0 | 0 | 0 | 1327 | 1327 | 1327 | 0 | 3283 | 3283 | 3284 | 219 (0) | 213/230 | 128/128/129 | 3.0/3.3 | 2.1/2.2 | 119.921 | 0..0 | 0 | PASS |
| 640x400 bin2 | synth 16 | 18 | 1199 | 0 | 0 | 8394 | 0 | 0 | 0 | 0 | 1593 | 1594 | 1599 | 0 | 3329 | 3329 | 3335 | 269 (320) | 257/280 | 174/174/180 | 3.1/3.4 | 2.2/2.4 | 119.921 | 18..18 | 0 | PASS |
| 640x400 bin2 | synth 64 | 64 | 1199 | 0 | 0 | 8394 | 0 | 0 | 0 | 0 | 2285 | 2286 | 2292 | 0 | 3491 | 3492 | 3498 | 375 (320) | 372/387 | 336/337/343 | 3.1/3.4 | 2.2/2.4 | 119.921 | 64..64 | 0 | PASS |

"fps" = frames finished by the detector per second. Frames complete = 3597 in 30 s and
1199 in 10 s: every frame the sensor sent.

Strips against whole frames from PSRAM (processing per frame mean/max in us; the frame
soak's synthetic frames carry exactly 16 and 64 discs):

| Mode | Run | Whole frame, PSRAM | 64-line strips, internal RAM | Ratio (mean) |
|---|---|---|---|---|
| 1280x800 | live | 10606 / 10841 (93.3 fps processed) | 4351 / 4889 (119.9 fps) | 2.44 |
| 1280x800 | synth 0 | 10455 / 10620 | 4323 / 4329 | 2.42 |
| 1280x800 | synth 64 | 11596 / 11881 | 5185 / 5199 | 2.24 |
| 1280x720 | live | 9535 / 9721 (103.6 fps processed) | 3920 / 3929 (119.9 fps) | 2.43 |
| 1280x720 | synth 0 | 9371 / 9499 | 3892 / 3898 | 2.41 |
| 1280x720 | synth 64 | 10492 / 10854 | 4834 / 4840 (66 blobs) | 2.17 |
| 640x480 | synth 0 | 3345 / 3423 | 1585 / 1591 | 2.11 |
| 640x480 | synth 64 | 4461 / 4600 | 2483 / 2490 | 1.80 |
| 640x400 | live | 7263 / 8507 (FAIL: over budget) | 5108 / 6453 (PASS) | 1.42 |
| 640x400 | synth 0 | 2813 / 2910 | 1327 / 1327 | 2.12 |
| 640x400 | synth 64 | 3982 / 4112 | 2286 / 2292 | 1.74 |

Latency, from the end of the frame to the centroids: with strips 0.25–0.41 ms at 1280 wide
and 0.12–0.34 ms at 640 wide (synthetic and dark live frames), because only the last strip
is left to process when the frame ends. With whole frames the detector starts only when the
frame is complete, so the centroids come at least one processing time later: 2.8–11.9 ms
[INFERENCE: processing time plus the 17–31 us invalidation; the frame soak did not time the
frame end].

Findings:

- **Every mode sustains 120 fps with every pixel processed from strips.** All 16 runs passed:
  every frame the sensor sent was complete and finished by the detector, with no stall,
  overrun, discarded strip or error. Processing takes at most 5.2 ms per frame at 1280x800
  with 64 blobs (62 % of the frame period) and 4.8 ms at 1280x720.
- **Strips are 2.2–2.4 times faster than whole frames from PSRAM** at 1280 wide, 1.7–2.1
  times at 640 wide, about the figure the benchmark predicted without DMA running (strip path
  4.2–5.2 ms at 1280x800). Reading internal RAM instead of PSRAM is the gain.
- **Per-strip load against the driver's limits:** about 330–450 us per 1280-wide strip, under
  the 608 us (1280x800) and 658 us (1280x720) a ring of 4 sustains. The longest hold was
  688 us, on the first strip of a live 1280x800 frame [INFERENCE: the lamp at the top of the
  scene], within the 1.5 ms one strip may be held.
- **The 640-wide modes need the ring of 8.** A first run with 4 strips of 40 KiB at 640 wide
  (half of each 80 KiB buffer used) passed every synthetic run, but the live 640x400 run
  failed: 1864 DMA stalls in 30 s (DMA bits 0x2009, bridge discard 0x4), 926 truncated and
  2689 skipped frames, only 12 complete. The scene gave 227..256 blobs per frame, and the
  strip at row 64 was held up to 1672 us, past the about 1.5 ms a ring of 4 allows for one
  strip. Each stall cost about 4 frames, and they kept coming. With the same 320 KiB split
  into 8 strips the same run passed: hold max 1632 us (row 64), no stall.
- **The bright live 640x400 scene still costs the most.** 173..256 blobs per frame, detector
  overflow 2733 in total, processing up to 6.5 ms, centroids up to 3.5 ms after the frame end.
  Within budget with strips, over it with whole frames from PSRAM (max 8.5 ms).
- **The driver's share is small:** the DMA interrupt handler takes 3.0–3.2 us on average (2.2
  in the live 640x400 run), 3.3–3.8 us max (9.7 us once, in the first run); the strip
  invalidation 2.1–2.3 us on average, 2.4 us max (7.9 us once). At 13 strips per frame that
  is about 70 us per frame.

Caveats:

- One core only; core 1 idle. No Ethernet, PTP or other bus master running.
- Synthetic strips are dark (4..19) with round discs, and the same strip is fed at every row
  position; the exact targets are not always reachable (13 instead of 16 at 1280x800, 12 at
  1280x720, 18 at 640x400, 66 instead of 64 at 1280x720).
- The live scene is the bench scene at the time (no IR ring, exposure 7.3 ms, gain 2x); the
  live runs are not repeatable workloads.
- Frame end to centroids is measured from the moment the handler queued the last strip; the
  driver's bring-up measured that at 4 us after the ISP's frame-tail bit (23 us at 640x400).
- The ring takes 320 KiB of internal RAM; with the 128 KiB buffer of the boot read bench this
  build leaves 62.7 KiB for the stack (about 61 KiB with the RAW10 strip code: `_bss_end`
  0x4FF9_EBC0, from the strips task).

### All 12 modes: the Inno-Maker mode list and RAW10

Modes 4..=11 are the Inno-Maker CAM-MIPIOV9281 V2 mode list. They were brought up in three
tasks on 2026-11-10 with hand-built register tables (logs in `/tmp/poc-p4-eth/raw10/`,
`/tmp/poc-p4-eth/mode8/` and `/tmp/poc-p4-eth/strips10/`), verified on the merged code
(commit `6963e63`, logs in `/tmp/poc-p4-eth/final/`), and then moved to the `ov9281` crate:
each mode is now a request (`src/modes.rs`: format, size, readout, the nominal rate with
±0.5 Hz) and the crate's solver chooses the setting under the measured limits. The tables
below are from the builder's register lists (commit `e18819d` on the crate's limits of
`3ddd024`), log `/tmp/poc-p4-eth/migrate/stills.log`; the sections after them are from the
hand-built tables, whose settings differ as the first table says. The new modes have
capture-only verification: 5 s of capture, stills, and the RAW10 strip runs below; no frame
soak at the new rates.

#### Capture, all 12 modes

`MODE_STILLS_PLAN`, one boot, modes in the order 0, 9, 1, 10, 6, 11, 4, 5, 7, 8, 2, 3;
exposure 800 lines (clamped to VTS − 25 in modes 6, 7, 8), gain 2x; 5 s first-to-last per
mode. Every mode: 0 wrong size, 0 dropped, 0 overwritten, 0 sequence gaps, DMA, host and
bridge errors all 0; the worst interval between frames is the frame period plus at most 8 us.

| Mode (request) | Format | Solver: SCLK, HTS, VTS, PSV, VFIFO | Before (hand-built table) | Predicted fps | Measured fps | ppm | Frames in 5 s | Bytes/frame |
|---|---|---|---|---|---|---|---|---|
| 0: 1280x800, 120 Hz | RAW8 | 80 MHz, 728, 914, on, 0x50 | VTS 915, VFIFO 0x30 | 120.049 | 120.052 | +30 | 601 | 1 024 000 |
| 1: 1280x720 crop, 120 Hz | RAW8 | 80 MHz, 728, 914, on, 0x50 | VTS 915, 0x30 | 120.049 | 120.052 | +29 | 601 | 921 600 |
| 2: 640x480 crop, 120 Hz | RAW8 | 80 MHz, 728, 914, on, 0x28 | VTS 915, 0x30 | 120.049 | 120.052 | +27 | 601 | 307 200 |
| 3: 640x400 binned, 120 Hz | RAW8 | 80 MHz, 728, 914, on, 0x28 | VTS 915, 0x30 | 120.049 | 120.052 | +28 | 601 | 256 000 |
| 4: 1280x800, 144 Hz | RAW8 | 96 MHz, 728, 914, on, 0x50 | VTS 915, 0x30 | 144.059 | 144.065 | +47 | 722 | 1 024 000 |
| 5: 1280x720 crop, 144 Hz | RAW8 | 96 MHz, 728, 914, on, 0x50 | VTS 915, 0x30 | 144.059 | 144.065 | +45 | 722 | 921 600 |
| 6: 640x400 binned, 210 Hz | RAW8 | 80 MHz, 728, 522, on, 0x28 | VTS 523, 0x30 | 209.964 | 209.969 | +28 | 1051 | 256 000 |
| 7: 640x400 binned, 253 Hz | RAW8 | **96 MHz**, 728, 520, on, 0x28 | 80 MHz, VTS 434, 0x30 | 252.923 | 252.930 | +28 | 1265 | 256 000 |
| 8: 320x200 binned, 453 Hz | RAW8 | 80 MHz, 728, 241, off, 0x14 | VFIFO 0x04 | 453.386 | 453.398 | +27 | 2268 | 64 000 |
| 9: 1280x800, 120 Hz | RAW10 | 80 MHz, 728, 914, on, 0x50 | VTS 915, 0x04 | 120.049 | 120.052 | +29 | 601 | 1 280 000 |
| 10: 1280x720 crop, 120 Hz | RAW10 | 80 MHz, 728, 914, on, 0x50 | VTS 915, 0x04 | 120.049 | 120.052 | +29 | 601 | 1 152 000 |
| 11: 640x400 binned, 210 Hz | RAW10 | 80 MHz, 728, 522, on, 0x28 | VTS 523, 0x04 | 209.964 | 209.970 | +30 | 1051 | 320 000 |

- **What changed against the hand-built tables:** VFIFO read start (0x4601) in every mode
  (now width / 16); VTS in every mode but 8, by one line (915 → 914, 523 → 522), because the
  solver takes the VTS nearest the requested 120 or 210 Hz (119.918 → 120.049 Hz, 209.563 →
  209.964 Hz); and mode 7 entirely. Its old VTS 434 (h + 34) is below the measured minimum
  plus margin (h + 35), so the crate's 80 MHz maximum for the window is 251.824 Hz and a
  253 Hz request resolves at 96 MHz, VTS 520, PSV on (h + 120). The old table ran 80 MHz, VTS
  434 with PSV on, in the narrow good window below the PSV bands ("Sensor driver crate").
  The 80 MHz alternative, 250 Hz, is request `rq07` in the request verification below.
- The readback in the log shows every mode's window, output size, binning, HTS, VTS,
  exposure, gain, VFIFO and PSV as written; 0x030D (the clock) reads 0x60 in modes 4, 5, 7.
- Stills, checked against the 1280x800 RAW8 still of the same boot (`ana.py` in
  `/tmp/poc-p4-eth/migrate/`, numpy): the bar pattern (`bars`) in every mode has every even row
  equal to row 0 and every odd row equal to row 1, values 0 and 255 (RAW10 0 and 1023), CRCs as
  in the earlier boots (1280x800 4e25f7bb, 1280x720 891881ed, 640x400 d2a5c43f, RAW10
  22ce4315, 58da8cf8, e716caa2); no scene still has a flat row; every crop is at its place
  (1280x720 at (0, 40), 640x480 at (320, 160), 320x200 at (320, 200), binned against a 2x2
  mean, within 2 px of drift, correlation 0.969–0.989); 320x200 sits at (160, 100) in the
  640x400 binned still (0.982); RAW10 / 4 against its RAW8 partner: correlation 0.977, 0.978
  and 0.989, mean +0.27 to +0.40 of an 8-bit step.

The detector check on one captured frame per mode (dark scene: 0 blobs at threshold 128 for
RAW8 and 512 for RAW10) gives, against the frame period: mode 0 10 778 us (8329), 1 9709
(8329), 2 3561 (8329), 3 3028 (8329), 4 10 925 (6941), 5 9759 (6941), 6 3099 (4762), 7 3093
(3953), 8 1115 (2205), 9 13 843 (8329), 10 12 369 (8329), 11 3888 (4762). Whole-frame
processing from PSRAM is slower than a frame in modes 0, 1, 4, 5, 9 and 10. The synthetic
RAW10 frames with 16 discs gave 16 blobs in modes 9, 10 and 11.

The hand-built tables' run on the same plan (`/tmp/poc-p4-eth/final/stills.log`, commit
`6963e63`) measured 119.921 fps (+25 ppm) at VTS 915, 143.907 (+41) for modes 4 and 5,
209.569 (+28) at VTS 523, 252.409 (+27) for mode 7 at VTS 434 and 453.399 (+28) for mode 8,
with the same clean link counters.

#### Frame period

A frame lasts **`(VTS − 1) × HTS + 1730` SCLK cycles**, not `VTS × HTS` (`Mode::frame_cycles()`
in the `ov9281` crate; the capture log prints both the measured cycles and this prediction,
and the ppm against it).

- Evidence: 21 runs of 10 s (`/tmp/poc-p4-eth/raw10/TMP_TIMING_PLAN.log`, `TMP_HTS_PLAN.log`)
  over HTS 728..=2912, VTS 434..=1830, SCLK 80 and 96 MHz, RAW8 and RAW10, crop and binned,
  exposure 100 and 800 lines fit it within 1.4 cycles, with 1730.2 cycles and the sensor clock
  28 ppm fast against the P4's timer. A model `HTS × VTS + constant` leaves up to 845 cycles.
  Exposure has no effect. At HTS 1820 and above the frame is shorter than `HTS × VTS`. The
  mode 8 task fitted its own runs at VTS 222..=915 to 9.09976 us × VTS + 12.51 us, the same
  model.
- The 12 modes above measure +27 to +47 ppm against it (hand-built tables: +25 to +41); the
  two 144 fps modes are the high end in both runs. The old `VTS × HTS` prediction was
  120.098 fps at VTS 915 and 80 MHz (measured −1474 ppm from it), 144.118 at 96 MHz (−1464),
  210.114 at VTS 523 (−2594), 253.203 at VTS 434 (−3136) and 455.976 at VTS 241 (−5652): the
  shorter the frame, the larger the error. That is why the first capture run saw the 640x400
  modes 0.26–0.31 % under prediction and the others 0.15 %.
- That one line per frame lasts 1730 cycles instead of HTS is [INFERENCE]: only whole frame
  periods were measured. Which of the sensor's 24 MHz oscillator and the P4's 40 MHz crystal
  is off by the 25–47 ppm is not known. The 144 fps offset is not a 96 MHz property: mode 7
  at 96 MHz measures +28 ppm.
- The first-to-last rate (periods / time from the first to the last frame) is the
  measurement. An earlier "frames over the window" figure counted one frame more than the
  periods spanned (601 in 5007 ms = 120.03) and is no longer printed.

#### RAW10 byte layout

RAW10 in memory is CSI-2 packed, as the detector, `Synth::fill_raw10` and `tools/still.py`
assume: per 4 pixels, bytes 0..3 hold bits 9..2 of P0..P3 and byte 4 holds
`P3[1:0] P2[1:0] P1[1:0] P0[1:0]` (P0 in bits 1..0); no byte swap, no row padding. Evidence
(`/tmp/poc-p4-eth/raw10/final_stills.log`, `MODE_STILLS_PLAN.log`, `stills.log`, scripts
`layout_pattern.py`, `p13.py`, `fields.py` there):

| Part | Evidence |
|---|---|
| Byte 4 holds the low bits; byte 0 = P0, byte 2 = P2; their field order | 0x80 bars in RAW8 and RAW10 of the same boot at 1280x800, 1280x720 and 640x400: of 2880 candidate layouts (byte swaps, low-byte position, orders of the top bytes and of the fields) only this one gives the RAW8 value in the top bits at every pixel; RAW8 0 and 255 become exactly 0 and 1023; byte 4 takes only 0x00, 0x33, 0xCC, 0xFF |
| Byte 1 = P1, byte 3 = P3 (the bars cannot tell) | Scene pixels where RAW8 P1 and P3 differ by 20 or more: mean \|top byte − RAW8\| 5.6–11.1 as assumed, 16.7–38.9 with them swapped |
| Field j of byte 4 = pixel j | Correlation of each 2-bit field with each pixel's residual below one RAW8 step: diagonal +0.43 (dark scene) and +0.07–0.10 (brighter), every other pair at most 0.06 |
| Same data as the random test pattern | The strip check's reference frames: RAW10 row 0 starts `3c 71 cb 35 40 7e 86 57 0b eb`, RAW8 row 0 `3c 71 cb 35 7e 86 57 0b` (`/tmp/poc-p4-eth/final/check.log`) |
| `tools/still.py` | Its 16-bit PGMs equal an independent unpack in all 10 RAW10 stills checked (`pgmcheck.py`) |

RAW10 / 4 against RAW8 in the bright-scene stills below: correlation 0.978, 0.979 and 0.991
(as high as between two RAW8 frames of the same window, 0.977), mean difference +0.37, +0.38
and +0.45 of an 8-bit step [INFERENCE: the RAW8 output drops the two low bits, which gives
+0.375 on average].

#### Stills, bright scene

Same log and boot as the capture table (`/tmp/poc-p4-eth/final/stills.log`; PNGs in
`/tmp/poc-p4-eth/final/stills/`, contrast-stretched copies in `stills_stretch/`; analysis
`/tmp/poc-p4-eth/final/ana.py` and `m3.py`, using the numpy helpers of the raw10 folder). The
scene: the underside of a desk with a cable, legs and a lit floor, gain 2x; the 1280x800
RAW8 still has mean 10.1 and maximum 67 of 255 (nothing saturated), so the picture is usable
but still dim. Every still was decoded with a good CRC and looked at: straight edges stay
straight, no torn, wrapped or flat rows, in every mode.

Bar pattern (0x5E00 = 0x80), every mode: every even row equals row 0 and every odd row row 1
(no shear, no lost byte), values 0 and 255 (RAW10: 0 and 1023). Modes of one output size give
byte-identical patterns: 1280x800 RAW8 CRC 4e25f7bb (modes 0 and 4), 1280x720 891881ed (1 and
5), 640x400 d2a5c43f (3, 6 and 7), the same CRCs as in the earlier boots; RAW10 22ce4315,
58da8cf8 and e716caa2. The generator scales its 8 bars to the output width, so the pattern
says nothing about which sensor area is read.

Scene, against the 1280x800 RAW8 still of the same boot (mode 0), or against the still of the
same window taken just before. Offset = best integer placement by correlation, in full-
resolution pixels; binned stills are compared with a 2x2 mean. Brightness = (mean − 4) / (mean
of the reference − 4), 4 being the black level; the expected ratio is the ratio of the
exposure times:

| Mode | Against | Best offset (expected, where different) | Correlation | Brightness ratio (exposure ratio) |
|---|---|---|---|---|
| 9: 1280x800 RAW10 | mode 0 | (0, 0) | 0.978 | 1.060 (1) |
| 1: 1280x720 crop | mode 0 | (0, 40) | 0.977 | 1.003 (1) |
| 10: 1280x720 RAW10 | mode 1 | (0, 0) | 0.979 | 1.063 (1) |
| 6: 640x400 binned, 210 fps | mode 0, 2x2 mean | (0, −1) (0, 0) | 0.992 | 1.253 (0.622): binning 2.01x |
| 11: 640x400 RAW10 | mode 6 | (0, 0) | 0.991 | 1.059 (1) |
| 4: 1280x800, 96 MHz | mode 0 | (0, −1) (0, 0) | 0.973 | 0.822 (0.833) |
| 5: 1280x720, 96 MHz | mode 1 | (0, −1) (0, 0) | 0.974 | 0.824 (0.833) |
| 7: 640x400 binned, 253 fps | mode 6 | (0, 0) | 0.988 | 0.820 (0.821) |
| 8: 320x200 binned crop | mode 6 | (160, 100) | 0.983 | 0.415 (0.434) |
| 8 | mode 7 | (160, 100) | 0.982 | 0.506 (0.528) |
| 2: 640x480 crop | mode 0 | (321, 159) (320, 160) | 0.979 | 0.998 (1) |
| 3: 640x400 binned, 120 fps | mode 2, 2x2 mean | (160, 80) | 0.994 | 2.267 (1) |

- **Every window is where the register tables put it**, within one pixel; the 1-px best
  offsets in the table are motion. The camera or the scene moves between stills (cause not
  looked for): in this boot the sub-pixel peak against the first still moves (in y steadily)
  from (+0.3, +0.2) for mode 9, seconds after mode 0, to (+0.6 to +0.8, −1.2 to −1.3) for
  modes 7, 8 and 2 and (+0.5, −1.5) for mode 3, three minutes later, whatever the window,
  while stills taken close together agree within about 0.2 px: mode 10 against 1 (0.04,
  −0.08), 11 against 6 (0.00, −0.02), 8 against 6, 7
  and 3 (160.15, 99.82), (160.03, 100.00), (159.98, 100.10). In the repeat run below, mode 0
  against itself 90 s later is off by (0.77, −0.11), and consecutive stills by up to 1.3 px.
- Mode 8 covers active columns 320..=959 and rows 200..=599, the centre half of the field of
  view; mode 2 columns 320..=959, rows 160..=639; mode 1 rows 40..=759. A binned pixel lines
  up with the 2x2 mean of the full-resolution pixels it covers (no half-pixel offset beyond
  the motion between stills).
- **Brightness follows the exposure time** within 2 % in the unbinned modes and in the binned
  modes at 210, 253 and 453 fps, and binning gives 2.01x at equal exposure (modes 6, 7 and 8
  among themselves and against mode 0). Mode 3 read 2.27x mode 2, taken five seconds earlier
  at the same exposure. The repeat run gives 2.03x for mode 3 against mode 0 and 0.621 for
  mode 6 against mode 3 (exposure ratio 0.622), and there mode 0 itself read 1.23x brighter
  after 90 s: the bench light changes now and then [INFERENCE], so a single brightness ratio
  between two stills can be off by that much.
- RAW10 modes read 6 % above their RAW8 partner in this brightness measure (+0.37 to +0.45
  of an 8-bit step, see the layout section).

An earlier run in the dim evening scene (`/tmp/poc-p4-eth/raw10/final_stills.log`, gain
15.5x, modes 0, 9, 1, 10, 6, 11, 4, 5, 7) gave the same offsets at integer precision, with
correlations 0.95 (unbinned, 80 MHz), 0.98 (binned) and 0.84 (96 MHz, with the stripes
below). Mode 8 was checked in its own task the same way (`/tmp/poc-p4-eth/mode8/final-scene.log`:
(160, 100), correlation 0.900, at the noise floor of two frames, 0.895).

#### Mode 8 (320x200): cause of "no frames" and fix

Two values of the common register block break the 320-pixel-wide binned crop. Mode 8 sets
both back to the datasheet defaults, which are also what Linux `ov9282.c` leaves: 0x4601 =
0x04 and 0x4F00 = 0x00 (`REGS_320X200_RAW8_453`). Logs and stills in `/tmp/poc-p4-eth/mode8/`
(`e1.log` .. `e12.log`, `final-scene.log`, `final-pattern.log`).

| Experiment (one setting changed) | Result |
|---|---|
| Mode 8 window at VTS 242, 300, 523, 915; whole-array binned crop to 320x200; 320x400 binned; 320x200 unbinned | no frame: one host frame start/end pairing error per frame; 64 000 B reach the bridge (61 440 B written, 2560 B left in the bridge), the frame never ends |
| 640x200 binned and unbinned (VTS 915) | 119.92 fps, clean |
| 0x4601 = 0x04, 0x20, 0x28 (320x200, VTS 915) | clean |
| 0x4601 = 0x29, 0x2F, 0x30 | no frame |
| 0x4601 = 0x04, VTS 241, 0x4F00 = 0x0C (common) | clean link, but rows 0..=187 flat at the black level (4; 16 with the black-level calibration off); VTS 260: all rows flat |
| same, exposure 50..=216 lines, window at the array top, calibration off, vertical-blanking power-down off | still flat |
| 0x4F00 = 0x00 or 0x08 | whole picture at VTS 241 and 260 |

- **VFIFO read start (0x4601)** counts 8-byte units and must not be longer than one line: the
  limit is exactly 0x28 x 8 B = 320 B, the line of this mode. The common block's 0x30 (384 B)
  is Espressif's RAW8 value; every other mode's line is 640 B or more. [INFERENCE: with a
  read start past the line end the last line waits in the VFIFO and arrives after the frame
  end.] The sensor characterisation later found the unit is 8 pixels (the same as 8 bytes in
  RAW8) and a lower limit as well; see "Sensor driver crate".
- **PSV mode (0x4F00 = 0x0C, power saving)** blanks rows when the frame is short. Espressif's
  table sets it, Linux never writes it; the datasheet does not describe it. [INFERENCE: the
  flat, noise-free rows point to the analog chain being off.]
- Lowest VTS with 0x4F00 = 0x00 at the exposure maximum (`e12.log`): 232, 234, 236, 238, 240
  and 241 give a correct picture; 222 and 230 whole frames with a nearly black picture (mean
  0.7–1.5, also at 100 lines); with 0x4F00 still at 0x0C, VTS 214..=220 stop the frames. VTS
  241 leaves 9 lines of margin. VTS 231, 233, 235, 237 and 239, and 0x4F00 = 0x00 below 222,
  were not tried here. The sensor characterisation settled it: last good VTS 231 (h + 31) with
  PSV off, and VTS 232 measured 470.884 fps (see "Sensor driver crate").

#### Mode 5 (1280x720 at 144 fps): VTS limit, PSV, 96 MHz

The first mode 5 (80 MHz, VTS 763) gave a wrong picture in every run (mean 0.6–2.0, bright
areas lowest, at every exposure from 100 to 738 lines), while its frames were whole and on
time (143.768–143.771 fps; `/tmp/poc-p4-eth/raw10/TMP_MODE5_PLAN.log`, `TMP_MODE5B_PLAN.log`,
`TMP_MODE5C_PLAN.log`).

| 1280x720 crop, 80 MHz | 0x4F00 | fps | Picture |
|---|---|---|---|
| VTS 763 | 0x0C (common) | 143.77 | wrong (dark, bright areas lowest); also with the window at rows 0..=735 |
| VTS 780..=810 | 0x0C | | flat at the black level |
| VTS 814, 816, 818, 820 | 0x0C | | normal |
| VTS 763 | 0x00 | 143.769 | wrong: mean 0.61, correlation −0.31 against mode 1 |
| VTS 780 | 0x00 | 140.641 | normal: shift (0, 0), correlation 0.934, brightness 1.001 against mode 1 |
| VTS 800 | 0x00 | 137.131 | normal: shift (1, 0), correlation 0.935, brightness 1.008 |
| VTS 780 | 0x0C (control, same boot) | 140.641 | 717 of 720 rows flat at 4 |

(Last four rows: `/tmp/poc-p4-eth/mode8/psv720.log`, exposure 700 lines, gain 15.5x.)

- So the flat frames at VTS 780..=810 are PSV, as in mode 8, and the dark, inverted-looking
  picture at the shortest VTS (763 here, 222..=230 in mode 8) is a second limit that PSV off
  does not fix. Its cause is unknown; the sensor characterisation later bisected it to a last
  good VTS of 771 (h + 51) for this window, the same at 96 MHz (see "Sensor driver crate").
- **Decision:** PSV stays on in the common block (only mode 8 turns it off), and mode 5 runs
  the 1280x720 window at 96 MHz with VTS 915, as mode 4: 143.907 fps, normal picture. The
  80 MHz alternative, PSV off at VTS 780 (140.6 fps), is a finding only.
- PSV off on modes that already worked (modes 1 and 3, VTS 915, same boot): frame rate,
  picture (correlation 0.933 / 0.985, brightness 0.985 / 1.001), black level (1st/10th
  percentile 3/5 and 4–5/7) and noise unchanged. Its power saving was not measured.

#### What the 96 MHz clock costs in the picture

Modes 4 and 5 run SCLK at 96 MHz, above the datasheet's 80 MHz. At gain 15.5x (dim scene,
`/tmp/poc-p4-eth/raw10/final_stills.log`) they showed vertical column stripes (column noise
0.834 against 0.101 in mode 0; std of the column means around their 9-column running median,
8-bit scale) and a raised black floor (the reference's darkest 5 % of pixels read 14.2 against
3.5). At gain 2x, bright scene, against the 80 MHz mode of the same window taken just before
(the repeat run, `/tmp/poc-p4-eth/final/clock96.log`, is a separate boot with modes 0, 4, 1,
5, 3, 6, 0; the same capture results as in the table above, modes 4 and 5 at 143.906 fps,
+34 ppm):

| Still | Column noise (80 MHz partner) | Darkest 5 % of the partner (partner) | Fit against the partner (exposure ratio) |
|---|---|---|---|
| Mode 4, `final/stills.log` | 0.028 (0.028) | 4.78 (4.73) | 0.815 x + 0.79 (0.833) |
| Mode 5, `final/stills.log` | 0.030 (0.029) | 4.39 (3.98) | 0.818 x + 0.77 (0.833) |
| Mode 4, `final/clock96.log` | 0.029 (0.029) | 4.82 (4.76) | 0.824 x + 0.76 (0.833) |
| Mode 5, `final/clock96.log` | 0.030 (0.030) | 4.77 (4.76) | 0.824 x + 0.73 (0.833) |

So at a normal gain the 96 MHz clock costs nothing measurable: no stripes, the black floor
within 0.4 of an 8-bit step, the slope at 98–99 % of the exposure ratio. The stripes and the
raised floor are a high-gain effect [INFERENCE: ADC or black-level timing at the
over-specified clock]. Whether they also appear at gains between 2x and 15.5x was not
measured.

#### RAW10 through the strip path (modes 9, 10, 11)

Strips against whole frames (`STRIP_CHECK_PLAN`, log `/tmp/poc-p4-eth/final/check.log`, on the
merged code): the sensor's random test pattern (0x5E00 = 0x81, and 0x5E01 = 0x51, "same seed
enable": without it every row changes from frame to frame; both read back), 8 whole frames
per mode as the reference (all 8 identical to the first, every row checksum unique), then
10 s of strips per mode with every row checksummed against the reference.

| Mode | Lines/strip | Strips/frame | Strips | Size / sequence errors | Frames | Frames = reference | Rows checked | Bad rows | Stalls, overruns, truncated, skipped, errors | Check max/strip (us) | fps | Verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0: 1280x800 RAW8 | 64 | 13 | 15 588 | 0 / 0 | 1199 | 1199 | 959 264 | 0 | 0 | 542 | 119.921 | PASS |
| 9: 1280x800 RAW10 | 64 | 13 | 15 588 | 0 / 0 | 1199 | 1199 | 959 264 | 0 | 0 | 562 | 119.921 | PASS |
| 10: 1280x720 RAW10 | 64 | 12 | 14 389 | 0 / 0 | 1199 | 1199 | 863 344 | 0 | 0 | 562 | 119.921 | PASS |
| 11: 640x400 RAW10 | 128 | 4 | 8 382 | 0 / 0 | 2095 | 2095 | 838 256 | 0 | 0 | 567 | 209.568 | PASS |

The strips task got the same table (`/tmp/poc-p4-eth/strips10/check3.log`).

Strip soak (`RAW10_STRIP_SOAK_PLAN`, log `/tmp/poc-p4-eth/final/raw10soak.log`, on the merged
code): the method of the RAW8 strip soak above, with a ring of 3 strips of 102 400 B in the
same 320 KiB, 64-line strips at 1280 wide and 128 at 640 (a 64-line strip there is 51 200 B,
not a whole number of the bridge's 4096-byte bursts; `strip_soak::lines`), threshold 512 in
10-bit units, and the budget the mode's frame period (1 s / nominal fps: 8333 us, 4762 us at
210 fps). The synthetic strip is filled with `Synth::fill_raw10`, its four 16-row bands
repeated in a 128-row strip. The receiver and the sensor stream are started for every run
and stopped before its results are logged: while the firmware logs or prepares the next run
the DMA would fill the ring and halt, and the frames such a stall costs until the driver has
resynchronised would count against the run.

| Mode | Run | Blobs/frame fed | Frames complete | Truncated | Skipped | Stalls | Overruns | Errors | Proc min | Mean | Max | > budget | Hold max (row) | Frame end->centroids min/mean/max | fps | Verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 9: 1280x800 RAW10 | live | scene | 3597 | 0 | 0 | 0 | 0 | 0 | 5623 | 5624 | 5962 | 0 | 815 (0) | 388/389/425 | 119.921 | PASS |
| 9 | synth 0 | 0 | 1199 | 0 | 0 | 0 | 0 | 0 | 5623 | 5623 | 5629 | 0 | 464 (704) | 394/394/399 | 119.921 | PASS |
| 9 | synth 16 | 13 | 1199 | 0 | 0 | 0 | 0 | 0 | 5802 | 5803 | 6072 | 0 | 741 (0) | 422/422/428 | 119.921 | PASS |
| 9 | synth 64 | 64 | 1199 | 0 | 0 | 0 | 0 | 0 | 6816 | 6816 | 6822 | 0 | 558 (704) | 559/560/566 | 119.921 | PASS |
| 10: 1280x720 RAW10 | live | scene | 3597 | 0 | 0 | 0 | 0 | 0 | 5062 | 5062 | 5070 | 0 | 463 (0) | 433/433/439 | 119.921 | PASS |
| 10 | synth 0 | 0 | 1199 | 0 | 0 | 0 | 0 | 0 | 5062 | 5062 | 5067 | 0 | 464 (640) | 438/438/444 | 119.921 | PASS |
| 10 | synth 16 | 12 | 1199 | 0 | 0 | 0 | 0 | 0 | 5227 | 5227 | 5233 | 0 | 478 (640) | 466/466/472 | 119.921 | PASS |
| 10 | synth 64 | 66 | 1199 | 0 | 0 | 0 | 0 | 0 | 6409 | 6410 | 6421 | 0 | 584 (256) | 565/566/575 | 119.921 | PASS |
| 11: 640x400 RAW10 | live | scene | 6287 | 0 | 0 | 0 | 0 | 0 | 1700 | 1700 | 1701 | 0 | 547 (0) | 484/485/485 | 209.568 | PASS |
| 11 | synth 0 | 0 | 2095 | 0 | 0 | 0 | 0 | 0 | 1700 | 1700 | 1701 | 0 | 550 (256) | 489/490/490 | 209.568 | PASS |
| 11 | synth 16 | 18 | 2095 | 0 | 0 | 0 | 0 | 0 | 2063 | 2063 | 2070 | 0 | 672 (128) | 611/612/612 | 209.568 | PASS |
| 11 | synth 64 | 64 | 2095 | 0 | 0 | 0 | 0 | 0 | 3035 | 3037 | 3044 | 0 | 975 (256) | 988/989/995 | 209.568 | PASS |

(Times in us. Frames complete = every frame the sensor sent in 30 s or 10 s. The driver's
interrupt handler took 2.0–3.2 us on average, 10.5 us at most; the strip invalidation 2.0–2.3
us on average, 2.4 us at most.)

- All 12 runs pass, as in the strips task (`/tmp/poc-p4-eth/strips10/soak2.log`, within 2 %).
  The live runs had 0 blobs in every mode: even the brighter bench scene (at most 67 of 255 in
  RAW8) stays below threshold 512, so RAW10 under a bright live load is still untested.
- **RAW10 costs about 1.3 times RAW8 from strips** at 1280 wide (dark: 5623 against 4323 us;
  64 blobs: 6816 against 5185 us, RAW8 from the table above) and still fits 120 fps with 82 %
  of the budget at most; 640x400 RAW10 fits 210 fps at 64 %.
- A ring of 3 RAW10 strips held: the longest hold was 975 us (640x400, 64 blobs, a 128-line
  strip), no stall in any run.
- RAW8 under the per-run restart (strips task, `/tmp/poc-p4-eth/strips10/raw8soak2.log`,
  modes 0 and 3): all 8 runs PASS, max 4631/4371/4675/5233 us at 1280x800 and
  1354/1355/1617/2296 us at 640x400 (live, synthetic 0, 16, 64); the means moved by at most
  about 3 % against the one-stream-per-mode runs above.

#### The stall fault (fixed)

Found by the strips task (`/tmp/poc-p4-eth/strips10/`, `soak1.log`, `diag1.log` ..
`diag10.log`), root-caused and fixed by the stall task (logs in `/tmp/poc-p4-eth/stall/`). The
fix is `9d21695` in `esp32p4-csi` (`kick()` in `strips.rs`, new `restart_isp_bridge` in
`lib.rs`; section "Stalls" of the `strips.rs` module docs), the stall test `3325d0c` in this
crate (`STRIP_STALL_PLAN`).

What was seen: the first RAW10 soak hung at its second run, after the pause between runs
during which the 3-strip ring filled and the DMA halted. A deliberate 30 ms busy wait right
after stream-on crashed or hung the board in RAW10 with rings of 3 and of 2 strips: load
access faults in the driver's DMA handler (`State::collect`, the list-item pointer holding
0xA4070706, which looks like image data) and in the executor's waker. RAW8 recovered from the
same halt with a few lost frames. Restarting the DMA channel instead of `BLK_TFR_RESUMEREQ`
(`diag7`) and turning the bridge off for the length of the halt (`diag10`) did not help.

Cause:

- The CSI bridge is the DMA's flow controller (`TT_FC` = 4, `CSI_DMA_FLOW_CONTROLLER`; TRM
  38.7.2, reg 38.147, 7.5.5), so it alone ends DMA blocks; the list item's `BLOCK_TS` (12799)
  does not bound them (TRM 7.5.1: "the flow controller controls the block length").
- While the DMA halted on a held strip, the bridge filled up and discarded the rest of the
  frame (`CSI_BRIG_DISCARD`). When simply resumed, it flushed its 961 buffered items as one
  block, and its block cuts were off from then on.
- In RAW10 (1280x800, ring of 3, a strip returned while the bridge was empty and discarding)
  it never ended the next block at all. A trace kept in LP RAM across a watchdog reset
  (`s4.log`) shows the DMA writing 365 248 B from the start of strip 1, over the rest of the
  ring, the statics and the whole stack, until about 8 ms later a destination decode error
  (`DST_DEC_ERR`, status 0x48) at 0x4FFC_0100, past the end of L2MEM (0x4FFC_0000). That
  explains the faults on pointers holding image data. The earlier sentinel evidence (the area
  after the ring never written) was invalid: the sentinel was only checked in runs that
  survived.
- Why RAW8 survived (`s5.log`): the same sequence gave a block of 9792 items instead of 10240
  (448 short) and a frame-end block of 5569 instead of 5120 (449 late). Wrong but bounded, and
  the driver's probation frame caught them. Why RAW10 never ends the block is [INFERENCE]:
  probably RAW10 frames end on a half burst (a last strip of 12.5 bursts) and RAW8 frames on
  whole bursts; the inside of the bridge is not visible.

Fix: before every resume after a halt (`BLK_TFR_RESUMEREQ`), and before every restart after a
DMA error, the driver resets and reprograms the ISP and the bridge, then enables the bridge.
The CSI host and the D-PHY keep running. It happens only while the DMA channel waits between
blocks, so no bridge transfer is in flight, inside the driver's existing critical section
(`HP_RST_EN0` is shared with other modules). Resetting the bridge alone gave no data after
the release (`s6.log`); the ISP reset is needed too (`s7.log`). The `s7` trace after the fix:
the first block is a full strip (0x31ff) at the next frame start, and every block after it has
its normal size (frame end 0x18ff). The probation rule is unchanged, and so is the
steady-state interrupt path: `kick` returns early unless the channel is halted.

Stall test (`STRIP_STALL_PLAN`, log `/tmp/poc-p4-eth/stall/f2-stall.log`): modes 0, 9, 10 and
11, six cases each (`STALL_CASES`: single halts of 1, 5, 30 and 100 ms, bursts of 20 x 2 ms
and 8 x 30 ms). Each case holds a strip until the driver reports the DMA halted, keeps it for
the case's time, then checks every strip row by row up to the first whole frame begun after
the last release, then runs a 1 s strip check on fresh counters. An address-pattern guard
covers 16 KiB before the list items, the space after them and the ring tail; it is filled
before each case and checked after.

| Modes | Bad rows, size/order errors, guard words written | Lost per halt | Next whole frame after the release | 1 s strip check after recovery | Verdict |
|---|---|---|---|---|---|
| 0 (1280x800 RAW8), 9 (1280x800 RAW10), 10 (1280x720 RAW10), 120 fps | 0 in every case | at most one frame skipped and one truncated | 25.4–32.6 ms | 119/119 frames matching, 0 gaps | all cases PASS |
| 11 (640x400 RAW10, 210 fps) | 0 in every case | at most one frame skipped and one truncated | 14.0–18.4 ms | 209/209 frames matching, 0 gaps | all cases PASS |

24/24 PASS. The driver counted 1 stall per single halt, 8 for 8 x 30 ms and 20 for
20 x 2 ms. A first run (`f1-stall.log`) gave 22/24: the two 640x400 burst cases failed
"sequence" from a test bug (frames delivered between the halts of a burst counted as
recovery), fixed before `f2`.

Without the fix (`u1-stall.log`, the same test with the fix removed): RAW8 mode 0 recovered
in all 6 cases (with the driver counting 2 stalls per single halt), then the board froze in
the first RAW10 case (1 x 1 ms): no output for the rest of the 90 s.

Regression runs on the fixed driver (same folder):

| Run | Log | Result |
|---|---|---|
| `STRIP_CHECK_PLAN` | `f-check.log` | 4/4 PASS: 15588 / 15588 / 14389 / 8382 strips checked, 0 bad rows, 0 stalls |
| `RAW10_STRIP_SOAK_PLAN` | `f-raw10soak.log` | 12/12 PASS, every frame complete, 0 stalls |
| `STRIP_SOAK_PLAN`, mode 0 only (temporary plan, removed afterwards) | `f-raw8soak.log` | 4/4 PASS: 3597 / 1199 / 1199 / 1199 frames complete, 0 stalls |

Interrupt handler times, mean/max in us:

| Soak | Before the fix | After the fix |
|---|---|---|
| RAW10 | strips task (`strips10/soak2.log`): 1280x800 live 3.0/3.1, synthetic up to 3.3/3.5, one 10.5 max; this README: 2.0–3.2 mean | 2.2–3.4 mean, 3.4–3.6 max |
| RAW8 mode 0 (live, synthetic 0, 16, 64) | strips task (`strips10/raw8soak2.log`): 3.0/3.2, 3.0/10.0, 3.1/3.3, 3.1/3.4; this README: 3.0–3.2 mean | 3.2/3.4, 3.2/3.4, 3.3/3.5, 3.3/3.6 |

The means are 0.1–0.3 us higher. The cause is not found; code layout is [INFERENCE], since
the restart code does not run without a stall.

Not covered or inferred:

- A bridge discard while the DMA is running but starved, rather than halted, does not go
  through the restart; it was never provoked and never seen in a soak. If it happens, the
  same overrun is possible [INFERENCE].
- A DMA write fence (TRM chapter 20, DMA APM, base 0x500A_6000) would contain any overrun; it
  is not implemented.
- The whole-frame path (`Csi`) may have the same exposure after dropped frames [INFERENCE]; it
  was not checked.
- That the ISP waits for the next frame start after its reset is seen in the `s7` trace only,
  not in the TRM.
- The guard covers the list-item area and the ring tail, not all of memory. Only the RAW8
  strip soak of mode 0 was rerun, not modes 1–3. One core, no Ethernet traffic.

### Sensor driver crate (`firmware/ov9281`)

The sensor side now has a configurable driver, `firmware/ov9281` (`no_std`, no heap):

- a mode builder: format (RAW8 or RAW10), output size, readout (crop or 2x2 binned), window
  origin, and frame rate with a tolerance in hertz. It chooses the clock (80 MHz, 96 MHz only
  above the fastest 80 MHz rate), HTS, VTS, PSV and the VFIFO read start, or refuses the
  request with the reason (`TooFast`, `TooSlow`, `Tolerance` with the nearest rate, `Size`,
  `Alignment`, `Window`). Every builder method is a `const fn`, so a mode can be a `const`;
- a device layer (`Ov9281`) over any `embedded-hal-async` I2C bus: chip ID, mode
  configuration, exposure, gain, test pattern, stream on and off, single register read and
  write.

Its limits (`src/limits.rs`) are the measured limits of this board's sensor (2026-11-10, 16
boots of a measurement harness in this crate, removed afterwards). The pass criterion was a
lit scene against a reference of the same window, because a wrong VTS, PSV, VFIFO or HTS
setting delivers whole, on-time frames with no receiver error, and the bar test pattern passes
where the scene is broken. The tables, the method and the evidence are in
[firmware/ov9281/README.md](../ov9281/README.md) ("Measured limits"); they are not repeated
here. What they settle or correct in this README:

- **Minimum VTS.** The dark, inverted picture at the shortest VTS is the sensor's minimum VTS
  with PSV off: crop h + 51, binned h + 31 lines, the same at 80 and 96 MHz, in RAW8 and RAW10
  and at every window and exposure measured. Below it the frames stay whole and on time with
  no error; further down no frames arrive. For this README's windows: 1280x720 crop last good
  771 (the 763 of the first mode 5 is below it), 320x200 binned last good 231 (222..=230 below
  it). That settles the VTS values left untested here (764..=779 at 1280x720, the odd values
  231..=239 and below 222 at 320x200). Its cause is still unknown. The crate adds 4 lines of
  margin: crop h + 55, binned h + 35.
- **PSV (0x4F00 = 0x0C).** Not monotonic in VTS − h: a right picture at crop h + 51..54
  (binned h + 31..34), no frames at h + 55..57 (h + 35..37), right at h + 58 (h + 38), rows
  flat at the black level from h + 59 to h + 89 or 90 (h + 39..70), right from h + 90 or 91
  (h + 71). The flat rows seen here at VTS 780..=810 (1280x720) and at VTS 241 (320x200,
  h + 41) are these bands. Where PSV is safe it changes nothing measurable (picture, black
  level, noise, frame period); its power saving was not measured. The crate turns PSV on only
  from crop h + 95 / binned h + 75 and off below, per mode, instead of this PoC's "on in the
  common block, off in mode 8".
- **Mode 7 (640x400 binned, VTS 434, 252.402 fps).** VTS 434 is binned h + 34: with PSV on, as
  mode 7 runs, it sits in the narrow good window h + 31..34, one line before the no-frame band
  h + 35..37. It passed here at that edge. The crate no longer allows it: the fastest 80 MHz
  640x400 binned mode is VTS 435 (251.824 Hz, PSV off), and 252.402 Hz requested within
  1 mHz moves to 96 MHz and fails with `Tolerance` (nearest 252.404 Hz).
- **VFIFO read start (0x4601).** It counts 8 pixels (8 bytes in RAW8, which this README's
  mode 8 notes assumed). The good range is from a lower limit to w / 8 (one line); above it no
  frames, below it shifted or smeared lines. The lower limit is 1 at 96 MHz and in RAW10, but
  in RAW8 at 80 MHz it scales with the width: `ceil(w / 40) - 4`, 28 at 1280, 12 at 640, 4 at
  320. So mode 8's 0x04 at 320 wide is the lowest good value (3 fails), at the edge of its
  range, and the common block's 0x30 gives no frames below 384 pixels. No fixed value serves
  every width; the crate uses `max(1, w / 16)` (0x14 at 320).
- **HTS.** The sensor's shortest line is HTS 723 with 1296 array columns (crop 1280, binned
  640), 671..677 for narrower windows. The 2-lane link adds a minimum of
  `ceil(line_bytes x MHz / 200) + 75` cycles, which binds only for RAW10 at 96 MHz from width
  1088: 1280-wide RAW10 at 96 MHz needs HTS 840 (no frames at 787 and below), and the crate
  uses 851 there, so its fastest 1280x800 RAW10 rate at 96 MHz is 131.781 Hz (run in the
  request verification below: 131.784 fps, a right picture). Long lines brighten the picture
  (1.04x at HTS 2912); the crate stops at 2912. Every mode of this PoC runs at HTS 728, inside
  the range.
- **Frame period.** `(VTS − 1) × HTS + 1730` holds over HTS 659..65535 and VTS 21..65535
  (1493 probes, both clocks and formats, crop and binned), every period 23.5..33.9 ppm below
  the formula.
- **Width, height, origin.** RAW8 widths must be multiples of 8 (a right picture from 120 up;
  most widths of 4 mod 8 give no frames); the ESP32-P4 receiver needs RAW10 widths that are
  multiples of 64 and even RAW10 heights. Heights stream from 1 to 800 (binned 400). The
  sensor truncates the x origin to a multiple of 8 and floors a binned y origin to even; a
  crop y origin is exact. A window past the array still streams (with flat rows). The crate
  refuses what the sensor would change or not deliver: RAW8 widths a multiple of 8, RAW10 of
  64, 128..=1280 crop and 128..=640 binned; heights 2..=800 (binned 2..=400), even in RAW10;
  x origin a multiple of 8, binned y even, the window inside the array.

Requested through the builder at each mode's rate within 1 mHz, 11 of this PoC's 12 modes
resolve to the PoC's own clock, HTS, VTS and PSV, with every register write and the order
equal to the PoC tables except 0x4601 (`tests/legacy.rs` in the crate); mode 7 does not
resolve (above).

The PoC itself requests its modes at the nominal rate ±0.5 Hz (`src/modes.rs`), so the solver
takes the nearest VTS instead; the settings it chose and their measurement are in "All 12
modes".

#### Request verification on the P4 (`REQUESTS_PLAN`)

After the move to the crate (`e18819d`, crate limits `3ddd024`), one boot ran the builder on
the P4 and the board through every request the characterisation verified and every one it
must refuse, plus low rates, the narrowest window and the fastest rate per window at each
clock. Log `/tmp/poc-p4-eth/migrate/requests.log` (2026-11-10), stills and the comparison
script (`ana.py`, numpy) in the same directory.

Method: each request is built 5 times at run time (`black_box` inputs, cycle counter); the
log gives the fastest call and the first. A refused request must give the expected error. An
accepted one is captured twice, both with the same exposure time (7.28 ms, clamped to the
mode's longest) and gain 8x: first a reference of the same window at 30 Hz (VTS 3662, PSV
forced off, 1 s), then the request (max(3 s, 6 periods)) with a scene still and a bar-pattern
still. Pass, as in the characterisation: right sizes, 0 dropped, overwritten, sequence gaps
and DMA, host or bridge errors; the rate the crate predicts (P4 timer); no flat row; 16x10
block-mean correlation with the reference ≥ 0.95 and brightness `(mean − 4) / (ref − 4)` in
0.8..1.25; the bar still with every even row equal to row 0 and every odd to row 1; the scene
still found at the solved origin (normalised correlation, ±4 px) in the nearest 1280x800 RAW8
scene still of the boot, binned against a 2x2 mean.

Accepted (rates in Hz; the tolerances are those of the crate's `tests/requests.rs`, the
solver's error on the edge of most; every choice is the one those tests expect):

| Request | Window | Rate ± tolerance | Solver: SCLK, HTS, VTS, PSV | Predicted | Measured (ppm) | Frames | Block corr / brightness | Found at |
|---|---|---|---|---|---|---|---|---|
| rq01 | RAW8 1280x800 crop | 120 ± 0.1 | 80, 728, 914, on | 120.049 | 120.052 (+31) | 361 | 0.9997 / 1.028 | (0, 0) |
| rq04 | RAW8 1280x720 crop | 140 ± 0.080 | 80, 728, 784, off | 139.920 | 139.924 (+30) | 421 | 1.0000 / 1.011 | (0, 40) |
| rq05 | RAW10 1280x720 crop | 120 ± 0.082 | 80, 728, 914, on | 120.049 | 120.052 (+30) | 361 | 1.0000 / 1.009 | (0, 39) |
| rq06 | RAW8 640x480 crop at (0, 0) | 200 ± 0.027 | 80, 728, 548, off | 200.027 | 200.032 (+29) | 601 | 0.9999 / 1.024 | (0, 0) |
| rq16b | RAW8 640x240 crop at (200, 160) | 300 ± 0.062 | 80, 728, 365, on | 299.938 | 299.946 (+29) | 901 | 0.9999 / 1.005 | (200, 160) |
| rq02 | RAW8 1280x800 crop | 128 ± 0.021 | 80, 728, 857, off | 128.021 | 128.024 (+29) | 385 | 0.9999 / 1.023 | (0, 0) |
| rq07 | RAW8 640x400 binned | 250 ± 0.105 | 80, 728, 438, off | 250.105 | 250.112 (+29) | 751 | 1.0000 / 1.010 | (0, 0) |
| rq08 | RAW10 640x400 binned | 210 ± 0.1 | 80, 728, 522, on | 209.964 | 209.970 (+29) | 631 | 0.9999 / 1.027 | (0, 0) |
| rq20 | RAW8 640x400 binned | 300 ± 2 | 96, 728, 438, off | 300.126 | 300.134 (+29) | 901 | 0.9999 / 1.008 | (0, 0) |
| rq09 | RAW8 320x200 binned | 470 ± 0.871 | 96, 728, 279, on | 470.325 | 470.337 (+25) | 1412 | 0.9999 / 0.996 | (320, 200) |
| rq10 | RAW8 320x200 binned at (96, 36) | 400 ± 0.509 | 80, 728, 273, off | 400.509 | 400.519 (+25) | 1202 | 0.9996 / 1.059 | (96, 36) |
| rq15 | RAW10 320x200 binned | 400 ± 0.509 | 80, 728, 273, off | 400.509 | 400.519 (+27) | 1202 | 0.9996 / 1.062 | (320, 200) |
| rq17 | RAW10 320x200 binned at (400, 160) | 560 ± 2.126 | 96, 728, 235, off | 557.874 | 557.888 (+27) | 1675 | 0.9999 / 1.004 | (400, 160) |
| rq03 | RAW8 1280x800 crop | 150 ± 0.044 | 96, 728, 878, off | 149.956 | 149.960 (+29) | 450 | 0.9999 / 0.991 | (0, 0) |
| rq12 | RAW10 1280x800 crop | 125 ± 0.036 | 80, 728, 878, off | 124.964 | 124.967 (+29) | 376 | 1.0000 / 1.012 | (0, 0) |
| rq21 | RAW8 1280x800 crop | 120 ± 0.001 | 80, **738**, 902, on | 120.000 | 120.003 (+29) | 361 | 1.0000 / 1.007 | (0, 0) |
| rq11 | RAW8 1280x64 crop | 1000 ± 5 | 96, 728, 130, off | 1003.743 | 1003.770 (+26) | 3013 | 0.9995 / 1.008 | (0, 368) |
| rq14b | RAW10 640x480 crop | 240 ± 0.032 | 96, 728, 548, off | 240.032 | 240.042 (+41) | 722 | 0.9999 / 1.017 | (320, 160) |
| rq18 | RAW8 1280x400 crop at (0, 200) | 15 ± 0.001 | 80, 728, 7325, on | 14.999 | 14.999 (+29) | 45 | 0.9997 / 0.972 | (0, 201) |
| rq19 | RAW10 1216x704 crop at (32, 48) | 130 ± 0.2 | 80, 728, 844, on | 129.990 | 129.993 (+29) | 391 | 1.0000 / 1.012 | (32, 48) |
| rq16 | RAW8 320x240 crop at (32, 501) | 364.6 ± 0.027 | 80, 728, 300, off | 364.627 | 364.637 (+27) | 1095 | 0.9987 / 1.082 | (32, 501) |
| rq13 | RAW8 1280x800 crop | 30 ± 0.011 | 80, 728, 3662, on | 29.997 | 29.997 (+29) | 90 | 0.9999 / 0.998 | (0, 0) |
| lo2 | RAW8 1280x800 crop | 2 ± 0.01 | 80, 728, 54944, on | 2.000 | 2.000 (+30) | 7 in 3.5 s | 0.9999 / 1.006 | (0, 0) |
| lo042 | RAW8 1280x800 crop | 0.42 ± 0.001 | 80, **2897**, 65535, on | 0.421 | 0.421 (+27) | 7 in 16.6 s | 0.9998 / 1.035 | (0, 0) |
| narrow | RAW8 128x16 crop | 1000 ± 5 | 80, 728, 109, off | 995.594 | 995.621 (+27) | 2988 | 0.9535 / 1.005 | (576, 393) |
| f800-80 | RAW8 1280x800 crop | 128.320 exactly | 80, 728, 855, off | 128.320 | 128.323 (+30) | 386 | 0.9999 / 0.999 | (0, 0) |
| f800-96 | RAW8 1280x800 crop | 153.984 exactly | 96, 728, 855, off | 153.984 | 153.988 (+29) | 463 | 0.9984 / 0.926 | (0, 0) |
| f720-80 | RAW8 1280x720 crop | 141.542 exactly | 80, 728, 775, off | 141.542 | 141.546 (+29) | 426 | 0.9999 / 0.981 | (0, 40) |
| f720-96 | RAW8 1280x720 crop | 169.851 exactly | 96, 728, 775, off | 169.851 | 169.855 (+29) | 511 | 0.9999 / 1.030 | (0, 40) |
| f480-80 | RAW8 640x480 crop | 204.875 exactly | 80, 728, 535, off | 204.875 | 204.881 (+29) | 615 | 1.0000 / 0.992 | (320, 160) |
| f480-96 | RAW8 640x480 crop | 245.850 exactly | 96, 728, 535, off | 245.850 | 245.857 (+29) | 738 | 1.0000 / 1.008 | (320, 160) |
| f400-80 | RAW8 640x400 binned | 251.824 exactly | 80, 728, 435, off | 251.824 | 251.831 (+29) | 756 | 1.0000 / 1.008 | (0, 0) |
| f400-96 | RAW8 640x400 binned | 302.189 exactly | 96, 728, 435, off | 302.189 | 302.197 (+29) | 907 | 1.0000 / 1.016 | (0, 0) |
| f200-80 | RAW8 320x200 binned | 464.895 exactly | 80, 728, 235, off | 464.895 | 464.906 (+25) | 1396 | 0.9999 / 1.003 | (320, 200) |
| f200-96 | RAW8 320x200 binned | 557.874 exactly | 96, 728, 235, off | 557.874 | 557.888 (+25) | 1675 | 0.9999 / 1.010 | (320, 201) |
| f800r10-80 | RAW10 1280x800 crop | 128.320 exactly | 80, 728, 855, off | 128.320 | 128.323 (+29) | 386 | 0.9999 / 1.005 | (0, 0) |
| f800r10-96 | RAW10 1280x800 crop | 131.781 exactly | 96, **851**, 855, off | 131.781 | 131.784 (+29) | 397 | 0.9999 / 1.016 | (0, 0) |
| f400r10-80 | RAW10 640x400 binned | 251.824 exactly | 80, 728, 435, off | 251.824 | 251.831 (+29) | 756 | 1.0000 / 1.012 | (0, 0) |
| f400r10-96 | RAW10 640x400 binned | 302.189 exactly | 96, 728, 435, off | 302.189 | 302.197 (+29) | 907 | 1.0000 / 1.008 | (0, 0) |
| rq01b | RAW8 1280x800 crop | 120 ± 0.1 | 80, 728, 914, on | 120.049 | 120.052 (+29) | 361 | 0.9999 / 1.003 | (0, 0) |

- All 40 pass: every capture (and every reference) with 0 wrong size, dropped, overwritten or
  sequence gaps and DMA, host and bridge errors all 0; no flat row in any scene or reference
  still; every bar still exact (0/255, RAW10 0/1023); every scene found at its origin, within
  1 px of drift. The worst block correlation is the 128x16 window (0.9535, one 16x10 block is
  8x1.6 pixels); the pixel correlations of the small, dim windows are low for the same reason
  (narrow 0.66, rq16 0.78, rq11 0.83, rq16b 0.94; location 0.78, 0.82, 0.90, 0.96), as in the
  characterisation (rq11 location 0.89, rq16 0.67). The others are ≥ 0.966.
- Measured against predicted: +25 to +41 ppm, the same offset as the 12 modes. The requests
  without a tolerance (`fNNN`) deliver the maximum the crate computes to the millihertz.
- The fastest rates per window, all now run on the board (80 / 96 MHz): 1280x800 128.320 /
  153.984, 1280x720 141.542 / 169.851, 640x480 204.875 / 245.850, 640x400 binned 251.824 /
  302.189, 320x200 binned 464.895 / 557.874; RAW10 1280x800 128.320 / 131.781 (HTS 851),
  640x400 binned 251.824 / 302.189. All at the crate's minimum VTS (h + 55, binned h + 35).
- `x11` (1280x800 at 120 Hz ± 0.0001, a tolerance that rounds to 0 mHz) builds as `rq21`
  (HTS 738, VTS 902, exactly 120.000 Hz) and was not captured again.

Refused, each with the expected error and bound (as the `Error`'s `Display` prints it):

| Request | Asked | Error |
|---|---|---|
| x1 | RAW8 1280x800 crop, 160 Hz ± 0.1 | `TooFast { max: 153.984 Hz }` |
| x2 | RAW10 1280x800 crop, 140 Hz ± 0.1 | `TooFast { max: 131.781 Hz }` |
| x3 | RAW10 640x480 crop, 250 Hz ± 0.1 | `TooFast { max: 245.850 Hz }` |
| x4 | RAW8 1276x800 crop, 120 Hz | `Alignment { width_multiple: 8, height_multiple: 1 }` |
| x5 | RAW10 1248x720 crop, 120 Hz | `Alignment { width_multiple: 64, height_multiple: 2 }` |
| x6 | RAW10 640x401 crop, 120 Hz | `Alignment { width_multiple: 64, height_multiple: 2 }` |
| x7 | RAW8 1296x800 crop | `Size { max_width: 1280, max_height: 800 }` |
| x8 | RAW8 0x0 crop | `Size { max_width: 1280, max_height: 800 }` |
| x9a | RAW8 640x480 crop at (700, 0) | `Window` |
| x9b | RAW8 640x480 crop at (0, 400) | `Window` |
| x10 | RAW8 48x200 crop | `Size { max_width: 1280, max_height: 800 }` |
| x11b | RAW8 1280x800 crop, 120.001 Hz exactly | `Tolerance { nearest: 120.000 Hz }` |
| x12 | RAW8 1280x800 crop, 0.1 Hz ± 0.01 | `TooSlow { min: 0.419 Hz }` |
| x13 | RAW8 640x480 crop at (101, 37) | `Window` |

The seven `fNNN-96` probes (1 MHz requested) gave `TooFast` with the maximum each then
captured.

`build()` on the P4 (400 MHz, code in flash through the cache; fastest of 5 calls, first call
in brackets):

| Case | Cycles | Time |
|---|---|---|
| Refused on shape (`Size`, `Alignment`, `Window`) | 157–205 (157–3092) | under 1 us |
| Accepted at the standard line length (HTS 728; 851 for `f800r10-96`) | 1011–2295 (1272–81 206) | 2.5–5.7 us (up to 203 us) |
| Accepted after a longer-line search (`rq21`, `x11`: HTS 738) | 1453–1475 (24 897–75 755) | 3.6–3.7 us (up to 189 us) |
| Accepted at HTS 2897 (`lo042`, the longest accepted search) | 72 993 (151 729) | 182 us (379 us) |
| `TooFast` (probes and x1..x3) | 41 938–45 342 (42 101–107 105) | 105–113 us (up to 268 us) |
| `Tolerance` after the worst-case search (`x11b`, 2185 iterations in the crate's count) | 45 417 (51 290) | 114 us (128 us) |
| `TooSlow` (`x12`) | 75 192 (78 177) | 188 us (195 us) |

A `const` mode costs nothing at run time; a mode built at run time cost at most 379 us here.
The first call is up to 63 times the fastest [INFERENCE: cache misses on code and constants
in flash; the repeats run from the cache].

### PSRAM tuning attempts

| Setting | Result |
|---|---|
| 200 MHz (`PsramTimingParams::MHZ_200`) | PSRAM init fails: esp-hal 1.2.2 runs the PSRAM source clock (MPLL) at 500 MHz, which is not a multiple of 200. Needs a clock-tree change, not tried |
| L2 line size 128 B | No esp-hal config key on the P4; needs ROM or register calls. Not tried |
| L1/L2 autoload (prefetch) | No esp-hal config key on the P4; needs ROM or register calls. Not tried |

The firmware runs with the default: 250 MHz, L2 128 KiB with 64 B lines, no autoload.

### JPEG encoder (`firmware/esp32p4-jpeg`, `JPEG_PLAN`)

The chip's hardware JPEG codec, greyscale baseline, driven by the `esp32p4-jpeg` crate (its
docs: bring-up, buffers, cache, interrupts, errors). `src/jpeg_bench.rs` runs five phases:
test picture, camera stills, speed alone, capture and encode, robustness. Encode times are
the whole `Encoder::encode` call (header copy, cache calls, descriptors, the codec, the
wait), from the cycle counter. Clocks: CPU 400 MHz, SYS_CLK 200 MHz (the codec's clock,
TRM §37.2; `root_clk` in the driver's `DebugStatus`: SYS = MEM / 1 = CPU / 2), PSRAM 250 MHz.

Logs: `/tmp/poc-p4-eth/jpeg/run1.log` .. `run5.log`; files decoded with `tools/still.py` and
checked with `tools/jpeg_check.py` in `/tmp/poc-p4-eth/jpeg/run<n>/`. Run 4 is the full plan;
run 5 repeats it on the final code.

#### Correctness

The test picture (`draw_test`: a gradient in the top half, `Synth` discs on noise in the
bottom half, a white border, a 3-pixel diagonal) and one camera frame per size, each at
quality 50, 75 and 90, dumped and checked against libjpeg-turbo (`djpeg` decodes it,
`cjpeg -grayscale -baseline -quality <q>` is the reference encoder). Every file: SOF0, 8 bits,
one component, the right size, EOI at the end, decoded without a warning. Bytes and PSNR
against the source (run 4; the test-picture files are identical in every run):

| Picture | q 50 / 75 / 90: board B | cjpeg B | board dB | cjpeg dB |
|---|---|---|---|---|
| test 1280x800 | 35 471 / 66 541 / 143 109 | 35 724 / 66 935 / 144 133 | 37.05 / 38.47 / 41.01 | 37.05 / 38.47 / 41.01 |
| test 640x400 | 12 881 / 20 126 / 39 805 | 12 883 / 20 198 / 39 995 | 36.17 / 38.07 / 40.89 | 36.18 / 38.08 / 40.89 |
| test 320x200 | 5 203 / 7 354 / 12 795 | 5 167 / 7 347 / 12 805 | 34.68 / 37.30 / 40.67 | 34.70 / 37.30 / 40.68 |
| camera 1280x800 | 15 402 / 19 358 / 47 591 | 15 373 / 19 468 / 49 655 | 46.56 / 47.76 / 48.54 | 46.56 / 47.76 / 48.54 |
| camera 640x400 binned | 6 038 / 8 659 / 17 874 | 6 006 / 8 685 / 18 321 | 44.79 / 46.45 / 47.94 | 44.79 / 46.44 / 47.94 |
| camera 320x200 binned | 1 342 / 1 587 / 2 742 | 1 287 / 1 538 / 2 773 | 48.01 / 50.13 / 51.01 | 48.01 / 50.13 / 51.02 |

The camera scene is dim and indoor (exposure 7.3 ms at 1280x800, gain 2x), so its files are
small and its PSNR high. The quality equals libjpeg-turbo's to 0.02 dB. The board's header is
56 bytes longer (a COM segment pads it to 384 bytes so the hardware's part starts on a cache
line); its entropy-coded data is 0.1 to 4.4 % smaller (the most at q90 on camera frames).
Decoded, 83 to 98 % of the pixels equal those of libjpeg-turbo's encode; the others differ by
at most 28 levels on the test picture (q50) and 6 on camera frames [INFERENCE: the codec's DCT
and quantiser round differently; the tables are the same]. The decoded pictures were looked
at: orientation, gradient, diagonal, discs and border right, no shifted rows, no swapped
blocks.

#### Speed alone (200 encodes each, run 4)

| Size | us per frame (mean) | fps | Mpixel/s | Study prediction |
|---|---|---|---|---|
| 1280x800 | 5 986 .. 5 988 | 167.0 | 171.0 | 147 fps |
| 1280x720 | 5 390 .. 5 392 | 185.5 | 171.0 | 163 fps (Espressif: CPU 360 MHz, PSRAM 200 MHz) |
| 640x480 | 1 814 .. 1 816 | 551 | 169.3 | 489 fps |
| 640x400 | 1 516 .. 1 518 | 659 | 168.8 | 587 fps |
| 320x200 | 398 .. 399 | 2 505 | 160.5 | about 2 300 fps |

- The quality changes nothing (q50, q75 and q90 within 2 us), nor does the content (camera
  1280x800 frames 5 988 us).
- Memory changes little: 320x200 from and to internal RAM 394 us, PSRAM to internal RAM 396 us,
  internal RAM to PSRAM 397 us; 1280x800 q50 from PSRAM into internal RAM 5 985 us.
- 171 Mpixel/s at 200 MHz is 0.86 pixel per codec clock [INFERENCE: Espressif's 150 Mpixel/s
  is the same rate at a 180 MHz SYS_CLK, which their 360 MHz CPU set-up gives]. The fixed
  cost per frame shows at 320x200 (160.5 instead of 171 Mpixel/s: about 25 us).
- The first encode after a size or quality change takes longer: up to 6 380 us at 1280x800.
  The worst of 200 back-to-back frames is 6 286 us; the codec is not reset between frames.

#### Capture and encode at once (run 4)

Per mode: 5 s of capture alone (`camera::measure`), then 20 s in which every delivered frame
(`Csi::wait_frame`, 3 buffers in PSRAM) is encoded at q90 into PSRAM while the CSI DMA writes
the next frames into PSRAM. Busy-skipped counts the frames the receiver completed while the
encoder was busy (sequence numbers skipped between two frames taken).

| Mode | Hz | Delivered = encoded | Busy-skipped | Encode us mean / max | Bytes mean | CSI errors |
|---|---|---|---|---|---|---|
| 1280x800 | 120.05 | 2 402 | 0 | 5 986 / 6 050 | 47 805 | 0 |
| 1280x800 | 128.32 | 2 567 | 0 | 5 988 / 5 989 | 47 893 | 0 |
| 1280x800 | 149.96 | 2 999 | 0 | 5 988 / 5 989 | 43 566 | 0 |
| 1280x800 | 153.98 (the sensor's maximum) | 3 080 | 0 | 5 986 / 5 987 | 43 489 | 0 |
| 1280x720 | 141.55 | 2 831 | 0 | 5 390 / 5 391 | 41 842 | 0 |
| 1280x720 | 169.86 | 3 397 | 0 | 5 390 / 5 391 | 38 017 | 0 |
| 640x400 binned | 250.11 | 5 003 | 0 | 1 516 / 1 517 | 12 306 | 0 |
| 640x400 binned | 300.13 | 6 003 | 0 | 1 516 / 1 516 | 11 160 | 0 |
| 320x200 binned | 464.91 | 9 299 | 0 | 398 / 399 | 2 708 | 0 |

The rates are those `camera::measure` logged before each window; run 3 gave the same counts
(without the 153.98 Hz run). Encoding while the CSI DMA writes into PSRAM takes as long as
alone. **Every frame is encoded at every rate the sensor gives**, 1280x800 at 153.98 Hz
included (the encoder busy 92 % of the frame period): the encoder's limit, 167 fps at
1280x800, is above the sensor's. 153.98 Hz x 43 489 B is 53.6 Mbit/s of JPEG for this scene.

#### Robustness (runs 3 and 4)

- `set_config` with 1284x800 gives `Geometry`, quality 0 gives `Quality`.
- An output of the header + 4 096 B for the 143 109 B file gives `OutputTooSmall` after
  702 us (run 4; 721 us in run 3), from the RX channel's `DSCR_EMPTY` interrupt: the codec
  stalls and raises nothing. The next encode runs the reset sequence (`Stats::resets`) and
  gives the reference file again (same length, same CRC).
- An encode dropped after 300 us (`with_timeout`): the next encode gives the reference file.
- 60 s at 1280x800 120 Hz q90: 7 203 delivered, 7 203 encoded, no error, no bad file; the
  last file decoded at 48.61 dB (cjpeg 48.61 dB).

Before the driver bound the 2D-DMA interrupts (run 2, 1280x800 q75 into 64 KiB of internal
RAM), the too-small output was also reported as `OutputTooSmall`, with no codec interrupt
seen (`last_jpeg` 0) [INFERENCE: found when the caller's 200 ms timeout polled the future;
the time was not logged].

## Findings for the camera board

- **SCCB address: 0x60** (7-bit; 8-bit 0xC0). Chip ID 0x9281 read at 0x60; nothing at 0x10.
  0x70 also answers with chip ID 0x9281 [INFERENCE: the sensor's second SCCB ID, not a second
  device; not checked]. The bus scan also finds 0x18 [INFERENCE: the ES8311 codec].
- **Enable polarity: half known.** With module pin 11 (PWDN) pulled to 3.3 V through 10 k,
  the sensor answers. So high = on for this module [INFERENCE: assumes the cable maps J1 pin 6
  to module pin 11 and the module has no strong pull-down]. Low = off is not tested: the dev
  board has no GPIO on that pin. To close it, a person can measure J1 pin 6 (or R42) to GND,
  expecting about 3.3 V, then tie CSI_IO0 to GND (for example 1 k from the J1 side of R42 to
  GND), reset and read the boot log. `OV9281: not found` means the enable is active-high, and
  the espion-cam board's GPIO10 must drive CAM_EN high to turn the camera on.
- **D-PHY supply:** on the dev board VDD_MIPI_DPHY comes from the P4's internal LDO channel 3
  (ESP_LDO_VO3), no external rail, as the architecture doc says for the ESP32-P4-Core module.
  The camera section sets VO3 to 2.5 V (`esp32p4_csi::enable_ldo`) before it starts the
  receiver; capture works with that.
- **CSI-2 link:** 2 lanes at 800 Mbit/s with a continuous clock lane work in all 12 modes,
  RAW8 and RAW10, free-running from 119.92 to 453.40 fps; the gated clock was not needed and
  not tried.
- **Frame period, for XVS and exposure timing:** a frame lasts `(VTS − 1) × HTS + 1730`
  SCLK cycles, not `VTS × HTS` (0.15 % longer at VTS 915, 0.57 % at VTS 241; measured within
  1.4 cycles over HTS 728..=2912 and VTS 434..=1830 here, and over HTS 659..65535 and VTS
  21..65535 in the sensor characterisation, 1493 probes). Any trigger period, exposure window
  or timestamp model on the product must use it; the sensor clock ran 25–41 ppm fast against
  the P4's timer on this board (23.5–33.9 ppm in the characterisation).
- **VFIFO read start (0x4601) has a range that depends on the width** (unit: 8 pixels): at
  most w / 8 (one line; above it no frames), at least 1, except RAW8 at 80 MHz, where the
  lower limit grows with the width (`ceil(w / 40) - 4`: 28 at 1280, 4 at 320; below it lines
  shift or smear). No fixed value serves every width: the common block's 0x30 loses every
  frame below 384 pixels, and 0x04 breaks RAW8 at 80 MHz above about 320. `w / 16` lies inside
  every range (the `ov9281` crate's rule). See "Sensor driver crate".
- **PSV mode (0x4F00 = 0x0C) is safe only from crop h + 91 / binned h + 71 lines of VTS:**
  below that, bands of no frames and of rows flat at the black level alternate with narrow
  good windows (the flat rows at VTS 780..=810 in 1280x720 and VTS 241 in 320x200 are such
  bands). Where it is safe it changes nothing measurable; its power saving was not measured.
  The `ov9281` crate sets it per mode, on only from h + 95 / h + 75.
- **Every window has a minimum VTS (PSV off): crop h + 51, binned h + 31 lines**, the same at
  80 and 96 MHz and in RAW8 and RAW10. Below it frames stay whole and on time with no error,
  but the picture is dark and inverted (VTS 763 at 1280x720, 222..=230 at 320x200); further
  down no frames arrive. Cause unknown; the crate keeps 4 lines of margin. So the 253 fps
  640x400 setting (VTS 434) worked only at the edge, and 144 fps at 1280x720 still needs the
  96 MHz clock (the crate's 80 MHz maximum there is 141.542 Hz).
- **RAW10 at 1280 wide is link-limited at 96 MHz:** it needs HTS 840 or more (the crate uses
  851), so 1280x800 RAW10 at 96 MHz tops out at 131.781 Hz (run on this board: a right picture).
  Elsewhere HTS 728 is enough (the link term binds only for RAW10 at 96 MHz from width 1088).
- **Window constraints:** RAW8 widths a multiple of 8; RAW10 widths a multiple of 64 and even
  heights (the P4 receiver); the sensor truncates the x origin to a multiple of 8 and floors a
  binned y origin to even; a window past the array still streams. The crate refuses all of
  these instead of letting the sensor change them.
- **96 MHz (modes 4 and 5) is above the datasheet's 80 MHz:** clean at gain 2x, but at gain
  15.5x vertical column stripes and a black floor raised by about 10 of 255. Keep the gain
  low at 96 MHz, or stay at 80 MHz.
- **RAW10 costs about 1.3x RAW8 in the detector from strips** (1280x800: 5.6 ms dark, 6.8 ms
  with 64 blobs) and fits 120 fps; 640x400 RAW10 fits 210 fps.
- **Strip stalls are recoverable since `9d21695`:** when the consumer lets the DMA reach a
  held strip, the driver restarts the ISP and the bridge before it resumes. In RAW8 and RAW10
  a halt costs at most one skipped and one truncated frame, and the next whole frame comes
  25–33 ms after the strip is returned at 120 fps (14–19 ms at 640x400, 210 fps). Stalls still
  lose frames, so the ring must still be sized for the consumer (see below). A bridge discard
  while the DMA runs starved, not halted, is not covered (see "Open points").
- **L2 cache:** the bootloader bundled with espflash 4.7.0 sets the L2 cache to 128 KiB, not the
  256 KiB esp-hal assumes by default (`ESP_HAL_CONFIG_L2_CACHE_SIZE`). This crate sets
  `"128KB"` in `.cargo/esp-config.toml` and gets 128 KiB more internal RAM. `cam-p4` still uses
  the default, which is safe but wastes that RAM.
- **PSRAM read ceiling:** about 163 MB/s for one core reading cold PSRAM with no other work,
  and 120 MB/s when every word is summed. [docs/architecture.md](../../docs/architecture.md)
  assumes about 200 MB/s. In the detector the PSRAM path runs at about 104 MB/s, which puts
  both 1280-wide modes over the 120 fps budget when the frame is read from PSRAM; the live
  soak confirms it (85–106 fps processed, 4–8 % slower again under DMA).
- **Capture into internal strips is the path for 1280-wide tracking:** with 64-line strips
  in a 320 KiB ring in internal RAM, all four modes processed every pixel of every frame at
  120 fps under live capture (at most 5.2 ms per 1280x800 frame), and the centroids are ready
  0.1–0.4 ms after the frame ends instead of a whole processing time later. Size the ring
  for the per-strip load spikes, not the average: a ring of 4 at 640 wide stalled in a
  bright scene (one strip held 1.7 ms); 8 strips in the same RAM did not.
- **Logging masks interrupts:** each `esp-println` line holds a critical section for as long
  as it takes to send, enough at 115200 baud to cut a frame short. Log at 2 Mbaud and never
  inside a capture or processing loop.

## Open points

- **Bridge discard with the DMA running:** a discard while the DMA is running but starved,
  rather than halted, does not go through the ISP and bridge restart. It was never provoked
  and never seen; the same overrun is possible then [INFERENCE].
- **No DMA write fence:** the DMA APM (TRM chapter 20, base 0x500A_6000) could contain any
  overrun of the strip DMA; it is not implemented.
- **Whole-frame path after dropped frames:** `Csi` may have the same exposure as the strip
  path had [INFERENCE]; not checked.
- **Interrupt handler 0.1–0.3 us slower since the stall fix** (means, RAW8 mode 0 and RAW10
  soaks), unexplained: the restart code does not run without a stall (code layout
  [INFERENCE]).
- **Stall test scope:** the guard covers the list-item area and the ring tail only; the RAW8
  strip soak was rerun on mode 0 only; one core, no Ethernet traffic.
- **Whole-frame detection from PSRAM is slower than a frame** in modes 0, 1, 4, 5, 9 and 10:
  one captured frame each, dark scene, 10.8, 9.7, 10.9, 9.8, 13.8 and 12.4 ms against 8.3,
  8.3, 6.9, 6.9, 8.3 and 8.3 ms (`/tmp/poc-p4-eth/migrate/stills.log`; modes 4..=11 within
  0.06 ms of the hand-built tables' runs). Those modes need the strip path; only modes 0..=3,
  9 and 10 were soaked on it.
- **No soak at the new rates:** modes 4..=8 have had capture-only verification (by
  decision); the RAW10 modes 9..=11 only the strip soak (the frame soak is RAW8 only). The
  40 requests of the request verification had 3 s of capture each (lo2 3.5 s and lo042
  16.6 s, 7 frames each), at gain 8x.
- **Mode 7 now runs 96 MHz** (253 Hz ± 0.5 resolves there, see "All 12 modes"): above the
  datasheet's 80 MHz, checked at gain 2x only. The 80 MHz alternative at 250 Hz (`rq07`, VTS
  438) and the 80 MHz maximum of 251.824 Hz (`f400-80`) were verified as well.
- **Rate offset of some 96 MHz settings:** the two 144 fps modes measure +45 / +47 ppm and
  `rq14b` (RAW10 640x480 at 240 Hz) +41, against +25..+31 for every other mode and request,
  96 MHz ones included (mode 7 +28, `rq03` +29). Unexplained; 10–20 us per second.
- **Sensor limits not measured** (kept conservative by the `ov9281` crate; list "Not
  measured, kept conservative" in its README): the minimum VTS of height 1; widths 113..119
  (crop) and 104..112 (binned); HTS below 728 and above 2912; the PSV bands at HTS other than
  728 and 1456; the dark-VTS limit at HTS 1000..1100 (crop) and 850..900 (binned); exposures
  between VTS − 13 and VTS; windows outside the array; 96 MHz at gain above 8x; the HTS 2912
  rates (formula only; HTS 2897 at 0.421 Hz was run in the request verification, as was the
  fastest 96 MHz RAW10 1280 rate, 131.781 Hz); the causes of the dark-picture limit, the PSV
  bands and the long-line brightening; one sensor at room temperature.
- **96 MHz artefacts:** the column stripes and the raised floor were seen at gain 15.5x only.
  The characterisation ran 96 MHz at gain 8x or less with its scene checks (correlation and
  brightness, not column noise); gain above 8x at 96 MHz is not measured. Cause unknown.
- **RAW10 under a bright live load:** every live RAW10 strip run had 0 blobs at threshold
  512, even in the brighter scene (at most 67 of 255 in RAW8 at gain 2x, 7.3 ms).
- **Brightness ratios between stills** vary with the bench light (mode 0 read 1.23x after
  90 s in one boot), and the picture moves by up to about 1.5 px between stills: geometry is
  verified to the whole pixel only.
- **Camera-enable polarity:** low = off is untested; the check needs a person (see
  "Enable polarity" above).
