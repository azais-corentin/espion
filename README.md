# espion

Multi-camera IR marker tracking. Each camera board triggers its global-shutter sensor at
the same instant as every other board, finds the IR blobs at 120 fps, computes subpixel
centroids and streams them over Ethernet (UDP). The host triangulates 3D marker positions,
runs calibration and coordinates time sync.

**Status:** firmware and host apps are stubs that build. The camera board's schematic is
captured in KiCad (hierarchical, wired, ERC clean); PCB layout has not started.

## Hardware

A custom KiCad baseboard for a Waveshare ESP32-P4-Core module (ESP32-P4 rev v3, 32 MB
in-package PSRAM, 16 MB flash). The P4 does the vision: it captures an OV9281 (1 MP
monochrome global shutter, MIPI CSI-2, 640×400 binned at 120 fps) triggered on its XVS pin,
and runs blob detection. An 850 nm LED ring (12 × 90° LEDs, 1 A pulses) lights passive
retroreflective markers at room scale (2–5 m). It is gated by the sensor's STROBE output,
with a P4 enable line for dark frames. The P4's own Ethernet MAC with an IP101GRI PHY
(100 Mbit/s) sends UDP to the host and keeps the boards' clocks in sync with PTP (IEEE 1588,
hardware timestamps; target: about 1 µs). The boards elect the PTP grandmaster among
themselves, with automatic failover. The board is powered over PoE (802.3af) or USB-C at
5 V; USB-C also flashes and debugs the P4.

## Layout

| Path | Content |
|---|---|
| `crates/espion-proto/` | no_std UDP wire format shared by firmware and host |
| `firmware/cam-p4/` | ESP32-P4: camera, blob detection, XVS trigger, LED gating, Ethernet/UDP, PTP sync |
| `firmware/esp32p4-csi/` | no_std MIPI CSI-2 receiver driver for the P4 (esp-hal lacks one) |
| `host/espion-server/` | UDP receive + triangulation (CLI) |
| `host/espion-viewer/` | egui live view |
| `host/espion-calib/` | Intrinsics + extrinsics calibration |
| `host/espion-sync/` | Time-sync coordinator |
| `hardware/` | KiCad project (`espion-cam/`) and local libraries with their 3D models (`lib/`) |
| `docs/` | [architecture](docs/architecture.md), [protocol](docs/protocol.md), [timing](docs/timing.md), [board design](docs/hardware/README.md) |

## Dev environment

Needs Nix with flakes. The dev shell provides Rust 1.99.1 with the host and
`riscv32imafc-unknown-none-elf` (P4) targets, espflash, probe-rs, KiCad 10 and SolidCraft
(builds the library parts' 3D models).

```sh
direnv allow        # or: nix develop
```

Local overrides (e.g. `ESPFLASH_PORT`) go in `.envrc.local`, which is git-ignored.

## Build, flash, run

Host workspace (repo root; includes `espion-proto`):

```sh
cargo build --workspace
cargo test --workspace
cargo run -p espion-server   # TODO: stub, does nothing yet
cargo run -p espion-viewer   # TODO: stub
cargo run -p espion-calib    # TODO: stub
cargo run -p espion-sync     # TODO: stub
```

Firmware (own target, runner and lockfile; run from its directory):

```sh
cd firmware/cam-p4 && cargo build --release   # also builds firmware/esp32p4-csi

# TODO: untested (no board yet). The P4's USB Serial/JTAG shows up on the board's USB-C
# port. Flashes and opens the monitor:
cd firmware/cam-p4 && ESPFLASH_PORT=/dev/ttyACM0 cargo run --release
```

Hardware:

```sh
kicad hardware/espion-cam/espion-cam.kicad_pro
kicad-cli sch erc hardware/espion-cam/espion-cam.kicad_sch

# Rebuild a library part's 3D model from its SolidCraft script:
solidcraft-cli run hardware/lib/3d-src/<Footprint>.json --out hardware/lib/<Library>.3dshapes/<Footprint>.step
```

Library conventions and KLC checks: [hardware/lib/README.md](hardware/lib/README.md).

## License

Code: [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
Hardware (`hardware/`): [CERN-OHL-P-2.0](hardware/LICENSE).
