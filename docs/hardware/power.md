# Power

The Power sheet (`power.kicad_sch`) turns PoE or USB-C into the board's two supplies: `+5V` for the
LED driver and the buck, and `+3V3` for everything else. The chain: J1's PoE centre taps → U2
DP1435-5V (isolated) → `+5V_POE` → π filter C11/L1/C13/C14/C15 → `+5V_POE_F` → U4 TPS2121 IN1
(priority); J2's `VBUS` → U4 IN2; U4 OUT → `+5V` → U3 TLV62569 → `+3V3`, and `+5V` → U8 → the
LED boost ([LED driver](led.md#led-power-switch)). U4's ST output reports the source to GPIO2 as
`PWR_SRC`. The USB VBUS parts D2, R24 and the VBUS_SENSE divider (R27, R28, C26) sit on the MCU
sheet and are described here under [USB VBUS](#usb-vbus).

Rail ranges and currents for the whole board are in the [Rails table](README.md#rails). Board
ambient is 15–30 °C, indoor (user decision 2026-10-31); the thermal figures below use 30 °C.

## PoE input

### Module

U2 is an SDAPO DP1435-5V (LCSC C5380097), an 802.3af powered-device module with an isolated
flyback: 37–57 V in through its own bridge rectifiers, 5.15 V typ out, 2.4 A nominal (12.4 W),
2.6 A maximum (DP1435 datasheet p.1, p.5). It is 802.3af compliant (detection and programmable
classification) and has thermal cut-off, short-circuit, over-current and enhanced surge protection
(p.1); SDAPO publishes no inrush figure or soft-start time. It sits on the bottom side,
through-hole, and is hand-soldered from the top after JLCPCB's assembly (user decision
2026-10-31: hand-assembly list).

- **Both pair sets.** VA1/VA2 (U2.1/U2.2) take the centre taps of pairs 1/2 and 3/6 (J1.7/J1.8),
  VB1/VB2 (U2.3/U2.4) the spare pairs 4/5 and 7/8 (J1.9/J1.10) (DP1435 p.2; HanRun HR871150C p.1
  schematic). The module is not polarity sensitive, so mode A and mode B PSEs both work.
- **Nothing on the cable side.** U2.1–U2.4 connect only to J1. SDAPO's typical application
  (p.4) wires them straight from the jack with no capacitor, and a capacitor from them to board
  GND would bridge the isolation barrier. The decoupling check exempts these four pins. The
  Bob-Smith termination (4 × 75 Ω, 4 × 22 nF, 1000 pF/2 kV to the shield) is inside J1
  (HR871150C p.1).
- **Centre-tap current.** At the board's worst +5V peak (1.96 A, see [+5V loads](#5v-loads)) the
  module delivers 1.96 A × 5.15 V = 10.1 W. DP1435 p.5 item 7 gives "Efficiency 84% max", the
  module's best case; no minimum is published. So the PD input is **at least** 10.1 W / 0.84 =
  12.0 W and the centre-tap current **at least** 0.32 A at 37 V. The 802.3af PD limit (12.95 W) is
  reached at η = 78 %, where the pair current at 37 V is 12.95 W / 37 V = 0.35 A, J1's 350 mA
  rating per centre tap (HR871150C p.1): the real margin is unknown, about 6 % of efficiency
  ([Accepted risks](#accepted-risks)). The four PoE tracks (class `POE_CABLE`) are 0.4 mm wide on
  In1 (15.2 µm copper): ΔT ≈ 2 °C at 0.35 A by the external-layer IPC-2152 fit of
  [layout](layout.md#net-classes) applied to In1's copper thickness.

### Isolation

The cable side is separated from the board by J1's magnetics and by U2's isolated flyback. The
board layout is meant to give some isolation between the PoE cable nets and the rest of the board;
no isolation level is rated or tested (user decision 2026-11-02).

On the board the four PoE nets run from J1.7–J1.10 to U2.1–U2.4 on In1, in a lane cut out of the
In1 GND plane, with In2 GND unbroken below:

- Rule `PoE isolation`: 2 mm from `POE_CABLE` copper to any other net on the same layer; copper on
  other layers is kept ≥ 1.0 mm sideways by hand ([layout](layout.md#poe-isolation)). The rule is
  waived between J1's own pins, the jack's pin field; J1's pads are trimmed so pin 5 (chip side) to
  pin 7 (cable side) is 1.09 mm and pin 10 to the shield 1.97 mm.
- F.Cu copper of other nets keeps ≥ 1.0 mm sideways from the In1 PoE copper, through the one
  0.2104 mm 7628 ply between F.Cu and In1. KiCad does not check copper across layers, so this was
  measured by hand. The tightest point is TP10.1 (CAM_3V3) at exactly 1.000 mm from POE_VA1. A 2 mm
  sideways keep-off does not fit.

The layout details (lane outline, distances) are in
[layout](layout.md#ethernet-and-poe-isolation).

### Output and filter

The DP1435 output has 5 % line and 5 % load regulation, 200 mVpp ripple at 250 kHz (DP1435 p.5)
and no stated set-point accuracy. With one 5 % term the output is 4.89–5.41 V; only if both terms
stacked (4.64–5.67 V) would it pass the 5.5 V limit of the TLV62569 and TPS22918.

A π filter cleans it before the mux:

| Part | Value | Role |
|---|---|---|
| C11 | 10 µF 25 V X5R 0805 | The module's one output decoupling capacitor (DP1435 p.1), at U2.6 |
| L1 | Changjiang FXL0630-2R2-M, 2.2 µH | DCR 15 mΩ, Isat 10.5 A, Irms 8.5 A (catalogue p.14): 39 mV drop at 2.6 A |
| C13 | AISHI SVZ1CM331E09E00RAXXX, 330 µF 16 V polymer | ESR 25 mΩ, 2.6 A rms ripple (AISHI VZ p.2) |
| C14 | JIERR JVE16V330M8X10, 330 µF 16 V aluminium | ESR 0.3 Ω at 100 kHz, 450 mA rms (JIERR JVE p.1, p.4) |
| C15 | 10 µF 25 V X5R 0805 | U4 IN1 bypass, at the pin |

- Corner frequency 1/(2π√(2.2 µH × 670 µF)) = 4.1 kHz, 60 × below the module's 250 kHz;
  characteristic impedance √(L/C) = 57 mΩ. L1's DCR and C14's higher ESR damp the resonance
  (Q ≈ 1.5–2.5): a 1 A load step rings by ≤ ≈ 0.15 V, far above the mux threshold.
- Ripple at 250 kHz: X(L1) = 3.46 Ω against C13's 25 mΩ, so 200 mVpp becomes ≈ 1.4 mVpp on IN1.
- When U4 drops IN1, L1's energy (½ × 2.2 µH × 2.6² A² = 7.4 µJ) lifts C13–C15 by ≈ 0.15 V.
- SDAPO gives no maximum capacitive load or soft-start time. The module starts into its own output
  capacitor plus ≈ 690 µF here; that is a first-board check.
- L1's drop is taken as ≤ 0.04 V (2.6 A × 15 mΩ) everywhere, so `+5V_POE_F` is 4.85–5.41 V.

`+5V_POE` and `+5V_POE_F` are class `PWR_5V`, 1.0 mm on F.Cu (ΔT 8.8 °C at 1.96 A, 15.5 °C at
the module's 2.6 A, IPC-2152 fit for 35 µm outer copper). The filter output reaches U4 on B.Cu:
from two 0.8/0.4 mm vias beside the filter (106.6, 138.45)/(106.6, 139.45) to U4's IN1 vias, a
0.8 mm leg of 12.3 mm (11.4 °C at 1.96 A), because 1.0 mm does not pass the VBUS IN2 via.

### Power budget

All PD input figures use the DP1435's maximum efficiency (84 %, p.5 item 7), so they are lower
bounds.

| Case | +5V current | PD input |
|---|---|---|
| LED design point (0.5 ms pulses at 120 fps), logic running | ≈ 0.87 A average nominal, ≤ 1.0 A worst case | ≥ 5.3 W average (≥ 6.1 W worst case) |
| Boost recharging `+32V` at its switch current limit (start-up, and 2.4–6.0 ms of every frame, see [+5V loads](#5v-loads)), logic running | 1.96 A peak | ≥ 12.0 W while it lasts |
| 802.3af PD limit | | 12.95 W in; ≤ 10.9 W out at 84 % |
| DP1435 rating | 2.4 A nominal, 2.6 A max | 12.4 W out |

The module's current rating covers the 1.96 A peak. Whether the PD input stays under 12.95 W and
the pair current under J1's 350 mA at that peak depends on the module's real efficiency, which is
not published: accepted by user decision 2026-11-02 ("Accept, measure on first boards"), with a
first-board measurement; if it is over, firmware limits the LEDs on PoE. The average at the design
point is less than half the 802.3af budget. The load breakdown is under [+5V loads](#5v-loads).

## Power mux

U4 is a TI TPS2121RUX (LCSC C485916), a two-input priority mux: IN1 = `+5V_POE_F` (PoE, preferred),
IN2 = `VBUS` (USB), OUT = `+5V`. Both inputs have back-to-back FETs that block reverse current
(TI SLVSEA3F §9.3.6 p.15), so PoE never back-feeds a USB host and USB never feeds the PoE module.

### Priority and hysteresis

The circuit is TI's XREF priority switchover with hysteresis (SLVSEA3F §10.4 Fig. 10-12, p.27):

- **PR1** senses IN1 through R18 100k / R19 47k (1 %).
- **CP2** is the reference, from `+3V3` through R20 100k / R21 60.4k (1 %), filtered by C52
  100 nF (Thevenin 37.7 kΩ, τ 3.8 ms) so the buck's ripple does not reach the comparator.
  CP2 = 1.249 V nominal, 1.19–1.31 V over `+3V3` and resistor tolerance, always above
  VREF max 1.10 V (p.8), so the XREF mode stays active.
- **R75** 1.00 M from ST to PR1 adds the hysteresis: while IN1 is selected, ST is high (≈ +3V3
  through R29) and pushes PR1 up; on IN2, ST is low.
- OV1 and OV2 are tied to GND: over-voltage cut-off is not used ("Connect to GND if not
  required", p.4).
- IN1 is selected while PR1 > CP2 (Table 9-3, p.18).

With G = 1/R18 + 1/R19 + 1/R75 and V_ST the ST voltage:

| Switch | Formula | Typical (+3V3 3.318 V) | Worst case |
|---|---|---|---|
| PoE → USB (ST high) | V_IN1 = ((CP2 + V_OFST)·G − V_ST/R75)·R18 | 3.70 V | 3.49–4.08 V |
| USB → PoE (ST low) | V_IN1 = (CP2 + V_OFST)·G·R18 | 4.03 V | 3.80–4.42 V |
| Hysteresis | | 0.33 V | 0.31–0.35 V |

Worst case: R ±1 %, `+3V3` 3.199–3.440 V, comparator offset V_OFST 5–40 mV (SLVSEA3F p.8);
typical with V_OFST 0.

- PoE always wins back: the lowest IN1 on PoE is 4.89 V − 0.04 V (L1) = 4.85 V, above the
  4.42 V worst return threshold.
- No chatter: after a PoE loss `+5V_POE_F` (≈ 670 µF) bleeds through R18 + R19 (147 kΩ, τ ≈ 98 s)
  downward from the trip point and never re-crosses the return threshold.
- **CP2 from +3V3, not a precision reference** (TI's figure uses a separate reference). It works
  because the switchover takes `+3V3` down by only ≈ 40 mV: at the lowest trip point (3.49 V) `+5V`
  at U4 OUT falls to 3.49 V − 1.96 A × 85 mΩ = 3.32 V, minus 0.06 V during the 5 µs switchover
  (1.96 A × 5 µs / 158 µF), ≥ 3.26 V. The board copper from U4.8 to U3.4 drops another ≈ 27 mV at
  the 1.96 A peak (U8 1.44 A + U3 0.52 A; the `+5V` copper network solved from `kicad_query
  copper`), so U3.4 sees ≥ 3.23 V. The TLV62569 in 100 % mode needs VOUT + IOUT × (RDS(on) + DCR) =
  3.199 V + 0.65 A × 0.115 Ω = 3.27 V (SLVSDG1C p.6 Eq. 1) to hold the set point; below that `+3V3`
  follows its input, 3.23 V − 0.65 A × 0.115 Ω ≈ 3.16 V, still inside the 3.135–3.465 V envelope.
  If `+3V3` were absent, CP2 < VREF and the TPS2121 falls back to its PR1-vs-VREF row (Table 9-3),
  which still picks IN1 when PoE is present. At power-up CP2 follows `+3V3` through C52; PoE alone
  or PoE + USB select IN1 in both rows, USB alone selects IN2, so there is no glitch.

### ST → PWR_SRC

ST (U4.9) is open drain, pulled up by R29 10k to `+3V3` on the MCU sheet (inside TI's 6–20 kΩ,
p.6), and goes to U6 pin 4, GPIO2, as `PWR_SRC`: high = PoE, low = USB. It is also the hysteresis
node, so firmware reads GPIO2 as an input with its internal pull-up off and never drives it.
Firmware keeps the LED supply off while `PWR_SRC` is low. On its falling edge (PoE lost, now on
USB) firmware drops LED_EN, BST_EN and LED_PWR_EN back to back with no wait, so the USB supply
never carries the boost; the boost then stops on its collapsing input (UVLO). The normal power-down
(with a ≥ 3 ms wait between BST_EN and LED_PWR_EN) and the power-up order are stated once in
[firmware rules](firmware-rules.md#rules) (FW-7).

### Current limit and soft start

- **ILIM:** R22 36k (1 %). ILM = 65.2 / R^0.861 (R in kΩ, SLVSEA3F Eq. 2 p.13) = 2.98 A typical;
  the p.8 table (29.8k: 3–4 A, 44.2k: 2–3 A) puts 36k at ≈ 2.5–3.5 A. R22 is inside the 18–100 kΩ
  range (p.6). The minimum, ≈ 2.5 A, stays above the 1.96 A board peak. It does not limit a
  USB-only load to a port's 500 mA: that is left to firmware (see [USB VBUS](#usb-vbus)).
- **Soft start:** C21 100 nF gives 780 V/s at 5 V (Table 9-1, p.13). With ≈ 158 µF on `+5V`
  (C20 47 µF, C22 100 µF, C10 10 µF, C34 1 µF, 100 nF parts) the inrush is 780 V/s × 158 µF =
  0.12 A and the ramp 5.15 V / 780 V/s = 6.6 ms. A VBUS attach sees only C16 (4.7 µF) before
  that.
- **Capacitors:** C15 10 µF at IN1, C16 4.7 µF at IN2, C20 47 µF X5R + C22 100 µF polymer
  (JIERR MA25V100M6x6, ESR 30 mΩ) + C23 100 nF on OUT.

`+5V` stays at 4.35–5.41 V in steady state: 4.40 V USB minus RON × 0.5 A at the low end,
5.41 V PoE at the high end (RON ≤ 85 mΩ at 85 °C, SLVSEA3F p.7). Every `+5V` load accepts this
range: TLV62569 2.5–5.5 V, TPS22918 1–5.5 V, TLV9061 1.8–5.5 V, TPS61170 3–18 V.

### +5V loads

| Load | Average | Peak | Basis |
|---|---|---|---|
| U3 buck input | ≈ 0.49 A | ≈ 0.52 A | 3.318 V × 0.65 A / (5 V × 0.88) |
| U8 → U10 LED boost, frame average at the design point | ≈ 0.38 A nominal, ≤ 0.51 A worst case | | `+32V` demand 50 mA nominal, 55 mA worst case ([LED driver](led.md#recovery-and-start-up)) |
| U8 → U10 LED boost, recharging at its switch current limit | 1.26 A | 1.44 A | TPS61170 ILIM max 1.44 A (SLVS789D p.5) minus half the ripple |
| U11, R63, U8 control | < 1 mA | | |
| **Total** | **≈ 0.87 A** nominal, **≤ 1.0 A** worst case (design point) | **1.96 A** (U3 0.52 A + boost 1.44 A) | 1.78 A = U3 0.52 A + boost 1.26 A while the boost is at its limit |

The boost runs at its switch current limit whenever it recharges `+32V`
([Recovery and start-up](led.md#recovery-and-start-up)):

- **Every frame on PoE.** After each LED pulse the boost refills the output bank at its current
  limit for 2.4–6.0 ms of the 8.33 ms frame, so the 1.44 A `+5V_BST` peak, the 1.96 A `+5V` peak
  and the matching PD input recur every frame, not only at start-up. The frame average stays at
  the figures above because the charge per frame is fixed.
- **Start-up of `+32V`.** Energy-limited: ≈ 80 ms nominal, 45–170 ms over tolerances.

### Layout

U4's periphery follows TI's layout example (SLVSEA3F §12.2 p.33): OUT pins 1 and 8 tied across,
C20/C23 on the OUT side, C16 at IN2, C15 at IN1, C21 and R22 straight off SS/ILIM, the divider
row R20, R21, R18, R75, R19 beside PR1/CP2. TI draws two vias per IN pin and four on OUT and marks
a via in each of pads 1, 2, 7 and 8 (land pattern p.39): the board has two vias on IN1 (0.45/0.2
and 0.6/0.3 mm), two on OUT (2 × 0.45/0.2 mm, a larger one would break 0.2 mm to C15 or U4.9) and
one 0.6/0.3 mm on IN2 (USB current only, ≤ 0.5 A), about one via per ampere. The `PWR_5V` tracks
neck down to ≥ 0.3 mm inside U4's courtyard to reach its 0.3–0.4 mm pads (rule
`Power neck-down at fine-pitch pads`): a 1 mm neck of 0.3 mm × 35 µm copper is 1.6 mΩ, 9 mW at
2.4 A. Probe `+5V` at TP5; `+5V_POE` at C11, `+5V_POE_F` at C15, VBUS at C16.

### Thermal

- U4: RON ≤ 90 mΩ (p.7) → 0.35 W at 1.96 A; RθJA 72.2 °C/W (p.6) → +25 °C, TJ ≈ 55 °C at 30 °C
  ambient (74 °C at 2.6 A), against 125 °C.
- L1: 2.6² A² × 15 mΩ = 0.1 W at the module's maximum.
- U2: SDAPO gives no thermal resistance. At its 84 % maximum efficiency its loss is ≥ 0.85 W at
  the design point (≥ 5.3 W in, 4.5 W out; ≥ 1.0 W in the worst case) and ≥ 1.9 W while `+5V` is
  at its 1.96 A peak; the real loss is higher by however much the efficiency falls short of 84 %.

## 3.3 V buck

U3 is a TI TLV62569DBVR (LCSC C141836), a 2 A synchronous buck in SOT-23-5, from `+5V` to `+3V3`.
EN is tied to VIN (allowed: "do not leave floating", VIH ≤ 1.2 V, SLVSDG1C p.3–4), so it starts
when the mux's soft start brings `+5V` up, with its own 800 µs soft start. It is the board's only
3.3 V source: module, PHY, camera, accelerometer, logic and pull-ups.

### Output voltage

VOUT = VFB × (1 + R16/R17) (SLVSDG1C Eq. 2 p.9), R16 453k (GiantOhm GR0402F453KTAG00), R17 100k,
both 1 %; VFB 0.588/0.600/0.612 V (p.4):

| | VFB | Resistors | VOUT |
|---|---|---|---|
| Minimum | 0.588 V | R16 −1 %, R17 +1 % | 3.199 V |
| Nominal | 0.600 V | nominal | 3.318 V |
| Maximum | 0.612 V | R16 +1 %, R17 −1 % | 3.440 V |

3.199–3.440 V lies inside 3.3 V ± 5 % (3.135–3.465 V), which the IP101G needs on VDDIO/AVDD33
(DS p.57); the module's VDD_LP accepts 3.0–3.6 V. R17 = 100k is under TI's 200 kΩ maximum (p.9).
C17 6.8 pF C0G across R16 is the feed-forward capacitor TI recommends with R2 = 100k (p.9); its
zero sits at 1/(2π × 453k × 6.8 pF) = 51.7 kHz.

This DC set point, 3.199–3.440 V, is the `+3V3` range every calculation in these documents uses
(the [Rails table](README.md#rails)). During a PoE → USB switchover `+3V3` can sag to ≥ 3.16 V
while `+5V` passes the trip point ([Priority and hysteresis](#priority-and-hysteresis)). Where a
logic-level check uses the generic ± 5 % envelope (3.135–3.465 V) instead, it names it as that.

### Inductor and ripple

L2 is a Changjiang FXL0630-2R2-M, 2.2 µH (DCR 15 mΩ, Isat 10.5 A, Irms 8.5 A, catalogue p.14).
2.2 µH with 10 µF is TI's recommended "++" cell for VOUT ≥ 1.8 V (Table 4, p.9).

- Ripple current ΔIL = VOUT × (1 − VOUT/VIN) / (L × fSW) (Eq. 3, p.10), fSW 1.5 MHz:
  0.34 App at 5 V and nominal L; 0.57 App worst case (VIN 5.5 V, L −30 %).
- Peak inductor current at the 2 A rating: 2 A + 0.29 A = 2.29 A; even at the IC's 3 A typical
  high-side limit plus ripple (≈ 3.0 A) L2 is far from saturation.
- Output ripple ΔV ≈ ΔIL / (8 × fSW × C): with the ≈ 36 µF effective on `+3V3` (below) ≈ 0.8 mV;
  with C18 alone at −50 % (5 µF) ≈ 5.6 mV. Ceramic ESR is neglected.

### Capacitors

- Input: C10 10 µF 25 V X5R 0805 at VIN (C10.1 0.515 mm from U3.4), C12 100 nF beside it on the
  outside (2.43 mm). TI's layout has one input capacitor (Fig. 20 p.13), C10's role; TI says
  4.7 µF is enough (p.10). C12 is an extra with little high-frequency effect where it sits.
- Output: C18 10 µF 25 V X5R + C19 100 nF at the buck, plus the rest of `+3V3`: ≈ 30 µF nominal on
  the board (C18, C29 4.7 µF, C32 10 µF, C8 4.7 µF, the 100 nF parts) and the module's own
  capacitors on its 3.3 V input (≈ 40 µF on the Waveshare ESP32-P4-Core schematic p.1). That is
  ≈ 60–70 µF nominal, ≈ 36 µF effective at 3.3 V bias (estimate). TI's Table 4 tests up to
  2 × 22 µF and says "further combinations should be checked": a first-board load-step check.

### Layout

U3 is turned 180° against TI's Fig. 20 (SLVSDG1C p.13), with the same loops:

- L2 south of U3, L2.1 (SW) 2.25 mm from U3.3; the switch node is one 0.8 mm F.Cu track, 4.3 mm.
- C10's VIN end 0.52 mm from U3.4; C18's GND end 0.668 mm from U3.2; C18, C19 and C10 return to
  U3.2 on F.Cu.
- R16, R17 and C17 at U3.5; the feedback is sensed at C18.1 and routed away from the switch node.
- C10, C12, C18 and C19 each have two GND vias, U3.2 one under the body. In1 and In2 GND are
  solid under U3, L2 and the switch node.
- The accelerometer's I²C lines pass L2 on B.Cu, below both GND planes; on F.Cu they keep
  ≥ 1.015 mm from switch-node copper, and IMU_SDA crosses the corner of L2's courtyard margin
  (not its body) for ≈ 0.7 mm ([IMU](imu.md#i2c-bus)).
- `+3V3` is class `PWR_3V3`, 0.65 mm (ESP32-P4 HDG §1.4.1 "at least 25 mil"); ΔT 1.6 °C at
  0.66 A. Probe it at TP4.

### +3V3 loads

| Load | Current | Basis |
|---|---|---|
| U6 ESP32-P4-Core | ≈ 450 mA | Espressif HDG: 380 mA minimum operating supply current (incl. flash/PSRAM) + MIPI PHY 50 mA + USB PHY 20 mA |
| U1 IP101GRI | ≈ 45 mA | < 0.15 W (IP101G DS p.1) |
| Camera module (CAM_3V3, through R53 0 Ω) | ≤ 100 mA | estimate; the module has no datasheet |
| D1 status LED | ≤ 15 mA | 5 mA per colour (Yongyu YY-0807BRGK26-07-FI DS p.2, p.4) |
| J1 link/activity LEDs | ≈ 12 mA | 2 × 6 mA through R4/R5 220 Ω |
| U12, U7, U9, pull-ups, dividers | ≤ 8 mA | |
| **Total** | **≈ 0.65 A (estimate)** | U3 rated 2 A (SLVSDG1C p.4) |

### Thermal

At 0.66 A the efficiency is ≈ 95 % (SLVSDG1C p.1 efficiency curve): ≈ 0.10 W, RθJA 188.2 °C/W
(p.4) → +19 °C, TJ ≈ 49 °C at 30 °C. Even at a pessimistic 84 % (0.4 W) TJ stays ≤ 105 °C, under
the 150 °C shutdown. L2 dissipates ≈ 6.5 mW.

## USB VBUS

USB-C J2 (HRO TYPE-C-31-M-12) is a sink only: CC1/CC2 have 5.1k Rd (R25/R23), and U4's IN2 blocks
any current back into the host. The connector, the data lines and their protection are in
[MCU](mcu.md#usb-c). The VBUS path:

- **J2 A4/A9/B4/B9 → `VBUS` → U4 IN2.** Range 4.40–5.5 V. The minimum is the USB 2.0 low-power
  port VBUS, 4.40–5.25 V (USB 2.0 Table 7-7 p.178; §7.2.1.3 p.173). A high-power function, which
  this board is (≈ 0.49 A on USB), must enumerate at 4.40 V and run at full power from 4.75 V
  (§7.2.1.4 p.174, §7.2.2 p.175), so designing down to 4.40 V is a conservative floor. The
  maximum is the 5.5 V that USB Type-C allows legacy devices to see at light
  load (USB Type-C Cable and Connector Specification R2.0 §4.4.2, p.141: "up to 5.5V maximum").
- **D2 TI TSD05DYFR**, a TVS at J2's VBUS pads (D2.1 under J2.A4, ≤ 5 mm from both VBUS pad pairs):
  VRWM 5.5 V, VBR 6 V, IEC 61000-4-2 ±30 kV contact, TLP clamp 7.5 V at 16 A, 19 pF (SLVSH43C
  p.1, p.6). TI's TPS2121 hot-plug guidance asks for a TVS on a hot-plugged input (SLVSEA3F §10.7
  p.31–32). When IN2 turns off at ≈ 3 A, a ≈ 1 µH cable into C16 gives
  3 A × √(1 µH / 4.7 µF) ≈ 1.4 V of overshoot, ≈ 6.9 V, far under U4's 24 V absolute maximum.
- **C16 4.7 µF X5R** at IN2. On attach the host sees C16 plus D2's 19 pF, under the 10 µF maximum
  load at the downstream end of a cable (USB 2.0 §7.2.4.1, p.177); the `+5V` bulk is behind U4's
  soft start.
- **R24 100k to GND.** With PoE only and an unpowered host plugged in, U4's IN2 leakage (≤ 5 µA up
  to 85 °C, SLVSEA3F p.7) flows into R24 ∥ (R27 + R28) = 55 kΩ: ≤ 0.27 V on VBUS, under vSafe0V
  (0.8 V maximum, USB Power Delivery specification, which USB Type-C R2.0 refers to for it).
- **VBUS_SENSE:** R27 47k / R28 75k with C26 100 nF to U6 pin 8, GPIO6, a digital input. With the
  VBUS range above, the `+3V3` set point 3.199–3.440 V and 1 % resistors: 4.40 V gives ≥ 2.68 V,
  above VIH 0.75 × 3.440 V = 2.58 V; 5.5 V gives ≤ 3.41 V, under VDD + 0.3 V = 3.499 V at 3.199 V
  (ESP32-P4 DS p.89). Firmware turns the USB Serial/JTAG D+ pull-up off while it is low
  ([firmware rules](firmware-rules.md#rules)).
- **Routing:** 0.25 mm F.Cu stubs from J2.A9/A4 to two vias (the only way out between J2's NPTH
  pegs and the neighbouring pads; ≈ 11 °C rise per stub at 1.0 A, 3 °C at 0.5 A, 1.8 mm long), a
  0.8 mm B.Cu link and a 1.0 mm B.Cu trunk to U4's IN2 via.

**Current on USB alone.** U4's current limit (≈ 2.5–3.5 A) does not hold a USB-only load to the
500 mA a high-power port supplies (USB 2.0 Table 7-7, ICCHPF). On USB alone the LED supply stays
off by a firmware rule only
(user decision 2026-10-31, "LEDs off on USB: firmware rule only"; no extra hardware gate), which
leaves the buck input: ≈ 0.49 A at 5 V for the estimated 0.65 A on `+3V3`, right at 500 mA. The
LED switch U8, the boost enable and the LED gate all default off through pull-downs (R57, R59,
R56), so nothing draws LED current before firmware runs.

## Accepted risks

- **Some isolation only on the PoE lane.** Other nets' F.Cu copper is ≥ 1.0 mm sideways (TP10.1
  exactly 1.000 mm) from the In1 PoE copper through one 0.2104 mm 7628 ply, not the 2 mm kept on
  the same layer. The layout is meant to give some isolation only; no isolation level is rated or
  tested (user decision 2026-11-02).
- **DP1435 output tolerance and capacitive load are not specified** (DP1435 p.1–5). The rails
  assume one 5 % regulation term (≤ 5.41 V) and start-up into ≈ 690 µF; both are first-board checks.
- **CP2 reference from `+3V3`** instead of a precision reference (TI Fig. 10-12): the thresholds
  move with `+3V3` (included in the worst case above), and the switchover cannot pull `+3V3` out of
  regulation for more than the 5 µs switchover (≥ 3.16 V at the worst corner, with the board's
  `+5V` copper drop). Accepted in the schematic review (2026-11-01).
- **USB current is limited by firmware, not hardware** (user decision 2026-10-31: LEDs off on USB
  by a firmware rule only). The USB-only estimate, ≈ 0.49 A, is at the 500 mA port limit; a
  firmware fault that enables the LED supply on USB draws more than a port supplies.
- **PoE loss with USB attached.** The TPS2121 switches to IN2 at once, so the host port briefly
  sees up to the current limit while `+5V` recharges (≈ 0.12 ms idle, ≈ 0.4 ms with the LEDs
  running), and with the LEDs running up to ≈ 1.96 A until firmware sees `PWR_SRC` low and drops
  LED_EN, BST_EN and LED_PWR_EN. A host whose port trips fast may cut VBUS and reset the board.
- **PoE power budget rests on an unpublished efficiency.** DP1435 p.5 item 7 gives 84 % as the
  maximum efficiency only. At the 1.96 A `+5V` peak, which recurs while the boost recharges
  `+32V` ([+5V loads](#5v-loads)), the PD draws ≥ 12.0 W and ≥ 0.32 A per pair at 37 V; at 78 %
  efficiency it would reach the 12.95 W 802.3af PD limit and J1's 350 mA per centre tap. User
  decision 2026-11-02: "Accept, measure on first boards"; if the measurement is over either
  limit, a firmware LED limit on PoE follows, with no design change now.
- **C15 and C23 closer than JLCPCB's 1.0 mm QFN spacing.** C15–U4 0.545 mm and C23.1–U4.1
  0.592 mm, kept at the pins as TI asks ("as close as possible", SLVSEA3F §11); C23 cannot move
  because C20's courtyard abuts it.
- **C12 outboard of C10 at the buck input.** Swapping them needs TP12 and the RMII_TX_EN track
  moved; TI's layout has one input capacitor, which C10 is.
- **`+3V3` capacitance outside TI's tested cells** (≈ 60–70 µF nominal with the module's own).
- **Reflow ratings of C13, C14 and C22 are not printed** in their datasheets (JLCPCB reflows at
  255 ± 5 °C). User decision 2026-10-31: "Ignore reflow times and temperatures. Assume it will all
  work out."

## First-board checks

1. **DP1435 output.** `+5V_POE` (at C11) at no load and at full load, with an 802.3af PSE at 44 V
   and at 57 V: must stay ≤ 5.5 V.
2. **PoE start-up.** Scope `+5V_POE` and `+5V_POE_F` at PoE plug-in (cold start): monotonic rise,
   no hiccup; the module stays up through 0.5 ms / 120 fps LED pulses.
3. **Switchover thresholds.** With U2 not fitted, a bench supply on `+5V_POE_F` (C15) and USB on
   J2: ramp the supply down and up and read `PWR_SRC`. Expected: to USB at 3.49–4.08 V (3.70 V
   typical), back to PoE at 3.80–4.42 V (4.03 V typical).
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
   120 fps with Ethernet linked (LEDs off): expected ≈ 0.49 A, must be ≤ 0.5 A. Measure the camera's
   share by replacing R53 with a 0.1 Ω shunt.
7. **`+3V3` load step.** 0.1 → 0.6 A at 5 V input: settles without ringing, overshoot ≤ 5 %. If
   not, reduce C18 or remove C17.
8. **Temperatures.** At PoE with the LEDs at 0.5 ms / 120 fps (and once during `+32V` start-up),
   a thermal camera on U4 and its IN1/OUT vias, the U8 neck, U3, L1 and U2.
9. **GND test loops.** Insert one RH-5002 loop into TP2's 1.0 mm plated hole before soldering:
   the drawing (LCSC C5199802) gives no hole size; if it is loose or tight, change the drill in
   the footprint.
