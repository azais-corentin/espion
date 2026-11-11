# AGENTS.md

Instructions for coding agents working in this repo. Read `docs/architecture.md` first.

## Repo map

| Path | What | Build / test (inside the dev shell) |
|---|---|---|
| `crates/espion-proto/` | no_std UDP wire format (serde + postcard), shared. The camera stream (protocol version 3: requests, state, raw and JPEG frame chunks) is implemented and used by `poc-p4-stream` and `poc-viewer`; the tracking messages are a draft | `cargo test -p espion-proto` (root); `cargo build -p espion-proto --target riscv32imafc-unknown-none-elf` |
| `firmware/cam-p4/` | ESP32-P4 firmware (camera, vision, XVS, LEDs, Ethernet/UDP, PTP, PPS capture via ETM) | `cargo build --release` in that directory |
| `firmware/esp32p4-cache/` | no_std cache maintenance for the P4 (ROM invalidate and write-back behind one cross-core spinlock taken with interrupts masked) and the prefetch set-up (L1 data and L2 autoload, manual L1 data preload, PAC-based); no esp-hal, esp-rtos or critical section, usable from a bare second core. Every cache operation of our crates goes through it; esp-hal's own cache calls are not covered | `cargo build --release` in that directory; also built by `esp32p4-csi` and `esp32p4-jpeg` (so by `cam-p4`, `poc-p4-eth`, `poc-p4-stream`) |
| `firmware/esp32p4-csi/` | no_std MIPI CSI-2 driver for the P4 (esp-hal style, PAC-based) | `cargo build --release` in that directory; also built by `cam-p4` |
| `firmware/esp32p4-jpeg/` | no_std hardware JPEG encoder driver for the P4 (greyscale, baseline) with the 2D-DMA it needs (esp-hal style, PAC-based) | `cargo build --release` in that directory; also built by `poc-p4-eth` (its `JPEG_PLAN` tests and measures it on the board) and `poc-p4-stream` |
| `firmware/esp32p4-perf/` | no_std performance counters of the P4 HP cores (`mcycle`, `minstret`, the three event counters, the L1/L2 cache counters), raw CSR and register reads, no esp dependency, usable from a bare second core | `cargo build --release` in that directory; also built by `poc-p4-eth` and `espion-core1` |
| `firmware/esp32p4-pie/` | no_std PIE (Xespv 2.2, 128-bit vector) support for the P4 on stable rustc: `const fn` instruction encoders fed to `asm!` as `.insn` words, the per-core `Pie` token (one PIE-using thread per core, never in an interrupt handler) and the kernels; silicon rev 3.0 or later only | `cargo test` (host: encoder words against Espressif's assembler output, kernel contract) and `cargo build --release --target riscv32imafc-unknown-none-elf` in that directory; disassemble with Espressif's `riscv32-esp-elf-objdump -M max` (standard objdump mis-decodes PIE) |
| `firmware/espion-core1/` | no_std runner of the blob detector on a bare core 1 of the P4 (no scheduler, interrupts, critical section or logging there): entry and trap vector, the poll/detect/publish loop on `esp32p4-csi`'s strip reader, the frame-result queue on atomics, the pixel check, and core 0's handle (start, reader hand-over, drain, once-a-second figures, heartbeat and trap watch); rules for code on core 1 in its README | `cargo build --release` and `cargo clippy --release` in that directory; host tests of the queue: `rustc --edition 2024 --test src/spsc.rs -o /tmp/spsc-test && /tmp/spsc-test`; also built by `poc-p4-eth` and `poc-p4-stream`; after a change, the call-graph walk from core 1's entry its README describes |
| `firmware/espion-vision/` | no_std blob detector (threshold, 8-connected components by runs and union-find, intensity-weighted centroids; RAW8 and CSI-2 RAW10, whole frames or strips) and the synthetic frame generator of its tests and benchmarks; no esp dependency | `cargo test` (host: unit tests and the randomised comparison with a flood-fill reference) and `cargo build --release --target riscv32imafc-unknown-none-elf` in that directory; timed on the board by `poc-p4-eth` |
| `firmware/ov9281/` | no_std OV9281 sensor driver: mode solver (RAW8/RAW10, size, crop or binning, frame rate → registers, limits in `src/limits.rs`) and async SCCB control over `embedded-hal-async`; no esp dependency | `cargo test` (host) and `cargo build --release --target riscv32imafc-unknown-none-elf` in that directory |
| `firmware/poc-p4-eth/` | Throwaway proof of concept for the Waveshare ESP32-P4-ETH dev board (OV9281 over CSI, detector timing at 120 fps; [README](firmware/poc-p4-eth/README.md)); not part of the product | `cargo build --release` in that directory |
| `firmware/poc-p4-stream/` | Throwaway proof of concept on the same ESP32-P4-ETH board: Ethernet and IPv6 link-local, and the `espion-proto` camera stream service (UDP 5009) with the OV9281, raw or hardware JPEG frames ([README](firmware/poc-p4-stream/README.md)); not part of the product | `cargo build --release` in that directory; on the board: the `poc-viewer` board test (next row) |
| `host/poc-viewer/` | egui live view and control of the `poc-p4-stream` camera stream (every sensor and stream setting, JPEG decoded off the UI thread), and a fake camera that speaks the protocol ([README](host/poc-viewer/README.md)); not part of the product | `cargo run -p poc-viewer --release` (board on `eth0`; `-- --fake` without it); `cargo test -p poc-viewer`; against the board `cargo test -p poc-viewer --release --test board -- --ignored --nocapture --test-threads=1 settings`; `--features inspection` to drive it with the `egui` MCP tools (section below) |
| `host/*` | Host apps of the product: server, viewer, calib, sync | `cargo build --workspace`, `cargo test --workspace` (root) |
| `hardware/espion-cam/` | KiCad project; it is the source of every part value and board figure; its design (requirements, decisions, formulas, layout constraints, firmware rules) is in `docs/hardware/`; the BOM is exported from the schematic with the `Order` preset | KiCad toolkit: `kicad_check` (ERC, drawing rules, connectivity, footprints, fields, decoupling, pulls, BOM lines, rules; on the board `drc`, track widths, return paths and vias, decoupling and footprint placement, `layout` constraints); board set-up and layout with the `kicad_edit` board ops (stack-up, board rules, outline, update from schematic, update footprint from library, placement, tracks, diff pairs, vias, zones, rule areas, board text); fab outputs (Gerbers, JLCPCB BOM/CPL, hand-assembly list from `assembly.hand` in `.omp/kicad.json`, assembly drawings, STEP): `kicad_export`; BOM: `kicad_query` `bom` (preset `Order`) |
| `hardware/lib/` | Project-local symbols, footprints, 3D models; `3d-src/` holds the SolidCraft scripts the models are built from | KiCad toolkit: `kicad_edit` library ops, `kicad_check` `klc`; model rebuild in `hardware/lib/README.md` |
| `.omp/` | Agent config: the KiCad toolkit extension (`extensions/kicad/`, settings in `kicad.json`, including the board's `layout` constraints) and its `kicad-*` skills (including `kicad-pcb-layout` and `kicad-pcb-review` for the board); the SolidCraft MCP server (`mcp.json`, may write only under `hardware/lib`) and its `solidcraft-*` skills, which the dev shell links from the flake input (`solidcraftSkills` in `flake.nix`); the `egui` MCP server (`egui-mcp` from the dev shell), which drives a running egui app | `bun test` in `.omp/extensions/kicad` |
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

## Host GUI apps (egui)

- Verify a GUI change by driving the running app, not by reading the code. Build the app
  with eframe's `inspection` feature and start it without a desktop:
  `egui-headless [-s WIDTHxHEIGHT] [-p PORT] COMMAND [ARG...]` (dev shell). It runs the
  command in a headless Wayland compositor with software rendering and eframe's inspection
  server on `127.0.0.1:PORT` (default 5719), apart from the user's session. Start it as a
  background job and stop it when done; stopping it stops the app.
- Then use the `egui` MCP tools: `attach` (with the port), `widget_tree` and `get_widget`
  to read the UI, `click`, `type_text`, `drag`, `scroll`, `press_key` to act, `wait_for` to
  let the UI settle, `screenshot` (with `save_path`) to look at it. Controls are found by
  their label or value, so every control needs a distinct text label.
- Use one port per app when several run at once. The `egui_mcp` version in `flake.nix`
  must match the workspace's eframe version.
- Rendering there is software: frame rates and frame times measured headless say nothing
  about the real display. Measure those on the desktop session, in short runs.

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
- The KiCad files are the source of every value that can change with the schematic or the
  layout: connections, parts and their values, positions, distances, lengths, rule values
  (net classes, `.kicad_dru`, the `layout` section of `.omp/kicad.json`). `docs/hardware/`
  holds what they can't: requirements and their sources, decisions, formulas (with the
  datasheet constants and the refdes they take), accepted risks named by the limit they
  break, layout constraints, firmware rules and first-board checks. It never states a
  part value, a part number, a measured board figure, a rule's value or a result
  calculated from them; query the KiCad files instead. A change updates `docs/hardware/`
  in the same commit when it changes a requirement, a formula, a decision, a risk, a
  constraint or a firmware rule, and the board list in `hardware/espion-cam/README.md`
  when it adds or removes a block or changes an IC or module family. Keep ERC at 0
  violations.
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

## Commits

- Every commit follows [Conventional Commits](https://www.conventionalcommits.org/):
  `type(scope): description`, e.g. `fix(espion-cam): …`. The types are cocogitto's
  defaults; the allowed scopes are the `scopes` list in `cog.toml` (a commit may also have
  no scope). A new crate, board or tool adds its scope there in the same commit.
- The hk hooks in `hk.pkl` check this: `commit-msg` on every commit, `pre-push` on every
  commit being pushed. The dev shell installs them; commit and push from inside it. Never
  bypass them (`--no-verify`, `HK=0`).
- `fixup!`/`squash!`/`amend!` commits are allowed locally; fold them
  (`git rebase -i --autosquash`) before pushing, pre-push rejects them. Git's default
  `Revert "…"` message is rejected: write `revert: …` instead.

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
