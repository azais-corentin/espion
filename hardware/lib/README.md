# Hardware library

Project-local KiCad libraries shared by the boards in `hardware/`. They follow the
[KiCad Library Convention](https://klc.kicad.org) (KLC): libraries are named by function
(KLC S1.1, F1.1), and the names don't clash with KiCad's stock libraries.

| Library | Content |
|---|---|
| `MCU_Module_Waveshare.kicad_sym` | Symbols for Waveshare MCU modules |
| `Module_Waveshare.pretty/` | Footprints for Waveshare modules |
| `74xGxx_TI.kicad_sym` | TI single-gate logic with a fixed package |
| `Connector_HanRun.kicad_sym` | HanRun connectors |
| `Converter_DCDC_SDAPO.kicad_sym`, `Converter_DCDC_SDAPO.pretty/` | SDAPO PoE PD modules |
| `Interface_Ethernet_ICPlus.kicad_sym` | IC Plus Ethernet PHYs |
| `Regulator_Switching_TI.kicad_sym` | TI switching regulators |
| `Power_Management_TI.kicad_sym` | TI power management ICs (power muxes) |
| `Transistor_FET_Infineon.kicad_sym` | Infineon MOSFETs |
| `Capacitor_SMD_AISHI.pretty/`, `Capacitor_SMD_JIERR.pretty/` | Aluminium SMD can capacitors |
| `Connector_FFC-FPC_XFCN.pretty/` | XFCN FFC/FPC connectors (not used by espion-cam any more) |
| `Connector_FFC-FPC_JUSHUO.pretty/` | JUSHUO FFC/FPC connectors |
| `Connector_RJ_HanRun.pretty/` | HanRun RJ45 jacks |
| `Connector_USB_HRO.pretty/` | Korean Hroparts USB connectors |
| `LED_SMD_Amicc.pretty/` | Amicc SMD LEDs |
| `LED_SMD_Worldsemi.pretty/` | Worldsemi SMD LEDs (not used by espion-cam any more) |
| `LED_Yongyu.kicad_sym`, `LED_SMD_Yongyu.pretty/` | Yongyu addressable RGB LEDs |
| `Button_Switch_SMD_XKB.pretty/` | XKB SMD switches |
| `Package_SON_TI.pretty/` | TI SON packages with TI's land patterns |
| `Package_DFN_QFN_TI.pretty/` | TI DFN/QFN packages with TI's land patterns |
| `NetTie_Courtyard.pretty/` | Net ties with a courtyard |
| `Module_SZYTF.3dshapes/` | 3D models of off-board modules (no symbol or footprint) |
| `<Library>.3dshapes/` | 3D models (STEP) for the footprints above |
| `3d-src/` | SolidCraft scripts the 3D models are built from |

Each project registers them in its `sym-lib-table` / `fp-lib-table` with
`${KIPRJMOD}`-relative paths, so the project opens on any machine, and a description
ending in "(espion project library)" (the toolkit's `libraryDescriptionSuffix` in
`.omp/kicad.json`; `lib_set_library` changes it). Edit them in KiCad;
agents use the KiCad toolkit's library ops (`kicad_edit` `lib_*`, skill
`kicad-library-part`), which build parts with the official KiCad generators and re-save
them through KiCad.

Check a change with the KLC scripts from
[kicad-library-tools](https://gitlab.com/kicad/libraries/kicad-library-tools) (the
`kicad-library-tools` flake input, at `$KICAD_LIBRARY_TOOLS` in the dev shell; it replaced
kicad-library-utils): `library_utils/klc-check/check_symbol.py --footprints <dir>
hardware/lib/*.kicad_sym` and `library_utils/klc-check/check_footprint.py
hardware/lib/*.pretty/*.kicad_mod`, with the dev shell's `python3`. `--footprints` takes
one directory: for symbols whose default footprint is a stock one, point it at a directory
that links both the stock `.pretty` libraries and these, or the checker reports a false
S5.1. Agents run the same checks with `kicad_check` `klc`, which does this linking.

Expected checker messages (`.omp/kicad.json`: `klcExpected` for rules every part may break,
`klcWaivers` for one part with the reason; `kicad_check` reports both as info):

- **F9.3** on every footprint with a project 3D model: the path is
  `${KIPRJMOD}/../lib/<Library>.3dshapes/<Footprint>.step`, not `${KICAD10_3DMODEL_DIR}`.
- **F9.3 "must be STEP"** and **F6.3 "pad 7 missing Paste"** on footprints that use a stock
  `.stpZ` model or split the exposed-pad paste into separate apertures. The stock footprints
  they come from get the same messages.
- **S4.5** on `Connector_HanRun:HR871150C` only (waiver): the part has no pin 6.
- **F6.1** (waiver) and **F6.3 "missing Paste"** on
  `Module_Waveshare:Waveshare_ESP32-P4-Core_HandSolder`: the hand-soldering variant has no
  paste on its pads, so the stencil has no apertures for it.
- **F6.3 "Rectangular SMD pad"** on `LED_SMD_Worldsemi:LED_WS2812B-2020_PLCC4_2.0x2.0mm` and
  `Button_Switch_SMD_XKB:SW_Push_1P1T_XKB_TS-1187A`: their pads are unchanged from the stock
  footprints, which get the same message.
- **F6.2** on `Connector_FFC-FPC_JUSHUO:JUSHUO_AFA07-S15ECA-00_1x15-1MP_P1.0mm_Horizontal` and
  `Connector_FFC-FPC_JUSHUO:JUSHUO_AFA07-S15FCA-00_1x15-1MP_P1.0mm_Horizontal` (waivers): the
  anchor is kept where KiCad's stock footprint has it.
- **F5.1** and **F5.2** on `NetTie_Courtyard:NetTie-2_SMD_Pad0.5mm` (waivers): Reference and
  Value stay hidden, as in the stock net tie it copies (a copper bridge with no body to label).

## 3D models

Built with [SolidCraft](https://github.com/tangency-labs/solidcraft) (in the dev shell) from
the manufacturers' dimension drawings. KiCad axes: X as the footprint, Y up (opposite the
footprint's Y), Z = 0 on the board top; the models line up with offset 0, scale 1, rotation 0.
Rebuild one with:

```sh
solidcraft-cli run hardware/lib/3d-src/<Footprint>.json --out hardware/lib/<Library>.3dshapes/<Footprint>.step
```

The scripts name their main dimensions as parameters, with the datasheet dimension each one
comes from. Ready-made models exist online for most of these parts (EasyEDA/LCSC), but their
terms allow use only inside EasyEDA, so none are used here. Footprints whose body matches a
stock KiCad model reference that model instead.

## Licence

Parts drawn for this project are under the hardware licence (CERN-OHL-P-2.0). Parts copied
or derived from KiCad's stock libraries keep KiCad's library licence (CC-BY-SA 4.0 with the
KiCad library exception); the Parts section says which.

## Parts

### `MCU_Module_Waveshare:ESP32-P4-Core`, `Module_Waveshare:Waveshare_ESP32-P4-Core`, `Module_Waveshare:Waveshare_ESP32-P4-Core_HandSolder`

Waveshare ESP32-P4-Core, 16 MB and 32 MB flash versions (same pinout and outline).

- Sources: [Waveshare documentation](https://docs.waveshare.com/ESP32-P4-Core) (pin
  description, dimension drawing, module schematic), the
  [ESP32-P4 datasheet](https://documentation.espressif.com/esp32-p4_datasheet_en.pdf) v0.7
  (Tables 2-3, 2-5 and 2-7 for the alternate pin functions, Table 2-10 for the MIPI pin
  types) and the
  [ESP32-P4 TRM](https://documentation.espressif.com/esp32-p4_technical_reference_manual_en.pdf)
  (chapter 12, strapping pins). Drawn for this project; same license as the hardware
  (CERN-OHL-P-2.0).
- Pin names follow the Waveshare pin description. Strapping pins (TRM §12) carry
  their strap in the name (KLC S4.1): `GPIO35/BOOT` and `GPIO36/BOOT_EN` with Waveshare's
  labels, and `GPIO34/STRAP` (JTAG source), `GPIO37/STRAP` and `GPIO38/STRAP` (SPI Download
  Boot). `GPIO0/XTAL_32K_N` and `GPIO1/XTAL_32K_P` are wired to the module's 32.768 kHz
  crystal. The MIPI CSI/DSI pins are bidirectional, as the datasheet's Table 2-10 types
  them.
- **Provisional land pattern.** Waveshare publishes the outline and pad pitch, not the pad
  size. Check the pads against a real module before ordering boards. There are no pads
  under the module. Two footprints, both 1.0 mm under the body like KiCad's castellated
  Espressif module footprints:
  - `Waveshare_ESP32-P4-Core` (reflow): 0.6 × 1.5 mm pads, 0.5 mm past the body edge, with
    paste.
  - `Waveshare_ESP32-P4-Core_HandSolder`: 0.6 × 2.5 mm pads, 1.5 mm past the edge so an iron
    tip reaches each castellation, and no paste, so the stencil leaves the module out. Same
    3D model as the reflow footprint.
- 3D model: `Module_Waveshare.3dshapes/`, from `3d-src/`. A 24 × 20 × 0.85 mm PCB with
  76 castellated half-holes on the footprint pads, under a shield can up to the published
  3.30 mm overall height (the components under the can aren't modelled). The can size
  (21.7 × 17.5 mm), half-hole Ø0.5 mm and pad size 0.6 × 0.8 mm are estimated from
  Waveshare's product photos.

### `Converter_DCDC_SDAPO:DP1435-5V`, `Converter_DCDC_SDAPO:Converter_DCDC_SDAPO_DP1435_THT`

SDAPO DP1435-5V IEEE 802.3af PoE PD module: bridge rectifiers, 1.5 kVrms isolation,
5.15 V / 2.4 A, SIP-6 through-hole (LCSC C5380097).

- Source: [SDAPO DP1435 datasheet](https://datasheet.lcsc.com/datasheet/pdf/7fde657788cd634288cd65d78c25cb31.pdf)
  (pin table and package drawing p. 2, side view and land sketch p. 3). Drawn for this
  project; CERN-OHL-P-2.0.
- VA1/VA2 (centre taps) and VB1/VB2 (spare pairs) are not polarity-sensitive. GND is
  `power_in`, so the output GND net needs a PWR_FLAG.
- **Provisional:** where the body sits along the pin row isn't dimensioned; the pin span is
  centred in the 35.6 mm body. Check against a real module before ordering boards.
- 3D model: `Converter_DCDC_SDAPO.3dshapes/`, from `3d-src/`.

### `Connector_HanRun:HR871150C`, `Connector_RJ_HanRun:RJ45_HanRun_HR871150C_Horizontal`

HanRun HR871150C RJ45 jack with magnetics (LCSC C19724786): 10/100BASE-T, 1CT:1CT
transformers, PoE centre taps and spare pairs brought out, Bob-Smith termination, yellow and
green LEDs, right angle.

- Source: [HanRun datasheet](https://datasheet.lcsc.com/datasheet/pdf/bae28869af3176e6a10bdf4859b444a5.pdf)
  (internal schematic, dimensions, recommended PCB layout).
- Pin names come from the internal schematic: P3 is the shared chip-side centre tap, VC1±
  the line-side centre taps, VC2± the spare pairs. There is no pin 6.
- Symbol drawn for this project (CERN-OHL-P-2.0). The footprint is derived from KiCad's
  `Connector_RJ:RJ45_Abracon_ARJP11A-MA_Horizontal`, renumbered and corrected to the HanRun
  layout (CC-BY-SA 4.0 with the KiCad library exception).
- 3D model: `Connector_RJ_HanRun.3dshapes/`, from `3d-src/`.

### `Interface_Ethernet_ICPlus:IP101GRI`

IC Plus IP101GRI 10/100 Ethernet PHY (MII/RMII), QFN-32 4 × 4 mm (LCSC C703537), on stock
`Package_DFN_QFN:QFN-32-1EP_4x4mm_P0.4mm_EP2.65x2.65mm`.

- Source: [IP101G datasheet IP101G-DS-R01](https://datasheet.lcsc.com/datasheet/pdf/14f15850d1faf0804f05f4a04b553bc6.pdf)
  (Figure 3 pinout, §3.2 pin table, Figure 25 package). Drawn for this project;
  CERN-OHL-P-2.0.
- Pins that latch a setting at reset carry it in the name (KLC S4.1): `COL/RMII`,
  `CRS/LEDMOD`, `RXDV/CRS_DV/FX_HEN`, `LED0/PHY_AD0`, `LED3/PHY_AD3`. The exposed pad
  (pin 33) is the only GND. REGOUT is the core regulator output (10 µF + 0.1 µF); ISET
  needs 6.19 kΩ 1 % to GND.

### `Regulator_Switching_TI:TPS61170DRV`, `Package_SON_TI:Texas_DRV0006A_WSON-6-1EP_2x2mm_P0.65mm_EP1x1.6mm` (+ `_ThermalVias`)

TI TPS61170 1.2 A, 40 V boost converter (LCSC C15163), the 5 V → 32 V LED boost.

- Source: [TPS61170 datasheet](https://www.ti.com/lit/ds/symlink/tps61170.pdf) SLVS789D (pin
  functions; DRV0006A package, example board layout and stencil).
- The symbol is a copy of KiCad's `Driver_LED:TPS61165DRV` (same pinout), renamed. The
  footprints are KiCad's `WSON-6-1EP_2x2mm_P0.65mm_EP1x1.6mm` with TI's land pattern: pads
  0.45 × 0.3 mm on 1.95 mm row centres, EP 1 × 1.6 mm, two paste openings. The
  `_ThermalVias` variant adds TI's two optional 0.2 mm vias; espion-cam stopped using it
  (0.2 mm holes cost extra at JLCPCB) and uses the plain one as U10. CC-BY-SA 4.0 with the
  KiCad library exception.
- 3D model: KiCad's stock WSON-6 model (same body).

### `Power_Management_TI:TPS2121RUX`, `Package_DFN_QFN_TI:Texas_RUX0012A_VQFN-HR-12_2x2.5mm_P0.5mm`

TI TPS2121RUXR 2.8–22 V, 4.5 A priority power mux with fast switchover, adjustable current
limit and soft start, VQFN-HR-12 2 × 2.5 mm (LCSC C485916). The symbol's default footprint is
the project one. Used by espion-cam as U4.

- Source: [TPS212x datasheet](https://www.ti.com/lit/ds/symlink/tps2121.pdf) SLVSEA3F (pin
  functions pp. 4–5, ratings p. 6, RUX0012A outline p. 38, land pattern p. 39, stencil p. 40).
  Symbol and 3D model drawn for this project (CERN-OHL-P-2.0).
- OUT is a native pin stack `[1,8]`, 5.08 mm long so the stacked pin number clears the body.
  ST is open drain (6–20 kΩ pull-up, p. 6). ILIM and SS
  take a resistor and a capacitor, so they are passive. The package has no exposed pad.
- Pin layout: IN1 and its PR1 at the top left, IN2 and CP2 below them, OV1/OV2 at the bottom
  left; OUT and ST on the right; GND, ILIM and SS at the bottom, where their parts go.
- The footprint is derived from KiCad's `Package_DFN_QFN:Texas_VQFN-HR-12_2x2.5mm_P0.5mm`
  (CC-BY-SA 4.0 with the KiCad library exception), whose pads already match TI's land
  pattern (p. 39): renamed after the TI package and given the project 3D model. KiCad ships
  no model for the stock footprint: its `.stpZ` path resolves to no file.
- 3D model: `Package_DFN_QFN_TI.3dshapes/`, from `3d-src/`, to the RUX0012A outline (p. 38):
  2.0 × 2.5 mm body, 1.0 mm high (the drawing gives only the maximum), 0.2 × 0.4 mm and
  0.35 × 0.85 mm terminals, pin-1 dot.

### `Transistor_FET_Infineon:IRLR024N`

Infineon IRLR024NTRPBF logic-level N-MOSFET, 55 V / 17 A / 65 mΩ, D-Pak (LCSC C3007), on
stock `Package_TO_SOT_SMD:TO-252-2` (G 1, D 2 = tab, S 3).

- Source: [IRLR024NPbF datasheet](https://www.infineon.com/dgdl/irlr024npbf.pdf). Graphic
  from KiCad's `Transistor_FET:IPD50R380CE` (CC-BY-SA 4.0 with the KiCad library exception).

### `74xGxx_TI:SN74LVC1G97DBV`

TI SN74LVC1G97DBVR configurable gate, SOT-23-6 (LCSC C128411), on stock
`Package_TO_SOT_SMD:SOT-23-6`.

- Source: [SN74LVC1G97 datasheet](https://www.ti.com/lit/ds/symlink/sn74lvc1g97.pdf)
  (SCES416N).
- Copy of KiCad's `74xGxx:74LVC1G97` (which has no default footprint) with the footprint
  and TI datasheet set (CC-BY-SA 4.0 with the KiCad library exception).

### `Device:LED` + `LED_SMD_Amicc:LED_Amicc_A-SC880IR3C_3.85x3.85mm`

Amicc A-SC880IR3C-P03-1T 850 nm, 1 A IR LED, ceramic 3.85 × 3.85 mm (LCSC C5355470).

- Source: [Amicc datasheet](https://datasheet.lcsc.com/datasheet/pdf/f1bce98067a5ec3e3f50cd2383e27cb7.pdf)
  ADE-880-017 v1, p. 6 (recommended solder pad). Drawn for this project; CERN-OHL-P-2.0.
- Three pads: cathode (pad 1), and a 1.0 mm centre pad plus an outer pad, both anode
  (both pad 2, matching `Device:LED`). Connect the centre pad to the anode copper for heat.
- **Provisional:** the datasheet's labels (0.5 mm gaps, outer pads at ±1.25 mm) disagree
  with its drawn scale and LCSC's footprint (0.35 mm gaps, ±1.125 mm). The outer pads are
  widened to 0.65 mm (x = 0.85–1.50 mm) to cover both.
- 3D model: `LED_SMD_Amicc.3dshapes/`, from `3d-src/`.

### `Connector_FFC-FPC_XFCN:XFCN_F1004-H-15-20G-R_1x15-1MP_P1.00mm_Horizontal`

XFCN F1004-H-15-20G-R, 15-pin 1.00 mm FFC/FPC connector, rear flip, dual contact (LCSC
C22438965), drawn for the OV9281 camera FFC. Symbol:
`Connector_Generic_MountingPin:Conn_01x15_MountingPin`.

- **Not used by espion-cam.** Dual contact lets a face-flipped FFC mirror the pins and put
  +3V3 on the camera's GND, so the board uses a single-contact JUSHUO connector (now the
  bottom-contact AFA07-S15FCA-00, `Connector_FFC-FPC_JUSHUO`).
  The library is still registered in `espion-cam/fp-lib-table`.
- Source: XFCN drawing F1004-H-xx-20G-R rev A1 ([LCSC](https://datasheet.lcsc.com/datasheet/pdf/b071ded21ff6dd235e3f83c07f03e103.pdf),
  15P row, recommended PCB layout); pin 1 cross-checked with LCSC's footprint. Drawn for this
  project; CERN-OHL-P-2.0.
- Pads follow XFCN's layout (0.30 × 1.30 mm signal, 1.60 × 2.80 mm MP). The actuator depth on
  F.Fab is approximate (not dimensioned).
- 3D model: `Connector_FFC-FPC_XFCN.3dshapes/`, from `3d-src/`.

### `Connector_USB_HRO:USB_C_Receptacle_HRO_TYPE-C-31-M-12`

Korean Hroparts TYPE-C-31-M-12 USB-C 2.0 receptacle (LCSC C165948), for the stock
`Connector:USB_C_Receptacle_USB2.0_16P` symbol.

- Source: HRO drawing dated 2020.12.08 ([LCSC](https://datasheet.lcsc.com/datasheet/pdf/9e56b777c022540fcce7c7f67825f55e.pdf)).
- Copy of KiCad's footprint of the same name with every pad, hole and shield slot corrected
  to the drawing: outer pads at 6.40 / 4.80 mm pitch, Ø0.60 mm NPTH, 0.6 × 1.7 / 0.6 × 1.4 mm
  slots (CC-BY-SA 4.0 with the KiCad library exception).
- 3D model: `Connector_USB_HRO.3dshapes/`, from `3d-src/` (KiCad ships none for this part).

### `Capacitor_SMD_AISHI:CP_Elec_6.3x9`, `Capacitor_SMD_JIERR:CP_Elec_8x10.5`

Aluminium SMD can footprints for `Device:C_Polarized` (pad 1 = +): AISHI VZ polymer
6.3 × 9 mm (SVZ1CM331E09E00RAXXX, LCSC C3291468) and JIERR JVE electrolytic 8 × 10.5 mm
(JVE16V330M8X10, LCSC C48615108).

- Sources: [AISHI VZ](https://datasheet.lcsc.com/datasheet/pdf/aabe05a1d48507568d8975f1e7578509.pdf),
  [JIERR JVE](https://datasheet.lcsc.com/datasheet/pdf/3ad662ff0f7907637937f3fa823d20da.pdf).
- Neither vendor publishes a land pattern. The pads are the Nichicon land KiCad uses for the
  same can diameter, checked against the datasheet terminal gap P, length C and width W.
  Outline, silk and courtyard come from KiCad's `CP_Elec_6.3x7.7` / `CP_Elec_8x10` (CC-BY-SA
  4.0 with the KiCad library exception).
- `JIERR:CP_Elec_8x10.5` shares its name with stock `Capacitor_SMD:CP_Elec_8x10.5` but has
  a different land: always use the library prefix.
- 3D models: AISHI from `3d-src/` (`Capacitor_SMD_AISHI.3dshapes/`); JIERR uses KiCad's stock
  `CP_Elec_8x10.5` model (same can).

### `LED_SMD_Worldsemi:LED_WS2812B-2020_PLCC4_2.0x2.0mm`

Worldsemi WS2812B-2020-V6 addressable RGB LED, 2.0 × 2.2 mm (LCSC C52917434), for the stock
`LED:WS2812B-2020` symbol.

- **Not used by espion-cam any more** (2026-10-31): JLCPCB lists the WS2812B-2020-V6 for
  Standard PCBA only, so D1 is the Yongyu YY-0807RGBJ26-07-FJ (`LED_Yongyu`).
- Source: [WS2812B-2020-V6 datasheet](https://datasheet.lcsc.com/datasheet/pdf/a29e59ba4bc7e47e236b4d50b6c26345.pdf)
  p. 2 (back, top and side views, pin table).
- Unchanged copy of KiCad's footprint of the same name (CC-BY-SA 4.0 with the KiCad library
  exception), which references a model KiCad doesn't ship. Only the description (V6
  datasheet) and the model path differ. Its pad numbering is the datasheet top view turned
  180°, and so is the model.
- 3D model: `LED_SMD_Worldsemi.3dshapes/`, from `3d-src/` (CERN-OHL-P-2.0). The width of the
  clear top block, terminal length, notch radius, the bottom polarity bar and the chips inside
  are scaled from the drawing. KiCad draws the clear block opaque, so the chips (the only
  polarity mark visible from the top) don't show.

### `LED_Yongyu:YY-0807RGBJ26-07-FJ`, `LED_SMD_Yongyu:LED_Yongyu_YY-0807_2.0x1.7mm`

Yongyu YY-0807RGBJ26-07-FJ addressable RGB LED, 2.0 × 1.7 × 0.85 mm, single-wire 24-bit GRB
(WS2812-type), VDD 3.0–7.5 V, 12 mA per channel (LCSC C54796803).

- Source: [Yongyu approval spec](https://datasheet.lcsc.com/datasheet/pdf/f9afa0e15f6569ed1e35edd063d6a18f.pdf)
  (package, pinout and suggested soldering pad p. 2, reflow profile and absolute ratings p. 4,
  electrical and switching characteristics p. 5, bit timing p. 6, application circuit p. 8).
- Pins (p. 2): 1 DOUT, 2 VDD, 3 DIN, 4 GND. Pins 2 and 4 are swapped against
  `LED:WS2812B-2020`, so that symbol doesn't fit. The symbol is drawn with the stock
  WS2812B-2020 graphic (CC-BY-SA 4.0 with the KiCad library exception), body 5.08 mm taller
  (2.54 mm at each end) so the VDD/GND names clear DIN/DOUT.
- Footprint drawn for this project (CERN-OHL-P-2.0): Yongyu's suggested pad (p. 2), four
  0.85 × 0.75 mm pads with 0.20 / 0.97 mm gaps, roundrect per KLC. The datasheet top view is
  turned 90° CCW so pad 1 sits top left: pads 1 (−0.86, −0.525), 2 (0.86, −0.525),
  3 (0.86, 0.525), 4 (−0.86, 0.525), 0.75 × 0.85 mm; the 2.0 mm body side runs along X. DOUT,
  VDD, DIN and GND sit where they are on `LED_WS2812B-2020_PLCC4_2.0x2.0mm`.
- The p. 2 drawing has no "top/bottom view" labels. The left view is the top view: its 1.20 mm
  between the terminal bands is the clear block (side view 1.2), and the p. 8 tape drawing
  (parts seen through the cover tape) puts − at GND (4) top left and + at VDD (2) bottom right,
  as the left view does. The right view is its mirror image (bottom view).
- Timing (p. 6): T0H 0.295 µs, T1H 0.595 µs, T0L 0.595 µs, T1L 0.295 µs, each ±0.15 µs;
  reset ≥ 80 µs. T1H must stay within 0.445–0.745 µs.
- 3D model: `LED_SMD_Yongyu.3dshapes/`, from `3d-src/` (CERN-OHL-P-2.0). Substrate, clear
  block, bottom and top terminals from the p. 2 dimensions. The castellation radius, the top
  terminal gap and the green polarity mark are scaled from the drawing (the mark sits on the
  pin 1/4 end: p. 2 draws it slightly toward pin 1, the p. 8 tape view toward pin 4); the metal
  thicknesses are assumed. KiCad draws the clear block opaque, so the mark doesn't show.

### `Button_Switch_SMD_XKB:SW_Push_1P1T_XKB_TS-1187A`

XKB TS-1187A-B-A-B 5.1 × 5.1 mm SMD tactile switch, 1.5 mm high, 160 gf (LCSC C318884), for
the stock `Switch:SW_Push` symbol.

- Source: [XKB TS-1187A drawing](https://datasheet.lcsc.com/datasheet/pdf/56c8799ae5193945a16a1ffbe378246a.pdf)
  (side and top views, height table, materials).
- Unchanged copy of KiCad's footprint of the same name (CC-BY-SA 4.0 with the KiCad library
  exception), which references a model KiCad doesn't ship. Only the description (datasheet
  URL) and the model path differ.
- 3D model: `Button_Switch_SMD_XKB.3dshapes/`, from `3d-src/` (CERN-OHL-P-2.0). Base height,
  corner notches and the octagonal cover boss are scaled from the drawing; the terminals are
  flat strips (the small step inside the body is left out).

### `Connector_FFC-FPC_JUSHUO:JUSHUO_AFA07-S15ECA-00_1x15-1MP_P1.0mm_Horizontal`

JUSHUO AFA07-S15ECA-00 15-pin 1.0 mm FFC/FPC connector, top contact, slide lock, 2.5 mm high
(LCSC C262742), for the camera FFC (`../../docs/hardware/camera.md`). Symbol:
`Connector_Generic_MountingPin:Conn_01x15_MountingPin`.

- **Not used by espion-cam any more** (J4 is the bottom-contact FCA since 2026-10-31, next
  entry).
- Sources: JUSHUO drawings rev A, [AFA07-S15ECA-00](https://datasheet.lcsc.com/datasheet/pdf/0bfd48b78181b65482fe0787b860af72.pdf)
  (top, front and side views, section A-A, DIM table, recommended PCB pattern) and
  [AFA07-S15FCA-00](https://datasheet.lcsc.com/datasheet/pdf/b01391708457137d9b8d8b493bdc4f0c.pdf)
  (compared).
- Copy of KiCad's `Connector_FFC-FPC:JUSHUO_AFA07-S15FCA-00_1x15-1MP_P1.0mm_Horizontal`
  (CC-BY-SA 4.0 with the KiCad library exception), which references a model KiCad doesn't
  ship. Pads and drawings are unchanged: the E (top) and F (bottom) parts share outline, land
  pattern and circuit-1 position, so the footprint fits the bottom-contact FCA too. Renamed
  after the fitted part, with its datasheet and the project model. The stock anchor breaks KLC
  F6.2 (waived: kept as KiCad's generator places it).
- 3D model: `Connector_FFC-FPC_JUSHUO.3dshapes/`, from `3d-src/` (CERN-OHL-P-2.0), the E part
  with the latch closed. The F part only differs in front: its FFC opening and latch notch sit
  low instead of high. Latch depth, FFC opening height, side arms, tail thickness and fitting
  nail width are scaled from the drawing; the nail sheet thickness (0.20 mm) and its hidden
  rear web are assumed. The F.Fab rectangle in front of the latch (y 3.475–4.475) is the
  latch pulled open (+1.65 mm, section A-A); the model shows it closed.

### `Connector_FFC-FPC_JUSHUO:JUSHUO_AFA07-S15FCA-00_1x15-1MP_P1.0mm_Horizontal`

JUSHUO AFA07-S15FCA-00 15-pin 1.0 mm FFC/FPC connector, bottom contact, slide lock, 2.5 mm
high (LCSC C262721), J4 on espion-cam, for the camera FFC (`../../docs/hardware/camera.md`).
Symbol: `Connector_Generic_MountingPin:Conn_01x15_MountingPin`.

- Source: JUSHUO drawing AFA07-S\*\*F\*\*-\*\* rev A
  ([LCSC](https://datasheet.lcsc.com/datasheet/pdf/b01391708457137d9b8d8b493bdc4f0c.pdf): top,
  front and side views, section A-A, DIM table, recommended PCB pattern), compared with the
  E drawing of the entry above.
- Copy of the ECA footprint above, whose pads and drawings are KiCad's
  `Connector_FFC-FPC:JUSHUO_AFA07-S15FCA-00_1x15-1MP_P1.0mm_Horizontal` unchanged (CC-BY-SA 4.0
  with the KiCad library exception). The F drawing's recommended PCB pattern is the E
  drawing's: pitch 1.00 ± 0.05, DIM A 14.00 ± 0.05, pads 0.60 × 1.80, MP pads 2.60 × 3.00,
  3.65 from circuit 1 to the MP pad's outer edge, 4.57 overall, circuit 1 on the left in the
  top view; DIM C/D, the 5.30 depth and the side view's 6.75/2.55/4.00/1.45/2.50 are the same
  too. Only the description (F drawing), tags and 3D model differ from the ECA footprint
  (KiCad ships no model for the stock one). The stock anchor breaks KLC F6.2 (waived, as for
  the ECA).
- 3D model: `Connector_FFC-FPC_JUSHUO.3dshapes/`, from `3d-src/` (CERN-OHL-P-2.0): the ECA
  model with its front turned upside down, as the F front view and section A-A draw it. The
  FFC opening sits 0.42–0.83 mm above the board, with the housing floor lip set into the latch
  notch below it; both heights are scaled from the F front view. The latch bar is solid on top
  across its full width (F top view). Everything else, estimates included, is as in the ECA
  model; the chamfer at the rear end of the latch side arms (low in the F side view, high in
  the E one) is modelled in neither.

### `NetTie_Courtyard:NetTie-2_SMD_Pad0.5mm`

Two-pad net tie (0.5 mm round pads joined by a 1.0 × 0.5 mm copper bridge), used as NT1 on
espion-cam: `ISNS_K` leaves R69's source-side pad through it (Kelvin sense,
`../../docs/hardware/layout.md`, LED driver and ring).

- Copy of KiCad's `NetTie:NetTie-2_SMD_Pad0.5mm` (CC-BY-SA 4.0 with the KiCad library
  exception). Pads, the copper bridge, the net-tie pad group and the attributes are
  unchanged. Added: an F.CrtYd rectangle 0.25 mm around the copper (±1.0 × ±0.5 mm), so DRC
  checks NT1 for courtyard overlaps and the board can treat `missing_courtyard` as an error.
  Description (with the stock footprint's URL) and tags changed. No 3D model, as in stock.

### SZYTF OV9281 camera module, 79° lens (3D model only)

SZYTF OV9281 camera module with the 79° lens (drawing WX-1M-9281-79-V1.0), the camera on
espion-cam's 15-pin FFC. It is an off-board part: it mounts in the 3D-printed enclosure
10 mm behind the espion-cam PCB, so it has no symbol or footprint. The model is for the
enclosure assembly.

- Sources: mechanical drawing WX-1M-9281-79-V1.0
  ([listing image](https://ae-pic-a1.aliexpress-media.com/kf/Sc05f84c44d8340139f90cdb8f6d23030K.jpg):
  top, side and bottom views), the
  [product photo](https://ae-pic-a1.aliexpress-media.com/kf/S55ea7644d5a34637b0eb6633bdcac8e03.jpg_960x960q75.jpg_.avif)
  (front and back) and the [listing](https://fr.aliexpress.com/item/1005012566040263.html).
  Drawn for this project; CERN-OHL-P-2.0.
- 3D model: `Module_SZYTF.3dshapes/SZYTF_OV9281_WX-1M-9281-79.step`, from
  `3d-src/SZYTF_OV9281_WX-1M-9281-79.json` (rebuild command above). It doesn't use the KiCad
  footprint frame. The origin is on the optical axis at the PCB's lens-side surface, +Z
  points toward the scene (lens direction), X runs along the 25 mm edge, and the FFC edge
  is toward −Y. Overall 25 × 23.86 × 7.135 mm (Z −1.635 to 5.5).
- From the drawing: the 25 × 23.86 × 1.6 mm board; 4 × Ø2.2 holes on 21 × 12.5 mm, centred
  on the lens axis; the 6.5 mm square lens holder; the Ø5 lens, 5.5 mm above the PCB; the
  FFC connector, 2.5 mm high, with 15 contacts at 1.0 mm.
- Estimated (scaled from the drawing or the photo): how far the hole pattern sits from the
  board edges (far edge 8.3 mm from the lens axis); the 1.0 mm corner radius; the gold
  rings (Ø3.8 and Ø3.0); the two small holes (Ø1.8 at X ±9.0 on the lens centre line); the
  holder block height (4.5 mm, barrel above it); the connector depth (5.8 mm, of which the
  latch is 2.1 mm, reading the side view's 2.1 as the latch); the housing and latch widths
  (21.0 and 22.0 mm); the back pads (Ø1.8: XVS, STROBE, IL_PWM and GPIO2 at Y +6.7 mm on a
  3.0 mm pitch, since the photo measures 3.07 rather than 2.54; 3V3 and GND at X ±8.55,
  Y −9.65); and the white silkscreen frame. The small SMD parts are left out.
