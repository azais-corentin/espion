# Ethernet

The PHY (U1, IC Plus IP101GRI) turns the P4's RMII bus into 10/100 Mb/s twisted pair. It runs from a 25 MHz crystal (Y1) and makes the 50 MHz RMII clock itself. The MagJack (J1, HanRun HR871150C) holds the transformers, chokes, Bob-Smith termination and two LEDs, and hands the PoE pairs to the PD ([power.md](power.md)). Both reference boards, Espressif's ESP32-P4-Function-EV-Board ([EV]) and Waveshare's ESP32-P4-ETH ([WS]), use the IP101GRI, so their straps and example configs carry over. Layout: see [layout.md](layout.md).

## Straps

All straps latch on the rising edge of RESET_N (setup 10 µs, hold 2.63 ms) and use the datasheet's 5.1 kΩ latch resistor. A 5.1 kΩ pull-up against the internal 104 kΩ pull-down gives 3.15 V, above V_IH min 2.64 V. (DS Tables 12, 13, 15)

- **RMII, PHY drives REF_CLK:** COL/RMII pulled up (R10). With a crystal on X1/X2 this selects RMII and puts 50 MHz out on pin 14. The internal pull-down would select MII. (DS §5.1.4, Table 3)
- **PHY address 1:** AD0 pulled up (R1), AD3 pulled down (R7) so it never floats; AD3 = 1 with AD0 = 0 would answer every address 2–31. Address 1 is what both reference boards strap, so their configs work unchanged; esp-hal `GenericPhy::new_auto()` and ESP-IDF `ethernet_init` (address −1) scan anyway. Avoiding 0 sidesteps PHYs that treat 0 as broadcast (the DS doesn't say whether the IP101G does). Firmware can move it via P16R29[12:8]. (DS Table 4, p.28)
- **LED mode 2:** CRS/LEDMOD pulled up (R9), as on [EV] (5.1 kΩ, its R155); see Jack LEDs. 5.1 kΩ like the other straps instead of the 4.7 kΩ in DS §5.4; both swamp the 104 kΩ. R9 draws 0.65 mA when CRS drives low.
- **FX_HEN = 0 (twisted pair):** set only by the internal 104 kΩ pull-down on pin 19, which shares the `RMII_CRS_DV` net with GPIO28. No external part, as on both reference boards. GPIO28 has no pull at reset; firmware must not add one before the strap latches ([firmware rule 3](firmware-rules.md)).
- `RMII_TXD1` (GPIO35) is the P4's BOOT strap, so it gets no pull-down here ([mcu.md](mcu.md)).

## PHY pins worth knowing

- TXER (pin 1) to GND: in RMII it must float or go to GND (DS p.16).
- TXD2/TXD3 (pins 6, 7) to GND: unused in RMII and they have no internal pull. Deviation: both reference boards leave them open.
- RXER (pin 21) unconnected: the P4's RMII_RXER pin (GPIO31) is used as MDC. Pin 21 is not a strap (DS p.17), though [EV] marks it like one. A PHY interrupt would need another GPIO.
- ISET 6.19 kΩ 1 % (R6) sets the transmit current (DS §5.8); [EV] uses 6.19 kΩ, [WS] 6.2 kΩ.
- The MDI pairs need no termination or bias parts: the 49.9 Ω resistors are inside the IC (DS p.1, Table 1).

## Reference clock and RMII lines

- Pin 14 (50 MHz out) → R13 (22 Ω) → star point → GPIO50 (`GMAC_RMII_CLK_PAD`) and back into the PHY's own 50M_CLKI (pin 10), which needs the clock even when the PHY makes it. Same as both reference boards. In esp-hal: `ExternalRefClock::new(GPIO50)`. (DS p.16, Fig. 8)
- Exactly two loads. A third load on `RMII_REF_CLK` needs a buffer (DS Fig. 10 p.45).
- 22 Ω source resistors on every RMII line, each at its driver. PHY-driven, on this sheet: R13 REF_CLK, R15 CRS_DV, R11 RXD0, R12 RXD1; ESP32-P4 HDG §1.3.14 recommends one on the clock. P4-driven, on the MCU sheet at the P4 pins: R37 TX_EN, R35 TXD0, R36 TXD1 ([mcu.md](mcu.md)). 22 Ω ≈ 50 Ω trace − 25–30 Ω pad output (estimate: neither datasheet gives an output impedance). [EV] fits 0 Ω in all these places, [WS] nothing. 0 Ω also works on short traces; keep the footprints. PHY drive is adjustable (P16R26/27), and so is the P4 pad drive.
- The TX resistors damp overshoot into the PHY's TXEN/TXD inputs (abs max 3.63 V, DS p.57) and delay TX data by ≈ 0.1 ns (22 Ω × ≈ 5 pF, estimate), small against the PHY's 4 ns setup and 2 ns hold. MDC and MDIO get no resistor: both run at ≤ 2.5 MHz.
- Timing: the PHY drives RXD/CRS_DV 6–13 ns after its clock edge, so the P4 sees ≈ 20 − 13 − t_flight ≈ 6 ns setup and ≥ 6 ns hold, provided both clock branches are equal length. RMII 1.2 asks 4 ns setup, 2 ns hold (estimate: spec not re-read); the P4 EMAC's own figure wasn't found. TX: the PHY needs 4 ns setup, 2 ns hold. (DS Tables 18, 19)

## Management and reset

- **MDIO pull-up 1.5 kΩ (R14)** to `+3V3`. The PHY's own is only 222 kΩ (DS p.12) and MDIO idles high through turnaround. 1.5 kΩ is the 802.3 Clause 22 value (22.4.4.2) and what both reference boards fit. Rise time ≈ 50 ns (estimate, ~15 pF), far under the 300 ns minimum MDC period (DS p.16; 802.3 asks 400 ns); 2.2 mA when driven low. MDC has no pull.
- **PHY_RST_N pull-down 10 kΩ (R8), test point TP1, no RC.** GPIO51 floats until firmware runs; 10 kΩ against the internal 222 kΩ pull-up holds the pin at 0.14 V, below V_RST 1.32 V (DS Table 12). So the PHY stays in reset while the P4 boots: the straps latch only after power and crystal are stable, and no clock or RX data reaches the P4 early. Firmware sequence: [firmware rule 3](firmware-rules.md); R8 is also why TX_EN, TXD0 and MDC need no pulls ([firmware rule 9](firmware-rules.md)). An RC would add parts for nothing. Deviation: [EV] fits neither pull and its PHY leaves reset during the power ramp. R8 draws 0.33 mA while GPIO51 drives the line high.

## Supply

- AVDD33 and VDDIO run straight from `+3V3`: 3.135–3.465 V, and VDDIO matches the P4's 3.3 V RMII I/O. The reference boards' 0 Ω link and `PHY_3V3` rail were dropped (827851f); the DS only asks for careful decoupling of AVDD33 (p.18). One net, so the 30 ms start-up skew limit is met by design. About 150 mW, 45 mA. (DS Tables 1, 11, 15)
- Decoupling: 100 nF at each pin (C9 VDDIO, C7 AVDD33) and one shared 4.7 µF (C8). Deviation: the reference boards fit 10 µF per pin; this keeps the total on the buck output down, and the `+3V3` bulk is a few cm away ([power.md](power.md)).
- REGOUT (1.1 V, internal only) gets 10 µF + 100 nF (C4, C6) and no load (DS p.18, Table 1 note 1). The 10 µF keeps nearly its full value at 1.1 V (estimate, X5R bias curves).

## Crystal

- Y1: Epson TSX-3225 X1E0000210139, 25 MHz, CL 12 pF, ESR ≤ 40 Ω, ±10 ppm / ±20 ppm over temperature, C0 1.39 pF, drive 200 µW max. The PHY needs ≤ 40 Ω ESR, ±50 ppm / ±50 ppm, ≤ 7 pF shunt, 100 µW drive; load "20 pF or specify" (DS Table 14). RMII allows ±50 ppm (DS Tables 18, 19).
- Why not the earlier YXC X322525MOB4SI: 50 Ω max ESR, out of spec. Every YXC 3225 25 MHz part and every JLC basic/preferred 25 MHz crystal is the same; of 15 parts surveyed, Epson was the only name brand at 40 Ω. 2016-size parts are 60–150 Ω or barely stocked.
- Second source: SCTF SX3B25.000F1210F30 (CL 12 pF, 30 Ω, ±10/±30 ppm), same pins and land. Its 100 µW maximum drive equals the PHY's typical drive and it gives no motional values: use it only if the Epson part is short.
- LCSC listings get crystal ESR wrong (KYX: listed 30 Ω, datasheet 40 Ω). Check the datasheet.
- **Load caps 18 pF C0G (C2, C5):** C = 2 × (C_L − C_stray) = 2 × (12 − 3) pF, with C_stray ≈ 3 pF of pin and trace (estimate). A 2 pF stray would want 20 pF, 5 pF 14 pF. The crystal pulls ≈ 10.5 ppm per pF of load error, so 3 ± 1.5 pF gives up to ≈ ±50 ppm worst case (≈ ±28 ppm RSS). Measure the frequency on the first boards and tune C2/C5.
- Y1 pins 2 and 4 are the metal lid and go to GND (Epson p.1 §3).

## MagJack

- J1 is the only in-stock LCSC jack that brings out both line-side centre taps (to PD VA1/VA2) and the spare pairs 4/5 and 7/8 (to VB1/VB2), with Bob-Smith inside. LCSC stock is low: buy spares.
- Inside, the four line-side nodes each go through 75 Ω + 22 nF/100 V to a common node, coupled to the shell by 1000 pF/2 kV (HR p.1). So: no termination parts on the board; the shield must not float or the termination floats with it; the 100 V capacitors clear the 57 V PoE maximum.
- Chip-side centre tap (pin 3): 100 nF (C3) to GND only, never to a supply. The IP101G has a voltage-mode driver with internal termination: "Do not connect to any power" (DS Table 1, §6.2). The older IP101A and current-mode PHYs differ. Both reference boards do the same.
- Isolation cable to board: 2250 V DC for 60 s; 1500 V rms per 802.3 (HR p.1–2). The jack has no pin 6. Auto-MDIX handles swapped pairs.

## Shield

- Shell → C1 (1 nF, 2 kV) ∥ R2 + R3 (2 × 499 kΩ ≈ 1 MΩ) → GND. The resistors see the capacitor's voltage. A plain 1206 is rated about 200 V (estimate). YAGEO RV1206 (1/4 W) is rated √(P·R) working, up to 500 V, and 2.5× that for 5 s, up to 1 kV: at 499 kΩ that is 353 V working, 883 V overload each, so the pair takes ≈ 706 V working, ≈ 1.77 kV for 5 s. That is below C1's 2 kV and not a 60 s hipot rating. (RV DS Table 2 p.5, p.6, p.8)
- In use the shell sits near 0 V; the ratings matter for surge, ESD and hipot. One RV1206FR-071ML (1 MΩ: 500 V working, 1 kV overload) is acceptable if space is short.
- At high frequency the cable's common-mode path to GND is the jack's 1 nF in series with C1, about 500 pF.

## Jack LEDs

- Green = link/activity (LED0, pin 11: on with link, blinks 26/78 ms on traffic at 10 and 100 Mb/s). Yellow = 100 Mb/s link (LED3, pin 12). That is LED mode 2; default mode 1 shows activity only at 100 Mb/s. (DS §5.4 Tables 8, 9)
- Polarity follows the address strap: a pin strapped 1 sinks its LED, a pin strapped 0 sources it (DS Fig. 12). So green has its anode on `+3V3` and pin 11 sinks it; pin 12 drives the yellow anode, cathode to GND. The active level comes from the figure, not the text: check on the first boards that the LEDs light with link, not without.
- The straps still latch: the LED pins are Hi-Z in reset, each LED path pulls the same way as its strap resistor, and neither LED has voltage across it.
- Green anode on `+3V3`, the PHY's own supply. Same voltage, so the LED stays dark while pin 11 is high.
- 220 Ω (R4, R5). The pad limits current to ≈ 1.6 mA at the default LED_DRIVE (estimate: specified only at 0.4 V, DS §4.6). Upper bound from 220 Ω alone: (3.3 − 1.8) / 220 = 6.8 mA, inside the 20 mA LED and pad ratings. At the Vf corner of 2.8 V: (3.3 − 2.8 − 0.4 V on the pad) / 220 = 0.45 mA (the DS's 510 Ω would give 0.2 mA). To brighten, firmware writes LED_DRIVE (P16R28[11:9]) to 3'b010–3'b011 (4.86–6.48 mA) after every PHY reset; not required.
- The PHY can't turn the LEDs off, and the OV9281 sees green and yellow. If they show up in another camera's image, leave R4/R5 unfitted; the straps keep working.

## Sources

- DS: IC Plus IP101G datasheet IP101G-DS-R01 (LCSC PDF, see `hardware/lib/README.md`)
- HR: HanRun HR871150C datasheet Rev 00, pp.1–4 (LCSC PDF, see `hardware/lib/README.md`)
- Epson: TSX-3225 X1E0000210139 part sheet, download.epsondevice.com, p.1 §1–§4
- SCTF 3X series crystal datasheet (LCSC PDF, image only), pp.1–2
- EV: Espressif ESP32-P4-Function-EV-Board schematic v1.5.2, sheets 2 and 5
- WS: Waveshare ESP32-P4-ETH schematic, p.1
- ESP32-P4 datasheet (IO MUX table) and ESP32-P4 Hardware Design Guidelines §1.3.14
- RV DS: YAGEO RV series datasheet (PYU-RV_51) version 14, Table 2 p.5, p.6, p.8; IEEE 802.3u-1995 Clause 22
- esp-hal 1.2.2 (`src/ethernet/`), esp-eth-drivers `ethernet_init`
