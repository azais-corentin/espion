# poc-p4-eth

A throwaway proof of concept on a Waveshare ESP32-P4-ETH dev board. It is not part of the
product (that is `firmware/cam-p4` on the espion-cam board). It answers two questions before the
camera PCBs are ordered:

1. Does the OV9281 module deliver images to the ESP32-P4 over MIPI CSI-2 (2 lanes, RAW8)?
2. At which resolutions can the P4 process every pixel for marker tracking (threshold,
   connected components, weighted centroids) at 120 fps, all on the board?

The modes under test are 1280x800, 1280x720 (crop), 640x480 (crop) and 640x400 (2x2 binned),
with the sensor free-running (no XVS trigger). The PoC is minimal by intent: no Ethernet, no
PTP, no LEDs, no protocol.

Status, measured on the board (details below):

- Question 1: yes. The camera delivers whole frames over CSI-2 in all four modes at
  119.92 fps, with no dropped or short frame and no error. It also delivers 64-line strips
  into internal RAM, byte-identical to whole frames.
- Question 2: all four modes sustain 120 fps with every pixel processed when the CSI DMA
  writes 64-line strips into internal RAM: every run of the strip soak passed, with at most
  5.2 ms of processing per frame at 1280x800 and the centroids ready 0.1–0.4 ms after the
  end of the frame (synthetic and dark live frames; up to 3.5 ms in a bright live scene).
  With whole frames read from PSRAM only 640x480 and 640x400 do; the 1280-wide modes fall to
  85–106 fps processed.

Modes 4..=11 (faster RAW8 and RAW10) have had one first capture-only run; see "Fast modes
and RAW10: first capture run".

## Hardware set-up

| Part | What |
|---|---|
| Board | [Waveshare ESP32-P4-ETH](https://docs.waveshare.com/ESP32-P4-ETH): ESP32-P4NRW32X (32 MB PSRAM in package), 32 MB flash, CH343P USB-UART bridge on the USB-C port |
| Camera | SZYTF WX-1M-9281-79 OV9281 module, Raspberry-Pi-style 15-pin FFC (pin table in [docs/hardware/camera.md](../../docs/hardware/camera.md)) |
| Cable | 22-pin to 15-pin FFC into the board's 22-pin MIPI-CSI connector J1 [INFERENCE: Pi-style mapping, J1 pin 6 to module pin 11] |
| Host link | USB-C to the CH343, `/dev/ttyACM0` (UART0: 115200 baud for the ROM and bootloader, 2 Mbaud once the firmware runs) |

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

From this directory, inside the dev shell (or prefix with `nix develop -c` from the repo root):

```sh
cargo build --release
E=target/riscv32imafc-unknown-none-elf/release/poc-p4-eth
timeout 120 espflash flash   --chip esp32p4 --port /dev/ttyACM0 --non-interactive "$E"
timeout 330 espflash monitor --chip esp32p4 --port /dev/ttyACM0 --non-interactive --no-addresses \
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
  (frame soak, four modes) needs about 330 s; the strip soak finishes in under 300 s; capture
  only takes about 70 s for four modes with the cache check (about 100 s with the still dumps
  too); the detector benchmark (`RUN_BENCH = true`) about 90 s.
- `--no-addresses` stops espflash from printing symbol lines for hex numbers in the log.
- `cargo run --release` uses the runner `espflash flash --monitor ...`, which is interactive.
- `tools/still.py <log> [out dir]` (Python standard library only) checks each still's CRC and
  line count and writes `<out dir>/<name>.pgm` and `.png` (default `/tmp/poc-p4-eth`). RAW10
  stills become a 16-bit PGM and an 8-bit PNG.

### Choosing what runs

All in constants at the top of `src/main.rs`; one plan per boot.

| Constant | Value | Runs |
|---|---|---|
| `RUN_BENCH` | `false` (default) / `true` | Detector benchmark on synthetic frames, before the camera |
| `RUN_CAMERA` | `true` (default) | The camera section |
| `PLAN` | `&CAMERA_PLAN` (default) | Modes 0..=3 (the 120 fps RAW8 modes), then the soak chosen by `SOAK` |
| `PLAN` | `&FAST_CAPTURE_PLAN` | Capture only over modes 4..=11, 5 s each, then the detector check per mode |
| `SOAK` | `Soak::Frames` (default) | Frame soak: whole frames in PSRAM (`SOAK_PLAN`) |
| `SOAK` | `Soak::Strips` | Strip soak: 64-line strips in internal RAM (`STRIP_SOAK_PLAN`). Needs `RUN_BENCH = false` (checked at compile time) |
| `SOAK` | `Soak::None` | Capture only, then the detector check per mode |

`CAMERA_PLAN` also holds the sensor settings (exposure 800 lines, gain 0x20, continuous
clock, scene), the measurement window (5 s) and the `cache_check` and `still_dump` switches.

## What the firmware does

At boot, in order:

1. Switches the log UART to 2 Mbaud, then prints the chip revision, CPU clock, MAC, and the L1
   data and L2 cache sizes read back from the cache registers.
2. Brings up PSRAM through esp-hal, prints its size and bus clock, and runs a write/read-back
   pattern test over the last 4 MiB plus an aliasing check.
3. Measures one core's sequential read speed over 1 MiB of cold PSRAM and 128 KiB of internal
   RAM.
4. Starts I2C0 at 100 kHz, scans the bus and reads the OV9281 chip ID.
5. If `RUN_BENCH`: runs the detector benchmark on synthetic frames (`bench::run`).
6. If `RUN_CAMERA`: the camera section (`camera::run`). It turns on LDO VO3 at 2.5 V (the
   D-PHY supply), then for each mode of the plan:
   1. writes the mode's registers, the exposure (clamped to the mode's maximum and printed as
      applied), gain, and optionally the test pattern or the gated clock; reads them back;
   2. configures and starts the receiver, checks the lanes are in stop state, writes
      `STREAM_ON`, waits for the first frame (500 ms timeout, then the driver's checklist);
   3. measures 5 s of capture: frames, sizes against the configured frame length, error
      counters, and the measured frame rate next to the sensor's predicted one. Nothing is
      logged inside the window;
   4. optionally times the cache invalidation, checks cache coherence and dumps a still;
   5. runs the frame soak, or in a capture-only plan the detector check (below);
   6. writes `STREAM_OFF`, waits 20 ms, stops the receiver.

   With the strip soak selected, the whole-frame receiver is never created, and steps 3 and
   4 are skipped. Consecutive modes of one width share one strip receiver; at a change of
   width it is dropped and a new one is built on the same 320 KiB, split for that width.

The detector check (capture-only plans) runs after the measurement window, once per mode. The
detector processes one held captured frame: `process_frame` at threshold 128 for RAW8,
`process_frame_raw10` at 512 for RAW10. For a RAW10 mode it also processes a `Synth::fill_raw10`
frame of the mode's size with 16 discs, held in PSRAM and written back from the cache first.
Each call prints its blob count, the detector overflow and the time from the cycle counter.

| File | What |
|---|---|
| `src/main.rs` | Boot report: log baud switch, chip and cache info, PSRAM init and test, read-speed kernels, SCCB scan and chip ID. Pin and address choices, and the plans (see "Choosing what runs"), are constants at the top |
| `src/camera.rs` | Camera section: LDO VO3, sensor set-up over SCCB, the CSI receiver (`esp32p4-csi`) with 3 frame buffers at the start of PSRAM (each sized for 1280x800 RAW10), capture measurement, cache check, still dump, detector check, and the strip receiver for the strip soak |
| `src/soak.rs` | Frame soak: per mode, 30 s on live frames, then 10 s each on synthetic frames with 0, 16 and 64 discs, while capture keeps running; result table |
| `src/strip_soak.rs` | Strip soak: the same runs on 64-line strips in a 320 KiB ring in internal RAM (4 strips of 80 KiB at 1280 wide, 8 of 40 KiB at 640), with per-frame processing and latency, per-strip hold times, and the driver's interrupt and invalidation times |
| `src/bench.rs` | Detector benchmark on synthetic frames: 4 modes x 3 blob counts x up to 3 memory paths, 50 passes each |
| `src/vision.rs` | Blob detector (`core` only, no heap): threshold, 8-connected runs with union-find, weighted centroids in 1/32 px. `DefaultDetector` = 256 runs per row, 256 live components, 256 blobs, about 19.5 KB. RAW8 or packed RAW10 (`begin_frame_raw10`, `process_frame_raw10`); whole frame (`process_frame`) or strips (`begin_frame` / `push_rows` / `finish`) |
| `src/synth.rs` | Deterministic synthetic frames: noise 4..19, discs of radius 3..5 peaking at 250. API: `Synth::NEW` (keep it in a static, it holds a 3 KiB disc list), `Synth::fill(&mut self, buf, width, height, blobs, seed)` for RAW8 and `Synth::fill_raw10` for the same scene in packed RAW10 |
| `src/ov9281.rs` | OV9281 register tables for 12 modes, from the datasheet and the ESP-IDF, Linux, Raspberry Pi and Inno-Maker drivers. 0..=3: the four 120 fps RAW8 modes (HTS 728, VTS 915, 120.098 fps, 800 Mbit/s per lane), run on the board. 4..=8: faster RAW8 modes (up to 454 fps); 9..=11: RAW10. 4..=11: one first capture run (see "Fast modes and RAW10: first capture run") |
| `tools/still.py` | Decodes the still dumps of a log into PGM and PNG files |

## Measured results

All figures are from this board on 2026-11-10. The boot report and benchmark are from the
firmware as committed then; the capture results from commit `c0a70eb` and the soak from
`08b7bef`, both with frame buffers of 1 028 096 B (1280x800 RAW8; the buffers are now sized
for RAW10).

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
  [INFERENCE: buffer alignment; the buffers are not cache-line aligned].

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
- The buffers are not cache-line aligned. Between builds, with the buffers at other
  addresses, detector times moved by up to 2 % and the internal-RAM line-load figure by 11 %.

### Camera capture (whole frames in PSRAM)

Set-up: three frame buffers at the start of PSRAM (0x4800_0000), filled with the sentinel
0x5A before the first frame. Continuous clock lane, exposure 800 lines, gain 0x20, indoor
scene. In every mode, before stream-on, the clock and both data lanes were in stop state.

The sensor predicts 120.098 fps (HTS 728, VTS 915). Every mode measured 600 frames in 5003 ms
= **119.922 fps** (first-to-last 119.921–119.922). In every mode the following were all 0:
wrong size, dropped, overwritten, DMA errors, host errors (main and all 9 detail registers),
bridge errors.

| Mode | Readback 0x3808..0x380B | Bytes/frame (= w·h) | `CMPLTD_BLK_TFR_SIZE` |
|---|---|---|---|
| 1280x800 | 0500 x 0320 | 1 024 000 | 127 999 |
| 1280x720 crop | 0500 x 02d0 | 921 600 | 115 199 |
| 640x480 crop | 0280 x 01e0 | 307 200 | 38 399 |
| 640x400 2x2 binned | 0280 x 0190 | 256 000 | 31 999 |

- The −0.15 % against the predicted rate is [INFERENCE]: XVCLK is slightly under 24 MHz.
- The first frame's tail never held the sentinel: the DMA writes the whole frame. The
  block-size register under-reports by one item, so the driver takes the frame length from
  the DMA destination address instead.

Stills (dumped over the UART, decoded with `tools/still.py`):

| Still | Checked |
|---|---|
| 1280x800 scene (min 4, max 255, mean 41.0, 7.3 s to dump) | Office ceiling with suspended tiles: grid lines straight across the whole frame, sharp, no torn or wrapped lines |
| 1280x720 scene (mean 47.2) | Best pixel match inside the 1280x800 frame at offset (0, 40): the centred crop |
| 640x480 scene (min 5, max 83, mean 51.7) | Best match inside the 1280x800 frame at (320, 160), the exact centre, mean absolute difference 4.6 (noise level) |
| 640x400 scene (mean 75.0) | The whole field of view: correlation 0.991 with a 2x2 average of the 1280x800 frame at (0, 0). It is 1.83x brighter [INFERENCE: the binning sums in at least one direction] |
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
processed rate. Each mode's 5 s capture-only measurement before the soak: 600 frames,
119.922 fps, 0 errors. A first run, built against the in-progress strip driver, gave the same
verdicts and figures within about 1 % (live 640x400 aside, which depends on the scene).

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
  cache-line aligned.

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
whole frame's block sizes (the probation frame) before it delivers again. A stall therefore
costs about 4 frames (the damaged one, the shifted next one, the probation frame). The first
two frames after `start` are never delivered. Before this rule, stalls produced strips
labelled with rows of the shifted next frame.

How long the consumer may hold strips (10 s runs, no processing, every strip held for a fixed
time):

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

Method (`SOAK = Soak::Strips`, 2026-11-10, log `/tmp/poc-p4-eth/strip-soak4.log`):

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
  build leaves 62.7 KiB for the stack.

### Fast modes and RAW10: first capture run

One 5 s capture-only run per mode, with frames returned at once. Log:
`/tmp/poc-p4-eth/fast-capture.log`.

|Mode|Measured fps|Predicted fps|Bytes per frame|Result|
|---|---|---|---|---|
|4: 1280x800 RAW8, 96 MHz|143.907|144.118|1 024 000|no drops, no errors|
|5: 1280x720 RAW8|143.769|144.023|921 600|no drops, no errors|
|6: 640x400 RAW8|209.570|210.114|256 000|no drops, no errors|
|7: 640x400 RAW8|252.410|253.203|256 000|no drops, no errors|
|8: 320x200 RAW8|no frames|454.09|-|fails: 225 host errors (frame start/end pairing), not investigated|
|9: 1280x800 RAW10|119.921|120.098|1 280 000|no drops, no errors|
|10: 1280x720 RAW10|119.922|120.098|1 152 000|no drops, no errors|
|11: 640x400 RAW10|209.570|210.114|320 000|no drops, no errors|

Not done:

- Mode 8 not investigated.
- RAW10 byte layout not confirmed with the test pattern. The detector assumes CSI-2 packed
  RAW10; its 16 synthetic discs were found on the board in all three RAW10 modes.
- No stills of modes 4..=11 dumped or looked at.
- RAW10 through the strip path not run.
- The 640x400 modes run 0.26–0.31 % under prediction, against 0.15 % for the others; not
  investigated.
- Whole-frame detection from PSRAM takes longer than a frame in modes 4, 5, 9 and 10 (11.1,
  9.8, 14.1, 12.6 ms).

### PSRAM tuning attempts

| Setting | Result |
|---|---|
| 200 MHz (`PsramTimingParams::MHZ_200`) | PSRAM init fails: esp-hal 1.2.2 runs the PSRAM source clock (MPLL) at 500 MHz, which is not a multiple of 200. Needs a clock-tree change, not tried |
| L2 line size 128 B | No esp-hal config key on the P4; needs ROM or register calls. Not tried |
| L1/L2 autoload (prefetch) | No esp-hal config key on the P4; needs ROM or register calls. Not tried |

The firmware runs with the default: 250 MHz, L2 128 KiB with 64 B lines, no autoload.

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
- **CSI-2 link:** 2 lanes at 800 Mbit/s RAW8 with a continuous clock lane work in all four
  modes, free-running at 119.92 fps; the gated clock was not needed and not tried.
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
