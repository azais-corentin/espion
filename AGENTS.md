# AGENTS.md

Instructions for coding agents working in this repo. Read `docs/architecture.md` first.

## Repo map

| Path | What | Build / test (inside the dev shell) |
|---|---|---|
| `crates/espion-proto/` | no_std UDP wire format (serde + postcard), shared | `cargo test -p espion-proto` (root) |
| `firmware/cam-p4/` | ESP32-P4 firmware (camera, vision, XVS, LEDs, Ethernet/UDP, PTP, PPS capture via ETM) | `cargo build --release` in that directory |
| `firmware/esp32p4-csi/` | no_std MIPI CSI-2 driver for the P4 (esp-hal style, PAC-based) | `cargo build --release` in that directory; also built by `cam-p4` |
| `host/*` | Host apps: server, viewer, calib, sync | `cargo build --workspace`, `cargo test --workspace` (root) |
| `hardware/espion-cam/` | KiCad project. `SCHEMATIC.md` is the schematic's design (sheets, buses, pins, values); `BOM.md` the selected parts | KiCad; ERC: `kicad-cli sch erc hardware/espion-cam/espion-cam.kicad_sch` |
| `hardware/lib/` | Project-local symbols, footprints, 3D models; `3d-src/` holds the SolidCraft scripts the models are built from | KiCad; KLC checks and model rebuild in `hardware/lib/README.md` |
| `.omp/` | Agent config: the SolidCraft MCP server (`mcp.json`, may write only under `hardware/lib`) and its vendored skills | — |
| `docs/` | Architecture, wire protocol, timing | — |

- Enter the environment with `direnv allow` or `nix develop`. Don't install toolchains
  any other way.
- The firmware crates are **not** in the root workspace. Run cargo from their own
  directory so their `.cargo/config.toml` (target, runner) applies.
- One Rust version everywhere: the root `rust-toolchain.toml` (the flake uses it) and every
  `firmware/*/rust-toolchain.toml` must name the same channel.

## Firmware rules

- `no_std` + embassy (`esp-hal` + `esp-rtos`). No `std`, no blocking waits in async tasks.
- No heap. The P4 has no allocator at all. Per-frame and network data use static buffers
  (`static_cell`, `heapless`).
- The vision loop has an **8.3 ms budget per frame** (120 fps): capture to centroids queued
  for UDP. Keep per-pixel work branch-light and measure it on hardware when you change it.
- Timing-critical edges (XVS) come from RMT edge schedules, never from software delays.
  The PTP clock's PPS comes from the EMAC hardware ([docs/timing.md](docs/timing.md)).

## Wire format

- `crates/espion-proto` is the single source of truth for the UDP format. Any change bumps
  the protocol version and updates `docs/protocol.md` (layout and changelog) in the same
  commit.
- `cam-p4` and the host apps use `espion-proto` types. Don't define a second copy of a
  wire type.

## Dependencies

- Before adding a dependency, check that it supports `no_std` without `alloc` (for
  firmware and shared crates) and builds for `riscv32imafc-unknown-none-elf` (P4).
- Keep the esp-* versions in step (see the pin table in `docs/architecture.md`).
- If esp-hal lacks a peripheral driver or feature you need (e.g. P4 MIPI CSI, ISP, ETM,
  EMAC PTP), write it down in `docs/architecture.md` (gaps section) instead of guessing at
  an API. Work from the TRM and the `esp32p4` PAC.

## Hardware

- Never edit KiCad files (`.kicad_pro`, `.kicad_sch`, `.kicad_pcb`, `.kicad_sym`,
  footprints) by hand. Changes go through KiCad.
- `hardware/espion-cam/SCHEMATIC.md` is the plan the schematic follows. A schematic change
  updates it (sheets, buses, hierarchical pins, parts, values) in the same commit, and
  `BOM.md` and the board list in `hardware/espion-cam/README.md` when a part changes. Keep
  ERC at 0 violations.
- Library parts follow `hardware/lib/README.md`: KLC naming, `${KIPRJMOD}`-relative paths,
  a Parts entry per part (say there when a board stops using it), and the KLC check
  scripts. `hardware/lib/.gitattributes` keeps the library files at LF line endings.
- 3D models are built with SolidCraft from the manufacturer's drawings, with the script in
  `hardware/lib/3d-src/`. Don't use EasyEDA/LCSC models (their terms forbid it). When you
  bump the `solidcraft` flake input, re-copy the skills (`.omp/skills/VENDORED.md`).
- Hardware is CERN-OHL-P-2.0 (`hardware/LICENSE`); code is MIT OR Apache-2.0.

## Hygiene

- Never commit generated files or build outputs (`target/`, Gerbers/BOM exports, KiCad
  backups, caches and `.history/`, `result*`). Never commit `.envrc.local`. The STEP models
  in `hardware/lib/*.3dshapes/` are the exception: commit them with their `3d-src` script.
- Keep changes small and focused.
- Before finishing, for every affected part:
  - `cargo fmt` (root for the workspace, and in each touched firmware crate)
  - `cargo clippy` (`--workspace --all-targets` at the root; plain `cargo clippy --release`
    in a firmware crate)
  - the build (and `cargo test --workspace` for host/shared crates)
  - for hardware: ERC on the schematic, and the KLC scripts on changed library parts
