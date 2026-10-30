# PCB layout

What the board layout must do, and why. The net classes live in `hardware/espion-cam/espion-cam.kicad_pro` and the custom rules in `espion-cam.kicad_dru`; this page explains them and lists what DRC can't check. The tables below match the project as of 2026-10-30.

Where each constraint is enforced. DRC checks the net classes, the custom rules and the rule areas drawn at placement. The `layout` check (`kicad_check`) checks the project's `layout` section in `.omp/kicad.json`; `track_widths`, `return_path`, `via_return` and `decoupling_placement` need no settings. The rest is reviewed by hand. A tag after a constraint says which applies: "(check: DRC)", "(check: layout near)" and so on, or "(rule area at placement)" for a keep-out to draw as a rule area once the parts are placed. Numbers in `.omp/kicad.json` stay on this page too: it is the design record. The `layout` section reads "at", "next to" and "close" as ≤ 3 mm copper edge to edge (the decoupling distance); its lens-cone heights are datasheet maximums, package-typical for chip passives and small ICs (estimate).

## Board and stack-up

- One 4-layer board, 90 × 86 mm, R1 corners, every part on the top side. The OV9281 module is not on the board: it sits in the 3D-printed enclosure 10 mm behind the board's back, looking through a Ø16 mm cutout; the 12 IR LEDs sit on a ring around the cutout. Its 3D model (`hardware/lib/Module_SZYTF.3dshapes/`) is for the enclosure assembly. No ring connector.
- Placed and locked at set-up (board mm from the top-left corner (100, 100) of the outline): H1–H4 at the corners, 4 mm in; J1 (RJ45) and J2 (USB-C) on the top edge, their fronts 0.3 mm inside it so the silk clears it, so every cable leaves one edge; lens axis at (163, 159), 27 mm from the right and the bottom edge; D4–D15 on r = 11.5 mm; J4 at (163, 179.5) above the FFC notch, an 18 × 3 mm notch in the bottom edge centred on the lens axis, R0.5 corners. U2 (DP1435, left edge under J1) and U6 (P4 module, between J2 and the ring) are placed but not locked.
- Stack-up: JLCPCB JLC04161H-7628, 1.6 mm, 1 oz outer / 0.5 oz inner copper. Top Cu 0.035, prepreg 7628 × 1 0.2104 (Dk 4.4), L2 Cu 0.0152, core 1.065 (NP-155F, Dk 4.6), L3 Cu 0.0152, prepreg 7628 × 1 0.2104, bottom Cu 0.035 (mm); KiCad sums 1.617 mm with the mask. OSP finish: free at JLCPCB and flat for U1's 0.4 mm pitch and the 0201 links. Assemble within the OSP shelf life (3 months unopened) and at most two reflows; hand-solder U6 right after reflow.
- L2 is solid GND. L3 is power/GND and must be solid under any bottom-layer pair. The stack-up is symmetric, so L4 over L3 uses the same geometry as L1 over L2.
- Mounting: 4 × M3 plated holes (H1–H4), each pad on GND. They keep ≥ 2 mm from `POE_CABLE` copper; the `PoE isolation` rule checks it.

## Board rules

Board-wide minimums (Board Setup > Constraints) are JLCPCB's multilayer capabilities at no extra charge (capabilities page and extra-charge rules, read 2026-10-30): clearance and track 0.1 mm (3.0–3.5 mil costs +20 %), via 0.45 / 0.3 mm (smaller costs extra), annular ring 0.075 mm, hole-to-hole 0.2 mm, hole clearance 0.2 mm, copper to edge 0.2 mm, plated slot 0.35 mm, silk clearance 0.15 mm, text 1.0 mm high and 0.15 mm thick, mask to copper 0.09 mm. The net classes set the real widths. (check: DRC)

Library footprints that break them (to settle at placement): the 0.2 mm thermal vias of U1 (QFN-32) and U10 (TPS61170) are under the 0.3 mm hole minimum (0.2 mm holes with < 0.45 mm pads cost extra); U12's LGA pads are 0.15–0.175 mm apart against the 0.2 mm class clearance; J2's USB-C pins are 0.2 mm from the USB pair against the 0.3 mm `USB spacing` rule.

## Camera opening

- The 79° lens (diagonal) sits 10 mm behind the board, its 5.5 mm barrel ending 4.5 mm behind the back face. Its view cone, with ±0.5 mm enclosure misalignment and the pupil up to 1 mm inside the barrel (estimates), is r 5.7 mm at the back face, 7.0 mm at the front and 8.9 mm at the LED tops (2.3 mm). The Ø16 mm cutout and the LEDs on r = 11.5 mm (courtyards meet below r = 10.5) clear it.
- Nothing taller than the LEDs within the cone: a part h mm tall must sit ≥ 7.0 + 0.83 × h mm from the lens axis (≥ 18.6 mm for the 14 mm DP1435, ≥ 16 mm for 10 mm caps). (check: layout heightZones)
- At 2 m the LEDs are 0.33° off the lens axis.
- The FFC wraps round the bottom edge through the notch into J4 ([camera.md](camera.md)). Keep the notch and the board edge beside it free of parts and copper for the cable. (rule area at placement)

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

DRC checks the clearances and minimum widths; `track_widths` checks each track against its class width and each pair's gap.

| Class | Nets (pattern) | Track (mm) | Clearance (mm) | Diff pair W / gap (mm) | Via (mm) |
|---|---|---|---|---|---|
| Default | everything else | 0.2 | 0.2 | — | 0.6 / 0.3 |
| `POE_CABLE` | `/Ethernet/POE_*` | 0.4 | 0.2 | — | 0.6 / 0.3 |
| `ETH_SHIELD` | `/Ethernet/ETH_SHIELD*` | 0.4 | 0.2 | — | 0.6 / 0.3 |
| `LED_PWR` | `+32V`, `/LED driver/LED_K`, `/LED driver/ISNS`, `/LED ring/RING_*` | 1.0 | 0.2 | — | 0.8 / 0.4 |
| `PWR_5V` | `+5V`, `VBUS`, `/Power/+5V_POE*`, `/LED driver/+5V_BST` | 1.0 | 0.2 | — | 0.8 / 0.4 |
| `PWR_3V3` | `+3V3`, `/Camera/CAM_3V3` | 0.5 | 0.2 | — | 0.6 / 0.3 |
| `MIPI_CSI` | `/Camera/CSI_*`, `/Camera/CAM_CLK_?`, `/Camera/CAM_D?_?` | 0.18 | 0.15 | 0.18 / 0.15 | 0.6 / 0.3 |
| `MDI` | `/Ethernet/ETH_TD_?`, `/Ethernet/ETH_RD_?` | 0.18 | 0.15 | 0.18 / 0.15 | 0.6 / 0.3 |
| `USB` | `/MCU/USB_C_D_?`, `/MCU/USB_D_?` | 0.245 | 0.15 | 0.245 / 0.15 | 0.6 / 0.3 |
| `RMII_CLK` | `/Ethernet/RMII_REF_CLK*` | 0.3 | 0.2 | — | 0.6 / 0.3 |
| `RMII` | `/Ethernet/RMII_TX*`, `/Ethernet/RMII_RXD*`, `/Ethernet/RMII_CRS_DV*`, `/MCU/RMII_TX*` | 0.36 | 0.2 | — | 0.6 / 0.3 |

- The diff-pair classes have a 0.15 mm clearance so the pair's own gap passes DRC; their wider spacing to other nets comes from the custom rules.
- Every pattern matches a labelled net name, so re-annotation can't move a net out of its class. `RMII` also takes the PHY-side RX nets between U1 and R11/R12/R15 and the P4-side TX nets between U6 and R35–R37 (`RMII_TX_EN_MCU`, `RMII_TXD0_MCU`, `RMII_TXD1_MCU`).
- Widths: `POE_CABLE` carries up to 57 V and 350 mA per centre tap (HR871150C DS p.1), ample at 0.4 mm (estimate). `PWR_5V` at 1.0 mm (or pours) carries the 2 A peak at ≈ 10 °C rise on 1 oz outer copper (IPC-2152, estimate). The 0.2 mm `LED_PWR` clearance is above IPC-2221B B2 for 33 V.
- Default-class nets that need care by hand: `BST_SW` (1.44 A peak), `MDC`, `MDIO`, `PHY_RST_N`, `STATUS_LED`, the jack LED nets and the IMU bus.

## Custom rules

DRC checks every rule in the table (check: DRC).

| Rule | What | Why |
|---|---|---|
| `PoE isolation` | 2 mm from `POE_CABLE` copper to any other class, all layers; only pad pairs inside J1 are exempt | 1500 V rms isolation between the cable and the board, the RJ45 shield and the GND mounting holes (802.3 §33.4.1; DP1435 1.5 kV). 1.5 mm is basic-insulation clearance for a 2.4–2.5 kV impulse (IEC 60664-1/62368-1, via a TI summary); 2 mm adds margin and covers creepage on uncoated FR-4 |
| `MIPI_CSI spacing` | 0.36 mm (2W) from `MIPI_CSI` to everything but the pair partner; the two pads of each 0 Ω link R45–R47 and R49–R51 are exempt | Keeps GND pour ≥ 0.30 mm away, which the 100 Ω geometry assumes. A 0201's pads are 0.18 mm apart and carry the two halves of one lane |
| `MIPI_CSI intra-pair skew` | P/N skew ≤ 0.254 mm within each `MIPI_CSI` pair | Espressif HDG §1.4.6 |
| `MDI spacing` | 0.55 mm from `MDI` to other classes, except MDI items that touch U1's courtyard | 3× spacing (IP101G DS §6.2). U1's 0.4 mm pitch pads (0.2 mm gap) can't hold 0.55 mm |
| `USB spacing` | 0.3 mm from `USB` to everything but the pair partner | Keeps GND and other nets clear of the 90 Ω pair |
| (not a rule) | CSI pair-to-pair length match ≤ 0.762 mm | KiCad applies only the last matching rule per constraint type, so a second `skew` rule would replace the intra-pair one. The `layout` check does it instead (Camera below) |

- Isolation spacing is bare copper to bare copper; solder mask doesn't count. There are three copper domains: cable (`POE_*`), shield (`ETH_SHIELD*`) and board (GND and the rest). The shield is on the board side of the barrier; C1 and R2/R3 join it to GND without isolation clearance.

## Power

- No GND or +5V pour, plane or via under DP1435 pins 1–4 (the cable side) (rule area at placement). The module's own pin 4 → 5 gap is 22.86 mm (DP1435 DS p.2).
- LC filter: C11 at DP1435 pins 6/5, C15 at TPS2121 pin 7 (check: layout near), C13 and C14 between them with a low-impedance GND return to DP1435 pin 5.
- TPS2121 (U4): C15 right at pin 7 (IN1), C16 at pin 2 (IN2), C20 and C23 at pins 1/8 (OUT) (check: layout near), C22 within a few mm. Short, wide IN1/IN2/OUT/GND copper, with vias under the IN and OUT pads for current and heat (SLVSEA3F §11–12) (check: layout copperArea).
- U4: R22 (ILIM) and C21 (soft start) next to pins 10 and 11, their ground straight to pin 12. R18/R19 next to pin 6 and R20/R21 next to pin 3 (check: layout near), away from L1, L2 and `SW_3V3`.
- Buck (U3): C10/C12 at pins 4/2 on the same layer (check: layout near), then L2. Keep `SW_3V3` as small as possible: L2 (FXL0630) has bigger pads than a 3 × 3 mm part, so keep the switch copper to the pad itself (SLVSDG1C §10).
- U3: R16, R17 and C17 next to pin 5 (check: layout near), away from `SW_3V3`. C18/C19, the only output caps at the inductor, return to the C10/C12 ground. Give pin 2 (GND) copper for heat: the datasheet R_θJA (188.2 °C/W) and ≈ 0.29 W of loss give +54 °C worst case (estimate).
- Keep the 14 mm-tall DP1435, the 8 × 10.5 mm C14 electrolytic and the C22 polymer out of the lens view cone (Camera opening above) (check: layout heightZones).

## MCU and USB

- C29 and C28 at U6 pins 75/76 (check: layout near); pin 74 on the same +3V3 copper. U6 pin 53 (ESP_LDO_VO3) feeds the module's MIPI PHY: no test pad, no load.
- R33, C27 and SW1 close to U6 pin 1 with a short `ESP_EN` trace (check: layout near, R33 and C27); this avoids reboots from interference (Espressif HDG).
- R35 (TXD0), R36 (TXD1) and R37 (TX_EN) within ≈ 3 mm of U6 pins 46, 47 and 67: they terminate the P4-driven RMII lines at the source (check: layout near).
- R39 (and R38) sit directly on the `RMII_TXD1_MCU` trace between U6 pin 47 and R36, keeping the BOOT stub to a few mm: TXD1 is synchronous to the 50 MHz REF_CLK. SW2 can sit anywhere after R39.
- USB pairs: 90 Ω, in parallel with equal lengths, GND around and underneath, few vias (Espressif HDG). R31/R32 at U6. U5 (USBLC6) at J2, flow-through, with short GND and pin 5 connections (USBLC6 DS §2.3). (check: layout near, R31, R32 and U5)
- D2 (VBUS TVS) within 5 mm of J2's VBUS pins, before `VBUS` branches to U5, R24 and R27, with a short via to GND (check: layout near, to both VBUS pad pairs). Not at U4; C16 stays at U4 IN2.
- `VBUS_SENSE`: R28 and C26 at U6 pin 16, C26 closest. It is a 29 kΩ node: keep it short and away from the USB pair and the switch nodes. R27 sits at the pin too, on its own thin tap from `VBUS` (45 µA). (check: layout near)
- PPS: the loop uses adjacent pins 44/45 (GPIO32/33) with R34 between them. TP6 on the PPS side, GND loop TP7 within ≈ 5 mm so a probe ground spring reaches it (check: layout near).
- GPIO numbers outside Ethernet may still be swapped at placement, within the groups in [firmware-rules.md](firmware-rules.md).

## Status LED

- Front side (the IR LED side), outside the lens view cone (check: layout heightZones) and away from the cutout edge. The OV9281 also sees visible light; without an IR-pass filter a lit LED adds a blob (firmware can blank it).
- C24 within 1 mm of D1 pin 4 (check: layout near); GND pin via straight to the plane.
- R30 at U6 pin 15, R26 at the LED (check: layout near). Keep `STATUS_LED` away from the RMII and MIPI pairs, the LED gate switching node and `XVS` (XVS edges carry timing).

## Ethernet

- MDI pairs: short, no vias except at the device. Leave U1's pad row at the pad pitch and open out to 0.55 mm at the courtyard edge (check: DRC). The `MDI spacing` exemption covers a whole segment if any part touches the courtyard, so end each escape segment at the courtyard edge with a vertex there.
- RMII: short, never parallel to REF_CLK, `MDC` or `XTAL_X1` (IP101G DS §6.1). R11–R13 and R15 within ≈ 3 mm of the PHY pins and R35–R37 within ≈ 3 mm of the P4 pins: each terminates its line at the driver (check: layout near).
- REF_CLK: R13 is the only series resistor; the star point is its pin 2. Both branches, to U1 pin 10 and to GPIO50 (U6 pin 68), ≤ 20 mm and equal in length (IP101G DS §6.1); the RX setup margin assumes it (check: layout paths; "equal" is within 1 mm, ≈ 7 ps). GND guard at 0.20 mm (coplanar row); a layout choice, not a DRC rule: a closer guard lowers the impedance.
- Crystal: Y1 right next to U1 pins 2/3, C2/C5 close to its pins (check: layout near), a GND guard around it, no signals under the crystal or its traces. The 18 pF load caps assume ≈ 3 pF of pin and trace capacitance (estimate).
- Decoupling, each with a short via to GND: C9 at pin 13 (VDDIO), C6 at pin 28 (REGOUT), C7 at pin 31 (AVDD33); C4 next to C6 (check: layout near), C8 between pins 13 and 31. U1's exposed pad is its only GND: the footprint has thermal vias, otherwise stitch at least 4–5.
- No GND or power plane under the jack's cable-side pins (7–10) or between them and the shell pads (rule area at placement). `ETH_SHIELD` gets its own small pour around the shell pads, ≥ 2 mm from `POE_*` (check: DRC); C1 and R2/R3 bridge it to GND at the plane's edge.
- Jack LEDs: R4/R5 next to U1 pins 11/12, so the long run to the jack sits behind the 220 Ω; R9 next to pin 20 (check: layout near). J1's LED pads 11–14 are on the board side, ≈ 8.6 mm from the nearest `POE_*` pad (10). Keep the LED traces clear of the MDI pairs and `RMII_CLK`.
- TP1 (`PHY_RST_N`) next to R8, reachable with a probe (check: layout near).

## Camera

- J4 on top, above the FFC notch, slot facing it: the cable makes a U-turn round the board edge, which keeps the conductor order ([camera.md](camera.md)). 2.5 mm tall; the slide latch opens toward the notch.
- Pad n carries camera pin n: D0 on pads 2–3, D1 on 5–6, CLK on 8–9. Silkscreen camera pin 1 (pad 1) and the contact side the ordered cable needs. Check the pin-1 edge in the 3D view.
- Route P→P, N→N and each lane straight: the P4 CSI host has no lane or polarity swap (TRM v0.7 ch. 42). U6 orders pins 28–33 D0_N, D0_P, CLK_P, CLK_N, D1_N, D1_P, unlike the FPC, so some pairs cross and some P/N orders flip; do it at the layer-change vias, the same for all three pairs.
- Length: P/N ≤ 0.254 mm (check: DRC); pair to pair ≤ 0.762 mm, tuned with the length tuner (Espressif HDG §1.4.6) (check: layout lengthGroups). The 0201 0 Ω links R45–R47 and R49–R51 sit at the FPC end and split each lane, so match the three `CSI_*` pairs to each other and the three `CAM_*` pairs to each other.
- GND around the pairs at ≥ 0.30 mm, always around CLK (check: DRC); continuous GND reference, L3 solid under B.Cu pairs (check: return_path). ≥ 3W from other fast signals, never parallel to them or RMII; away from the TLV62569/TPS61170 switch nodes and inductors and from REF_CLK. The pairs run between the LEDs: keep them off the `RING_*` and `+32V` copper.
- Same via count on every lane (check: layout viaParity, on each side of the 0 Ω links), with a pair of GND return vias at each layer change (check: via_return finds at least one within 1 mm).
- Pigtail: J3, R40/R44, U7 and R48 within a few mm of each other; `XVS_CAM` and `STROBE_CAM` short. Keep J3 and the pigtail away from `LED_K`, the +32 V storage caps and the boost switch node. TP8–TP10 reachable with the module fitted.
- C32 and C30 at J4 pad 15 (check: layout near); GND pads 1/4/7/10 via straight to the plane.
- No copper or parts in the lens cutout and its 0.2 mm edge clearance, or in the FFC notch (rule area at placement).

## LED driver and ring

- Pulse loop (1 A, µs edges): C46/C47 (+ C43/C45) → `+32V` → ring → `LED_K` → Q1 → R69 → GND → C46/C47. C46/C47 at the ring's anode end (D4), Q1/R69 at its cathode end (D15). Both ends sit in the ring's gap at the upper right (toward the P4 module), the string running counter-clockwise from D4.
- Run the ring trace over solid GND (inner or bottom layer) so the return follows it around the cutout. Edges are ≥ 1 µs, so 200 nH of loop gives ≤ 0.2 V: no snubber.
- The 12 LEDs sit evenly on r = 11.5 mm, pads in a row along the ring, each cathode pad facing the next LED of the string, so every `RING_*` link is a short hop: retroreflectors send the light back in a narrow cone around its source, so the ring stays as tight as the courtyards allow.
- Enlarge each LED's anode/centre-pad copper for heat (≈ 0.12 W average each). The ring has no GND connection, so its heat copper stays on the LED anode nets.
- Kelvin sense: `ISNS_K` leaves R69's source-side pad through NT1 (at the pad), as a thin trace to R64 only. Return U11 V−, C44, R61, C40 and C48 to GND next to R69's GND pad.
- Keep `ISNS_K`, `ISET` and `SINK_G` short and away from `BST_SW` and `LED_K`. R67 at Q1's gate; C42 and R64 at U11 (check: layout near).
- Q1 (DPAK): ≥ 600 mm² of `LED_K` copper on the component side plus a matching pour on the other side, tied with ≥ 12 × 0.3 mm vias: the datasheet's 1 in² rating (R_θJA 50 K/W, +40 °C at 0.8 W) (check: layout copperArea, within 15 mm of Q1; via drill by hand). `LED_K` is a slow node, so the area is no EMI concern. TH1 within 1 mm of the tab copper edge, on a GND-connected pad (not on the drain copper).
- Boost: smallest loops for C36/C38 → VIN/GND and SW → D3 → C43/C45 → U10 GND; C43/C45 next to D3's cathode (check: layout near). `BST_SW` is Default class but carries 1.44 A peak: draw it short, wide and small by hand.
- R65/R66 and R62/C41/C39 at U10's FB/COMP pins (check: layout near), away from SW and L3. U10's exposed pad to GND through the footprint's two 0.2 mm vias (SLVS789D layout example).
- Timeout RC: C33, R55 and R58 at U9, R54 right at C33 (check: layout near). `STROBE` reaches R54 from U7 on the camera sheet.

## IMU

- U12 near the lens axis, package edges parallel to the image axes (the board's x/y). Record its pin-1 corner relative to the image axes for the firmware's axis mapping. The camera is in the enclosure, not on this board, so the IMU follows the camera only as far as the enclosure holds both rigidly.
- Away from heat, since its offset drifts with temperature: Q1 and its copper, the boost (U10, L3, D3) and the PoE module U2. Away from H1–H4 and the board edges, where screw torque bends the board (estimate: MEMS offset is stress sensitive).
- C50 at pin 5 and C51 at pin 8, with short vias to L2 (check: layout near).

## Ordering

- Order JLC04161H-7628 with impedance control on; the calculator assumes a Nan Ya NP-155F core. Keep solder mask over impedance tracks: opening it raises Z by 3–8 %.

## Sources

- JLCPCB JLC04161H-7628 stack-up and impedance calculator, captured 2026-10-29; JLCPCB PCB capabilities and "In what cases will there be charged extra?", read 2026-10-30; "JLCPCB Launches OSP Starting from 2-Layer PCBs" (2026-07-30)
- SZYTF drawing WX-1M-9281-79-V1.0 (module, lens 79.3°)
- IEEE 802.3 Clause 33.4.1 (via interpretation summaries); IEC 60664-1/62368-1 via TI SLUP421 (summary, not read); IPC-2221B, IPC-2152
- Datasheets: SDAPO DP1435-5V; HanRun HR871150C; IC Plus IP101G; ST USBLC6-2SC6; Infineon IRLR024N; TI SLVSEA3F (TPS2121), SLVSDG1C (TLV62569), SLVS789D (TPS61170)
- Espressif ESP32-P4 Hardware Design Guidelines (schematic checklist, PCB layout), docs.espressif.com, read 2026-10-28; ESP32-P4 TRM v0.7
