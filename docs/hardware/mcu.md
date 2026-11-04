# MCU block

The Waveshare ESP32-P4-Core module (U6) and what sits around it on the board: its supply
decoupling, the USB-C port for power, flashing, logs and JTAG, the reset and boot buttons,
the RGB status LED, the PPS loop-back and the test points. Pin use and the firmware side:
[firmware-rules.md](firmware-rules.md#gpio-map). Placement and routing:
[layout.md](layout.md#mcu-and-usb). Sheet: `mcu.kicad_sch`.

## Module

**Part.** U6, Waveshare ESP32-P4-Core, 16 MB flash version (SKU 36096): ESP32-P4NRW32X
(chip revision v3.x) with 32 MB PSRAM in the package, 16 MB flash, 40 MHz and 32.768 kHz
crystals, 76 castellated pads at 1.0 mm pitch, 24 × 20 mm, 3.30 mm overall height
([Waveshare documentation](https://docs.waveshare.com/ESP32-P4-Core)). It has no LCSC part
number: it is bought from Waveshare and soldered by hand (user decision 2026-10-31).

**Side.** U6 sits on the bottom side, with the CSI pins facing north toward the camera
connector and the RMII pins east toward the PHY (user decision 2026-10-31: hand-soldered
parts on the bottom, everything machine-placed on the top). It is locked in place.

**Hand-soldering footprint.** U6 uses `Module_Waveshare:Waveshare_ESP32-P4-Core_HandSolder`,
not the reflow variant:

- The pads run under the module and well past its edge, so an iron tip reaches each
  castellation from the side. The reflow variant's pads end only just past the edge.
- Copper and solder mask only, **no paste layer**: the stencil has no apertures for U6, and
  JLCPCB's assembly leaves it out. This is why the footprint is exempt from KLC's
  "missing paste" check (F6.1/F6.3 waiver in `hardware/lib/README.md`).
- No pads under the module.

**Soldering.** U6 is soldered after JLCPCB's top-side reflow, with an iron, pad by pad, from
the bottom. No hot air and no second reflow on U6 (user decision 2026-11-01: "Iron only, pad
by pad"). Reason: many top-side parts lie directly over or beside U6's pads (among them
C27, C28, C29, C46, C47, C50, U12, R29, R31–R34, R36–R38 and SW1), and heating the whole
module area from below would reflow them; some even join a U6 pad through a via (R37 to
U6.67, C28/C29 to U6.74–76).

**Provisional land pattern.** Waveshare publishes the outline and the pad pitch but not the
pad size. The pad length and width are an estimate; check them against a real module before
ordering boards ([First-board checks](#first-board-checks)).

**Supply.** VBAT (pin 74) and ESP_3V3 (pins 75, 76) are all on +3V3; the module's own
regulators and decoupling take it from there (Waveshare module schematic, p.1). The P4's I/O
domains all run from ESP_3V3, so every board signal on U6 is a 3.3 V signal. The load on
+3V3 from the module is the sum of the ESP32-P4 HDG figures: 380 mA minimum operating supply
current including flash and PSRAM, plus 50 mA for the MIPI PHY and 20 mA for the USB PHY.
Rail limits in [README](README.md#rails).

**Decoupling.** C29 (bulk) and C28 (high-frequency) at the supply pins, on the top side:
C29 over pins 74–76, C28 on the +3V3 trunk next to pin 76, both beside the +3V3 via pair
that feeds the pins ("as close as possible", ESP32-P4 HDG §1.3.3; no distance given, the
project uses the "close" heuristic of the `.omp/kicad.json` `near` entries C29.1 → U6.75 and
C28.1 → U6.76). These add to the module's own capacitors. Each GND pin (20, 27, 34, 55, 73)
has its own GND via; U6.73 shares one with C29's and C28's ground ends on purpose. +3V3
tracks are net class `PWR_3V3`, whose width comes from ESP32-P4 HDG §1.4.1 (minimum track
width for the supply).

**Unused module functions.** MIPI DSI (pins 21–26), USB 2.0 HS (pins 35/36), the module's LDO
outputs ESP_LDO_VO1–VO4 (pins 51–54; VO3 feeds the MIPI PHY inside the module, see
[firmware-rules.md](firmware-rules.md#rules)), GPIO0/1 (the module's 32.768 kHz crystal) and
the spare GPIOs listed in the [GPIO map](firmware-rules.md#gpio-map).

## USB-C

One USB 2.0 full-speed port straight to the P4's USB Serial/JTAG (no hub): bench power,
flashing, console and JTAG. Power path from VBUS onward: [power.md](power.md#usb-vbus).

| Part | Role | Decision and reason |
|---|---|---|
| J2 HRO TYPE-C-31-M-12 | USB-C receptacle, top edge | Machine-placed by JLCPCB, shell overhanging the board edge by HRO's recommended amount (HRO TYPE-C-31-M-12 drawing p.1, PCB EDGE line; user decision 2026-11-04: "HRO recommended", "Keep machine-placed, add an order note"), so a plug overmold seats fully. JLCPCB Economic PCBA keeps parts ≥ 0.3 mm from routed edges, hence an order note ([layout](layout.md#assembly)). SBU pins open, shield and GND pins to GND |
| R25 (CC1), R23 (CC2) | Rd pull-downs | Make the board a USB-C sink (Rd per the USB Type-C specification), so a Type-C source turns VBUS on. No USB PD |
| U5 ST USBLC6-2SC6 | ESD clamp on D+/D− at J2 | 2.5–3.5 pF line to GND (ST datasheet p.2), negligible at 12 Mbit/s. Its supply pin 5 is on +3V3 with decoupling C25, not on VBUS as ST draws it: on VBUS, the P4's D+ pull-up would back-power an unplugged VBUS through U5's upper diode. On +3V3 an unplugged VBUS stays at 0 V, and D2 clamps VBUS |
| D2 TI TSD05 | VBUS TVS at J2's VBUS pads | VRWM 5.5 V ≥ the 5.5 V VBUS maximum the design assumes, IEC 61000-4-2 ±30 kV contact (TI SLVSH43C §5.1, §5.7). Close to both VBUS pad pairs (`near` entries D2.1 → J2.A4 and J2.A9). Same part as D16 (one BOM line) |
| R31 (D+), R32 (D−) | Series resistors at U6 pins 38/37 | ESP32-P4 HDG (USB section): "22/33 Ω series resistor … close to the chip end"; `near` entries R31.2 → U6.38 and R32.2 → U6.37 |
| R27 / R28, C26 | `VBUS_SENSE` divider to GPIO6 | Tells the firmware a USB host is supplying VBUS, so it can turn the D+ pull-up off without one ([firmware-rules.md](firmware-rules.md#rules)). Digital level only |

**USB pins.** Module pins 37/38 (USB1P1_N/P) are GPIO24/25, the P4's USB 1.1 full-speed PHY,
which is USB Serial/JTAG by default (ESP32-P4 datasheet v0.7 p.28 note 1). D+ = J2.A6/B6 →
U5 → R31 → U6.38, D− = J2.A7/B7 → U5 → R32 → U6.37.

**`VBUS_SENSE` levels.** `VBUS_SENSE` = VBUS × R28 / (R27 + R28), with the tolerance of
both resistors. Over the VBUS range of the [README](README.md#rails) and the +3V3 set-point
range the pin must stay between the P4's VIH and VDD + 0.3 V; the derivation is in
[power.md](power.md#usb-vbus). The Thevenin resistance R27 ∥ R28 limits any current into the
pad to the highest pin voltage divided by R27 ∥ R28, also while +3V3 is still off. Filter
time constant: (R27 ∥ R28) × C26, a slow edge; the firmware debounces it.

R24 from VBUS to GND belongs to the VBUS supply path ([power.md](power.md#usb-vbus)).

**Routing.** The connector-side lines (J2 → U5 → R31/R32) are long and mostly uncoupled;
the module-side lines are short. Both lines flow through U5 with no stub (ST DS11265 Fig. 18;
TI SLVA680A p.4): in at one pin (U5.6 / U5.4), a link under the package, out at the other
(U5.1 / U5.3). At full speed (rise time ≥ 4 ns) the lines are electrically short, so
impedance and length matching do not matter here; the `USB` net class still carries the 90 Ω
differential geometry. Details: [layout.md](layout.md#mcu-and-usb).

## Reset and boot

Both buttons are XKB TS-1187A on the bottom board edge, labelled "BOOT" and "RST" between
them (user decision 2026-11-01: "Keep on the bottom edge"). SW2 is BOOT, SW1 is RESET. The
enclosure must leave both reachable. The "B" keystake option is brass (XKB TS-1187A drawing,
material table), so a discharge to the button can reach the contact net.

**Reset (SW1).** `ESP_EN` (U6.1) goes straight to the P4's CHIP_PU; the module has no RC on it
(Waveshare module schematic p.1). The board adds R33 to +3V3 and C27 to GND, Espressif's
recommendation "R = 10 kΩ and C = 1 µF" (ESP32-P4 HDG, chip power-up and reset timing):

- τ = R33 × C27; CHIP_PU reaches 0.75 × VDD after τ × ln 4 (also with C27 at its low
  tolerance), which must be far above t_STBL 50 µs (ESP32-P4 datasheet Table 2-14 p.34) and
  after the rails are stable, as t_SU ≥ 0 ms asks (Table 3-2 p.37). The straps are static
  resistors, so they stay valid for the 3 ms hold t_H after CHIP_PU rises and beyond.
- SW1 shorts C27, so a reset lasts as long as the press, much longer than t_RST 1 ms.
- A contact discharge to SW1 (IEC 61000-4-2, 8 kV × 150 pF) lands on C27: step ΔV = Q / C27
  with Q = 150 pF × 8 kV.

**Boot (SW2).** GPIO35 is the boot-mode strap and also RMII TXD1 (`RMII_TXD1_MCU`):

- R38 to +3V3 (plus the pad's internal pull-up at reset) makes it 1: SPI boot. The other
  load, the IP101G TXD1 input through R36, is high impedance with no pull.
- SW2 pulls it through R39 against R38 ∥ the internal pull-up (45 kΩ typical, ESP32-P4
  datasheet Table 5-4 p.90): V(GPIO35) = VDD × R39 / (R39 + R38 ∥ R_pu), at the top of the
  +3V3 set-point range ([README](README.md#rails)), must stay below VIL = 0.25 × VDD
  (Table 5-4 p.89). With GPIO36 = 1 (pull-up on the module) that is joint download boot
  (ESP32-P4 datasheet Table 3-3 p.37). To enter it: hold BOOT, press and release RESET,
  release BOOT. The host can also reset into download mode over USB (CDC RTS/DTR), without
  buttons.
- R39 sits at the strap, so the long `BOOT_SW` line hangs off the 50 MHz TXD1 net only
  through R39 (path "BOOT stub" in `.omp/kicad.json`). Its load (D16's 19 pF plus the line)
  behind R39 gives a time constant of (R39 + R38 ∥ R_pu) × (C_D16 + C_line), nothing like the
  large strap capacitor the HDG warns about.
- D16 TI TSD05 at SW2 pin 1, GND via at its anode (`near` entry D16.1 → SW2.1): clamps a
  discharge to the brass button at 7.5 V at 16 A (TLP), and R39 limits the current into
  GPIO35 to (V_clamp − the pad's maximum input voltage) / R39 (TI SLVSH43C p.4, p.6).
- SW2 is not a runtime button: while it is held, any reset (watchdog, brown-out, host RTS/DTR)
  enters download mode ([firmware-rules.md](firmware-rules.md#rules)). While the firmware
  drives TXD1 high, SW2 only adds a load of VDD / R39 through R39 (dissipation VDD² / R39,
  against R39's rating), which the P4's output (20 mA default drive, ESP32-P4 datasheet p.17
  note 3) holds. Whether Ethernet frames survive it is a first-board check.

## Status LED

**Part.** D1 Yongyu YY-0807BRGK26, a 2.0 × 1.7 × 0.85 mm RGB LED with a WS2812-type
one-wire controller, driven by RMT from GPIO13. It replaced the YY-0807RGBJ26, which
JLCPCB lists as "Wave Soldering", i.e. placed by hand (user decision 2026-11-01: "Switch to
YY-0807BRGK26-07-FI"; JLCPCB part page for the LED read 2026-11-01: "SMT Assembly", "Economic
and Standard"). Same package, pinout (1 DOUT, 2 VDD, 3 DIN, 4 GND) and suggested pad (datasheet
A.0 p.2), so the footprint `LED_SMD_Yongyu:LED_Yongyu_YY-0807_2.0x1.7mm` stayed.

**Circuit.** VDD on +3V3 with decoupling C24 (datasheet application circuit p.7). Data: GPIO13 →
R30 series resistor → DIN, with R26 to GND holding DIN low while GPIO13 floats
through reset (the pad has no reset pull). DOUT is open: one LED, no chain.

**Levels and timing** (datasheet A.0, https://datasheet.lcsc.com/datasheet/pdf/f10321733b2b8242740cb1ad891264de.pdf):

- DIN VIH 0.7 × VDD (p.4) at the top of the +3V3 set-point range, against the P4's
  VOH ≥ 0.8 × VDD on the same rail (ESP32-P4 datasheet Table 5-4 p.89); logic input
  ≤ 5.5 V (p.3).
- Bit times T0H 0.3 / T1H 0.6 / T0L 0.6 / T1L 0.3 µs ± 0.05 µs, reset ≥ 80 µs (p.6). The
  datasheet contradicts itself on speed: 800 kHz typical, 1100 kHz maximum (p.5), against
  0.90 µs (1.11 MHz) for the nominal bit times; the firmware uses 0.95 µs bits inside both
  limits. 24 bits MSB first, sent in the order G, R, B (p.7) although the name says "BRG".
  Firmware details: [firmware-rules.md](firmware-rules.md#rules).
- Each colour sinks a fixed 5 mA (p.4), 0.5 mA quiescent; all three colours on is the maximum
  load from +3V3. Off after power-up (p.2).

**Supply margin (accepted).** The datasheet gives VDD 3.0–7.5 V as its rating (p.3) but
characterises only 5 V typical (p.4). Green and blue have VF up to 3.2 V at 5 mA (p.4), at or
above the lower end of +3V3 (set-point range in [README](README.md#rails); the decision below
was taken against the ±5 % envelope's lower end), so at the low end of the rail green and blue
may lose the headroom their constant-current outputs need and look dimmer. Accepted rather
than moving D1 to +5V with a level shifter (user decision 2026-11-02: "Accept, check on first
boards").

## Test points

| Ref | Signal | Purpose |
|---|---|---|
| TP6 | `PPS` (GPIO32) | Scope point for the PTP pulse per second |
| TP7 | GND | Ground clip for TP6 and the RMII area. RH-5002 wire loop in a plated hole, hand-soldered. Its "GND" label is on the bottom silkscreen, the top has no free spot beside it |

**PPS loop-back.** GPIO32 drives the EMAC's PTP PPS output (GPIO-matrix signal
`emac_ptp_pps_pad_out`, ESP32-P4 TRM p.1007); R34, a link, ties it to GPIO33, which a GPIO
ETM event channel captures (TRM p.1017). It is not confirmed that a GPIO ETM event fires on a
pad the EMAC drives as an output; with the loop-back the capture works either way, and
removing R34 frees GPIO33.

**Other probe points on the top side** (part pads, none under another part; the camera module
sits behind the board): `ESP_EN` on C27/R33, `BOOT_SW` on R39, `PPS_IN` on R34, VBUS on
R24/R27, `VBUS_SENSE` on C26, the accelerometer bus on R73/R74. `MDC` and `IMU_INT1` have no
part on them, but U6's hand-soldering pads reach past the module (pads 43 and 69), so
they can be probed from the bottom. Rail test points: [power.md](power.md).

## Accepted risks

- **Provisional U6 pads.** Pad size estimated, as Waveshare publishes none; checked against
  a real module before ordering.
- **Heat into top-side parts while U6 is soldered** (user decision 2026-11-01: iron only, pad
  by pad). Many top-side parts lie over U6's pads; no source gives the through-board
  heating of a 1.6 mm FR-4 board.
- **Module underside over board copper.** B.Cu tracks and tented vias run under U6's body; most
  of these vias are too close to pads for JLCPCB to plug them. Waveshare's drawing does not
  show whether the module's underside has exposed copper. The copper under the body carries
  only nets the module already has at its pads (+3V3, GND, signals ≤ 3.3 V) and +5V (the buck
  branch and B.Cu pours); +32V was routed out from under the module (user decision
  2026-11-03, [layout](layout.md#32v-and-the-ring)), so a mask or tenting fault there can't put
  the LED supply on the module.
- **Tall capacitors beside small parts.** R37 and R38 (0402) sit closer to C47, and R34 closer
  to C46, than the small-part spacing allows next to a tall electrolytic capacitor: possible
  reflow shadowing; JLCPCB's spacing table has no electrolytic row (find the pairs with
  `kicad_query distance`).
- **D1 on +3V3** (user decision 2026-11-02): green and blue may be dim at the low end of the
  rail. D1's byte order G, R, B (datasheet p.7) contradicts its name and is confirmed on the
  first board.
- **J2 overhangs the board edge** (user decision 2026-11-04). It sits nearer the routed
  edge than JLCPCB's ≥ 0.3 mm for parts, so the order carries a note: no break-off tab, rail
  or neighbouring board within the overhang along J2 ([layout](layout.md#assembly)).
- **USB routing.** The full-speed lines are narrow, long and mostly uncoupled, and the D−
  via at J2's side has its nearest GND via farther than the ≤ 1 mm return-via limit (no
  legal site; `kicad_query distance`): electrically short at 12 Mbit/s, accepted in design
  review (2026-11-01).
- **U5 supply on +3V3**, a deviation from ST's application circuit (reason under
  [USB-C](#usb-c)).
- **RC-only reset.** Espressif warns that an RC on CHIP_PU may not reset cleanly with slow
  supply ramps or quick power cycling (for example a fast PoE ↔ USB re-plug before C27 has
  discharged).

## First-board checks

1. **U6 pads:** before ordering, lay a real module on a printout of the footprint; at assembly
   check every castellation wets its pad.
2. **U6 soldering heat:** after soldering U6 pad by pad, inspect R37, C28/C29, C50, U12, SW1
   and the 0402s over the module ring for reflowed or shifted joints. If any moved, lower the
   iron temperature or dwell, or heat-sink the top side.
3. **Module underside:** inspect it before soldering; if it has exposed pads or test points
   over the board's vias or tracks, add Kapton there.
4. **Reset:** scope +3V3 and `ESP_EN` at power-up and at a fast unplug/replug on USB and on PoE;
   the P4 must start every time. Press RESET: the board restarts.
5. **Boot:** hold BOOT, press RESET: the board enumerates as USB Serial/JTAG in download mode;
   released, it boots from flash. Flash and log over USB. Then, with the firmware running and
   Ethernet linked, hold BOOT during an iperf run and check for lost or corrupt frames.
6. **USB-C:** plug in each cable in use (charger, flashing cable): it latches fully and
   enumerates. Check the panel left J2's shell and the board edge in front of it undamaged.
7. **`VBUS_SENSE`:** measure GPIO6 with a USB host (VBUS × R28 / (R27 + R28) at the measured
   VBUS) and without (0 V); watch the edge for bounce when plugging.
8. **D1:** set full red, then green, then blue at +3V3 and judge brightness; confirm the byte
   order G, R, B; scope DIN at the chosen 0.95 µs bit timing and check all three colours latch
   correctly.
9. **PPS:** scope TP6; check the ETM capture on GPIO33.
10. **TP7:** test-fit an RH-5002 in its plated hole (the maker gives no hole size).
11. **AOI/visual:** R37, R38 and R34 joints next to C46/C47.
