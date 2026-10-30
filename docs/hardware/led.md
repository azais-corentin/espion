# LED ring and driver

Twelve 850 nm IR LEDs around the lens light the retroreflective markers. One series string on a 32 V
boosted rail runs at a fixed ≈ 1 A only while `STROBE ∧ LED_EN`. Firmware caps a pulse at 0.75 ms; a
hardware timeout ends it after 0.78–1.32 ms at the latest. Layout: see [layout.md](layout.md).

## Ring sizing

- **Target ≥ 2.3 W/sr on axis:** ≈ 1 ke⁻ per pixel from a 14 mm sphere at 5 m in the image corner of
  the 79° lens, in a 500 µs exposure (estimate). Assumes OV9281 QE 25 % at 850 nm (OmniVision publishes
  none), f/2.0, sphere retroreflection 150 cd/(lx·m²) (half of flat 3M 7610), and 17 % of the on-axis
  signal left in the corner (beam, lens falloff, observation angle, marker wear).
- **12 LEDs give 4.6 W/sr** typ, 3.4 W/sr with minimum-bin parts (380 / 285 mW/sr each at 1 A): ≈ 2× margin.
- **90° beam:** the image corner is 40–44° off axis, where it still gives half intensity; 120° wastes
  light outside the image, 60° leaves the corners dark. Lens filter: bandpass around the LED's 850 nm
  centroid (855 nm peak), wide enough for its 25 nm FWHM + drift.
- **Thermal:** 1 A at 6 % duty is within the 1 A continuous rating (1.2 A only ≤ 100 µs, ≤ 1 % duty); ≈ 0.12 W per LED, R_θJC 18 K/W: plain FR4.
- **Fixed current, no continuous mode.** Near markers (≈ 40× brighter at 2 m than at 5 m) get shorter
  exposures (Config); bring-up and focusing use the strobed light too.
- **String voltage:** V_F at 1 A is 1.7 / 1.95 / 2.3 V min/typ/max, each ±0.1 V: 19.2 V best, 23.4 V
  typ, 28.8 V worst. The 31.3–33.9 V rail covers it. (Amicc DS p.3)

## +32V rail and storage

- **32.57 V (31.30–33.88 V):** R65 255 kΩ / R66 10.0 kΩ, V_REF 1.204–1.254 V, 1 %; under the 40 V SW
  rating. C43/C45 are 100 V: TI wants ceramics rated ≥ 1.5× their bias. (SLVS789D Eq. 2, §7.5, §9.2.2.7)
- **Storage 2 × 220 µF/50 V** (C46, C47). A 500 µs pulse takes 0.5 mC: ≈ 1.1 V droop plus ≤ 0.42 V on
  the ESR (≤ 0.84 Ω each, from tan δ 0.14). Two caps for ripple: 0.24 A RMS at 6 % duty (0.36 A at
  15.8 %), while one is rated 230 mA at 120 Hz, ≈ 322 mA above 10 kHz. (RVT DS p.2)
- **Low-corner headroom, accepted.** Rail 31.30 V, 28.8 V string + 0.1 V shunt, caps −20 %: 2.13 V droop
  at 0.75 ms + 0.42 V ESR leaves −0.15 V for Q1, which needs ≈ 0.6 V (IRLR024N DS Fig. 1). The current
  sags from ≈ 0.49 ms into a pulse (0.56 V left at 500 µs). A typical board keeps several volts.
- **+32V stays charged** ≈ 58 s after the boost stops (D3 blocks the way back): `LED_EN` low turns it off.
- **Power:** 60 mA average (500 µs at 120 fps) is 1.9 W, ≈ 2.3 W at the boost input. PoE supplies only
  this average; the 23 W pulses come from the caps. LEDs run only on PoE ([firmware rule 4](firmware-rules.md)).

## Boost (TPS61170, U10)

- **≈ 100 mA at 32 V** (I_LIM min 0.96 A, 85 % efficiency); 91 mA worst case (f_S min, L −20 %); 165 mA
  at I_LIM max 1.44 A. A 500 µs pulse is refilled in ≈ 5 ms of the 8.33 ms frame; the boost sustains
  ≈ 0.8 ms of light per frame at 120 fps. (SLVS789D Eq. 3–5)
- L3 (10 µH) peaks at ≤ 1.44 A, under its 1.95 A I_sat and 1.50 A thermal rating; no L-vs-I curve is
  published. D3 SS16: 60 V (TI asks > 40 V), ≤ 0.17 A average. (FNR catalog p.10, SLVS789D §9.2.2.5)
- **Compensation, outside TI's tested range.** TI's default network (10 kΩ, 680 pF) assumes 1–10 µF of
  ceramics; with 440 µF it models at only 15° phase margin. Chosen: R62 100 kΩ + C41 10 nF (zero 159 Hz),
  C39 220 pF (pole 7.2 kHz, above the 0.43–0.86 kHz ESR zero, below the 108 kHz RHPZ). Over all corners:
  crossover 411–2650 Hz, PM ≥ 71.8°, GM ≥ 22.8 dB; cold (ESR × 2): crossover up to 12.6 kHz, GM ≥ 16.7 dB;
  one 0 dB crossing everywhere, without C39 some cross twice (estimate: SLVS789D Eq. 7–11).
- U10's WSON exposed pad needs solder paste and hot air, not an iron.
- **`BST_EN` → CTRL:** held high = on; low > 2.5 ms = off. A boot glitch could select EasyScale, whose
  default is full scale, so V_OUT is unchanged. All three enables have 100 kΩ pull-downs (CTRL's own is
  400–1600 kΩ): the rail stays off through PoE start-up and boot. (SLVS789D §8.3.5, §8.4)

## Load switch and start-up (TPS22918, U8)

- **Why:** without it L3 and D3 charge 440 µF from +5V at every power-up: ≈ 1.3–1.6 A, over the USB
  inrush limit, and a large start-up load on the PoE module.
- **≈ 27 ms ramp** (10–90 %, TI Table 2 for 10 nF X7R on CT); C35 is C0G and keeps its full capacitance,
  so expect up to ≈ 30 ms (≈ 33 ms 0–100 %), plus ≈ 7 ms delay (estimates). Inrush ≈ 447 µF × 4 V /
  26.55 ms ≈ 70 mA. (SLVSD76C Table 2, Eq. 3)
- **Boost start:** the reference ramps too fast for 440 µF, so the boost runs at current limit until the
  rail is up: ≈ 50–120 ms, 0.8–1.3 A through U8 (2 A rated). `BST_EN` waits until U8 has finished its
  ramp, so the boost's current never flows through U8 while U8 is still in its linear region ([firmware rule 4](firmware-rules.md)).
- **PoE → USB switchover:** the boost draws up to 1.44 A until firmware reacts; with the buck that is
  ≤ 2.0 A, under the mux's ≥ 2.5 A limit. **+5V up to 5.67 V, accepted:** over the 5.5 V recommended max
  of U8 and U11, under their 6 V abs max; the same accepted case as U3 in [power.md](power.md). (SLVSD76C §6, SBOS839N §6)
- U8 uses the stock `Power_Management:TPS22917DBV` symbol (same DBV pinout) with Value `TPS22918DBV`.

## Gate and timeout (SN74LVC1G97, U9)

- A 2-input Schmitt AND (In1 tied low, Y = In2·In0). In0 is `LED_EN`; In2 is STROBE through R54
  1.00 kΩ → C33 10 nF → node (R55 91 kΩ to GND) → R58 10.0 kΩ. (SCES416N Table 1)
- **Timeout 0.78–1.32 ms.** A rising STROBE lifts the node to 3.26 V, which decays with τ = 92 kΩ ×
  10 nF = 0.92 ms; Y falls at V_T−. No 3.3 V threshold row: interpolated between the 3 V and 4.5 V rows,
  V_T+ ≤ 2.04 V, V_T− 0.95–1.33 V (estimate); ±1 %/±5 % parts included. The 0.75 ms cap ([firmware rule 5](firmware-rules.md))
  is below the minimum, so the timeout only acts when firmware fails. A stuck-high STROBE lights one pulse.
- **R54** limits the STROBE buffer's edge current into C33 to 3.4 mA (rated ±7 mA). **R58** limits the
  clamp current to ≤ 0.26 mA when STROBE falls and the node steps to ≈ −1.3 V (−3.0 V after a long
  pulse), as SCES416N §6.1 note 2 allows. Recovery needs STROBE low ≥ 0.90 ms; at 120 fps it is ≥ 7 ms.

## Current sink (TLV9061 U11, IRLR024N Q1, 0.1 Ω R69)

- The op-amp holds the shunt at `ISET`, the gate output divided to 102.1 mV (R60 47 kΩ / R61 1.5 kΩ),
  less R63's 4.9 mV offset: **0.972 A typ**, 0.86–1.06 A worst case (divider, +3V3 ±3 %, offset ±2 mV,
  shunt ±1 % and ±100 ppm/K). Kept under 1 A so the top stays near the LED's rating. (SBOS839N §6.10)
- **Off state:** a +2 mV offset at `ISET` = 0 would pass 20 mA (visible glow). R63 1.00 MΩ from +5V
  lifts IN− by 4.9 mV, so the output rails low. R68 holds the gate off while +5V is gone.
- **Loop:** TI's dual-feedback V-I converter (R67 220 Ω, C42 220 pF, R64 1.00 kΩ from Kelvin tap NT1):
  68–80° phase margin, 22° without them (estimate). On 8–10 µs, off ≈ 5 µs: ≈ 1.5 % less light, centroid
  +3 µs, the same on every board and negligible next to the `exposure / 2` in the capture time
  ([timing.md](../timing.md)). Gate needs ≈ 2.5–3.0 V, inside the 5 V swing. (SBOA327A)
- **Q1 is planar, 55 V, for its linear-mode SOA.** It drops ≈ 9.1 V typ, up to 14.6 V with min-V_F
  LEDs: 9–15 W per pulse, ΔT_JC ≈ 12–13 K (≈ 20 K at the 1.32 ms timeout; estimates from DS Fig. 11).
  The 1 ms SOA line gives > 2× margin even derated 50 % at T_C 100 °C (Fig. 8). Average 0.78 W, T_J ≈
  T_A + 49 °C on 1 in² copper; ≤ 1.5 W (2.4 W worst) if every pulse hits the timeout. Fallback NCE6020AK
  (60 V trench, LCSC C108639) only after checking its SOA: trench parts often derate badly in linear mode.
- Shunt: 0.1 W per pulse, ≤ 16 mW average (1/3 W, Walsin WW DS p.3). TP14 (`ISNS`) reads 100 mV/A.

## Temperature and rail sense

- **NTC TH1** (10 kΩ, B 3380 K) by Q1's tab, R70 10.0 kΩ from +3V3: 1.65 V at 25 °C, 0.29 V at 100 °C;
  ±0.5 K and ±2.3 K with the ADC's ±15 mV, plus ≈ ±1.5 K from +3V3. Above 60 °C ATTEN1 resolves better.
  Open (≈ 3.3 V) or short (0 V) is a fault ([firmware rule 5](firmware-rules.md)). (P4 DS Table 5-6, Murata NTC)
- **`VLED_SENSE`** = +32V × 10/265 (R71 255 kΩ / R72 10.0 kΩ): 1.23 V at 32.6 V, 1.51 V at 40 V. Absolute
  error ≈ ±1 V, but the ≈ 1.1 V droop is a difference on one divider: ≈ 51 LSB, still visible through
  the 0.96 ms filter ([firmware rule 6](firmware-rules.md)). R71 is 0603 for working voltage (0402: 50 V).
- **P4 off, +32V charged, accepted:** the divider pushes ≤ 80 µA into GPIO22 with +3V3 at 0 V, over the
  pin's VDD + 0.3 V limit but too little to back-power +3V3 (estimate); as `VBUS_SENSE` in [mcu.md](mcu.md).

## The ring (D4–D15)

- **No per-LED parts:** every LED carries the same regulated current, so nothing needs balancing. An
  open LED stops the string (seen on `VLED_SENSE`); a shorted one adds ≈ 2 W per pulse in Q1 (NTC).
- **Reverse voltage** (5 V max) never occurs: the anode is +32V, ≥ 0 V and held by 440 µF behind D3;
  the cathode sees only Q1's drain, clamped at ≥ −0.7 V by its body diode.
- **ESD:** HBM 2 kV. Both ends sit on large capacitance, so only a direct hit mid-string matters. No
  TVS: the ring sits behind the enclosure window (estimate). Edges ≥ 1 µs: no snubber.
- **Second source:** MEIHUA MHP3838IR138CT (LCSC C409781), same ratings, 350 mW/sr typ; compare its
  land pattern first. **Moisture sensitive:** 168 h floor life after opening, else bake 60 °C / 24 h.

## Not measured

- **Boost loop stability.** If +32V rings after a pulse, scope TP13 and COMP; retune R62/C41/C39.
- **Current pulse** at TP14: 0.86–1.06 A, 8–10 µs rise. Ringing: R67/C42/R64; late sag: low corner.
- **τ vs Schmitt thresholds:** with STROBE held high, `LED_ON` (TP12) must fall in 0.78–1.32 ms; raise R55 if < 0.75 ms.
- **Ring intensity** against the 2.3 W/sr target. **Eye safety (IEC 62471)** at 500 µs, 6 % duty: rough
  estimate exempt group (average IR irradiance ≈ 7 W/m² at 20 cm, limit 100 W/m²); measure and compare.

## Sources

- SLVS789D: TI TPS61170 datasheet, rev D (Dec 2014)
- SLVSD76C: TI TPS22918 datasheet, rev C
- SCES416N: TI SN74LVC1G97 datasheet, rev N (Jan 2017)
- SBOS839N: TI TLV906x datasheet, rev N (Jul 2026)
- SBOA327A: TI Analog Engineer's Circuit "V-I converter circuit with MOSFET" (Aug 2021)
- IRLR024N DS: Infineon/IR IRLR024NPbF, PD-95081A
- Amicc DS: Amicc A-SC880IR3C, ADE-880-017 v1
- RVT DS: ROQANG RVT series (LCSC C72499 datasheet)
- FNR catalog: Changjiang FNR series power inductor catalog
- Walsin WW DS: Walsin WW12 current-sense resistor datasheet
- Murata NTC: Murata NTC thermistor catalogue, NCP15 series (LCSC C77131 PDF)
- P4 DS: Espressif ESP32-P4 Series Datasheet v0.7
