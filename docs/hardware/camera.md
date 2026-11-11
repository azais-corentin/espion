# Camera

The camera block connects an OV9281 global-shutter module to the ESP32-P4 (U6). Frames travel over
MIPI CSI-2 (two data lanes and a clock lane). The sensor is configured over SCCB. The P4 triggers
each exposure with XVS, and the sensor's STROBE output gates the LED ring and is time-stamped by
the P4.

| Signal | U6 pin | GPIO | Use |
|---|---|---|---|
| XVS | U6.9 | GPIO7 | Frame trigger, RMT output |
| SCCB_SDA | U6.10 | GPIO8 | SCCB data |
| SCCB_SCL | U6.11 | GPIO9 | SCCB clock |
| CAM_EN | U6.12 | GPIO10 | Module pin 11 (PWDN) |
| STROBE | U6.14 | GPIO12 | Exposure strobe, ETM capture |
| CSI_D0_N … CSI_D1_P | U6.28–U6.33 | CSI | MIPI CSI-2 receiver |

Firmware use of these pins: [firmware-rules](firmware-rules.md#gpio-map).

## Camera module

The module is the SZYTF WX-1M-9281-79: an OmniVision OV9281 1 MP monochrome global-shutter sensor
with a 79.3° lens. The firmware runs it at 640 × 400 (2 × 2 binned), RAW8, 120 fps. Its only
mechanical source is the vendor drawing WX-1M-9281-79-V1.0, published as an image on the
[AliExpress listing](https://fr.aliexpress.com/item/1005012566040263.html). The drawing is a
redraw and not to scale.

| Item | Value (drawing) |
|---|---|
| Module PCB | 25 ± 0.2 × 23.86 ± 0.2 mm, 1.6 mm thick |
| Lens | Ø5 mm, tip 5.5 ± 0.2 mm above the module's front (lens) face; 6.5 × 6.5 mm holder |
| Lens position | Centred on the 25 mm width, 15.8 mm from the FFC edge |
| Mounting holes | 4 × Ø2.2 mm on a 21 × 12.5 mm rectangle, 6.25 mm each side of the lens centre line |
| FFC connector | 15 pins at 1.0 mm, on the front (lens) face, at the FFC edge; 2.1 mm tall and 2.5 mm from the edge (the drawing can also be read as 2.5 mm tall and 2.1 mm deep) |
| Supplies | DVDD, AVDD and DOVDD are listed as "NC": the module makes them from its 3.3 V input |
| Interface voltage | Not stated |
| Back pads (vendor photo) | 3V3, XVS, STROBE, IL_PWM, GPIO2, GND |

FFC pin table (drawing), which J4 copies one to one:

| Pin | Module | J4 net | Pin | Module | J4 net |
|---|---|---|---|---|---|
| 1 | GND | GND | 9 | MCP | CAM_CLK_P |
| 2 | MDN0 | CAM_D0_N | 10 | GND | GND |
| 3 | MDP0 | CAM_D0_P | 11 | PWDN | CAM_EN |
| 4 | GND | GND | 12 | GPIO | — (no-connect) |
| 5 | MDN1 | CAM_D1_N | 13 | SIO-C | SCCB_SCL |
| 6 | MDP1 | CAM_D1_P | 14 | SIO-D | SCCB_SDA |
| 7 | GND | GND | 15 | 3.3V | CAM_3V3 |
| 8 | MCN | CAM_CLK_N | | | |

### Position

- The module sits behind the board, on the bottom side, with its lens looking out through the Ø16 mm
  board cutout. The lens axis is the cutout centre, (129.7, 130.3) in board coordinates.
- The FFC edge of the module faces north, 15.8 mm from the lens axis.
- A bottom keep-out keeps the module outline free: 25 × 23.86 mm around the lens axis. The neighbours
  on the bottom side are the PoE module U2 to the west and the RJ45 J1 to the east. Both gaps are
  small, and the module's ±0.2 mm size tolerance shrinks each of them (`kicad_query distance`).
- The board has no fixing for the module. The enclosure holds it by its four Ø2.2 mm holes.

### Heights

Distances are measured from the board's bottom face, toward the back of the product.

- **Module front face ≥ 2.1 mm behind the board.** The module's own FFC connector is on the lens face,
  between the module and the board, and it is 2.1 mm tall (2.5 mm if the drawing is read the other
  way).
- **Lens tip ≤ 5.4 mm below the board's top face** (user decision, 2026-10-31). With the 1.6 mm board
  and the 5.5 mm lens, the module's front face is then at most 5.4 − 1.6 + 5.5 = 9.3 mm behind the
  board.
- So the enclosure must place the module's front face 2.1–9.3 mm behind the bottom face. At 2.1 mm
  the lens tip stands 1.8 mm above the top face, inside the cutout.
- The lens cone for parts on the top side assumes the worst case, the tip 5.4 mm down. A part h mm
  tall must sit at least r0 + tan(79.3°/2) × h from the lens axis, where r0 = lens radius + tan(79.3°/2)
  × 5.4 mm. The numbers are held by the `layout` entry "lens view cone" in `.omp/kicad.json`, which
  checks every top-side part. See [layout](layout.md#camera-and-csi-2).

## FFC orientation

J4 is a JUSHUO AFA07-S15FCA-00. It is a 15-pin, 1.0 mm, ZIF, right-angle connector
with **bottom (lower) contacts**: the cable's contacts face the board (JUSHUO drawing AFA07-S\*\*F\*\*,
Rev A, note 4 "F: Lower"). It is on the top side, centred on the lens axis, rotated 180°, with its
opening facing north into the FFC notch. Pad 1 (GND) is the east end. Pad 15 (CAM_3V3) is the west
end. The silkscreen marks "1" at the east end and "CONTACTS DOWN" below the connector.

### The cable

The module ships with a white FFC whose contacts are on opposite faces at its two ends. This is the
only cable the board is designed for (user decision, 2026-10-31).

The path from the module to J4:

1. The cable leaves the module connector northward and runs along the board's bottom face, over the
   FFC lane between the module edge and the notch.
2. It wraps 180° round the notch edge.
3. It enters J4 from the north on the top side.

Every fold is about an axis parallel to x, so every conductor keeps its x position. Module pin 1
(at the east end when the lens face points at the board) arrives at J4 pin 1 (east). **Module pin n
meets J4 pin n.** The module end of the cable mates with the module connector's bottom contacts. At
the J4 end, the opposite-face contacts face down, onto J4's lower contacts.

- **Never twist the cable.** A half-twist about its long axis mirrors the pin order, so pin n goes
  to pin 16 − n. J4.15 (CAM_3V3) then lands on module pin 1 (GND), which shorts CAM_3V3 to GND.
- **A same-face cable makes no contact.** Its J4-end contacts face up, away from J4's lower contacts.
- **The quick check:** plug the cable into the module and lay it out straight. The contacts at the
  free end must face the same way as the lens. If a replacement cable has them facing the back of
  the module, fit the top-contact JUSHUO AFA07-S15ECA-00 as J4 instead. The project
  footprint's description gives it the same land pattern.

### Bend room

- The cable is 16 mm wide ((15 + 1) × 1.0 mm, JUSHUO "recommended FPC/FFC dimension"). Centred on
  the lens axis, it fits inside the 18 mm wide FFC notch.
- J4's closed latch front sits just inside the notch edge. The open latch reaches further into the
  notch.
- The 180° loop has an inner radius of about 1.3 mm. The loop stays inside the board outline only if
  the cable goes far enough into J4. A stiffener about 4 mm long, as on the cable in the vendor photo,
  would make the loop stick out past the board edge (`kicad_query` gives J4's position against the
  outline).
- Not known: J4's insertion depth, the supplied cable's stiffener length and thickness, and its
  minimum bend radius. The enclosure must allow for the loop.

## CSI-2 link

The sensor drives a clock lane and two data lanes into the P4's CSI-2 receiver, up to
800 Mbit/s per lane (OV9281 datasheet v1.1 §2.3.1). D-PHY needs no AC coupling and no external
termination: the receiver terminates HS mode internally, and LP mode needs a DC path. The P4's
CSI reference resistor and its MIPI PHY supply are on the ESP32-P4-Core module. The P4's DSI pins
are unused.

### Lane mapping

The P4 can't remap lanes or swap P and N. Register 42.2 CSI_HOST_N_LANES offers only "lane 0" or
"lanes 0 and 1" (ESP32-P4 TRM v0.7 p.2611), and §42.5.3.2 (p.2603) documents no remap control. So
the schematic wires lane to lane and P to P:

| U6 pin | U6 net | Link | J4 net | J4 pin | Module signal |
|---|---|---|---|---|---|
| U6.28 | CSI_D0_N | R51 | CAM_D0_N | 2 | MDN0 |
| U6.29 | CSI_D0_P | R47 | CAM_D0_P | 3 | MDP0 |
| U6.30 | CSI_CLK_P | R45 | CAM_CLK_P | 9 | MCP |
| U6.31 | CSI_CLK_N | R49 | CAM_CLK_N | 8 | MCN |
| U6.32 | CSI_D1_N | R50 | CAM_D1_N | 5 | MDN1 |
| U6.33 | CSI_D1_P | R46 | CAM_D1_P | 6 | MDP1 |

The physical order differs at the two ends:

- U6 pads, west to east: D0_N, D0_P, CLK_P, CLK_N, D1_N, D1_P.
- J4 pins, west to east: CLK_P (9), CLK_N (8), D1_P (6), D1_N (5), D0_P (3), D0_N (2).

The copper reorders them on B.Cu between J4 and the link row. The crossing is planar: the D0 and D1
pairs each make a U-turn, which mirrors the pair, and the clock pair swaps its two lines once round a
via. Details are in [layout](layout.md#camera-and-csi-2).

### 0 Ω links R45–R47, R49–R51

Each lane passes through two 0 Ω links.

- Espressif's ESP32-P4 hardware design guidelines (§1.3.15, p.23) recommend reserving series
  resistors on the MIPI lines, fitted with 0 Ω at first. A series resistor can then be added if the
  first boards need one.
- They give each line a probe and cut point.
- They split each lane into a camera half (CAM_\* nets, J4 to link) and a P4 half (CSI_\* nets, link
  to U6). Each half has its own length and skew check.

The links sit at the J4 end in one row, in U6 order from west to east: R51, R47, R45, R49, R50, R46.
The planes stay solid under the link pads and J4's MIPI pads, except where the MIPI via antipads reach
under them ([layout](layout.md#csi-bus-and-the-plane-neck)). That adds a little pad capacitance (design
review estimate, not measured). The first-board full-rate test covers it.

### Geometry and impedance

- All MIPI pairs are net class MIPI_CSI. They run on F.Cu over the In1 GND plane and on B.Cu over the
  In2 GND plane. The stack-up is JLCPCB JLC04161H-7628 ([layout](layout.md#impedance)). Track width
  and gap are set in the net class, and are calculated for 100 Ω differential impedance (the target).
- **No impedance control is ordered** (user decision, 2026-11-01: "No impedance control"). JLCPCB's
  ±20 % track-width tolerance then spreads the impedance outside the 100 Ω ± 10 % of Espressif's
  guidelines (§1.4.6). The method is in [layout](layout.md#impedance); the results come from a field
  solve on the extremes of width and gap.
- Spacing (rules "MIPI_CSI spacing" and "MIPI_CSI to clocks and high-speed"): at least 2W to
  everything but the pair's partner, and 3W to USB, MDI and the RMII clock (Espressif guidelines
  §1.4.6). The clearances are held by those two rules. The clock pairs run between their two data
  pairs at 2W, with no GND guard (user decision, 2026-11-01: "Accept 2W, no guard").

### Lengths and skew

The lengths are measured pad to pad on the routed board, with each via counted at its own length
(`kicad_query` gives them). Both members of a pair carry the same number of vias (TI SPRAAR7J §3.9
p.12), held by the `viaParity` entries "CSI lanes, P4 side" and "CAM lanes, FPC side" in
`.omp/kicad.json`.

- **P/N skew:** Espressif's guidelines (§1.4.6) allow 10 mil per lane. Each lane is two routed halves
  in series, so each half gets half of that. Held by DRC rule "MIPI_CSI intra-pair skew".
- **Lane-to-lane:** the guidelines allow 30 mil end to end, so half of that per half. Held by the
  `lengthGroups` entries "CSI pairs, P4 side" and "CAM pairs, FPC side" in `.omp/kicad.json`, on the
  pair means.
- **Compensation:** the skew bumps are trapezoids with 45° legs, so the copper path equals the
  measured net length.
- **Not counted:** the traces inside the ESP32-P4-Core module (unpublished) and inside the camera
  module.

## SCCB and control

### SCCB

- SCCB_SDA (GPIO8) and SCCB_SCL (GPIO9) run to module pins 14 and 13. R41 and R42 pull them up to
  +3V3. The bus has one device. Its address is set on the module, and the OV9281
  datasheet v1.1 (p.1-9) lists two 8-bit write addresses, 0xC0 and 0x20.
- **Pull-up range:** R_min = (V(+3V3) max − V_OL)/I_OL, with V_OL 0.4 V and I_OL 3 mA (I²C
  standard-mode sink limit) and +3V3 at the top of its ±5 % envelope. R41/R42 at their low tolerance
  extreme must stay above R_min.
- **Rise time:** t_r = 0.8473 × R × C_bus, which must stay under the 300 ns allowed at 400 kHz (OV9281
  §2.9.4, fSCL ≤ 400 kHz). Assumed C_bus: the sensor's 10 pF (OV9281 datasheet), plus the P4 pin, the
  board track and the FFC, estimated at design time (`kicad_query` gives the track length). The
  largest pull-up that still meets 300 ns is 300 ns/(0.8473 × C_bus).
- **Level:** the pull-ups go to +3V3, but the module's SCCB level is unknown (see
  [Accepted risks](#accepted-risks)). If the module turns out to have its own pull-ups, R41/R42 may be
  left out, as long as the parallel value stays ≥ R_min. With R41/R42 fitted, a module pull-up
  R_mod in parallel must keep R41 ∥ R_mod at its low tolerance ≥ R_min; the fee-free R41/R42 value
  (user decision 2026-11-09) leaves little room, so R_mod must be large ([Accepted risks](#accepted-risks)).
- **Unpowered module:** R41/R42 hold SCL/SDA at +3V3 whenever the board is powered, and R43 holds
  module pin 11 low from reset. If that level is the module's off state and pin 11 gates its
  regulators, the sensor's I/O ring is unpowered while both lines sit above its "DOVDD + 1 V"
  limit (OV9281 Table 8-1); each line then feeds up to (V(+3V3) max − V_clamp)/R at R41/R42's low
  tolerance into the I/O clamps, V_clamp being a diode drop above the unpowered DOVDD. Firmware holds
  GPIO8/GPIO9 low while the camera is off (FW-17), but nothing covers reset and boot. A host on the
  Raspberry Pi pinout presents the same condition (inference: Pi-compatible modules tolerate it).
  [Accepted risks](#accepted-risks), first-board check 2.
- **Bus recovery:** the firmware can clock SCL nine times on GPIO9. CAM_EN (module PWDN) may serve as
  a camera reset once its polarity is measured; until then the SCCB bus clear is the only recovery
  ([firmware-rules](firmware-rules.md#rules)).

### CAM_EN and R43

- CAM_EN (GPIO10) drives module pin 11, "PWDN". R43 pulls it to GND and holds it low.
- GPIO10 has no pull and no input enable at or after reset (ESP32-P4 datasheet v0.7 Table 2-1,
  p.14). So the module stays in its PWDN-low state until the firmware drives the pin.
- What pin 11 does, and its polarity, are unknown. It may be the sensor's active-low XSHUTDOWN, an
  active-high regulator enable or an active-high power-down. Low means "off" in the first two cases
  and "on" in the third. With XSHUTDOWN, holding it low until CAM_3V3 is stable is the order the
  OV9281 asks for. The firmware learns the polarity on the first module
  ([firmware-rules](firmware-rules.md#rules)).

### Module pin 12

- Module pin 12 is labelled only "GPIO" on the drawing. J4.12 carries a no-connect flag: nothing on
  the board drives it or pulls it, so pin 12 floats on the board side.
- The board had an unfitted 0 Ω link (R52) from pin 12 to CAM_EN, the assembly option for a module
  that needs pin 12 driven with pin 11. It was removed with its net CAM_IO1 (user decision
  2026-11-11, board simplification); such a module now needs a board change.

### CAM_3V3

- The module's 3.3 V input (pin 15) is CAM_3V3. It comes from +3V3 through R53, a 0 Ω 0402 jumper. R53 is
  the one place the camera supply can be opened, to measure the module's current or to power it
  separately.
- C32 decouples CAM_3V3 at J4 pin 15; the small extra C30 and the test pad TP10 were removed (user
  decision 2026-11-11, board simplification). Probe CAM_3V3 on C32.1.
- **Load:** the OV9281's typical supply currents are IDD-A 24 mA, IDD-IO 2.5 mA and IDD-D 52 mA
  (datasheet Table 8-3). The module's regulators add their own losses. The camera budget is in
  [+3V3 loads](power.md#3v3-loads).
- **Supply drop:** the +3V3 feed to this corner has a small track resistance (L2.2 to R53.1). Its drop
  with the camera and the other north-west loads is worked out in [layout](layout.md#power).
- There is no camera power switch. CAM_EN is the only control.

## XVS level options

XVS is the frame trigger. The P4 drives it from GPIO7 (RMT) at 3.3 V. It passes the divider R40 (series)
and R44 (to GND) to XVS_CAM, then J3 pin 1, a short pigtail and the module's back pad "XVS" (the
sensor's FSIN, high impedance by default, OV9281 Table 1-2). TP8 ("XVS") is on XVS_CAM.

The module's I/O level (DOVDD) is unknown. So the divider is an assembly option, fitted by default
for 1.8 V I/O (user decision, 2026-10-31).

The high level is V_XVS_CAM = V_GPIO × R44/(R40 + R44), worked out with the resistor tolerance
in the direction that makes each result worst. The fitted values are in the schematic.

Requirement for every option: with the divider at its worst case, the high level must

- reach the sensor's VIH at DOVDD +5 %, even when the P4 output sits only at its guaranteed VOH, and
- stay under the sensor's abs max at DOVDD −5 %, with the P4 output at the top of the +3V3 envelope.

Inputs:

- P4 output high: near its rail at the dividers' load, taken over the generic ±5 % envelope of +3V3
  (3.3 V ±5 %). That envelope is wider than the DC set point range in [Rails](README.md#rails), so the
  results hold in both. The P4 guarantees only VOH ≥ 0.8 × VDD (ESP32-P4 datasheet v0.7 p.89), so
  that floor is checked too.
- Sensor input high: VIH = 1.26 V at DOVDD 1.8 V (OV9281 Table 8-3), that is 0.7 × DOVDD, taken here to
  scale with DOVDD. Sensor input low: VIL = 0.54 V at DOVDD 1.8 V, met by the 0 V low level.
- Sensor abs max: DOVDD + 1 V on any I/O (OV9281 Table 8-1).
- DOVDD tolerance: assumed ±5 % (the module states none).
- R44 also holds XVS_CAM low while GPIO7 floats during reset.

Options, by module I/O level (R40 and R44 are the only parts that change):

- **1.8 V (fitted).**
  - The Thevenin resistance (R40 ∥ R44) times the pad and pigtail capacitance must stay fast for a
    trigger edge.
  - If the sensor's pin were set up as an output fighting the P4, R40 limits the current.
- **2.8 V.**
  - The top level must stay well under the abs max, and the VOH floor must still clear VIH even at
    the top of the DOVDD tolerance.
  - The resistor values this needs are already on the board's BOM: R40 stays as fitted, R44 takes R1's value.
  - The plain alternative, R40 as a 0 Ω link with R44 removed, is not recommended. It puts the full
    +3V3 rail on a 2.8 V I/O, close to its abs max, with nothing to limit the pad current.
- **3.3 V.**
  - Only for a module that level-shifts its back pads to 3.3 V. The OV9281 itself is rated for
    DOVDD up to 3.0 V (Table 8-3), so a true 3.3 V I/O means a shifter on the module.
  - XVS_CAM follows the GPIO: R40 is a 0 Ω link, and R44 is large enough to keep only the reset
    pull-down. The VIH requirement assumes a 3.3 V CMOS input (0.7 × the top of the ±5 % envelope),
    because the shifter is unknown.

Until the first module shows what CAM_EN does, the module may be unpowered at either CAM_EN level. The
fitted divider's high level on an unpowered I/O ring exceeds the abs max ("DOVDD + 1 V" is then 1 V). So
the firmware keeps XVS (GPIO7) low, RMT idle level low, whenever CAM_EN is in its off state and until
CAM_3V3 is up. Until the CAM_EN polarity is measured, it treats both CAM_EN levels as possibly off and
starts triggers only after an SCCB ACK ([firmware-rules](firmware-rules.md#rules)).

## STROBE

- **Connector:** J3, a JST SH (1.0 mm, 4 pins, side entry) connector, carries the two back-pad
  signals. The silkscreen marks it "TRIG".

  | J3 pin | Net | Module back pad |
  |---|---|---|
  | 1 | XVS_CAM | XVS |
  | 2 | GND | GND |
  | 3 | STROBE_CAM | STROBE |
  | 4 | GND | GND |
  | MP | GND | — |

- **Pull-down:** STROBE_CAM is high impedance until the sensor enables the output (OV9281 Table 1-2).
  R48 holds it low. It loads the sensor output lightly.
- **Buffer:** U7, a Tech Public TP74LV1T34 (SOT-23-5, the pinout and input thresholds of TI's
  SN74LV1T34), is a single buffer with an input tolerant above its +3V3 supply. It turns the
  module-level strobe into a 3.3 V signal.
  - Without U7, the P4 could not read a 1.8 V strobe: its VIH is 0.75 × VDD (P4 datasheet p.89).
  - U7's VIH is at most 1.39 V at VCC 3.0–3.3 V and 1.48 V at 3.6 V over −40…125 °C
    (TP74LV1T34 datasheet p.3), interpolated at
    the top of the +3V3 ±5 % envelope. The sensor's VOH is at least 1.62 V at DOVDD 1.8 V (OV9281
    Table 8-3), scaled to DOVDD −5 %. The scaled VOH must stay above U7's VIH. VOL 0.18 V is under U7's
    VIL 0.65 V (p.3).
  - U7 works for any module I/O level from 1.7 V to 3.3 V without a part change, and its input stays
    below its 7 V absolute maximum (p.2).
  - C31 decouples U7's supply at pin 5 with the 0.1 µF TI asks for this function (SCLS743E §9).
    U7 pin 1 is NC.
- **Output:** U7 drives STROBE to GPIO12 (U6.14), which the P4 captures with an ETM event. STROBE also
  feeds the LED driver's hardware timeout through R54
  ([led](led.md#strobe-gating-and-timeout)). U7 drives one way only, camera to board. TP9 ("STB") is
  the test pad.

## Accepted risks

- **Module I/O level unknown.** There is no module sample, and the drawing gives no interface voltage.
  So the board can't be designed to a measured level (user decision, 2026-10-31: "No sample: keep the
  assembly options"). The user accepted these open points:
  - the module's DOVDD and the levels of its back pads XVS and STROBE;
  - whether SCCB is level-shifted on the module, and whether the module has its own SCCB pull-ups;
  - what pin 11 (PWDN) does, and its polarity;
  - whether the back pads go straight to the sensor's FSIN and STROBE balls.

  Mitigations: the XVS divider options above, R41/R42 as an option, U7 accepting any level and R43
  holding CAM_EN low.
- **SCCB at 3.3 V.** The OV9281 rates SIOC/SIOD to 3.0 V and every I/O to DOVDD + 1 V (2.8 V at DOVDD
  1.8 V). The board pulls SCCB to +3V3 (up to the top of the +3V3 range in [Rails](README.md#rails))
  and drives PWDN from a 3.3 V GPIO. That is safe only if the module level-shifts these pins or runs
  DOVDD ≥ 2.5 V. The 15-pin port is the Raspberry Pi camera pinout, whose host drives I²C at 3.3 V,
  which suggests the module expects it. A 1.8 V module without a shifter needs a level shifter on the
  next board revision.
- **Module SCCB pull-ups must be weak.** With R41/R42 fitted, any pull-up on the module adds in
  parallel, and the pair must stay ≥ R_min ([SCCB](#sccb)): R_mod ≥ R_min × R_low / (R_low − R_min),
  with R_low the board pull-up at its low tolerance. The fee-free R41/R42 value (user decision
  2026-11-09) sits close to R_min, so only a weak module pull-up fits; a stronger one means
  removing R41/R42 (first-board check 2).
- **SCCB pull-ups into an unpowered module.** If pin 11 low turns the module off, R41/R42 feed the
  sensor's I/O clamps through reset and boot, above the OV9281's DOVDD + 1 V limit
  ([SCCB](#sccb)); firmware holds GPIO8/GPIO9 low only once it runs (FW-17). Accepted on the
  Raspberry Pi precedent until first-board check 2 shows what pin 11 does (design review
  2026-11-09).
- **FFC pin map and mating from vendor material.**
  - The pin table comes from the vendor drawing.
  - That the module connector is bottom-contact comes from a vendor photo (the stiffener on the lens
    side of the cable, the gold contacts visible from the module back). No drawing states it.
  - That the supplied cable has its contacts on opposite faces is the user's statement.
  - The n → n mapping itself follows from the geometry. The first-board continuity check below closes
    this before power is applied.
- **CSI impedance outside 100 Ω ± 10 %, no impedance control** (user decision, 2026-11-01). JLCPCB's
  ±20 % track-width tolerance puts the extremes outside Espressif's limit (§1.4.6). The first boards
  must pass the full-rate test before the design is released.
- **CSI clock pairs without a GND guard**, 2W to their neighbours (user decision, 2026-11-01).
- **Reference-plane edges near the CSI and CAM pairs**, under the 90 mil of TI SPRAAR7J (p.9):
  - The CSI bus crosses the In2 neck between the lens cutout and J1 closer than 90 mil to a plane edge on
    each side, on a 45° crossing (user decision, 2026-11-01: "Use 45°, accept 71.7 mil").
  - At the FFC notch, CAM_D1_P runs closer than 90 mil to the In1/In2 plane edge over part of its
    length (user decision, 2026-11-01: "Pull planes to 0.3 mm").

  See [layout](layout.md#accepted-deviations).
- **Planes under the link and J4 pads**, solid except where the via antipads reach (a small pad
  capacitance, estimated). Accepted in design review, 2026-11-01.
- **FFC loop may stick out** past the north board edge, depending on the cable's stiffener and J4's
  insertion depth (unknown).
- **Module-internal CSI lengths unknown.** That share of the lane skew can't be measured on this board.

## First-board checks

1. **Cable and pin 1, before any power.** Plug the supplied cable into the module and lay it straight:
   the free-end contacts must face the lens direction. Fit it into J4, then beep the module's GND back
   pad to J4 pad 1 (east end) and its 3V3 pad to J4 pad 15 (west end). Never power a board
   with this check failed.
2. **Module levels, on the module alone** (powered from 3.3 V, not yet on the board's SCCB):
   - Measure DOVDD at the sensor's decoupling.
   - Measure the idle voltage on SDA/SCL: it shows any on-module pull-ups and their rail.
   - Power the module with pin 11 low, then high. Measure DOVDD, AVDD and DVDD and check for an SCCB
     ACK. Record pin 11's polarity for the firmware.
   - With pin 11 low, pull SDA and SCL to 3.3 V through the board's R41/R42 value and measure DOVDD
     and the SDA/SCL voltage. If DOVDD is off and SDA/SCL sit near 3.3 V, the lines feed an unpowered
     I/O ring ([SCCB](#sccb)): before the board takes the module, drop R41/R42 for module pull-ups, or
     move R43 to +3V3 so the module is on from reset.
   - Then choose the XVS option above and decide on R41/R42.
3. **XVS.** Scope TP8 while the P4 sends triggers: the high level meets the requirement of
   [XVS level options](#xvs-level-options) for the fitted option (worked out from the schematic), and
   each edge starts a frame. Check that XVS stays low while the module is off if CAM_EN gates DOVDD.
4. **STROBE.** Scope J3 pin 3 and TP9 during exposure: STROBE_CAM reaches the module's I/O level, STROBE
   reaches 3.3 V, and the ETM capture time-stamps each edge.
5. **CSI at full rate.** Run 2 lanes at the full lane rate (800 Mbit/s per lane) with a frame-error
   count before the design is released. This covers the impedance spread, the pad capacitance and the
   unknown module-internal lengths. If errors show, try a lower lane rate first.
6. **Camera current.** Lift R53 (0402) and fit a 0.1 Ω 0402 shunt, or measure across it, with the
   sensor streaming. Compare with the camera budget in [+3V3 loads](power.md#3v3-loads).
7. **Mechanics.** Measure the module connector's real height (2.1 or 2.5 mm). Check the enclosure
   puts the module's front face 2.1–9.3 mm behind the board, with the lens tip no more than 5.4 mm
   below the top face. Measure J4's insertion depth and the cable's stiffener, and check the FFC loop
   against the north edge and the enclosure.
