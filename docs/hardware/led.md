# LED driver and ring

The 850 nm ring lights the markers only while the camera exposes. The chain:
`LED_PWR_EN` connects +5V to the boost through a load switch, the boost makes +32V, the string of
12 LEDs runs from +32V into a linear current sink, and the sink's setpoint is on only while the
sensor's STROBE and the P4's `LED_EN` are both high and the STROBE pulse is younger than a
hardware timeout. Firmware runs the boost just above what the string needs, by PWM on `BST_EN`
and a reading of the sink's drain (`LED_K_SENSE`), so the sink drops about 1 V instead of 9–13 V
([Headroom control](#headroom-control)). Sheets: *LED driver* and *LED ring*.

| Block | Parts | Control (U6 pin) | Test point |
|---|---|---|---|
| [LED power switch](#led-power-switch) | U8 TI TPS22918DBVR, C34, C35, R57 | `LED_PWR_EN` GPIO20 (U6.16) | – |
| [Boost converter](#boost-converter) | U10 TI TPS61170DRVR, L3, D3, C36, C38, C39, C41, C43, C45, C46, C47, R59, R62, R65, R66 | `BST_EN` GPIO53 (U6.71), PWM | TP13 `+32V` |
| [Current sink](#current-sink) | U11 TI TLV9061IDBVR, Q1 Infineon IRLR024NTRPBF, R69, NT1, R60, R61, R63, R64, R67, R68, C40, C42, C44 | – | TP12 `LED_ON`, TP14 `ISNS` |
| [STROBE gating and timeout](#strobe-gating-and-timeout) | U9 TI SN74LVC1G97DBVR, C33, R54, R55, R56, R58, C37 | `LED_EN` GPIO11 (U6.13), STROBE GPIO12 (U6.14) | TP11 `LED_EN`, TP9 `STB` |
| [LED ring](#led-ring) | D4–D15 Amicc A-SC880IR3C-P03-1T | – | – |
| Sense | R71, R72, C49 (`VLED_SENSE`); R70, R76, C48 (`LED_K_SENSE`) | GPIO21 ADC1_CH5 (U6.17), GPIO23 ADC1_CH7 (U6.19) | – |

The product limits that set every number below:

- **Pulse:** ≤ 0.5 ms per frame at ≤ 120 fps, so ≤ 6 % duty (user decision 2026-10-31).
- **Ambient:** 15–30 °C, indoor (user decision 2026-10-31).
- **LED current:** on the Amicc DC derating curve at 30 °C (user decision 2026-10-31,
  [Current sink](#current-sink)).
- **USB power alone:** the LEDs stay off by a firmware rule, with no extra hardware gate
  (user decision 2026-10-31).

Firmware rules for this block are in [firmware-rules.md](firmware-rules.md#rules). Layout details
are in [layout.md](layout.md#led-driver-and-ring).

## LED power switch

U8 (TPS22918, TI SLVSD76C) connects +5V to `+5V_BST`, the boost input. The boost needs this
switch because a boost converter has a DC path from input to output through L3 and D3 even while
it is not switching. Without U8, +32V would sit at about +5V minus a diode drop whenever the board
is powered. U8 is the one hardware switch that removes all power from the LED side. The schematic
places it with KiCad's TPS22917DBV symbol: the Value, MPN and Description are the TPS22918, which
has the same DBV pinout (SLVSD76C p.3).

| Item | Value | Reason / source |
|---|---|---|
| ON | `LED_PWR_EN`, GPIO20 (U6.16), R57 100 kΩ to GND | ON must not float (SLVSD76C p.3). The pull-down keeps the switch off from reset until firmware drives the pin, below ON VIL 0.5 V (p.4) |
| CT | C35 10 nF C0G 50 V (Murata GRM1885C1H103JA01D) | Slew rate 0.55 × 10000 + 30 = 5530 µs/V (SLVSD76C p.14 Eq. 3, Table 2): `+5V_BST` rises in ≈ 26.6 ms at 5 V. Inrush into ≈ 670 µF (C36, then C43/C45/C46/C47 through L3 and D3) ≈ 670 µF × 5 V / 26.6 ms ≈ 0.12 A (p.19 Eq. 9), well under the 2 A rating, so the +5V rail sees no step |
| QOD | Pin 5 open (no-connect flag) | Quick output discharge disabled, an allowed option (p.3, p.13) |
| Input | C34 1 µF 50 V at U8.1 | CIN ≥ 1 µF (p.4, p.17). The +5V bulk (C20 47 µF, C22 100 µF) is larger than `+5V_BST`'s C36 10 µF, as p.17 advises |
| Load | 1.26 A average, 1.44 A peak at the boost's current limit (TPS61170 ILIM max 1.44 A, SLVS789D p.5, minus half the ripple) | Below the 2 A rating at 70 °C (p.4); VIN 4.68–5.41 V on PoE ≤ 5.5 V |

The `+5V_BST` path leaves U8 at pin 6 on 0.5–1.0 mm F.Cu to C36 and L3, necked to 0.5 mm at U10's
pins ([layout](layout.md#led-driver-and-ring)).

### Boost input range

The LEDs run only on PoE (firmware rule), so the boost input range is the one on PoE:

| Rail | Range | Derivation |
|---|---|---|
| `+5V` on PoE | 4.68–5.41 V | `+5V_POE_F` 4.85–5.41 V ([Rails](README.md#rails)) minus the TPS2121 drop, RON ≤ 85 mΩ × 1.96 A board peak = 0.17 V |
| `+5V_BST` | 4.57–5.41 V | `+5V` minus U8's drop, RON ≤ 79 mΩ (SLVSD76C p.5) × 1.44 A = 0.11 V |

During a PoE → USB switchover `+5V` dips to ≥ 3.26 V at U4 OUT (≈ 19 mV less at U8.1) for a moment ([power mux](power.md#power-mux)).
The boost loses regulation then, and firmware shuts the LEDs down on the same `PWR_SRC` edge.

### Enable order

The enable order is stated once, as **FW-7** in [firmware rules](firmware-rules.md#rules). This is
a summary of it. `PWR_SRC` comes from the [power mux](power.md#power-mux); low means USB.

**Power-up (on PoE only)**
1. `LED_PWR_EN` high.
2. Wait a fixed 50 ms for `+5V_BST`. There is no `+5V_BST` sense; C35 sets a 10–90 % rise of only
   ≈ 27 ms (SLVSD76C Eq. 3 p.14), so 50 ms covers the full swing and the turn-on delay.
3. `BST_EN` high.
4. Wait for +32V on `VLED_SENSE`.
5. `LED_EN` pulses.

**Normal power-down**
1. `LED_EN` low.
2. `BST_EN` low.
3. Wait ≥ 3 ms. The TPS61170 shuts down only after CTRL has been low for more than 2.5 ms
   (SLVS789D p.5, §8.3.5 p.10). In PWM mode a shorter low only lowers the reference, through a
   180 µs filter (p.5, §8.4.2 p.11).
4. `LED_PWR_EN` low.

**`PWR_SRC` falling edge (PoE lost, now on USB), taken as an interrupt**
- `LED_EN`, `BST_EN` and `LED_PWR_EN` go low back to back, with no wait, because the USB supply
  must not carry the boost.
- The boost then stops on its collapsing input (UVLO 2.2–2.5 V, SLVS789D p.5). This is accepted:
  `LED_EN` is already low, so no LED current flows.

Why this order (design reasoning, apart from the datasheet timings above):

- **`LED_EN` first:** no LED pulse starts while the supply is going away.
- **`BST_EN` before `LED_PWR_EN`, with the 3 ms wait:** on a normal shutdown the boost is stopped
  by its own control pin before its input is removed.
- **`BST_EN` only after the `+5V_BST` wait on the way up:** U10's soft start ramps its reference
  in only 6.8 ms (32 × 213 µs, SLVS789D p.10 §8.3.1). Started together with U8, the ramp would be
  over while `+5V_BST` is still rising.

## Boost converter

U10 (TPS61170, TI SLVS789D) boosts `+5V_BST` to +32V for the LED string. It has an integrated
40 V switch, runs at 1.2 MHz (1.0–1.5 MHz) and limits the switch current to 0.96–1.44 A (p.5).

### Output voltage

R65 255 kΩ 1 % (0603) and R66 10.0 kΩ 1 % set the output: Vout = VREF × (1 + R65/R66) (SLVS789D
p.17 Eq. 2). R66 = 10 kΩ is TI's recommended value (p.17). These are the values with `BST_EN`
steady high (PWM duty 1), the highest the output can be; in operation firmware runs it lower
([Headroom control](#headroom-control)).

| | VREF (p.5) | Resistors | Vout |
|---|---|---|---|
| Minimum | 1.204 V | R65 −1 %, R66 +1 % | 31.30 V |
| Nominal | 1.229 V | nominal | 32.57 V |
| Maximum | 1.254 V | R65 +1 %, R66 −1 %, plus IFB 200 nA × R65 | 33.93 V |

Everything on +32V is rated above 33.93 V:

| Part | Rating at 33.93 V |
|---|---|
| U10 SW | 40 V abs. max (p.3), 34.6 V with D3's drop |
| D3 SS16 | 60 V |
| Q1 IRLR024N | 55 V |
| C43/C45 | 100 V |
| C46/C47 | 50 V (68 %) |
| R65/R71 (0603 thick film) | 75 V working voltage (UNI-ROYAL chip-resistor datasheet p.5) |

Full scale can't go lower: at duty 1 the end of a pulse leaves V(`LED_K`) ≥ 31.30 − 29.92 ≈ 1.38 V
on every unit (the table under [String voltage vs +32V](#string-voltage-vs-32v) without Q1's
0.11 V), above the headroom loop's band, so the loop always finds its band by lowering the duty.
Every volt above what the string needs is dissipated in Q1 ([Thermal](#thermal)).

### Headroom control

`BST_EN` carries a PWM signal (user decision 2026-11-03, design option 1b). In PWM mode the
TPS61170 chops its 1.229 V reference at the duty cycle D and filters it: V_FB = D × 1.229 V
(SLVS789D §8.4.2 p.11, Eq. 1). With R65/R66 unchanged, +32V = D × 32.57 V nominal.

| Item | Value | Reason / source |
|---|---|---|
| PWM frequency | 50 kHz | Inside TI's 5–100 kHz (p.11). Its low phases (≤ 20 µs) stay far below the 260 µs EasyScale detection time and the 2.5 ms shutdown delay (p.5, p.10), so PWM never selects EasyScale or shuts the boost down. The 40 ns on-time error adds ≤ 0.2 % duty (p.11); the loop absorbs it |
| Reference in PWM mode | 477–507 mV at a 492 mV setting (±3 %) | p.5 V(REF_PWM). Open loop the set point is only that good; the loop measures the result instead |
| Duty range | 0.62–1 | 1 is steady high: 31.30–33.93 V. 0.62 sets ≈ 20.2 V nominal (≈ 20.8 V with the +3 % reference), below the lowest string (20.4 V) plus its ≈ 1.2 V need, so the loop reaches every string; an ADC fault that drives D down only dims the LEDs |
| Reference ripple | ≈ 1–4 % p-p of V_FB | First order through the internal t_REF filter (180 µs, p.5; Fig. 9 p.11): ≈ 1.229 V × D(1 − D) × 20 µs / 180 µs = 32 mV (4.2 %) at D 0.62, 24 mV (2.6 %) at D 0.77, 12 mV (1.1 %) at D 0.9. The boost loop (≤ 0.8 kHz crossover, [Compensation](#compensation)) passes ≈ f_c / 50 kHz of it: ≲ 15 mV at +32V (estimate) |

Firmware (FW-22 in [firmware rules](firmware-rules.md#rules)) reads V(`LED_K`) through
`LED_K_SENSE` ([LED_K sense](#led_k-sense)) late in each LED pulse and steps D so that the reading
at the end of the pulse stays in a **0.6–0.9 V** band:

- **Lower edge 0.6 V.** Q1 needs ≥ 0.11 V (0.915 A × 0.080 Ω × 1.5 hot, IRLR024N p.2) to hold the
  current at VGS ≤ 4.9 V. The reading is good to ±0.18 V, so 0.6 V leaves ≥ 0.42 V, 0.31 V above
  that need.
- **Upper edge 0.9 V.** A 0.3 V band is ≥ 7 duty steps (≤ 41 mV each at the output, FW-8), so
  the loop settles inside it instead of hunting between two steps.
- **Set points.** +32V = string + R69 + copper + droop + ESR + V(`LED_K`): from ≈ 21.8 V (lowest
  string, 20.4 V) to ≈ 31.0 V (28.8 V string, 0.915 A, 528 µF, reading 0.18 V low at the band's
  top), ≈ 25 V (D ≈ 0.77) for a typical string. The highest stays under the 31.30 V full-scale
  minimum.
- **Start-up.** The boost starts at duty 1 as before (FW-7, FW-8); the loop starts with the pulses.
  Only the pulses lower +32V (the boost can't sink current): each removes ≤ 0.46 mC from 662 µF,
  ≈ 0.63 V at the nominal 0.831 A. Fine steps alone would take ≈ 185 steps × 8 samples ≈ 1 500 lit
  frames, ≈ 12 s at 120 fps, so the loop takes one coarse step first: from the first averaged
  reading V at duty 1 it sets D = 1 − (V − 1.2 V) / 34.3 V. 34.3 V per unit of duty is the
  steepest +32V(D) on any unit (32.57 V × 1.053: ±3.05 % reference, ±1.9 % divider, +0.2 %
  on-time), and the reading is good to ±0.43 V at 13 V (±15 mV × 11 + 2 %), so after the step
  V(`LED_K`) lands in 0.77–2.8 V: never below the band's need, at most ≈ 1.9 V above its top. The
  bank takes ≈ 20 pulses to come down (≤ 12 V at ≈ 0.63 V a pulse), then ≤ ≈ 46 fine steps finish,
  ≈ 3 s at 120 fps. Q1 dissipates the duty-1 figures only for the first ≈ 30 frames (≈ 0.25 s),
  then ≤ (2.8 + 0.44) V × 0.915 A × 6 % ≈ 0.18 W.
- **Fallback.** Without a valid reading the firmware holds D = 1: the full-scale state the
  [Thermal](#thermal) bound and the drain copper are sized for.

### Power stage

| Part | Value | Check (source) |
|---|---|---|
| L3 | Changjiang FNR4030S100MT, 10 µH ±20 % | Inside TI's 10–22 µH (SLVS789D p.4, p.18). Isat 1.95 A > ILIM max 1.44 A; Irms 1.50 A > 1.26 A average input at the current limit; DCR ≤ 0.130 Ω (Cjiang catalog p.10). Ripple at 5 V in, 32.57 V out: 0.354 A p-p, 37 % of the ≈ 0.96 A input, inside TI's 30–40 % (SLVS789D p.17 Eq. 3, p.18) |
| D3 | MDD SS16 Schottky, 60 V, 1 A | TI asks for a diode rated above the 40 V switch (p.18). Average ≤ 0.16 A, peak 1.49 A vs IFSM 25 A, loss ≈ 0.11 W (MDD datasheet p.1) |
| C36, C38 | 10 µF 25 V X5R 0805 + 100 nF | CI ≥ 1 µF is met (p.4). C36 is above TI's recommended 1–4.7 µF (p.19). That is harmless: more input capacitance only lowers the ripple on `+5V_BST`. Loop stability is set by the output capacitors and the COMP network, not by CI, and U8's slew rate limits C36's inrush. The 0805 X5R part keeps ≈ 6.5 µF at 5 V (estimate, DC-bias derating) |
| C43, C45 | 1 µF 100 V X7R 1206 (YAGEO CC1206KKX7R0BB105) | High-frequency output capacitors right at D3's cathode |

The duty cycle is D = 1 − Vin·η/Vout, with η ≈ 0.83 (p.7 Fig. 1). D is 0.873 at 5 V in / 32.57 V
out, and 0.888 at 4.57 V (the `+5V_BST` minimum on PoE) / 33.93 V. Both are under the 90 % minimum
Dmax (p.5, p.18).

With losses (DCR 0.13 Ω, RDS(on) 0.6 Ω max, 0.96 A input), regulation holds down to ≈ 4.1 V in.
`+5V_BST` stays above that in steady state on PoE. Only the switchover dip above goes lower.

### Output capability

Worst-case output current is Iout_max = Vin × (ILIM − IP/2) × η / Vout (SLVS789D p.17 Eq. 3/4).
With these inputs it is **77–81 mA**:

- Vin 4.57 V (`+5V_BST` minimum on PoE),
- ILIM 0.96 A (minimum, specified at D = Dmax, p.5),
- L3 −20 % (8 µH),
- fs 1.0 MHz,
- Vout 33.93 V, VF 0.7 V,
- η 0.80–0.84.

The upper bound is ≈ 190 mA (193 mA). Its inputs are Vin 5.41 V, ILIM 1.44 A (maximum, p.5),
L3 +20 % (12 µH), fs 1.5 MHz, η 0.85, Vout 31.30 V and VF 0.7 V.

Typical delivery is ≈ 121 mA (5 V, 10 µH, 1.2 MHz, η 0.83). Its ILIM is ≈ 1.13 A, read from
SLVS789D Fig. 4 p.7 (switch current limit vs duty cycle) at the operating duty of ≈ 0.87, not the
1.2 A typical of the p.5 table.

Demand at the pulse limit is 0.915 A × 0.5 ms × 120 Hz + 0.25 mA (the two dividers) + ≤ 0.31 mA
(the `LED_K_SENSE` divider, through the string) = **55.5 mA** worst case (50 mA at the nominal
0.831 A). The margin is ≥ 1.39×.

These figures are at full scale (duty 1, Vout up to 33.93 V). At the headroom loop's set points
(≈ 21.8–31.0 V) the stage delivers proportionally more (Iout ∝ 1/Vout), so the margin only grows.

### Recovery and start-up

These are estimates; the [first-board checks](#first-board-checks) cover both.

**Per frame.** Each pulse takes ≤ 0.46 mC out of the bank. To correct the 0.92 V droop at its
≈ 450 Hz crossover, the loop would ask for about C·ΔV·2π·fc = 662 µF × 0.92 V × 2π × 450 Hz ≈ 1.7 A
of output. That is 9–22× what the stage can deliver (77–190 mA over the tolerances). So the error
amplifier saturates, and the boost runs at its switch current limit from the start of each pulse
until the charge is back.

That takes t = 0.46 mC / Iout_max = 2.4–6.0 ms (≈ 3.8 ms typical) of every 8.33 ms frame. During
that time `+5V_BST` carries the current-limit input, ≤ 1.26 A average and 1.44 A peak, so on PoE
this peak recurs every frame ([power mux](power.md#power-mux)).

Averaged over a frame, energy sets the input:

- **Nominal:** 32.57 V × 50 mA / 0.83 / 5.15 V ≈ 0.38 A.
- **Worst case:** 33.93 V × 55.5 mA / 0.80 / 4.57 V ≈ 0.52 A.

Both are at full scale. At the loop's set points the bank holds the same charge per pulse at a
lower voltage, so the recharge is shorter (Iout ∝ 1/Vout) and the input energy per frame falls
with Vout: these figures are upper bounds.

**Start-up.** When `BST_EN` goes high, the FB reference ramps in 32 steps of 213 µs (6.8 ms), and
the switch current limit is half the normal value (≤ 0.7 A typ) for the first 5 ms (SLVS789D p.10
§8.3.1, p.5). The bank needs ½ × 662 µF × (32.57 V)² ≈ 0.35 J, far more than the stage delivers
in 6.8 ms, so +32V rises at the current limit.

Stepping t ≈ ∫ C·V dV / (Vin·(ILIM − IP(V)/2)·η) up from +32V ≈ `+5V_BST` − 0.4 V (with the half
limit for 5 ms) gives:

| Case | Inputs | Start-up time |
|---|---|---|
| Nominal | 662 µF, 5.15 V, ILIM 1.2 A, 10 µH, 1.2 MHz, η 0.83 | ≈ 80 ms |
| Range | C ±20 %, Vin 4.57–5.41 V, ILIM 0.96–1.44 A, L ±20 %, fs 1.0–1.5 MHz, η 0.80–0.85 | 45–170 ms |

The nominal start-up uses the p.5 typical ILIM of 1.2 A rather than Fig. 4's 1.13 A. While +32V
rises the duty runs from near 0 up to ≈ 0.87, and Fig. 4 shows ≈ 1.2–1.23 A over 20–70 % duty.

Firmware waits for `VLED_SENSE` instead of a fixed delay.

### Output capacitors C46, C47

The bulk is two **Panasonic EEEFT1H331AP** (330 µF 50 V, FT series, 10 mm can, LCSC/JLCPCB
C178596). C46 sits at the pulse loop, 7.27 mm from Q1's tab. C47 sits at the boost end of the
+32V trunk.

The capacitance comes from the pulse. Two parts at 330 µF −20 % give 528 µF. A 0.915 A × 0.5 ms
pulse drops the rail by 0.915 × 0.5 ms / 528 µF = 0.87 V, plus 0.055 V across the two ESRs in
parallel (0.12 Ω each). That fits the string headroom ([LED ring](#led-ring)).

Why this part:

- **Economic PCBA.** The JLCPCB part page lists it as "Economic and Standard", SMT assembly
  (https://jlcpcb.com/partdetail/C178596, read 2026-10-31). The ROQANG RVT1H221M1010 220 µF used
  before is "Standard only", which the Economic PCBA order can't place.
- **Lowest ESR and highest ripple rating** of the Economic-eligible 50 V candidates: 0.12 Ω at
  100 kHz and 900 mA rms (Panasonic FT datasheet, characteristics list). The bank carries
  ≈ 0.915 A × √0.06 ≈ 0.22 A rms in total, against 900 mA × 0.65 (120 Hz factor) = 585 mA rms per
  capacitor.
- **Temperature and voltage.** Rated −55…+105 °C, ±20 % (FT datasheet p.1), and 50 V at
  33.93 V (68 %).
- **Stock.** It was in stock (17,481 at JLCPCB on 2026-10-31). The other candidates were
  EEEFT1H221AP (220 µF, 8 mm can) and Nichicon UWD1H221MCL1GS (220 µF, 1,685 in stock). The
  330 µF part gives the most capacitance in the same 10 mm can.

The can is 10.8 mm tall (L 10.2 + 0.3 mm tolerance + 0.3 mm seat plate). The lens view cone needs
it ≥ 7.0 + 0.83 × 10.8 = 15.96 mm from the lens axis. The nearest courtyard is C46's chamfered
corner at 24.0 mm (C47 25.7 mm). The footprint is KiCad's `CP_Elec_10x10` (pads 4.0 × 2.5 mm, gap 4.0 mm). Against
Panasonic's G-size land pattern the gap is 0.6 mm smaller and the pads 0.5 mm wider, and the
terminals land fully.

### Compensation

R62 100 kΩ + C41 10 nF C0G put a zero at 159 Hz, and C39 220 pF C0G puts a pole at 7.2 kHz, all
on COMP. With TI's small-signal model (SLVS789D Eq. 7–11, p.19: Gea 320 µS, Rsense 0.1 Ω, Vin
5 V, Vout 32.6 V, Cout 662 µF), the loop crosses over at ≈ 450 Hz with ≈ 67° phase margin. The
right-half-plane zero sits at ≈ 120 kHz at 0.1 A, and the ESR zero of the two electrolytics at
≈ 4 kHz (0.06 Ω, 660 µF), both above the crossover.

At the headroom loop's set points the crossover rises with (1 − D_sw) = Vin·η/Vout. TI's model
(Eq. 7–11) puts it ≈ 1.45× higher at 22 V than at 32.6 V (≈ 0.65–0.75 kHz), with the phase margin
a few degrees higher, because the 159 Hz zero adds more phase there and the right-half-plane zero
moves up. Still an estimate on the same terms: [first-board check](#first-board-checks).

TI's recommended output capacitance is 1–10 µF, and other values "should be fully tested"
(p.4 note 1, p.19). 662 µF is far outside that range, so stability is a
[first-board check](#first-board-checks).

### BST_EN

`BST_EN` is GPIO53 (U6.71) on U10's CTRL pin. R59 100 kΩ pulls it down, together with U10's
internal ≈ 800 kΩ (p.5, p.10), so the boost stays off from reset until firmware drives the pin
(below CTRL VIL 0.4 V).

CTRL picks one of two ways to program the feedback reference at each start from shutdown (p.10):
PWM, the default, or TI's EasyScale. This board uses PWM ([Headroom control](#headroom-control)):

- **Start:** drive `BST_EN` steady high (duty 1). Never hold it low for ≥ 260 µs within 1 ms of a
  start from shutdown: that selects EasyScale mode, which drops the FB reference to 200 mV, so
  +32V would fall to ≈ 5 V and the LEDs would stay dark (p.10–11).
- **Run:** 50 kHz PWM, duty 0.62–1, set by the headroom loop. Its low phases last ≤ 20 µs.
- **Shutdown:** hold it low for ≥ 2.5 ms (p.5).

The enable order with `LED_PWR_EN` and `LED_EN` is under
[Enable order](#enable-order).

After `BST_EN` goes low, D3 blocks back-feed and C46/C47 stay charged. They bleed through the two
dividers, (R65 + R66) ∥ (R71 + R72) = 132.5 kΩ (τ ≈ 662 µF × 132.5 kΩ ≈ 88 s), and through the
string into R70/R76 (≤ 0.31 mA, [LED_K sense](#led_k-sense)), so +32V stays above 20 V for at most
≈ 43 s after a shutdown. During that time `LED_EN` low (with R56, and R68 at Q1's gate) keeps the
ring dark apart from that ≤ 0.31 mA bias.

### Output voltage sense

R71 255 kΩ 1 % (0603, for the 75 V working voltage) and R72 10.0 kΩ 1 % divide +32V into
`VLED_SENSE`, which goes to GPIO21 = ADC1_CH5 (U6.17). C49 100 nF sits at the pin (Espressif
ESP32-P4 HDG §1.3.10).

| Item | Value |
|---|---|
| Conversion | V(+32V) = V_ADC × 26.5 |
| Readings | 1.23 V at 32.57 V; ≤ 1.31 V at 33.93 V with resistor tolerance |
| Source impedance | 9.6 kΩ, with C49 τ ≈ 1 ms: a DC reading |
| Error | ±2 % from the 1 % resistors |

Firmware can read it to confirm the boost is up before enabling the LEDs, and to watch the rail.

The two dividers (R65/R66 and R71/R72) draw ≈ 0.25 mA from +32V; the `LED_K_SENSE` divider adds
≤ 0.31 mA through the string.

### Layout essentials

TI §11.1 p.23 asks for a short switch node. L3.2 is 1.65 mm and D3.2 1.66 mm from U10.4 (SW),
under a 2.0 mm limit. Pin 4 leaves at the 0.3 mm pad width, so the switch node keeps TI's land
gaps. C36.2 and C43.2 tie to U10's exposed pad on F.Cu, so both capacitor loops close on the top
layer as in TI Fig. 23 p.24. The exposed pad has 2 GND vias 1.1 mm apart (DRV0006A land pattern
p.35).

The +32V trunk starts at D3's cathode (U10 itself is not on +32V). Lengths are from kicad_query
`lengths` paths, with via heights counted:

| Path | Length | Vias |
|---|---|---|
| D3.1 → D4.2 | 89.9 mm (84.1 mm of track) | 2 × 0.6/0.3 |
| C45.1 → D4.2 | 92.9 mm | 2 |
| C47.1 → D4.2 | 62.1 mm | 2 |
| C46.1 → D4.2 | 24.9 mm | 0 |

The whole `+32V` net is 115.7 mm (112.6 mm of track + 3.1 mm of via height). That includes 20.9 mm
of 0.3 mm feeds to R65, R71 and TP13, which carry µA.

The trunk is mostly 0.6 mm wide, a pour at D3, and necked to 0.45 mm for 12.9 mm east of Q1. The
21.9 mm from D3 to C47 is 0.4 mm on F.Cu along the bottom edge, kept off the board copper under the
module U6 ([layout](layout.md#32v-and-the-ring)); it carries only the recharge current (≤ 0.22 A
peak). The pulse comes mostly from C46. Its path to D4.2 is ≈ 21.7 mΩ (22.19 mm of track:
9.28 mm at 0.6 mm + 12.91 mm at 0.45 mm, 35 µm copper), ≈ 0.02 V at 0.915 A. The headroom
budget uses ≈ 0.1 V, a bound over the whole D3 → D4 path.

See [layout](layout.md#led-driver-and-ring) for the copper and spacing rules.

## Current sink

Q1 (IRLR024N, 55 V logic-level N-MOSFET in TO-252) sits in series with the bottom of the string:
`LED_K` is the drain and `ISNS` the source. R69 goes from `ISNS` to GND. U11 (TLV9061 on +5V, TI
SBOS839N) regulates the voltage across R69 to the setpoint `ISET`.

### Setpoint

`ISET` is a divider R60 51.1 kΩ / R61 **1.40 kΩ** 1 % fed from `LED_ON`, the output of the gate U9
([STROBE gating](#strobe-gating-and-timeout)). U9's output high is +3V3 at a 63 µA load.

- **LED_ON high:** `ISET` = +3V3 × 1.40/(51.1 + 1.40) ≈ 88 mV at 3.3 V.
- **LED_ON low:** `ISET` = 0.

C40 1 nF C0G at U11.3 filters it: (R60 ∥ R61) × C40 = 1.36 µs, so the LED current follows
`LED_ON` within microseconds.

R63 1.00 MΩ from +5V and R64 1.00 kΩ from the Kelvin tap `ISNS_K` meet at U11's inverting input.
They add a fixed offset of 5 V × 1k/1001k = 5.0 mV. With `ISET` = 0 this is above the op-amp's
±1.6 mV Vos (SBOS839N p.13), so U11 drives Q1 fully off rather than leaving a trickle. With the
offset, the LED current is:

$$I_{LED} = \frac{(ISET \pm V_{os})\cdot\frac{R63+R64}{R63} - V_{+5V}\cdot\frac{R64}{R63}}{R69}$$

### LED current

The limit is the DC derating curve of the A-SC880IR3C (Amicc ADE-880-017 v1, p.4 Fig. 1). It runs
linearly from 1000 mA at 25 °C to 200 mA at 85 °C (13.3 mA/°C), so at the 30 °C ambient
maximum it allows **933 mA**.

The datasheet's 1200 mA peak rating applies only to ≤ 100 µs pulses at ≤ 1 % duty (p.3). The
0.5 ms / 6 % pulses here don't qualify for it. The user chose to stay on the DC curve instead of
keeping ≈ 0.89 A (user decision 2026-10-31).

| R61 | Nominal | Worst case | vs 933 mA |
|---|---|---|---|
| **1.40 kΩ (fitted, UNI-ROYAL 0402WGF1401TCE, C39650)** | **0.831 A** | **0.759–0.915 A** | ≈ 18 mA margin |
| 1.43 kΩ (rejected) | 0.849 A | 0.776–0.935 A | 2 mA over |

Inputs of the worst case:

| Input | Range | Source |
|---|---|---|
| +3V3 | 3.199–3.440 V | DC set-point range: TLV62569 VFB 0.588–0.612 V with R16 453 kΩ / R17 100 kΩ 1 % (TI SLVSDG1C p.4; [Rails](README.md#rails), [3.3 V buck](power.md#33-v-buck)). `ISET` follows `LED_ON`, which is +3V3 |
| +5V | 4.68–5.41 V | `+5V` on PoE, the only source the LEDs run on ([Boost input range](#boost-input-range)). U11 and R63 are on `+5V`, not `+5V_BST` |
| Resistors | ±1 % each | All value-setting resistors are 1 % |
| Op-amp offset Vos | ±1.6 mV | SBOS839N p.13 |

Nominal assumes 3.3 V and 5.0 V. At the set-point centre (3.318 V, `+5V` 5.15 V) it is 0.834 A.

The LED current depends on the DC level of +3V3, which the buck's set point bounds. So it uses the
3.199–3.440 V range, not the generic ±5 % envelope (3.135–3.465 V). With the envelope, the worst
case would be 0.742–0.922 A, 11 mA under the limit.

The ≈ 18 mA margin equals 1.35 °C of LED heating above the 30 °C ambient, a thin allowance. The
[first-board checks](#first-board-checks) test it.

R69 is a Walsin WW12WR100FTL, 0.1 Ω 1 % 100 ppm/°C, 1206, 1/3 W at 70 °C. It dissipates
0.915² × 0.1 = 84 mW during a pulse, 5 mW on average.

The sense is Kelvin-connected:

- **High side:** net tie NT1 sits 0.525 mm from R69.1. `ISNS_K` leaves through it to R64. The pulse
  reaches R69.1 from Q1.3 through the F.Cu pour "ISNS pour 122"; its lobe round the tap south of
  R69.1 carries no load current, so the pulse path is not in the measurement.
- **Low side:** R69.2 returns the pulse to GND through the F.Cu pour "GND pour 80" with three vias,
  plus a 0.4 mm link to a fourth; less than the ≈ 0.8 mΩ estimated for the former three 0.4 mm links
  (ρCu 1.72e-8 Ω·m, 35 µm copper, 0.3 mΩ per via). That is ≤ 0.8 % of R69, and it lowers the LED
  current by the same amount, on the safe side of the table.

### Loop and gate

| Part | Role |
|---|---|
| C42 220 pF C0G | U11 integrator capacitor (SINK_DRV to IN−), with R64 1 kΩ |
| R67 220 Ω | Gate resistor. It isolates U11's output from Q1's 480 pF input capacitance (IRLR024N p.2). Gate-charge peaks ≤ 23 mA, µs long |
| R68 100 kΩ | Gate pull-down: Q1 stays off while U11 is unpowered |
| C44 100 nF | U11 supply bypass |

U11 swings to within 20–60 mV of its rails (SBOS839N p.13), so VGS reaches ≈ 4.9 V. That is well
above Q1's ≈ 3.5 V plateau (IRLR024N p.4 Fig. 6), so Q1 has drive left over.

The loop estimate uses gm ≈ 2.4 S at 0.9 A, scaled from gfs 8.3 S at 11 A (p.2). The source
follower then has a gain of gm·R69/(1 + gm·R69) ≈ 0.19, and the loop crosses over at
0.19/(2π × 1 kΩ × 220 pF) ≈ 140 kHz. That is below the gate pole (≈ 1.5 MHz) and U11's 10 MHz
GBW. The datasheet doesn't give gm at 0.9 A, so the phase margin is an estimate:
[first-board check](#first-board-checks).

In the headroom loop's band Q1 runs near or inside its linear (triode) region at the end of a
pulse. U11 then drives VGS higher, still well under its ≈ 4.9 V swing, because the band's 0.42 V
minimum is ≈ 4× the 0.11 V that RDS(on) needs at 4.9 V. The lower gm there lowers the crossover,
which adds phase margin. An estimate too: [first-board check](#first-board-checks).

**Q1 ratings.**

- **Safe operating area:** 13.3 V × 0.915 A for 0.5 ms lies far inside the 1 ms line (IRLR024N p.4
  Fig. 8).
- **Turn-off:** ≈ 250 nH of loop at 0.915 A over ≥ 0.3 µs overshoots by ≈ 0.75 V, so
  VDS ≤ 34.7 V < 55 V. The part is also avalanche-rated (EAS 68 mJ).
- **Body diode:** never conducts, because the source goes to R69 and GND.

### LED_K sense

R70 100 kΩ 1 % (0402) from `LED_K`, Q1's drain, and R76 10.0 kΩ 1 % to GND divide V(`LED_K`) by
11.0 into `LED_K_SENSE` on GPIO23 = ADC1_CH7 (U6.19). C48 1 nF C0G sits at the pin. They replace
the NTC TH1 and its +3V3 pull-up, and reuse that route (user decision 2026-11-03: "Replace
TH1/R70 on NTC_SENSE (GPIO23)", divider "100k / 10.0k (÷11)"). R70 sits where TH1 was, its
`LED_K` pad joined to Q1's tab copper; R76 and C48 sit next to U6.19.

| Item | Value |
|---|---|
| Conversion | V(`LED_K`) = 11.0 × V(pin) |
| In a pulse | The loop's 0.6–0.9 V band is 55–82 mV at the pin; ≤ 13.3 V (1.21 V) at duty 1 |
| Between pulses | `LED_K` sits at +32V minus the string's forward voltage at the divider's ≤ 0.31 mA, ≈ 12 × VF at µA currents: ≈ 6–12 V while the loop runs, ≈ 16–21 V at full scale, taking VF ≈ 1.1–1.3 V per LED (an inference: the Amicc datasheet gives VF only at 1 A; first-board check 7). The bound is +32V itself: ≤ 33.93 V gives ≤ 3.08 V at the pin nominal, ≤ 3.14 V with R70 −1 % and R76 +1 % (33.93 V × 10.1 / 109.1), inside ATTEN3's 0–3300 mV (P4 DS Table 5-6 p.90) |
| Error | ±15 mV at the ADC × 11 = ±0.165 V, plus ±2 % of the reading from the two 1 % resistors: ≈ ±0.18 V at 0.6 V; ±0.43 V at 13 V. Espressif specifies the ±15 mV with 100 nF at the pin (P4 DS p.90); with 1 nF and a 9.09 kΩ source the sampling error is unverified, so first-board check 7 measures it |
| Source impedance, settling | 100 kΩ ∥ 10 kΩ = 9.09 kΩ with C48: τ = 9.1 µs. From the off level (≤ 3.14 V) to within 1 mV takes 9.1 µs × ln 3140 ≈ 73 µs, so a sample ≥ 0.1 ms into the pulse has settled. The droop slews `LED_K` by ≤ 1.74 V/ms; the pin lags that by ≈ 16 mV at `LED_K` |
| R70 | ≤ 30.9 V across it and ≤ 9.5 mW (33.93 V over 110 kΩ), against 50 V and 1/16 W for an 0402 (UNI-ROYAL chip-resistor datasheet p.4, p.5) |
| String bias | Between pulses the divider draws ≤ 0.31 mA (33.93 V / 110 kΩ) through the ring: ≈ 0.04 % of the 831 mA pulse, invisible at 850 nm and negligible against the markers |
| Unpowered P4 | After a power loss C46/C47 may still hold charge: the pin then sees ≤ 3.14 V through 9.09 kΩ, ≤ 0.34 mA (33.93 V / 99 kΩ) into the unpowered pad. The P4 datasheet gives no injection limit ([Accepted risks](#accepted-risks)) |

C48 is 1 nF, not the 0.1 µF that Espressif's HDG §1.3.10 suggests at an ADC pin: with 0.1 µF the
time constant would be 0.9 ms, longer than the pulse.

## STROBE gating and timeout

### Gate

U9 (SN74LVC1G97, TI SCES416N) is a configurable gate with Schmitt-trigger inputs. With In1 tied
to GND it works as an AND gate (Table 1, p.8): In2 low gives Y = In1 = low, and In2 high gives
Y = In0. So:

`LED_ON = STROBE_TO ∧ LED_EN`

| Input | Net | Source | Default |
|---|---|---|---|
| In0 (U9.3) | `LED_EN` | GPIO11 (U6.13), TP11 | R56 100 kΩ to GND: off from reset |
| In2 (U9.6) | `STROBE_TO` | The camera STROBE through the timeout network below | – |
| In1 (U9.1) | GND | – | – |
| Y (U9.4) | `LED_ON` | Drives the R60/R61 setpoint, TP12 | – |

U9 runs from +3V3 with C37 100 nF. Its inputs come only from +3V3-powered parts (U6 and the STROBE
buffer U7 on the Camera sheet), so nothing back-powers it.

STROBE is the sensor's exposure output, buffered by U7. The P4 also captures it on GPIO12 (U6.14)
([camera](camera.md#strobe)).

### Timeout

A one-shot high-pass network makes the LEDs go dark even if STROBE sticks high or a wrong sensor
setting gives a long exposure:

1. STROBE drives R54 1.00 kΩ into C33 10 nF C0G ±5 % (Murata GRM1885C1H103JA01D).
2. R55 68.0 kΩ pulls the far side, `STROBE_HP`, to GND.
3. R58 10.0 kΩ feeds `STROBE_HP` to U9's In2.

A rising STROBE edge lifts `STROBE_HP` to V0 = VOH × R55/(R54 + R55). It then decays with
τ = C33 (R54 + R55) = 0.69 ms. When it falls below U9's VT−, `LED_ON` drops. The LED pulse
therefore ends at the earlier of STROBE falling and

$$t = \tau \cdot \ln\frac{V_0}{V_{T-}}$$

U7 (the STROBE buffer) and U9 share +3V3, so VOH and the thresholds move together. VT− is
0.84–1.19 V at VCC 3 V and 1.41–1.9 V at 4.5 V (SCES416N p.5). Between the rows it is interpolated
linearly: VT− max is 1.284 / 1.332 / 1.398 V at 3.199 / 3.3 / 3.44 V.

| Case | Inputs | Timeout |
|---|---|---|
| Shortest, design bound | Generic ±5 % envelope 3.135–3.465 V, named as such: V0 from 3.135 V − 0.1 V VOH, VT− max 1.332 V (3.3 V); C −5 %, R −1 % | 0.525 ms |
| Shortest, set-point range | +3V3 3.199–3.440 V on the shared rail: V0 from VCC − 0.1 V, VT− max at the same VCC; C −5 %, R −1 % | 0.555 ms |
| Nominal | 3.318 V, C and R nominal, VT− 1.09 V (mid-range) | ≈ 0.76 ms |
| Longest, set-point range | 3.440 V, VT− 0.84 V (3 V row minimum); C +5 %, R +1 % | 1.021 ms |
| Longest, design bound | Envelope 3.465 V, VT− 0.84 V; C +5 %, R +1 % | 1.026 ms |

The published window is the design bound, **0.525–1.026 ms**. The DC set-point range narrows it to
0.555–1.021 ms.

The user's answer was "≤ 0.5 ms at ≤ 120 fps (6 %)" (user decision 2026-10-31). The option they
chose said the timeout cuts above ≈ 0.6 ms. The circuit can't cut that close: U9's threshold spread
sets the window. The user accepted the window (user decision 2026-11-02: "Accept 0.525–1.026 ms"):

- the RC/U9 timeout stays a backstop;
- firmware limits pulses to 0.5 ms;
- a firmware fault could run the LEDs up to 1.026 ms per frame, ≈ 12 % duty at 120 fps, before the
  cut.

- **Passing 0.5 ms:** even the shortest unit passes a 0.5 ms pulse, with 5 % margin at the design
  bound and 11 % on the set-point range.
- **Why the cut can't be tighter:** U9's own VT− spread (0.84–1.33 V) sets the ≈ 2:1 width of the
  window, so no RC value cuts closer above 0.5 ms.

Further checks:

- **Every edge switches:** VT+ max interpolated at 3.44 V is 2.13 V (2.14 V at the envelope's
  3.465 V), below the minimum V0 of 2.99 V at the envelope (3.05 V on the set-point range).
- **Recovery:** the 8.33 ms frame period is ≈ 12 τ, so the network has fully recovered before
  the next frame.
- **Negative swing:** when STROBE falls after a timed-out pulse, `STROBE_HP` swings to about
  −V0 ≈ −3.4 V. R58 limits U9's input clamp current to ≈ 0.3 mA, against the −50 mA IIK rating.
  The datasheet allows a negative input while the clamp current is respected (p.4 note 2).
- **Edge current:** R54 limits the edge current from the STROBE buffer to ≤ 3.4 mA.

The timeout is a backstop. Firmware keeps the exposure, and so the LED pulse, at ≤ 0.5 ms per
frame at ≤ 120 fps ([firmware rules](firmware-rules.md#rules)). A fault that only the hardware
stops can still give pulses up to 1.026 ms, ≈ 12 % duty at 120 fps.

## LED ring

D4–D15 are 12 × **Amicc A-SC880IR3C-P03-1T** (LCSC C5355470): 850 nm centroid (855 nm peak),
90° viewing angle, 285–450 mW/sr at 1 A (Amicc ADE-880-017 p.3).

They form one series string: D4's anode is on +32V, `RING_1`…`RING_11` join the LEDs, and D15's
cathode is `LED_K` at Q1's drain. One string means one current for all twelve LEDs, no ballast
resistors and one sink. The LEDs are never reverse-biased.

The footprint is the project library part `LED_SMD_Amicc:LED_Amicc_A-SC880IR3C_3.85x3.85mm`: pad 1
is the cathode, and pad 2 (centre and outer pad) is the anode, as Amicc p.6 shows.

**Placement.** The LEDs sit at 30° pitch on an 11.5 mm radius around the lens axis
(129.7, 130.3), outside the Ø16 mm lens cutout. They are locked in place. The string opens between
D4 and D15 on the east side, where +32V comes in and `LED_K` leaves for Q1 next to them.

The LEDs are 2.55 mm tall. The lens view cone needs them ≥ 7.0 + 0.83 × 2.55 = 9.12 mm from the
axis, and the nearest courtyard is at 9.43 mm.

**Copper.** The ring links are 0.5 mm: a 1.0 mm track would break the 0.4 mm spacing to the LED's
other pad. They rise ≈ 4.2 °C at 0.915 A as DC (IPC-2152 fit).

Exposed copper on the LED nets keeps 0.4 mm to other nets, the IPC-2221A Table 6-1 A6 figure for
uncoated terminations at 31–50 V (rules "LED_PWR pad spacing" and "LED_PWR pad spacing (other
pads)", [layout](layout.md#custom-rules)).

### String voltage vs +32V

VF is 1.7 / 1.95 / 2.3 V min/typ/max at 1 A, with a ±0.1 V test tolerance (Amicc p.3). The string
therefore drops 20.4–27.6 V (23.4 V typical), up to 28.8 V with the tolerance.

| Needed at the top of the string | Value |
|---|---|
| String, VF max + test tolerance (12 × 2.4 V) | 28.8 V |
| R69 at 0.915 A | 0.09 V |
| Q1 RDS(on), 0.080 Ω at VGS 5 V × 1.5 hot (IRLR024N p.2) | 0.11 V |
| +32V copper (bound over D3 → D4; ≈ 0.02 V from C46, which supplies the pulse) | ≈ 0.1 V |
| Droop over 0.5 ms from 528 µF | 0.87 V |
| ESR step | 0.055 V |
| **Total** | **≈ 30.0 V** |

Against the minimum +32V of 31.30 V that leaves ≥ 1.3 V of margin, so the sink stays in
regulation through the whole 0.5 ms pulse on every unit.

The other side of the range is Q1's job: at full scale (duty 1), with the highest +32V and the
lowest-VF LEDs, Q1 drops up to 13.3 V ([Thermal](#thermal)). The headroom loop removes most of
that by lowering +32V to the string ([Headroom control](#headroom-control)).

## Thermal

### Q1 dissipation

Q1 takes up whatever voltage the string doesn't use. With the [headroom loop](#headroom-control)
running, that is the loop's band at the end of the pulse plus the bank's droop over it. At full
scale (duty 1: the first frames after start-up, and the fallback when the loop or its reading
fails, FW-22) it is everything between +32V and the string.

| Case | VDS over the pulse (average; the duty-1 rows use the start of the pulse, an upper bound) | Pulse power | Average at 6 % |
|---|---|---|---|
| Loop, typical (0.831 A, 662 µF: droop 0.63 V) | 0.6–0.9 V at the end + 0.31 V ≈ 0.9–1.2 V | ≈ 0.75–1.0 W | ≈ 0.045–0.060 W |
| Loop, worst (0.915 A, 528 µF: droop 0.87 V; reading 0.18 V low at the band's top) | ≤ 1.08 V at the end + 0.44 V ≈ 1.52 V | ≈ 1.4 W | **≈ 0.083 W** |
| Duty 1, typical (32.57 V, VF 1.95 V, 0.831 A) | ≈ 9.0 V | ≈ 7.5 W | ≈ 0.45 W |
| Duty 1, worst (33.93 V, VF 1.7 V, 0.915 A) | 33.93 − 20.4 − 0.09 − 0.1 ≈ 13.3 V | ≈ 12.2 W | **0.73 W** |

IRLR024N (p.1) gives RθJC 3.3 °C/W, RθJA 110 °C/W on the minimum pad and 50 °C/W on 1 in² of
copper, and TJ max 175 °C. With the loop, TJ ≤ 30 + 0.083 × 110 ≈ 39 °C at the 30 °C ambient
maximum (≈ 37 °C typical). The drain copper is still sized for duty 1, because the loop is firmware.

The board gives Q1 ≈ 100–120 mm² of drain copper:

- the 6.4 × 5.8 mm tab,
- a little F.Cu beside it,
- 16 thermal vias 0.6/0.3 (4 × 4) in the tab,
- the B.Cu zone "Q1 drain B.Cu" (x 144.8–151.0, y 126.7–137.65, 63.7 mm² filled, pads solid).

That is less than 1 in² (645 mm²). The datasheet gives nothing between the minimum pad and 1 in²,
so the bound at duty 1 uses the minimum-pad figure:

- **Junction:** TJ ≤ 30 + 0.73 × 110 ≈ 110 °C.
- **Tab:** ≤ 110 − 0.73 × 3.3 ≈ 108 °C.

Both are well inside 175 °C. The layout check requires ≥ 100 mm² and ≥ 12 vias on this net.

More copper doesn't fit. The drain copper keeps clear of the crystal, the RMII and CSI B.Cu
routes, U6's courtyard and the ring. Q1 can't move either: the top side has no free
7.5 × 11.5 mm area anywhere (Q1's courtyard is 7.0 × 11.1 mm).

### Neighbours

Edge-to-edge distances from Q1's tab:

| Part | Distance | Concern |
|---|---|---|
| R70 (`LED_K_SENSE` divider) | Its `LED_K` pad joins the tab copper | – |
| C8 | 1.27 mm | – |
| C2 (crystal load cap) | 1.42 mm | – |
| D15 (pad 1) | 1.74 mm | LED heating |
| D4 (pad 2) | 1.79 mm | LED heating |
| Y1 (25 MHz crystal) | 2.35 mm (1.35 mm from the B.Cu zone) | Rated −40…+85 °C; ±20 ppm specified only over that range (Epson TSX-3225 p.1) |
| C46 | 7.27 mm | Rated 105 °C |
| C47 | 22.3 mm | Rated 105 °C |

With the loop running Q1 dissipates ≤ 0.083 W, ≈ 9 °C above the board on the minimum-pad figure.
The ≈ 108 °C tab next to D15, D4, Y1 and C46 remains possible at duty 1 only. No datasheet figure
gives their temperature in that case. Even a few degrees from a tab 1.7 mm away may use up the
1.35 °C LED allowance of the [current setpoint](#led-current).

The LEDs also heat themselves. Each dissipates up to 0.915 A × 2.3 V × 6 % ≈ 0.126 W (≈ 1.5 W for
the ring) through Rs 18 °C/W junction to case. Amicc rates them at a 25 °C solder point and tests
on a 1 × 1 cm metal-core board. Here each LED has only its pads and 0.5 mm links on FR-4.

The user chose to measure this on the first boards rather than lower the current or move Q1 now
(user decision 2026-11-01): see [First-board checks](#first-board-checks).

### Q1 protection

The board has no temperature sensor: the NTC that sat at Q1's tab gave way to the `LED_K_SENSE`
divider (user decision 2026-11-03), whose reading bounds Q1's dissipation directly. Firmware keeps
(V(`LED_K`) at the end + 0.44 V) × 0.915 A × LED duty ≤ 0.80 W and reports a reading or string
fault (FW-22). The pulse average is the end reading plus half the worst droop (0.87 V / 2), as in
the [table](#q1-dissipation). At duty 1 the end reading is ≤ 13.3 V − 0.575 V (the smallest droop,
0.915 A into 662 µF + 20 %) + 0.43 V reading error ≈ 13.2 V, so the rule computes ≤ 0.75 W on any
unit in tolerance and doesn't trip; 0.80 W holds TJ ≤ 30 + 0.80 × 110 = 118 °C. In the band it
computes 0.057–0.074 W.

A pulse longer than the set exposure, up to the hardware timeout's 1.026 ms (≈ 12.3 % duty,
[Timeout](#timeout)), averages over the pulse ≈ (13.3 − 1.78 / 2) V × 0.915 A × 12.3 % ≈ 1.40 W
at duty 1 (droop 1.78 V into 528 µF): a TJ of ≈ 184 °C on the minimum-pad figure, above the
175 °C rating; with the loop still running, ≲ 0.17 W. The power rule above sees it only in its
average, so FW-9 adds a pulse-width fault: firmware times every STROBE pulse and drops `LED_EN`
before the next frame when one exceeds 0.55 ms. That leaves single pulses, which the average
doesn't feel; the hardware timeout stays the backstop if the firmware itself fails
([Accepted risks](#accepted-risks)).

## Accepted risks

- **LED, C46 and Y1 temperature next to Q1 at duty 1 is unknown.** With the headroom loop Q1
  dissipates ≤ 0.083 W; at duty 1 (the first frames, the fallback) up to 0.73 W with ≈ 100–120 mm²
  of drain copper, 1.74 mm from D15 and 1.79 mm from D4, against a 1.35 °C LED allowance. Accepted
  by measuring the first boards instead of lowering the current or re-floorplanning now (user
  decision 2026-11-01).
- **Headroom control is firmware.** Until the loop runs, and whenever it falls back, +32V sits at
  full scale and Q1 dissipates as before, ≤ 0.73 W (user decision 2026-11-03, design option 1b).
- **A firmware failure at duty 1 can overheat Q1.** If firmware both leaves +32V at full scale and
  lets STROBE run past 0.5 ms, only the hardware timeout ends each pulse, at up to 1.026 ms:
  ≈ 1.40 W and TJ ≈ 184 °C, above Q1's 175 °C. FW-9's pulse-width fault and FW-22's power rule
  prevent it; no hardware bound does (design review 2026-11-03, [Q1 protection](#q1-protection)).
- **No temperature sensor.** TH1 was dropped to reuse its route for `LED_K_SENSE` (user decision
  2026-11-03); firmware bounds Q1's power from the drain voltage instead
  ([Q1 protection](#q1-protection)).
- **C48 is 1 nF, not the 0.1 µF HDG §1.3.10 suggests at an ADC pin,** because 0.1 µF would not
  settle within the 0.5 ms pulse ([LED_K sense](#led_k-sense)).
- **`LED_K_SENSE` can drive ≤ 0.34 mA into an unpowered P4 pin** while C46/C47 hold charge after a
  power loss; the P4 datasheet gives no injection limit.
- **LED current set to the DC curve at 30 °C with only ≈ 18 mA (1.35 °C) margin.** R61 1.40 kΩ
  (user decision 2026-10-31, stay on the DC curve). Any LED heating above ambient needs a lower R61
  ([first-board checks](#first-board-checks)).
- **No hardware gate on USB power.** The LEDs stay off on USB alone only because firmware keeps
  `LED_PWR_EN` low while `PWR_SRC` is low (user decision 2026-10-31).
- **The hardware timeout cuts only within 0.525–1.026 ms.** The ≤ 0.5 ms limit is a firmware rule.
  U9's threshold spread sets the window, so it can't cut closer. A firmware fault could run the
  LEDs up to 1.026 ms per frame, ≈ 12 % duty at 120 fps (user decision 2026-11-02: "Accept
  0.525–1.026 ms").
- **Boost output capacitance (662 µF) is far outside TI's 1–10 µF recommendation.** The
  compensation is designed for it (estimate ≈ 450 Hz crossover, ≈ 67° phase margin), but TI asks
  for such values to be tested. Accepted in design review (2026-11-01), tested on the first boards.
- **Current-loop phase margin is an estimate.** Q1's gm at 0.9 A is not in the datasheet. Accepted
  in design review (2026-11-01), tested on the first boards.
- **No GND guard between the crystal (Y1, C2) and the LED copper.** The +32V track runs 0.45 mm from
  C2.1, and the Q1 drain zone overlaps C2.1 in plan view on B.Cu, with both inner GND planes in
  between. A guard track doesn't fit, and IP101G §6.1 p.56 asks for guards only "if possible".
  Accepted in design review, 2026-11-01 ([layout](layout.md#led-driver-and-ring)).
- **Small parts beside C46 and Q1.** R5 and C8 sit beside C46 (10.8 mm tall), courtyards 0.01 /
  0.00 mm apart (pads 4.45 / 4.40 mm); R67, C33 and R68 beside Q1's TO-252, pads 1.00 / 1.19 /
  1.10 mm from Q1.1 (courtyards 0.08 / 0.31 / 0.33 mm); R70 beside Q1's tab, courtyards 0.02 mm,
  R70.1–Q1.2 0.42 mm (no other site keeps R70.1's short link to the tab): possible reflow shadowing;
  no source gives a number ([layout](layout.md#assembly), [first-board check 8](#first-board-checks)).

## First-board checks

1. **Thermal test** (user decision 2026-11-01). Run the LEDs at the worst duty, 0.5 ms pulses at
   120 fps, for 30 min, on PoE, twice: with the headroom loop running, and with `BST_EN` held at
   duty 1 (the fallback). Record the ambient temperature.
   - **Thermocouples:** on the cases of D15 and D4 (nearest Q1), on C46 and on Y1's case. Add Q1's
     tab and an LED far from Q1 (D9 or D10) to separate self-heating from Q1 heat.
   - **Loop:** log the duty and `LED_K_SENSE` during the first run.
   - **LED limit:** take the hottest LED case temperature T as the curve's temperature, which is
     conservative. The worst-case current must stay ≤ 1000 mA − 13.3 mA/°C × (T − 25 °C). If it
     doesn't, lower R61 (table below).
   - **Other parts:** C46 must stay well under 105 °C. Y1 must stay ≤ 80 °C (5 °C under its 85 °C
     rating); if it doesn't, lower the LED current (R61) or the duty, or pull the B.Cu zone "Q1 drain
     B.Cu"'s east edge back from x 151.0 ([layout](layout.md#thermal)).
2. **LED current.** Measure the voltage across R69 at TP14 (0.1 V/A) during a pulse: expect
   ≈ 83 mV (0.831 A) at nominal rails, flat over 0.5 ms. Measure it on the unit with the highest
   +3V3 together with the thermal test, because a ground offset between R69.2 and U11's reference
   shifts the current by ≈ 10 mA per mV.
3. **Current-loop response.** Scope `ISNS` (TP14) against `LED_ON` (TP12) at both edges. Expect
   < 10 % overshoot, no ringing and a µs-scale rise.
4. **Boost stability.** Scope +32V (TP13) and `BST_SW` (at L3.2/D3.2) at start-up and during
   0.5 ms / 120 fps pulses, at duty 1 and at every set point down to the loop's lowest (≈ 0.67 on
   a low-VF unit), including the loop's duty steps. Expect:
   - no ringing or sub-harmonic switching,
   - +32V within 31.30–33.93 V at duty 1 and within 0.951–1.052 × D × 32.57 V below it (±3.05 %
     reference, ±1.9 % divider, +0.2 % on-time),
   - no overshoot above ≈ 34 V,
   - recovery before the next frame (droop ≤ ≈ 0.9 V, back within ≈ 2.4–6.0 ms at duty 1);
   - a start-up time in the 45–170 ms range (≈ 80 ms nominal), with no EasyScale entry or
     shutdown when the 50 kHz PWM follows the soft start.
   Compare the `VLED_SENSE` reading × 26.5 with TP13. Scope `+5V_BST` during the recovery to see
   whether the boost runs at its current limit there, as [estimated](#recovery-and-start-up).
5. **Timeout.** With `LED_EN` high:
   - a 0.5 ms STROBE must give a whole 0.5 ms `LED_ON` pulse;
   - a STROBE held high (a long exposure) must end `LED_ON` within 0.525–1.026 ms;
   - `LED_ON` must follow the next normal frame unchanged;
   - STROBE's width must match the configured exposure within 50 µs over the exposure range
     (FW-9's 0.55 ms pulse-width fault assumes it), and a 0.6 ms STROBE must make firmware drop
     `LED_EN` before the next frame.
6. **Default-off and USB.** At reset and in download mode, nothing drives the three enables:
   `LED_PWR_EN` (R57), `BST_EN` (R59) and `LED_EN` (R56); R68 also holds Q1's gate low. Then +32V
   must stay unpowered (U8 off) and `LED_ON` low. On USB power alone, firmware must hold
   `LED_PWR_EN` low: check that +32V stays at 0 V. Pulling PoE must drop `LED_EN`, `BST_EN` and
   `LED_PWR_EN` in that order.
7. **Headroom loop.** With probes on Q1's drain (R70.1) and on `ISNS` (TP14), on one unit unless
   a step names two:
   - `LED_K_SENSE` × 11 must match the drain at the end of a pulse within ±0.2 V;
   - step the duty down until the current at the end of the pulse starts to sag, and record
     V(`LED_K`) there (expected ≈ 0.1–0.3 V): the band's 0.6 V lower edge must stay ≥ 0.3 V above
     it;
   - repeat check 3 at the band's lower edge, where Q1 runs nearest its linear region;
   - on a low-VF and a high-VF unit, at the lowest set point and at duty 1, log the in-pulse and
     the between-pulse `LED_K_SENSE` readings of each frame and their difference; then hold Q1
     off (`LED_EN` low with STROBE running, or Q1's gate shorted) and log the same pair. Set
     FW-22's sink-off threshold (1.5 V, a first value) between the two differences, and record
     VF per LED at the divider current;
   - from power-up, log the coarse step and the settling: V(`LED_K`) right after the coarse step
     within 0.77–2.8 V, inside the band within ≈ 5 s;
   - time the `LED_K_SENSE` one-shot conversion: it must fit the last 0.05 ms of a
     pulse.
8. **Joints beside big parts:** AOI or a visual check of R5 and C8 (beside C46) and R67, C33, R68
   and R70 (beside Q1) for wetting, tombstoning and shifted parts.

If the thermal test fails, pick R61 from this table. Worst case uses the same inputs as
[LED current](#led-current), with 1 % E96 values. The table gives the highest temperature at which
each value stays on the DC curve. Radiant output drops roughly in proportion to the current.

| R61 | Nominal | Worst case | On the curve up to |
|---|---|---|---|
| 1.40 kΩ (fitted) | 0.831 A | 0.759–0.915 A | 31.3 °C |
| 1.33 kΩ | 0.788 A | 0.718–0.869 A | 34.8 °C |
| 1.27 kΩ | 0.751 A | 0.684–0.830 A | 37.8 °C |
| 1.21 kΩ | 0.714 A | 0.649–0.790 A | 40.7 °C |
| 1.15 kΩ | 0.677 A | 0.614–0.750 A | 43.7 °C |
| 1.10 kΩ | 0.646 A | 0.585–0.717 A | 46.2 °C |
| 1.05 kΩ | 0.615 A | 0.555–0.684 A | 48.7 °C |
| 1.00 kΩ | 0.584 A | 0.526–0.651 A | 51.2 °C |
