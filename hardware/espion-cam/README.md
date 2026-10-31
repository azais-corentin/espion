# espion-cam

KiCad project for the espion camera board: a baseboard for the Waveshare ESP32-P4-Core
module. The schematic is captured and ERC clean: a root sheet and seven hierarchical sheets
(power, mcu, ethernet, camera, led_driver, led_ring, imu), with the multi-signal interfaces
crossing sheets as buses. The board is set up (outline, stack-up, rules), every part is
placed and the keep-out rule areas are drawn; nothing is routed yet.
Licensed under CERN-OHL-P-2.0 (`../LICENSE`).

## Board

- Waveshare ESP32-P4-Core, 16 MB flash (SKU 36096): ESP32-P4NRW32X (rev v3.x, as esp-hal
  requires), 32 MB in-package PSRAM, 40 MHz and 32.768 kHz crystals. 76 stamp holes,
  1 mm pitch, 24 × 20 mm. Symbol `MCU_Module_Waveshare:ESP32-P4-Core`, footprint
  `Module_Waveshare:Waveshare_ESP32-P4-Core_HandSolder` (`../lib/`; hand-soldered, not on
  the stencil)
- OV9281 global-shutter mono camera module (Raspberry Pi style) on a 15-pin 1.0 mm FPC
  (MIPI CSI-2, 2 lanes), held by the enclosure 10 mm behind the board and looking through a
  Ø16 mm cutout; its FFC wraps round the bottom edge through a notch into the board's FPC
  connector (JUSHUO AFA07-S15FCA-00, bottom contact, so the module's own opposite-face
  FFC lands pin n on pin n). XVS and STROBE (level not known: 1.8, 2.8 or 3.3 V) come from pads on the
  module over a JST-SH 4-pin pigtail: XVS through a resistor divider set as an assembly
  option, STROBE through an SN74LV1T34 to the LED gate and a P4 GPIO (ETM capture). A 0 Ω
  0402 link (UNI-ROYAL 0402WGF0000TCE) on each CSI line at the FPC end leaves room for a
  series value
- 850 nm LED ring for passive retroreflective markers at 2–5 m: 12 × Amicc
  A-SC880IR3C-P03-1T (90°) in one string, on r = 11.5 mm around the lens cutout. TPS22918
  load switch (`LED_PWR_EN`) and TPS61170 boost to 32 V (`BST_EN`) into 2 × 220 µF storage,
  TLV9061 + IRLR024N sink at a fixed ≈ 0.9 A, gated by `STROBE ∧ LED_EN` (SN74LVC1G97) with a
  0.53–1.03 ms hardware timeout per pulse behind the 0.5 ms firmware exposure cap. No
  continuous-on mode
- IP101GRI Ethernet PHY on RMII (25 MHz crystal, PHY supplies the 50 MHz REF_CLK), HanRun
  HR871150C MagJack with both centre taps and the spare pairs brought out, its LEDs driven
  by the PHY (green link/activity, yellow 100 Mb/s), PPS and PHY reset test points
- Power: SDAPO DP1435-5V IEEE 802.3af PoE PD module (isolated 5.15 V, LCSC C5380097)
  through an LC filter, OR-ed with USB-C 5 V by a TPS2121 priority mux; TLV62569 buck to 3.3 V
- USB-C straight to the P4's USB Serial/JTAG (no hub), with ESD protection and a TI
  TSD05DYFR VBUS TVS. VBUS is sensed on GPIO20 (`VBUS_SENSE`, 47 kΩ / 75 kΩ divider):
  firmware turns the D+ pull-up off while no USB host supplies VBUS
- Yongyu YY-0807RGBJ26-07-FJ RGB status LED (WS2812-type protocol, shorter bit times);
  BOOT and RESET buttons (BOOT with a TSD05DYFR ESD clamp)
- ST LSM6DS3TR-C 6-axis IMU on its own I2C bus (GPIO3/4), INT1 on GPIO54: wake-on-motion
  flags a bumped camera, the accelerometer gives the gravity vector for rig calibration
- 4 × M3 plated mounting holes (H1–H4), each pad on GND, ≥ 2 mm from the PoE cable-side
  copper; 3 × 1.152 mm NPTH tooling holes (TOOL1–TOOL3, board-only) for JLCPCB Economic
  PCBA

Board design (decisions, values and their reasons, layout constraints, firmware rules, the
checks for the first boards): [`docs/hardware/`](../../docs/hardware/README.md).

The BOM itself comes from the schematic: every sourced symbol carries Manufacturer, MPN
and LCSC fields, and the project's BOM preset `Order` exports them. Generate it for each
order (from this directory) and don't commit it:

```sh
kicad-cli sch export bom --preset Order -o espion-cam-bom.csv espion-cam.kicad_sch
```

The CSV has the columns Reference, Qty, Value, Footprint, Manufacturer, MPN, LCSC and DNP,
one row per group of identical parts, without DNP parts or parts excluded from the BOM.

Net classes (track widths, clearances, diff-pair geometry) are set in `espion-cam.kicad_pro`
for the JLCPCB JLC04161H-7628 stack-up (4 layers, 1.6 mm). The spacing rules that net
classes can't express (PoE isolation, MIPI CSI, MDI, USB and LED_PWR pad spacing) are in the custom
rules file `espion-cam.kicad_dru`.

System view: `../../docs/architecture.md`. Timing requirements for XVS, STROBE and PPS:
`../../docs/timing.md`.

## Conventions

- Edit only with KiCad (the dev shell provides KiCad 10). Never hand-edit `.kicad_*` files.
  Agents work through the KiCad toolkit instead (`kicad_query`, `kicad_edit`,
  `kicad_check`, `kicad_render`, `kicad_export`; see `../../AGENTS.md`).
- The schematic is the source of truth. A change that alters a decision, a derived value,
  a layout constraint, a firmware rule or a part updates `../../docs/hardware/` in the same
  commit. A part change also updates the symbol's Manufacturer, MPN and LCSC fields and the
  board list above. There is no committed BOM file.
- Keep ERC at 0 violations: `kicad-cli sch erc espion-cam.kicad_sch` (from this directory).
- Project-specific symbols, footprints and 3D models go in `../lib/` (conventions and KLC
  checks in `../lib/README.md`), not in global libraries.
- Fabrication outputs (Gerbers, drill files, BOM/CPL exports) and KiCad's `.history/` are
  not committed. Generate the outputs for each release (`kicad_export` writes them to
  `fab/`).
