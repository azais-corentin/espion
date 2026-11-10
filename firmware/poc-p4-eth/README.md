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

Status: question 2 is answered on synthetic frames (below). Camera capture over CSI is not in
this firmware yet.

## Hardware set-up

| Part | What |
|---|---|
| Board | [Waveshare ESP32-P4-ETH](https://docs.waveshare.com/ESP32-P4-ETH): ESP32-P4NRW32X (32 MB PSRAM in package), 32 MB flash, CH343P USB-UART bridge on the USB-C port |
| Camera | SZYTF WX-1M-9281-79 OV9281 module, Raspberry-Pi-style 15-pin FFC (pin table in [docs/hardware/camera.md](../../docs/hardware/camera.md)) |
| Cable | 22-pin to 15-pin FFC into the board's 22-pin MIPI-CSI connector J1 [INFERENCE: Pi-style mapping, J1 pin 6 to module pin 11] |
| Host link | USB-C to the CH343, `/dev/ttyACM0` (UART0, 115200 baud) |

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
timeout 100 espflash monitor --chip esp32p4 --port /dev/ttyACM0 --non-interactive --no-addresses --elf "$E" \
  > boot.log 2>&1          # exits 124 when timeout stops it: expected
sed 's/\x1b\[[0-9;]*m//g' boot.log | tr -d '\r'
```

- `monitor` resets the chip through the CH343's DTR/RTS on connect, so the log starts at the
  ROM banner. No BOOT or RESET press is needed.
- The boot report is out within about 1 s of reset; the detector benchmark takes about 90 s
  more, hence the 100 s timeout.
- `--no-addresses` stops espflash from printing symbol lines for hex numbers in the log.
- `cargo run --release` uses the runner `espflash flash --monitor ...`, which is interactive.

## What the firmware does

At boot, in order:

1. Prints the chip revision, CPU clock, MAC, and the L1 data and L2 cache sizes read back from
   the cache registers.
2. Brings up PSRAM through esp-hal, prints its size and bus clock, and runs a write/read-back
   pattern test over the last 4 MiB plus an aliasing check.
3. Measures one core's sequential read speed over 1 MiB of cold PSRAM and 128 KiB of internal
   RAM.
4. Starts I2C0 at 100 kHz, scans the bus and reads the OV9281 chip ID.
5. Runs the detector benchmark on synthetic frames (`bench::run`).

| File | What |
|---|---|
| `src/main.rs` | Boot report: chip and cache info, PSRAM init and test, read-speed kernels, SCCB scan and chip ID. Pin and address choices are constants at the top |
| `src/bench.rs` | Detector benchmark on synthetic frames: 4 modes x 3 blob counts x up to 3 memory paths, 50 passes each |
| `src/vision.rs` | Blob detector (`core` only, no heap): threshold, 8-connected runs with union-find, weighted centroids in 1/32 px. `DefaultDetector` = 256 runs per row, 256 live components, 256 blobs, 19 520 B. Whole frame (`process_frame`) or strips (`begin_frame` / `push_rows` / `finish`) |
| `src/synth.rs` | Deterministic synthetic RAW8 frames: noise 4..19, discs of radius 3..5 peaking at 250. API: `Synth::NEW` (keep it in a static, it holds a 3 KiB disc list) and `Synth::fill(&mut self, buf, width, height, blobs, seed)` |
| `src/ov9281.rs` | OV9281 register tables for the four modes (HTS 728, VTS 915, 120.098 fps, 800 Mbit/s per lane), from the datasheet and the ESP-IDF, Linux and Raspberry Pi drivers. Not used yet; nothing in it has run on hardware |

## Measured results

All figures are from this board on 2026-11-10, with the firmware as committed.

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
  or strip before reading it; that cost is not measured.
- The strip path is a stand-in. Strip capture is not implemented: DMA set-up and per-strip
  interrupt latency are not included, and whether the CSI DMA can fill a ring of internal
  strips is still to be confirmed.
- Synthetic frames: a dark background (4..19) and round discs. Real frames with brighter
  backgrounds take the detector's per-pixel path more often [INFERENCE].
- The buffers are not cache-line aligned. Between builds, with the buffers at other
  addresses, detector times moved by up to 2 % and the internal-RAM line-load figure by 11 %.

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
  This firmware does not set VO3 yet; the CSI driver has to.
- **L2 cache:** the bootloader bundled with espflash 4.7.0 sets the L2 cache to 128 KiB, not the
  256 KiB esp-hal assumes by default (`ESP_HAL_CONFIG_L2_CACHE_SIZE`). This crate sets
  `"128KB"` in `.cargo/esp-config.toml` and gets 128 KiB more internal RAM. `cam-p4` still uses
  the default, which is safe but wastes that RAM.
- **PSRAM read ceiling:** about 163 MB/s for one core reading cold PSRAM with no other work,
  and 120 MB/s when every word is summed. [docs/architecture.md](../../docs/architecture.md)
  assumes about 200 MB/s. In the detector the PSRAM path runs at about 104 MB/s, which puts
  both 1280-wide modes over the 120 fps budget when the frame is read from PSRAM. Only the
  internal-strip path brings them under it.
