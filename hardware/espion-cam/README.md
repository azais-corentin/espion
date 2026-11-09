# espion-cam

KiCad project for the espion camera board: a baseboard for the Waveshare ESP32-P4-Core
module. The schematic is captured and ERC clean: a root sheet and seven hierarchical sheets
(power, mcu, ethernet, camera, led_driver, led_ring, imu), with the multi-signal interfaces
crossing sheets as buses. The board is fully placed and routed (4 layers, 64 × 64 mm,
JLCPCB Economic PCBA on the top side, hand-soldered parts listed in `.omp/kicad.json`).
Licensed under CERN-OHL-P-2.0 (`../LICENSE`).

## Board

- **MCU:** U6 Waveshare ESP32-P4-Core module (bottom side, hand-soldered, not on the stencil).
  Symbol `MCU_Module_Waveshare:ESP32-P4-Core`, footprint
  `Module_Waveshare:Waveshare_ESP32-P4-Core_HandSolder` (`../lib/`).
  [mcu](../../docs/hardware/mcu.md#module)
- **Camera:** off-board OV9281 global-shutter mono module behind a lens cutout, on a 15-pin 1.0 mm
  FFC (MIPI CSI-2, 2 lanes) through a notch in the board edge into J4 (AFA07 family, bottom
  contact). XVS (GPIO7) and STROBE come from the module's pads over a JST-SH 4-pin pigtail into J3:
  XVS through the R40/R44 divider, an assembly option; STROBE through U7 Tech Public TP74LV1T34 to the LED
  gate and GPIO12 (ETM capture). A 0 Ω link on each CSI line at the FFC end (R45–R47, R49–R51).
  [camera](../../docs/hardware/camera.md)
- **LED:** 850 nm ring D4–D15, 12 × A-SC880IR3C in one string around the lens cutout. U8 TPS22918
  load switch (`LED_PWR_EN`, GPIO20), U10 TPS61170 boost (`BST_EN`, GPIO53; PWM sets the
  output) into the storage capacitors C46/C47, U11 TLV9061 + Q1 HL2310A (SOT-23) current sink (R69
  sense) gated by `STROBE ∧ LED_EN` in U9 SN74LVC1G97 (`LED_EN`, GPIO11), with a hardware
  timeout per pulse behind the firmware exposure cap. No continuous-on mode. Firmware keeps
  +32V just above the string from a divider on Q1's drain (R70/R76, `LED_K_SENSE`, GPIO23), a
  required headroom loop: Q1 is not rated for full boost output;
  a +32V divider (R71/R72, `VLED_SENSE`, GPIO21) watches the rail.
  [led](../../docs/hardware/led.md)
- **Ethernet:** U1 IP101G PHY on RMII (Y1 crystal; the PHY supplies the REF_CLK), J1 HR871150C
  MagJack (bottom side, hand-soldered) with both centre taps and the spare pairs brought out,
  its LEDs driven by the PHY, PPS and PHY reset test points. `PHY_RST_N` GPIO22.
  [ethernet](../../docs/hardware/ethernet.md)
- **Power:** U2 DP1435-5V IEEE 802.3af PoE PD module (isolated, bottom side, hand-soldered)
  through an LC filter (L1, C13, C14), OR-ed with USB-C 5 V by U4 TPS2121 priority mux (source
  on `PWR_SRC`, GPIO2); U3 TLV62569 buck to 3.3 V. [power](../../docs/hardware/power.md)
- **USB-C:** J2 straight to the P4's USB Serial/JTAG (no hub, GPIO24/25), U5 SRV05-4 ESD
  protection, D2 H7VHD3UA VBUS TVS. VBUS is sensed on GPIO6 (`VBUS_SENSE`, R27/R28 divider):
  firmware turns the D+ pull-up off while no USB host supplies VBUS.
  [mcu](../../docs/hardware/mcu.md#usb-c)
- **Status and buttons:** D1 YY-0807BRGK26 RGB status LED (`STATUS_LED`, GPIO13); SW1
  RESET and SW2 BOOT.
  [mcu](../../docs/hardware/mcu.md#status-led)
- **IMU:** U12 MSA3S02 3-axis accelerometer on its own I²C bus (GPIO3/4), INT1 on GPIO51: the
  motion interrupt flags a bumped camera, the accelerometer gives the gravity vector for rig
  calibration (no gyroscope). [imu](../../docs/hardware/imu.md)
- **Mechanical:** 4 × M3 plated mounting holes H1–H4, each pad on GND, kept clear of the PoE
  cable-side copper; 3 NPTH tooling holes TOOL1–TOOL3 (board-only) for JLCPCB Economic PCBA.
  [layout](../../docs/hardware/layout.md#board-and-stack-up)

Assembly: everything on the top side is machine-placed by JLCPCB Economic PCBA except the
hand-assembly list (`assembly.hand` in `.omp/kicad.json`; reasons in
[layout](../../docs/hardware/layout.md#assembly)).

Board design (decisions, requirements and their reasons, formulas, layout constraints,
firmware rules, the checks for the first boards): [`docs/hardware/`](../../docs/hardware/README.md).

The parts list is the schematic: every sourced symbol carries Manufacturer, MPN and LCSC fields,
and the project's BOM preset `Order` exports them. Read it with `kicad_query bom`, or generate
the CSV for each order (from this directory) and don't commit it:

```sh
kicad-cli sch export bom --preset Order -o espion-cam-bom.csv espion-cam.kicad_sch
```

The CSV has the columns Reference, Qty, Value, Footprint, Manufacturer, MPN, LCSC and DNP,
one row per group of identical parts, without DNP parts or parts excluded from the BOM.

Net classes (track widths, clearances, diff-pair geometry) are set in `espion-cam.kicad_pro`
for the JLCPCB JLC04161H-7628 stack-up (4 layers, 1.6 mm). The spacing rules that net
classes can't express (PoE isolation, MIPI CSI spacing and skew, MDI spacing, pair-to-pair spacing and skew, LED_PWR pad
spacing, the U12 LGA lands, hole spacing, power neck-downs and silkscreen) are in the custom
rules file `espion-cam.kicad_dru`; [layout](../../docs/hardware/layout.md#custom-rules) gives each one's reason.

System view: `../../docs/architecture.md`. Timing requirements for XVS, STROBE and PPS:
`../../docs/timing.md`.

## Conventions

- Edit only with KiCad (the dev shell provides KiCad 10). Never hand-edit `.kicad_*` files.
  Agents work through the KiCad toolkit instead (`kicad_query`, `kicad_edit`,
  `kicad_check`, `kicad_render`, `kicad_export`; see `../../AGENTS.md`).
- The schematic and the board are the source of every value. `docs/hardware/` holds
  requirements, decisions, formulas, accepted risks, layout constraints, firmware rules and
  first-board checks, never values the KiCad files hold or that are calculated from them. A
  change updates `../../docs/hardware/` in the same commit when it changes one of those. A
  part change also updates the symbol's Manufacturer, MPN and LCSC fields. A new or removed
  block or IC family updates the board list above. There is no committed BOM file.
- Keep ERC at 0 violations: `kicad-cli sch erc espion-cam.kicad_sch` (from this directory).
- Project-specific symbols, footprints and 3D models go in `../lib/` (conventions and KLC
  checks in `../lib/README.md`), not in global libraries.
- Fabrication outputs (Gerbers, drill files, BOM/CPL exports) and KiCad's `.history/` are
  not committed. Generate the outputs for each release (`kicad_export` writes them to
  `fab/`).
