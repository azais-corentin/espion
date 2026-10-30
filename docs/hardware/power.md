# Power

This sheet makes `+5V` and `+3V3` from PoE or USB-C. An isolated PoE module (U2) makes 5.15 V from the cable and
a damped LC filter cleans it. A priority mux (U4, TPS2121) passes PoE when it is good and USB VBUS otherwise, as
`+5V`, which feeds the LED boost ([led.md](led.md)) and the 3.3 V buck (U3, TLV62569). The mux limits current at
every switchover, soft-starts, and blocks back-feed. Its status pin is `PWR_SRC`. Layout: see [layout.md](layout.md).

## PoE module

- SDAPO DP1435-5V: 802.3af PD, bridge rectifiers, isolated flyback (1.5 kV rms), 5.15 V out, class 0 (no class
  pin brought out). Through-hole SIP-6, standing 35.6 × 16.2 mm on the board, 14 mm tall. (DS p.1–2, p.5 item 9)
- It takes power from both centre taps and both spare pairs, so it works with Alternative A and B PSEs. Inputs are
  not polarity sensitive: no external diode or input cap. Bob-Smith is inside the MagJack ([ethernet.md](ethernet.md)).
- Output 5.15 V typ, 2 A typ / 2.6 A max. Line and load regulation 5 % each: 4.64–5.67 V if both stack,
  4.89–5.41 V with one. Ripple 200 mVpp at 250 kHz. (DS p.5 items 2–8)
- IEEE 802.3 §33.4.1 wants 1500 V rms for 60 s (or 2250 V dc, or 10 × 2400 V impulses; from summaries) between
  the cable and anything touchable. 802.3af gives ≈ 10.9 W out (12.95 W × 84 %), less than the module's 12.4 W.
- No maximum capacitive load is given. The module drives ≈ 680 µF directly (C11, C13, C14, C15) and the ≈ 124 µF on
  `+5V` through the mux's soft start (≈ 0.1 A); accepted.

## LC filter

- C11 10 µF at the module, L1 2.2 µH (FXL0630: 15 mΩ, I_sat 10.5 A, 5× the 2 A peak, 30 mV drop), then C13 330 µF
  polymer ∥ C14 330 µF electrolytic ∥ C15 10 µF; the module adds ≈ 220 µF of its own (estimate, DS p.6 photo). DS p.5
  asks for 0.33–3.3 µH and puts the 330 µF before L1; here it sits after L1, which gives more output capacitance
  and a lower peak (the DS order gives 100–172 mΩ).
- C14 damps the filter. With the polymer alone (5.9 kHz, Z₀ 82 mΩ, Q ≈ 2) the output impedance peaks at 151 mΩ
  and the 120 Hz strobe steps could make it ring. C14's ESR (0.1–0.64 Ω) brings the peak to 91–123 mΩ at
  4.1–5.0 kHz; with tolerances up to 141 mΩ (L1 +20 %) and 158 mΩ at ≈ 4.9 kHz (L1 +20 %, C13/C14 −20 %);
  nominal close to the Middlebrook optimum R_d ≈ 1.45 Z₀ ≈ 0.12 Ω. (JVE table; estimate)
- Stability: the converters behind it draw ≈ 10 W worst case and look like −V²/P ≈ −2.5 Ω, ≈ 20× the 123 mΩ peak
  (16× at 158 mΩ), which meets Middlebrook's |Z_out| ≪ |Z_in|. (Erickson & Maksimović)
- Load step: the boost starting at 1.44 A dips `+5V_POE_F` by ≈ 0.18 V (× 0.123 Ω), up to ≈ 0.23 V with
  tolerances (× 0.158 Ω). The module's own step response is not specified; the mux threshold keeps 0.07 V above
  this; accepted.
- Ripple at 250 kHz: −46 dB modelled, ≥ 30 dB with real ESL (estimate), so ≤ 6 mVpp. Model: ideal source;
  L1 + 15 mΩ; C13 + 25 mΩ; C15 ≈ 7 µF; C14 + ESR; mux ≈ 60 mΩ; `+5V` ≈ 124 µF (C22 + 30 mΩ).

## Mux priority threshold

- The mux switches from PoE (IN1) to USB (IN2) when PoE drops below 3.91 V (3.53–4.31 V worst case). It compares
  PR1 = IN1 × 47/147 (R18, R19; 0.315–0.324) with CP2 = `+3V3` × 60.4/160.4 (R20, R21) = 1.249 V (1.190–1.312 V
  over the buck's 3.20–3.44 V), ±40 mV offset, ±0.1 µA pin leakage. (TPS2121 DS Table 9-3 p.18, EC p.8)
- Upper margin 0.07 V: the lowest PoE output, 4.64 V, minus the 0.23 V load-step dip and 0.03 V in L1 is 4.38 V.
- Lower margin 0.07 V: the buck needs 3.39 V to regulate 0.78 A (3.3 V + 0.78 A × 115 mΩ), 3.46 V at IN1 after
  90 mΩ in U4. The 5 µs switchover adds a 0.08 V dip (2 A × 5 µs / 124 µF): at the lowest corner the buck input
  touches 3.38 V, 0.01 V short, for 5 µs; accepted. (TLV62569 DS §7.3.2 Eq. 1; TPS2121 DS Eq. 3–4 p.15)
- 60.4 kΩ balances the margins; 62 kΩ gives 3.61–4.35 V and only 0.03 V at the top.
- CP2 above V_REF (1.10 V max) selects XREF mode and the 5 µs fast switchover; with CP2 grounded it takes 100 µs,
  a 1.6 V dip that drops `+3V3` out. Until `+3V3` passes 2.65–2.96 V the mux runs in VREF mode (IN1 while above
  ≈ 3.1–3.5 V, else the higher input, 100 µs switchover). Only at start-up with both inputs present; accepted.
- **Intentional deviation from TI Fig. 10-12** (DS p.27), which takes CP2 from a REF3325 2.5 V reference. Using
  `+3V3` saves that part and its rail, at the cost of the VREF-mode start-up and `+3V3` tolerance in the window.
- **No hysteresis resistor.** TI's example adds 1 MΩ from PR1 to ST; the comparator has none of its own; the margins
  leave no room. Accepted consequence: on a PoE fault (overload or hiccup; the normal ≤ 2 A load is within the
  module's rating) with USB present, the mux cycles PoE ↔ USB; every handover is current-limited.

## Switchover

- **PoE lost, USB present.** 2 A drains the ≈ 790 µF on IN1 and `+5V` at 2.5 V/ms; 0.3–0.7 ms after PoE stops,
  `+5V` has sagged with IN1 to the threshold, then USB turns on at once under the current limit (§9.3.7 p.16),
  charging the caps with ≤ 3.5 A (4.5 A rated; 0.5–1.5 A at 2 A load; ≈ 0.7 A slew-limited per TI Fig. 10-8,
  estimate), and `+5V` climbs back to VBUS without falling further. The 6.0–8.4 A fast trip (25 ms off, board
  reset) is far away; accepted (§9.3.3 p.14). Firmware: [firmware rule 4](firmware-rules.md).
- **PoE plugged in on USB.** The mux keeps USB through the settling time t_SET (C21), then compares. The module
  may still be starting (start-up time not specified), so the mux can toggle until PoE is stable. Handovers are
  current-limited and the bulk holds `+5V` at about the threshold (≥ 3.53 V; estimate, Fig. 9-9). `PWR_SRC` can
  chatter: [firmware rule 4](firmware-rules.md). (§9.3.1 pp.12–13)
- **USB plugged in on PoE.** The mux waits t_SET, keeps PoE, skips a second soft start; only C16 charges.
- **USB absent.** IN1 powers `+5V` whatever PR1 reads, so there is no PoE undervoltage lockout beyond the mux's
  2.5–2.8 V UVLO (rising); `+5V` rises at the soft-start slew. The buck's UVLO and the P4's EN RC handle it; accepted.

## Mux limits, back-feed, heat, ratings

- R22 = 36 kΩ: I_LM = 65.2 / R^0.861 = 2.98 A typ, ≈ 2.5–3.5 A (estimate, between the EC rows), above the 2.0 A
  peak load. OV1 and OV2 are grounded: no overvoltage cut-off on either input. (DS Eq. 2 p.13, p.4, p.8)
- The off input leaks ≤ ±1 µA at 25 °C, ≤ ±35 µA at 85 °C (DS p.7). Open VBUS on PoE floats to ≤ 1.9 V at 85 °C
  into the ≈ 55 kΩ of the R24 bleeder and the sense divider: `VBUS_SENSE` ≤ 1.2 V still reads "no USB". That is
  above Type-C vSafe0V (0.8 V) at the hot corner; accepted, the board runs far cooler (estimate).
- Open PoE input on USB: IN1 stays off below 3.53 V, which holds while the unpowered module loads its output
  below ≈ 320 kΩ (≈ 130 kΩ keeps it under the UVLO's 2.4 V falling minimum); a flyback output loads far more (estimate).
- A channel pushed below `+5V` turns off within 10 µs and back on within 50 mV; C20 and C23 carry it (§9.3.6).
- R_ON ≤ 90 mΩ: +26 °C at the 2 A peak (0.36 W × 72.2 °C/W), +6.5 °C at the 1 A average (p.6–7).
- TPS2121 inputs take 2.8–22 V and its pins ≤ 5.5 V (PR1 ≤ 1.84 V, CP2 ≤ 1.31 V; dividers 35 µA and 21 µA).
  The TLV62569's VIN and EN (tied) are rated 5.5 V, 6 V abs max: the module's 5.67 V stacked-band case, passed at
  light load, is 0.33 V under abs max; accepted. No other part here is outside its recommended input. (§6.1, §6.3)
- `+5V` holds ≈ 124 µF effective at 5 V (C22 100 µF polymer, C20 47 µF → 18 µF, C10 → 5 µF, small caps), ≥ 97 µF
  worst case. The TPS2121 sets no minimum. C22 sees ≤ 0.37 A rms of buck ripple, < 15 % of its rating.
- C16 4.7 µF on VBUS keeps USB 2.0's 10 µF attach limit (§7.2.4.1); it is the only cap charged without soft start.
- Soft start (USB attach, or PoE alone): C21 100 nF sets 780 V/s, so the caps draw ≈ 0.10 A for ≈ 6.4 ms (Table
  9-1). That is 12× the charge of 10 µF; accepted. The boost's 440 µF waits behind its load switch.
- Footprints: C14 takes `Capacitor_SMD_JIERR:CP_Elec_8x10.5` (the stock one of that name differs); U4 is a project part.

## 3.3 V buck

- R16 453 kΩ / R17 100 kΩ give 3.318 V; 3.20–3.44 V worst case (3.19–3.45 V with Fig. 3's drift): ≈ 25 mV under
  the PHY's 3.465 V (≈ 15 mV with drift, less the unquantified power-save lift); accepted. The P4 takes 3.0–3.6 V.
  C17 6.8 pF is TI's feed-forward for a 100 kΩ bottom resistor. (DS EC, §8.2.2.2; IP101G Table 11)
- EN tied to VIN (TI Fig. 5), never floating; no RC: the chip soft-starts in 0.8 ms and the P4 has its own EN RC.
  No power-good output on the SOT-23 part. ≈ 0.29 W loss at ≈ 90 % (estimate) × 188.2 °C/W = +54 °C: give it copper.
- L2 is the same 2.2 µH FXL0630: 0.36 A ripple, 1.00 A peak at 0.78 A, 10× under I_sat; the 3 A min current limit
  trips first. Core loss at 1.5 MHz is unpublished and taken as small (estimate).
- C18 10 µF (≈ 7.5 µF at 3.3 V, estimate): ≈ 4 mV ripple. C10 10 µF at the input; TI asks 4.7 µF. All of `+3V3`
  is ≈ 43 µF effective, in the upper half of TI's 10–47 µF. Accepted: most sits behind trace inductance
  (estimate), which is why C8 and C29 are only 4.7 µF ([mcu.md](mcu.md), [ethernet.md](ethernet.md)).

## Power budget

- `+3V3` ≈ 0.78 A, 39 % of the buck's 2 A: P4 module 500 mA (Espressif's required supply; 150 mA measured, P4 DS
  Tables 5-2, 5-7; Waveshare gives none), PHY 45, camera 150 (estimate from the brief's 156 mW; [camera.md](camera.md)'s
  ≤ 250 mA is a conservative trace-sizing bound), status LED 60, logic 10, jack LEDs 14 mA.
- `+5V`: buck 0.57 A; boost 0.41 A average, 0.8–1.3 A while recharging `+32V` (50–120 ms), at most its 1.44 A switch
  limit. **Average ≈ 1.0 A (5.0 W), peak ≈ 2.0 A (10.3 W)**: within the module's 2.6 A max, the mux limit and 802.3af.
- USB only: the LED rail is off: ≤ 0.57 A by budget, ≈ 0.25 A realistic, against a 500 mA port; accepted (bench).

## PWR_SRC

High = PoE powers `+5V`, low = USB. Also high with both inputs gone, when the P4 is off. It cannot tell whether
USB is present on PoE. Open drain, pulled up by R29 (10 kΩ to `+3V3`, mcu sheet) so no 5 V reaches the GPIO;
ST wants 6–20 kΩ (DS p.6, Table 9-3). Used by [firmware rules](firmware-rules.md) 4 and 10.

## Accepted risks

- No hysteresis (PoE ↔ USB cycling on a PoE fault); thin margins (0.07 V, 0.07 V; buck 0.01 V short for 5 µs).
- CP2 from `+3V3` (deviation from TI Fig. 10-12); no PoE undervoltage lockout without USB.
- TLV62569 at 5.67 V; `+3V3` 25 mV under the PHY maximum; `+3V3` caps near TI's limit; VBUS at 1.9 V hot.
- Not measured: module start-up and step response (watch `+5V_POE_F` and `PWR_SRC` when PoE arrives on USB and
  when the boost starts; ringing at 4–6 kHz means C14 damps too little), buck temperature, `+3V3` at light load.

## Sources

- SDAPO DP1435-5V datasheet (LCSC C5380097, PDF 7fde6577…). TI TPS2121 SLVSEA3F (Aug 2020, PDF page numbers).
- TI TLV62569 SLVSDG1C (Oct 2017); TI TPS61170 SLVS789D; ESP32-P4 DS v0.7; IP101G-DS-R01; OV9281 brief v1.4.
- cjiang FXL0630 (LCSC PDF 40dfb6cb…); AISHI VZ (aabe05a1…); JIERR JVE (3ad662ff…), MA (3f7d3c65…); Samsung DC-bias.
- IEEE 802.3 §33.4.1, USB 2.0 §7.2.4.1 (from summaries); Erickson & Maksimović, *Fundamentals of Power Electronics*.
