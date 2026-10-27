# AGENTS.md

Instructions for coding agents working in this repo. Read `docs/architecture.md` first.

## Repo map

| Path | What | Build / test (inside the dev shell) |
|---|---|---|
| `crates/espion-proto/` | no_std UDP wire format (serde + postcard), shared | `cargo test -p espion-proto` (root) |
| `firmware/p4-c5-link/` | no_std P4↔C5 SPI protocol; host workspace member for tests | `cargo test -p p4-c5-link` (root); also built by both firmware crates |
| `firmware/cam-p4/` | ESP32-P4 firmware (camera, vision, XVS, LEDs, SYNC capture via ETM) | `cargo build --release` in that directory |
| `firmware/esp32p4-csi/` | no_std MIPI CSI-2 driver for the P4 (esp-hal style, PAC-based) | `cargo build --release` in that directory; also built by `cam-p4` |
| `firmware/radio-c5/` | ESP32-C5 firmware (Wi-Fi, UDP, master election, FTM sync, SYNC) | `cargo build --release` in that directory |
| `firmware/vendor/` | Patched `esp-wifi-sys-esp32c5` (see its `VENDORED.md`) | Built through `radio-c5` |
| `host/*` | Host apps: server, viewer, calib, sync | `cargo build --workspace`, `cargo test --workspace` (root) |
| `hardware/espion-cam/` | KiCad project | KiCad only |
| `hardware/lib/` | Project-local symbols, footprints, 3D models | KiCad only |
| `docs/` | Architecture, wire protocol, timing | — |

- Enter the environment with `direnv allow` or `nix develop`. Don't install toolchains
  any other way.
- The firmware crates are **not** in the root workspace. Run cargo from their own
  directory so their `.cargo/config.toml` (target, runner) applies.
- One Rust version everywhere: the root `rust-toolchain.toml` (the flake uses it) and both
  `firmware/*/rust-toolchain.toml` must name the same channel.

## Firmware rules

- `no_std` + embassy (`esp-hal` + `esp-rtos`). No `std`, no blocking waits in async tasks.
- No heap in hot paths. The P4 has no allocator at all. On the C5 the heap exists only for
  the Wi-Fi stack. Per-frame data uses static buffers (`static_cell`, `heapless`).
- The vision loop has an **8.3 ms budget per frame** (120 fps): capture to centroids sent
  over SPI. Keep per-pixel work branch-light and measure it on hardware when you change it.
- Timing-critical edges (SYNC, XVS) come from RMT edge schedules, never from software
  delays ([docs/timing.md](docs/timing.md)).

## Wire format

- `crates/espion-proto` is the single source of truth for the UDP format. Any change bumps
  the protocol version and updates `docs/protocol.md` (layout and changelog) in the same
  commit.
- `p4-c5-link` reuses `espion-proto` types. Don't define a second copy of a wire type.

## Dependencies

- Before adding a dependency, check that it supports `no_std` (for firmware and shared
  crates) and builds for `riscv32imafc-unknown-none-elf` (P4) and/or
  `riscv32imac-unknown-none-elf` (C5).
- Keep the esp-* versions in step (see the pin table in `docs/architecture.md`).
  `esp-radio` is pinned to `=1.0.0-beta.1` because beta.2 is yanked.
- If esp-hal lacks a peripheral driver you need (e.g. P4 MIPI CSI, ISP, ETM), write it down
  in `docs/architecture.md` (gaps section) instead of guessing at an API. Work from the
  TRM and the `esp32p4`/`esp32c5` PACs.

## Hardware

- Never edit KiCad files (`.kicad_pro`, `.kicad_sch`, `.kicad_pcb`, `.kicad_sym`,
  footprints) by hand. Changes go through KiCad.
- Hardware is CERN-OHL-P-2.0 (`hardware/LICENSE`); code is MIT OR Apache-2.0.

## Hygiene

- Never commit generated files or build outputs (`target/`, Gerbers/BOM exports, KiCad
  backups and caches, `result*`). Never commit `.envrc.local`.
- Keep changes small and focused.
- Before finishing, for every affected part:
  - `cargo fmt` (root for the workspace, and in each touched firmware crate)
  - `cargo clippy` (`--workspace --all-targets` at the root; plain `cargo clippy --release`
    in a firmware crate)
  - the build (and `cargo test --workspace` for host/shared crates)
