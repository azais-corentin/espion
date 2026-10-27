# Architecture

espion tracks IR-reflective or IR-emitting markers with several camera boards. Each board
exposes all cameras at the same instant, finds the bright blobs in every frame, and
streams their subpixel centroids to the host over Wi-Fi. The host triangulates 3D marker
positions.

```mermaid
flowchart LR
  subgraph board["Camera board (×N)"]
    S[OV9281<br/>global shutter, mono] -- "MIPI CSI-2<br/>2 lanes, RAW8" --> P4[ESP32-P4<br/>capture + blob detection]
    P4 -- "XVS trigger" --> S
    S -- STROBE --> G{{LED gate}}
    P4 -- "LED_EN / LED_FORCE" --> G
    G --> L[850 nm LED ring]
    P4 -- "SPI (P4 master)" --> C5[ESP32-C5<br/>Wi-Fi, UDP, FTM sync]
    C5 -- "DATA_READY, PPS" --> P4
  end
  C5 -- "UDP: espion-proto" --> AP[(Wi-Fi AP)]
  C5 <-. "FTM (802.11mc)" .-> M[master board's C5<br/>FTM responder]
  AP --> H[host:<br/>espion-server, viewer,<br/>calib, sync]
```

## Camera board

| Part | Role |
|---|---|
| ESP32-P4 (silicon **rev v3.0 or later**: esp-hal does not support older revisions) | Sensor control, MIPI CSI capture, blob detection, XVS trigger, LED gating, SPI master |
| OV9281 | 1 MP monochrome global-shutter sensor, MIPI CSI-2 2-lane. Mode: 640×400 2×2 binned, RAW8, 120 fps, external trigger on XVS, STROBE output |
| ESP32-C5 | Wi-Fi co-processor (dual-band, Wi-Fi 6): UDP to the host, FTM clock sync, PPS to the P4, SPI slave |
| 850 nm LED ring + driver | Illumination, on only while `(STROBE ∧ LED_EN) ∨ LED_FORCE` |
| USB-C | 5 V power, flashing and logs |

### Board signals

| Signal | Direction | Purpose |
|---|---|---|
| MIPI CSI-2 (CLK + 2 data lanes) | OV9281 → P4 | Pixel data |
| SCCB (I2C) | P4 → OV9281 | Sensor registers |
| XVS | P4 → OV9281 | Frame trigger, one edge per frame at a scheduled network time |
| STROBE | OV9281 → LED gate | High during exposure |
| LED_EN, LED_FORCE | P4 → LED gate | Enable strobed LEDs / force them on (bring-up, focus) |
| SPI (SCLK, MOSI, MISO, CS) | P4 (master) ↔ C5 (slave) | `p4-c5-link` messages |
| DATA_READY | C5 → P4 | C5 has a message queued for the P4 |
| PPS | C5 → P4 | Pulse at whole seconds of network time ([timing.md](timing.md)) |

Open hardware questions (to settle in the KiCad project):
- How the C5 gets flashed on a board with a single USB-C port (USB hub to both chips' USB Serial/JTAG, test pads, or P4 pass-through).
- The LED gate implementation and the eye-safety limits of the 850 nm ring (IEC 62471 exposure at the expected distance and duty cycle).

## Data flow per frame

```mermaid
sequenceDiagram
  participant P4
  participant S as OV9281
  participant C5
  participant H as Host
  P4->>S: XVS edge at network time T(k)
  S-->>S: expose (STROBE high, LEDs on)
  S->>P4: frame k over MIPI CSI (DMA into a frame buffer)
  P4->>P4: threshold, connected components, weighted centroids (≤ 8.3 ms)
  P4->>C5: SPI: frame k, capture time, centroids
  C5->>H: UDP: espion-proto Centroids packet
  H->>H: group by frame k across boards, triangulate
```

- **Frame numbers agree across boards by construction**: frame `k` is the frame triggered
  at network time `T(k) = T0 + ⌊k · 10⁹ / 120⌋ ns`. Each packet carries `k` and the capture
  time ([timing.md](timing.md)).
- **Vision budget**: 1/120 s = 8.3 ms per frame, end to end on the P4 (capture DMA runs in
  parallel with processing of the previous frame).
- **Frame buffers**: 640 × 400 × 1 byte = 250 KiB per frame. Two buffers fit in the P4's
  internal L2 memory; PSRAM is the fallback. No heap on the P4.

## Firmware

| Crate | Chip / target | Runs |
|---|---|---|
| `firmware/cam-p4` | ESP32-P4, `riscv32imafc-unknown-none-elf` | One core runs the vision loop. The other runs embassy tasks: sensor control over SCCB, the XVS edge schedule (RMT), the PPS servo, the SPI link and the LED gate |
| `firmware/radio-c5` | ESP32-C5, `riscv32imac-unknown-none-elf` | Embassy tasks: Wi-Fi station and UDP (embassy-net), FTM sync (initiator, or responder on the elected master), PPS output (RMT edge schedule), SPI slave. Heap only for the Wi-Fi stack |
| `firmware/p4-c5-link` | both (no_std) | SPI message types and framing. Reuses `espion-proto` types |

Both firmware crates are built with `esp-hal` + `esp-rtos` (embassy executor). Time sync
reuses the approach and code from the private
[esp-wifi-sync](https://github.com/azais-corentin/esp-wifi-sync) experiments
([timing.md](timing.md)).

## Host

| App | Role |
|---|---|
| `espion-server` | CLI. Receives UDP packets, groups centroids by frame number, triangulates with the current calibration |
| `espion-viewer` | egui live view: per-camera blobs, 3D markers, sync health |
| `espion-calib` | Per-camera intrinsics (lens model) and rig extrinsics |
| `espion-sync` | Time-sync coordinator. The network clock lives on the elected master C5, not on the host. espion-sync selects the master, sets the frame epoch `T0`, maps network time to host time and reports per-board sync health |

`crates/espion-proto` is the single source of truth for the UDP wire format
([protocol.md](protocol.md)).

## Pinned versions

Checked on 2026-10-27. The firmware crates commit their `Cargo.lock`.

| Component | Version | Notes |
|---|---|---|
| Rust | 1.99.1 (stable) | `rust-toolchain.toml`; esp-hal MSRV is 1.95 |
| esp-hal | ~1.2.2 | `unstable` feature needed for RMT, SPI slave, DMA, PSRAM |
| esp-rtos | 0.4.0 | embassy integration (replaces esp-hal-embassy) |
| esp-radio | =1.0.0-beta.1 | C5 Wi-Fi. 1.0.0-beta.2 is yanked on crates.io |
| esp-wifi-sys-esp32c5 | 0.3.0, vendored | Wi-Fi libraries replaced with esp32-wifi-lib `9e91edc` (FTM fix), see `firmware/vendor/esp-wifi-sys-esp32c5/VENDORED.md` |
| esp-bootloader-esp-idf | 0.6.0 | App descriptor for the ESP-IDF 2nd-stage bootloader |
| esp-println / esp-backtrace / esp-alloc | 0.18.0 / 0.20.0 / 0.11.0 | |
| esp32p4 (PAC) | 0.3.x | Same major as esp-hal 1.2.2, for the in-repo CSI driver |
| embassy-executor / embassy-time / embassy-net | 0.10.0 / 0.5.1 / 0.9.1 | |
| serde / postcard | 1.0.229 / 1.1.3 | `default-features = false` on firmware |
| espflash / probe-rs | 4.6.0 / 0.32.0 | From nixpkgs (flake.lock) |

## esp-hal support and gaps

Status from the esp-hal 1.2.2 support table and `esp-metadata` (✔ supported, ⚒ partial,
❌ not supported). Only peripherals espion needs are listed.

### ESP32-P4

| Need | esp-hal 1.2.2 | Plan |
|---|---|---|
| MIPI CSI-2 receiver | ❌ | In-repo driver on the `esp32p4` PAC. Write it like an esp-hal driver (`Config`, typed pins, DMA buffers, blocking and async APIs, HIL-testable) without planning to upstream it. Planned as its own crate under `firmware/` |
| ISP | ❌ | Bypass: RAW8 goes straight to memory. Blob detection does not need demosaicing (mono sensor). Check the CSI → memory path that skips the ISP against the TRM |
| DMA for CSI | ⚒ (DMA partial) | Confirm which DMA engine the CSI bridge uses on the P4 and whether esp-hal drives it; otherwise part of the CSI driver |
| I2C master (SCCB) | ✔ | — |
| SPI master | ✔ | P4 side of the link |
| RMT | ⚒ | XVS edge schedule, as in esp-wifi-sync `wave.rs` |
| GPIO interrupts | ✔ | PPS edge timestamping (interrupt latency adds jitter) |
| ETM | ❌ | Would allow hardware PPS capture (GPIO event → timer capture task). PAC-level fallback if interrupt jitter is too high |
| MCPWM / PCNT / LEDC | ❌ | Not needed: STROBE gates the LEDs |
| SYSTIMER, TIMG | ⚒ | Local timebase, embassy time driver |
| PSRAM | ⚒ | Fallback for frame buffers |
| USB Serial/JTAG | ⚒ | Flashing and logs |
| Chip revision | v3.0+ only | The PCB must use rev v3.x silicon |

Tooling: esp-generate 1.4.0 has no ESP32-P4 template. `cam-p4` was adapted from the C5
template and the esp-hal 1.2.2 examples.

### ESP32-C5

| Need | esp-hal / esp-radio | Plan |
|---|---|---|
| Wi-Fi station, 2.4/5 GHz | ⚒ (esp-radio 1.0.0-beta.1) | — |
| SoftAP FTM responder | Not exposed: esp-radio hard-codes `ftm_responder = false` and leaves FTM out of `g_wifi_feature_caps` | Set both through `esp-wifi-sys-esp32c5` bindings, as esp-wifi-sync does |
| FTM initiator | Libraries on crates.io (esp32-wifi-lib `7cd4d7b`) lack the t2/t3 validation fix that made C3↔C3 FTM work | Vendored `9e91edc` libraries |
| FTM timestamps vs SYSTIMER | On the C3 the MAC clock is SYSTIMER µs. **Unverified on the C5** | Measure before relying on it |
| SPI slave | ⚒ (SPI2 only) | C5 side of the link |
| RMT | ⚒ | PPS edge schedule |
| ETM / MCPWM / LEDC | ❌ | Not needed |
| UDP (embassy-net / smoltcp) | ✔ (crate level) | — |
