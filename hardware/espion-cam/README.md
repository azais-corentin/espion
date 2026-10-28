# espion-cam

KiCad project for the espion camera board: a baseboard for the Waveshare ESP32-P4-Core
module. Not started yet. Licensed under CERN-OHL-P-2.0 (`../LICENSE`).

## Board

- Waveshare ESP32-P4-Core, 16 MB flash (SKU 36096): ESP32-P4NRW32X (rev v3.x, as esp-hal
  requires), 32 MB in-package PSRAM, 40 MHz and 32.768 kHz crystals. 76 stamp holes,
  1 mm pitch, 24 × 20 mm. Symbol and footprint go in `../lib/`
- OV9281 global-shutter mono sensor on MIPI CSI-2 (2 lanes), external trigger on XVS,
  STROBE output to the LED gate
- 850 nm LED ring with driver for passive retroreflective markers at 2–5 m, gated by
  `(STROBE ∧ LED_EN) ∨ LED_FORCE`
- IP101GRI Ethernet PHY on RMII (25 MHz crystal, PHY supplies the 50 MHz REF_CLK), RJ45
  MagJack with PoE-rated centre taps, PPS test point
- Power: IEEE 802.3af PoE PD (isolated 5 V) OR-ed with USB-C 5 V
- USB-C straight to the P4's USB Serial/JTAG (no hub)

Signal list, module pin notes, Ethernet pin map and hardware tasks: see
`../../docs/architecture.md`. Timing requirements for XVS, STROBE and PPS: see
`../../docs/timing.md`.

## Conventions

- Edit only with KiCad (the dev shell provides it). Never hand-edit `.kicad_*` files.
- Project-specific symbols, footprints and 3D models go in `../lib/`, not in global libraries.
- Fabrication outputs (Gerbers, drill files, BOM/CPL exports) are not committed. Generate
  them for each release.
