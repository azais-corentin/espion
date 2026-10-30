# PCB layout

What the board layout must do, and why. The net classes live in `hardware/espion-cam/espion-cam.kicad_pro` and the custom rules in `espion-cam.kicad_dru`; this page explains them and lists what DRC can't check. The tables below match the project as of 2026-10-30.

## Board and stack-up

- One 4-layer board. The OV9281 camera module sits on the back (B.Cu) with its lens through a cutout; the 12 IR LEDs and the status LED sit on the front, around the cutout. No ring connector.
- Stack-up: JLCPCB JLC04161H-7628, 1.6 mm, 1 oz outer / 0.5 oz inner copper. Top Cu 0.035, prepreg 7628 × 1 0.2104, L2 Cu 0.0152, core 1.065, L3 Cu 0.0152, prepreg 7628 × 1 0.2104, bottom Cu 0.035 (mm).
- L2 is solid GND. L3 is power/GND and must be solid under any bottom-layer pair. The stack-up is symmetric, so L4 over L3 uses the same geometry as L1 over L2.
- Mounting: 4 × M3 plated holes (H1–H4), each pad on GND. They keep ≥ 2 mm from `POE_CABLE` copper; the `PoE isolation` rule checks it.

## Impedance

Coated microstrip on L1 over the L2 plane. W = drawn base width, S = gap inside a pair, D = edge gap to coplanar GND (mm).

| Target | Nets | Structure | W | S | D | JLCPCB calc | Field solver |
|---|---|---|---|---|---|---|---|
| 100 Ω diff | CSI data, MDI | edge-coupled | 0.18 | 0.15 | — | 100.42 Ω | 100.74 Ω |
| 100 Ω diff | CSI CLK with GND around | coplanar diff | 0.18 | 0.15 | ≥ 0.30 | 99.47 Ω (D 0.30) | 99.67 Ω |
| 90 Ω diff | USB | edge-coupled | 0.245 | 0.15 | — | 90.09 Ω | 90.35 Ω |
| 50 Ω SE | RMII | microstrip | 0.36 | — | — | 49.74 Ω | 49.82 Ω |
| 50 Ω SE | REF_CLK, GND-guarded | coplanar | 0.30 | — | 0.20 | 50.03 Ω | 50.08 Ω |

- The CSI CLK pair uses the data pairs' width and gap. Keep its GND at D ≥ 0.30 mm (D ≥ 0.40 mm gives ≥ 100 Ω; D 0.20 gives 97.7 Ω). A guarded single-ended track needs the coplanar row: a 0.36 mm RMII track with a guard at 0.20 mm drops to 45.8 Ω.
- The JLCPCB values are what its calculator gives for JLC04161H-7628 (prepreg Dk 4.3, finished outer copper 1.6 mil, mask 1.0 mil over substrate and 0.6 mil over copper, mask Dk 3.8), captured 2026-10-29. A separate 2D field solver agrees within 0.41 % and reads slightly high. Both rest on JLCPCB's material data (estimate).
- JLCPCB holds impedance to ±10 %. The ±10 % etched width dominates (≈ ±6–8 % in Z); the published Dk of 4.4 instead of 4.3 shifts Z by −0.8 %. Every width is ≥ 0.15 mm, over the 0.09 mm multilayer minimum.

## Net classes

| Class | Nets (pattern) | Track (mm) | Clearance (mm) | Diff pair W / gap (mm) | Via (mm) |
|---|---|---|---|---|---|
| Default | everything else | 0.2 | 0.2 | — | 0.6 / 0.3 |
| `POE_CABLE` | `/Ethernet/POE_*` | 0.4 | 0.2 | — | 0.6 / 0.3 |
| `ETH_SHIELD` | `/Ethernet/ETH_SHIELD*` | 0.4 | 0.2 | — | 0.6 / 0.3 |
| `LED_PWR` | `+32V`, `/LED driver/LED_K`, `/LED driver/ISNS`, `/LED ring/RING_*` | 1.0 | 0.2 | — | 0.8 / 0.4 |
| `PWR_5V` | `+5V`, `VBUS`, `/Power/+5V_POE*`, `/LED driver/+5V_BST` | 1.0 | 0.2 | — | 0.8 / 0.4 |
| `PWR_3V3` | `+3V3`, `/Ethernet/PHY_3V3`, `/Camera/CAM_3V3` | 0.5 | 0.2 | — | 0.6 / 0.3 |
| `MIPI_CSI` | `/Camera/CSI_*`, `/Camera/CAM_CLK_?`, `/Camera/CAM_D?_?` | 0.18 | 0.15 | 0.18 / 0.15 | 0.6 / 0.3 |
| `MDI` | `/Ethernet/ETH_TD_?`, `/Ethernet/ETH_RD_?` | 0.18 | 0.15 | 0.18 / 0.15 | 0.6 / 0.3 |
| `USB` | `/MCU/USB_C_D_?`, `/MCU/USB_D_?` | 0.245 | 0.15 | 0.245 / 0.15 | 0.6 / 0.3 |
| `RMII_CLK` | `/Ethernet/RMII_REF_CLK*` | 0.3 | 0.2 | — | 0.6 / 0.3 |
| `RMII` | `/Ethernet/RMII_TX*`, `/Ethernet/RMII_RXD*`, `/Ethernet/RMII_CRS_DV*` | 0.36 | 0.2 | — | 0.6 / 0.3 |

- The diff-pair classes have a 0.15 mm clearance so the pair's own gap passes DRC; their wider spacing to other nets comes from the custom rules.
- Every pattern matches a labelled net name, so re-annotation can't move a net out of its class. `RMII` also takes the PHY-side RX nets between U6 and R12–R14.
- Widths: `POE_CABLE` carries up to 57 V and 350 mA per centre tap (HR871150C DS p.1), ample at 0.4 mm (estimate). `PWR_5V` at 1.0 mm (or pours) carries the 2 A peak at ≈ 10 °C rise on 1 oz outer copper (IPC-2152, estimate). The 0.2 mm `LED_PWR` clearance is above IPC-2221B B2 for 33 V.
- Default-class nets that need care by hand: `BST_SW` (1.44 A peak), `MDC`, `MDIO`, `PHY_RST_N`, `STATUS_LED`, the jack LED nets and the IMU bus.

## Custom rules

| Rule | What | Why |
|---|---|---|
| `PoE isolation` | 2 mm from `POE_CABLE` copper to any other class, all layers; only pad pairs inside J2 are exempt | 1500 V rms isolation between the cable and the board, the RJ45 shield and the GND mounting holes (802.3 §33.4.1; DP1435 1.5 kV). 1.5 mm is basic-insulation clearance for a 2.4–2.5 kV impulse (IEC 60664-1/62368-1, via a TI summary); 2 mm adds margin and covers creepage on uncoated FR-4 |
| `MIPI_CSI spacing` | 0.36 mm (2W) from `MIPI_CSI` to everything but the pair partner; the two pads of each 0 Ω link R38–R43 are exempt | Keeps GND pour ≥ 0.30 mm away, which the 100 Ω geometry assumes. A 0201's pads are 0.18 mm apart and carry the two halves of one lane |
| `MIPI_CSI intra-pair skew` | P/N skew ≤ 0.254 mm within each `MIPI_CSI` pair | Espressif HDG §1.4.6 |
| `MDI spacing` | 0.55 mm from `MDI` to other classes, except MDI items that touch U6's courtyard | 3× spacing (IP101G DS §6.2). U6's 0.4 mm pitch pads (0.2 mm gap) can't hold 0.55 mm |
| `USB spacing` | 0.3 mm from `USB` to everything but the pair partner | Keeps GND and other nets clear of the 90 Ω pair |
| (not a rule) | CSI pair-to-pair length match ≤ 0.762 mm | KiCad applies only the last matching rule per constraint type, so a second `skew` rule would replace the intra-pair one. Check by hand (Camera below) |

- Isolation spacing is bare copper to bare copper; solder mask doesn't count. There are three copper domains: cable (`POE_*`), shield (`ETH_SHIELD*`) and board (GND and the rest). The shield is on the board side of the barrier; C12 and R15/R16 join it to GND without isolation clearance.

## Power

- No GND or +5V pour, plane or via under DP1435 pins 1–4 (the cable side). The module's own pin 4 → 5 gap is 22.86 mm (DP1435 DS p.2).
- LC filter: C13 at DP1435 pins 6/5, C14 at TPS2121 pin 7, C1 and C2 between them with a low-impedance GND return to DP1435 pin 5.
- TPS2121 (U4): C14 right at pin 7 (IN1), C15 at pin 2 (IN2), C16 and C18 at pins 1/8 (OUT), C17 within a few mm. Short, wide IN1/IN2/OUT/GND copper, with vias under the IN and OUT pads for current and heat (SLVSEA3F §11–12).
- U4: R68 (ILIM) and C52 (soft start) next to pins 10 and 11, their ground straight to pin 12. R17/R18 next to pin 6 and R66/R67 next to pin 3, away from L1, L2 and `SW_3V3`.
- Buck (U5): C21/C22 at pins 4/2 on the same layer, then L2. Keep `SW_3V3` as small as possible: L2 (FXL0630) has bigger pads than a 3 × 3 mm part, so keep the switch copper to the pad itself (SLVSDG1C §10).
- U5: R19, R20 and C23 next to pin 5, away from `SW_3V3`. C19/C20, the only output caps at the inductor, return to the C21/C22 ground. Give pin 2 (GND) copper for heat: the datasheet R_θJA (188.2 °C/W) and ≈ 0.29 W of loss give +54 °C worst case (estimate).
- Keep the 14 mm-tall DP1435, the 8 × 10.5 mm C2 electrolytic and the C17 polymer clear of the front-side LED ring and the lens cutout (estimate: follows from the module on the back).

## MCU and USB

- C26 and C27 at U1 pins 75/76; pin 74 on the same +3V3 copper. U1 pin 53 (ESP_LDO_VO3) feeds the module's MIPI PHY: no test pad, no load.
- R1, C24 and SW1 close to U1 pin 1 with a short `ESP_EN` trace; this avoids reboots from interference (Espressif HDG).
- R3 (and R2) sit directly on the `RMII_TXD1` trace, keeping the BOOT stub to a few mm: TXD1 is synchronous to the 50 MHz REF_CLK. SW2 can sit anywhere after R3.
- USB pairs: 90 Ω, in parallel with equal lengths, GND around and underneath, few vias (Espressif HDG). R23/R24 at U1. U2 (USBLC6) at J1, flow-through, with short GND and pin 5 connections (USBLC6 DS §2.3).
- D2 (VBUS TVS) within 5 mm of J1's VBUS pins, before `VBUS` branches to U2, R25 and R62, with a short via to GND. Not at U4; C15 stays at U4 IN2.
- `VBUS_SENSE`: R63 and C49 at U1 pin 16, C49 closest. It is a 29 kΩ node: keep it short and away from the USB pair and the switch nodes. R62 sits at the pin too, on its own thin tap from `VBUS` (45 µA).
- PPS: the loop uses adjacent pins 44/45 (GPIO32/33) with R27 between them. TP1 on the PPS side, GND loop TP9 within ≈ 5 mm so a probe ground spring reaches it.
- GPIO numbers outside Ethernet may still be swapped at placement, within the groups in [firmware-rules.md](firmware-rules.md).

## Status LED

- Front side (the IR LED side), outside the lens field of view and away from the cutout edge. The OV9281 also sees visible light; without an IR-pass filter a lit LED adds a blob (firmware can blank it).
- C28 within 1 mm of D1 pin 4; GND pin via straight to the plane.
- R28 at U1 pin 15, R29 at the LED. Keep `STATUS_LED` away from the RMII and MIPI pairs, the LED gate switching node and `XVS` (XVS edges carry timing).

## Ethernet

- MDI pairs: short, no vias except at the device. Leave U6's pad row at the pad pitch and open out to 0.55 mm at the courtyard edge. The `MDI spacing` exemption covers a whole segment if any part touches the courtyard, so end each escape segment at the courtyard edge with a vertex there.
- RMII: short, never parallel to REF_CLK, `MDC` or `XTAL_X1` (IP101G DS §6.1). R11–R14 within ≈ 3 mm of the PHY pins: they terminate the PHY-driven lines at the source.
- REF_CLK: R11 is the only series resistor; the star point is its pin 2. Both branches, to U6 pin 10 and to GPIO50 (U1 pin 68), ≤ 20 mm and equal in length (IP101G DS §6.1); the RX setup margin assumes it. GND guard at 0.20 mm (coplanar row); a layout choice, not a DRC rule: a closer guard lowers the impedance.
- Crystal: Y1 right next to U6 pins 2/3, C3/C4 close to its pins, a GND guard around it, no signals under the crystal or its traces. The 18 pF load caps assume ≈ 3 pF of pin and trace capacitance (estimate).
- Decoupling, each with a short via to GND: C5 at pin 13 (VDDIO), C6 at pin 28 (REGOUT), C7 at pin 31 (AVDD33); C8 next to C6, C9 between pins 13 and 31. U6's exposed pad is its only GND: the footprint has thermal vias, otherwise stitch at least 4–5.
- No GND or power plane under the jack's cable-side pins (7–10) or between them and the shell pads. `ETH_SHIELD` gets its own small pour around the shell pads, ≥ 2 mm from `POE_*`; C12 and R15/R16 bridge it to GND at the plane's edge.
- Jack LEDs: R69/R70 next to U6 pins 11/12, so the long run to the jack sits behind the 220 Ω; R71 next to pin 20. J2's LED pads 11–14 are on the board side, ≈ 8.6 mm from the nearest `POE_*` pad (10). Keep the LED traces clear of the MDI pairs and `RMII_CLK`.
- TP14 (`PHY_RST_N`) next to R10, reachable with a probe.

## Camera

- J3 on B.Cu beside the module, so the FFC runs flat with no twist or fold: either flips the contact side the single-contact connector relies on. 2.5 mm tall; the slide latch needs room on the cable side.
- Pad n carries camera pin 16 − n: CLK on pads 7–8, D1 on 10–11, D0 on 13–14. Silkscreen "contacts → away from board" (top contact; reverse for the bottom variant), the stiffener side and camera pin 1 (pad 15). Check the pin-1 edge in the 3D view.
- Route P→P, N→N and each lane straight: the P4 CSI host has no lane or polarity swap (TRM v0.7 ch. 42). U1 orders pins 28–33 D0_N, D0_P, CLK_P, CLK_N, D1_N, D1_P, unlike the FPC, so some pairs cross and some P/N orders flip; do it at the layer-change vias, the same for all three pairs.
- Length: P/N ≤ 0.254 mm (DRC); pair to pair ≤ 0.762 mm by hand with the length tuner (Espressif HDG §1.4.6). The 0201 0 Ω links R38–R43 sit at the FPC end and split each lane, so match the three `CSI_*` pairs to each other and the three `CAM_*` pairs to each other.
- GND around the pairs at ≥ 0.30 mm, always around CLK; continuous GND reference (L3 solid under B.Cu pairs). ≥ 3W from other fast signals, never parallel to them or RMII; away from the TLV62569/TPS61170 switch nodes and inductors and from REF_CLK.
- Same via count on every lane, with a pair of GND return vias at each layer change.
- Pigtail: J4, R31/R32, U7 and R30 within a few mm of each other; `XVS_CAM` and `STROBE_CAM` short. Keep J4 and the pigtail away from `LED_K`, the +32 V storage caps and the boost switch node. TP5–TP7 reachable with the module fitted.
- C30 and C31 at J3 pad 1; GND pads 15/12/9/6 via straight to the plane.
- No copper or parts in the lens cutout and its clearance ring, or under the module outline on B.Cu except its standoffs (fix at placement).

## LED driver and ring

- Pulse loop (1 A, µs edges): C36/C37 (+ C38/C39) → `+32V` → ring → `LED_K` → Q1 → R58 → GND → C36/C37. C36/C37 at the ring's anode end (D4), Q1/R58 at its cathode end (D15).
- Run the ring trace over solid GND (inner or bottom layer) so the return follows it around the cutout. Edges are ≥ 1 µs, so 200 nH of loop gives ≤ 0.2 V: no snubber.
- The 12 LEDs sit evenly spaced, close around the lens holder: retroreflectors send the light back in a narrow cone around its source.
- Enlarge each LED's anode/centre-pad copper for heat (≈ 0.12 W average each). The ring has no GND connection, so its heat copper stays on the LED anode nets.
- Kelvin sense: `ISNS_K` leaves R58's source-side pad through NT1 (at the pad), as a thin trace to R55 only. Return U11 V−, C45, R53, C44 and C47 to GND next to R58's GND pad.
- Keep `ISNS_K`, `ISET` and `SINK_G` short and away from `BST_SW` and `LED_K`. R54 at Q1's gate; C46 and R55 at U11.
- Q1 (DPAK): ≥ 600 mm² of `LED_K` copper on the component side plus a matching pour on the other side, tied with ≥ 12 × 0.3 mm vias: the datasheet's 1 in² rating (R_θJA 50 K/W, +40 °C at 0.8 W). `LED_K` is a slow node, so the area is no EMI concern. TH1 within 1 mm of the tab copper edge, on a GND-connected pad (not on the drain copper).
- Boost: smallest loops for C34/C35 → VIN/GND and SW → D3 → C38/C39 → U9 GND; C38/C39 next to D3's cathode. `BST_SW` is Default class but carries 1.44 A peak: draw it short, wide and small by hand.
- R45/R46 and R47/C40/C41 at U9's FB/COMP pins, away from SW and L3. U9's exposed pad to GND through the footprint's two 0.2 mm vias (SLVS789D layout example).
- Timeout RC: C43, R50 and R51 at U10, R72 right at C43. `STROBE` reaches R72 from U7 on the camera sheet.

## IMU

- U12 on the board region that holds the lens mount, package edges parallel to the image axes. Record its pin-1 corner relative to the image axes for the firmware's axis mapping.
- Away from heat, since its offset drifts with temperature: Q1 and its copper, the boost (U9, L3, D3) and the PoE module U3. Away from H1–H4 and the board edges, where screw torque bends the board (estimate: MEMS offset is stress sensitive).
- C50 at pin 5 and C51 at pin 8, with short vias to L2.

## Ordering

- Order JLC04161H-7628 with impedance control on; the calculator assumes a Nan Ya NP-155F core. Keep solder mask over impedance tracks: opening it raises Z by 3–8 %.

## Sources

- JLCPCB JLC04161H-7628 stack-up and impedance calculator, captured 2026-10-29
- IEEE 802.3 Clause 33.4.1 (via interpretation summaries); IEC 60664-1/62368-1 via TI SLUP421 (summary, not read); IPC-2221B, IPC-2152
- Datasheets: SDAPO DP1435-5V; HanRun HR871150C; IC Plus IP101G; ST USBLC6-2SC6; Infineon IRLR024N; TI SLVSEA3F (TPS2121), SLVSDG1C (TLV62569), SLVS789D (TPS61170)
- Espressif ESP32-P4 Hardware Design Guidelines (schematic checklist, PCB layout), docs.espressif.com, read 2026-10-28; ESP32-P4 TRM v0.7
