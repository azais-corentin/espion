# Hardware library

Project-local KiCad libraries shared by the boards in `hardware/`. They follow the
[KiCad Library Convention](https://klc.kicad.org) (KLC): libraries are named by function
(KLC S1.1, F1.1), and the names don't clash with KiCad's stock libraries.

| Library | Content |
|---|---|
| `MCU_Module_Waveshare.kicad_sym` | Symbols for Waveshare MCU modules |
| `Module_Waveshare.pretty/` | Footprints for Waveshare modules |

Each project registers them in its `sym-lib-table` / `fp-lib-table` with
`${KIPRJMOD}`-relative paths, so the project opens on any machine. Edit them only in KiCad.

Check a change with the KLC scripts from
[kicad-library-utils](https://gitlab.com/kicad/libraries/kicad-library-utils):
`klc-check/check_symbol.py --footprints hardware/lib hardware/lib/*.kicad_sym` and
`klc-check/check_footprint.py hardware/lib/*.pretty/*.kicad_mod`. Run them with the Python
that ships with KiCad.

## Parts

### `MCU_Module_Waveshare:ESP32-P4-Core`, `Module_Waveshare:Waveshare_ESP32-P4-Core`

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
  size. The pads are 0.6 × 1.5 mm, reaching 1.0 mm under the body and 0.5 mm past its
  edge, like KiCad's castellated Espressif module footprints. Check them against a real
  module before ordering boards. There are no pads under the module.
- 3D model: referenced as
  `${KICAD10_3DMODEL_DIR}/Module_Waveshare.3dshapes/Waveshare_ESP32-P4-Core.step`
  (KLC F9.3). No model exists yet.
