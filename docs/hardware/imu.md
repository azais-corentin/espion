# Accelerometer block

Motion sensing for the camera board: a bump flags that the rig calibration may be stale, and
the gravity vector helps rig calibration. Sheet: `imu.kicad_sch`. Firmware side:
[firmware-rules.md](firmware-rules.md#rules). Land pattern and routing constraints:
[layout.md](layout.md#imu).

## Accelerometer

**Part.** U12 MEMSensing MSA3S02: 3-axis accelerometer, ±2/4/8/16 g,
12-bit, output data rate 1–1000 Hz, one interrupt pin, I²C, LGA-12 2 × 2 × 0.95 mm
([datasheet V1.0, May 2024](https://datasheet.lcsc.com/datasheet/pdf/17622eabedb09583cf1836f55c7a0a26.pdf)
pp.2, 6–7). VDD 1.62–3.6 V, absolute maximum 3.6 V; 110 µA in normal mode; 3 ms start-up from
power off (p.7 Table 4).

**Why this part, no gyroscope.** The board was designed around a 6-axis ST LSM6DS3. JLCPCB
lists it, and every ST LGA IMU found in its parts library, for Standard PCBA only (JLCPCB
part page, read 2026-10-31), while this board is built with Economic PCBA. The MSA3S02 is
"SMT Assembly", "Economic and Standard" (JLCPCB part page, read 2026-10-31). The user chose it
and dropped the gyroscope (user decision 2026-10-31: "Let's go with the C51928204, no
gyroscope."); the other options were a genuine TDK ICM-42688-P (≈ $20 each), an ICM-42688-P
listing of unknown provenance, hand-soldering the LSM6DS3, or an MPU-6050. The board therefore
measures no rotation rate: bumps come from the accelerometer's active (slope) interrupt (§4.5.5
p.13), the gravity vector from the static acceleration. Sensor axes: datasheet §8.2 p.32; U12
sits on the top side at 0°, pin 1 at the north-west corner.

**Pins** (datasheet p.5 Table 1, application circuit p.30 Fig. 8):

| Pin | Function | Board |
|---|---|---|
| 7 | VDD | +3V3, decoupling C50 to GND |
| 9 | GND | GND |
| 4, 8 | NC, "connected to GND recommended" | GND |
| 1, 3, 6, 10, 11 | NC | open |
| 2 | SDA | `IMU_SDA`, GPIO3 |
| 12 | SCL | `IMU_SCL`, GPIO4 |
| 5 | INT1 | `IMU_INT1`, GPIO51 |

**Supply.** +3V3 (set-point range in [README](README.md#rails)) is inside the 1.62–3.6 V
operating range; at the top of the rail it stays under the 3.6 V absolute maximum (margin:
3.6 V minus the set-point maximum). The board has no separate VDDIO: the package has no such
pin.

**Decoupling.** C50 at VDD, as Fig. 8 p.30 draws it, close to U12's pads (`near` entry
C50.1 → U12.7). No GND via fits within 1 mm of C50's ground end (U6's pads lie under it on the
bottom and the nearby +32V copper leaves no site); an F.Cu pour joins C50.2 and U12.4 to a
GND via. Its loop to U12's GND pin 9 is C50.2 → pour → GND via → In1 → second GND via → U12.9;
U12.4 is an NC pin and gives no return. Harmless for a 110 µA sensor (p.7); accepted in
design review (2026-11-01).

**INT1.** Push-pull, active low and non-latched after reset (registers 0x20 = 0x00 and
0x21 = 0x00, p.24), so the line needs no pull-up. It runs under U12's body to U6 pin 69
(GPIO51) directly below. Interrupt sources (active, tap, orientation, freefall,
new data) are mapped to INT1 by registers 0x19/0x1A (p.12).

**Placement.** U12 is on the top side over U6's south pad row (U6 is on the bottom), close to
the 3.3 V buck inductor L2 and to the nearest +32V copper (C47's pad). The
MSA3S02 is a capacitive MEMS sensor read over I²C, with the In1/In2 GND planes between its
F.Cu side and the B.Cu tracks under it. No vias under the LGA pads other than INT1's at the
body centre, clear of every pad; the datasheet gives no keep-out under the package.

**Land pattern.** MEMSensing publishes no land pattern (pp.31–34), so the footprint
`Package_LGA_MEMSensing:MEMSensing_LGA-12_2x2mm_P0.5mm_LayoutBorder2x4y` was calculated from
the package drawing (p.31), covering every terminal at its worst-case size. The eight corner
lands are shortened at their inner end so that every gap between pads of different
nets meets JLCPCB's SMD pad-to-pad clearance for different nets; the custom rule
"U12 LGA pads" holds it. Reflow peak 260 °C (p.34) covers JLCPCB's 255 ± 5 °C. Details:
[layout.md](layout.md#imu).

## I2C bus

**Bus.** GPIO3 (SDA) / GPIO4 (SCL) on its own P4 I²C controller; the camera's SCCB uses the
other one. U12 is the only device: 7-bit address 0x62 (1100010b), fixed, no address pin
(p.28 Table 12); f_SCL ≤ 400 kHz (p.28 Table 13).

**Pull-ups.** R73 (SCL) and R74 (SDA) to +3V3, which is the sensor's VDD (Fig. 8 p.30 shows
4.7 kΩ; p.28 says pull up to VDD). Their value follows from these checks:

| Value | Formula, inputs |
|---|---|
| Lowest allowed pull-up | R_p,min = (VDD − VOL) / IOL, with VDD at the top of the +3V3 set-point range, VOL 0.4 V, IOL 3 mA (NXP UM10204 §7.1) |
| Sink current | VDD / R_p |
| Bus capacitance (estimate) | C_b = P4 pin 2 pF (ESP32-P4 datasheet Table 5-4 p.89) + MSA3S02 pin ≤ 10 pF (not published; I²C Fast-mode limit per pin, UM10204 Table 10) + track and via capacitance (track length from `kicad_query`) + module track |
| Rise time 30–70 % | t_r = 0.8473 × R_p × C_b (UM10204 §7.1) ≤ 300 ns (Fast mode, UM10204 Table 10) |
| Largest C_b for 300 ns | C_b,max = 300 ns / (0.8473 × R_p) |

**Levels.** MSA3S02 VIL 0.3 × VDD / VIH 0.7 × VDD (p.7 Table 4) against the P4's VOL 0.1 × VDD
/ VOH 0.8 × VDD; P4 VIL 0.25 × VDD / VIH 0.75 × VDD (ESP32-P4 datasheet Table 5-4 p.89) against
the MSA3S02's VOL 0.1 × VDD. All on the same +3V3.

**Routing.** SDA and SCL leave U6 on B.Cu and stay on B.Cu
under the buck inductor L2 (In1/In2 GND between them and L2's switch node), coming up to F.Cu
only east of L2, away from the switch-node copper (TI TLV62569 SLVSDG1C §10.1 p.13: keep
traces away from switch nodes). SDA's last diagonal still crosses the corner of L2's courtyard
margin to reach U12 pin 2. Details: [layout.md](layout.md#imu).

**Recovery.** U12 has no reset pin and no power switch. The firmware clocks SCL 9 times, sends
a STOP and soft-resets the sensor (register 0x00, p.20) at init
([firmware-rules.md](firmware-rules.md#rules)).

## Accepted risks

- **No gyroscope** (user decision 2026-10-31): bump detection and gravity only, from the
  accelerometer.
- **Calculated land pattern.** No maker land pattern exists; the LGA joints can't be inspected
  visually.
- **R74 close to U12.** R74's pad is closer to U12 pin 2 than JLCPCB's recommended 1.0 mm
  between a chip part and a QFN-type package
  (https://jlcpcb.com/help/article/minimum-spacing-for-smd-components, read 2026-10-31). R74
  can't move further away: turned, it hits C50's GND via or L2. Accepted in design review
  (2026-11-01). C50 and R37 must meet the 1.0 mm and R73 stays clear of it (find the pairs
  with `kicad_query distance`).
- **C50's ground path** has no GND via within 1 mm of its ground end, and its loop to U12.9
  runs through In1 and a second via (accepted in design review, 2026-11-01).
- **Heat from U6 soldering.** U12 and C50 lie over U6's pads, which are iron-soldered from the
  bottom after reflow ([mcu.md](mcu.md#module)).
- **Next to tall parts.** U12's courtyard touches L2's and sits close to C47's (a
  tall electrolytic capacitor): possible reflow shadowing; no source gives a number.
- **Estimated bus capacitance.** The MSA3S02's pin capacitance is not published; the rise-time
  margin rests on the I²C 10 pF per-pin limit.
- **Supply headroom.** +3V3 at its set-point maximum ([README](README.md#rails)) is close
  under the sensor's 3.6 V absolute maximum.

## First-board checks

1. **Presence:** read register 0x01 (PARTID) at address 0x62: 0x13 (datasheet p.19). Repeat
   after U6 has been soldered, to catch heat damage or a lifted joint.
2. **Rise time:** scope SCL and SDA at 400 kHz; 30–70 % rise ≤ 300 ns, and near the t_r the
   formula above gives for the measured bus.
3. **INT1:** enable the active interrupt, tap the board: GPIO51 goes low.
4. **Gravity and axes:** board flat, then on each edge: ≈ 1 g on the expected axis with the
   expected sign (axes per datasheet p.32), to fix the board-to-camera axis mapping in firmware.
5. **Noise:** compare the output noise with the LED ring pulsing at 0.5 ms / 120 fps and with
   the LEDs off (buck inductor and +32V copper nearby).
6. **Joints:** if any check fails, X-ray or reflow-inspect U12's LGA joints and the land gaps
   for bridges.
