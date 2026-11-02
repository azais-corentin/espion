# espion-cam board layout

How the board is built and why: outline, stack-up, fabrication and design rules, impedance, routing style, the
placement and routing of each block with its measurable constraints, assembly, and the check findings that are
accepted on purpose. The board file is `hardware/espion-cam/espion-cam.kicad_pcb`, the custom rules are in
`espion-cam.kicad_dru`, and the measurable placement constraints are the `layout` section of `.omp/kicad.json`, which
`kicad_check layout` enforces.

Positions are KiCad board coordinates in mm (x right, y down); the board's top-left corner is (100, 100). Distances
are copper edge to copper edge unless stated otherwise (pad edge to pad edge for parts), measured with
`kicad_query distance` on the routed board. "≤ 3 mm" for a part a datasheet wants "as close as possible" is this
project's heuristic, not a source value.

## Board and stack-up

### Outline and holes

| Item | Value | Reason |
|---|---|---|
| Outline | 64 × 64 mm, (100, 100)–(164, 164), corners r 1 mm, 3840 mm² net of cutouts | Smallest outline that fits the floorplan the user approved (user decisions 2026-10-31: 64 × 64 mm, J1 on the right, DP1435 on the left, J4 at an edge) |
| FFC notch | Top edge, x 120.7–138.7 (18 mm), 3 mm deep (to y 103), inner corners r 0.5 mm | The camera FFC wraps round the top edge into J4 (user decision 2026-10-31: "J4 at an edge", no slot); a 16 mm cable has 1 mm each side |
| Lens cutout | Ø16 mm round cutout, centre (129.7, 130.3) | The camera module sits behind the board (bottom side) with its lens on this axis; see [Camera and CSI-2](#camera-and-csi-2) |
| Mounting holes H1–H4 | M3, Ø3.2 mm plated, Ø6.4 mm GND pad, at (103.5, 103.5), (160.5, 103.5), (103.5, 160.5), (160.5, 160.5): 3.5 mm in from both edges | User decision 2026-10-31: "4 × M3 corners" |
| Tooling holes TOOL1–TOOL3 | Ø1.152 mm NPTH, 0.148 mm mask expansion, at (101.6, 108), (162.6, 152), (106.4, 155.8) | Required for Economic PCBA; see [Assembly](#assembly) |
| Other NPTH | J1 pegs Ø3.25 mm (2), J2 pegs Ø0.6 mm (2) | Part datasheets |

M3 washer clearance (ISO 7089 M3 washer Ø7.0 mm, r 3.5 mm + 0.3 mm): no copper other than GND lies within r 3.8 mm
of H1's centre (closest: the +3V3 loop round J2's north side, 3.807 mm); H2–H4 have no non-GND copper within
r 4.6 mm.

The board carries "espion-cam rev 0.1" and "2026-11-01" on the bottom silkscreen.

### Stack-up

JLCPCB stack-up JLC04161H-7628, 4 layers, 1.6 mm, ENIG, green mask. Source: JLCPCB "Controlled Impedance PCB Layer
Stackup" (https://jlcpcb.com/impedance) and the impedance calculator (https://jlcpcb.com/pcb-impedance-calculator),
both read 2026-10-31.

| Layer | Material | Thickness (mm) | Dk | Use on this board |
|---|---|---|---|---|
| F.Cu (L1) | Copper 1 oz | 0.035 | | Parts (all machine-placed parts), signals, power |
| Prepreg | 7628, RC 49 % | 0.2104 | 4.4 | |
| In1.Cu (L2) | Copper 0.5 oz | 0.0152 | | GND plane, except the PoE lane ([Ethernet and PoE isolation](#ethernet-and-poe-isolation)) |
| Core | FR-4 (the calculator assumes Nan Ya NP-155F) | 1.065 | 4.6 | |
| In2.Cu (L3) | Copper 0.5 oz | 0.0152 | | Unbroken GND plane, no tracks |
| Prepreg | 7628, RC 49 % | 0.2104 | 4.4 | |
| B.Cu (L4) | Copper 1 oz | 0.035 | | Bottom parts (U2, U6, J1), signals |
| Solder mask | LPI, both sides | 1.2 mil over substrate, 0.6 mil over copper | 3.8 | Covers every impedance-controlled track |

- JLCPCB labels the finished thickness 1.59 mm ± 10 % (1.43–1.75 mm); KiCad sums the layers to 1.617 mm.
- Why this stack-up: it is JLCPCB's default ("No requirement" on the order page), the only 4-layer 1.6 mm stack-up its
  calculator tags "Standard", it costs no stack-up fee, and Economic PCBA accepts "standard stack-up only"
  (https://jlcpcb.com/capabilities/pcb-assembly-capabilities, read 2026-10-31; JLCPCB does not publish which named
  stack-ups count as standard).
- The loss tangent 0.02 in KiCad's stack-up is a placeholder; JLCPCB publishes none. It does not affect geometry.
- Via heights count in every length on this board (`use_height_for_length_calcs`): one through via adds 1.5512 mm.

## Fabrication rules

Board rules in the board setup, with the JLCPCB figure behind each. Source unless stated: JLCPCB PCB capabilities,
https://jlcpcb.com/capabilities/pcb-capabilities, read 2026-10-31 (4-layer, 1 oz outer, 0.5 oz inner).

| Rule | Board value | JLCPCB figure | Notes |
|---|---|---|---|
| Minimum track width / clearance | 0.1 / 0.1 mm | Multilayer 0.09 / 0.09 mm | The design's narrowest track is 0.15 mm and its closest copper 0.15 mm (`kicad_query stats`) |
| Copper to board edge | 0.3 mm | Routed edge 0.2 mm; PCBA FAQ part 2 (https://jlcpcb.com/help/article/pcb-assembly-faqs-part-2, read 2026-10-31): "edges of the traces and components must be more than 0.3mm away from the edge" (≈ 0.2 mm routing tolerance at depanelling) | Also the plane pull-back at the FFC notch and the lens cutout |
| Via size | Minimum Ø0.45 mm, hole 0.2 mm | No extra cost for a 0.2 mm hole with Ø ≥ 0.45 mm, or a hole ≥ 0.3 mm (quote page, 2026-10-31) | Used: 0.45/0.2 (294 vias), 0.6/0.3 (32: power and thermal), 0.8/0.4 (2: +5V_POE_F at the L1/C13/C14 filter output), 0.5/0.25 (1: +5V beside U8) |
| Via annular ring | 0.075 mm | Via 0.25 mm on a 0.15 mm hole allowed | 0.45/0.2 vias have 0.125 mm |
| Hole to copper | 0.2 mm (rule "PTH hole to copper": 0.3 mm for through-hole pads) | Via hole to track 0.2 mm; inner-layer PTH hole to copper 0.3 mm | |
| Hole to hole | 0.2 mm (rule "Pad hole to hole": 0.45 mm between pads) | Via hole-to-hole 0.2 mm; pad hole-to-hole 0.45 mm | |
| PTH annular ring | | ≥ 0.15 mm absolute, 0.20 mm recommended | J1 signal pins 0.28 mm, J1 shield tabs 0.36 mm |
| NPTH | | ≥ 0.5 mm | Smallest: J2 pegs 0.6 mm |
| Plated slot | 0.35 mm minimum groove | 0.35 mm (multilayer) | J2 shell slots 0.6 × 1.4 / 0.6 × 1.7 mm |
| SMD pad to pad, different nets | | 0.15 mm | Rule "U12 LGA pads" ([Custom rules](#custom-rules)) |
| Solder mask | Openings 1:1 with pads; mask-to-copper 0.09 mm | "Pad size:soldermask opening can be 1:1"; keep 0.09 mm between openings and neighbouring traces; mask bridge 0.10 mm (green, 1 oz) | Vias tented on both sides |
| Silkscreen | Line ≥ 0.15 mm, text ≥ 1.0 mm high with 0.15 mm stroke | Line width ≥ 0.15 mm, text height ≥ 1.0 mm, width:height 1:6, pad to silkscreen 0.15 mm | All footprint silk lines and texts are 0.15 mm or wider |
| Silk clearance | 0.15 mm, but 0 between two silk items (rule "Silk to silk") | No silkscreen-to-silkscreen figure | See below |
| Tolerances | | Track width ± 20 %, board thickness ± 10 %, hole position ± 0.075 mm | Width tolerance drives [Impedance](#impedance) |

Vias next to pads: the quote page turns "tented" into "plugged" for free, but JLCPCB plugs a via with ink only if it
is ≥ 0.35 mm from any pad opening (https://jlcpcb.com/help/article/pcb-via-covering). Vias are never placed in pads;
where a via must sit closer, its drilled hole stays ≥ 0.10 mm (the mask bridge) outside the pad opening: C15.2's via
0.125 mm, C37.2's 0.107 mm, the C40.2/U11.2 via 0.100/0.130 mm, the U6.55 via beside C47.2 0.120 mm (all GND, same net
as the pad).

Silk against pads: KiCad 10 applies `min_silk_clearance` only between silk items and checks silk against pads only for
overlap on the mask layer, so DRC does not enforce JLCPCB's 0.15 mm pad-to-silkscreen. The stock footprint silk stays
where it is closer: 0402 resistor and capacitor outlines 0.085 mm from their own pads, test-point rings 0.125 mm, J4's
outline 0.131 mm from its mounting pad; JLCPCB clips legend at pads (user decision 2026-11-02: "Keep stock silk, JLC
clips it"). Polarity marks and board texts are held ≥ 0.15 mm from pads by hand ([Assembly](#assembly)). Because JLCPCB
gives no silk-to-silk figure, the rule "Silk to silk" lets two silk items touch; without it, widening the stock silk to
0.15 mm made 15 pairs of neighbouring 0402/SOT outlines 0.13–0.145 mm apart, which KiCad would flag.

## Net classes

Track widths and diff-pair geometry come from the net classes; the spacing that depends on what is next to a net is in
[Custom rules](#custom-rules). Temperature rises use Brooks & Adam's curve fit to the IPC-2152 data for an external
layer, as coded in KiCad's source (`track_width_calculations.cpp`, coefficients `EXTERNAL_COEFFS {215.3, 2.0, −1.15, −1.0}`,
https://docs.kicad.org/doxygen/track__width__calculations_8cpp_source.html, read 2026-11-01): ΔT = 215.3 · I² · W^−1.15 ·
Th^−1, W and Th in mil, ΔT in °C. (The released KiCad 9 track-width calculator uses IPC-2221 instead, which gives lower
rises, e.g. ≈ 1.1 °C instead of 1.6 °C for 0.65 mm at 0.66 A; the fit is the more conservative of the two.)

| Class | Nets | Track | Clearance | Diff pair (width / gap) | Reason |
|---|---|---|---|---|---|
| Default | 77 signal nets (plus 45 single-pin nets of unconnected pins) | 0.2 mm | 0.2 mm | 0.2 / 0.25 (unused) | Twice JLCPCB's minimum; fits between 0.4–0.5 mm pitch pins after a short neck |
| PWR_3V3 | +3V3, CAM_3V3 | 0.65 mm | 0.2 mm | | Espressif ESP32-P4 Hardware Design Guidelines (HDG) §1.4.1: module supply "at least 25 mil"; 0.66 A gives ΔT ≈ 1.6 °C |
| PWR_5V | +5V, VBUS, +5V_POE, +5V_POE_F, +5V_BST | 1.0 mm | 0.2 mm | | DP1435 2.6 A maximum: ΔT 15.5 °C at 2.6 A, 8.8 °C at the 1.96 A board peak |
| LED_PWR | +32V, LED_K, ISNS, RING_1–RING_11 | 1.0 mm | 0.2 mm | | 0.915 A worst-case LED pulses ([led](led.md#current-sink)); up to 33.9 V, so exposed pads keep 0.4 mm ("LED_PWR pad spacing") |
| POE_CABLE | POE_VA1, POE_VA2, POE_VB1, POE_VB2 | 0.4 mm | 0.2 mm (2 mm to other nets by rule) | | Centre-tap current 350 mA (HanRun HR871150C p.1); routed on In1 only |
| MIPI_CSI | CSI_* and CAM_* pairs (12 nets) | 0.18 mm | 0.15 mm (0.36 mm to other nets by rule) | 0.18 / 0.15 | 100 Ω differential ([Impedance](#impedance)) |
| MDI | ETH_TD_P/N, ETH_RD_P/N | 0.18 mm | 0.15 mm (0.55 mm to other nets by rule) | 0.18 / 0.15 | 100 Ω differential |
| USB | USB_C_D_P/N (connector side), USB_D_P/N (module side) | 0.245 mm | 0.15 mm | 0.245 / 0.15 | 90 Ω differential (HDG §1.4.3) |

Tracks narrower than their class are allowed only where a reason is recorded; every such case is in
[Accepted deviations](#accepted-deviations).

## Custom rules

The `.kicad_dru` rules (`kicad_query rules`), in file order; the two MDI rules at the end were added last.

| Rule | Applies to | Constraint | Reason and source |
|---|---|---|---|
| MIPI_CSI spacing | MIPI_CSI copper against everything except its own partner; the two pads of each 0 Ω link R45–R47, R49–R51 exempt | Clearance ≥ 0.36 mm (2W) | HDG, PCB layout, MIPI: "maintain a minimum spacing of 2W between MIPI trace pairs". Keeps the GND pours ≥ 0.36 mm away too, which the 100 Ω geometry assumes. The link pads carry the two halves of one lane |
| MDI spacing | MDI copper against non-MDI copper | Clearance ≥ 0.55 mm (3W, 3 × 0.18 rounded up) | IP101G datasheet §6.2 p.56 "three times space". Exempt: MDI items touching U1's courtyard or lying wholly inside the F.Cu rule area "U1 MDI escape" (x 157.3–160.6, y 125.69–128.9), where U1's 0.4 mm pitch puts pins 25 (ISET), 28 (REGOUT) and 31 (AVDD33) 0.2 mm from the MDI pads ([Ethernet and PoE isolation](#ethernet-and-poe-isolation)). A segment crossing the area edge is not exempt |
| PoE isolation | POE_CABLE copper against other nets, same layer | Clearance ≥ 2 mm | IEEE 802.3 clause 33.4.1 isolation (1500 V rms 60 s, or 2250 V dc, or 1500 V 10/700 µs impulse) between cable conductors and board; IEC 60664-1 (2007) tables interpolated for 2250 V dc give ≈ 1.53 mm clearance and 1.25 mm creepage; 2 mm adds margin. Pad pairs inside J1 exempt (HanRun's own 2250 V dc rating, HR871150C p.1). Copper across layers is not checked by KiCad: see [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| MIPI_CSI intra-pair skew | Each MIPI_CSI pair | Skew ≤ 0.127 mm | HDG §1.4.6 allows 10 mil (0.254 mm) P/N skew per lane; a lane is two pairs in series (U6 → 0 Ω link → J4), so each gets half |
| U12 LGA pads | Pad pairs inside U12 | Clearance ≥ 0.15 mm | MSA3S02 land pattern has 0.15 mm between neighbouring pads of different nets (corner lands shortened to 0.375 mm at their inner end for it); JLCPCB "SMD pad to pad clearance (different nets) 0.15mm", mask bridge 0.10 mm |
| LED_PWR pad spacing | An exposed pad of LED_PWR or BST_SW against any other net (not POE_CABLE or MDI, which have stricter rules) | Clearance ≥ 0.4 mm | Up to 33.9 V (TPS61170 set point, worst case). IPC-2221A Table 6-1, 31–50 V row: A6 (external component terminations, uncoated) 0.4 mm; B4 (solder mask) 0.13 mm, so mask-covered tracks keep the 0.2 mm class clearance. Exempt: pad pairs inside U10 (TI land pattern) and inside each LED D4–D15 (one forward voltage apart) |
| MIPI_CSI to clocks and high-speed | MIPI_CSI against USB, MDI, RMII_REF_CLK, RMII_REF_CLK_SRC | Clearance ≥ 0.54 mm (3W) | HDG §1.4.6: 3W from other high-speed and clock signals |
| PTH hole to copper | Through-hole pads | Hole clearance ≥ 0.3 mm | JLCPCB inner-layer PTH hole to copper 0.3 mm (PTH to track 0.28 mm minimum) |
| Pad hole to hole | Pad against pad | Hole to hole ≥ 0.45 mm | JLCPCB pad hole-to-hole spacing 0.45 mm (vias: 0.2 mm, the board minimum) |
| Power neck-down at fine-pitch pads | PWR_5V, LED_PWR and +5V_BST tracks touching the courtyard of U4, U8 or U10 | Track width ≥ 0.3 mm | A 1.0 mm class track cannot enter U4's 0.4 mm pads (0.3 mm gaps, TI TPS2121 land pattern p.39), U8's SOT-23-6 (0.95 mm pitch) or U10's 0.3 mm WSON pads (TPS61170 DRV0006A, p.35) at 0.2 mm clearance; a 1 mm long 0.3 mm neck is 1.6 mΩ, 9 mW at 2.4 A ([Power](#power)) |
| LED_PWR pad spacing (other pads) | An exposed pad of any other net against LED_PWR or BST_SW copper (tracks and vias included) | Clearance ≥ 0.4 mm | The mirror of "LED_PWR pad spacing", which only fires when the exposed pad is the LED_PWR item; same IPC-2221A source. Exempt: U10's own pads against BST_SW copper inside U10's courtyard (pin 4's 0.3 mm exit keeps TI's DRV0006A land gaps, 0.35 mm to pin 5 and 0.25 mm to the exposed pad) and ISNS_K against ISNS (the Kelvin tap through net tie NT1, same node, ≤ 0.1 V) |
| Silk to silk | Two non-pad items on the same silk layer | Silk clearance ≥ 0 mm | JLCPCB gives no silkscreen-to-silkscreen spacing; see [Fabrication rules](#fabrication-rules) |
| MDI pair to pair | RD pair (ETH_RD_P/N) against TD pair (ETH_TD_P/N); same U1 exemption as "MDI spacing" (either item touching U1's courtyard or wholly inside "U1 MDI escape") | Clearance ≥ 0.54 mm (3W) | IP101G §6.2 p.56 "three times space" also between the RX and TX pairs: 3 × 0.18 mm = 0.54 mm. Closest spot: the RD_N/TD_P diagonals at (159.2–159.6, 125.2–125.6), west of R6, 0.5399 mm (`kicad_query distance`), which DRC passes |
| MDI intra-pair skew | Each MDI pair | Skew ≤ 1.27 mm | TI SNLA387 p.2 "MDI Traces": length-match the two lines of a pair within 50 mil (1.27 mm) for 100BASE-TX |

## Impedance

Method: JLCPCB's impedance calculator (https://jlcpcb.com/pcb-impedance-calculator, read 2026-10-31) for stack-up
JLC04161H-7628, cross-checked with an independent 2-D quasi-static field solver (trapezoidal 35 µm track with etch-back,
JLCPCB's conformal mask: Dk 3.8, 1.2 mil over substrate, 0.6 mil over copper). Every pair runs on an outer layer over
the adjacent GND plane: F.Cu over In1, B.Cu over In2 (both 0.2104 mm away, so both give the same geometry).

| Use | Geometry (width / gap) | JLCPCB calculator | Field solver, with mask | At ± 20 % track width | Target |
|---|---|---|---|---|---|
| MIPI_CSI, MDI | 0.18 / 0.15 mm | 100 Ω at 0.1826 mm | 101.1 Ω (100.5 Ω with GND pour 0.36 mm away) | 88–114 Ω | 100 Ω ± 10 % (HDG, MIPI) |
| USB | 0.245 / 0.15 mm | 90 Ω at 0.2474 mm | 90.5 Ω | 76–105 Ω | 90 Ω ± 10 % (HDG §1.4.3) |
| Single-ended reference | 0.36 mm | 50 Ω at 0.3586 mm | 49.5 Ω | | Not used: RMII is short and runs at the Default width |

- The geometry holds only with solder mask over the pairs (without mask ≈ 109 Ω / 97 Ω) and a solid GND plane on the
  adjacent layer; `return_path` checks the plane under every MIPI_CSI, MDI and USB track.
- Odd-mode propagation delay ≈ 5.8 ps/mm (εeff ≈ 3.03, solver).
- Where it matters: MIPI CSI-2 at 800 Mbit/s per lane (edges 150–375 ps, critical length ≈ 4–11 mm). USB full speed
  (12 Mbit/s, edges ≥ 4 ns, critical length ≈ 115 mm) and 100BASE-TX MDI are electrically short on this board.
- No impedance control is ordered (user decision 2026-11-01: "No impedance control"). JLCPCB's ± 20 % width tolerance
  puts the MIPI pairs at 88–114 Ω at the extremes (field solve: 0.144 mm / 0.186 mm gap → 114.3 Ω; 0.216 mm /
  0.114 mm → 87.9 Ω), outside the HDG's ± 10 % there; a ± 10 % width spread gives 93.8–106.8 Ω. Controlled impedance
  (± 10 %, about USD 33 more) was offered and declined; whether Economic PCBA accepts it on this stack-up is not known.
- First-board check: run the CSI link at full rate (2 lanes, 800 Mbit/s per lane) and count frame errors before the
  design is released ([camera](camera.md#csi-2-link)).

## Routing style

User instruction (2026-11-01, repeated 2026-11-02, exact): "For the routing, youre only allowed to 45° bends, except when
its impossible otherwise. Avoid 90° bends." Applied as:

- every straight track segment runs at a multiple of 45°; no arc tracks;
- every bend (a direction change between two consecutive segments of one net on one layer, with no via at the joint)
  is exactly 45°; a 90° direction change is two 45° bends whose 45° leg is at least the track width; no acute bends;
- T-junctions and track ends in pads or vias are not bends; no bend sits inside a pad (turns that would land on a pad
  centre start inside the pad, off the centre);
- exceptions only where nothing else is possible, each recorded with location and reason.

Audit at the released design (a script over every track): 1541 tracks, 0 arcs, 0 off-angle segments, 0 non-45° bends,
0 short 45° legs. **No exceptions remain.** The MDI pin escapes at U1, which once needed slanted segments, are 45°-only
([Ethernet and PoE isolation](#ethernet-and-poe-isolation)); the CSI bus crosses its narrowest plane neck at 45° rather
than the ideal 58° ([Camera and CSI-2](#camera-and-csi-2)).

## Power

Parts: U2 SDAPO DP1435-5V PoE PD module (bottom, west edge, hand-soldered), L1/C13/C14 filter on +5V_POE_F, U4 TI
TPS2121 power mux at (115, 138.6), U3 TI TLV62569 3.3 V buck at (136, 148.6) with L2, U8 TI TPS22918 LED-supply switch
at (122.34, 145.85). Circuit: [power](power.md).

### Placement constraints

TPS2121 (SLVSEA3F §11 p.33 "as close as possible", §12.2 layout example):

| Pair | Max | Reason |
|---|---|---|
| C15.1–U4.7 | 3 mm | IN1 bypass |
| C16.1–U4.2 | 3 mm | IN2 bypass |
| C20.1–U4.1, C23.1–U4.8 | 3 mm | OUT bypass |
| R22.1–U4.10 | 3 mm | R_ILM beside ILM, as the layout example draws it |

U4's periphery: OUT pins 1 and 8 tied straight across; C20/C23 on the west OUT flare, C16 west, C15 east, C21 (SS) and
R22 straight north of U4.11/U4.10, the OV/PR1 divider row (R20, R21, R18, R75, R19) south with a GND via between R21 and
R18.

TLV62569 buck loop (SLVSDG1C §10.1 p.13, Fig. 20; U3 is turned 180° against Fig. 20). Each `max` is the placed distance
rounded up, so the loop shape cannot drift:

| Pair | Max | Placed | Reason |
|---|---|---|---|
| C10.1–U3.4 | 1.0 mm | 0.515 mm | Input capacitor's VIN end at VIN |
| C10.2–U3.5 | 1.0 mm | | C10's GND end away from SW, beside the FB row where the GND copper and vias are |
| C12.1–U3.4 | 2.5 mm | 2.43 mm | 100 nF input capacitor beside C10 |
| C12.2–C10.2 | 1.0 mm | 0.747 mm | GND ends together |
| C18.2–U3.2 | 1.0 mm | 0.668 mm | Output capacitor's GND end at the GND pin (C2 in Fig. 20) |
| C18.1–L2.2 | 3.5 mm | | Output capacitor's +3V3 end toward the inductor output |
| C19.1–C18.1, C19.2–C18.2 | 1.0 mm | | C19 beside C18 |
| L2.1–U3.3 | 2.5 mm (and 3 mm) | 2.25 mm | Short SW node |
| R16.2–U3.5, R17.1–U3.5, C17.2–U3.5 | 3 mm | | FB divider and feed-forward capacitor at FB |

C10 (10 µF 0805) sits at VIN and C12 (100 nF 0402) outboard. TI's Fig. 20 has one input capacitor, in C10's role; C12
is an extra with little high-frequency benefit where it is. Swapping them does not fit (TP12, the +5V trunk vias and
the RMII_TX_EN diagonal block it).

### Vias at the mux

`copperArea` entries, counted on F.Cu within 0.5 mm of U4's pads (TPS2121 §12.1: vias under the input and output pins;
§12.2 p.33 draws two vias per IN pin and four on OUT; land pattern p.39 "VIA TYP" in pads 1, 2, 7, 8; one via per
ampere for the 1.8–2 A input and output):

| Entry | Net | Min vias | Placed |
|---|---|---|---|
| U4 IN1 vias | +5V_POE_F | 2 | 0.45/0.2 at (116.45, 139.75) and 0.6/0.3 at (116.45, 140.35) |
| U4 IN2 vias | VBUS | 1 | 0.6/0.3 at (113.4, 139.65) |
| U4 OUT vias | +5V | 2 | 0.45/0.2 at (116.4, 137.9) and (116.3, 137.3) (a 0.6 mm via there breaks 0.2 mm to C15.2 or U4.9) |

+5V_POE_F leaves the L1/C13/C14 filter for the B.Cu trunk through two 0.8/0.4 vias at (106.6, 138.45) and
(106.6, 139.45).

### Track widths

| Run | Width | Current | ΔT (fit in [Net classes](#net-classes)) |
|---|---|---|---|
| +5V and +5V_POE trunks, +5V_POE_F on F.Cu | 1.0 mm | 1.96 A board peak, 2.6 A DP1435 maximum | 8.8 °C / 15.5 °C |
| +5V_POE_F B.Cu leg, 12.3 mm, from the filter vias (106.6, 138.45)/(106.6, 139.45) to U4's IN1 vias (1.0 mm does not pass the VBUS IN2 via at (113.4, 139.65)) | 0.8 mm | 1.96 A | 11.4 °C |
| +5V into U3: the 1.0 mm B.Cu trunk ends at via (139.3, 150.8), then F.Cu to C10.1 and on to U3.4 | 0.6 mm | ≤ 0.52 A (buck input peak) | ≈ 1.1 °C |
| C10.1 → C12.1 link and C12.1 → via (140.3, 150.8), which joins the U11/R63 branch | 0.5 mm | C12's ripple current and < 1 mA | |
| +5V B.Cu branch from via (140.3, 150.8) to R63/C44/U11 at (149.45, 145.1) | 0.8 mm | < 1 mA (U11 supply and the R63 divider) | |
| +5V neck from the trunk vias (122.45, 146.9) and (122.35, 146.2) into U8.1, 1.07 mm long, and U8.1 → C34.1 | 0.6 mm (0.3 mm for the 0.7 mm stub from via (122.35, 146.2) into via (122.45, 146.9) inside U8's courtyard) | ≈ 1.78 A average, 1.96 A peak | ≈ 16 °C as an infinite track (upper bound; a 1 mm neck between a via pair and a pad runs much cooler) |
| +3V3 trunks | 0.65 mm | 0.66 A | 1.6 °C |
| Necks inside the U4, U8 and U10 courtyards | ≥ 0.3 mm | | 1 mm of 0.3 mm is 1.6 mΩ, 9 mW at 2.4 A |
| Branches carrying no load current (enable pins, dividers, FB sense, op-amp supply, pull-up feeds, short stubs into pins) | 0.2–0.4 mm | µA–mA | |

The +3V3 feed to the north and west parts (U5, C25, R53 → CAM_3V3/J4, R41/R42, D1/C24, R20) cannot take a shorter
route: the USB pair, the camera bundle, XVS and +5V_POE_F fence it on B.Cu, the PoE lane and U2 on F.Cu. It is fed from
the U3/U6 area by a 0.3 mm B.Cu feed (15 mm), a 0.4 mm loop along the west edge (40 mm) and a 0.4 mm F.Cu loop round
J2's north side (46 mm, 0.2 mm for 8.5 mm past H1): ≈ 0.125 Ω, ≈ 19 mV drop at 150 mA (camera 100 mA, D1 36 mA,
pull-ups), < 1 °C rise.

### Ground and return

- In2 is one unbroken GND plane; In1 is GND everywhere except the PoE lane. Both pours: clearance 0.2 mm (raised by the
  custom rules near MIPI, MDI and PoE copper), thermal reliefs 0.5 mm gap / 0.5 mm spokes, islands removed. Filled area
  In1 3160 mm², In2 3366 mm².
- Every decoupling capacitor's GND pad has a GND via within 1 mm on its own short stub, never in the pad. The buck
  capacitors C10, C12, C18 and C19 have two each; U3.2 has one under the body and joins C18.2 and C10.2 on F.Cu.
- `decoupling_placement` (decoupling capacitor ≤ 3 mm from its power pin, GND via ≤ 1 mm from its GND pad) is clean
  except C50 ([IMU](#imu)).

## MCU and USB

Parts: U6 Waveshare ESP32-P4-Core module on the bottom at (139, 150.75), courtyard x 125.25–152.75, y 139–162.5,
hand-soldered with the hand-solder footprint variant ([mcu](mcu.md#module)); J2 HRO TYPE-C-31-M-12 USB-C at the top edge;
U5 USBLC6-2 and D2 TSD05 ESD protection at J2; SW1 (RESET) and SW2 (BOOT) on the bottom edge. Circuit: [mcu](mcu.md).

### Placement constraints

| Pair | Max | Reason |
|---|---|---|
| C29.1–U6.75 | 3 mm | HDG §1.4.1: capacitor at the power entry (ESP_3V3) |
| C28.1–U6.76 | 3 mm | HDG §1.4.1: 0.1 µF per power pin |
| R33.2–U6.1, C27.1–U6.1 | 3 mm | HDG §1.3.3: RC at CHIP_PU (ESP_EN), short trace |
| R31.2–U6.38, R32.2–U6.37 | 3 mm | HDG §1.3.12 / §1.4.3: USB series resistors at the chip end (D+ / D−) |
| C48.1–U6.19 | 3 mm | HDG §1.3.10: 0.1 µF at an ADC pin (NTC_SENSE) |
| C49.1–U6.17 | 3 mm | HDG §1.3.10: 0.1 µF at an ADC pin (VLED_SENSE, GPIO21 / ADC1_CH5); placed 2.0 mm |
| U5–J2.A6 | 3 mm | USBLC6 Rev 5 §2.3 p.5–6: protection as close as possible to the connector, short I/O tracks |
| D2.1–J2.A4, D2.1–J2.A9 | 5 mm | TI SLVA680A p.4 (cited by the TSD05 datasheet §6.1): "place the TVS as near to the connector as design rules allow"; 5 mm reaches both VBUS pad pairs |
| D16.1–SW2.1 | 3 mm | D16 (TSD05) at the BOOT button's exposed contact, same source; its anode has a GND via |
| BOOT stub: R36.1 → R39.1 and R36.1 → R38.2 (`paths`) | 3 mm each | The BOOT strap R39 and pull-up R38 hang on the 50 MHz RMII_TXD1_MCU line; their stubs stay short (3 mm ≈ 17 ps, negligible against the edge). Routed 0.94 mm and 2.00 mm |

GPIO assignments that moved to make the routing possible are listed in
[firmware rules](firmware-rules.md#gpio-changes-in-this-revision).

### USB

- J2 at (112.6, 103.955), rotated 180°, locked, machine-placed; its shell front (F.Fab) is at y 100.305, 0.3 mm inside
  the top edge (user decision 2026-11-01: "Keep machine-placed, 0.3 mm inside"; JLCPCB PCBA FAQ part 2: components
  ≥ 0.3 mm from depanel-routed edges). HRO's drawing p.1 lets the shell overhang the edge by ≈ 0.46 mm; here it sits
  ≈ 0.77 mm further in, so a plug overmold taller than ≈ 2.6 mm stops on the FR-4 edge ≈ 0.25 mm before full insertion
  ([mcu](mcu.md#usb-c)). The silk line across the mating face was removed from the footprint so silk stays ≥ 0.15 mm
  from the edge.
- Connector side (USB_C_D_P/N, J2 → U5 → R31/R32): 0.2 mm tracks, mostly uncoupled (≈ 37.7 mm coupled at 0.26–0.30 mm
  on B.Cu, 1.3 mm on F.Cu), D+ 80.18 mm with 4 vias, D− 73.66 mm with 3 vias. Full speed is electrically short here (edge ≥ 4 ns, critical
  length ≈ 115 mm), so width, coupling and the 6.5 mm skew don't matter; impedance is not controlled.
- Module side (USB_D_P/N, R31/R32 → U6.38/U6.37): 8.58 / 7.01 mm, one via each, sharing the GND return via
  (147.5, 138.85) (HDG §1.4.3: return vias at USB via transitions).
- USB vias near U5 lie at x ≥ 113.78 and y ≤ 113.5, 2 mm from the In1 PoE copper.
- USB_C_D_N's J2 branch leaves U5's pocket on B.Cu through a via at (114.1, 109.0); no GND via fits within 1 mm of it
  (nearest: D2.2's at (116.6, 109.9), 2.66 mm), In2 is solid GND under it. USB_C_D_P's 2.8 mm link to U5 crosses the In1
  gap of the PoE lane, with In2 GND below. Both are accepted for full-speed edges ([Accepted deviations](#accepted-deviations)).
- VBUS: 0.25 mm F.Cu stubs from J2.A9/A4 north to two vias (the only exit between the NPTH pegs and the neighbouring
  pads), a 0.8 mm B.Cu link, then a 1.0 mm B.Cu trunk at x 112.8 to U4's IN2 via. ΔT ≈ 11 °C per stub at 1.0 A each,
  3 °C at 0.5 A (1.8 mm long).
- U5.5 (clamp supply) reaches C25.1 on F.Cu; U5.2 has a GND via between the pin rows.

### Buttons and boot

- SW1 (RESET) at (124.8, 160.8) and SW2 (BOOT) at (113, 160.8) stay on the bottom edge (user decision 2026-11-01: "Keep
  on the bottom edge"); the enclosure must give access to both there. Labels "BOOT" and "RST" sit between them.
- BOOT_SW (60 mm, 2 vias) runs along the bottom edge on B.Cu at y 163.4, 1.05 mm below U6's bottom pad row and 0.5 mm
  from the edge, tented.

### Module pads

- U6's GND pads 20, 27, 34 and 55 have their own GND vias. U6.55's via (149.5, 157.03) touches C47.2's pad edge (same
  net; the REF_CLK F.Cu at y 156.5 leaves no other spot), U6.27's via (138.1, 142.05) sits 0.1 mm from U9.1 (same net).
- Top-side reflowed parts lie above U6's pads (C47, C50, U12, R37 among them). U6 is soldered with an iron only, pad by
  pad, no hot air (user decision 2026-11-01: "Iron only, pad by pad"), so they are not reflowed again.

## Camera and CSI-2

Parts: J4 JUSHUO AFA07-S15FCA-00 (bottom-contact 15-way 1.0 mm FFC connector) at (129.7, 106.5), rotated 180°, at the
notch; the six 0 Ω links R45–R47, R49–R51; the camera module behind the board on the lens axis. Circuit and cable:
[camera](camera.md); cable orientation: [camera](camera.md#ffc-orientation).

### Keep-outs

| Rule area | Layers | Keeps out | Region | Reason |
|---|---|---|---|---|
| Camera module | B.Cu | Footprints | x 117.25–142.2, y 114.5–138.35 | Module 25 ± 0.2 × 23.86 ± 0.2 mm around the lens axis, FFC edge north at y 114.5 (module drawing WX-1M-9281-79). U2's body face is ≈ 0.5 mm from the module's west edge (0.3 mm at the +0.2 mm module tolerance); J1's body starts at x 142.8 (0.6 mm) |
| FFC lane | B.Cu | Footprints | x 121–139, y 100–114.5 | The cable's path from the notch to the module edge (16 mm cable; width assumed, not on the drawing) |
| FFC notch | F.Cu, B.Cu | Tracks, vias, pours | x 119.7–139.7, y 100–104 | Cable wrap at the edge |
| FFC notch inner | In1, In2 | Tracks, vias | Same | The In1/In2 GND pours run to the board-edge clearance, y 103.30 (user decision 2026-11-01: "Pull planes to 0.3 mm") |
| Lens cutout | All copper | Tracks, vias, pads, pours, footprints | r 8.2 mm round the axis | The Ø16 cutout plus margin; the planes end at r 8.3 mm (edge clearance 0.3 mm) |
| CSI link vias south | F.Cu | Footprints | x 135.6–143.5, y 113.25–114.45 | Keeps the CSI via field free |
| CSI link vias north | F.Cu | Footprints | x 141.3–143.2, y 109.5–111.2 | Keeps the CAM via-2 corner free |

### Lens view cone

`heightZones` entry "lens view cone": centre (129.7, 130.3), r0 7.0 mm, slope 0.83. A top-side part h mm tall must keep
its courtyard ≥ 7.0 + 0.83 · h mm from the lens axis, so it stays out of the lens's field of view:

- slope: tan(79.3° / 2) = 0.829 (lens view angle, module drawing);
- r0 = lens rim 2.5 mm + 0.83 × 5.4 mm: the lens tip may sit up to 5.4 mm below the top face (user decision 2026-10-31:
  "Tip ≤ 5.4 mm below top face"; the module's front face is ≥ 2.1 mm behind the board because its FFC connector, 2.1 mm
  high, is on the lens face);
- bottom-side parts (U2, U6, J1) do not count.

Heights used (datasheet maxima, mm): 8 × 10.5 electrolytic 11.0; 10 × 10 electrolytic (C46, C47) 10.8; 6.3 × 9
electrolytic 9.5; 6.3 × 5.8 electrolytic 6.5; test loop 4.72; USB-C 3.36; FXL0630 and FNR4030S inductors 3.0; JST SH
2.9; J4 2.6; IR LED 2.55; SMA diode 2.45; TO-252 2.39; 1206 capacitor 1.9; TS-1187A button 1.55; 0805 capacitor and
SOT-23-5/6 1.45; SOD-323 and TPS2121 RUX 1.0; YY-0807 LED and MSA3S02 0.95; 0603 capacitor and QFN-32 0.9; WSON-6 0.8;
0805/1206 resistors 0.65; crystal 0.6; 0402 parts and 0603 resistors 0.55; test pads and the net tie 0. The 12 ring LEDs
(r 11.5 mm, need ≥ 9.1 mm) and every other top part pass; `layout` is clean.

### Lane order and links

The ESP32-P4 cannot remap CSI lanes or swap P/N (ESP32-P4 TRM v0.7 Register 42.2 p.2611, §42.5.3.2 p.2603), and the
camera's lane order at J4 differs from U6's:

- U6 pads west → east: D0_N, D0_P, CLK_P, CLK_N, D1_N, D1_P.
- J4 west → east: CLK_P (pin 9), CLK_N (8), D1_P (6), D1_N (5), D0_P (3), D0_N (2).

Each lane is two pairs in series: CAM_* from J4 to a 0 Ω link, CSI_* from the link to U6. The six links stand in U6
order in one row at y 112.25, pitch 1.04 mm: R51 D0_N x 137.5, R47 D0_P 138.54, R45 CLK_P 139.58, R49 CLK_N 140.62, R50
D1_N 141.66, R46 D1_P 142.70. The reordering is done on B.Cu between J4 and the links, without crossings:

- D1 runs north of J4's pads, east under J4 and back south into its links; the U-turn mirrors the pair, so P (west at J4)
  arrives east, as R50/R46 need.
- CLK runs on F.Cu north under J4 to its vias at x 133.7; on B.Cu N wraps west of the P via to swap sides, then the pair
  runs east and south into its links.
- D0 goes south of J4's pads as a B.Cu serpentine (west to x 129.33 and back); the U-turn mirrors the pair, so D0_N (east
  at J4) arrives west, as R51/R47 need.

### Via fields and return vias

All vias 0.45/0.2 mm. Via counts are equal within each pair (`viaParity`, TI SPRAAR7J §3.9 p.12):

| Segment | Vias per leg | Where |
|---|---|---|
| CAM (J4 → links) | 2 | Via 1 under J4's body north of its pad (y 108.45; CLK at (133.7, 106.95)/(133.7, 107.55)); via 2 north of the link row at y 110.5 |
| CSI (links → U6) | 1 | South of the link row at y 113.4 (D0_N at (136.75, 112.85), fed west from R51.1, because the PoE lane leaves no legal spot for its return via at (137.5, 113.4)) |

Every signal via has a GND through via within 0.95 mm centre to centre (`via_return` limit 1.0 mm; TI SPRAAR7J §3.9
p.12: a return via at each transition, p.8: stitching vias "within 200 mils" of the signal vias). Neighbouring legs share
one where the in-row spots are taken, e.g. (133.7, 108.45) serves CAM_D1_N, CAM_D0_P and CAM_CLK_N. The GND vias under J4
at x 127.7/130.7/133.7/136.7 have F.Cu stubs to J4's GND pads 10/7/4/1.

### CSI bus and the plane neck

The CSI legs run on B.Cu over In2 to U6.28–U6.33 as a three-pair bus, pair centres 0.87 mm apart, CLK between D0 and
D1. Spacing: ≥ 0.36 mm (2W) to everything but the partner, ≥ 0.54 mm (3W) to USB, MDI and REF_CLK (rules above). The CLK
pairs have no GND guard copper (HDG §1.4.6 p.28 asks for the MIPI CLK pair to be surrounded by ground copper); a B.Cu guard
each side would widen the bus to ≈ 3.37 mm and leave ≈ 1.41 mm a side at the neck below instead of 1.97 mm (user decision
2026-11-01: "Accept 2W, no guard").

The bus passes through a neck of the In2 reference between the lens plane edge (r 8.3 mm) and the 2 mm PoE antipad of
J1.9 (144.31, 121.19): 17.22 − 8.3 − 2.725 = 6.19 mm wide, against the 6.83 mm (2.25 mm bus + 2 × 2.29 mm) that TI
SPRAAR7J p.9's 90 mil from bus to plane edge asks for. Widening it would mean moving J1 (locked at the edge) or the lens
and ring (approved floorplan). The bus crosses the neck at 45° (route rule), corners (138.45, 123.14), (139.21, 123.90),
(139.21, 124.14), (141.5, 126.43): measured track edge to plane edge 1.841 mm (72.5 mil) to the lens edge and 1.842 mm
to J1.9's antipad (user decision 2026-11-01: "Use 45°, accept 71.7 mil"; the ideal 58° crossing gave 77 mil).

At the FFC notch the In1/In2 planes end at y 103.30. Track edge to that plane edge: CAM_D1_P 1.375 mm (54 mil, over
≈ 5.5 mm under J4), CAM_D1_N 1.705 mm (67 mil), CAM_CLK_N 2.245 mm (88 mil), CAM_CLK_P 2.265 mm (89 mil), against
SPRAAR7J p.9's 90 mil (2.286 mm). Only the D1 pair is materially short; meeting it needs a new crossing plan under J4.
Accepted with the user decision above.

The J1 antipads in In2 are voids, not plane edges, and SPRAAR7J p.9 asks for ≥ 1.5 W (0.27 mm) from voids: CSI_D1_P
passes J1.3's ETH_CT void at 0.958 mm, J1.5's antipad at 1.227 mm, J1.7's at 1.566 mm and J1.9's at 1.972 mm.

The planes stay solid under the wide pads on the lanes: In1/In2 run unbroken under the six link pads (0.54 × 0.64 mm)
and J4's MIPI pads 2, 3, 5, 6, 8, 9 (0.6 × 1.8 mm), against TI SPRAAR7J §3.10 p.13 ("voiding the SMD mounting pads of the
reference plane by 100%"). Estimated ≈ 0.07 pF excess per link pad and ≈ 0.2 pF per J4 pad, a 1–3 % reflection at a
150 ps edge (estimate, not measured); accepted at design review 2026-11-01, covered by the first-board CSI test.

### Lengths and skew

Tracks 0.18 mm, gap 0.15 mm. Skew compensation uses trapezoid bumps (45° legs, a flat ≥ 0.45 mm, ≥ 0.45 mm of straight
track between same-sense bends, no copper overlap), placed next to the mismatch, so the pad-to-pad copper path equals
the net length. Lengths (via heights counted; pad to pad equals net length on every MIPI net):

| Pair | P (mm) | N (mm) | Skew (mm), ≤ 0.127 |
|---|---|---|---|
| CAM_CLK | 24.7990 | 24.8295 | −0.031 |
| CAM_D0 | 24.7578 | 24.8290 | −0.071 |
| CAM_D1 | 24.7556 | 24.6438 | +0.112 |
| CSI_CLK | 32.4364 | 32.4364 | 0.000 |
| CSI_D0 | 32.5014 | 32.4737 | +0.028 |
| CSI_D1 | 32.4931 | 32.4931 | 0.000 |

`lengthGroups` "CSI pairs, P4 side" and "CAM pairs, FPC side": pair-mean spread ≤ 0.381 mm per segment (HDG §1.4.6
pair-to-pair ≤ 30 mil = 0.762 mm end to end, half per segment). Spread: CAM 0.115 mm, CSI 0.057 mm.

### Other parts near the link field

C3 (ETH_CT, 100 nF) at (142.3, 115.4) ties its GND end to the GND via (141.14, 114.1) by a 0.2 mm F.Cu track; ETH_CT runs
on F.Cu from C3.1 to J1.3 (2.44 mm). J4 is marked "1" and "CONTACTS DOWN" on the top silk.

First-board check: full-rate CSI frame-error test ([Impedance](#impedance)); FFC fit, bend and insertion depth
([camera](camera.md#ffc-orientation)).

## Ethernet and PoE isolation

Parts: U1 IP101GRI PHY at (158.6, 130.8) with its MDI pins north; J1 HanRun HR871150C MagJack on the bottom at
(144.31, 111.03), rotated −90°, locked, hand-soldered; Y1 25 MHz crystal at (153.8, 128.7); U2 PoE PD module. Circuit:
[ethernet](ethernet.md), [power](power.md#poe-input).

### MDI

- U1 sits next to J1 so MDI is short (IP101G §6.2 p.56: keep vias out of MDI except "in close proximity to device"). MDI
  runs on F.Cu, 0.18 mm tracks at 0.155 mm gap, from U1 up the east strip (route reservations x 157.3–160.6,
  y 122–128.9 and x 158.3–161, y 106.8–122) and between J1's pegs and LED pins to J1 pins 1, 2, 4, 5.
- The two pairs are swapped at J1 so they reach it without crossing (U1.30/29 → J1.4/5, U1.27/26 → J1.1/2; net names
  follow the J1 pin); this relies on the PHY's auto-MDIX, which firmware must never disable
  ([ethernet](ethernet.md#magjack-and-mdi), [firmware rules](firmware-rules.md#rules)).
- Inside each pair P must end north of N at J1 but leaves U1 on the west, so P hops once under N: two 0.45/0.2 vias and a
  1.33 mm B.Cu hop, 6–8 mm from U1's pins: RD_P vias (158.788, 122.105)/(159.728, 121.165), TD_P vias
  (160.298, 122.055)/(161.238, 121.115). N stays on F.Cu. RD_P to TD_P is ≥ 0.607 mm via to via.
- Spacing: MDI keeps 0.55 mm (3W) to other nets (rule "MDI spacing") and RD keeps ≥ 0.54 mm (3W) from TD edge to edge
  (rule "MDI pair to pair"; closest the RD_N–TD_P diagonals at (159.2–159.6, 125.2–125.6), west of R6, at 0.5399 mm, which DRC passes). At U1's north row the 0.4 mm pitch makes 3W impossible: pins 25 (ISET), 28
  (REGOUT) and 31 (AVDD33, +3V3) sit 0.2 mm from the MDI pads and their tracks leave between or beside the pairs. Each MDI
  pin escapes 0.55 mm north inside U1's courtyard, then 45°-only inside the F.Cu rule area "U1 MDI escape"
  (x 157.3–160.6, y 125.69–128.9), where the rule "MDI spacing" is waived. Inside it only U1's own pin nets come closer
  than 0.55 mm: the pads 0.2 mm (land pattern), the REGOUT track 0.21 mm and its via (158.8, 126.9) 0.286 mm, ISET's
  vertical 0.2175 mm, +3V3 0.21–0.25 mm, the PHY_RST_N via (156.95, 126.85) 0.5275 mm. Every MDI point outside the area
  is ≥ 0.55 mm from other copper (DRC). This is a deviation from IP101G §6.2's "three times space", which has no
  exception.
- Lengths (via heights counted; pad to pad equals the net length): TD_P / TD_N 32.9423 / 32.9423 mm, RD_P / RD_N
  27.4155 / 27.4156 mm, skew ≤ 0.0001 mm, within TI SNLA387 p.2's 50 mil (1.27 mm) for 100 Mbit/s, which the rule "MDI
  intra-pair skew" enforces. TD_N carries three 45° bumps and RD_N one for the matching.
- Return path: the N tracks pass the swap vias' In1 antipads (r 0.775 mm) and the tails at J1.2/J1.4 cross J1's own MDI
  antipads; accepted ([Accepted deviations](#accepted-deviations)).

### RMII

| Constraint | Entry | Value | Reason |
|---|---|---|---|
| Series resistors at the driving pin | `near` R35.1–U6.46, R36.1–U6.47, R37.1–U6.67 (TXD0, TXD1, TX_EN at the MAC); R11.1–U1.18, R12.1–U1.17, R15.1–U1.19 (RXD0, RXD1, CRS_DV at the PHY); R13.1–U1.14 (REF_CLK series resistor, HDG §1.3.14, at the PHY's 50M_CLKO) | ≤ 3 mm | 3 mm ≈ 17 ps at 5.8 ps/mm, far below any edge ≥ 0.5 ns: the resistor works as a lumped source termination |
| Long data segments | `lengthGroups` "RMII data, long segments": RXD0/RXD1/CRS_DV after the PHY resistors, TXD0/TXD1/TX_EN after the MAC resistors | ≤ 80 mm each, no spread limit | The ESP32-P4 publishes no RMII output delay (IP101G p.61 and RMII 1.2 §7.4 give the rest), so the board takes a budget of 1.0 ns (5 % of the 20 ns period, a heuristic) for the MAC clock branch plus one TX line: (60 + 80) mm × 7.1 ps/mm = 0.99 ns. IP101G §6.1 "equal" is met within this tolerance. Derivation: [ethernet](ethernet.md#timing) |
| REF_CLK star | `paths` "REF_CLK branches" from R13.2 to U1.10 and U6.68 | MAC branch ≤ 60 mm longer than the PHY branch | No ledge at U1.10 (near end) while the round trip to U6.68 is no longer than the fastest RMII edge (1 ns, RMII 1.2 §7.4.3): ΔL ≤ 1 ns / (2 × 7.1 ps/mm) = 70 mm, minus 10 mm reserved for the unpublished trace inside the module = 60 mm. RX/TX timing alone allows ≈ ± 420 mm ([ethernet](ethernet.md#ref_clk-branches)) |

Routed (via heights counted): RXD0 18.47, RXD1 17.92, CRS_DV 18.76, TXD0 21.52, TXD1 23.77, TX_EN 40.88 mm. REF_CLK:
R13.2 → U1.10 9.97 mm (2 vias), R13.2 → U6.68 46.42 mm (3 vias), difference 36.45 mm.

- The RX resistors R15/R11/R12 stand vertical in a row east of U1 with their vias south of them; U1's RX pin order is the
  reverse of U6's, so the B.Cu runs pass north of the vias to their west and arrive in U6 order without a crossing.
- REF_CLK runs ≥ 0.6 mm (3W) from the RMII data lines wherever their tracks run side by side (IP101G §6.1 p.56: "Avoid
  signals path parallel to clock signals path"): 0.613 mm to CRS_DV on B.Cu, 0.65 mm to TXD0 on F.Cu; the +3V3 trunk runs
  beside it at 0.35 mm for 8.1 mm. Closer spots that are not parallel runs: the REF_CLK via (157.45, 138.45) 0.49 mm from
  the CRS_DV B.Cu track and 0.50 mm from a TXD0 via; R13.2 0.41 mm from R12.2 (RXD1) in the resistor row; +3V3 copper
  0.20 mm at a via; U1.10 0.2 mm from U1.9 (TXD0) at the 0.4 mm pin pitch. REF_CLK_SRC and U1.10's pin stub run 0.15 mm
  where 0.2 mm does not fit.
- GND return vias 0.45/0.2 sit within 0.67–1.1 mm (centre to centre) of the RMII, REF_CLK, XVS and STROBE layer changes
  (TI SNLA387 p.4: return vias close to signal vias). Ten layer changes have none within 1.1 mm and no legal spot for one
  within 1.6 mm (grid search against every track, via, pad, rule area and the edge). Eight have no GND via within 1.6 mm at
  all: REF_CLK at (157.45, 133.75) and (157.45, 138.45), TXD0 (156.5, 133.45), TXD1 (155.95, 132.9), TX_EN
  (155.4, 131.75) in U1's south-west fan-out, TX_EN_MCU (138, 162.6), STROBE (117.60, 130.30) and XVS (123.98, 151.27)
  (nearest 1.77–4.76 mm). The other two have one a little further out: XVS (131.65, 104.8) at 1.27 mm and STROBE
  (115.95, 131.95) at 1.42 mm. These are 50 MHz RMII and slow strobe/sync lines without an impedance class; accepted.
  `via_return` does not check them (Default class).
- U1's exposed pad has 4 GND vias (0.45/0.2) on a 1.0 mm grid; U1.1 (GND corner pin) has its own via.
- Crystal Y1: no GND guard between it and the LED driver copper (IP101G §6.1: "Use guard traces to protect clock traces if
  possible"); there is no room for one (a 0.2 mm guard needs ≥ 0.6 mm, the gap is 0.4 mm). It relies on distance and the
  In1/In2 planes: C2.1 (XTAL_X1) to the filtered +32V track 0.45 mm, Y1.1 to +32V 1.51 mm, the switched LED_K tab 1.42 mm
  from C2.1, the B.Cu LED_K zone behind In1 and In2.
- JLCPCB recommends 1.0 mm from a QFN to any part ("Minimum Spacing Requirements for SMD Components",
  https://jlcpcb.com/help/article/minimum-spacing-for-smd-components, read 2026-10-31). At U1 every part keeps it:
  closest Y1.3–U1.1 1.000 mm, C9.1–U1.13 1.020, R7.1–U1.9 1.022, C7.1–U1.32 1.028, R1.2–U1.10 and R13.1–U1.16 1.030,
  R12.1–U1.16 1.067 mm.
- The Ethernet +3V3 loads (U1, pull-ups, LED anodes, ≈ 0.1 A, taken as 0.15 A) take the 0.65 mm class width to R38.1 and
  0.4 mm after it (ΔT ≈ 0.15 °C); 0.2–0.3 mm only where nothing wider fits.
- ETH_LED_G runs west of the crystal (F.Cu, 3 vias), ETH_LED_Y east (B.Cu, 1 via); ETH_SHIELD and ETH_SHIELD_R are
  0.5 mm (chassis/ESD path).

### PHY-side decoupling

| Pair | Max | Reason |
|---|---|---|
| C9.1–U1.13 | 3 mm | IP101G §6.1 p.56: VDDIO decoupling |
| C6.1–U1.28 | 3 mm | IP101G §6.1 and pin 28 p.18: REGOUT 0.1 µF |
| C4–C6 | 3 mm | IP101G pin 28 p.18: REGOUT 10 µF beside the 0.1 µF |
| C7.1–U1.31 | 3 mm | IP101G §6.1 p.56: AVDD33 decoupling |

### PoE isolation

The four PoE cable nets (POE_VA1, POE_VA2, POE_VB1, POE_VB2: from J1's centre taps to U2's input pins 1–4) carry the
cable side of the 802.3 isolation barrier. Requirement: IEEE 802.3 clause 33.4.1, 1500 V rms for 60 s or 2250 V dc
between the cable conductors and everything else.

Layer plan:

- The PoE tracks run on In1 only, 0.4 mm wide with 0.2 mm gaps (4 × 0.4 + 3 × 0.2 = 2.2 mm), in one lane. In1 is GND
  elsewhere; In1 tracks are allowed only inside the lane (rule area "In1 GND plane, PoE lane excepted"); In2 is solid
  GND with no tracks (rule area "In2 GND plane").
- Lane on In1 (≥ 2.3 mm wide everywhere): U2 end x 108.0–111.75, y 110.6–120.5; west band y 115.7–118.0; a 45° step to
  the middle band y 114.2–116.5 (x 123.8–133.4) under the ring top; a 45° staircase (steps 0.6 and 2.0 mm with a 0.8 mm
  level) to the east band y 116.8–119.1; 45° walls into the J1 end x 143.0–147.6, y 119.05–123.4. Every bend is 45°.
- In1 GND keeps 2 mm from the PoE copper (rule "PoE isolation", DRC clean). PoE copper to In1 via annular rings ≥ 2.025 mm
  (signal vias) and ≥ 2.036 mm (GND vias); J1.5 (chip side) 2.186 mm.
- Every other net crosses the lane on B.Cu over In2, with vias ≥ 2 mm from the lane copper. +32V, +5V and +3V3 never run on
  In1 or In2.
- The "PoE U2 pins" rule area (x 106.3–112.1, y 108.76–122.18, all copper layers) keeps vias and pours away from U2's
  cable-side pins.

Across layers (F.Cu over In1), which KiCad does not check: no F.Cu copper of another net lies over the In1 PoE copper or
within 1.0 mm of it sideways. Closest (to the routed PoE copper): TP10.1 (CAM_3V3) exactly 1.000 mm from POE_VA1 (TP10
cannot move: R53, J4, C25/U5 and C32 box it in), U5.1–3 1.05 mm, D6.1 and D7.2 1.09 mm, R52 1.10 mm, C3.2 1.36 mm; F.Cu tracks
beside the lane: C3's GND track 1.415 mm, ETH_CT 1.881 mm. Through the one 7628 ply (0.2104 mm) between F.Cu and In1,
IPC-4101's 30 kV/mm minimum (as quoted in the Isola 370HR datasheet) gives 6.3 kV, 2.8 × the 2250 V dc test; JLCPCB
publishes no through-thickness strength and a single ply carries pinhole risk (IPC-TM-650 2.5.6.2 tests two plies). A
2 mm sideways keep-off would be safer but does not fit; the hipot test below decides.

J1: the rule exempts pad pairs inside J1, which rest on HanRun's rating (HR871150C p.1: 2250 V dc, cable side to chip
side). J1's pads are trimmed to Ø1.45 mm on Ø0.89 mm holes (signal) and Ø2.35 mm on Ø1.63 mm (shield), so pin 5 (chip
side) to pin 7 (cable side) is 1.09 mm and the shield to pin 10 1.97 mm.

U2: the DP1435's barrier is rated 1.5 kV rms (DS p.1, p.5 item 9), ≈ 2121 V dc, below the 2250 V dc test. Its cable-side
pins U2.1–U2.4 have no capacitor to board GND: the datasheet's typical application (p.4) wires them straight from the
jack, and a capacitor to GND would bridge the barrier. That is why the decoupling check exempts U2.1–U2.4.

The top silk marks "POE 57V" beside J1 and U2.

First-board checks:

1. Bare board (U2 not yet soldered; it is hand-assembled): hipot 2250 V dc for 60 s, the four PoE nets tied together
   against everything else.
2. Assembled board: 1500 V rms for 60 s, or ≤ 2121 V dc (the DP1435's rating).

## IMU

Parts: U12 MEMSensing MSA3S02 (LGA-12, 2 × 2 mm, 0.5 mm pitch) at (137.2, 158.4) on the top side above U6, C50 100 nF,
R73/R74 I²C pull-ups. Circuit: [imu](imu.md).

- Land pattern: the eight corner lands are 0.025 mm shorter at their inner end (0.375 mm) so every gap between pads of
  different nets inside U12 is 0.15 mm (JLCPCB SMD pad-to-pad minimum); rule "U12 LGA pads".
- IMU_INT1 leaves pin 5 north to a tented via under U12's body centre (137.2, 158.4) on a 0.15 mm track (≥ 0.32 mm from
  every pad) and runs on B.Cu to U6.69. No other vias lie under the LGA pads; the datasheet gives no via or trace keep-out
  under the package. GND pin 9 and NC pin 8 (to GND, as the datasheet recommends) run to a GND via at (138.1, 156.2); NC
  pin 4 joins C50.2 on F.Cu.
- `near` C50.1–U12.7 ≤ 3 mm (MSA3S02 DS V1.0 p.30 Fig. 8: 100 nF at VDD): placed 1.15 mm. C50.2 has no GND via within
  1 mm (U6's bottom pads below and the +32V F.Cu round it leave no site); it reaches the GND via (134.69, 159.18) 2.1 mm
  away and U12.4. Accepted ([Accepted deviations](#accepted-deviations)).
- I²C away from the buck switch node (TI SLVSDG1C §10.1 p.13: "Keep these traces away from SW nodes"): IMU_SCL and
  IMU_SDA run on B.Cu under L2 (with In1 and In2 GND between them and L2's switch pad) and come up to F.Cu only east of it,
  at vias (137.82, 152.95) and (137.28, 153.6). F.Cu distance to SW_3V3 copper: SCL 1.43 mm, SDA 1.015 mm. SDA crosses the
  corner of L2's courtyard for ≈ 0.7 mm, 0.69 mm from the body corner (it must pass U12.1 to reach U12.2).
- JLCPCB's 1.0 mm from a QFN/LGA to other parts holds for C50 (1.02 mm, C50.1–U12.5) and R37 (1.33 mm); R73 is 3.2 mm away. It does not hold for R74: R74.2–U12.2
  0.64 mm. R74.1 (+3V3) must keep 0.4 mm from the +32V F.Cu at x 133.35 ("LED_PWR pad spacing (other pads)"), and a
  vertical R74 hits the C50 GND via or L2's courtyard. Accepted.

## LED driver and ring

Parts: 12 Amicc A-SC880IR3C IR LEDs D4–D15 in a ring of r 11.5 mm round the lens axis (30° pitch); U10 TI TPS61170 boost
at (116, 150.5) with L3, D3, C43, C45; C46/C47 330 µF bulk on +32V at (157.51, 143.2) and (145.8, 158.5); Q1 IRLR024N sink
MOSFET at (147.1, 133); U11 TLV9061 current loop with R69 (0.1 Ω sense) and net tie NT1; U9 SN74LVC1G97 STROBE gate. Circuit:
[led](led.md).

### Placement constraints

| Pair | Max | Reason |
|---|---|---|
| C36.1–U10.6, C36.2–U10.3 | 3 mm | TPS61170 SLVS789D §11.1: the input capacitor "needs not only to be close to the VIN pin, but also to the GND pin" |
| C43.1–D3.1, C45.1–D3.1 | 3 mm | §11.1: switch–diode–output capacitor path as short as possible |
| L3.2–U10.4, D3.2–U10.4 | 2.0 mm | §11.1 p.23: "minimize the length and area of all traces connected to the SW pin"; placed 1.65 / 1.66 mm |
| R65.2–U10.1, R66.1–U10.1 | 3 mm | Fig. 23: minimise the FB node |
| R62.1–U10.2, C41–U10.2, C39.1–U10.2 | 3 mm | Fig. 23: minimise the COMP node |
| C42–U11.4, R64.1–U11.4 | 3 mm | TLV9061 §8.4.1: feedback parts at the inverting input |
| C37.1–U9.5 | 3 mm | SN74LVC1G97 SCES416N §10: bypass capacitor at VCC |

### Boost stage

- U10's exposed pad has 2 GND vias 1.1 mm apart (`copperArea` "U10 exposed pad vias", min 2 inside the pad; TPS61170
  DRV0006A land pattern p.35, §11.3).
- C43.2's GND via ties to the exposed pad on F.Cu (0.6 mm) and C36.2 to its top (0.5 mm), so both capacitor loops close
  on the top layer as in TI Fig. 23. C45.2 keeps its own via: D3.2 (BST_SW) lies between it and C43.2.
- Pin 4 (SW) leaves at 0.3 mm, the pad width, for 0.85 mm (keeping TI's land gaps, 0.35 mm to pin 5 and 0.25 mm to the
  exposed pad), then 0.5 mm to L3.2. It keeps the switch node small (§11.1).
- +5V_BST necks to 0.5 mm at U10.6; BST_SW leaves U10.4 at 0.3 mm (above).

### +32V and the ring

- +32V goes from the boost output (D3's cathode, C45.1) to C47.1, on to C46.1, then north through a corridor east of Q1 to D4.2, 0.6 mm
  F.Cu/B.Cu with 0.6/0.3 vias (`kicad_query lengths` paths, via heights counted): C45.1 → D4.2 97.2 mm (91.0 mm of track,
  4 vias; D3.1 → D4.2 100.5 mm), of which C45.1 → C47.1 35.6 mm, C47.1 → C46.1 37.0 mm and C46.1 → D4.2 24.6 mm (F.Cu, no
  via). The whole +32V net is 128.6 mm (122.3 mm of track plus via heights), which adds the 0.3 mm R65/R71/TP13 feeds (20.0 mm) and short stubs. The pulse current comes from
  C46: 12.9 mm at 0.45 mm (1.09 mΩ/mm) plus 11.7 mm at 0.6 mm (0.82 mΩ/mm) ≈ 24 mΩ, ≈ 22 mV at 0.915 A (35 µm copper,
  ρ 1.72 × 10⁻⁸ Ω·m). ΔT ≈ 3.5 °C at 0.915 A DC for 0.6 mm. East of Q1 it runs 0.45 mm wide
  for 12.9 mm at x 150.625 (ΔT ≈ 4.8 °C at 0.915 A DC; the pulses run at ≈ 6 % duty), held 0.4 mm from Q1's tab and from
  C2/C8 by the pad-spacing rules. The C45.1 → L2-area via run is 0.4 mm for 6.8 mm (recharge current ≤ 0.1 A average).
- The ring links and the LED_K neck are 0.5 mm (1.0 mm breaks the 0.4 mm pad spacing to each LED's other pad): ΔT
  ≈ 4.2 °C at 0.915 A DC. Each LED's two pads of one node are joined inside the pads.
- Exposed pads of LED_PWR/BST_SW keep 0.4 mm to other nets, and other exposed pads keep 0.4 mm to LED_PWR/BST_SW copper
  (the two "LED_PWR pad spacing" rules). TH1 (the NTC) at (145.2, 127.8) is 0.42 mm from Q1's tab, which only tightens its
  thermal coupling to Q1.
- PHY_RST_N (75.4 mm, 5 vias) and NTC_SENSE (≈ 55 mm, B.Cu) route round the lens inside the LED ring, PHY_RST_N ≈ 41 mm of
  it at 0.5–1.1 mm from the ring pads and links. Both are slow lines: PHY_RST_N is static with R8 pulling it down,
  NTC_SENSE has C48 at the ADC pin.

### Current sense

- ISNS: Q1.3 → R69.1 0.55 mm (both pads ≥ 0.55 mm wide); NT1 takes the Kelvin tap from R69.1; TP14 by 0.25 mm.
- R69.2 (the 0.915 A return) has three GND paths: its own via (148.54, 142.1) plus 0.4 mm F.Cu links to GND vias
  (147.65, 142.2) and (147.5, 138.85); no via fits at the pad (U6's bottom pads 34–38 lie under R69). Return resistance
  ≈ 0.8 mΩ (three paths of ≈ 1.0, 1.4 and 1.9 mΩ give ≈ 0.45 mΩ in parallel, plus ≈ 0.3 mΩ of spreading in the pad and
  copper, an estimate; 35 µm copper, 0.3 mΩ per via), 0.8 % of R69,
  which only lowers the LED current.
- C46.2 has four GND vias: three 0.6/0.3 (hole 0.15 mm outside the pad) and one 0.45/0.2 at (161.45, 141.6) (hole 0.25 mm
  outside the pad).

### Thermal

Q1 dissipates up to 0.73 W average at the 0.5 ms / 120 fps limit ([led](led.md#thermal)). IRLR024N datasheet p.1: RθJA
110 °C/W on the minimum pad, 50 °C/W on 1 in² (645 mm²), RθJC 3.3 °C/W, TJ max 175 °C.

- Drain copper (`copperArea` "Q1 drain copper", net LED_K, F.Cu + B.Cu within 15 mm of Q1: ≥ 100 mm², ≥ 12 vias): Q1's
  6.4 × 5.8 mm tab with 16 thermal vias 0.6/0.3 (4 × 4), a little F.Cu beside it, and the B.Cu zone "Q1 drain B.Cu"
  (x 144.8–151.0, y 126.7–137.65, solid pad connection, 61.96 mm² filled). That is the ≈ 100–120 mm² that fits; 1 in²
  does not: `kicad_query board_free_space` finds no free 7.5 × 11.5 mm area on the top side for Q1 to move to.
- The zone stays clear of the crystal (Y1/C5), the RMII B.Cu strip (x 152.75–157.3), the CSI bus (1.04 mm from CSI_D1_P,
  ≥ 0.54 mm) and U6's courtyard, and never reaches the ring side.
- Estimate at 30 °C ambient: TJ ≤ 30 + 0.73 × 110 = 110 °C on any copper at least the minimum pad (the datasheet gives
  nothing between that and 1 in²); case ≤ 108 °C. Both are inside TJ 175 °C.
- Q1's tab is 1.74 mm from D15 and 1.79 mm from D4, C46 7.27 mm, C47 22.3 mm. The LED current is set for only 1.35 °C of
  LED heating above ambient, which a tab at up to ≈ 108 °C nearby almost certainly exceeds; C46/C47 are rated to 105 °C.
  Accepted pending measurement (user decision 2026-11-01: "Measure first boards").

First-board check: thermocouples on D15 and D4 cases and on C46 at 0.5 ms / 120 fps for 30 min; if the LEDs run above
the derating curve, lower the LED current ([led](led.md#first-board-checks)).

## Assembly

- JLCPCB Economic PCBA (https://jlcpcb.com/capabilities/pcb-assembly-capabilities, read 2026-10-31): single-sided
  placement, standard stack-up, parts ≥ 0402, IC pitch ≥ 0.4 mm, BGA ≥ 0.5 mm, fixed 255 ± 5 °C reflow peak, no
  castellated holes, edge plating or gold fingers, panels with mouse bites only (no V-cut), edge rails and fiducials not
  needed. Every machine-placed part is on the top side.
- Hand-assembly list (left out of JLCPCB's BOM and placement file; `assembly.hand`): U6, J1, U2 on the bottom side and
  TP2, TP3, TP7 (through-hole GND wire loops) on the top (user decisions 2026-10-31: "U6, J1, U2, TP2, TP3, TP7" and "Hand
  parts on the bottom"). 188 footprints: 185 top, 3 bottom.
- Order: JLCPCB reflows the top side; then the hand parts are soldered:
  - U6: iron only, pad by pad, no hot air (user decision 2026-11-01), with the hand-solder footprint variant that has no
    paste apertures ([mcu](mcu.md#module)); top-side parts above its pads must not reflow again.
  - J1: through-hole from the bottom, soldered from the top. Its lands are 0.565–0.717 mm from reflowed SMD pads
    (J1.1–R46.2 0.565, J1.5–C3.1 0.626, J1.14–R8.1 0.644, J1.SH–C1.1 0.699, J1.3–R46.1 0.717 mm); JLCPCB gives no
    through-hole-to-SMD figure. Use a conical tip ≤ 1 mm and Kapton over R46, C3, R8 and C1, then inspect those four for
    bridges and shifted parts. If that fails, open the gaps to ≥ 1.0 mm in the next revision.
  - U2: only after the bare-board hipot ([Ethernet and PoE isolation](#ethernet-and-poe-isolation)).
- Tooling holes TOOL1 (101.6, 108), TOOL2 (162.6, 152), TOOL3 (106.4, 155.8): 1.152 mm round NPTH with 0.148 mm mask
  expansion (1.448 mm opening), far apart in three corners, each with a copper/footprint keep-out ring (JLCPCB "How to add
  tooling holes for PCB assembly order", https://jlcpcb.com/help/article/How-to-add-tooling-holes-for-PCB-assembly-order,
  read 2026-10-31: "For Economic PCBA type, tooling holes are a must"; 2–3 holes in opposite corners; 1.152 mm round
  non-plated, 0.148 mm mask expansion).
- J2 sits with its shell front 0.3 mm inside the top edge ([MCU and USB](#mcu-and-usb)); JLCPCB PCBA FAQ part 2 asks for
  components ≥ 0.3 mm from depanel-routed edges.
- Part spacing: JLCPCB's 1.0 mm from a QFN to any part holds at U1 and U10. Exceptions: C15–U4 0.545 mm and C23.1–U4.1
  0.592 mm (TPS2121 bypass "as close as possible"; C23 cannot move west, C20's courtyard abuts it), R74.2–U12.2 0.64 mm
  ([IMU](#imu)).
- Polarity and labels: JLCPCB reads polarity from the silk layer (PCBA FAQ part 2, #10). Marks are held ≥ 0.15 mm from
  neighbouring pads, measured on the footprint geometry: U8's pin-1 triangle 0.165 mm from C34.1, C46's "+" 0.16 mm from
  R69.1. J4 carries "1" and "CONTACTS DOWN". Board texts 1.0 mm high, 0.15 mm stroke, name each probe point's signal: GND
  (TP2, TP3; TP7 on the bottom silk), +5V, 3V3, 32V, CAM_3V3, LED_EN, LED_ON, ISNS, PPS, XVS, STB, PRST (TP1, the PHY reset;
  "RST" is the module reset button), plus "BOOT"/"RST" between the buttons, "USB", "TRIG" and "POE 57V". Each keeps ≥ 0.15 mm
  from pads and mask openings (TP3's "GND" ≈ 0.21 mm from TOOL2's mask opening; "PRST" ≈ 0.16 mm from J1.14 and J1's NPTH
  opening).

First-board checks: J1 iron trial and inspection of R46, C3, R8, C1; U6 iron soldering; USB-C cables in use (charger,
flashing cable) latch and enumerate ([mcu](mcu.md#first-board-checks)); hipot before U2.

## Accepted deviations

Every warning class in the released check pass (`kicad_check`: erc, style, footprints, fields, pulls, rules, drc, layout
clean; unrouted 0), with its reason. A new warning not in these lists is a regression.

### track_widths (65 warnings)

| Nets and widths | Where | Reason |
|---|---|---|
| +3V3 0.4 mm (F.Cu 100 mm, B.Cu 70 mm) | Ethernet branch after R38.1, north/west feed loops | ≤ 0.15 A ([Power](#power), [Ethernet and PoE isolation](#ethernet-and-poe-isolation)) |
| +3V3 0.3 mm (F.Cu 17.5 mm, B.Cu 36 mm) | Short branches to pull-ups and U9's supply; R20's B.Cu feed to the north-west | mA loads; ≈ 19 mV drop at 150 mA for the whole feed |
| +3V3 0.2 mm (F.Cu 21.7 mm, B.Cu 8.0 mm) | The loop past H1 (r 3.8 mm washer clearance, 0.4 mm does not pass J2's shell), U12.7's feed, the link under Y1 | ≤ 0.15 A, ≈ 21 mΩ over 8.5 mm |
| +3V3 0.25 mm (0.63 mm) | R73.1 to its via | Pull-up, ≤ 0.7 mA |
| +3V3 0.5 mm (1.2 mm) | North of L2 at (131.5–132.7, 149.55) | Short; ΔT ≈ 2.2 °C even at the full 0.66 A |
| +3V3 0.15 mm (2.4 mm) | U1's +3V3 pin stubs (0.4 mm pitch) | Pin pitch |
| CAM_3V3 0.5 mm (3.3 mm) | R53.2 → C32/C30/J4.15/TP10 | Camera ≤ 100 mA |
| +5V 0.6 mm (F.Cu 5.3 mm, B.Cu 0.8 mm) | Trunk vias → U8.1 neck and U8.1 → C34.1; trunk via (139.3, 150.8) → C10.1 → U3.4 (U3 input, ≤ 0.52 A); link between the +5V vias (128.9, 147.9)/(129.7, 147.9) | Necks at a SOT-23-6 and at U3's input capacitors ([Power](#power)) |
| +5V 0.8 mm (B.Cu 10.2 mm) | Via (140.3, 150.8) → R63/C44/U11 at (149.45, 145.1) | < 1 mA (U11 supply and the R63 divider) |
| +5V 0.5 mm (F.Cu 4.0 mm) | U4's OUT flare to C20/C23; C10.1 → C12.1 → via (140.3, 150.8) (joins the U11/R63 branch) | Short necks at fine-pitch pads; C12's ripple current and < 1 mA |
| +5V 0.4 mm (F.Cu 9.8 mm, B.Cu 1.9 mm) | U3 EN feed from via (129.7, 147.9) to U3.1; U11/R63 supply stub | Enable pin and op-amp supply, mA |
| +5V 0.3 mm (F.Cu 5.1 mm) | U4 pin necks; the 0.7 mm stub from via (122.35, 146.2) into via (122.45, 146.9) inside U8's courtyard | Necks inside the U4/U8 courtyards |
| +5V_POE_F 0.8 mm (B.Cu 12.3 mm) | Filter vias (106.6, 138.45)/(106.6, 139.45) → U4's IN1 vias | 1.0 mm does not pass the VBUS IN2 via; 11.4 °C at 1.96 A |
| +5V_POE_F 0.3 / 0.4 mm (F.Cu 1.5 / 1.8 mm) | U4 IN1 pad entry and neck to its vias | Neck inside U4's courtyard |
| +5V_POE_F 0.25 / 0.4 mm (F.Cu 0.9, B.Cu 2.7 mm) | Feed to R18 (PR1/OV divider) | µA |
| +5V_BST 0.5 mm (1.9 mm), 0.8 mm (2.2 mm) | At U10's VIN; toward C36 between C36 and L3 | Neck at the WSON; 0.8 mm where 1.0 mm does not fit |
| VBUS 0.25 mm (4.5 mm) | J2.A4/A9 stubs to their vias | Only exit between J2's pegs and pads; ≈ 11 °C at 1.0 A each, 1.8 mm long |
| VBUS 0.5 / 0.4 mm (F.Cu 1.7 / 2.9 mm) | To D2; U4's IN2 pad entry | Short necks |
| VBUS 0.3 mm (F.Cu 6.0 mm, B.Cu 7.7 mm) | Feeds to R24 and R27 (VBUS_SENSE divider) | µA |
| VBUS 0.8 mm (B.Cu 5.0 mm) | Link between J2's VBUS vias | 1.0 mm does not fit |
| +32V 0.6 mm (F.Cu 62.5 mm, B.Cu 11.7 mm) | Boost output → C47 → C46 → ring | 0.915 A pulses at ≈ 6 % duty, ΔT ≈ 3.5 °C ([LED driver and ring](#led-driver-and-ring)) |
| +32V 0.45 mm (12.9 mm) | Corridor east of Q1 | Pad-spacing rules; ΔT ≈ 4.8 °C at 0.915 A DC |
| +32V 0.4 mm (6.8 mm) | C45.1 → L2-area via | Recharge ≤ 0.1 A average |
| +32V 0.5 mm (0.9 mm) | Short stub beside the D4.2 entry at x 141.51 | Short; pad spacing at the LED |
| +32V 0.3 mm (20.0 mm) | Feeds to R65 (boost FB divider), R71 (VLED sense divider) and TP13 | µA |
| RING_1–RING_11 0.5 mm, LED_K 0.5 mm (1.2 mm) | Ring links and the LED_K neck | 1.0 mm breaks the 0.4 mm LED pad spacing; ΔT ≈ 4.2 °C at 0.915 A DC |
| ISNS 0.55 mm (1.6 mm), 0.25 mm (6.1 mm) | Q1.3 → R69.1; the TP14 branch | Pad width; test point carries no current |
| USB_C_D_P/N 0.2 mm (F.Cu 13.2 mm, B.Cu 120 mm); USB_C_D pair coupled at 0.20–0.30 mm | Connector side J2 → U5 → R31/R32 | Full speed is electrically short; impedance not controlled ([MCU and USB](#mcu-and-usb)) |
| USB_D pair coupled at 0.257 mm (1.57 mm) | Into R31/R32 | Resistor pad pitch |
| CAM_D1 pair coupled at 0.415 mm (0.45 mm); CAM_CLK pair at 0.335 mm (0.51 mm) | CAM_D1's 0.45 mm bump flat; CAM_CLK_P's F.Cu tail into its via | Skew-compensation geometry, not coupled runs |
| ETH_TD pair at 0.22 mm (0.65 mm); ETH_RD pair at 0.22 / 0.428 mm (1.29 mm) | U1's MDI pin escapes | 0.4 mm pin pitch |
| RMII_REF_CLK, RMII_REF_CLK_SRC 0.15 mm (0.7 / 1.5 mm) | U1.10's pin stub and where 0.2 mm does not fit | Pin pitch |
| IMU_INT1 0.15 mm (F.Cu 0.9 mm, B.Cu 3.1 mm) | From U12.5 to the via under U12 | Between 0.5 mm-pitch LGA pads |

### return_path (12 warnings)

| Track | Where | Reason |
|---|---|---|
| USB_C_D_P, 2.25 mm and 0.54 mm on F.Cu | U5 link at (113.05, 111.8–114.0) | Crosses the In1 gap of the PoE lane (2 mm from the PoE copper); In2 is solid GND below. Full-speed USB, electrically short |
| ETH_TD_N 1.04 mm and 1.21 mm; ETH_RD_N 0.54, 1.21 and 0.54 mm (x 160.77 / 159.26, y 120.5–122.8) | Beside the P/N swap vias | The swap vias' In1 antipads (r 0.775 mm) reach under the N track; the hop sits where IP101G §6.2 allows vias |
| ETH_TD_P 2 × 0.89 mm, ETH_RD_N 2 × 0.91 mm, ETH_TD_N 0.69 mm | TD_P passing over J1.2 into J1.1, RD_N passing under J1.4 into J1.5, TD_N leaving J1.2 | J1's MDI antipads in In1 (pad plus the 0.55 mm MDI spacing); a track leaving or passing a pin there crosses them |

### via_return (3 warnings)

| Net | Vias | Reason |
|---|---|---|
| ETH_RD_P | (159.728, 121.165), (158.788, 122.105) | A GND via must keep ≥ 1.0 mm from an MDI via ("MDI spacing" 0.55 + 2 × 0.225 mm), and J1.13/J1.14 take the only spots at exactly 1.0 mm. MDI is short at 100 Mbit/s |
| ETH_TD_P | (160.298, 122.055), (161.238, 121.115) | Same |
| USB_C_D_N | (114.1, 109.0) | No site within 1 mm (D2, the VBUS link and the D+ track surround it); nearest GND via 2.66 mm, In2 solid below; full-speed edges ≥ 4 ns |

### decoupling_placement (1 warning)

| Capacitor | Reason |
|---|---|
| C50 (U12 VDD): GND pad C50.2 has no GND via or fill within 1 mm | No via site (U6's bottom pads below, the +32V F.Cu round it); it reaches the GND via (134.69, 159.18) 2.1 mm away and U12.4 ([IMU](#imu)) |

### Other accepted deviations

| Deviation | Accepted by | Section |
|---|---|---|
| No impedance control ordered; MIPI 88–114 Ω at the width extremes | User decision 2026-11-01 | [Impedance](#impedance) |
| Stock silk 0.085–0.131 mm from pads, JLCPCB clips it | User decision 2026-11-02 | [Fabrication rules](#fabrication-rules) |
| CSI In2 neck 6.19 mm vs 6.83 mm; 72.5 mil a side with the 45° crossing | User decision 2026-11-01 | [Camera and CSI-2](#camera-and-csi-2) |
| CAM pairs 54–89 mil from the plane edge at the FFC notch (90 mil asked) | User decision 2026-11-01 | [Camera and CSI-2](#camera-and-csi-2) |
| No GND guard round the MIPI CLK pairs | User decision 2026-11-01 | [Camera and CSI-2](#camera-and-csi-2) |
| Planes not voided under the link and J4 pads | Design review 2026-11-01 | [Camera and CSI-2](#camera-and-csi-2) |
| MDI 3W waived inside "U1 MDI escape" | Design review 2026-11-01 (0.4 mm pitch) | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| Ten RMII/strobe/sync layer changes without a GND via within 1.1 mm (eight with none within 1.6 mm) | Design review | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| No GND guard at the crystal | Design review (IP101G: "if possible") | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| F.Cu 1.0 mm (not 2 mm) from the In1 PoE copper; TP10.1 at exactly 1.000 mm | Hipot test on the first boards | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| Q1 drain copper ≈ 100–120 mm² instead of 1 in², Q1 1.7 mm from D15/D4 | User decision 2026-11-01: measure first boards | [LED driver and ring](#led-driver-and-ring) |
| C15/C23–U4 and R74–U12 closer than JLCPCB's 1.0 mm QFN spacing | Design review | [Assembly](#assembly) |
| J1 lands 0.565–0.717 mm from reflowed SMD pads | First-board soldering trial | [Assembly](#assembly) |
| J2 shell 0.77 mm further in than HRO's recommended overhang | User decision 2026-11-01 | [MCU and USB](#mcu-and-usb) |
| Buttons on the bottom edge | User decision 2026-11-01 | [MCU and USB](#mcu-and-usb) |
