# LED driver and ring

The 850 nm ring lights the markers only while the camera exposes. The chain:
`LED_PWR_EN` connects +5V to the boost through a load switch, the boost makes +32V, the string of
12 LEDs runs from +32V into a linear current sink, and the sink's setpoint is on only while the
sensor's STROBE and the P4's `LED_EN` are both high and the STROBE pulse is younger than a
hardware timeout. Firmware runs the boost just above what the string needs, by PWM on `BST_EN`
and a reading of the sink's drain (`LED_K_SENSE`), so the sink drops only the headroom it needs
instead of everything between +32V and the string ([Headroom control](#headroom-control)).
Sheets: *LED driver* and *LED ring*. Every passive value, result and placement figure is in the
KiCad files; this page keeps the requirements, the formulas with their datasheet constants, the
decisions, the accepted risks and the checks.

| Block | Parts | Control (U6 pin) | Test point |
|---|---|---|---|
| [LED power switch](#led-power-switch) | U8 TPS22918, C34, C35, R57 | `LED_PWR_EN` GPIO20 (U6.16) | – |
| [Boost converter](#boost-converter) | U10 TPS61170, L3, D3, C36, C38, C39, C41, C43, C45, C46, C47, R59, R62, R65, R66 | `BST_EN` GPIO53 (U6.71), PWM | TP13 `+32V` |
| [Current sink](#current-sink) | U11 TLV9061, Q1 HL2310A, R69, NT1, R60, R61, R63, R64, R67, R68, C40, C42, C44 | – | TP12 `LED_ON`, TP14 `ISNS` |
| [STROBE gating and timeout](#strobe-gating-and-timeout) | U9 SN74LVC1G97, C33, R54, R55, R56, R58, C37 | `LED_EN` GPIO11 (U6.13), STROBE GPIO12 (U6.14) | TP11 `LED_EN`, TP9 `STB` |
| [LED ring](#led-ring) | D4–D15 A-SC880IR3C | – | – |
| Sense | R71, R72, C49 (`VLED_SENSE`); R70, R76, C48 (`LED_K_SENSE`) | GPIO21 ADC1_CH5 (U6.17), GPIO23 ADC1_CH7 (U6.19) | – |

The product limits that set every number below:

- **Pulse:** ≤ 0.5 ms per frame at ≤ 120 fps, so the LED duty is t_pulse × fps (user decision
  2026-10-31: "≤ 0.5 ms at ≤ 120 fps (6 %)").
- **Ambient:** 15–30 °C, indoor (user decision 2026-10-31).
- **LED current:** on the Amicc DC derating curve at the ambient maximum (user decision
  2026-10-31, [Current sink](#current-sink)).
- **USB power alone:** the LEDs stay off by a firmware rule, with no extra hardware gate
  (user decision 2026-10-31).

Firmware rules for this block are in [firmware-rules.md](firmware-rules.md#rules). Layout details
are in [layout.md](layout.md#led-driver-and-ring).

## LED power switch

U8 (TPS22918, TI SLVSD76C) connects +5V to `+5V_BST`, the boost input. The boost needs this
switch because a boost converter has a DC path from input to output through L3 and D3 even while
it is not switching. Without U8, +32V would sit at +5V minus a diode drop whenever the board
is powered. U8 is the one hardware switch that removes all power from the LED side. The schematic
places it with KiCad's TPS22917DBV symbol: the Value and Description are the TPS22918, which
has the same DBV pinout (SLVSD76C p.3).

| Item | Requirement / formula | Reason / source |
|---|---|---|
| ON | `LED_PWR_EN`, GPIO20 (U6.16), R57 pull-down | ON must not float (SLVSD76C p.3). The pull-down keeps the switch off from reset until firmware drives the pin, below ON VIL 0.5 V (p.4) |
| CT | C35 sets the slew rate: SR = 0.55 × C35 (pF) + 30 µs/V (SLVSD76C p.14 Eq. 3, Table 2), with C35 at its tolerance and the X7R temperature coefficient (±15 %, FH MLCC datasheet p.5); TI measures Table 2 with an X7R CT (p.14), and CT must be rated ≥ 20 V at VIN 4–5.5 V (Table 3 p.15). `+5V_BST` rise time = SR × VIN. Inrush = C_out × VIN / t_rise (p.19 Eq. 9), with C_out the capacitance behind U8: C36, then C43/C45/C46/C47 through L3 and D3 | The inrush stays well under U8's 2 A rating, so the +5V rail sees no step |
| QOD | Pin 5 open (no-connect flag) | Quick output discharge disabled, an allowed option (p.3, p.13) |
| Input | C34 at U8.1, CIN ≥ 1 µF (p.4, p.17) | The +5V bulk (C20, C22) is larger than `+5V_BST`'s C36, as p.17 advises |
| Load | Boost input current at its current limit: ILIM max (TPS61170, SLVS789D p.5) minus half the inductor ripple, peak = ILIM max | Below U8's 2 A rating at 70 °C (p.4); VIN on PoE stays ≤ U8's 5.5 V maximum |

The `+5V_BST` path leaves U8 at pin 6 on F.Cu to C36 and L3, necked at U10's pins
([layout](layout.md#led-driver-and-ring)).

### Boost input range

The LEDs run only on PoE (firmware rule), so the boost input range is the one on PoE:

| Rail | Formula |
|---|---|
| `+5V` on PoE | `+5V_POE_F` range ([Rails](README.md#rails)) minus the TPS2121 drop: RON ≤ 85 mΩ at 85 °C (TPS2121, SLVSEA3F p.7) × the board peak current ([power mux](power.md#power-mux)) |
| `+5V_BST` | `+5V` minus U8's drop: RON ≤ 79 mΩ (SLVSD76C p.5) × the boost input current at its limit |

During a PoE → USB switchover `+5V` dips to the power mux's minimum at U4 OUT (a little less at
U8.1) for a moment ([power mux](power.md#power-mux)). The boost loses regulation then, and
firmware shuts the LEDs down on the same `PWR_SRC` edge.

### Enable order

The enable order is stated once, as **FW-7** in [firmware rules](firmware-rules.md#rules). This is
a summary of it. `PWR_SRC` comes from the [power mux](power.md#power-mux); low means USB.

**Power-up (on PoE only)**
1. `LED_PWR_EN` high.
2. Wait a fixed 60 ms for `+5V_BST` (user decision 2026-11-09, with the X7R C35). There is no
   `+5V_BST` sense; the full swing from the C35 formula above (SLVSD76C Eq. 3 p.14: the 10–90 %
   rise / 0.8 plus the turn-on delay, C35 at its high tolerance, `+5V` at its top) stays under
   60 ms with margin, typical figures only.
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
  in 32 steps of 213 µs (SLVS789D §8.3.1 p.10). Started together with U8, the ramp would be
  over while `+5V_BST` is still rising.

## Boost converter

U10 (TPS61170, TI SLVS789D) boosts `+5V_BST` to +32V for the LED string. It has an integrated
40 V switch, runs at 1.2 MHz (1.0–1.5 MHz) and limits the switch current to 0.96–1.44 A (p.5).

### Output voltage

R65 and R66 set the output: Vout = VREF × (1 + R65/R66) + IFB × R65 (SLVS789D p.17 Eq. 2; VREF
1.204 / 1.229 / 1.254 V min/typ/max and IFB 200 nA max, p.5). TI puts R66's optimum around 10 kΩ,
for divider leakage and FB noise (p.17); R66 sits above it to use Basic parts (user decision
2026-11-09), so the IFB × R65 term in the maximum is larger at the same ratio and the FB node has a
higher impedance. The minimum output takes VREF min, R65 low and R66 high; the maximum takes the
opposite corners, both within the resistors' tolerance. These are the values with `BST_EN` steady
high (PWM duty 1), the highest the output can be; in operation firmware runs it lower
([Headroom control](#headroom-control)).

Everything on +32V must be rated above the maximum output:

| Part | Rating that must hold |
|---|---|
| U10 SW | 40 V abs. max (p.3) against Vout max plus D3's drop |
| D3 | Reverse voltage above Vout max |
| Q1 HL2310A | 60 V VDS (HL2310A p.1) |
| C43, C45, C46, C47 | Rated voltage above Vout max with margin |
| R65, R71 (0603) | Working voltage above Vout max (UNI-ROYAL chip-resistor datasheet p.5) |

Full scale can't go lower: at duty 1 the end of a pulse leaves V(`LED_K`) ≥ Vout min minus the
[String voltage vs +32V](#string-voltage-vs-32v) terms (without Q1's RDS(on) term), on every unit.
That is above the headroom loop's band, so the loop always finds its band by lowering the duty.
Every volt above what the string needs is dissipated in Q1 ([Thermal](#thermal)).

### Headroom control

`BST_EN` carries a PWM signal (user decision 2026-11-03, design option 1b). In PWM mode the
TPS61170 chops its reference at the duty cycle D and filters it: V_FB = D × VREF (SLVS789D §8.4.2
p.11, Eq. 1). With R65/R66 unchanged, +32V = D × Vout(duty 1).

| Item | Requirement / formula | Reason / source |
|---|---|---|
| PWM frequency | 50 kHz | Inside TI's 5–100 kHz (p.11). The low phase (1 − D) / f_PWM stays far below the 260 µs EasyScale detection time and the 2.5 ms shutdown delay (p.5, p.10), so PWM never selects EasyScale or shuts the boost down. The 40 ns on-time error adds ≤ 40 ns × f_PWM of duty (p.11); the loop absorbs it |
| Reference in PWM mode | V(REF_PWM) 477–507 mV at a 492 mV setting (±3 %) | p.5. Open loop the set point is only that good; the loop measures the result instead |
| Duty range | D_min … 1. 1 is steady high. D_min is the firmware's duty clamp (≈ 0.62 at nominal values, as FW-8 and FW-22 use it), given by the criterion D_min × Vout(duty 1) nominal × (1 + reference tolerance) < the lowest string (12 × VF min, [LED ring](#led-ring)) plus the headroom the loop needs | The loop reaches every string; an ADC fault that drives D down only dims the LEDs |
| Reference ripple | ΔV_FB ≈ VREF × D(1 − D) × T_PWM / t_REF, first order through the internal filter (t_REF 180 µs, p.5; Fig. 9 p.11). The boost loop (crossover from [Compensation](#compensation)) passes ≈ f_c / f_PWM of it | Estimate of the ripple on +32V; it is small against the band below |

Firmware (FW-22 in [firmware rules](firmware-rules.md#rules)) reads V(`LED_K`) through
`LED_K_SENSE` ([LED_K sense](#led_k-sense)) late in each LED pulse and steps D so that the reading
at the end of the pulse stays in a band, whose edges are set in FW-22:

- **Lower edge.** At least Q1's minimum VDS to hold the current at VGS ≤ U11's swing:
  I_LED max × RDS(on) (≤ 125 mΩ at VGS 4.5 V, HL2310A p.2, × 1.5 hot, Fig. 4 p.3 at 100 °C), plus
  the reading error ([LED_K sense](#led_k-sense)) and a margin above that need. U11's swing must
  reach the 4.5 V of that figure: VGS = `+5V` on PoE minus U11's swing minus V(`ISNS`).
- **Upper edge.** The band is at least 7 duty steps wide (each step is ≤ 0.13 % of the PWM period
  times Vout(duty 1), FW-8), so the loop settles inside it instead of hunting between two steps.
- **Set points.** +32V = string + R69 × I_LED + copper + droop + ESR step + V(`LED_K`), from the
  lowest string (12 × VF min) to the highest (12 × VF max with the test tolerance, at I_LED max,
  the bank's lowest capacitance, the reading error at the band's top). The highest stays under
  the full-scale minimum.
- **Start-up.** The boost starts at duty 1 as before (FW-7, FW-8); the loop starts with the pulses.
  Only the pulses lower +32V (the boost can't sink current): each removes Q = I_LED × t_pulse
  from the bank, a step ΔV = Q / C_bank. Fine steps alone would need (number of steps) × (samples
  per step) lit frames, many seconds at 120 fps, so the loop takes one coarse step first: from the
  first averaged reading V at duty 1 it sets D = 1 − (V − V_T) / S_max, clamped to D_min … 1.
  V_T = 1.2 V is the coarse target, a firmware decision: the band's lower edge plus the reading
  error at the ≈ 13 V reading of duty 1 (±15 mV × divider ratio plus the resistor tolerances,
  [LED_K sense](#led_k-sense)) plus a margin. A target at the band's lower edge itself would land
  below the band on some units. S_max is the steepest +32V(D) on any unit: Vout nominal × (1 +
  reference tolerance + divider tolerance + on-time error), from the reference and error terms
  above. After the step V(`LED_K`) lands never below the band's lower edge and a bounded amount
  above its top, set by S_max and the reading error at that voltage. The bank then takes
  (V_after − V_top) / ΔV pulses to
  come down, and the fine steps finish. Q1 sees full-scale and near-full-scale figures in the
  start-up burst: the lit frames that make the first average plus that bleed-down after the coarse
  step (FW-22), then ≤ (V_after + droop / 2) × I_LED max × duty in the band. The burst is bounded
  in [Thermal](#q1-dissipation).
- **Fallback.** Without a valid reading the firmware turns the LEDs off (`LED_EN` low) and
  reports; it never holds D = 1 with the LEDs pulsing. The loop is a requirement, not an
  optimisation (user decision 2026-11-09): at full scale Q1's junction reaches its maximum
  ([Thermal](#thermal)).

### Power stage

| Part | Check (source) |
|---|---|
| L3 | Inside TI's 10–22 µH (SLVS789D p.4, p.18). Isat > ILIM max; Irms > the average input at the current limit; DCR from the Changjiang FNR4030S catalogue p.10. Ripple ΔIL / IIN at the nominal input and output inside TI's 30–40 % (SLVS789D p.17 Eq. 3, p.18) |
| D3 | Schottky; TI asks for a diode rated above the 40 V switch (p.18). Peak current vs IFSM 30 A, loss = VF × average current (R+O SS16 datasheet p.1 IFSM, p.2 VF) |
| C36, C38 | CI ≥ 1 µF is met (p.4). C36 is above TI's recommended 1–4.7 µF (p.19). That is harmless: more input capacitance only lowers the ripple on `+5V_BST`. Loop stability is set by the output capacitors and the COMP network, not by CI, and U8's slew rate limits C36's inrush. The 0805 X5R part loses capacitance with DC bias at 5 V (estimate) |
| C43, C45 | High-frequency output capacitors right at D3's cathode |

The duty cycle is D = 1 − Vin × η / Vout, with η ≈ 0.83 (p.7 Fig. 1). It must stay under the 90 %
minimum Dmax (p.5, p.18) at both corners: nominal, and the `+5V_BST` minimum on PoE with Vout max.

With losses (DCR of L3, RDS(on) 0.6 Ω max, the input current at the limit), regulation holds
down to the input given by the loss model. `+5V_BST` stays above that in steady state on PoE.
Only the switchover dip above goes lower.

### Output capability

Worst-case output current is Iout_max = Vin × (ILIM − IP/2) × η / Vout (SLVS789D p.17 Eq. 3/4).
The lower bound takes:

- Vin = the `+5V_BST` minimum on PoE,
- ILIM 0.96 A (minimum, specified at D = Dmax, p.5),
- L3 at its low tolerance,
- fs 1.0 MHz,
- Vout max, with D3's VF 0.7 V,
- η 0.80–0.84 (Fig. 1 p.7).

The upper bound takes Vin max, ILIM 1.44 A (maximum, p.5), L3 at its high tolerance, fs 1.5 MHz,
η 0.85, Vout min and D3's VF 0.7 V.

Typical delivery takes the nominal inputs, 1.2 MHz and η 0.83. Its ILIM is read from SLVS789D
Fig. 4 p.7 (switch current limit vs duty cycle) at the operating duty, not the 1.2 A typical of the
p.5 table.

Demand at the pulse limit is I_LED max × t_pulse × fps, plus the two dividers' current
(R65 + R66, R71 + R72), plus the `LED_K_SENSE` divider current through the string
([LED_K sense](#led_k-sense)). The lower delivery bound must exceed the worst demand; the ratio
is the margin.

These figures are at full scale (duty 1, Vout max). At the headroom loop's set points the stage
delivers proportionally more (Iout ∝ 1/Vout), so the margin only grows.

### Recovery and start-up

These are estimates; the [first-board checks](#first-board-checks) cover both.

**Per frame.** Each pulse takes Q = I_LED × t_pulse out of the bank. To correct the droop
(Q / C_bank) at the loop's crossover f_c, the loop would ask for about C × ΔV × 2π × f_c of
output, far above what the stage can deliver (Iout_max over the tolerances). So the error
amplifier saturates, and the boost runs at its switch current limit from the start of each pulse
until the charge is back.

That takes t_rec = Q / Iout_max (the lower and upper bounds above) of every frame (1 / fps). During
that time `+5V_BST` carries the current-limit input, so on PoE this peak recurs every frame
([power mux](power.md#power-mux)).

Averaged over a frame, energy sets the input: I_in = Vout × I_out,avg / (η × Vin), taken at the
nominal and at the worst-case corner (Vout max, worst demand, η 0.80, Vin min).

Both are at full scale. At the loop's set points the bank holds the same charge per pulse at a
lower voltage, so the recharge is shorter (Iout ∝ 1/Vout) and the input energy per frame falls
with Vout: these figures are upper bounds.

**Start-up.** When `BST_EN` goes high, the FB reference ramps in 32 steps of 213 µs, and the
switch current limit is half the normal value for the first 5 ms (SLVS789D p.10 §8.3.1, p.5). The
bank needs ½ × C_bank × Vout², far more than the stage delivers during the ramp, so +32V rises at
the current limit.

Stepping t ≈ ∫ C × V dV / (Vin × (ILIM − IP(V)/2) × η) up from +32V ≈ `+5V_BST` minus D3's drop
(with the half limit for 5 ms) gives:

| Case | Inputs |
|---|---|
| Nominal | Nominal C_bank, nominal Vin, ILIM typical (p.5), nominal L3, 1.2 MHz, η 0.83 |
| Range | C_bank at its capacitor tolerance, Vin over the PoE range, ILIM 0.96–1.44 A, L3 tolerance, fs 1.0–1.5 MHz, η 0.80–0.85 |

The nominal start-up uses the p.5 typical ILIM rather than Fig. 4's lower value. While +32V
rises the duty runs from near 0 up to the operating duty, and Fig. 4 shows a current limit near the
typical over the middle of that range.

Firmware waits for `VLED_SENSE` instead of a fixed delay.

### Output capacitors C46, C47

The bulk is two SMD aluminium electrolytics (ACMECON GVZ series, low impedance). C46 sits at the pulse loop, next
to Q1's drain copper. C47 sits at the boost end of the +32V trunk. Requirement: rated above Vout max with
margin, low ESR.

The capacitance comes from the pulse. With both at their capacitance tolerance, a pulse of
I_LED max × t_pulse drops the rail by I_LED max × t_pulse / C_bank, plus I_LED max × ESR (the
two ESRs in parallel). That fits the string headroom ([LED ring](#led-ring)).

Why this part:

- **Economic PCBA.** JLCPCB lists it as an SMT part (JLCPCB parts API, read 2026-11-09). The
  smaller part used before is "Standard only", which the Economic PCBA order can't place.
- **Cheapest in-stock candidate that meets the ESR step and ripple** (user decision 2026-11-09):
  its datasheet gives the impedance and ripple rating at 100 kHz (GVZ datasheet p.1);
  general-purpose parts give only tanδ at 120 Hz, which leaves the set-point margin and the ESR
  zero unproven. The bank carries ≈ I_LED max × √duty rms in total, against the per-capacitor
  rating at 100 kHz × the 120 Hz factor (0.65).
- **Temperature and voltage.** Rated −55…+105 °C (GVZ datasheet p.1), and the rated voltage is above
  Vout max with margin.
- **Stock.** In stock on 2026-11-09.
- **One part with C14.** The PoE input filter's C14 uses the same part, so both share one BOM line
  (user decision 2026-11-09; [power](power.md#output-and-filter)).

The can height is the datasheet length plus tolerance plus seat plate. The can must stay outside
the cone of the `layout.heightZones` entry "lens view cone" in `.omp/kicad.json`, which takes the
part height h from the datasheet; check with `kicad_query distance`. The footprint is KiCad's
`CP_Elec_10x10`. ACMECON gives no land pattern; its terminals (P, C, W, GVZ datasheet p.1) land
fully on the pads.

### Compensation

R62 + C41 put a zero at 1 / (2π × R62 × C41), and C39 puts a pole at about 1 / (2π × R62 × C39),
all on COMP. C41 counts with its tolerance and the X7R temperature coefficient (±15 %, FH MLCC
datasheet p.5), which spreads the zero by about ±25 %. With TI's small-signal model (SLVS789D
Eq. 7–11, p.19: Gea 320 µS, Rsense 0.1 Ω; inputs Vin, Vout, and Cout = C43 + C45 + C46 + C47 as
built), the loop crosses over at a few hundred Hz with a comfortable phase margin over the whole
zero spread. The right-half-plane zero and the ESR zero of C46 ∥ C47 (1 / (2π × ESR × C)) both
sit above the crossover.

At the headroom loop's set points the crossover rises with (1 − D_sw) = Vin × η / Vout. TI's model
(Eq. 7–11) puts it higher at lower Vout, with the phase margin a few degrees higher, because the
zero adds more phase there and the right-half-plane zero moves up. Still an estimate on the same
terms: [first-board check](#first-board-checks).

TI's recommended output capacitance is 1–10 µF, and other values "should be fully tested"
(p.4 note 1, p.19). Cout here is far outside that range, so stability is a
[first-board check](#first-board-checks).

### BST_EN

`BST_EN` is GPIO53 (U6.71) on U10's CTRL pin. R59 pulls it down, together with U10's
internal ≈ 800 kΩ (p.5, p.10), so the boost stays off from reset until firmware drives the pin
(below CTRL VIL 0.4 V).

CTRL picks one of two ways to program the feedback reference at each start from shutdown (p.10):
PWM, the default, or TI's EasyScale. This board uses PWM ([Headroom control](#headroom-control)):

- **Start:** drive `BST_EN` steady high (duty 1). Never hold it low for ≥ 260 µs within 1 ms of a
  start from shutdown: that selects EasyScale mode, which drops the FB reference to 200 mV, so
  +32V would fall to a few volts and the LEDs would stay dark (p.10–11).
- **Run:** 50 kHz PWM, duty D_min (≈ 0.62 at nominal values) … 1, set by the headroom loop. Its low phases last (1 − D) / f_PWM.
- **Shutdown:** hold it low for ≥ 2.5 ms (p.5).

The enable order with `LED_PWR_EN` and `LED_EN` is under
[Enable order](#enable-order).

After `BST_EN` goes low, D3 blocks back-feed and C46/C47 stay charged. They bleed through the two
dividers, (R65 + R66) ∥ (R71 + R72) (τ = C_bank × R), and through the string into R70/R76
([LED_K sense](#led_k-sense)), so +32V falls from Vout by t = τ × ln(V0 / V), and the ring can
still conduct until it falls below the lowest string voltage (12 × VF min). During that time
`LED_EN` low (with R56, and R68 at Q1's gate) keeps the ring dark apart from the divider bias.

### Output voltage sense

R71 and R72 divide +32V into `VLED_SENSE`, which goes to GPIO21 = ADC1_CH5 (U6.17). R71 is a 0603
for its working voltage. C49 sits at the pin (Espressif ESP32-P4 HDG §1.3.10).

| Item | Formula |
|---|---|
| Conversion | V(+32V) = V_ADC × (R71 + R72) / R72 |
| Readings | V(+32V) × R72 / (R71 + R72) over the output range; below the ADC's ATTEN3 range with margin |
| Source impedance | R71 ∥ R72 with C49: τ = R × C49, longer than an LED pulse, so it reads the DC rail |
| Error | From the resistor tolerances |

Firmware can read it to confirm the boost is up before enabling the LEDs, and to watch the rail.

The two dividers (R65/R66 and R71/R72) draw V(+32V) / (R65 + R66) + V(+32V) / (R71 + R72) from
+32V; the `LED_K_SENSE` divider adds its current through the string.

### Layout essentials

TI §11.1 p.23 asks for a short switch node. The `near` entries L3.2 → U10.4 and D3.2 → U10.4 in
`.omp/kicad.json` check it. Pin 4 leaves at the pad width, so the switch node keeps TI's land
gaps. C36.2 and C43.2 tie to U10's exposed pad on F.Cu, so both capacitor loops close on the top
layer as in TI Fig. 23 p.24. The exposed pad takes thermal GND vias (DRV0006A land pattern
p.35; the "U10 exposed pad vias" entry).

The +32V trunk starts at D3's cathode (U10 itself is not on +32V). Find its lengths with
`kicad_query lengths` paths from D3.1, C45.1, C47.1 and C46.1 to D4.2, with via heights counted.
The branches to R65, R71 and TP13 carry µA and are thin.

The trunk is wide near D3 and necks only where the space forces it. The run from D3 to C47 stays
on F.Cu along the bottom edge, kept off the board copper under the module U6
([layout](layout.md#32v-and-the-ring)); it carries only the recharge current. The pulse comes
mostly from C46, so its path to D4.2 is short. Its drop is I_LED max × (copper resistance of
that path, ρCu × length / (width × thickness)). The headroom budget uses a bound over the whole
D3 → D4 path instead.

See [layout](layout.md#led-driver-and-ring) for the copper and spacing rules.

## Current sink

Q1 (HL2310A, R+O, 60 V logic-level N-MOSFET in SOT-23) sits in series with the bottom of the string:
`LED_K` is the drain and `ISNS` the source. R69 goes from `ISNS` to GND. U11 (TLV9061 on +5V, TI
SBOS839N) regulates the voltage across R69 to the setpoint `ISET`.

### Setpoint

`ISET` is a divider R60 / R61 fed from `LED_ON`, the output of the gate U9
([STROBE gating](#strobe-gating-and-timeout)). U9's output high is +3V3 (VOH at a light load).

- **LED_ON high:** `ISET` = +3V3 × R61 / (R60 + R61).
- **LED_ON low:** `ISET` = 0.

C40 at U11.3 filters it: τ = (R60 ∥ R61) × C40, microseconds, so the LED current follows `LED_ON`
within microseconds.

R63 from +5V and R64 from the Kelvin tap `ISNS_K` meet at U11's inverting input. They add a fixed
offset of +5V × R64 / (R63 + R64). With `ISET` = 0 this is above the op-amp's ±1.6 mV Vos
(SBOS839N p.13), so U11 drives Q1 fully off rather than leaving a trickle. With the offset, the
LED current is:

$$I_{LED} = \frac{(ISET \pm V_{os})\cdot\frac{R63+R64}{R63} - V_{+5V}\cdot\frac{R64}{R63}}{R69}$$

### LED current

The limit is the DC derating curve of the A-SC880IR3C (Amicc ADE-880-017 v1, p.4 Fig. 1). It runs
linearly from 1000 mA at 25 °C to 200 mA at 85 °C, so I_max(T) = 1000 mA − (1000 − 200) mA /
(85 − 25) °C × (T − 25 °C), taken at the 30 °C ambient maximum.

The datasheet's 1200 mA peak rating applies only to ≤ 100 µs pulses at ≤ 1 % duty (p.3). The
0.5 ms / 6 % pulses here don't qualify for it. The user chose to stay on the DC curve instead of
a lower current that would keep margin (user decision 2026-10-31).

R60 and R61 set the current. The worst case (lowest to highest I_LED from the formula above) must
stay at or under I_max(30 °C). The pair is chosen from fee-free values (user decision 2026-11-09)
so that its worst case stays under the curve at 30 °C.

Inputs of the worst case:

| Input | Range | Source |
|---|---|---|
| +3V3 | DC set-point range: VFB 0.588–0.612 V (TI SLVSDG1C p.4) × (1 + R16/R17) at the resistor tolerances | [Rails](README.md#rails), [3.3 V buck](power.md#33-v-buck). `ISET` follows `LED_ON`, which is +3V3 |
| +5V | `+5V` on PoE, the only source the LEDs run on | [Boost input range](#boost-input-range). U11 and R63 are on `+5V`, not `+5V_BST` |
| Resistors | Tolerance corners of every value-setting resistor | All value-setting resistors are one tolerance class |
| Op-amp offset Vos | ±1.6 mV | SBOS839N p.13 |
| R69 temperature coefficient | ±800 ppm/°C (UNI-ROYAL low-ohm chip-resistor datasheet p.6, 0.03 Ω < R < 1 Ω). I_LED ∝ 1 / R69 at R69's own temperature | The worst case takes R69 at 25 °C; an R69 that drops by TCR × ΔT when warm raises the current by the same fraction, out of the same margin |

Nominal assumes +3V3 3.3 V and +5V 5.0 V. The LED current depends on the DC level of +3V3, which
the buck's set point bounds. So it uses the DC set-point range, not the generic ±5 % envelope.
With the envelope the margin to the curve would be smaller.

The margin to I_max(30 °C), divided by the curve's slope, is the degrees of LED heating above the
30 °C ambient that the setting allows, shared with the current rise from R69's temperature
coefficient (the TCR row): a thin allowance. The [first-board checks](#first-board-checks) test it.

R69 is a 1206 sense resistor. Its rating must cover I_LED max² × R69 during a pulse and the
average over the frame; its temperature coefficient is in the table above.

The sense is Kelvin-connected:

- **High side:** net tie NT1 sits next to R69.1. `ISNS_K` leaves through it to R64. The pulse
  reaches R69.1 from Q1.2 through the F.Cu ISNS pour; its lobe round the tap south of
  R69.1 carries no load current, so the pulse path is not in the measurement.
- **Low side:** R69.2 returns the pulse to GND through the F.Cu GND pour with several vias, plus a
  short link to one more. The series resistance (via and copper, ρCu 1.72e-8 Ω·m, 35 µm copper)
  is a small fraction of R69, and it lowers the LED current by the same fraction, on the safe side.

### Loop and gate

| Part | Role |
|---|---|
| C42 | U11 integrator capacitor (SINK_DRV to IN−), with R64 |
| R67 | Gate resistor. It isolates U11's output from Q1's input capacitance (Ciss, HL2310A p.2 and Fig. 5 p.3). Gate-charge peaks are small and µs long |
| R68 | Gate pull-down: Q1 stays off while U11 is unpowered |
| C44 | U11 supply bypass |

U11 swings to within 20–60 mV of its rails (SBOS839N p.13), so VGS_max = +5V minus that swing. That
is well above the gate voltage Q1 needs at the LED current (HL2310A p.3 Fig. 2, transfer curve at
25 °C and 125 °C; the gate-charge plateau is ≈ 3.3 V even at 3 A, Fig. 6), so Q1 has drive left
over.

The loop estimate takes gm at the LED current from the slope of the transfer curve (HL2310A p.3
Fig. 2); the datasheet gives no gfs. The source follower then has a gain of gm × R69 / (1 + gm ×
R69), and the loop crosses over at gain / (2π × R64 × C42). That is below the gate pole (R67 with
Q1's Ciss) and U11's 10 MHz GBW. Read from a typical curve, gm is an estimate, so the phase
margin is too: [first-board check](#first-board-checks).

In the headroom loop's band Q1 runs near or inside its linear (triode) region at the end of a
pulse. U11 then drives VGS higher, still under its VGS_max swing, because the band's minimum is a
few times the VDS that RDS(on) needs at VGS_max. The lower gm there lowers the crossover, which
adds phase margin. An estimate too: [first-board check](#first-board-checks).

**Q1 ratings.**

- **Safe operating area:** Q1's worst full-scale VDS (Vout max − 12 × VF min − R69 and copper
  drops) × I_LED max lies inside the 1 ms line for t_pulse, and inside the 10 ms line up to the
  timeout window's upper end (HL2310A p.4 Fig. 7, single pulse at 25 °C). Repeated pulses are
  bounded by the junction temperature ([Thermal](#thermal)), not by this figure.
- **Turn-off:** the overshoot is L_loop × I_LED max / t_off (estimated loop inductance from the
  layout, t_off ≥ 0.3 µs), so VDS peak = Vout max + that overshoot, below the 60 V rating. The
  datasheet gives no avalanche rating, so the overshoot must stay under 60 V on its own.
- **Body diode:** never conducts, because the source goes to R69 and GND.

### LED_K sense

R70 from `LED_K`, Q1's drain, and R76 to GND divide V(`LED_K`) down into `LED_K_SENSE` on
GPIO23 = ADC1_CH7 (U6.19). C48 sits at the pin. They replace the NTC TH1 and its +3V3 pull-up,
and reuse that route (user decision 2026-11-03: "Replace TH1/R70 on NTC_SENSE (GPIO23)", divider
"100k / 10.0k (÷11)"). R70 sits where TH1 was, its `LED_K` pad joined to Q1's drain copper; R76 and
C48 sit next to U6.19.

| Item | Formula / requirement |
|---|---|
| Conversion | V(`LED_K`) = V(pin) × (R70 + R76) / R76 |
| In a pulse | The loop's band at the pin is the band edges × R76 / (R70 + R76); at duty 1 the pin sees the highest V(`LED_K`), which is Vout max minus the string minimum and the other drops |
| Between pulses | `LED_K` sits at +32V minus the string's forward voltage at the divider's current, ≈ 12 × VF at µA currents (an inference: the Amicc datasheet gives VF only at 1 A; first-board check 7). The bound is +32V itself: Vout max × R76 / (R70 + R76), with R70 low and R76 high within tolerance, must stay inside ATTEN3's 0–3300 mV (P4 DS Table 5-6 p.90) |
| Error | ±15 mV at the ADC × (R70 + R76) / R76, plus the two resistor tolerances as a share of the reading. Espressif specifies the ±15 mV with 100 nF at the pin (P4 DS p.90); with C48 and this source impedance the sampling error is unverified, so first-board check 7 measures it |
| Source impedance, settling | R70 ∥ R76 with C48: τ = R × C48, with C48 at its tolerance and the X7R temperature coefficient (±15 %, FH MLCC datasheet p.5). From the off level to within 1 mV takes τ × ln(V_off / 1 mV), so a sample ≥ 0.1 ms into the pulse has settled. The droop slews `LED_K` at I_LED max / C_bank; the pin lags that by τ × slew |
| R70 | Working voltage and power of its 0402 package must exceed Vout max across it and Vout max² / (R70 + R76) (UNI-ROYAL chip-resistor datasheet p.4, p.5) |
| String bias | Between pulses the divider draws Vout max / (R70 + R76) through the ring: a negligible fraction of the pulse current, invisible at 850 nm and negligible against the markers |
| Unpowered P4 | After a power loss C46/C47 may still hold charge: the pin then sees up to Vout max × R76 / (R70 + R76) through R70 ∥ R76 into the unpowered pad, a current of about Vout max / (R70 + R76 ∥ pad). The P4 datasheet gives no injection limit ([Accepted risks](#accepted-risks)) |

C48 is small, not the 0.1 µF that Espressif's HDG §1.3.10 suggests at an ADC pin: with 0.1 µF the
time constant would be longer than the pulse.

## STROBE gating and timeout

### Gate

U9 (SN74LVC1G97, TI SCES416N) is a configurable gate with Schmitt-trigger inputs. With In1 tied
to GND it works as an AND gate (Table 1, p.8): In2 low gives Y = In1 = low, and In2 high gives
Y = In0. So:

`LED_ON = STROBE_TO ∧ LED_EN`

| Input | Net | Source | Default |
|---|---|---|---|
| In0 (U9.3) | `LED_EN` | GPIO11 (U6.13), TP11 | R56 pull-down: off from reset |
| In2 (U9.6) | `STROBE_TO` | The camera STROBE through the timeout network below | – |
| In1 (U9.1) | GND | – | – |
| Y (U9.4) | `LED_ON` | Drives the R60/R61 setpoint, TP12 | – |

U9 runs from +3V3 with C37. Its inputs come only from +3V3-powered parts (U6 and the STROBE
buffer U7 on the Camera sheet), so nothing back-powers it.

STROBE is the sensor's exposure output, buffered by U7. The P4 also captures it on GPIO12 (U6.14)
([camera](camera.md#strobe)).

### Timeout

A one-shot high-pass network makes the LEDs go dark even if STROBE sticks high or a wrong sensor
setting gives a long exposure:

1. STROBE drives R54 into C33.
2. R55 pulls the far side, `STROBE_HP`, to GND.
3. R58 feeds `STROBE_HP` to U9's In2.

A rising STROBE edge lifts `STROBE_HP` to V0 = VOH × R55 / (R54 + R55). It then decays with
τ = C33 × (R54 + R55). When it falls below U9's VT−, `LED_ON` drops. The LED pulse
therefore ends at the earlier of STROBE falling and

$$t = \tau \cdot \ln\frac{V_0}{V_{T-}}$$

U7 (the STROBE buffer) and U9 share +3V3, so VOH and the thresholds move together. VT− is
0.84–1.19 V at VCC 3 V and 1.41–1.9 V at 4.5 V (SCES416N p.5). Between the rows it is interpolated
linearly in VCC.

| Case | Inputs |
|---|---|
| Shortest, design bound | Generic ±5 % envelope on +3V3, named as such: V0 from the envelope minimum − 0.1 V VOH, VT− max interpolated at 3.3 V; C and R at their low tolerance |
| Shortest, set-point range | +3V3 over the DC set-point range ([Rails](README.md#rails)) on the shared rail: V0 from VCC − 0.1 V, VT− max at the same VCC; C and R low |
| Nominal | Nominal +3V3, C and R nominal, VT− at mid-range |
| Longest, set-point range | +3V3 maximum, VT− minimum (the 3 V row); C and R high |
| Longest, design bound | Envelope maximum, VT− minimum; C and R high |

The published window is the design bound, from the shortest to the longest of those cases. The DC
set-point range narrows it. C33 counts with its tolerance and the X7R temperature coefficient
(±15 %, FH MLCC datasheet p.5) in "low" and "high".

The user's answer was "≤ 0.5 ms at ≤ 120 fps (6 %)" (user decision 2026-10-31). The option they
chose said the timeout cuts above ≈ 0.6 ms. The circuit can't cut that close: U9's threshold spread
sets the window. The user accepted the window (user decision 2026-11-02: "Accept 0.525–1.026 ms"),
then moved C33 to a fee-free X7R part with R55 raised to keep the 0.5 ms pass, which widens the
window to 0.61–1.79 ms (user decision 2026-11-09, superseding the 2026-11-02 window):

- the RC/U9 timeout stays a backstop;
- firmware limits pulses to 0.5 ms;
- a firmware fault could run the LEDs up to the window's upper end per frame, a duty of t_max × fps,
  before the cut.

- **Passing 0.5 ms:** even the shortest unit passes a 0.5 ms pulse, with margin at the design bound
  and more on the set-point range.
- **Why the cut can't be tighter:** U9's own VT− spread sets a ≈ 2:1 width, and C33's X7R
  tolerance and temperature coefficient widen it further, so no RC value cuts closer above 0.5 ms.

Further checks:

- **Every edge switches:** VT+ max interpolated at the supply maximum (and at the envelope
  maximum) is below the minimum V0 at the envelope (and on the set-point range).
- **Recovery:** the frame period (1 / fps) is many τ, so the network has fully recovered before
  the next frame.
- **Negative swing:** when STROBE falls after a timed-out pulse, `STROBE_HP` swings to about
  −V0. R58 limits U9's input clamp current to about V0 / R58, against the −50 mA IIK rating.
  The datasheet allows a negative input while the clamp current is respected (p.4 note 2).
- **Edge current:** R54 limits the edge current from the STROBE buffer to VOH / R54.

The timeout is a backstop. Firmware keeps the exposure, and so the LED pulse, at ≤ 0.5 ms per
frame at ≤ 120 fps ([firmware rules](firmware-rules.md#rules)). A fault that only the hardware
stops can still give pulses up to the window's upper end, a duty of t_max × fps.

## LED ring

D4–D15 are 12 × A-SC880IR3C: 850 nm centroid (855 nm peak), 90° viewing angle, 285–450 mW/sr at
1 A (Amicc ADE-880-017 p.3).

They form one series string: D4's anode is on +32V, `RING_1`…`RING_11` join the LEDs, and D15's
cathode is `LED_K` at Q1's drain. One string means one current for all twelve LEDs, no ballast
resistors and one sink. The LEDs are never reverse-biased.

The footprint is the project library part `LED_SMD_Amicc:LED_Amicc_A-SC880IR3C_3.85x3.85mm`: pad 1
is the cathode, and pad 2 (centre and outer pad) is the anode, as Amicc p.6 shows.

**Placement.** The LEDs sit evenly around the lens axis, outside the Ø16 mm lens cutout. They
are locked in place. The string opens between D4 and D15 on the east side, where +32V comes in
and `LED_K` leaves for Q1 next to them.

Each LED must stay outside the cone of the `layout.heightZones` entry "lens view cone" in
`.omp/kicad.json` (h = LED height from the Amicc datasheet); check with `kicad_query distance`.

**Copper.** The ring links are as wide as the exposed-copper spacing to the LED's other pad
allows: a wider track would break that spacing. At I_LED max as DC the temperature rise of such a
link is small (IPC-2152 fit).

Exposed copper on the LED nets keeps the IPC-2221A Table 6-1 A6 spacing for uncoated terminations
at 31–50 V (rules "LED_PWR pad spacing" and "LED_PWR pad spacing (other pads)",
[layout](layout.md#custom-rules)).

### String voltage vs +32V

VF is 1.7 / 1.95 / 2.3 V min/typ/max at 1 A, with a ±0.1 V test tolerance (Amicc p.3). The string
therefore drops 12 × VF, up to 12 × (VF max + 0.1 V) with the tolerance.

| Needed at the top of the string | Formula |
|---|---|
| String, VF max + test tolerance | 12 × (VF max + 0.1 V) |
| R69 | I_LED max × R69 |
| Q1 RDS(on) | RDS(on) at VGS 4.5 V × 1.5 hot (HL2310A p.2, Fig. 4 p.3) × I_LED max |
| +32V copper | Bound over D3 → D4 (the C46 path that supplies the pulse is shorter) |
| Droop over the pulse | I_LED max × t_pulse / C_bank (capacitor tolerance low) |
| ESR step | I_LED max × ESR of C46 ∥ C47 |
| **Total** | Sum of the above |

The total must stay under Vout min (duty 1), so the sink stays in regulation through the whole
0.5 ms pulse on every unit.

The other side of the range is Q1's job: at full scale (duty 1), with the highest +32V and the
lowest-VF LEDs, Q1 drops Vout max − 12 × VF min − R69 and copper drops ([Thermal](#thermal)). The
headroom loop removes most of that by lowering +32V to the string
([Headroom control](#headroom-control)).

## Thermal

### Q1 dissipation

Q1 takes up whatever voltage the string doesn't use. With the [headroom loop](#headroom-control)
running, that is the loop's band at the end of the pulse plus the bank's droop over it. At full
scale (duty 1: only the start-up burst, the first average and the bleed-down after the coarse step, FW-22) it is
everything between +32V and the string.

P_pulse = V_DS × I_LED, and the average is P_pulse × duty (duty = t_pulse × fps).

| Case | V_DS over the pulse (average; the duty-1 rows use the start of the pulse, an upper bound) | I_LED and C_bank |
|---|---|---|
| Loop, typical | Band at the end + droop / 2 | Nominal current, nominal C_bank |
| Loop, worst | Band top + reading error at the end + droop / 2 | I_LED max, C_bank at its low tolerance |
| Duty 1, typical | Vout nominal − string at VF typ − R69 − copper | Nominal current |
| Duty 1, worst | Vout max − 12 × VF min − R69 − copper | I_LED max |

HL2310A (p.1) gives RθJA 104 °C/W (the test board is not stated) and TJ max 150 °C; Fig. 9 (p.4)
gives the normalized transient impedance Zθ(t, D). The junction peaks at
TJ = T_amb + P_pulse × Zθ(t_pulse, duty) × RθJA, at the 30 °C ambient maximum; the average alone is
T_amb + P_avg × RθJA.

- **Loop running:** the loop rows give a small rise. The loop is Q1's design basis, a requirement
  (user decision 2026-11-09).
- **Full scale, continuous:** the duty-1 worst row at t_pulse and t_pulse × fps reaches about
  TJ max on the datasheet figure, so it is not an operating state. FW-22 never holds duty 1 with
  the LEDs pulsing, and its power rule cuts a loop stuck at full scale long before Q1 overheats
  ([Q1 protection](#q1-protection)); a firmware failure there is an
  [accepted risk](#accepted-risks).
- **Start-up burst:** the lit frames from the first pulse until the fine steps begin. Those are the
  n_avg frames of the first average at full scale, then the bleed-down after the coarse step: the
  boost can't sink current, so V(`LED_K`) falls only by ΔV = I_LED × t_pulse / C_bank per pulse
  ([Headroom control](#headroom-control)). The burst is therefore
  n = n_avg + (V_full,max − V_T) / ΔV_min frames, with V_full,max the highest full-scale end
  reading, V_T the coarse target and ΔV_min = I_LED min × t_pulse / C_bank,max: a few tens of
  frames, a fraction of a second at 120 fps. Taking every frame at the full-scale worst row (an
  upper bound: the bleed-down frames are lower), over the burst
  TJ ≤ T_amb + P_avg × Zθ_single(n / fps) × RθJA + P_pulse × Zθ_single(t_pulse) × RθJA (the
  single-pulse curve of Fig. 9), far inside 150 °C.

The board gives Q1 a limited area of drain copper:

- the drain pad (Q1.3) on the F.Cu pour "LED_K pour 121",
- thermal vias in that pour beside the drain pad,
- a B.Cu zone ("Q1 drain B.Cu").

HL2310A doesn't state the board behind its 104 °C/W, so the bound takes the figure as given and
the first boards measure it ([check 1](#first-board-checks)). The drain lead carries the heat into
that copper. The `.omp/kicad.json` layout entry "Q1 drain copper" requires a minimum copper area
and via count on this net.

More copper doesn't fit. The drain copper keeps clear of the crystal, the RMII and CSI B.Cu
routes, U6's courtyard and the ring. Q1 stays in the pulse loop, between the string's return,
R69 and C46.

### Neighbours

Parts next to Q1's drain copper (distances: `kicad_query distance` from Q1 and the `LED_K` pour):

| Part | Concern |
|---|---|
| R70 (`LED_K_SENSE` divider) | Its `LED_K` pad joins the drain copper |
| C8 | – |
| C2 (crystal load cap) | – |
| D15 (pad 1) | LED heating |
| D4 (pad 2) | LED heating |
| Y1 (25 MHz crystal) | Rated −40…+85 °C; ±20 ppm specified only over that range (Epson TSX-3225 p.1) |
| C46 | Rated maximum temperature (GVZ datasheet p.1) |
| C47 | Rated maximum temperature (GVZ datasheet p.1) |

With the loop running Q1's rise above the board is P_avg × RθJA, small. Full-scale drain-copper
temperatures next to D15, D4, Y1 and C46 occur only in the start-up burst or with a failed loop.
No datasheet figure gives their temperature then. Even a few degrees from copper this close may
use up the LED heating allowance of the [current setpoint](#led-current).

The LEDs also heat themselves. Each dissipates up to I_LED max × VF max × duty (the ring, 12
times that) through Rs 18 °C/W junction to case. Amicc rates them at a 25 °C solder point and tests
on a 1 × 1 cm metal-core board. Here each LED has only its pads and thin links on FR-4.

The user chose to measure this on the first boards rather than lower the current or move Q1 now
(user decision 2026-11-01): see [First-board checks](#first-board-checks).

### Q1 protection

The board has no temperature sensor: the NTC that sat beside Q1 gave way to the `LED_K_SENSE`
divider (user decision 2026-11-03), whose reading bounds Q1's dissipation directly. Firmware keeps
(V(`LED_K`) at the end + droop_max / 2) × I_LED max × LED duty ≤ P_limit = 0.40 W and reports a
reading or string fault (FW-22). P_limit is a firmware decision, recalculated for the HL2310A's
150 °C TJ max (2026-11-09): at every LED duty up to FW-9's cap it holds the peak
TJ = T_amb + (P_limit / duty) × Zθ(t_pulse, duty) × RθJA (HL2310A Fig. 9 p.4) well under 150 °C.
The pulse average is the end reading plus half the worst droop, as in the
[table](#q1-dissipation). It applies only once the fine steps begin, after the post-step hold of
FW-22: during the start-up burst V(`LED_K`) is still bleeding down from its full-scale value and
the rule would trip on typical units, while the burst bound of [Q1 dissipation](#q1-dissipation)
already covers Q1 there. In the band it computes far less. P_limit sits below the typical
full-scale power, so a loop stuck at full scale (or one that has drifted far above the band)
trips the rule on most units, well before Q1 overheats; the rule shortens the exposure or drops
`LED_EN` there.

A pulse longer than the set exposure, up to the hardware timeout's upper end
([Timeout](#timeout), duty t_max × fps), averages over the pulse (V_DS at duty 1 − droop' / 2) ×
I_LED max × that duty at duty 1 (droop' for the longer pulse into the lowest C_bank): a TJ far
above the 150 °C rating; with the loop still running, below it (Fig. 9 at t_max and t_max × fps).
The power rule above sees it only in its average, so FW-9 adds a pulse-width fault: firmware times
every STROBE pulse and drops `LED_EN` before the next frame when one exceeds its limit (the 0.5 ms
cap plus STROBE's width tolerance, [check 5](#first-board-checks), below the shortest hardware
timeout). That leaves single pulses, which the average doesn't feel; the hardware timeout stays
the backstop if the firmware itself fails ([Accepted risks](#accepted-risks)).

## Accepted risks

- **LED, C46 and Y1 temperature next to Q1 at full scale is unknown.** With the headroom loop Q1
  dissipates only its band's power; at full scale (the start-up burst, a failed loop) the worst
  case of [Q1 dissipation](#q1-dissipation) on a limited area of drain copper, close to D15 and D4
  (`kicad_query distance` from Q1), against the LED heating allowance. Accepted by measuring
  the first boards instead of lowering the current or re-floorplanning now (user decision
  2026-11-01).
- **Headroom control is firmware, and Q1 depends on it.** The loop is a requirement (user
  decision 2026-11-09; design option 1b, user decision 2026-11-03). If firmware leaves +32V at
  full scale with the LEDs pulsing, Q1's junction reaches or exceeds its 150 °C maximum at the
  worst duty ([Q1 dissipation](#q1-dissipation)). FW-22's fallback turns the LEDs off, and its
  power rule trips a loop stuck at full scale on most units, well before Q1 overheats; no hardware
  bound does.
- **A firmware failure at duty 1 can overheat Q1.** If firmware both leaves +32V at full scale and
  lets STROBE run past 0.5 ms, only the hardware timeout ends each pulse, at up to the window's
  upper end: the power and TJ of [Q1 protection](#q1-protection), above Q1's 150 °C rating. FW-9's
  pulse-width fault and FW-22's power rule prevent it; no hardware bound does (design review
  2026-11-03).
- **No temperature sensor.** TH1 was dropped to reuse its route for `LED_K_SENSE` (user decision
  2026-11-03); firmware bounds Q1's power from the drain voltage instead
  ([Q1 protection](#q1-protection)).
- **C48 is small, not the 0.1 µF HDG §1.3.10 suggests at an ADC pin,** because 0.1 µF would not
  settle within the pulse ([LED_K sense](#led_k-sense)).
- **`LED_K_SENSE` can drive current into an unpowered P4 pin** while C46/C47 hold charge after a
  power loss (bound in [LED_K sense](#led_k-sense)); the P4 datasheet gives no injection limit.
- **LED current set to the DC curve at 30 °C with a thin margin.** R60/R61 (user decision
  2026-10-31, stay on the DC curve; fee-free values, user decision 2026-11-09). The margin is
  I_max(30 °C) − worst-case I_LED, in degrees of LED heating = margin / the curve's slope
  ([LED current](#led-current)). R69's temperature coefficient takes from the same margin: an R69
  ΔT above 25 °C moves the current by up to TCR × ΔT, which alone uses the margin up only far
  above any expected board temperature (user decision 2026-11-09). Any LED heating above ambient
  beyond that needs a lower R61 ([first-board checks](#first-board-checks)).
- **No hardware gate on USB power.** The LEDs stay off on USB alone only because firmware keeps
  `LED_PWR_EN` low while `PWR_SRC` is low (user decision 2026-10-31).
- **The hardware timeout cuts only within its window.** The ≤ 0.5 ms limit is a firmware rule.
  U9's threshold spread and C33's X7R tolerance set the window, so it can't cut closer. A firmware
  fault could run the LEDs up to the window's upper end per frame, a duty of t_max × fps (user
  decision 2026-11-09: X7R C33, window 0.61–1.79 ms, superseding the 2026-11-02 "Accept
  0.525–1.026 ms").
- **Boost output capacitance is far outside TI's 1–10 µF recommendation.** The compensation is
  designed for it (estimated crossover and phase margin in [Compensation](#compensation)), but TI
  asks for such values to be tested. Accepted in design review (2026-11-01), tested on the first
  boards.
- **Current-loop phase margin is an estimate.** Q1's gm at the LED current comes from a typical
  curve (HL2310A Fig. 2); the datasheet gives no gfs.
  Accepted in design review (2026-11-01), tested on the first boards.
- **No GND guard between the crystal (Y1, C2) and the LED copper.** The +32V track runs close to
  C2.1, and the Q1 drain zone overlaps C2.1 in plan view on B.Cu, with both inner GND planes in
  between (`kicad_query distance`). A guard track doesn't fit, and IP101G §6.1 p.56 asks for guards
  only "if possible". Accepted in design review, 2026-11-01
  ([layout](layout.md#led-driver-and-ring)).
- **Small parts beside C46.** R5 and C8 sit beside C46 (the tall can), with their courtyards
  touching or nearly: possible reflow shadowing; no source gives a limit
  (`kicad_query distance`; [layout](layout.md#assembly),
  [first-board check 8](#first-board-checks)).

## First-board checks

1. **Thermal test** (user decision 2026-11-01). Run the LEDs at the worst duty, 0.5 ms pulses at
   120 fps, for 30 min, on PoE, with the headroom loop running. Record the ambient temperature. No
   long run at full scale: Q1's junction reaches its maximum there ([Thermal](#thermal); user
   decision 2026-11-09).
   - **Thermocouples:** on the cases of D15 and D4 (nearest Q1), on C46 and on Y1's case. Add Q1's
     drain lead (Q1.3) and an LED far from Q1 (D9 or D10) to separate self-heating from Q1 heat.
   - **Loop:** log the duty and `LED_K_SENSE` during the run.
   - **LED limit:** take the hottest LED case temperature T as the curve's temperature, which is
     conservative. The worst-case current must stay ≤ 1000 mA − (1000 − 200) mA / (85 − 25) °C ×
     (T − 25 °C). If it doesn't, lower R61 (procedure below).
   - **Other parts:** C46 must stay well under its rated maximum temperature. Y1 must stay 5 °C
     under its 85 °C rating; if it doesn't, lower the LED current (R61) or the duty, or pull the
     east edge of the B.Cu zone "Q1 drain B.Cu" back ([layout](layout.md#thermal)).
2. **LED current.** Measure the voltage across R69 at TP14 during a pulse: expect I_LED × R69 at
   nominal rails (+3V3 3.3 V, +5V 5.0 V; the [LED current](#led-current) formula), flat over the pulse. Measure it on
   the unit with the highest +3V3 together with the thermal test, because a ground offset between
   R69.2 and U11's reference shifts the current by δV / R69. U11 holds V(R69) at the setpoint, so
   the reading gives the current only through R69's resistance at its own temperature (TCR row in
   [LED current](#led-current)): take it in the first seconds of the run, with R69 at the board
   temperature, and log R69's case temperature at the end of the run with the reading there.
3. **Current-loop response.** Scope `ISNS` (TP14) against `LED_ON` (TP12) at both edges. Expect
   < 10 % overshoot, no ringing and a µs-scale rise.
4. **Boost stability.** Scope +32V (TP13) and `BST_SW` (at L3.2/D3.2) at start-up and during
   0.5 ms / 120 fps pulses, at duty 1 for the start-up burst only (FW-22) and at every set point
   down to the loop's lowest (on a low-VF unit), including the loop's duty steps. Expect:
   - no ringing or sub-harmonic switching,
   - +32V within the [Output voltage](#output-voltage) range at duty 1 and within
     D × Vout(duty 1) nominal × (1 ± reference tolerance ± divider tolerance ± on-time error)
     below it,
   - no overshoot above the output maximum, and well under U10's 40 V switch rating,
   - recovery before the next frame (droop ≤ I_LED × t_pulse / C_bank, back within
     Q / Iout_max at duty 1, [Recovery and start-up](#recovery-and-start-up));
   - a start-up time in the range of [Recovery and start-up](#recovery-and-start-up), with no
     EasyScale entry or shutdown when the 50 kHz PWM follows the soft start.
   Compare the `VLED_SENSE` reading × the R71/R72 ratio with TP13. Scope `+5V_BST` during the
   recovery to see whether the boost runs at its current limit there, as
   [estimated](#recovery-and-start-up).
5. **Timeout.** With `LED_EN` high:
   - a 0.5 ms STROBE must give a whole 0.5 ms `LED_ON` pulse;
   - a STROBE held high (a long exposure) must end `LED_ON` within the accepted window
     (0.61–1.79 ms, user decision 2026-11-09);
   - `LED_ON` must follow the next normal frame unchanged;
   - STROBE's width must match the configured exposure within 50 µs over the exposure range
     (FW-9's pulse-width limit, the exposure cap plus 50 µs, assumes it), and a longer STROBE, e.g. 0.6 ms, must make firmware
     drop `LED_EN` before the next frame.
6. **Default-off and USB.** At reset and in download mode, nothing drives the three enables:
   `LED_PWR_EN` (R57), `BST_EN` (R59) and `LED_EN` (R56); R68 also holds Q1's gate low. Then +32V
   must stay unpowered (U8 off) and `LED_ON` low. On USB power alone, firmware must hold
   `LED_PWR_EN` low: check that +32V stays at 0 V. Pulling PoE must drop `LED_EN`, `BST_EN` and
   `LED_PWR_EN` in that order.
7. **Headroom loop.** With probes on Q1's drain (R70.1) and on `ISNS` (TP14), on one unit unless
   a step names two:
   - `LED_K_SENSE` × (R70 + R76) / R76 must match the drain at the end of a pulse within the
     [LED_K sense](#led_k-sense) reading error;
   - step the duty down until the current at the end of the pulse starts to sag, and record
     V(`LED_K`) there: the band's lower edge must stay ≥ 0.3 V above it;
   - repeat check 3 at the band's lower edge, where Q1 runs nearest its linear region;
   - on a low-VF and a high-VF unit, at the lowest set point and in the start-up burst at duty 1, log the in-pulse and
     the between-pulse `LED_K_SENSE` readings of each frame and their difference; then hold Q1
     off (`LED_EN` low with STROBE running, or Q1's gate shorted) and log the same pair. Set
     FW-22's sink-off threshold (a first value) between the two differences, and record
     VF per LED at the divider current;
   - from power-up, log the coarse step and the settling: V(`LED_K`) right after the coarse step
     must land at or above the band's need and within the bounded excess of
     [Headroom control](#headroom-control), then settle inside the band within the estimated
     time;
   - time the `LED_K_SENSE` one-shot conversion: it must fit the last 0.05 ms of a
     pulse;
   - on a low-VF unit, a test build that holds the duty at 1 in place of the fine steps: FW-22's
     power rule must shorten the exposure or drop `LED_EN` within one average;
   - from power-up on a low-VF and a high-VF unit, count the lit frames until the fine steps
     begin (the start-up burst of [Q1 dissipation](#q1-dissipation)) and check that the power rule
     doesn't trip before them or in the band.
8. **Joints beside big parts:** AOI or a visual check of R5 and C8 (beside C46) for wetting,
   tombstoning and shifted parts.

If the thermal test fails, pick R61 so that its worst-case current (the inputs of
[LED current](#led-current), with 1 % values, fee-free ones first, user decision 2026-11-09)
stays on the DC curve up to the measured
temperature: the highest temperature at which a value stays on the curve is
T = 25 °C + (1000 mA − I_worst) / ((1000 − 200) mA / (85 − 25) °C). Radiant output drops roughly
in proportion to the current.
