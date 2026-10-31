# AGENTS.md

Instructions for coding agents working in this repo. Read `docs/architecture.md` first.

## Repo map

| Path | What | Build / test (inside the dev shell) |
|---|---|---|
| `crates/espion-proto/` | no_std UDP wire format (serde + postcard), shared | `cargo test -p espion-proto` (root) |
| `firmware/cam-p4/` | ESP32-P4 firmware (camera, vision, XVS, LEDs, Ethernet/UDP, PTP, PPS capture via ETM) | `cargo build --release` in that directory |
| `firmware/esp32p4-csi/` | no_std MIPI CSI-2 driver for the P4 (esp-hal style, PAC-based) | `cargo build --release` in that directory; also built by `cam-p4` |
| `host/*` | Host apps: server, viewer, calib, sync | `cargo build --workspace`, `cargo test --workspace` (root) |
| `hardware/espion-cam/` | KiCad project; its design (decisions, values, layout constraints, firmware rules) is in `docs/hardware/`; the BOM is exported from the schematic with the `Order` preset | KiCad toolkit: `kicad_check` (ERC, drawing rules, connectivity, footprints, fields, decoupling, pulls, BOM lines, rules; on the board `drc`, track widths, return paths and vias, decoupling and footprint placement, `layout` constraints); board set-up and layout with the `kicad_edit` board ops (stack-up, board rules, outline, update from schematic, placement, tracks, diff pairs, vias, zones, rule areas, board text); fab outputs (Gerbers, JLCPCB BOM/CPL, hand-assembly list from `assembly.hand` in `.omp/kicad.json`, assembly drawings, STEP): `kicad_export`; BOM: `kicad_query` `bom` (preset `Order`) |
| `hardware/lib/` | Project-local symbols, footprints, 3D models; `3d-src/` holds the SolidCraft scripts the models are built from | KiCad toolkit: `kicad_edit` library ops, `kicad_check` `klc`; model rebuild in `hardware/lib/README.md` |
| `.omp/` | Agent config: the KiCad toolkit extension (`extensions/kicad/`, settings in `kicad.json`, including the board's `layout` constraints) and its `kicad-*` skills (including `kicad-pcb-layout` and `kicad-pcb-review` for the board); the SolidCraft MCP server (`mcp.json`, may write only under `hardware/lib`) and its `solidcraft-*` skills, which the dev shell links from the flake input (`solidcraftSkills` in `flake.nix`) | `bun test` in `.omp/extensions/kicad` |
| `docs/` | Architecture, wire protocol, timing; `hardware/`: the camera board's design (per block, layout, firmware rules) | — |

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

- KiCad files (`.kicad_pro`, `.kicad_sch`, `.kicad_pcb`, `.kicad_dru`, `.kicad_sym`,
  `.kicad_mod`, the lib tables) are **toolkit-only for changes**. Change and check them only
  with the KiCad toolkit: `kicad_query`, `kicad_edit`, `kicad_check`, `kicad_render`,
  `kicad_export` for the fab outputs, and `kicad_datasheet` for the datasheets (skills
  `kicad-schematic-edit`, `kicad-schematic-review`, `kicad-pcb-layout`, `kicad-pcb-review`,
  `kicad-part-sourcing`, `kicad-library-part`, `kicad-library-review`). Reading them with
  `read`/`grep` is allowed, but prefer the queries: they answer with nets, pins and
  positions KiCad computes. Never write them with other tools (write, edit, shell, eval)
  and never run `kicad-cli`, `kicad`, `eeschema` or `pcbnew` yourself; a hook blocks the
  shell and file tools, and eval is on you. If the toolkit can't do something, stop, tell
  the user what is missing and use the `ask` tool to ask whether it should be added to the
  toolkit.
- `kicad_edit` checks for KiCad's lock files itself: it ignores a stale one (this host and
  user, no KiCad running) and refuses while KiCad has the project open. Then ask the user
  to close it; never delete lock files.
- The schematic is the source of truth for connections and parts; `docs/hardware/` holds
  what it can't: decisions, derived values, accepted risks, layout constraints and firmware
  rules. A schematic change updates `docs/hardware/` in the same commit when it changes one
  of those or a part, and the board list in `hardware/espion-cam/README.md` when a part
  changes. Keep ERC at 0 violations.
- There is no hand-kept BOM file. Every sourced symbol carries Manufacturer, MPN and LCSC
  fields; set them when you add or change a part. The BOM comes from the project's `Order`
  preset and is never committed (see Hygiene).
- Library parts follow `hardware/lib/README.md`: KLC naming, `${KIPRJMOD}`-relative paths,
  a Parts entry per part (say there when a board stops using it), and the KLC checks
  (`kicad_check` with `klc`). `hardware/lib/.gitattributes` keeps the library files at LF
  line endings.
- The KLC scripts and the official symbol/footprint generators come from the
  `kicad-library-tools` flake input. Its `library_utils/` layout may still move: after
  bumping it, run the toolkit's tests.
- 3D models are built with SolidCraft from the manufacturer's drawings, with the script in
  `hardware/lib/3d-src/`. Don't use EasyEDA/LCSC models (their terms forbid it). The
  SolidCraft skills follow the `solidcraft` flake input: after bumping it, re-enter the dev
  shell and restart omp.
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
  - for hardware: `kicad_check` (ERC, drawing rules, connectivity, footprints, fields,
    decoupling, pulls, BOM lines, rules, DRC with schematic parity, and for a board change
    track widths, return paths, return vias, decoupling and footprint placement and the
    `layout` constraints), with `klc` for changed library parts
  - for the KiCad toolkit: `bun test` in `.omp/extensions/kicad`
