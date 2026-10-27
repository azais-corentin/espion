# espion

Multi-camera IR marker tracking. Each camera board triggers its global-shutter sensor at
the same instant as every other board, finds the IR blobs at 120 fps, computes subpixel
centroids and streams them over Wi-Fi (UDP). The host triangulates 3D marker positions,
runs calibration and coordinates time sync.

**Status:** repository skeleton. Firmware and host apps are stubs that build; the PCB is not
started.

## Hardware

A custom KiCad board with three main parts. An ESP32-P4 does the vision: it captures an
OV9281 (1 MP monochrome global shutter, MIPI CSI-2, 640×400 binned at 120 fps) triggered on
its XVS pin, and runs blob detection. An 850 nm LED ring is gated by the sensor's STROBE
output, with P4 enable/force lines. An ESP32-C5 is the Wi-Fi co-processor on SPI: it sends
UDP to the host and keeps the boards' clocks in sync over Wi-Fi FTM (target: about 1 µs).
One board's C5 is the elected network clock. The board is powered by USB-C at 5 V.

## Layout

| Path | Content |
|---|---|
| `crates/espion-proto/` | no_std UDP wire format shared by firmware and host |
| `firmware/cam-p4/` | ESP32-P4: camera, blob detection, XVS trigger, LED gating |
| `firmware/radio-c5/` | ESP32-C5: Wi-Fi, UDP, FTM clock sync, PPS to the P4 |
| `firmware/p4-c5-link/` | no_std SPI protocol between the two chips |
| `firmware/vendor/` | Patched `esp-wifi-sys-esp32c5` (FTM-fixed Wi-Fi libraries) |
| `host/espion-server/` | UDP receive + triangulation (CLI) |
| `host/espion-viewer/` | egui live view |
| `host/espion-calib/` | Intrinsics + extrinsics calibration |
| `host/espion-sync/` | Time-sync coordinator |
| `hardware/` | KiCad project (`espion-cam/`) and local libraries (`lib/`) |
| `docs/` | [architecture](docs/architecture.md), [protocol](docs/protocol.md), [timing](docs/timing.md) |

## Dev environment

Needs Nix with flakes. The dev shell provides Rust 1.99.1 with the host,
`riscv32imafc-unknown-none-elf` (P4) and `riscv32imac-unknown-none-elf` (C5) targets,
espflash, probe-rs and KiCad.

```sh
direnv allow        # or: nix develop
cp .envrc.local.example .envrc.local   # Wi-Fi/host settings for radio-c5 (git-ignored)
```

## Build, flash, run

Host workspace (repo root; includes `espion-proto` and `p4-c5-link`):

```sh
cargo build --workspace
cargo test --workspace
cargo run -p espion-server   # TODO: stub, does nothing yet
cargo run -p espion-viewer   # TODO: stub
cargo run -p espion-calib    # TODO: stub
cargo run -p espion-sync     # TODO: stub
```

Firmware (each crate has its own target, runner and lockfile; run from its directory):

```sh
cd firmware/cam-p4   && cargo build --release
cd firmware/radio-c5 && cargo build --release

# TODO: untested (no board yet). Flashes over USB with espflash and opens the monitor:
cd firmware/cam-p4   && cargo run --release
cd firmware/radio-c5 && cargo run --release   # TODO: how the C5 is reached on the board is open
```

Hardware:

```sh
kicad hardware/espion-cam/   # TODO: project not created yet
```

## License

Code: [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
Hardware (`hardware/`): [CERN-OHL-P-2.0](hardware/LICENSE).
