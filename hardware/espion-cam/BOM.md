# espion-cam BOM

Selected parts for the camera board, with their LCSC numbers. Stock and prices were checked
on the LCSC product pages on **2026-10-28**. Check them again before you order. The prices
are LCSC's lowest price break.

Generic passives (decoupling, pull-ups, PHY straps, buck input/output caps, boost feedback
and compensation, the LED timeout RC) are chosen during schematic capture and are not
listed here.

## Sourced elsewhere

| Ref | Part | Source | Notes |
|---|---|---|---|
| — | Waveshare ESP32-P4-Core, 16 MB flash (SKU 36096) | Waveshare | See `../../docs/architecture.md` |
| — | OV9281 camera module (SZYTF, 79°/110°/160° lens) | [AliExpress 1005012566040263](https://fr.aliexpress.com/item/1005012566040263.html) | 15-pin 1.0 mm Raspberry Pi camera FPC; XVS, STROBE, IL_PWM, GPIO2, 3V3, GND on 2.54 mm pads on the back. Ships with a 15→15 FFC and a 15→22 FPC |

## LCSC

| Block | Qty | MPN | Manufacturer | LCSC | Package | Stock | Unit price |
|---|---|---|---|---|---|---|---|
| PoE PD module | 1 | DP1435-5V | SDAPO | C5380097 | SIP-6 THT | 365 | $3.64 |
| RJ45 MagJack | 1 | HR871150C | HanRun | C19724786 | THT, right angle | 306 | $1.16 |
| Ethernet PHY | 1 | IP101GRI | IC Plus | C703537 | QFN-32 4×4 | 13,529 | $0.49 |
| PHY crystal, 25 MHz | 1 | X322525MOB4SI | YXC | C9006 | 3225 | 60,945 | $0.049 |
| Crystal load caps, 18 pF C0G | 2 | 0402CG180J500NT | FH | C1549 | 0402 | 876,700 | $0.0012 |
| 5 V OR-ing (power mux) | 1 | TPS2116DRLR | TI | C3235557 | SOT-583 | 54,819 | $0.19 |
| 3.3 V buck | 1 | TLV62569DBVR | TI | C141836 | SOT-23-5 | 16,700 | $0.038 |
| Buck inductor, 2.2 µH | 1 | FNR3015S2R2MT | cjiang | C167747 | 3×3 | 57,660 | $0.036 |
| PoE LC filter inductor, 2.2 µH | 1 | FXL0630-2R2-M | cjiang | C167218 | 7×6.6 | 70,530 | $0.083 |
| PoE LC filter, 330 µF/16 V polymer | 1 | SVZ1CM331E09E00RAXXX | AISHI | C3291468 | SMD 6.3×9 | 3,055 | $0.093 |
| PoE LC filter, 330 µF/16 V electrolytic (damping) | 1 | JVE16V330M8X10 | JIERR | C48615108 | SMD 8×10.5 | 1,935 | $0.057 |
| PoE LC filter, 10 µF/25 V X5R | 1 | CL21A106KAYNNNE | Samsung | C15850 | 0805 | 1,905,660 | $0.026 |
| USB-C receptacle | 1 | TYPE-C-31-M-12 | Korean Hroparts | C165948 | SMD, 16-pin | 283,635 | $0.098 |
| USB-C CC pull-downs, 5.1 kΩ | 2 | 0402WGF5101TCE | Uni-Royal | C25905 | 0402 | 1,113,900 | $0.0012 |
| USB ESD (D+/D−, VBUS) | 1 | USBLC6-2SC6 | ST | C7519 | SOT-23-6 | 39,350 | $0.093 |
| BOOT and RESET buttons | 2 | TS-1187A-B-A-B | XKB | C318884 | SMD 5.1×5.1 | 887,760 | $0.011 |
| Camera FPC connector, 15-pin 1.0 mm | 1 | F1004-H-15-20G-R | XFCN | C22438965 | SMD, rear flip, dual contact | 2,230 | $0.092 |
| Camera aux connector (XVS, STROBE), JST-SH 4-pin | 1 | SM04B-SRSS-TB(LF)(SN) | JST | C160404 | SMD, right angle | 60,695 | $0.13 |
| Camera aux pigtail, SH 1.0 mm 4-pin, 200 mm, single-ended tinned | 1 | LNN100-157128-200-4P | DEALON | C2999003 | Cable assembly | 1,740 | $0.057 |
| LED ring, 850 nm, 90° | 12 | A-SC880IR3C-P03-1T | Amicc | C5355470 | SMD 3.85×3.85 | 885 | $0.54 |
| LED boost, 5 V → 32 V | 1 | TPS61170DRVR | TI | C15163 | WSON-6 2×2 | 45,173 | $0.42 |
| LED boost inductor, 10 µH, 1.6 A sat | 1 | FNR4030S100MT | cjiang | C167879 | 4×4 | 148,730 | $0.039 |
| LED boost diode, Schottky 60 V 1 A | 1 | SS16 | MDD | C2481 | SMA | 256,250 | $0.0097 |
| LED storage cap, 220 µF/50 V electrolytic | 2 | RVT1H221M1010 | ROQANG | C72499 | SMD 10×10.2 | 101,025 | $0.061 |
| LED sink op-amp, 10 MHz RRIO | 1 | TLV9061IDBVR | TI | C398358 | SOT-23-5 | 265,180 | $0.048 |
| LED sink MOSFET, 55 V planar, logic level | 1 | IRLR024NTRPBF | Infineon | C3007 | DPAK | 29,900 | $0.12 |
| LED sink shunt, 0.1 Ω 1 % 250 mW | 1 | 1206W4F100LT5E | Uni-Royal | C25334 | 1206 | 271,050 | $0.0048 |
| LED gate, AND with Schmitt inputs (STROBE ∧ LED_EN, ≈ 1 ms timeout) | 1 | SN74LVC1G97DBVR | TI | C128411 | SOT-23-6 | 21,610 | $0.064 |

## Notes

- **MagJack stock is low** (306). It is the only in-stock LCSC jack that brings out both
  centre taps (P7/P8 → DP1435 VA1/VA2) and the spare pairs (P9/P10 → VB1/VB2), with the
  Bob-Smith termination inside, so the board needs no termination parts. Buy spares.
- **Crystal**: the IP101GRI datasheet asks for ±50 ppm and a 20 pF load "or specify". C9006
  is a 12 pF crystal; 18 pF caps assume ≈ 3 pF of stray capacitance. Tune them on the
  first boards.
- **OR-ing**: PoE on TPS2116 VIN1, priority set with the PR1 divider. Both inputs must stay
  under 5.5 V.
- **LC filter**: the polymer cap alone barely damps the 2.2 µH / 330 µF filter (≈ 5.9 kHz),
  and the 120 Hz strobe current steps could make it ring; the electrolytic's ESR damps it.
- **Camera pigtail**: DEALON's housing is a SH 1.0 mm clone. Check that it mates with the
  JST header on the first order. Solder the tinned ends to the module's XVS, STROBE and GND
  pads (one spare wire).
- **LEDs**: 885 in stock is enough for 73 boards. Second source with the same ratings and
  chip size: MEIHUA MHP3838IR138CT (C409781, 350 mW/sr typ, 1,340 in stock); compare its
  land pattern before treating it as a drop-in. Moisture sensitive: 168 h floor life
  after the bag is opened. Sizing: `../../docs/architecture.md` (LED ring).
- **LED driver**: design and sizing in `../../docs/architecture.md` (LED ring). The
  storage caps are two in parallel because one RVT 220 µF is rated 230 mA ripple at
  120 Hz, at the edge of the ≈ 0.24 A RMS that 1 A pulses at 6 % duty draw. The MOSFET is
  a planar part for its linear-mode SOA (9–13 W for 500 µs per pulse). NCE6020AK (C108639,
  60 V trench, TO-252, 39,040 in stock) is a fallback only after its SOA curve is checked:
  trench parts often derate badly in linear mode.
  The TPS61170's WSON-6 has an exposed pad: solder it with paste and hot air.

## Not selected yet

- 850 nm bandpass filter for the camera lens (not a PCB part).
