# espion-cam

KiCad project for the espion camera board: a baseboard for the Waveshare ESP32-P4-Core
module. Not started yet. Licensed under CERN-OHL-P-2.0 (`../LICENSE`).

## Board

- Waveshare ESP32-P4-Core, 16 MB flash (SKU 36096): ESP32-P4NRW32X (rev v3.x, as esp-hal
  requires), 32 MB in-package PSRAM, 40 MHz and 32.768 kHz crystals. 76 stamp holes,
  1 mm pitch, 24 × 20 mm. Symbol `MCU_Module_Waveshare:ESP32-P4-Core`, footprint
  `Module_Waveshare:Waveshare_ESP32-P4-Core` (`../lib/`)
- OV9281 global-shutter mono camera module (Raspberry Pi style) on a 15-pin 1.0 mm FPC
  (MIPI CSI-2, 2 lanes). XVS and STROBE come from pads on the module over a JST-SH 4-pin
  pigtail; STROBE goes to the LED gate
- 850 nm LED ring for passive retroreflective markers at 2–5 m: 12 × Amicc
  A-SC880IR3C-P03-1T (90°) in one string. TPS61170 boost to 32 V (enabled by
  `LED_PWR_EN`) into a storage capacitor, op-amp + MOSFET sink at a fixed 1 A, gated by
  `STROBE ∧ LED_EN` with a ≈ 1 ms hardware timeout per pulse. No continuous-on mode
- IP101GRI Ethernet PHY on RMII (25 MHz crystal, PHY supplies the 50 MHz REF_CLK), HanRun
  HR871150C MagJack with both centre taps and the spare pairs brought out, PPS test point
- Power: SDAPO DP1435-5V IEEE 802.3af PoE PD module (isolated 5.15 V, LCSC C5380097)
  through an LC filter, OR-ed with USB-C 5 V by a TPS2116; TLV62569 buck to 3.3 V
- USB-C straight to the P4's USB Serial/JTAG (no hub)

Selected parts and LCSC numbers: `BOM.md`.

Signal list, module pin notes, Ethernet pin map and hardware tasks: see
`../../docs/architecture.md`. Timing requirements for XVS, STROBE and PPS: see
`../../docs/timing.md`.

## Conventions

- Edit only with KiCad (the dev shell provides it). Never hand-edit `.kicad_*` files.
- Project-specific symbols, footprints and 3D models go in `../lib/`, not in global libraries.
- Fabrication outputs (Gerbers, drill files, BOM/CPL exports) are not committed. Generate
  them for each release.
