# espion-cam

KiCad project for the espion camera board. Not started yet. Licensed under
CERN-OHL-P-2.0 (`../LICENSE`).

## Board

- ESP32-P4 with in-package PSRAM (silicon rev v3.0 or later; esp-hal does not support older
  revisions)
- OV9281 global-shutter mono sensor on MIPI CSI-2 (2 lanes), external trigger on XVS,
  STROBE output to the LED gate
- 850 nm LED ring with driver for passive retroreflective markers at 2–5 m, gated by
  `(STROBE ∧ LED_EN) ∨ LED_FORCE`
- ESP32-C5 module (certified, integrated antenna; 5 GHz): SPI slave to the P4, plus
  `DATA_READY` and 10 Hz `SYNC` lines to the P4
- USB-C, 5 V, through a USB 2.0 hub to both chips' USB Serial/JTAG

Signal list and hardware tasks (LED driver sizing, IEC 62471 eye-safety check): see
`../../docs/architecture.md`. Timing requirements for XVS, STROBE and SYNC: see
`../../docs/timing.md`.

## Conventions

- Edit only with KiCad (the dev shell provides it). Never hand-edit `.kicad_*` files.
- Project-specific symbols, footprints and 3D models go in `../lib/`, not in global libraries.
- Fabrication outputs (Gerbers, drill files, BOM/CPL exports) are not committed. Generate
  them for each release.
