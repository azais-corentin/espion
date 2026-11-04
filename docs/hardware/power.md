# Power

The Power sheet (`power.kicad_sch`) turns PoE or USB-C into the board's two supplies: `+5V` for the
LED driver and the buck, and `+3V3` for everything else. The chain: J1's PoE centre taps → U2
DP1435-5V (isolated) → `+5V_POE` → π filter C11/L1/C13/C14/C15 → `+5V_POE_F` → U4 TPS2121 IN1
(priority); J2's `VBUS` → U4 IN2; U4 OUT → `+5V` → U3 TLV62569 → `+3V3`, and `+5V` → U8 → the
LED boost ([LED driver](led.md#led-power-switch)). U4's ST output reports the source to GPIO2 as
`PWR_SRC`. The USB VBUS parts D2, R24 and the VBUS_SENSE divider (R27, R28, C26) sit on the MCU
sheet and are described here under [USB VBUS](#usb-vbus).

Rail ranges and currents for the whole board are in the [Rails table](README.md#rails). Board
ambient is 15–30 °C, indoor (user decision 2026-10-31); the thermal formulas below are evaluated at
the 30 °C end.

Component values are in the schematic, not here. Each calculation below gives the requirement, the
formula and its datasheet constants, and names the refdes that are its inputs.

## PoE input

### Module

U2 is an SDAPO DP1435-5V, an 802.3af powered-device module with an isolated flyback: 37–57 V in
through its own bridge rectifiers, 5.15 V typ out, 2.4 A nominal (12.4 W), 2.6 A maximum (DP1435
datasheet p.1, p.5). It is 802.3af compliant (detection and programmable classification) and has
thermal cut-off, short-circuit, over-current and enhanced surge protection (p.1); SDAPO publishes no
inrush figure or soft-start time. It sits on the bottom side, through-hole, and is hand-soldered
from the top after JLCPCB's assembly (user decision 2026-10-31: hand-assembly list).

- **Both pair sets.** VA1/VA2 (U2.1/U2.2) take the centre taps of pairs 1/2 and 3/6 (J1.7/J1.8),
  VB1/VB2 (U2.3/U2.4) the spare pairs 4/5 and 7/8 (J1.9/J1.10) (DP1435 p.2; HanRun HR871150C p.1
  schematic). The module is not polarity sensitive, so mode A and mode B PSEs both work.
- **Nothing on the cable side.** U2.1–U2.4 connect only to J1. SDAPO's typical application
  (p.4) wires them straight from the jack with no capacitor, and a capacitor from them to board
  GND would bridge the isolation barrier. The decoupling check exempts these four pins. The
  Bob-Smith termination (resistors, capacitors and the 2 kV capacitor to the shield) is inside J1
  (HR871150C p.1).
- **Centre-tap current.** Requirement: the PD input stays within the 802.3af PD limit (12.95 W) and
  J1's 350 mA per centre tap (HR871150C p.1). Formulas:
  P_out = I(+5V peak) × VOUT(U2, 5.15 V typ), with I(+5V peak) from [+5V loads](#5v-loads);
  P_in = P_out / η; pair current = P_in / 37 V (the module's lowest input). DP1435 p.5 item 7 gives
  "Efficiency 84% max", the module's best case; no minimum is published. So P_in is **at least**
  P_out / 0.84, and the pair current at least that over 37 V. The 802.3af limit is reached at
  η = P_out / 12.95 W and J1's rating at η = P_out / (37 V × 0.35 A); the real margin is unknown
  ([Accepted risks](#accepted-risks)). The four PoE tracks (class `POE_CABLE`) run on In1; their
  rise at 0.35 A comes from the external-layer IPC-2152 fit of
  [layout](layout.md#net-classes) applied to In1's copper thickness.

### Isolation

The cable side is separated from the board by J1's magnetics and by U2's isolated flyback. The
board layout is meant to give some isolation between the PoE cable nets and the rest of the board;
no isolation level is rated or tested (user decision 2026-11-02).

On the board the four PoE nets run from J1.7–J1.10 to U2.1–U2.4 on In1, in a lane cut out of the
In1 GND plane, with In2 GND unbroken below:

- Rule `PoE isolation` (`.kicad_dru`): keeps `POE_CABLE` copper clear of any other net on the same
  layer. Copper on other layers is kept ≥ 1.0 mm sideways from the In1 PoE copper by hand
  ([layout](layout.md#poe-isolation)). The rule is waived between J1's own pins, the jack's pin
  field; J1's pads are trimmed so the chip-side to cable-side pin spacing and the pin 10 to shield
  spacing fit the jack.
- F.Cu copper of other nets keeps ≥ 1.0 mm sideways from the In1 PoE copper, through the one thin
  7628 ply between F.Cu and In1. KiCad does not check copper across layers, so this is measured by
  hand (`kicad_query distance`). The tightest point is TP10.1 (CAM_3V3) to POE_VA1. The same-layer
  keep-off of rule `PoE isolation` does not fit here.

The layout details (lane outline, distances) are in
[layout](layout.md#ethernet-and-poe-isolation).

### Output and filter

The DP1435 output has 5 % line and 5 % load regulation, 200 mVpp ripple at 250 kHz (DP1435 p.5)
and no stated set-point accuracy. Requirement: `+5V_POE` stays under the 5.5 V limit of the
TLV62569 and TPS22918. Formula: 5.15 V × (1 + line term + load term). With one 5 % term it passes;
only if both terms stacked would it not.

A π filter cleans it before the mux:

| Part | Role |
|---|---|
| C11 | The module's one output decoupling capacitor (DP1435 p.1), at U2.6 |
| L1 | Series inductor; DCR, Isat and Irms from the Changjiang FXL0630 catalogue p.14 |
| C13 | Polymer bulk; ESR and rms ripple rating from AISHI VZ p.2 |
| C14 | Aluminium bulk; ESR at 100 kHz and rms ripple rating from JIERR JVE p.1, p.4 |
| C15 | U4 IN1 bypass, at the pin |

- L1's Isat and Irms must exceed the module's 2.6 A maximum.
- Corner frequency f₀ = 1/(2π√(L1 × (C13 + C14 + C15))), well below the module's 250 kHz;
  characteristic impedance Z₀ = √(L1 / (C13 + C14 + C15)). L1's DCR and C14's higher ESR damp the
  resonance. A load step's ringing must leave `+5V_POE_F` well above the mux's PoE → USB threshold.
- Ripple at 250 kHz: X(L1) = 2π × 250 kHz × L1 against C13's ESR, so the 200 mVpp is divided by
  roughly X(L1) / ESR(C13) on IN1.
- When U4 drops IN1, L1's energy E = ½ × L1 × (2.6 A)² lifts C13–C15 by ΔV from
  E = ½ × C × ((V + ΔV)² − V²).
- SDAPO gives no maximum capacitive load or soft-start time. The module starts into its own output
  capacitor plus C13–C15; that is a first-board check.
- L1's drop is taken as ≤ 2.6 A × DCR everywhere, so `+5V_POE_F` is the module's output range minus
  that drop.

`+5V_POE` and `+5V_POE_F` are class `PWR_5V` on F.Cu; rise by the IPC-2152 fit for 35 µm outer
copper at the +5V peak and at the module's 2.6 A. The filter output reaches U4 on B.Cu from two
vias beside the filter to U4's IN1 vias. That leg is narrower than `PWR_5V` because the full class
width does not pass the VBUS IN2 via.

### Power budget

All PD input figures use the DP1435's maximum efficiency (84 %, p.5 item 7), so they are lower
bounds: P_in ≥ P_out / 0.84.

| Case | +5V current | PD input |
|---|---|---|
| LED design point (0.5 ms pulses at 120 fps), logic running | Average of the [+5V loads](#5v-loads) with the boost at full scale, nominal and worst case | P_out / 0.84, P_out = I × 5.15 V |
| Boost recharging `+32V` at its switch current limit (start-up, and part of every frame, see [+5V loads](#5v-loads)), logic running | The +5V peak | P_out / 0.84 while it lasts |
| 802.3af PD limit | | 12.95 W in; out = 12.95 W × η |
| DP1435 rating | 2.4 A nominal, 2.6 A max | 12.4 W out |

The module's current rating covers the +5V peak. Whether the PD input stays under 12.95 W and
the pair current under J1's 350 mA at that peak depends on the module's real efficiency, which is
not published: accepted by user decision 2026-11-02 ("Accept, measure on first boards"), with a
first-board measurement; if it is over, firmware limits the LEDs on PoE. The load breakdown is
under [+5V loads](#5v-loads).

## Power mux

U4 is a TI TPS2121, a two-input priority mux: IN1 = `+5V_POE_F` (PoE, preferred), IN2 = `VBUS`
(USB), OUT = `+5V`. Both inputs have back-to-back FETs that block reverse current (TI SLVSEA3F
§9.3.6 p.15), so PoE never back-feeds a USB host and USB never feeds the PoE module.

### Priority and hysteresis

The circuit is TI's XREF priority switchover with hysteresis (SLVSEA3F §10.4 Fig. 10-12, p.27):

- **PR1** senses IN1 through the divider R18 (top) / R19 (bottom).
- **CP2** is the reference, from `+3V3` through the divider R20 / R21, filtered by C52 so the
  buck's ripple does not reach the comparator (time constant (R20 ∥ R21) × C52). CP2 =
  `+3V3` × R21 / (R20 + R21). Requirement: CP2 stays above VREF max (1.10 V, p.8) over the `+3V3`
  range and the resistor tolerance, so the XREF mode stays active.
- **R75** from ST to PR1 adds the hysteresis: while IN1 is selected, ST is high (≈ `+3V3` through
  R29) and pushes PR1 up; on IN2, ST is low.
- OV1 and OV2 are tied to GND: over-voltage cut-off is not used ("Connect to GND if not
  required", p.4).
- IN1 is selected while PR1 > CP2 (Table 9-3, p.18).

With G = 1/R18 + 1/R19 + 1/R75 and V_ST the ST voltage:

| Switch | Formula |
|---|---|
| PoE → USB (ST high) | V_IN1 = ((CP2 + V_OFST)·G − V_ST/R75)·R18 |
| USB → PoE (ST low) | V_IN1 = (CP2 + V_OFST)·G·R18 |
| Hysteresis | difference of the two |

Typical: V_OFST 0, `+3V3` at the buck's nominal set point. Worst case: resistor tolerance, `+3V3`
over the buck's set-point range ([Output voltage](#output-voltage)), comparator offset V_OFST
5–40 mV (SLVSEA3F p.8). The first-board check ([switchover thresholds](#first-board-checks))
compares against these formulas, worked out from the schematic.

- PoE always wins back: the lowest IN1 on PoE (module's lowest output minus L1's drop) must be
  above the worst-case USB → PoE threshold.
- No chatter: after a PoE loss `+5V_POE_F` bleeds through R18 + R19 into the C13–C15 bank (time
  constant (R18 + R19) × (C13 + C14 + C15)) downward from the trip point and never re-crosses the
  return threshold.
- **CP2 from +3V3, not a precision reference** (TI's figure uses a separate reference). It works
  because the switchover takes `+3V3` down only slightly. Requirement: U3 keeps `+3V3` inside
  3.3 V ± 5 % through a switchover. Chain, at the lowest PoE → USB trip point and the +5V peak:
  - `+5V` at U4 OUT = V_trip,min − I(+5V peak) × RON(U4), minus the switchover droop
    I(+5V peak) × 5 µs / C(`+5V`), with RON ≤ 85 mΩ at 85 °C (p.7) and C(`+5V`) the sum of the
    capacitors on `+5V` (the same capacitance as in the soft-start inrush below).
  - The board copper from U4.8 to U3.4 drops a little more at the peak (U8 + U3 share; the `+5V`
    copper network solved from `kicad_query copper`), so U3.4 sees VIN(U3) = U4 OUT minus that
    drop.
  - The TLV62569 in 100 % mode needs VIN ≥ VOUT + IOUT × (RDS(on) + DCR) (SLVSDG1C p.6 Eq. 1),
    VOUT being the buck's minimum set point, to hold the set point. IOUT is the `+3V3` total from
    [+3V3 loads](#3v3-loads), RDS(on) is U3's (SLVSDG1C p.6) and DCR is L2's (Changjiang FXL0630
    catalogue p.14). Below that `+3V3` follows its input, VIN − IOUT × (RDS(on) + DCR), which must
    still be inside 3.3 V ± 5 %.
  - If `+3V3` were absent, CP2 < VREF and the TPS2121 falls back to its PR1-vs-VREF row (Table
    9-3), which still picks IN1 when PoE is present. At power-up CP2 follows `+3V3` through C52;
    PoE alone or PoE + USB select IN1 in both rows, USB alone selects IN2, so there is no glitch.

### ST → PWR_SRC

ST (U4.9) is open drain, pulled up by R29 to `+3V3` on the MCU sheet (within TI's 6–20 kΩ pull-up
range, p.6), and goes to U6 pin 4, GPIO2, as `PWR_SRC`: high = PoE, low = USB. It is also the
hysteresis node, so firmware reads GPIO2 as an input with its internal pull-up off and never drives
it. Firmware keeps the LED supply off while `PWR_SRC` is low. On its falling edge (PoE lost, now on
USB) firmware drops LED_EN, BST_EN and LED_PWR_EN back to back with no wait, so the USB supply
never carries the boost; the boost then stops on its collapsing input (UVLO). The normal power-down
(with a ≥ 3 ms wait between BST_EN and LED_PWR_EN) and the power-up order are stated once in
[firmware rules](firmware-rules.md#rules) (FW-7).

### Current limit and soft start

- **ILIM:** R22 (1 %). ILM = 65.2 / R^0.861 (R in kΩ, SLVSEA3F Eq. 2 p.13); the p.8 table (29.8k:
  3–4 A, 44.2k: 2–3 A) brackets the range. Requirements: R22 inside the 18–100 kΩ range (p.6), and
  the minimum ILM above the +5V peak. It does not limit a USB-only load to a port's 500 mA: that
  is left to firmware (see [USB VBUS](#usb-vbus)).
- **Soft start:** C21 sets the output slew rate SR (Table 9-1, p.13). Inrush = SR × C(`+5V`) with
  C(`+5V`) the sum of C20, C22, C10, C34 and the small bypass parts; ramp time = VOUT / SR. A VBUS attach
  sees only C16 before that.
- **Capacitors:** C15 at IN1, C16 at IN2, C20 (ceramic) + C22 (polymer) + C23 on OUT.

`+5V` range in steady state: low end = USB VBUS minimum (4.40 V, see [USB VBUS](#usb-vbus)) minus
RON × the USB-only current; high end = the PoE upper bound from
[Output and filter](#output-and-filter) (RON ≤ 85 mΩ at 85 °C, SLVSEA3F p.7). Every `+5V` load
accepts this range: TLV62569 2.5–5.5 V, TPS22918 1–5.5 V, TLV9061 1.8–5.5 V, TPS61170 3–18 V.

### +5V loads

| Load | Basis |
|---|---|
| U3 buck input | I = V(`+3V3`) × I(`+3V3` total) / (V(`+5V`) × η), η 0.88 assumed; peak is the same input at its highest |
| U8 → U10 LED boost, frame average at the design point | `+32V` demand at full scale, nominal and worst case ([LED driver](led.md#recovery-and-start-up)); less while the headroom loop runs +32V lower |
| U8 → U10 LED boost, recharging at its switch current limit | TPS61170 ILIM max 1.44 A (SLVS789D p.5) at peak; ILIM max minus half the ripple on average |
| U11, R63, U8 control | < 1 mA |
| **Total** | Sum of the rows (design point). **+5V peak** = U3 peak + boost at ILIM max; the average at the limit is U3 + boost average at the limit |

The boost runs at its switch current limit whenever it recharges `+32V`
([Recovery and start-up](led.md#recovery-and-start-up)):

- **Every frame on PoE.** After each LED pulse the boost refills the output bank at its current
  limit for part of the 120 fps frame, so the +5V_BST peak, the `+5V` peak and the matching PD
  input recur every frame, not only at start-up. The frame average stays at the design-point row
  because the charge per frame is fixed.
- **Start-up of `+32V`.** Energy-limited; how to work out its time over tolerances is in
  [Recovery and start-up](led.md#recovery-and-start-up).

### Layout

U4's periphery follows TI's layout example (SLVSEA3F §12.2 p.33): OUT pins 1 and 8 tied across,
C20/C23 on the OUT side, C16 at IN2, C15 at IN1, C21 and R22 straight off SS/ILIM, the divider
row R20, R21, R18, R75, R19 beside PR1/CP2. TI draws two vias per IN pin and four on OUT and marks
a via in each of pads 1, 2, 7 and 8 (land pattern p.39). The board has two vias on IN1 and two on
OUT (a larger OUT via would break the clearance to C15 or U4.9) and one on IN2 (USB current only),
about one via per ampere; the via counts are enforced by `.omp/kicad.json` `copperArea` entries
"U4 IN1 vias", "U4 IN2 vias" and "U4 OUT vias". The `PWR_5V` tracks neck down inside U4's
courtyard to reach its fine-pitch pads (rule `Power neck-down at fine-pitch pads`); the neck's
resistance and dissipation at the module's 2.4 A follow from R = ρ × length / (width × thickness).
Probe `+5V` at TP5; `+5V_POE` at C11, `+5V_POE_F` at C15, VBUS at C16.

### Thermal

- U4: P = I² × RON, RON ≤ 90 mΩ (p.7); TJ = TA + P × RθJA, RθJA 72.2 °C/W (p.6), evaluated at the
  +5V peak and at the module's 2.6 A, against 125 °C.
- L1: I² × DCR at the module's maximum, 2.6 A.
- U2: SDAPO gives no thermal resistance. Its loss is P_in − P_out; at its 84 % maximum efficiency
  that is P_out / 0.84 − P_out, evaluated at the design point and at the +5V peak. The real loss
  is higher by however much the efficiency falls short of 84 %.

## 3.3 V buck

U3 is a TI TLV62569, a 2 A synchronous buck in SOT-23-5, from `+5V` to `+3V3`. EN is tied to VIN
(allowed: "do not leave floating", VIH ≤ 1.2 V, SLVSDG1C p.3–4), so it starts when the mux's soft
start brings `+5V` up, with its own 800 µs soft start. It is the board's only 3.3 V source: module,
PHY, camera, accelerometer, logic and pull-ups.

### Output voltage

VOUT = VFB × (1 + R16/R17) (SLVSDG1C Eq. 2 p.9), VFB 0.588/0.600/0.612 V (p.4), R16 and R17 at
their schematic tolerance. Minimum: VFB min with R16 low and R17 high. Nominal: VFB 0.600 V and
nominal resistors. Maximum: VFB max with R16 high and R17 low.

The whole range must lie inside 3.3 V ± 5 %, which the IP101G needs on VDDIO/AVDD33 (DS p.57); the
module's VDD_LP accepts 3.0–3.6 V. R17 must be under TI's 200 kΩ maximum (p.9). C17 across R16 is
the feed-forward capacitor TI recommends for its R2 = 100k (p.9); its zero sits at
1/(2π × R16 × C17).

This DC set-point range is the `+3V3` range every calculation in these documents uses (the
[Rails table](README.md#rails)). During a PoE → USB switchover `+3V3` can sag below it while `+5V`
passes the trip point ([Priority and hysteresis](#priority-and-hysteresis)). Where a logic-level
check uses the generic 3.3 V ± 5 % envelope instead, it names it as that.

### Inductor and ripple

L2 is a Changjiang FXL0630 (DCR, Isat and Irms from its catalogue, p.14). L2 and C18 stay
in TI's recommended "++" cell for VOUT ≥ 1.8 V (Table 4, p.9).

- Ripple current ΔIL = VOUT × (1 − VOUT/VIN) / (L2 × fSW) (Eq. 3, p.10), fSW 1.5 MHz; nominal at
  5 V in and nominal L2, worst case at VIN 5.5 V and L2 −30 %.
- Peak inductor current at the 2 A rating: 2 A + ΔIL/2, compared with L2's Isat; also at the IC's
  3 A typical high-side limit plus ripple.
- Output ripple ΔV ≈ ΔIL / (8 × fSW × C), with C the effective `+3V3` capacitance (below) and, as a
  worst case, C18 alone at −50 %. Ceramic ESR is neglected.

### Capacitors

- Input: C10 at VIN, close to U3.4; C12 beside it on the outside. TI's layout has one input
  capacitor (Fig. 20 p.13), C10's role; TI says 4.7 µF is enough (p.10). C12 is an extra with
  little high-frequency effect where it sits.
- Output: C18 and C19 at the buck, plus the rest of `+3V3` (C29, C32, C8 and the small bypass parts) and
  the module's own capacitors on its 3.3 V input (Waveshare ESP32-P4-Core schematic p.1). The
  effective capacitance is the nominal sum derated for 3.3 V bias (estimate). TI's Table 4 tests up
  to 2 × 22 µF and says "further combinations should be checked": a first-board load-step check.

### Layout

U3 is turned 180° against TI's Fig. 20 (SLVSDG1C p.13), with the same loops:

- L2 at U3's SW side; the switch node is one short F.Cu track.
- C10's VIN end and C18's GND end close to U3.4 and U3.2; C18, C19 and C10 return to U3.2 on F.Cu.
- R16, R17 and C17 at U3.5; the feedback is sensed at C18.1 and routed away from the switch node.
- C10, C12, C18 and C19 each have GND vias, U3.2 one under the body. In1 and In2 GND are solid
  under U3, L2 and the switch node.
- The accelerometer's I²C lines pass L2 on B.Cu, below both GND planes; on F.Cu they keep clear of
  switch-node copper, and IMU_SDA crosses the corner of L2's courtyard margin (not its body)
  ([IMU](imu.md#i2c-bus)).
- `+3V3` is class `PWR_3V3` (ESP32-P4 HDG §1.4.1 "at least 25 mil"); rise by the IPC-2152 fit at
  the `+3V3` total. Probe it at TP4.

### +3V3 loads

| Load | Basis |
|---|---|
| U6 ESP32-P4-Core | Espressif HDG: 380 mA minimum operating supply current (incl. flash/PSRAM) + MIPI PHY 50 mA + USB PHY 20 mA |
| U1 IP101G | < 0.15 W (IP101G DS p.1) |
| Camera module (CAM_3V3, through R53) | ≤ 100 mA, estimate; the module has no datasheet |
| D1 status LED | 5 mA per colour (Yongyu YY-0807BRGK26 DS p.2, p.4) |
| J1 link/activity LEDs | Two LEDs, current set by R4/R5: (V(`+3V3`) − VF) / R |
| U12, U7, U9, pull-ups, dividers | Small, from the schematic |
| **Total** | Sum of the rows (estimate); U3 is rated 2 A (SLVSDG1C p.4) |

### Thermal

P = I × V(`+3V3`) × (1/η − 1), η from the SLVSDG1C p.1 efficiency curve at the `+3V3` total;
TJ = TA + P × RθJA, RθJA 188.2 °C/W (p.4), against the 150 °C shutdown. A pessimistic η of 84 %
must also stay under it. L2 dissipates I² × DCR.

## USB VBUS

USB-C J2 (HRO TYPE-C-31-M-12) is a sink only: CC1/CC2 have the 5.1 kΩ Rd that USB Type-C requires
of a sink (USB Type-C Cable and Connector Specification R2.0; R25/R23), and U4's IN2
blocks any current back into the host. The connector, the data lines and their protection are in
[MCU](mcu.md#usb-c). The VBUS path:

- **J2 A4/A9/B4/B9 → `VBUS` → U4 IN2.** Range 4.40–5.5 V. The minimum is the USB 2.0 low-power
  port VBUS, 4.40–5.25 V (USB 2.0 Table 7-7 p.178; §7.2.1.3 p.173). A high-power function, which
  this board is (the U3 input is its USB load), must enumerate at 4.40 V and run at full power from
  4.75 V (§7.2.1.4 p.174, §7.2.2 p.175), so designing down to 4.40 V is a conservative floor. The
  maximum is the 5.5 V that USB Type-C allows legacy devices to see at light
  load (USB Type-C Cable and Connector Specification R2.0 §4.4.2, p.141: "up to 5.5V maximum").
- **D2 TI TSD05D**, a TVS at J2's VBUS pads: VRWM 5.5 V, VBR 6 V, IEC 61000-4-2 ±30 kV contact, TLP
  clamp 7.5 V at 16 A, 19 pF (SLVSH43C p.1, p.6). TI's TPS2121 hot-plug guidance asks for a TVS on
  a hot-plugged input (SLVSEA3F §10.7 p.31–32). When IN2 turns off at U4's current limit, a ≈ 1 µH
  cable into C16 gives an overshoot of I_limit × √(L_cable / C16) on top of VBUS, to be compared
  with U4's 24 V absolute maximum.
- **C16** at IN2. On attach the host sees C16 plus D2's 19 pF, which must stay under the 10 µF
  maximum load at the downstream end of a cable (USB 2.0 §7.2.4.1, p.177); the `+5V` bulk is behind
  U4's soft start.
- **R24 to GND.** With PoE only and an unpowered host plugged in, U4's IN2 leakage (≤ 5 µA up
  to 85 °C, SLVSEA3F p.7) flows into R24 ∥ (R27 + R28): V = I_leak × (R24 ∥ (R27 + R28)) must stay
  under vSafe0V (0.8 V maximum, USB Power Delivery specification, which USB Type-C R2.0 refers to
  for it).
- **VBUS_SENSE:** divider R27 (top) / R28 (bottom) with C26 to U6 pin 8, GPIO6, a digital input.
  Requirements, with the VBUS range above, the `+3V3` set-point range and the resistor tolerance:
  VBUS min × R28 / (R27 + R28) ≥ VIH = 0.75 × VDD at `+3V3` max; VBUS max × R28 / (R27 + R28) ≤
  VDD + 0.3 V at `+3V3` min (ESP32-P4 DS p.89). Firmware turns the USB Serial/JTAG D+ pull-up off
  while it is low ([firmware rules](firmware-rules.md#rules)).
- **Routing:** narrow F.Cu stubs from J2.A9/A4 to two vias (the only way out between J2's NPTH
  pegs and the neighbouring pads; rise per stub at 1.0 A and at 0.5 A from the IPC-2152 fit), a
  B.Cu link and a B.Cu trunk to U4's IN2 via.

**Current on USB alone.** U4's current limit (minimum ILM, [Current limit](#current-limit-and-soft-start))
does not hold a USB-only load to the 500 mA a high-power port supplies (USB 2.0 Table 7-7, ICCHPF).
On USB alone the LED supply stays off by a firmware rule only
(user decision 2026-10-31, "LEDs off on USB: firmware rule only"; no extra hardware gate), which
leaves the buck input, from the estimated `+3V3` total ([+5V loads](#5v-loads)): it must stay
≤ 500 mA. The LED switch U8, the boost enable and the LED gate all default off through pull-downs
(R57, R59, R56), so nothing draws LED current before firmware runs.

## Accepted risks

- **Some isolation only on the PoE lane.** Other nets' F.Cu copper is closer sideways to the In1 PoE
  copper than rule `PoE isolation`'s same-layer keep-off, through one thin 7628 ply; it keeps only
  the hand-kept ≥ 1.0 mm. The tightest spot is TP10.1 (`kicad_query distance`). The layout is meant
  to give some isolation only; no isolation level is rated or tested (user decision 2026-11-02).
- **DP1435 output tolerance and capacitive load are not specified** (DP1435 p.1–5). The rails
  assume one 5 % regulation term (see [Output and filter](#output-and-filter)) and start-up into
  the module's output capacitor plus C13–C15; both are first-board checks.
- **CP2 reference from `+3V3`** instead of a precision reference (TI Fig. 10-12): the thresholds
  move with `+3V3` (included in the worst case above), and the switchover cannot pull `+3V3` out of
  regulation for more than the 5 µs switchover (chain in
  [Priority and hysteresis](#priority-and-hysteresis), with the board's `+5V` copper drop).
  Accepted in the schematic review (2026-11-01).
- **USB current is limited by firmware, not hardware** (user decision 2026-10-31: LEDs off on USB
  by a firmware rule only). The USB-only load, from the buck input, is at the 500 mA port limit; a
  firmware fault that enables the LED supply on USB draws more than a port supplies.
- **PoE loss with USB attached.** The TPS2121 switches to IN2 at once, so the host port briefly
  sees up to the current limit while `+5V` recharges (time ≈ C(`+5V`) × ΔV / I_limit, idle and with
  the LEDs running), and with the LEDs running up to the `+5V` peak until firmware sees `PWR_SRC`
  low and drops LED_EN, BST_EN and LED_PWR_EN. A host whose port trips fast may cut VBUS and reset
  the board.
- **PoE power budget rests on an unpublished efficiency.** DP1435 p.5 item 7 gives 84 % as the
  maximum efficiency only. At the `+5V` peak, which recurs while the boost recharges `+32V`
  ([+5V loads](#5v-loads)), the PD draws at least P_out / 0.84 and P_in / 37 V per pair
  ([Centre-tap current](#module)); below some efficiency it reaches the 12.95 W 802.3af PD limit
  and J1's 350 mA per centre tap. User decision 2026-11-02: "Accept, measure on first boards"; if
  the measurement is over either limit, a firmware LED limit on PoE follows, with no design change
  now.
- **C15 and C23 closer than JLCPCB's 1.0 mm QFN spacing** to U4 (C15–U4 and C23.1–U4.1,
  `kicad_query distance`), kept at the pins as TI asks ("as close as possible", SLVSEA3F §11); C23
  cannot move because C20's courtyard abuts it.
- **C12 outboard of C10 at the buck input.** Swapping them needs TP12 and the RMII_TX_EN track
  moved; TI's layout has one input capacitor, which C10 is.
- **`+3V3` capacitance outside TI's tested cells** (Table 4: up to 2 × 22 µF), counting the
  module's own capacitors.
- **Reflow ratings of C13, C14 and C22 are not printed** in their datasheets (JLCPCB reflows at
  255 ± 5 °C). User decision 2026-10-31: "Ignore reflow times and temperatures. Assume it will all
  work out."

## First-board checks

1. **DP1435 output.** `+5V_POE` (at C11) at no load and at full load, with an 802.3af PSE at 44 V
   and at 57 V: must stay ≤ 5.5 V.
2. **PoE start-up.** Scope `+5V_POE` and `+5V_POE_F` at PoE plug-in (cold start): monotonic rise,
   no hiccup; the module stays up through 0.5 ms / 120 fps LED pulses.
3. **Switchover thresholds.** With U2 not fitted, a bench supply on `+5V_POE_F` (C15) and USB on
   J2: ramp the supply down and up and read `PWR_SRC`. Expected: to USB inside the PoE → USB worst
   case band and back to PoE inside the USB → PoE band, both from the R18/R19/R75 formulas in
   [Priority and hysteresis](#priority-and-hysteresis), worked out from the schematic.
4. **PoE pull with USB attached.** On a laptop port and on a bench port, unplug PoE (a) idle and
   (b) during 0.5 ms / 120 fps LED pulses. Scope VBUS, `+5V`, `+3V3` and `PWR_SRC`: the module must
   not reset and USB must stay enumerated. Record the firmware latency from the `PWR_SRC` falling
   edge to LED_EN, BST_EN and LED_PWR_EN low.
5. **PoE input power at peak load.** On a 44 V PSE, with the LEDs at 0.5 ms / 120 fps, the camera
   streaming and Ethernet up: measure the PD input power and the pair current, and scope the
   `+5V` current through a frame (does the boost recharge at its 1.44 A switch limit?). Limits:
   12.95 W (802.3af PD) and 350 mA per pair (J1). If either is exceeded, firmware limits the LEDs
   on PoE.
6. **USB-alone current.** Measure the VBUS current on USB alone while streaming 640 × 400 at
   120 fps with Ethernet linked (LEDs off): must be ≤ 0.5 A, and compared with the buck-input
   estimate from [+5V loads](#5v-loads). Measure the camera's share by replacing R53 with a 0.1 Ω
   shunt.
7. **`+3V3` load step.** 0.1 → 0.6 A at 5 V input: settles without ringing, overshoot ≤ 5 %. If
   not, reduce C18 or remove C17.
8. **Temperatures.** At PoE with the LEDs at 0.5 ms / 120 fps (and once during `+32V` start-up),
   a thermal camera on U4 and its IN1/OUT vias, the U8 neck, U3, L1 and U2.
9. **GND test loops.** Insert one GND test loop into TP2's plated hole before soldering: the
   loop's drawing gives no hole size; if it is loose or tight, change the drill in the footprint.
10. **B.Cu under U2.** At the `+5V` peak on PoE (LEDs at 0.5 ms / 120 fps, camera streaming,
    Ethernet up), scope XVS and SCCB_SCL/SDA at J4 for ringing or extra edges, and run USB
    enumeration and a bulk transfer; repeat on USB power alone as the reference. VBUS, the USB pair,
    CAM_EN, SCCB and a short XVS hop run on B.Cu right under the module's flyback magnetics
    ([layout](layout.md#bcu-under-u2)).
