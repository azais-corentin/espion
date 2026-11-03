# espion-cam

KiCad project for the espion camera board: a baseboard for the Waveshare ESP32-P4-Core
module. The schematic is captured and ERC clean: a root sheet and seven hierarchical sheets
(power, mcu, ethernet, camera, led_driver, led_ring, imu), with the multi-signal interfaces
crossing sheets as buses. The board is fully placed and routed (4 layers, 64 × 64 mm,
JLCPCB Economic PCBA on the top side, hand-soldered parts listed in `.omp/kicad.json`).
Licensed under CERN-OHL-P-2.0 (`../LICENSE`).

## Board

- U6 Waveshare ESP32-P4-Core, 16 MB flash (SKU 36096; no LCSC part number):
  ESP32-P4NRW32X (rev v3.x, as esp-hal requires), 32 MB in-package PSRAM, 40 MHz and
  32.768 kHz crystals. 76 stamp holes, 1 mm pitch, 24 × 20 mm. Symbol
  `MCU_Module_Waveshare:ESP32-P4-Core`, footprint
  `Module_Waveshare:Waveshare_ESP32-P4-Core_HandSolder` (`../lib/`; bottom side,
  hand-soldered, not on the stencil). [mcu](../../docs/hardware/mcu.md#module)
- SZYTF OV9281 global-shutter mono camera module (drawing WX-1M-9281-79, 79° lens; off-board,
  not in the BOM) on a 15-pin 1.0 mm FFC (MIPI CSI-2, 2 lanes), held by the enclosure with
  its front face 2.1–9.3 mm behind the board's bottom face (lens tip ≤ 5.4 mm below the top
  face, [camera](../../docs/hardware/camera.md#camera-module)) and looking through a Ø16 mm
  cutout; its FFC wraps round the board edge
  through a notch into J4, JUSHUO AFA07-S15FCA-00 (C262721, bottom contact, so the module's
  own opposite-face FFC lands pin n on pin n). XVS and STROBE (level not known: 1.8, 2.8 or
  3.3 V) come from pads on the module over a JST-SH 4-pin pigtail into J3 (SM04B-SRSS-TB,
  C160404): XVS (GPIO7) through the R40/R44 divider set as an assembly option, STROBE through
  U7 SN74LV1T34 (C100024) to the LED gate and GPIO12 (ETM capture). A 0 Ω 0402 link
  (UNI-ROYAL 0402WGF0000TCE, C17168; R45–R47, R49–R51) on each CSI line at the FFC end
  leaves room for a series value. [camera](../../docs/hardware/camera.md)
- 850 nm LED ring for passive retroreflective markers at 2–5 m: D4–D15, 12 × Amicc
  A-SC880IR3C-P03-1T (C5355470, 90°) in one string, on r = 11.5 mm around the lens cutout.
  U8 TPS22918 load switch (`LED_PWR_EN`, GPIO20) and U10 TPS61170 boost to ≤ 32 V (`BST_EN`,
  GPIO53, PWM sets the output) into 2 × 330 µF storage (C46, C47 Panasonic EEEFT1H331AP), U11
  TLV9061 + Q1 IRLR024N sink at a fixed ≈ 0.83 A (R69 0.1 Ω sense), gated by `STROBE ∧ LED_EN`
  (U9 SN74LVC1G97, `LED_EN` on GPIO11) with a 0.53–1.03 ms hardware timeout per pulse behind the
  0.5 ms firmware exposure cap. No continuous-on mode. Firmware keeps +32V just above the
  string from a divider on Q1's drain (R70/R76, `LED_K_SENSE`, GPIO23); a +32 V divider
  (`VLED_SENSE`, GPIO21) watches the rail. [led](../../docs/hardware/led.md)
- U1 IP101GRI Ethernet PHY on RMII (Y1 25 MHz crystal, PHY supplies the 50 MHz REF_CLK),
  J1 HanRun HR871150C MagJack (bottom side, hand-soldered) with both centre taps and the
  spare pairs brought out, its LEDs driven by the PHY (green link/activity, yellow
  100 Mb/s), PPS and PHY reset test points. [ethernet](../../docs/hardware/ethernet.md)
- Power: U2 SDAPO DP1435-5V IEEE 802.3af PoE PD module (isolated 5.15 V, LCSC C5380097;
  bottom side, hand-soldered) through an LC filter (L1, C13, C14), OR-ed with USB-C 5 V by
  U4 TPS2121 priority mux (source on `PWR_SRC`, GPIO2); U3 TLV62569 buck to 3.3 V.
  [power](../../docs/hardware/power.md)
- USB-C (J2 Korean Hroparts TYPE-C-31-M-12) straight to the P4's USB Serial/JTAG (no hub),
  with U5 USBLC6-2SC6 ESD protection and D2 TI TSD05DYFR VBUS TVS. VBUS is sensed on GPIO6
  (`VBUS_SENSE`, 47 kΩ / 75 kΩ divider): firmware turns the D+ pull-up off while no USB
  host supplies VBUS. [mcu](../../docs/hardware/mcu.md#usb-c)
- D1 Yongyu YY-0807BRGK26-07-FI RGB status LED (C54796802, WS2812-type protocol, shorter
  bit times; `STATUS_LED`, GPIO13); SW1 RESET and SW2 BOOT buttons (XKB TS-1187A-B-A-B,
  BOOT with D16 TSD05DYFR ESD clamp). [mcu](../../docs/hardware/mcu.md#status-led)
- U12 MEMSensing MSA3S02 3-axis accelerometer (C51928204, LGA-12 2 × 2 mm, I2C address 0x62)
  on its own I2C bus (GPIO3/4), INT1 on GPIO51: motion interrupt flags a bumped camera, the
  accelerometer gives the gravity vector for rig calibration (no gyroscope).
  [imu](../../docs/hardware/imu.md)
- 4 × M3 plated mounting holes (H1–H4), each pad on GND, ≥ 2 mm from the PoE cable-side
  copper; 3 × 1.152 mm NPTH tooling holes (TOOL1–TOOL3, board-only) for JLCPCB Economic
  PCBA. [layout](../../docs/hardware/layout.md#board-and-stack-up)

Assembly: everything on the top side is machine-placed by JLCPCB Economic PCBA except the
hand-assembly list U6, U2, J1 (bottom side) and TP2, TP3, TP7 (top side, through-hole
GND loops) ([layout](../../docs/hardware/layout.md#assembly)).

Every part, as the schematic's Manufacturer, MPN and LCSC fields give it:

| References | Value | Manufacturer MPN | LCSC | Package |
|---|---|---|---|---|
| U6 | ESP32-P4-Core | Waveshare ESP32-P4-Core (SKU 36096) | – | module, 76 stamp holes (hand) |
| U1 | IP101GRI | IC Plus IP101GRI | C703537 | QFN-32 4 × 4 mm |
| U2 | DP1435-5V | SDAPO DP1435-5V | C5380097 | SIP-6 THT (hand) |
| U3 | TLV62569 | TI TLV62569DBVR | C141836 | SOT-23-5 |
| U4 | TPS2121 | TI TPS2121RUXR | C485916 | VQFN-HR-12 2 × 2.5 mm |
| U5 | USBLC6-2SC6 | ST USBLC6-2SC6 | C7519 | SOT-23-6 |
| U7 | SN74LV1T34 | TI SN74LV1T34DBVR | C100024 | SOT-23-5 |
| U8 | TPS22918 | TI TPS22918DBVR | C131941 | SOT-23-6 |
| U9 | SN74LVC1G97 | TI SN74LVC1G97DBVR | C128411 | SOT-23-6 |
| U10 | TPS61170 | TI TPS61170DRVR | C15163 | WSON-6 2 × 2 mm |
| U11 | TLV9061 | TI TLV9061IDBVR | C398358 | SOT-23-5 |
| U12 | MSA3S02 | MEMSensing MSA3S02 | C51928204 | LGA-12 2 × 2 mm |
| Q1 | IRLR024N | Infineon IRLR024NTRPBF | C3007 | TO-252 |
| Y1 | 25 MHz | Epson X1E0000210139 | C91750 | 3225 4-pin |
| D1 | YY-0807BRGK26-07-FI | Yongyu Photoelectric YY-0807BRGK26-07-FI | C54796802 | 2.0 × 1.7 mm |
| D2, D16 | TSD05DYFR | TI TSD05DYFR | C22396185 | SOD-323 |
| D3 | SS16 | MDD SS16 | C2481 | SMA |
| D4–D15 | A-SC880IR3C | Amicc A-SC880IR3C-P03-1T | C5355470 | 3.85 × 3.85 mm |
| J1 | HR871150C | HanRun HR871150C | C19724786 | RJ45 THT (hand) |
| J2 | TYPE-C-31-M-12 | Korean Hroparts TYPE-C-31-M-12 | C165948 | USB-C 16P |
| J3 | SM04B-SRSS-TB | JST SM04B-SRSS-TB(LF)(SN) | C160404 | JST-SH 4-pin |
| J4 | AFA07-S15FCA-00 | JUSHUO AFA07-S15FCA-00 | C262721 | FFC 15-pin 1.0 mm |
| SW1, SW2 | RESET, BOOT | XKB TS-1187A-B-A-B | C318884 | 5.1 × 5.1 mm |
| TP2, TP3, TP7 | GND | ronghe RH-5002 | C5199802 | test loop THT (hand) |
| L1, L2 | 2.2 µH | cjiang FXL0630-2R2-M | C167218 | FXL0630 |
| L3 | 10 µH | cjiang FNR4030S100MT | C167879 | 4030 |
| C3, C6, C7, C9, C12, C19, C21, C23–C26, C28, C30, C31, C37, C38, C44, C49, C50, C52 | 100 nF 50 V | YAGEO CC0402KRX7R9BB104 | C131394 | 0402 |
| C4, C10, C11, C15, C18, C32, C36 | 10 µF 25 V | Samsung CL21A106KAYNNNE | C15850 | 0805 |
| C8, C16, C29 | 4.7 µF 25 V | Samsung CL21A475KAQNNNE | C1779 | 0805 |
| C27, C34 | 1 µF 50 V | Samsung CL21B105KBFNNNE | C28323 | 0805 |
| C43, C45 | 1 µF 100 V | YAGEO CC1206KKX7R0BB105 | C70463 | 1206 |
| C20 | 47 µF 10 V | Samsung CL31A476MPHNNNE | C96123 | 1206 |
| C1 | 1 nF 2 kV | FH 1206B102K202NT | C9196 | 1206 |
| C2, C5 | 18 pF C0G | FH 0402CG180J500NT | C1549 | 0402 |
| C17 | 6.8 pF C0G | FH 0402CG6R8C500NT | C1576 | 0402 |
| C39, C42 | 220 pF C0G | FH 0402CG221J500NT | C39122 | 0402 |
| C40, C48 | 1 nF C0G | FH 0402CG102J500NT | C53547 | 0402 |
| C33, C35, C41 | 10 nF C0G 50 V | Murata GRM1885C1H103JA01D | C85973 | 0603 |
| C13 | 330 µF 16 V | AISHI SVZ1CM331E09E00RAXXX | C3291468 | can 6.3 × 9 mm |
| C14 | 330 µF 16 V | JIERR JVE16V330M8X10 | C48615108 | can 8 × 10.5 mm |
| C22 | 100 µF 25 V | JIERR MA25V100M6x6 | C46550465 | can 6.3 × 5.8 mm |
| C46, C47 | 330 µF 50 V | Panasonic EEEFT1H331AP | C178596 | can 10 × 10 mm |
| R45–R47, R49–R51 | 0 Ω | UNI-ROYAL 0402WGF0000TCE | C17168 | 0402 |
| R34, R53 | 0 Ω | UNI-ROYAL 0805W8F0000T5E | C17477 | 0805 |
| R11–R13, R15, R31, R32, R35–R37 | 22 Ω 1 % | UNI-ROYAL 0402WGF220JTCE | C25092 | 0402 |
| R4, R5, R30, R67 | 220 Ω 1 % | UNI-ROYAL 0402WGF2200TCE | C25091 | 0402 |
| R39, R54, R64 | 1.00 kΩ 1 % | UNI-ROYAL 0402WGF1001TCE | C11702 | 0402 |
| R61 | 1.40 kΩ 1 % | UNI-ROYAL 0402WGF1401TCE | C39650 | 0402 |
| R14, R40 | 1.5 kΩ 1 % | UNI-ROYAL 0402WGF1501TCE | C25867 | 0402 |
| R41, R42, R44 | 1.8 kΩ 1 % | UNI-ROYAL 0402WGF1801TCE | C25871 | 0402 |
| R73, R74 | 4.7 kΩ 1 % | UNI-ROYAL 0402WGF4701TCE | C25900 | 0402 |
| R1, R7, R9, R10, R23, R25 | 5.1 kΩ 1 % | UNI-ROYAL 0402WGF5101TCE | C25905 | 0402 |
| R6 | 6.19 kΩ 1 % | FH RC-02K6191FT | C324848 | 0402 |
| R8, R26, R29, R33, R38, R48, R58, R66, R72, R76 | 10.0 kΩ 1 % | FOJAN FRC0402F1002TS | C2906861 | 0402 |
| R22 | 36 kΩ 1 % | HWA CHN HRC0402F3602DNTO | C54920666 | 0402 |
| R19, R27, R43 | 47 kΩ 1 % | UNI-ROYAL 0402WGF4702TCE | C25792 | 0402 |
| R60 | 51.1 kΩ 1 % | UNI-ROYAL 0402WGF5112TCE | C25904 | 0402 |
| R21 | 60.4 kΩ 1 % | UNI-ROYAL 0402WGF6042TCE | C25911 | 0402 |
| R55 | 68 kΩ 1 % | UNI-ROYAL 0402WGF6802TCE | C36871 | 0402 |
| R28 | 75 kΩ 1 % | UNI-ROYAL 0402WGF7502TCE | C25798 | 0402 |
| R17, R18, R20, R24, R56, R57, R59, R62, R68, R70 | 100 kΩ 1 % | UNI-ROYAL 0402WGF1003TCE | C25741 | 0402 |
| R16 | 453 kΩ 1 % | GiantOhm GR0402F453KTAG00 | C49653416 | 0402 |
| R63, R75 | 1.00 MΩ 1 % | UNI-ROYAL 0402WGF1004TCE | C26083 | 0402 |
| R65, R71 | 255 kΩ 1 % | UNI-ROYAL 0603WAF2553T5E | C23354 | 0603 |
| R2, R3 | 499 kΩ | YAGEO RV1206FR-07499KL | C309667 | 1206 |
| R69 | 0.1 Ω 1 % 100 ppm | Walsin WW12WR100FTL | C164014 | 1206 |

Not in the BOM: R52 (0 Ω 0805, DNP: CAM_EN to the FFC's pin 12, an assembly option), the
bare-pad test points TP1, TP4–TP6 and TP8–TP14, the net tie NT1, the mounting holes H1–H4
and the tooling holes TOOL1–TOOL3.

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
classes can't express (PoE isolation, MIPI CSI spacing and skew, MDI spacing, pair-to-pair spacing and skew, LED_PWR pad
spacing, the U12 LGA lands, hole spacing, power neck-downs and silkscreen) are in the custom
rules file `espion-cam.kicad_dru`; [layout](../../docs/hardware/layout.md#custom-rules) gives each one's reason.

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
