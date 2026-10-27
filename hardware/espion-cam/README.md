# espion-cam

KiCad project for the espion camera board. Not started yet. Licensed under
CERN-OHL-P-2.0 (`../LICENSE`).

## Board

- ESP32-P4 (silicon rev v3.0 or later; esp-hal does not support older revisions)
- OV9281 global-shutter mono sensor on MIPI CSI-2 (2 lanes), external trigger on XVS,
  STROBE output to the LED gate
- 850 nm LED ring with driver, gated by `(STROBE ∧ LED_EN) ∨ LED_FORCE`
- ESP32-C5 Wi-Fi co-processor: SPI slave to the P4, plus `DATA_READY` and `PPS` lines to the P4
- USB-C, 5 V

Signal list and open questions (C5 flashing path, LED eye safety): see
`../../docs/architecture.md`. Timing requirements for XVS, STROBE and PPS: see
`../../docs/timing.md`.

## Conventions

- Edit only with KiCad (the dev shell provides it). Never hand-edit `.kicad_*` files.
- Project-specific symbols, footprints and 3D models go in `../lib/`, not in global libraries.
- Fabrication outputs (Gerbers, drill files, BOM/CPL exports) are not committed. Generate
  them for each release.
