# Ethernet

10/100 Ethernet for the P4's EMAC: an IP101GRI PHY (U1) on RMII, and a HanRun HR871150C MagJack
(J1) that also hands both PoE pair sets to the PD module ([power](power.md#poe-input)). The PHY
supplies the 50 MHz RMII clock.

| Signal | U6 pin | GPIO | Use |
|---|---|---|---|
| RMII_CRS_DV | U6.40 | GPIO28 | RMII receive data valid (PHY strap FX_HEN) |
| RMII_RXD0 / RXD1 | U6.41 / U6.42 | GPIO29 / GPIO30 | RMII receive data |
| RMII_TXD0_MCU / TXD1_MCU | U6.46 / U6.47 | GPIO34 / GPIO35 | RMII transmit data (P4 straps) |
| RMII_TX_EN_MCU | U6.67 | GPIO49 | RMII transmit enable |
| RMII_REF_CLK | U6.68 | GPIO50 | 50 MHz clock input from the PHY |
| MDC / MDIO | U6.43 / U6.70 | GPIO31 / GPIO52 | PHY management (GPIO matrix) |
| PHY_RST_N | U6.18 | GPIO22 | PHY reset, active low |

Firmware use of these pins and the PHY: [firmware-rules](firmware-rules.md#gpio-map),
[rules](firmware-rules.md#rules).

## PHY

U1 is an IC Plus IP101GRI (LCSC C703537, QFN-32 4 × 4 mm, 0.4 mm pitch), on the top side next to J1.
The source is the IP101G datasheet IP101G-DS-R01 (2014-05-20), cited below by page. VDDIO (pin 13) and
AVDD33 (pin 31) are on +3V3, so the 3.3 V I/O and timing rows apply. The PHY and the P4's I/O share
one rail, so neither can back-power the other.

- **Part choice.** IC Plus publishes no MSL or reflow rating for the IP101GRI. JLCPCB lists MSL 3, but
  that is distributor data. The part is kept, and the missing maker statement is an accepted risk
  (user decision, 2026-10-31: "Keep IP101GRI, accept risk"). There is no drop-in alternative, and a
  0.4 mm QFN can't be hand-soldered reliably.
- **Decoupling.** C7 (100 nF) sits at AVDD33 and C9 (100 nF) at VDDIO, with C8 (4.7 µF) as the local
  bulk capacitor. The datasheet sets no values here, only "decoupled carefully" (p.18) and "as close
  as possible" (p.56). The exposed pad (pin 33, ground) carries four GND vias.
- **Unused pins.** TXD2/TXD3 (pins 6/7) and TXER (pin 1) go to GND (p.16). RXD2/RXD3 and RXER/INTR_32
  are outputs and stay open. TEST_ON (pin 24) stays open on its internal pull-down (p.18).

### Crystal

Y1 is a 25 MHz Epson TSX-3225 X1E0000210139 (LCSC C91750). RMII with a crystal takes 25 MHz (Table 3,
p.45).

| Item | Y1 (Epson p.1) | IP101G needs (Table 14, p.58) |
|---|---|---|
| Load capacitance C_L | 12 pF | — |
| ESR | ≤ 40 Ω | ≤ 40 Ω |
| Drive level | 10–100–200 µW | 100 µW typ |
| C0 / C1 | 1.39 pF / 3.78 fF | C0 ≤ 7 pF |
| Tolerance | ±10 ppm at 25 °C, ±20 ppm over −40…+85 °C | ±50 ppm |

Load capacitors C2 = C5 = 18 pF C0G ±5 % (FH 0402CG180J500NT, LCSC C1549):

- C = 2 × (C_L − C_stray) = 2 × (12 − 3) pF = 18 pF. C_stray = 3 pF is assumed for the pin and trace,
  because the IP101G gives no pin capacitance.
- Pulling: Δf/f = (C1/2) × (1/(C0 + C_L,a) − 1/(C0 + C_L,b)), about 10.5 ppm/pF at 12 pF. For C_stray
  between 2 and 5 pF, the effective load is 11–14 pF, which pulls the frequency by +11 / −18 ppm.
- Worst sum: 18 ppm pull + 10 ppm + 20 ppm = 48 ppm, within the PHY's and RMII's ±50 ppm.

Placement:

- XTAL_X2 runs 3.0 mm from U1.3 straight to Y1.3. XTAL_X1 (8.7 mm) loops over the crystal to Y1.1.
  At 25 MHz both lines are electrically short.
- Y1 is 1.0 mm from U1.
- There is no GND guard trace round the crystal. The datasheet asks for one "if possible" (§6.1 p.56),
  and here it isn't possible. The crystal relies on distance and the In1/In2 GND planes. The nearest
  other copper is a +32V track 0.45 mm from C2's X1 pad, and the LED switch Q1's tab is 2.35 mm from
  Y1 ([layout](layout.md#ethernet-and-poe-isolation)).

### REGOUT and ISET

- **REGOUT** (pin 28) is the PHY's internal 1.1 V regulator output, 1.07–1.18 V (p.57). C4 (10 µF X5R
  25 V) and C6 (100 nF) decouple it, as the datasheet asks ("A 10uF and 0.1uF should be connected",
  p.18).
- **ISET** (pin 25) sets the transmit amplitude. R6 is 6.19 kΩ 1 % to GND, the datasheet value (p.53).
- Both pins sit 0.2 mm from the MDI pads at the 0.4 mm pitch, so their tracks leave between the MDI
  pairs (see [MagJack and MDI](#magjack-and-mdi)).

### Reset

- PHY_RST_N runs from GPIO22 (U6.18) to RESET_N (U1.32). R8, 10.0 kΩ to GND, holds it low.
  - RESET_N has an internal 222 kΩ pull-up (p.12). With R8, at the top of the +3V3 ±5 % envelope:
    3.465 V × 10/232 = 0.149 V, under VRST (0.4 × VDDIO ≈ 1.25 V, p.57).
  - GPIO22 has no pull and its input is disabled at and after reset (ESP32-P4 datasheet v0.7 p.15).
  - So the PHY stays in reset through the whole P4 boot, far longer than the 10 ms minimum (p.54),
    with the crystal already running.
- TP1 (silkscreen "PRST") is on the net.
- **Firmware order.** While in reset, the PHY drives no 50 MHz clock (50M_CLKO is Hi-Z), and the
  EMAC has no other clock. So the firmware releases PHY_RST_N and waits ≥ 2.5 ms before it touches the
  EMAC or MDIO ([firmware-rules](firmware-rules.md#rules)).
- PHY_RST_N is 75.4 mm long with 5 vias. It runs round the lens inside the LED ring, partly
  0.5–1.1 mm from the ring pads. Once driven high it is a low-impedance push-pull line. A first-board
  check confirms the LED pulses don't disturb it.

## MagJack and MDI

### J1

J1 is a HanRun HR871150C (LCSC C19724786): an RJ45 with magnetics (1CT:1CT), PoE centre taps
brought out, Bob-Smith termination, two LEDs and a shield. It is a through-hole part on the bottom
side at the east edge, soldered by hand ([layout](layout.md#assembly)).

- **Isolation.** HanRun rates the jack at 2250 V dc from the cable side to the chip side (p.1).
  - The J1 lands are trimmed: signal pads Ø1.45 mm on Ø0.89 mm holes, shield pads Ø2.35 mm on Ø1.63
    mm holes.
  - That gives 1.09 mm from pin 5 (chip side) to pin 7 (cable side) and 1.97 mm from pin 10 to the
    shield.
  - The 2 mm "PoE isolation" rule exempts pad pairs inside J1 and relies on HanRun's rating
    ([layout](layout.md#ethernet-and-poe-isolation)).
- **PoE.** J1.7–J1.10 (VC1+, VC1−, VC2+, VC2−) carry the cable voltage from both pair sets (mode A
  and mode B) to U2 pins 1–4 (POE_VA1, POE_VA2, POE_VB1, POE_VB2). These four nets are class POE_CABLE:
  0.4 mm tracks for the jack's 350 mA centre-tap rating (p.1), in an isolated lane on In1. The PD side,
  the 2 mm isolation and the hipot test are in [power](power.md#poe-input).
- **Chip-side centre tap.** J1.3 (ETH_CT) goes only to C3, 100 nF to GND. The IP101G has internal
  terminations and wants no bias on the centre tap ("Do not connect to any power", p.7), and it asks
  for decoupling there (p.56).
- **Shield.** J1's shield (ETH_SHIELD) goes to GND through two paths:
  - C1, 1 nF 2 kV 1206 (FH 1206B102K202NT), which matches the jack's own internal 1000 pF/2 kV
    Bob-Smith capacitor (p.1);
  - a DC bleed of R2 + R3, two 499 kΩ Yageo RV1206 high-voltage resistors in series (≈ 1 MΩ). Two parts
    in series halve the voltage across each one.

  The shield tracks are 0.5 mm wide.
- **Hand-soldering trial.** J1's lands are 0.565–0.717 mm from four reflowed parts: R46 (a CSI link),
  C3, R8 and C1. J1's hand-assembly note asks for a conical tip ≤ 1 mm and Kapton over those four.

### MDI pairs crossed at J1

The pairs are swapped at J1, so that both route on F.Cu without crossing:

| U1 pin | PHY function | Net | J1 pin | Jack function |
|---|---|---|---|---|
| U1.30 | MDI_TP (transmit +) | ETH_RD_P | J1.4 | RD+ (RJ45 3) |
| U1.29 | MDI_TN (transmit −) | ETH_RD_N | J1.5 | RD− (RJ45 6) |
| U1.27 | MDI_RP (receive +) | ETH_TD_P | J1.1 | TD+ (RJ45 1) |
| U1.26 | MDI_RN (receive −) | ETH_TD_N | J1.2 | TD− (RJ45 2) |

- The PHY transmits on RJ45 pins 3/6. That is MDI-X wiring, and it works because the IP101G swaps its
  pairs itself.
- The net names follow the J1 pin they reach. P stays on P and N on N, so no polarity is inverted.
- The Ethernet sheet carries a note at U1/J1 saying so.

IP101G register facts the crossing relies on:

- "Supports Auto MDI/MDIX function" (p.1).
- Page 16, register 16 bit 11, AUTO_MDIX_DIS: R/W, default 0. "Set high to disable the automatic switch
  of MDI and MDI-X modes" (p.25).
- With auto-MDIX on, the PHY "will switch TX and RX pairs automatically", in auto-negotiation and in
  forced mode (§5.1.6 p.46).
- Register 18 bit 9 (MDI/MDIX) and bit 8 (POLARITY) are read-only status bits (p.27).
- Page 16, register 30 bit 3, FORCE_MDIX: default 0, which means MDI when auto-MDIX is off (p.29).

So the firmware must never set AUTO_MDIX_DIS. If auto-MDIX ever has to be off, it must set
FORCE_MDIX = 1, because the board's wiring is the PHY's MDI-X channel
([firmware-rules](firmware-rules.md#rules)).

The datasheet claims no automatic polarity correction. That is why each pair keeps P and N physically
correct, with a short via hop, rather than swapping P and N at the jack.

### MDI routing

- **Geometry.** Class MDI: 0.18 mm tracks with a 0.15 mm gap on F.Cu over the In1 GND plane. That is
  100 Ω differential nominal, the same geometry as the MIPI pairs. At about 33 mm the MDI lines are
  electrically short for 100BASE-TX, so the 88–114 Ω spread without impedance control doesn't matter
  ([layout](layout.md#impedance)).
- **Vias.** IP101G §6.2 (p.56) says "it is best not use via to place anywhere other than in close
  proximity to device". The crossing at J1 fixes the order of the two pairs, but inside each pair P
  must pass over N once. Each P line takes two vias and a 1.33 mm hop on B.Cu, 6–8 mm from U1's pins.
  The N lines have no vias.
- **Length matching.** TI SNLA387 (p.2, "MDI Traces") asks for P/N matching within 50 mil (1.27 mm)
  for 100 Mbit/s; the DRC rule "MDI intra-pair skew" holds it. Measured pad to pad, with via heights
  counted:

  | Pair | P (mm) | N (mm) | Skew |
  |---|---|---|---|
  | TD (U1.27/26 → J1.1/2) | 32.9422 | 32.9423 | 0.0001 mm |
  | RD (U1.30/29 → J1.4/5) | 27.4155 | 27.4156 | 0.0001 mm |

- **Spacing.** IP101G §6.2 asks for "three times space" between an MDI pair and other signals. The DRC
  rule "MDI spacing" holds 0.55 mm (3 × 0.18 mm, rounded up).
  - Inside U1's escape area (x 157.3–160.6, y 125.69–128.9) the 0.4 mm pitch makes that impossible.
    ISET (pin 25), REGOUT (pin 28) and AVDD33 (pin 31) sit 0.2 mm from the MDI pads, so only the
    0.2 mm default clearance applies there. This is an accepted deviation; the datasheet grants no
    exception.
  - The TX and RX pairs keep ≥ 0.54 mm (3W) from each other outside the same U1 exemption (DRC rule
    "MDI pair to pair"). The closest spot is the RD_N–TD_P diagonal pair south of R6, at 0.5399 mm,
    which the DRC passes.

### LEDs

| LED | J1 pins | Driven by | Mode 2 meaning (Table 8, p.53) |
|---|---|---|---|
| Green | LEDG+ J1.13 = +3V3, LEDG− J1.14 → R4 → PHY_LED0 (U1.11) | LED0 sinks (active low) | Link / activity |
| Yellow | LEDY+ J1.11 ← R5 ← PHY_LED3 (U1.12), LEDY− J1.12 = GND | LED3 sources (active high) | 100BASE-TX |

- R4 and R5 are 220 Ω 1 %.
- The LED pad driver limits the current to its default 1.62 mA (LED_DRIVE, p.35). The resistors
  limit it to ≤ 8.5 mA if the firmware raises the drive: (3.465 V − 1.6 V)/220 Ω, with +3V3 at the
  top of its ±5 % envelope. Each resistor then dissipates ≤ 16 mW in a 0402 (31 mW at 50 % derating).
- Each LED's polarity follows its pin's strap value (Fig. 12, p.47): the LED goes to VDD on a pin
  strapped 1 (AD0) and to GND on a pin strapped 0 (AD3). See [Straps](#straps).

## RMII

The PHY makes the RMII clock. With COL/RMII strapped to RMII and a 25 MHz crystal, 50M_CLKO (U1.14)
puts out 50 MHz (p.45). The path:

- 50M_CLKO → R13 (22 Ω) → net RMII_REF_CLK.
- At R13.2 the net splits in a star to U1.10 (50M_CLKI, the PHY's own timing reference, p.16) and to
  U6.68 (GPIO50, EMAC_CLK_EXT_IN). GPIO50 is input only, so the MAC can't drive the clock.

This is the datasheet's internal-clock topology (Fig. 8, p.44) and the one on Espressif's
ESP32-P4-Function-EV-Board. Its buffer (Fig. 10, p.45) is needed only for a third clock load, which
this board doesn't have.

### Series resistors

All seven are 22 Ω 1 % 0402, one at each driver.

- **PHY outputs** (Ethernet sheet): R11 (RXD0), R12 (RXD1), R15 (CRS_DV) and R13 (REF_CLK).
- **MAC outputs** (MCU sheet): R35 (TXD0), R36 (TXD1) and R37 (TX_EN). Espressif's design guidelines
  also recommend a series resistor on the RMII clock (§1.3.14, p.20).
- Each sits within 3 mm of its driving pin. 3 mm is about 17 ps at 5.8 ps/mm, far below any edge of
  0.5 ns or more, so the resistor works as a lumped source termination.
- TXD0/TXD1 are also the P4's strapping pins GPIO34/GPIO35. R38 (10 kΩ to +3V3) and the BOOT button
  through R39 (1 kΩ) sit on the MCU side of R36 ([mcu](mcu.md#reset-and-boot)).

### REF_CLK branches

Measured pad to pad, with via heights counted:

| Branch | Length | Vias |
|---|---|---|
| R13.2 → U1.10 (PHY) | 9.97 mm | 2 |
| R13.2 → U6.68 (MAC) | 46.42 mm | 3 |
| Difference | 36.45 mm | limit 60 mm |

The datasheet asks for equal clock lengths to the PHY and the MAC (§6.1, p.56) but gives no
tolerance. The limit comes from signal integrity, not from timing:

- **Near-end ledge.** U1.10 sits near the source end of a source-terminated line. Its edge completes
  only when the reflection returns from U6.68, 2 × v × ΔL later. No ledge forms while that round trip
  is no longer than the fastest RMII edge, 1 ns (RMII 1.2 §7.4.3): ΔL ≤ 1 ns/(2 × 7.1 ps/mm) = 70 mm.
  10 mm is reserved for the unpublished trace inside the P4 module, which leaves 60 mm.
- **Timing.** RX/TX timing alone tolerates about ±420 mm of branch difference (next section).

The clock is kept off the data lines (§6.1, "avoid signals path parallel to clock signals path"): it
runs 0.613 mm from CRS_DV and 0.65 mm from TXD0 where they share a stretch. Its longest close
neighbour is the +3V3 trunk, 8.1 mm at 0.35 mm. GND return vias sit at the RMII layer changes wherever
one fits ([layout](layout.md#ethernet-and-poe-isolation)).

### Data line lengths

These are the long segments, from the series resistor to the receiving pin:

| Net | Length | Limit |
|---|---|---|
| RMII_RXD0 | 18.47 mm | 80 mm |
| RMII_RXD1 | 17.92 mm | 80 mm |
| RMII_CRS_DV | 18.76 mm | 80 mm |
| RMII_TXD0 | 21.52 mm | 80 mm |
| RMII_TXD1 | 23.77 mm | 80 mm |
| RMII_TX_EN | 40.88 mm | 80 mm |

The short segments (driver to resistor) are 2.2–6.9 mm. TI SNLA387's 50 mil matching inside the RX
and TX groups is a generic checklist value. The timing below gives no matching need: 1.27 mm is 9 ps
against margins of nanoseconds. So the lines are not meandered.

### Timing

Inputs:

- IP101G (Tables 18/19, p.61; measured with the PHY's own 50M_CLKO clock, as here):
  - TX setup Ts ≥ 4 ns and hold Th ≥ 2 ns at 50M_CLKI;
  - RX output delay Td 6–13 ns at VDDIO 3.3 V;
  - clock period 20 ns ± 50 ppm.
- P4 MAC: setup, hold and output delay are **not published**. The ESP32-P4 datasheet v0.7, TRM
  chapter 55 and the hardware design guidelines §1.3.14 were searched. The RMII 1.2 figures (§7.4:
  Tsu 4 ns, Thold 2 ns) are assumed for its inputs.
- Board: Δ = clock arrival at U6.68 minus at U1.10 = v × 36.45 mm. At v = 7.1 ps/mm (the
  conservative figure; F.Cu is about 6.0) that is 0.26 ns.

| Path | Formula | Board figure | Slack |
|---|---|---|---|
| RX setup (PHY → MAC) | T + Δ − Td,max − t_flight − Tsu = 3 ns + Δ − t_flight | t_flight ≤ 0.17 ns (CRS_DV, 23.1 mm incl. its PHY stub) | 3.1 ns |
| RX hold | Td,min + t_flight − Δ − Th = 4 ns + t_flight − Δ | t_flight ≥ 0.14 ns (RXD1, 20.1 mm) | 3.9 ns |
| TX setup (MAC → PHY) | T − Δ − Tco,max − t_flight − Ts = 16 ns − Tco,max − (Δ + t_flight) | Δ + t_flight ≤ 0.58 ns (TX_EN, 45.6 mm incl. its MCU stub) | 15.4 ns − Tco,max |
| TX hold | Δ + Tco,min + t_flight − Th | t_flight ≥ 0.19 ns (TXD0, 26.5 mm) | Tco,min − 1.55 ns |

- RX has more than 3 ns of setup and hold.
- TX depends on the P4's output delay. The board takes a 1.0 ns budget (5 % of the 20 ns period) for
  the MAC clock branch plus a TX line: (60 + 80) mm × 7.1 ps/mm = 0.99 ns. That budget sets the limits
  above. The routed board uses 0.58 ns of it.

**Open item: RMII TX timing.** The board can't be proven by calculation until Espressif publishes the
P4 EMAC's output delay. Also unknown are the IP101G's 50M_CLKO edge rate and output impedance (no
figure and no IBIS model; default drive 4.86 mA, page 4 register 22 bits 15:13 RXC_DRIVE, p.39) and the
trace lengths inside the Waveshare module. The first-board checks close this item. If a ledge shows on
U1.10, the remedies, in order:

1. raise RXC_DRIVE;
2. lengthen the PHY branch toward equal branches;
3. give each branch its own series resistor (a schematic change).

### MDC and MDIO

- MDC (GPIO31) and MDIO (GPIO52) reach the P4 through the GPIO matrix. R14, 1.5 kΩ to +3V3, pulls up
  MDIO. MDC is always driven by the MAC and has no pull.
- MDC must stay at or below 3.33 MHz: period ≥ 300 ns, high and low ≥ 150 ns (Table 20, p.62). That is a
  firmware setting ([firmware-rules](firmware-rules.md#rules)).
- MDIO is 61.7 mm long with 6 vias, and MDC 35.6 mm with 3: slow signals, length doesn't matter.

## Straps

The IP101G latches its straps when RESET_N rises (pp.16–18):

- Internal pull-downs are 104 kΩ and pull-ups 222 kΩ (p.12). The external straps are 5.1 kΩ 1 %, the
  datasheet's latch resistor (Table 13, p.58).
- VIH ≥ 0.8 × VDDIO = 2.51 V at the bottom of the +3V3 ±5 % envelope (3.135 V) (p.57). These strap
  checks use the generic ±5 % envelope, 3.135–3.465 V, which is wider than the DC set point
  3.199–3.440 V and the ≥ 3.19 V switchover minimum ([Rails](README.md#rails)).
- LED0 and LED3 are Hi-Z during reset (p.18), so only the strap resistors and the LED paths set
  them. Both pull the same way as their strap.

| Strap | U1 pin | Set by | Level | Result |
|---|---|---|---|---|
| PHY_AD0 | 11 (LED0) | R1, 5.1 kΩ to +3V3 (+ green LED to +3V3) | 1 | PHY address bit 0 = 1 |
| PHY_AD3 | 12 (LED3) | R7, 5.1 kΩ to GND (+ internal pull-down, yellow LED to GND) | 0 | PHY address bit 3 = 0 |
| | | | | **PHY address 1** (Table 4, p.47, 32-pin package) |
| COL/RMII | 4 | R10, 5.1 kΩ to +3V3: 3.135 × 104/(104 + 5.15) = 2.99 V ≥ 2.51 V | 1 | RMII mode (p.17) |
| CRS/LEDMOD | 20 | R9, 5.1 kΩ to +3V3: 2.99 V | 1 | LED mode 2: LED0 link/activity, LED3 100BASE-TX (Table 8, p.53) |
| FX_HEN (RXDV/CRS_DV) | 19 | internal 104 kΩ pull-down only; GPIO28 has no pull at reset | 0 | Twisted-pair (copper) mode (p.16, p.55) |
| RESET_N | 32 | R8, 10 kΩ to GND against the internal 222 kΩ pull-up | 0.149 V | Held in reset until GPIO22 drives high |

- The firmware addresses the PHY at MDIO address 1.
- The firmware must not enable GPIO28's internal pull-up while RESET_N rises: it fights the 104 kΩ
  pull-down and can latch FX_HEN = 1 (fiber mode).
- The Ethernet sheet carries a strap note with this summary.

## Accepted risks

- **IP101GRI without a maker MSL or reflow statement** (JLCPCB lists MSL 3 from distributor data).
  User decision, 2026-10-31: "Keep IP101GRI, accept risk".
- **RMII TX timing unproven.** The P4 EMAC output delay is unpublished, so the 1.0 ns board budget is a
  heuristic. The 50M_CLKO edge rate is unknown too, so the 36.45 mm REF_CLK branch difference rests on
  the 1 ns fastest-edge assumption. See the open item under [RMII](#rmii).
- **MDI relies on auto-MDIX.** The pairs are crossed at J1, so a firmware that disables auto-MDIX
  without setting FORCE_MDIX gets no link.
- **MDI spacing inside U1's escape** is 0.2 mm instead of the 0.55 mm "three times space", forced by
  the 0.4 mm pitch.
- **MDI vias.** Each P line takes two vias 6–8 mm from U1. IP101G §6.2 allows vias near the device. The
  return-path and via-return check findings at those vias are accepted, because 0.55 mm MDI spacing
  leaves no GND via within 1.0 mm.
- **Crystal without a guard trace**, 0.45 mm from a +32V track and 2.35 mm from Q1's tab. Q1's tab may
  reach about 108 °C at the worst LED duty (30 °C ambient, [led](led.md#thermal)), and Y1's ±20 ppm
  holds only to 85 °C. The IP101GRI is rated to 85 °C ambient (p.57).
- **PHY_RST_N inside the LED ring**, 0.5–1.1 mm from the ring pads over part of its 75 mm.
- **J1 lands 0.565–0.717 mm from reflowed parts** (R46, C3, R8, C1), to be checked in the
  hand-soldering trial.

## First-board checks

1. **Link at 10 and 100 Mbit/s, with both cable types.** Bring up a link at each speed (forced on the
   link partner, then auto-negotiated), once with a straight cable and once with a crossover cable.
   Read page 16 register 18: bit 9 (MDI/MDIX) must show both states across the two cables, and bit 8
   (POLARITY) must stay normal.
2. **Throughput.** Run iperf at 100 Mbit/s full duplex in both directions with zero CRC errors on the
   link partner. This is the test for the RMII TX timing open item.
3. **REF_CLK.** Count the clock at R13: 50 MHz ± 50 ppm. Probe U1.10 and U6.68 for a ledge between
   0.66 V and 2.64 V (0.2 and 0.8 × VDDIO) on the edges. If one shows, use the remedies under
   [RMII](#rmii).
4. **Straps and LEDs.** The PHY answers on MDIO address 1, the EMAC gets its clock in RMII mode, the
   green LED shows link and activity, and the yellow LED lights at 100 Mbit/s only.
5. **Reset order.** With the firmware releasing PHY_RST_N ≥ 2.5 ms before the EMAC starts, Ethernet
   comes up on every boot.
6. **LED interference.** Run the LEDs at 0.5 ms, 120 fps. Scope TP1: PHY_RST_N stays above 2.0 V
   (0.6 × VDDIO). Watch the link: it never drops while the LEDs pulse.
7. **Temperatures.** In the LED thermal test (0.5 ms, 120 fps, 30 min, 30 °C ambient), put
   thermocouples on Y1 and U1 too. Accept if Y1 stays ≤ 85 °C and REF_CLK stays within ± 50 ppm.
8. **J1 hand soldering.** After soldering J1 as its assembly note says, inspect R46, C3, R8 and C1 for
   bridges and shifted parts.
9. **PoE.** Bring up the link on PoE power from a mode A and a mode B source. The PoE isolation hipot
   is in [power](power.md#first-board-checks).
