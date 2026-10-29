# espion-cam

KiCad project for the espion camera board: a baseboard for the Waveshare ESP32-P4-Core
module. The schematic is captured and ERC clean: a root sheet and seven hierarchical sheets
(power, mcu, ethernet, camera, led_driver, led_ring, imu), with the multi-signal interfaces
crossing sheets as buses. PCB layout has not started (`espion-cam.kicad_pcb` is empty).
Licensed under CERN-OHL-P-2.0 (`../LICENSE`).

## Board

- Waveshare ESP32-P4-Core, 16 MB flash (SKU 36096): ESP32-P4NRW32X (rev v3.x, as esp-hal
  requires), 32 MB in-package PSRAM, 40 MHz and 32.768 kHz crystals. 76 stamp holes,
  1 mm pitch, 24 × 20 mm. Symbol `MCU_Module_Waveshare:ESP32-P4-Core`, footprint
  `Module_Waveshare:Waveshare_ESP32-P4-Core` (`../lib/`)
- OV9281 global-shutter mono camera module (Raspberry Pi style) on a 15-pin 1.0 mm FPC
  (MIPI CSI-2, 2 lanes), mounted on the back with the lens through a cutout. The board's
  FPC connector (JUSHUO AFA07-S15ECA-00) is single contact, so a face-flipped cable makes
  no contact. XVS and STROBE (1.8 V) come from pads on the module over a JST-SH 4-pin
  pigtail: XVS through a resistor divider, STROBE through an SN74LV1T34 to the LED gate
  and a P4 GPIO (ETM capture)
- 850 nm LED ring for passive retroreflective markers at 2–5 m: 12 × Amicc
  A-SC880IR3C-P03-1T (90°) in one string, on the front around the lens cutout. TPS22918
  load switch (`LED_PWR_EN`) and TPS61170 boost to 32 V (`BST_EN`) into 2 × 220 µF storage,
  TLV9061 + IRLR024N sink at a fixed 1 A, gated by `STROBE ∧ LED_EN` (SN74LVC1G97) with a
  ≈ 1 ms hardware timeout per pulse. No continuous-on mode
- IP101GRI Ethernet PHY on RMII (25 MHz crystal, PHY supplies the 50 MHz REF_CLK), HanRun
  HR871150C MagJack with both centre taps and the spare pairs brought out, its LEDs driven
  by the PHY (green link/activity, yellow 100 Mb/s), PPS test point
- Power: SDAPO DP1435-5V IEEE 802.3af PoE PD module (isolated 5.15 V, LCSC C5380097)
  through an LC filter, OR-ed with USB-C 5 V by a TPS2121 priority mux; TLV62569 buck to 3.3 V
- USB-C straight to the P4's USB Serial/JTAG (no hub), with ESD protection and a TI
  TSD05DYFR VBUS TVS. VBUS is sensed on GPIO20 (`VBUS_SENSE`, 47 kΩ / 75 kΩ divider):
  firmware turns the D+ pull-up off while no USB host supplies VBUS
- WS2812B-2020-V6 RGB status LED; BOOT and RESET buttons
- ST LSM6DS3TR-C 6-axis IMU on its own I2C bus (GPIO3/4), INT1 on GPIO54: wake-on-motion
  flags a bumped camera, the accelerometer gives the gravity vector for rig calibration
- 4 × M3 plated mounting holes (H1–H4), each pad on GND, ≥ 2 mm from the PoE cable-side
  copper

Schematic design (sheets, buses, hierarchical pins, pin-level connections, component values
and the checks still open for the first boards): `SCHEMATIC.md`. The notes on the selected
parts are in its "Bill of materials" section.

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
classes can't express (PoE isolation, MIPI CSI, MDI and USB spacing) are in the custom
rules file `espion-cam.kicad_dru`.

Signal list, module pin notes, Ethernet pin map and hardware tasks: see
`../../docs/architecture.md`. Timing requirements for XVS, STROBE and PPS: see
`../../docs/timing.md`.

## Conventions

- Edit only with KiCad (the dev shell provides KiCad 10). Never hand-edit `.kicad_*` files.
  Agents work through the KiCad toolkit instead (`kicad_query`, `kicad_edit`,
  `kicad_check`, `kicad_render`; see `../../AGENTS.md`).
- `SCHEMATIC.md` is the plan the schematic follows: a schematic change updates it in the
  same commit. A part change also updates the symbol's Manufacturer, MPN and LCSC fields
  and the board list above. There is no committed BOM file.
- Keep ERC at 0 violations: `kicad-cli sch erc espion-cam.kicad_sch` (from this directory).
- Project-specific symbols, footprints and 3D models go in `../lib/` (conventions and KLC
  checks in `../lib/README.md`), not in global libraries.
- Fabrication outputs (Gerbers, drill files, BOM/CPL exports) and KiCad's `.history/` are
  not committed. Generate the outputs for each release.
