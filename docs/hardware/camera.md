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
| 4 | GND | GND | 12 | GPIO | CAM_IO1 |
| 5 | MDN1 | CAM_D1_N | 13 | SIO-C | SCCB_SCL |
| 6 | MDP1 | CAM_D1_P | 14 | SIO-D | SCCB_SDA |
| 7 | GND | GND | 15 | 3.3V | CAM_3V3 |
| 8 | MCN | CAM_CLK_N | | | |

### Position

- The module sits behind the board, on the bottom side, with its lens looking out through the Ø16 mm
  board cutout. The lens axis is the cutout centre, (129.7, 130.3) in board coordinates.
- The FFC edge of the module faces north. It lies at y 114.5 (130.3 − 15.8 mm).
- A bottom keep-out keeps the module outline free: x 117.25–142.2, y 114.5–138.35. That is
  25 × 23.86 mm around the lens axis. The neighbours on the bottom side are the PoE module U2 to the
  west (0.5 mm from the nominal module edge) and the RJ45 J1 to the east (0.6 mm). Both gaps shrink
  by 0.2 mm at the module's size tolerance.
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
  tall sits at least 7.0 + 0.83·h mm from the lens axis. Here 7.0 mm = 2.5 mm lens radius + 0.83 × 5.4 mm,
  and 0.83 = tan(79.3°/2). [layout](layout.md#camera-and-csi-2) checks every top-side part against it.

## FFC orientation

J4 is a JUSHUO AFA07-S15FCA-00 (LCSC C262721). It is a 15-pin, 1.0 mm, ZIF, right-angle connector
with **bottom (lower) contacts**: the cable's contacts face the board (JUSHUO drawing AFA07-S\*\*F\*\*,
Rev A, note 4 "F: Lower"). It is on the top side at (129.7, 106.5), rotated 180°, with its opening
facing north into the FFC notch (x 120.7–138.7, y 100–103). Pad 1 (GND) is the east end, at
x 136.7. Pad 15 (CAM_3V3) is the west end, at x 122.7. The silkscreen marks "1" at the east end and
"CONTACTS DOWN" below the connector.

### The cable

The module ships with a white FFC whose contacts are on opposite faces at its two ends. This is the
only cable the board is designed for (user decision, 2026-10-31).

The path from the module to J4:

1. The cable leaves the module connector northward and runs along the board's bottom face, over the
   FFC lane (x 121–139, from the module edge at y 114.5 to the notch at y 103).
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
  the module, fit the top-contact AFA07-S15ECA-00 (LCSC C262742) as J4 instead. The project
  footprint's description gives it the same land pattern.

The Camera sheet's note at J4 points here.

### Bend room

- The cable is 16 mm wide ((15 + 1) × 1.0 mm, JUSHUO "recommended FPC/FFC dimension"). Centred on
  x 129.7, it leaves 1.0 mm to each side of the 18 mm notch.
- J4's closed latch front is at y 103.675, 0.675 mm inside the notch edge. The open latch reaches
  y 102.0.
- The 180° loop has an inner radius of about 1.3 mm. The loop stays inside the board outline
  (y ≥ 100) only if the cable goes at least 3.9 mm into J4. A stiffener about 4 mm long, as on the
  cable in the vendor photo, would make the loop stick out about 1.9 mm past the board edge.
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

Each lane passes through two 0 Ω 0402 links (UNI-ROYAL 0402WGF0000TCE, LCSC C17168).

- Espressif's ESP32-P4 hardware design guidelines (§1.3.15, p.23) recommend reserving series
  resistors on the MIPI lines, fitted with 0 Ω at first. A series resistor can then be added if the
  first boards need one.
- They give each line a probe and cut point.
- They split each lane into a camera half (CAM_\* nets, J4 to link) and a P4 half (CSI_\* nets, link
  to U6). Each half has its own length and skew check.

The links sit at the J4 end in one row at y 112.25, pitch 1.04 mm, in U6 order from west to east:
R51, R47, R45, R49, R50, R46. The planes stay solid under the link pads and J4's MIPI pads. The
excess pad capacitance is estimated at about 0.07 pF per link pad and 0.2 pF per J4 pad (design
review estimate, not measured). The first-board full-rate test covers it.

### Geometry and impedance

- All MIPI pairs are class MIPI_CSI: 0.18 mm tracks with a 0.15 mm gap. They run on F.Cu over the In1
  GND plane and on B.Cu over the In2 GND plane. The stack-up is JLCPCB JLC04161H-7628: 0.2104 mm of
  7628 prepreg, εr 4.4, between each outer layer and its plane ([layout](layout.md#impedance)).
- That geometry is 100 Ω differential nominal. JLCPCB's calculator gives 0.1826 mm for 100 Ω, and an
  independent field solve with solder mask gives 101.1 Ω.
- **No impedance control is ordered** (user decision, 2026-11-01: "No impedance control"). JLCPCB's
  ±20 % track-width tolerance then gives 88–114 Ω: a field solve at 0.144/0.186 mm gives 114.3 Ω and
  at 0.216/0.114 mm gives 87.9 Ω. The extremes fall outside the 100 Ω ± 10 % of Espressif's guidelines
  (§1.4.6). A ±10 % width spread gives 93.8–106.8 Ω.
- Spacing ("MIPI_CSI spacing" and "MIPI_CSI to clocks and high-speed" rules): at least 2W (0.36 mm)
  to everything but the pair's partner, and 3W (0.54 mm) to USB, MDI and the RMII clock (Espressif
  guidelines §1.4.6). The clock pairs run between their two data pairs at 2W, with no GND guard
  (user decision, 2026-11-01: "Accept 2W, no guard").

### Lengths and skew

The figures are measured pad to pad on the routed board, with 1.5512 mm counted per via. The CAM
halves have 2 vias per line and the CSI halves 1.

| Lane | CAM P / N (mm) | CAM skew | CSI P / N (mm) | CSI skew | End-to-end skew |
|---|---|---|---|---|---|
| CLK | 24.799 / 24.830 | −0.031 | 32.436 / 32.436 | 0.000 | −0.031 |
| D0 | 24.758 / 24.829 | −0.071 | 32.501 / 32.474 | +0.028 | −0.044 |
| D1 | 24.756 / 24.644 | +0.112 | 32.493 / 32.493 | 0.000 | +0.112 |

- **P/N skew:** Espressif's guidelines (§1.4.6) allow 10 mil (0.254 mm) per lane. Each lane is two
  routed halves in series, so each half gets 0.127 mm (DRC rule "MIPI_CSI intra-pair skew"). The
  worst half is 0.112 mm (CAM_D1).
- **Lane-to-lane:** the guidelines allow 30 mil (0.762 mm) end to end, so 0.381 mm per half. The
  spread of the pair means is 0.115 mm on the CAM halves and 0.057 mm on the CSI halves.
- **Compensation:** the skew bumps are trapezoids with 45° legs and flats of at least 0.45 mm, so the
  copper path equals the measured net length.
- **Not counted:** the traces inside the ESP32-P4-Core module (unpublished) and inside the camera
  module.

## SCCB and control

### SCCB

- SCCB_SDA (GPIO8) and SCCB_SCL (GPIO9) run to module pins 14 and 13. R41 and R42, 1.8 kΩ 1 % to
  +3V3, pull them up. The bus has one device. Its address is set on the module, and the OV9281
  datasheet v1.1 (p.1-9) lists two 8-bit write addresses, 0xC0 and 0x20.
- **Pull-up range:** R_min = (3.465 V − 0.4 V)/3 mA = 1.02 kΩ ≤ 1.782 kΩ (1.8 kΩ − 1 %), taking +3V3
  at the top of its ±5 % envelope. The sink current is 3.465 V/1.782 kΩ = 1.94 mA.
- **Rise time:** the bus is assumed at 50 pF (sensor 10 pF, P4 pin, 60 mm of board track and the FFC).
  t_r = 0.8473 × 1.818 kΩ × 50 pF = 77 ns, under the 300 ns allowed at 400 kHz (OV9281 §2.9.4, fSCL
  ≤ 400 kHz). The largest pull-up that still meets 300 ns is 7.1 kΩ.
- **Level:** the pull-ups go to +3V3, but the module's SCCB level is unknown (see
  [Accepted risks](#accepted-risks)). If the module turns out to have its own pull-ups, R41/R42 may be
  left out, as long as the parallel value stays ≥ 1.02 kΩ.
- **Bus recovery:** the firmware can clock SCL nine times on GPIO9. CAM_EN (module PWDN) may serve as
  a camera reset once its polarity is measured; until then the SCCB bus clear is the only recovery
  ([firmware-rules](firmware-rules.md#rules)).

### CAM_EN and R43

- CAM_EN (GPIO10) drives module pin 11, "PWDN". R43, 47 kΩ to GND, holds it low.
- GPIO10 has no pull and no input enable at or after reset (ESP32-P4 datasheet v0.7 Table 2-1,
  p.14). So the module stays in its PWDN-low state until the firmware drives the pin.
- What pin 11 does, and its polarity, are unknown. It may be the sensor's active-low XSHUTDOWN, an
  active-high regulator enable or an active-high power-down. Low means "off" in the first two cases
  and "on" in the third. With XSHUTDOWN, holding it low until CAM_3V3 is stable is the order the
  OV9281 asks for. The firmware learns the polarity on the first module
  ([firmware-rules](firmware-rules.md#rules)).

### R52 and CAM_IO1

- Module pin 12 is labelled only "GPIO" on the drawing. On the board it is net CAM_IO1, and it goes
  only to R52.
- R52 is a 0 Ω 0805 link from CAM_IO1 to CAM_EN, **not fitted** (DNP, not in the BOM). It is the
  assembly option for a module that needs pin 12 driven with pin 11.
- With R52 open, pin 12 floats on the board side.

### CAM_3V3

- The module's 3.3 V input (pin 15) is CAM_3V3. It comes from +3V3 through R53, a 0 Ω 0805 jumper
  (UNI-ROYAL 0805W8F0000T5E, rated 2 A). R53 is the one place the camera supply can be opened, to
  measure the module's current or to power it separately.
- C30 (100 nF X7R) and C32 (10 µF X5R 25 V) decouple CAM_3V3 at J4 pin 15, 0.70 mm and 1.09 mm from
  the pad. TP10 ("CAM_3V3") is the test pad.
- **Load:** the OV9281's typical supply currents are IDD-A 24 mA, IDD-IO 2.5 mA and IDD-D 52 mA
  (datasheet Table 8-3), about 80 mA before the module's regulators. The budget takes 50–100 mA.
- **Supply drop:** the +3V3 feed to this corner is about 0.125 Ω, which drops about 19 mV at the
  150 mA of the north-west parts. See [Rails](README.md#rails).
- There is no camera power switch. CAM_EN is the only control.

## XVS level options

XVS is the frame trigger. The P4 drives it from GPIO7 (RMT) at 3.3 V. It passes the divider R40 (series)
and R44 (to GND) to XVS_CAM, then J3 pin 1, a short pigtail and the module's back pad "XVS" (the
sensor's FSIN, high impedance by default, OV9281 Table 1-2). TP8 ("XVS") is on XVS_CAM.

The module's I/O level (DOVDD) is unknown. So the divider is an assembly option, fitted by default
for 1.8 V I/O (user decision, 2026-10-31). The Camera sheet's note points here for the other levels.

Inputs to every option:

- P4 output high: near its rail at the ≤ 1.1 mA these dividers draw, taken over the generic ±5 %
  envelope of +3V3, 3.135–3.465 V. That envelope is wider than the DC set point 3.199–3.440 V and
  the ≥ 3.19 V switchover minimum ([Rails](README.md#rails)), so the results hold in both. The P4
  guarantees only VOH ≥ 0.8 × VDD = 2.508 V (ESP32-P4 datasheet v0.7 p.89), so that floor is checked
  too.
- Sensor input high: VIH = 1.26 V at DOVDD 1.8 V (OV9281 Table 8-3), that is 0.7 × DOVDD, taken here to
  scale with DOVDD.
- Sensor abs max: DOVDD + 1 V on any I/O (OV9281 Table 8-1).
- DOVDD tolerance: assumed ±5 % (the module states none).
- Resistors: 1 %.
- R44 also holds XVS_CAM low while GPIO7 floats during reset.

| Module I/O | R40 | R44 | XVS_CAM high (near rail) | High at VOH 2.508 V | Needs (VIH at DOVDD +5 %) | Abs max (DOVDD −5 % + 1 V) |
|---|---|---|---|---|---|---|
| **1.8 V (fitted)** | 1.5 kΩ (0402WGF1501TCE, C25867) | 1.8 kΩ (0402WGF1801TCE, C25871) | 1.694–1.907 V | 1.356 V | ≥ 1.32 V | ≤ 2.71 V |
| 2.8 V | 1.0 kΩ (0402WGF1001TCE, C11702) | 5.1 kΩ (0402WGF5101TCE, C25905) | 2.612–2.906 V | 2.090 V | ≥ 2.06 V | ≤ 3.66 V |
| 3.3 V | 0 Ω (0402WGF0000TCE, C17168) | 10.0 kΩ (FRC0402F1002TS, C2906861) | 3.135–3.465 V | 2.508 V | ≥ 2.43 V | — |

Calculations (high level = V_GPIO × R44/(R40 + R44), with the resistor extremes taken in the
direction that makes the result worst):

- **1.8 V:**
  - Highest: 3.465 × 1.818/(1.485 + 1.818) = 1.907 V.
  - Lowest: 3.135 × 1.782/(1.515 + 1.782) = 1.694 V.
  - At the VOH floor: 2.508 × 0.540 = 1.356 V.
  - Thevenin resistance 1.5 kΩ ∥ 1.8 kΩ = 818 Ω. With about 60 pF of pad and pigtail, τ ≈ 50 ns: fine
    for a trigger edge.
  - Divider current ≤ 1.06 mA. If the sensor's pin were set up as an output fighting the P4, R40
    limits the current to ≤ 2.3 mA.
  - Low level 0 V, under VIL 0.54 V.
- **2.8 V:**
  - Highest: 3.465 × 5.151/(0.99 + 5.151) = 2.906 V. That is at most DOVDD(min) + 0.25 V, well under
    the 3.66 V abs max.
  - Lowest: 3.135 × 5.049/(1.01 + 5.049) = 2.612 V.
  - At the VOH floor: 2.090 V, above VIH even at DOVDD 2.94 V.
  - Thevenin 836 Ω, current ≤ 0.57 mA.
  - Both values are already on the board's BOM (R39, R1).
  - The plain alternative, R40 = 0 Ω with R44 removed, is not recommended. It puts up to 3.465 V on a
    2.66 V I/O, 0.2 V from the abs max, with nothing to limit the pad current.
- **3.3 V:**
  - Only for a module that level-shifts its back pads to 3.3 V. The OV9281 itself is rated for
    DOVDD up to 3.0 V (Table 8-3), so a true 3.3 V I/O means a shifter on the module.
  - XVS_CAM follows the GPIO. The "needs" column assumes a 3.3 V CMOS input on a rail at the top of
    the ±5 % envelope (0.7 × 3.465 V), because the shifter is unknown.
  - R44 at 10 kΩ keeps only the reset pull-down: 0.35 mA load.

Until the first module shows what CAM_EN does, the module may be unpowered at either CAM_EN level. Up
to 1.9 V on an unpowered I/O ring exceeds the abs max ("DOVDD + 1 V" is then 1 V). So the firmware
keeps XVS (GPIO7) low, RMT idle level low, whenever CAM_EN is in its off state and until CAM_3V3 is
up. Until the CAM_EN polarity is measured, it treats both CAM_EN levels as possibly off and starts
triggers only after an SCCB ACK ([firmware-rules](firmware-rules.md#rules)).

## STROBE

- **Connector:** J3, a JST SM04B-SRSS-TB (1.0 mm SH, 4 pins, side entry, LCSC C160404), carries the
  two back-pad signals. The silkscreen marks it "TRIG".

  | J3 pin | Net | Module back pad |
  |---|---|---|
  | 1 | XVS_CAM | XVS |
  | 2 | GND | GND |
  | 3 | STROBE_CAM | STROBE |
  | 4 | GND | GND |
  | MP | GND | — |

- **Pull-down:** STROBE_CAM is high impedance until the sensor enables the output (OV9281 Table 1-2).
  R48, 10.0 kΩ to GND, holds it low. It loads the sensor output with ≤ 0.3 mA.
- **Buffer:** U7, a TI SN74LV1T34DBV (SOT-23-5, LCSC C100024), is a single buffer with a
  5.5 V-tolerant input and +3V3 supply. It turns the module-level strobe into a 3.3 V signal.
  - Without U7, the P4 could not read a 1.8 V strobe: its VIH is 0.75 × VDD = 2.48 V (P4 datasheet
    p.89).
  - U7's VIH is at most 1.39 V at VCC 3.0–3.3 V and 1.48 V at 3.6 V (TI SCLS743E §6.5), 1.44 V
    interpolated at 3.465 V (top of the +3V3 ±5 % envelope). The sensor's VOH is at least 1.62 V at
    DOVDD 1.8 V (OV9281 Table 8-3), 1.53 V scaled to DOVDD 1.7 V. The margin is ≥ 0.09 V. VOL
    0.18 V is under U7's VIL 0.65 V.
  - U7 works for any module I/O level from 1.7 V to 3.3 V without a part change, and its input stays
    within its 5.5 V rating.
  - C31 (100 nF) decouples U7's supply, 0.62 mm from pin 5 (SCLS743E §9 asks for 0.1 µF). U7 pin 1 is
    NC.
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

  Mitigations: the XVS divider options above, R41/R42 as an option, U7 accepting any level, R43
  holding CAM_EN low, and R52 as an option.
- **SCCB at 3.3 V.** The OV9281 rates SIOC/SIOD to 3.0 V and every I/O to DOVDD + 1 V (2.8 V at DOVDD
  1.8 V). The board pulls SCCB to +3V3 (≤ 3.440 V DC set point, ≤ 3.465 V in the ±5 % envelope) and
  drives PWDN from a 3.3 V GPIO. That is safe only if the module level-shifts these pins or runs
  DOVDD ≥ 2.5 V. The 15-pin port is the Raspberry Pi camera pinout, whose host drives I²C at 3.3 V,
  which suggests the module expects it. A 1.8 V module without a shifter needs a level shifter on the
  next board revision.
- **FFC pin map and mating from vendor material.**
  - The pin table comes from the vendor drawing.
  - That the module connector is bottom-contact comes from a vendor photo (the stiffener on the lens
    side of the cable, the gold contacts visible from the module back). No drawing states it.
  - That the supplied cable has its contacts on opposite faces is the user's statement.
  - The n → n mapping itself follows from the geometry. The first-board continuity check below closes
    this before power is applied.
- **CSI impedance 88–114 Ω, no impedance control** (user decision, 2026-11-01). The first boards must
  pass the full-rate test before the design is released.
- **CSI clock pairs without a GND guard**, 2W to their neighbours (user decision, 2026-11-01).
- **Reference-plane edges near the CSI and CAM pairs**, under the 90 mil of TI SPRAAR7J (p.9):
  - The CSI bus crosses the In2 neck between the lens cutout and J1 at 1.84 mm (72.5 mil) a side, on a
    45° crossing (user decision, 2026-11-01: "Use 45°, accept 71.7 mil").
  - At the FFC notch, CAM_D1_P runs 1.375 mm (54 mil) from the In1/In2 plane edge (user decision,
    2026-11-01: "Pull planes to 0.3 mm").

  See [layout](layout.md#accepted-deviations).
- **Solid planes under the link and J4 pads** (about 0.07–0.2 pF per pad, estimated). Accepted in
  design review, 2026-11-01.
- **FFC loop may stick out about 1.9 mm** past the north board edge, depending on the cable's
  stiffener and J4's insertion depth (unknown).
- **Module-internal CSI lengths unknown.** That share of the lane skew can't be measured on this board.

## First-board checks

1. **Cable and pin 1, before any power.** Plug the supplied cable into the module and lay it straight:
   the free-end contacts must face the lens direction. Fit it into J4, then beep the module's GND back
   pad to J4 pad 1 (east, x 136.7) and its 3V3 pad to J4 pad 15 (west, x 122.7). Never power a board
   with this check failed.
2. **Module levels, on the module alone** (powered from 3.3 V, not yet on the board's SCCB):
   - Measure DOVDD at the sensor's decoupling.
   - Measure the idle voltage on SDA/SCL: it shows any on-module pull-ups and their rail.
   - Power the module with pin 11 low, then high. Measure DOVDD, AVDD and DVDD and check for an SCCB
     ACK. Record pin 11's polarity for the firmware.
   - Then choose the XVS row above and decide on R41/R42.
3. **XVS.** Scope TP8 while the P4 sends triggers: the high level falls in the table's range for the
   fitted option, and each edge starts a frame. Check that XVS stays low while the module is off if
   CAM_EN gates DOVDD.
4. **STROBE.** Scope J3 pin 3 and TP9 during exposure: STROBE_CAM reaches the module's I/O level, STROBE
   reaches 3.3 V, and the ETM capture time-stamps each edge.
5. **CSI at full rate.** Run 2 lanes at the full lane rate (800 Mbit/s per lane) with a frame-error
   count before the design is released. This covers the impedance spread, the pad capacitance and the
   unknown module-internal lengths. If errors show, try a lower lane rate first.
6. **Camera current.** Lift R53, or measure across it, with the sensor streaming. Compare with the
   50–100 mA budget.
7. **Mechanics.** Measure the module connector's real height (2.1 or 2.5 mm). Check the enclosure
   puts the module's front face 2.1–9.3 mm behind the board, with the lens tip no more than 5.4 mm
   below the top face. Measure J4's insertion depth and the cable's stiffener, and check the FFC loop
   against the north edge and the enclosure.
