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
of H1's centre (closest: the 0.4 mm +3V3 loop running SW between J2's two west shield pads, 3.901 mm). Measured separately from each hole centre to
nearby non-GND nets: H2 4.603 mm (+3V3 via (155.85, 104.8)); H3 4.165 mm (BOOT_SW via (107.6, 158.93)); H4 5.746 mm
(+32V track near (154.7, 157.75)). All keep the r 3.8 mm washer allowance; H3 does not keep r 4.6 mm.

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

- JLCPCB's stack-up page labels the finished thickness 1.59 mm; its capabilities page gives ± 10 % (1.44–1.76 mm for a
  1.6 mm board, read 2026-11-02). KiCad sums the layers to 1.617 mm.
- Why this stack-up: it is JLCPCB's default ("No requirement" on the order page), the only 4-layer 1.6 mm stack-up its
  calculator tags "Standard", it costs no stack-up fee, and Economic PCBA accepts "standard stack-up only"
  (https://jlcpcb.com/capabilities/pcb-assembly-capabilities, read 2026-10-31; JLCPCB does not publish which named
  stack-ups count as standard).
- The loss tangent 0.02 in KiCad's stack-up is a placeholder; JLCPCB publishes none. It does not affect geometry.
- Via heights count in every length on this board (`use_height_for_length_calcs`): one through via adds 1.5512 mm.

## Fabrication rules

Board rules in the board setup and the custom rules, with the JLCPCB figure behind each. Source unless stated: JLCPCB PCB
capabilities, https://jlcpcb.com/capabilities/pcb-capabilities, read 2026-11-02 (4-layer, 1 oz outer, 0.5 oz inner; the
page gives no separate figures for 0.5 oz inner copper).

| Rule | Board value | JLCPCB figure | Notes |
|---|---|---|---|
| Minimum track width / clearance | 0.1 / 0.1 mm | Multilayer 0.09 / 0.09 mm; pad to track 0.1 mm | The design's narrowest track is 0.15 mm and its closest copper 0.15 mm (`kicad_query stats`) |
| Copper to board edge | 0.3 mm | Routed edge 0.2 mm; PCBA FAQ part 2 (https://jlcpcb.com/help/article/pcb-assembly-faqs-part-2, read 2026-10-31): "edges of the traces and components must be more than 0.3mm away from the edge" (≈ 0.2 mm routing tolerance at depanelling) | Also the plane pull-back at the FFC notch and the lens cutout |
| Via size | Minimum Ø0.45 mm, hole 0.2 mm | "0.2mm or 0.25mm hole size with via diameter less than 0.45mm, will cost more"; preferred minimum via hole 0.2 mm | Used: 0.45/0.2 (301 vias), 0.6/0.3 (33: power and thermal), 0.8/0.4 (2: +5V_POE_F at the L1/C13/C14 filter output), 0.5/0.25 (1: +5V beside U8) |
| Via annular ring | 0.075 mm | "Via diameter should be 0.1mm(0.15mm preferred) larger than Via hole size" (0.05 / 0.075 mm ring) | 0.45/0.2 vias have 0.125 mm |
| Via types | Through vias only (rule "Through vias only") | Microvias "only available for board thickness ≤1mm"; blind and buried vias are HDI only | |
| Hole to copper | 0.2 mm (rule "PTH hole to copper": 0.3 mm for through-hole pads) | Via hole to track 0.2 mm; inner-layer PTH hole to copper 0.3 mm | |
| Hole to hole | 0.2 mm, DRC severity error (rule "Pad hole to hole": 0.45 mm between pads) | Via hole-to-hole 0.2 mm; pad hole-to-hole 0.45 mm | Closest via pair 0.230 mm. KiCad's hole-to-hole check skips milled (oval) holes, so J2's slots are not covered |
| PTH annular ring | 0.15 mm (rule "PTH annular ring"); the 0.075 mm board minimum covers vias | ≥ 0.15 mm absolute, 0.20 mm recommended | J2 shell slots exactly 0.15 mm (stock HRO land pattern, under the 0.20 mm recommended); J1 signal pins 0.28 mm, J1 shield tabs 0.36 mm |
| NPTH | Round ≥ 0.5 mm, slots ≥ 1.0 mm (rules "NPTH hole size", "NPTH slot width") | Non-plated holes ≥ 0.50 mm; non-plated slots ≥ 1.0 mm | Smallest: J2 pegs 0.6 mm; no non-plated slots |
| Plated slot | ≥ 0.5 mm wide (rule "Plated slot width") | "Multi-layer: 0.35mm" and "The minimum plated slot width is 0.5mm"; slot length at least 2 × its width | J2 shell slots 0.6 × 1.4 / 0.6 × 1.7 mm (2.3× / 2.8×). The board setting `min_groove_width` (0.35 mm) is KiCad's creepage groove width, not a slot check |
| SMD pad to pad, different nets | 0.15 mm by the net classes; rule "U12 LGA pads" | 0.15 mm | |
| Minimum SMD pad | 0.1 × 0.1 mm (rule "Minimum SMD pad"; user decision 2026-11-02: "Use 0.1mm×0.1mm for minimum pad size") | "Minimum SMD pad: 0.25mm × 0.25mm" (a note under SMD pad to pad clearance) | U1's QFN pads 0.2 × 0.8 mm (24) and U4's 0.2 × 0.6 mm (8), the manufacturers' land patterns for 0.4 mm pitch, are under JLCPCB's note and kept; the same page makes 0.2–0.25 mm BGA pads with ENIG. Smallest pads: U1's corner anchors 0.129 mm |
| Solder mask | Openings 1:1 with pads; mask-to-copper 0.09 mm; minimum web 0.1 mm (`solder_mask_min_width`) | "Pad size:soldermask opening can be 1:1"; keep 0.09 mm between openings and neighbouring traces; minimum pad spacing for a mask bridge 0.10 mm (green, 1 oz) | Vias tented on both sides. KiCad flags and merges openings closer than 0.1 mm; the smallest web is 0.15 mm (U12) |
| Silkscreen | Lines ≥ 0.15 mm (rule "Silk line width"), text ≥ 1.0 mm high with 0.15 mm stroke (DRC severity error) | Line width ≥ 0.15 mm, text height ≥ 1.0 mm, width:height 1:6 preferred, pad to silkscreen 0.15 mm | All silk lines and texts are 0.15 mm; 0.15 mm on 1.0 mm text is 1:6.7, slightly thinner than the preferred 1:6. The board's defaults for new silk items (Board Setup › Text & Graphics) are still 0.1 mm |
| Silk clearance | 0.15 mm to the board edge; board-level silk 0.15 mm from pads (rule "Board silk to pads"); 0 between two silk items (rule "Silk to silk") | Pad to silkscreen 0.15 mm; no silkscreen-to-silkscreen figure | See below |
| Tolerances | | Track width ± 20 %, board thickness ± 10 %, hole position ± 0.075 mm, plated holes +0.13 / −0.08 mm | Width tolerance drives [Impedance](#impedance) |

Vias next to pads: the quote page turns "tented" into "plugged" for free, but JLCPCB plugs a via with ink only if it
is ≥ 0.35 mm from any pad opening (https://jlcpcb.com/help/article/pcb-via-covering). Signal, power and decoupling vias
are never placed in pads; where a via must sit closer, its drilled hole stays ≥ 0.10 mm (the mask bridge) outside the pad
opening: C15.2's via 0.125 mm, C37.2's 0.107 mm, the C40.2/U11.2 via 0.100/0.130 mm, the U6.55 via beside C47.2 0.120 mm
(all GND, same net as the pad).

Thermal vias are the exception: they sit inside the pads they cool, open (unfilled, unplugged) on the pad side and tented
on the far side. U1's exposed pad has 4 × 0.45/0.2 (GND), U10's exposed pad 2 × 0.45/0.2 (GND; TPS61170 DRV0006A land
pattern p.35 draws them there) and Q1's tab 16 × 0.6/0.3 (LED_K). Solder paste over them can wick into the holes and leave
voids under the pad; that is accepted (user decision 2026-11-03: "Accept and document"): no windowpane paste, no filled
and capped vias. Inspect those three joints on the first boards ([Assembly](#assembly)).

Silk against pads: KiCad 10's `min_silk_clearance` board setting covers silk to silk and silk to the board edge, and checks
silk against pads only for overlap on the mask layer; a custom `silk_clearance` rule covers more. Rule "Board silk to
pads" holds board-level silk (texts, marks) ≥ 0.15 mm from pads. Footprint silk is exempt: the stock silk stays where it
is closer, 0402 resistor and capacitor outlines 0.085 mm from their own pads, test-point rings 0.125 mm, J4's outline
0.131 mm from its mounting pad; JLCPCB clips legend at pads (user decision 2026-11-02: "Keep stock silk, JLC clips it").
Polarity marks inside footprints are held ≥ 0.15 mm from pads by hand ([Assembly](#assembly)). Because JLCPCB gives no
silk-to-silk figure, the rule "Silk to silk" lets two silk items touch; without it, widening the stock silk to 0.15 mm
made 15 pairs of neighbouring 0402/SOT outlines 0.13–0.145 mm apart, which KiCad would flag.

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
| POE_CABLE | POE_VA1, POE_VA2, POE_VB1, POE_VB2 | 0.4 mm | 0.2 mm (2 mm to other nets by rule) | | Centre-tap current 350 mA (HanRun HR871150C p.1); routed on In1 only (0.5 oz): ΔT ≈ 11 °C at 350 mA, 0.108–0.119 Ω per net ([PoE isolation](#poe-isolation)) |
| MIPI_CSI | CSI_* and CAM_* pairs (12 nets) | 0.18 mm | 0.15 mm (0.36 mm to other nets by rule) | 0.18 / 0.15 | 100 Ω differential ([Impedance](#impedance)) |
| MDI | ETH_TD_P/N, ETH_RD_P/N | 0.18 mm | 0.15 mm (0.55 mm to other nets by rule) | 0.18 / 0.15 | 100 Ω differential |
| USB | USB_C_D_P/N (connector side), USB_D_P/N (module side) | 0.245 mm | 0.15 mm | 0.245 / 0.15 | 90 Ω differential (HDG §1.4.3) |

Tracks narrower than their class are allowed only where a reason is recorded; every such case is in
[Accepted deviations](#accepted-deviations).

## Custom rules

The `.kicad_dru` rules (`kicad_query rules`), in file order; the two MDI rules were added after "Silk to silk", and the
JLCPCB fabrication rules after them (2026-11-02; figures and sources in [Fabrication rules](#fabrication-rules)).

| Rule | Applies to | Constraint | Reason and source |
|---|---|---|---|
| MIPI_CSI spacing | MIPI_CSI copper against everything except its own partner; the two pads of each 0 Ω link R45–R47, R49–R51 exempt | Clearance ≥ 0.36 mm (2W) | HDG, PCB layout, MIPI: "maintain a minimum spacing of 2W between MIPI trace pairs". Keeps the GND pours ≥ 0.36 mm away too, which the 100 Ω geometry assumes. The link pads carry the two halves of one lane |
| MDI spacing | MDI copper against non-MDI copper | Clearance ≥ 0.55 mm (3W, 3 × 0.18 rounded up) | IP101G datasheet §6.2 p.56 "three times space". Exempt: MDI items touching U1's courtyard or lying wholly inside the F.Cu rule area "U1 MDI escape" (x 157.3–160.6, y 125.69–128.9), where U1's 0.4 mm pitch puts pins 25 (ISET), 28 (REGOUT) and 31 (AVDD33) 0.2 mm from the MDI pads ([Ethernet and PoE isolation](#ethernet-and-poe-isolation)). A segment crossing the area edge is not exempt |
| PoE isolation | POE_CABLE copper against other nets, same layer | Clearance ≥ 2 mm | Keeps the PoE cable side (J1's centre taps to U2.1–U2.4) apart from the rest of the board. The layout is meant to give some isolation only; no isolation level is rated or tested (user decision 2026-11-02). 2 mm is the chosen same-layer gap. Pad pairs inside J1 exempt: that pin field is the jack's own (HanRun HR871150C p.1 rates it 2250 V dc, cable side to chip side). Copper across layers is not checked by KiCad: see [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| MIPI_CSI intra-pair skew | Each MIPI_CSI pair | Skew ≤ 0.127 mm | HDG §1.4.6 allows 10 mil (0.254 mm) P/N skew per lane; a lane is two pairs in series (U6 → 0 Ω link → J4), so each gets half |
| U12 LGA pads | Pad pairs inside U12 | Clearance ≥ 0.15 mm | MSA3S02 land pattern has 0.15 mm between neighbouring pads of different nets (corner lands shortened to 0.375 mm at their inner end for it); JLCPCB "SMD pad to pad clearance (different nets) 0.15mm", mask bridge 0.10 mm |
| LED_PWR pad spacing | An exposed pad of LED_PWR or BST_SW against any other net (not POE_CABLE or MDI, which have stricter rules) | Clearance ≥ 0.4 mm | Up to 33.9 V (TPS61170 set point, worst case). IPC-2221A Table 6-1, 31–50 V row: A6 (external component terminations, uncoated) 0.4 mm; B4 (solder mask) 0.13 mm, so mask-covered tracks keep the 0.2 mm class clearance. Exempt: pad pairs inside U10 (TI land pattern) and inside each LED D4–D15 (one forward voltage apart) |
| MIPI_CSI to clocks and high-speed | MIPI_CSI against USB, MDI, every /Ethernet/RMII_* and /MCU/RMII_* net, XVS/XVS_CAM and STROBE/STROBE_CAM | Clearance ≥ 0.54 mm (3W) | Espressif ESP32-P4 HDG, MIPI PCB layout: 3W from other high-speed and fast signals; includes both sides of the sync/strobe series links and all PHY/MAC RMII nets. All groups included without exemptions; closest XVS clearance 0.5401 mm on F.Cu, 0.7504 mm on B.Cu |
| PTH hole to copper | Through-hole pads | Hole clearance ≥ 0.3 mm | JLCPCB inner-layer PTH hole to copper 0.3 mm (PTH to track 0.28 mm minimum) |
| Pad hole to hole | Pad against pad | Hole to hole ≥ 0.45 mm | JLCPCB pad hole-to-hole spacing 0.45 mm (vias: 0.2 mm, the board minimum) |
| Power neck-down at fine-pitch pads | PWR_5V, LED_PWR and +5V_BST tracks touching the courtyard of U4, U8 or U10 | Track width ≥ 0.3 mm | A 1.0 mm class track cannot enter U4's 0.4 mm pads (0.3 mm gaps, TI TPS2121 land pattern p.39), U8's SOT-23-6 (0.95 mm pitch) or U10's 0.3 mm WSON pads (TPS61170 DRV0006A, p.35) at 0.2 mm clearance; a 1 mm long 0.3 mm neck is 1.6 mΩ, 9 mW at 2.4 A ([Power](#power)) |
| LED_PWR pad spacing (other pads) | An exposed pad of any other net against LED_PWR or BST_SW copper (tracks and vias included) | Clearance ≥ 0.4 mm | The mirror of "LED_PWR pad spacing", which only fires when the exposed pad is the LED_PWR item; same IPC-2221A source. Exempt: U10's own pads against BST_SW copper inside U10's courtyard (pin 4's 0.3 mm exit keeps TI's DRV0006A land gaps, 0.35 mm to pin 5 and 0.25 mm to the exposed pad) and ISNS_K against ISNS (the Kelvin tap through net tie NT1, same node, ≤ 0.1 V) |
| Silk to silk | Two non-pad items on the same silk layer | Silk clearance ≥ 0 mm | JLCPCB gives no silkscreen-to-silkscreen spacing; see [Fabrication rules](#fabrication-rules) |
| MDI pair to pair | RD pair (ETH_RD_P/N) against TD pair (ETH_TD_P/N); same U1 exemption as "MDI spacing" (either item touching U1's courtyard or wholly inside "U1 MDI escape") | Clearance ≥ 0.54 mm (3W) | IP101G §6.2 p.56 "three times space" also between the RX and TX pairs: 3 × 0.18 mm = 0.54 mm. Closest spot: the RD_N/TD_P diagonals at (159.2–159.6, 125.2–125.6), west of R6, 0.5399 mm (`kicad_query distance`), which DRC passes |
| MDI intra-pair skew | Each MDI pair | Skew ≤ 1.27 mm | TI SNLA387 p.2 "MDI Traces": length-match the two lines of a pair within 50 mil (1.27 mm) for 100BASE-TX |
| PTH annular ring | Plated through-hole pads | Annular width ≥ 0.15 mm | JLCPCB absolute minimum for 1 oz multilayer (0.20 mm recommended) |
| Plated slot width | Plated pads with an oval hole | Hole ≥ 0.5 mm | JLCPCB "The minimum plated slot width is 0.5mm" (also "Multi-layer: 0.35mm") |
| NPTH hole size | Round non-plated holes | Hole ≥ 0.5 mm | JLCPCB "Min. Non-plated holes 0.50mm" |
| NPTH slot width | Non-plated oval holes | Hole ≥ 1.0 mm | JLCPCB "Min. Non-Plated Slots 1.0mm". A guard for future parts: the board has no non-plated slot, so nothing exercises it yet. Its condition joins the two tested ones of "NPTH hole size" (non-plated) and "Plated slot width" (oval hole) |
| Through vias only | Every via | Micro, blind and buried vias disallowed | JLCPCB: microvias only on boards ≤ 1 mm, blind/buried vias HDI only |
| Silk line width | Graphic items on F.SilkS / B.SilkS | Line width ≥ 0.15 mm (assertion) | JLCPCB "Minimum Line Width ≥0.15mm"; KiCad has no board setting for it |
| Board silk to pads | Board-level silk (not in a footprint) against pads | Silk clearance ≥ 0.15 mm | JLCPCB pad to silkscreen 0.15 mm; footprint silk exempt by user decision ([Fabrication rules](#fabrication-rules)) |
| Minimum SMD pad | SMD pads | Size X and Y ≥ 0.1 mm (assertion) | User decision 2026-11-02, below JLCPCB's 0.25 mm note ([Fabrication rules](#fabrication-rules)). Custom-shape pads are checked on their anchor, which is smaller than the whole pad |

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
- T-junctions and track ends in pads or vias are not bends.

Pad and via entries (user instruction 2026-11-02, with example images):

- every track ends at the centre of the pad or via it joins, not at its edge;
- a track enters a round pad or a via radially, and a rectangular pad along one of the pad's axes, at right angles to the
  edge it crosses; its last bend lies outside the pad or via, at least max(0.2 × track width, 0.04 mm) beyond its copper;
- no acute angle between two tracks, or between a track and a pad edge;
- where power pins of one net sit close together, a pour joins them instead of several tracks
  ([Ground and return](#ground-and-return)).

Exceptions only where nothing else is possible, each recorded with location and reason: the LED ring links
([LED driver and ring](#led-driver-and-ring)) and the entries that placement blocks ([Pad entries](#pad-entries)).

Audit at the original released design (a script over every track): 1542 tracks, 0 arcs, 0 off-angle segments,
0 non-45° bends, 0 short 45° legs. Pad-entry audit (2026-11-02, a script over every track end in a pad or via, on the
board file as `kicad_query copper` reads it), before → after: track ends off the pad or via centre 552 → 21, bends
inside a pad or via 132 → 6, entries off the pad axis or not radial 77 → 3, bends closer than the margin above 14 → 2,
acute junctions 16 → 0. The 32 remaining findings sit on 19 pads and vias, all in [Pad entries](#pad-entries). After:
1619 tracks, 340 vias, 0 arcs; outside the LED ring 0 off-angle segments and 0 non-45° bends; every 90° change keeps a
45° leg at least the track width. Reaching pad centres from lanes a few hundredths of a millimetre off left 56 short jogs
(two opposite 45° bends with a diagonal shorter than the track width; 13 of them older, shortest 0.007 mm on SW_3V3),
which the rules allow.

Hooks and detours (user instruction 2026-11-02, with example images of CC2 looping round J2's shield pad and XVS_CAM
hooking into TP8): a track runs from its pad or via toward where it goes, never leaving the other way and looping back,
and a via that only serves one stub sits on its pad's axis (or radial ray) so the stub is one straight segment. Reworked
2026-11-02: XVS_CAM into TP8 (one radial segment to via (149.7, 102.4), 1.02 mm from R48's GND via), LED_ON into TP12,
CC2 (R23.1 → J2.B5, 4.81 mm instead of ≈ 9.5 mm; [MCU and USB](#usb)), ETH_SHIELD_R (R2 turned, 1.675 mm instead of
13.3 mm with two vias; [Ethernet](#rmii)), the GND stubs of C11.2, C14.2, C22.2, R19.2 and J4's east MP pad (vias moved
onto the pad axis), and C37.2's loop inside "GND pour 81" (removed: the pour joins pad and via). A script over every
track chain with ≥ 135° of turning finds the loops left forced: U5.5 → C25.1 (round U5.4), R3.2's GND via (ETH_LED_Y
under R3 on B.Cu, the MDI pair to the west), C16.2's GND via (+5V, VBUS and +5V_POE_F round it), the +5V EN feed into
U3.1 (round C18.1), +32V from C46.1 (MDC), NTC_SENSE and VLED_SENSE (round C48.2 / R72.2), PHY_RST_N (round R8.2),
RMII_REF_CLK on B.Cu (+3V3), and the length-matched diff pairs. After the rework: 1589 tracks, 337 vias, 0 arcs; outside
the LED ring 0 off-angle segments and 0 non-45° bends.

Geometry cleanup (2026-11-03, after the +32V reroute in [+32V and the ring](#32v-and-the-ring)): every track end now lies
on a pad, a via, a pour of its net or exactly on another track's centreline (ten joints that overlapped instead, among
them USB_C_D_P's J2.A6/B6 tie, MDIO, ETH_LED_Y and four +3V3 branches, were rebuilt; VLED_SENSE's branch into the C49.1
run keeps its 45° T, now on the centreline); the stubs inside U1.1 and U1.32 are gone; collinear segments of one width
were merged, except ETH_TD_P at (159.66, 125.80), where the split keeps the lower segment wholly inside "U1 MDI escape"
as the "MDI spacing" exemption needs. After: 1556 tracks, 335 vias, 0 arcs; outside the LED ring 0 off-angle segments and
0 non-45° bends; lengths of the MIPI, MDI and RMII nets unchanged.

The MDI pin escapes at U1, which once needed slanted segments, are 45°-only ([Ethernet and PoE isolation](#ethernet-and-poe-isolation));
the CSI bus crosses its narrowest plane neck at 45° rather than the ideal 58° ([Camera and CSI-2](#camera-and-csi-2)).

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
| +5V and +5V_POE trunks, +5V_POE_F on F.Cu | 1.0 mm | Common +5V trunk 1.78 A while the boost is current-limited / 1.96 A board peak; 2.6 A DP1435 maximum | 8.8 °C at board peak / 15.5 °C at DP1435 maximum |
| +5V_POE_F B.Cu leg, 12.3 mm, from the filter vias (106.6, 138.45)/(106.6, 139.45) to U4's IN1 vias (1.0 mm does not pass the VBUS IN2 via at (113.4, 139.65)) | 0.8 mm | 1.96 A | 11.4 °C |
| +5V into U3: the 1.0 mm B.Cu trunk ends at via (139.3, 150.8); the F.Cu pour "C12/C10/U3 +5V" joins it, via (140.3, 150.8) (the U11/R63 branch), C10.1, C12.1 and U3.4 | Pour | ≤ 0.52 A (buck input peak) | |
| +5V B.Cu branch from via (140.3, 150.8) to R63/C44/U11 at (149.45, 145.1) | 0.8 mm | < 1 mA (U11 supply and the R63 divider) | |
| +5V common node at U8.1: the trunk vias (122.45, 146.9) and (122.35, 146.2), U8.1, C34.1 and TP5.1 in the F.Cu pour "+5V pour 100"; the B.Cu pour "+5V pour 105" brings the common trunk to the via pair | Pours | Boost and buck together: 1.78 A while the boost is current-limited / 1.96 A peak ([power](power.md#5v-loads)) | |
| From the U8.1 pour east to vias (128.9, 147.9)/(129.7, 147.9) (pours "+5V pour 101"/"+5V pour 104" round them) and the B.Cu trunk toward U3.4; U8.1 → U3.4 is 25.82 mm with two via heights, the pours counted as straight lines | 1.0 mm trunk (0.6 mm between the east via pair and for 0.6 mm leaving the U8.1 pour) | Buck branch: ≤ 0.52 A peak | ≈ 1.1 °C at 0.52 A for 0.6 mm × 35 µm as an infinite track |
| +3V3 trunks | 0.65 mm | 0.66 A | 1.6 °C |
| Necks inside the U4, U8 and U10 courtyards | ≥ 0.3 mm | | 1 mm of 0.3 mm is 1.6 mΩ, 9 mW at 2.4 A |
| Branches carrying no load current (enable pins, dividers, FB sense, op-amp supply, pull-up feeds, short stubs into pins) | 0.2–0.4 mm | µA–mA | |

The +3V3 feed to the north and west parts (U5, C25, R53 → CAM_3V3/J4, R41/R42, D1/C24, R20) cannot take a shorter
route: the USB pair, the camera bundle, XVS and +5V_POE_F fence it on B.Cu, the PoE lane and U2 on F.Cu. It is fed from
the U3/U6 area by a B.Cu feed (15 mm, 0.4 mm except 0.61 mm beside the GND via (121.9, 153.075), which stays 0.3 mm;
R20's pull-up branch off its west end, via (113.08, 143.4), stays 0.3 mm), a 0.4 mm loop along the west edge (40 mm) and a 0.4 mm F.Cu loop round
J2's north side (≈ 45 mm). Past H1 the loop drops east of J2's north-west shield pad and runs SW between the two west
shield pads, 3.901 mm from H1's centre, with CC1 parallel on its south-east side (2026-11-02; it necked to 0.2 mm for
8.5 mm between H1 and the shield pad before). The R41/R42 branch's B.Cu feed from via (122.1, 112.9) is 0.5 mm except
its last 1.5 mm beside CAM_IO1's via (116.65, 120.4). L2.2 → R53.1 ≈ 0.12 Ω (estimate: the 0.14 Ω network figure
solved from `kicad_query copper` less the series stretches widened on 2026-11-02), < 17 mV at R53.1 (camera 100 mA,
D1 ≤ 15 mA, pull-ups), < 1 °C rise.

### Ground and return

- In2 is one unbroken GND plane; In1 is GND everywhere except the PoE lane. Both pours: clearance 0.2 mm (raised by the
  custom rules near MIPI, MDI and PoE copper), thermal reliefs 0.5 mm gap / 0.5 mm spokes, islands removed. Filled area
  In1 3156 mm², In2 3362 mm² (zone boundaries, In1 PoE lane and In2 plane unchanged; the routing fixes alter antipads).
- Local pours, 83 on F.Cu and B.Cu (priority 10–122, above the planes' 0), join power pins of one net that sit close
  together and carry the high-current nodes: BST_SW (U10.4, L3.2, D3.2), LED_K (Q1.2 with its tab vias, D15.1) and ISNS
  (Q1.3, R69.1; NT1 keeps its Kelvin tap from R69.1, in a lobe of the pour that carries no load current). Each pour:
  clearance 0 (the class and custom rules still set the gap; KiCad fills to the larger), solid pad connection (no
  thermal reliefs), minimum width 0.25 mm, islands removed; names "<parts> <net>" or "<net> pour N". The In1/In2 planes
  keep their thermal reliefs. Where a rule is waived for parts' own pads, the pours are clipped to what the pads keep:
  "BST_SW pour 120" stays 0.4 mm off U10's other pads (pin 4 itself keeps TI's 0.35 / 0.25 mm land gaps), "U1 GND"
  0.225 mm off U1's MDI pins (as the exposed pad) and 0.55 mm off the MDI tracks. F.Cu pours keep ≥ 1.025 mm in plan
  from the In1 PoE copper ([PoE isolation](#poe-isolation)); "LED_K pour 121" keeps ≥ 1.43 mm from the crystal
  copper, so Q1's tab stays the closest LED_K copper to C2.1 (1.42 mm).
- Every decoupling capacitor's GND pad has a GND via within 1 mm, never in the pad: on its own short stub or through a
  GND pour that holds pad and via. The buck capacitors C10, C12, C18 and C19 and U3.2 share the F.Cu pour
  "C19/C18/U3 GND" with ten GND vias.
- U4.12 has its own 0.45/0.2 GND via at (114, 136.8), 0.696 mm centre to centre, on a 0.754 mm F.Cu stub
  (0.25 mm wide, one 45° bend). The pour "GND pour 87" joins C23.2's via (113.75, 136.45) to that via.
- `decoupling_placement` (decoupling capacitor ≤ 3 mm from its power pin, GND via ≤ 1 mm from its GND pad) is clean.

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

C29 sits over U6.74–76; C28 sits on the 0.65 mm +3V3 trunk 1.08 mm north of U6.76's pad, beside the +3V3 via pair
(129.5, 159.2)/(130.5, 159.2) that feeds pins 74–76 on B.Cu. Both ground ends join U6.73's GND via (131.675, 159.45) on
F.Cu (C28.2 0.95 mm from it, edge to edge). Copper paths, via heights counted, one via each: C28.1 → U6.76 4.32 mm,
C28.2 → U6.73 4.67 mm, C29.1 → U6.75 3.62 mm, C29.2 → U6.73 4.82 mm. BST_EN crosses the trunk on B.Cu north of C28 and
hops to F.Cu between vias (130.6, 157.6) and (132.6, 158.55) to pass the +3V3 B.Cu feed.

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
  on B.Cu, 1.6 mm on F.Cu), D+ 81.38 mm with 4 vias, D− 74.91 mm with 4 vias. Full speed is electrically short here (edge ≥ 4 ns, critical
  length ≈ 115 mm), so width, coupling and the 6.4 mm skew don't matter; impedance is not controlled.
- Both lines flow through U5 (ST USBLC6-2 DS11265 Rev 5 p.5 and Fig. 18; TI SLVA680A p.4: no stub between the TVS and
  the protected line). D+: J2.A6 → U5.6 on F.Cu (4.59 mm, no via), a link under the package to U5.1, then via
  (113.8, 112.89) off U5.1. D−: the J2 run comes up at via (115.2, 112.65), 0.013 mm below U5.4's pad; a 0.245 mm F.Cu
  link runs under the package to U5.3 (pins 3 and 4 also join inside the part, DS11265 Fig. 16), and the run to R32
  leaves U5.3 through via (115.6, 113.39). Paths with via heights: J2.A7 → U5.4 12.06 mm (2 vias), U5.4 → U5.3 1.80 mm,
  U5.3 → R32.1 60.88 mm (2 vias). GND return vias: (114.6, 112.95) 0.67 mm from the via at U5.4, (116.25, 113.39)
  0.65 mm from the one at U5.3.
- Module side (USB_D_P/N, R31/R32 → U6.38/U6.37): 8.58 / 8.26 mm, one via each, sharing the GND return via
  (147.5, 138.85) (HDG §1.4.3: return vias at USB via transitions).
- USB vias near U5 lie at x ≥ 113.78 and y ≤ 113.5, 2 mm from the In1 PoE copper.
- USB_C_D_N's J2 branch leaves U5's pocket on B.Cu through a via at (114.1, 109.0); no GND via fits within 1 mm of it
  (nearest: D2.2's at (116.6, 109.9), 2.66 mm), In2 is solid GND under it. USB_C_D_P's 2.8 mm link to U5 crosses the In1
  gap of the PoE lane, with In2 GND below. Both are accepted for full-speed edges ([Accepted deviations](#accepted-deviations)).
- VBUS: J2.A9/B4 (one pad, west) and the 0.6/0.3 via (110, 105.65) share the F.Cu pour "J2 VBUS"; the 0.8 mm B.Cu
  link at y 105.65–105.9 reaches via (114.85, 105.9), and a 0.25 mm F.Cu stub runs from it into the centre of J2.A4/B9
  (one pad, east). From there a 0.5 mm F.Cu run goes south into D2.1's centre. The protected feed leaves inside D2.1's
  east side at (115.25, 109.9), bends 45° to (115.65, 109.5), then reaches the 0.6/0.3 via (115.65, 109.2), all at
  0.5 mm. A 0.8 mm B.Cu return goes north to (115.65, 108.2), NW to (114.65, 107.2), west to (113.6, 107.2)
  and SW to (112.8, 108); only there does the 1.0 mm trunk start toward U4's IN2 via. Thus neither J2 VBUS contact
  bypasses D2.1's junction (TI SLVA680A p.4: flow-through TVS routing, no protected-line stub); D2.2's short GND via is
  unchanged. D2.1 → U4.2 is 37.73 mm of track / 41.83 mm with two via heights and the B.Cu pour "VBUS pour 109" at
  U4's IN2 via (`lengths paths`). The 0.25 mm connector stub keeps ΔT ≈ 11 °C at 1.0 A, 3 °C at 0.5 A (1.8 mm long).
- U5.5 (clamp supply) reaches C25.1 on F.Cu; U5.2 has a GND via between the pin rows.
- CC1 and CC2 (Rd R25/R23): CC2 runs 4.81 mm from R23.1 east under J2's pad row (y 108.64, 2.02 mm from U2.1's PoE pad)
  and 45° up into B5's centre. CC1 leaves A5 north-west into the corridor between J2's west shield pads (y 104.675),
  then runs SW, 0.51 mm centre to centre (0.21 mm copper gap) from the +3V3 loop, down into R25.1 (11.86 mm). R23 sits at (106.85, 108.13);
  its GND pad joins J2's west shield pad through the pour "GND pour 96".

### Buttons and boot

- SW1 (RESET) at (124.8, 160.8) and SW2 (BOOT) at (113, 160.8) stay on the bottom edge (user decision 2026-11-01: "Keep
  on the bottom edge"); the enclosure must give access to both there. Labels "BOOT" and "RST" sit between them.
- BOOT_SW (74.07 mm including two via heights, 70.96 mm track) leaves D16.1 westward directly to the via at
  (107.6, 158.93), rather than branching toward the module from SW2's east pin-1 pad. SW2's two pin-1 pads retain their
  5.1 mm F.Cu link and enter D16.1 from the east; only D16.1 has the outbound protected run (TI SLVA680A p.4).
  D16.2's short GND via is unchanged. B.Cu goes SE to (108.25, 159.58), south to (108.25, 162.9), then SE to
  (108.75, 163.4) and east along the bottom edge, 1.05 mm below U6's bottom pad row and 0.5 mm copper to edge, tented.

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
| CAM (J4 → links) | 2 | Via 1 under J4's body north of its pad, on the pad axis (y 108.45; CLK at (133.7, 106.95)/(133.7, 107.55)); via 2 north of the link row at y 110.5–110.6 |
| CSI (links → U6) | 1 | South of the link row at y 113.4 (D0_N at (136.75, 112.85), fed west from R51.1, because the PoE lane leaves no legal spot for its return via at (137.5, 113.4)) |

Every signal via has a GND through via within 1.0 mm centre to centre (the D0/D1 via-1s, on J4's pad axes since 2026-11-02, at exactly 1.000 mm; `via_return` limit 1.0 mm; TI SPRAAR7J §3.9
p.12: a return via at each transition, p.8: stitching vias "within 200 mils" of the signal vias). Neighbouring legs share
one where the in-row spots are taken, e.g. (133.7, 108.45) serves CAM_D1_N, CAM_D0_P and CAM_CLK_N. The GND vias under J4
at x 127.7/130.7/133.7/136.7 have F.Cu stubs to J4's GND pads 10/7/4/1.

### CSI bus and the plane neck

The CSI legs run on B.Cu over In2 to U6.28–U6.33 as a three-pair bus, pair centres 0.87 mm apart, CLK between D0 and
D1. Spacing: ≥ 0.36 mm (2W) to everything but the partner, ≥ 0.54 mm (3W) to USB, MDI, all RMII, XVS/XVS_CAM and
STROBE/STROBE_CAM (rules above). The CLK pairs have no GND guard copper (HDG §1.4.6 p.28 asks for the MIPI CLK pair to be surrounded by ground copper); a B.Cu guard
each side would widen the bus to ≈ 3.37 mm and leave ≈ 1.41 mm a side at the neck below instead of 1.97 mm (user decision
2026-11-01: "Accept 2W, no guard").

The bus passes through a neck of the In2 reference between the lens plane edge (r 8.3 mm) and the 2 mm PoE antipad of
J1.9 (144.31, 121.19): 17.22 − 8.3 − 2.725 = 6.19 mm wide, against the 6.83 mm (2.25 mm bus + 2 × 2.29 mm) that TI
SPRAAR7J p.9's 90 mil from bus to plane edge asks for. Widening it would mean moving J1 (locked at the edge) or the lens
and ring (approved floorplan). The bus crosses the neck at 45° (route rule), corners (138.45, 123.14), (139.21, 123.90),
(139.21, 124.14), (141.5, 126.43): measured track edge to plane edge 1.841 mm (72.5 mil) to the lens edge and 1.842 mm
to J1.9's antipad (user decision 2026-11-01: "Use 45°, accept 71.7 mil"; the ideal 58° crossing gave 77 mil).

At the FFC notch the In1/In2 planes end at y 103.30. Track edge to that plane edge: CAM_D1_P 1.375 mm (54 mil, over
≈ 6.3 mm of its B.Cu run at y 104.765 under J4, x 132.73–139.0; 9.3 mm of it lies inside 90 mil), CAM_D1_N 1.705 mm
(67 mil), CAM_CLK_N 2.245 mm (88 mil), CAM_CLK_P 2.265 mm (89 mil), against SPRAAR7J p.9's 90 mil (2.286 mm). Only the
D1 pair is materially short; meeting it needs a new crossing plan under J4.
Accepted with the user decision above.

The J1 antipads in In2 are voids, not plane edges, and SPRAAR7J p.9 asks for ≥ 1.5 W (0.27 mm) from voids. CSI_D1_P's via
(142.7, 113.4) antipad (r 0.585 mm) joins J1.3's ETH_CT void on In1 and In2 (the 0.108 mm web between them is under the
pour's 0.25 mm minimum width, so it is not filled): the leg leaves its via inside that void. Further on it passes J1.5's
antipad at 1.227 mm, J1.7's at 1.566 mm and J1.9's at 1.843 mm.

The planes stay solid under the wide pads on the lanes except where the MIPI via antipads (r 0.585 mm) reach under them,
against TI SPRAAR7J §3.10 p.13 ("voiding the SMD mounting pads of the reference plane by 100%"):

- Link pads (0.54 × 0.64 mm, rotated 90°: 0.54 mm in board Y, south edge y 113.03): the vias at y 113.4 sit on their
  pads' axes (the CLK vias at R45/R49 moved there 2026-11-02) with their north antipad boundary at y 112.815, voiding at
  most 0.215 mm of the south pads of R45, R46, R47, R49 and R50. Integrating the circular antipad over each rectangular
  pad gives ≈ 34 % voided area, not a uniform strip across their width. R51's west edge is separately voided by the
  D0_N via (136.75, 112.85), ≈ 20 % of its pad area by the same approximation. The north pads are fully backed.
- J4's MIPI pads 2, 3, 5, 6, 8, 9 (0.6 × 1.8 mm): the via-1 antipads take ≈ 0.06 mm off pads 2, 3, 5 and 6.
- Adjacent antipads join into one slot void along each via row (y 113.4, y 110.5–110.6 and the via-1 pairs at y 108.45).

Estimated excess capacitance, using the same parallel-plate approximation C = ε₀ · 4.4 · backed area / 0.2104 mm:
fully backed link pad ≈ 0.064 pF (rounded to 0.07 pF); south aligned pad (CLK included) ≈ 0.042 pF,
R51's south pad ≈ 0.051 pF. North link pads remain ≈ 0.07 pF and J4 pads just under 0.2 pF, a reflection of at most
1–3 % at a 150 ps edge (rectangular-pad/circular-antipad estimate, not measured); accepted at design review
2026-11-01, covered by the first-board CSI test.

### Lengths and skew

Tracks 0.18 mm, gap 0.15 mm. Skew compensation uses trapezoid bumps (45° legs, a flat ≥ 0.45 mm, ≥ 0.45 mm of straight
track between same-sense bends, no copper overlap), placed next to the mismatch, so the pad-to-pad copper path equals
the net length. Lengths (via heights counted; pad to pad equals net length on every MIPI net):

| Pair | P (mm) | N (mm) | Skew (mm), ≤ 0.127 |
|---|---|---|---|
| CAM_CLK | 24.8350 | 24.8690 | −0.034 |
| CAM_D0 | 24.6438 | 24.6679 | −0.024 |
| CAM_D1 | 24.7444 | 24.7155 | +0.029 |
| CSI_CLK | 32.3771 | 32.4599 | −0.083 |
| CSI_D0 | 32.5014 | 32.4986 | +0.003 |
| CSI_D1 | 32.4931 | 32.4931 | 0.000 |

`lengthGroups` "CSI pairs, P4 side" and "CAM pairs, FPC side": pair-mean spread ≤ 0.381 mm per segment (HDG §1.4.6
pair-to-pair ≤ 30 mil = 0.762 mm end to end, half per segment). Spread: CAM 0.196 mm, CSI 0.082 mm.

### Other parts near the link field

C3 (ETH_CT, 100 nF) at (142.3, 115.4) ties its GND end to the GND via (141.14, 114.1) by a 0.2 mm F.Cu track; ETH_CT runs
on F.Cu from C3.1 to J1.3 (2.54 mm). J4 is marked "1" and "CONTACTS DOWN" on the top silk.

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
- Lengths (via heights counted; pad to pad equals the net length): TD_P / TD_N 33.2530 / 33.2188 mm, RD_P / RD_N
  27.7055 / 27.4156 mm, skew +0.034 / +0.290 mm, within TI SNLA387 p.2's 50 mil (1.27 mm) for 100 Mbit/s, which the
  rule "MDI intra-pair skew" enforces. TD_N carries three 45° bumps and RD_N one; the radial via and pad entries of
  2026-11-02 added the rest to the P lines.
- Return path: the N tracks pass the swap vias' In1 antipads (r 0.775 mm) and the tails at J1.2/J1.4 cross J1's own MDI
  antipads; accepted ([Accepted deviations](#accepted-deviations)).

### RMII

| Constraint | Entry | Value | Reason |
|---|---|---|---|
| Series resistors at the driving pin | `near` R35.1–U6.46, R36.1–U6.47, R37.1–U6.67 (TXD0, TXD1, TX_EN at the MAC); R11.1–U1.18, R12.1–U1.17, R15.1–U1.19 (RXD0, RXD1, CRS_DV at the PHY); R13.1–U1.14 (REF_CLK series resistor, HDG §1.3.14, at the PHY's 50M_CLKO) | ≤ 3 mm | 3 mm ≈ 17 ps at 5.8 ps/mm, far below any edge ≥ 0.5 ns: the resistor works as a lumped source termination |
| Long data segments | `lengthGroups` "RMII data, long segments": RXD0/RXD1/CRS_DV after the PHY resistors, TXD0/TXD1/TX_EN after the MAC resistors | ≤ 80 mm each, no spread limit | The ESP32-P4 publishes no RMII output delay (IP101G p.61 and RMII 1.2 §7.4 give the rest), so the board takes a budget of 1.0 ns (5 % of the 20 ns period, a heuristic) for the MAC clock branch plus one TX line: (60 + 80) mm × 7.1 ps/mm = 0.99 ns. IP101G §6.1 "equal" is met within this tolerance. Derivation: [ethernet](ethernet.md#timing) |
| REF_CLK star | `paths` "REF_CLK MAC branch" from R13.2 to U6.68 | R13.2 → U6.68 ≤ 60 mm | U1.10 sits near the source end: its edge waits on a ledge until the reflection from U6.68 returns, 2 × v × L_MAC after it arrives, so the limit is on the MAC branch measured from the star. No ledge while that round trip, the module's internal trace included, is no longer than the fastest RMII edge (1 ns, RMII 1.2 §7.4.3): L_MAC ≤ 1 ns / (2 × 7.1 ps/mm) = 70 mm, minus 10 mm reserved for the unpublished trace inside the module = 60 mm. The PHY/MAC difference (36.41 mm) is only a note against IP101G §6.1 "equal" (no tolerance given); RX/TX timing alone allows ≈ ± 420 mm ([ethernet](ethernet.md#ref_clk-branches)) |

Routed (via heights counted): RXD0 18.47, RXD1 17.92, CRS_DV 18.76, TXD0 21.61, TXD1 23.93, TX_EN 41.26 mm. REF_CLK:
R13.2 → U1.10 10.60 mm (2 vias), R13.2 → U6.68 47.01 mm (3 vias; round trip with the module's 10 mm 2 × 57.01 mm ×
7.1 ps/mm = 0.81 ns), difference 36.41 mm.

- The RX resistors R15/R11/R12 stand vertical in a row east of U1 with their vias south of them; U1's RX pin order is the
  reverse of U6's, so the B.Cu runs pass north of the vias to their west and arrive in U6 order without a crossing.
- REF_CLK runs ≥ 0.6 mm (3W) from the RMII data lines wherever their tracks run side by side (IP101G §6.1 p.56: "Avoid
  signals path parallel to clock signals path"): 0.613 mm to CRS_DV on B.Cu, 0.65 mm to TXD0 on F.Cu; the +3V3 trunk runs
  beside it at 0.35 mm for 8.1 mm. Closer spots that are not parallel runs: the REF_CLK via (157.45, 138.45) 0.49 mm from
  the CRS_DV B.Cu track and 0.50 mm from a TXD0 via; R13.2 0.41 mm from R12.2 (RXD1) in the resistor row; +3V3 copper
  0.20 mm at a via; U1.10 0.2 mm from U1.9 (TXD0) at the 0.4 mm pin pitch. REF_CLK_SRC and U1.10's pin stub run 0.15 mm
  where 0.2 mm does not fit.
- GND return vias 0.45/0.2 sit within 0.65–1.1 mm (centre to centre) of most RMII, REF_CLK, XVS and STROBE layer changes
  (TI SNLA387 p.4: return vias close to signal vias). Eight layer changes have none within 1.1 mm and no legal spot for
  one within 1.6 mm (the original grid search against every track, via, pad, rule area and edge). Seven have no GND via
  within 1.6 mm at all: REF_CLK at (157.45, 133.75) and (157.45, 138.45), TXD0 (156.5, 133.45), TXD1 (155.95, 132.9),
  TX_EN (155.4, 131.75) in U1's south-west fan-out, TX_EN_MCU (138, 162.6), and STROBE (117.60, 130.30)
  (nearest 1.91–4.76 mm). STROBE (115.95, 131.95) has one at 1.42 mm. These are 50 MHz RMII and slow strobe lines
  without an impedance class; the eight distant returns remain accepted. `via_return` does not check them (Default class).
  All seven XVS layer changes now have a GND via within 0.67–0.90 mm:
  (128.5, 149.65) → (128.7, 148.9), 0.776 mm; (124.2, 149.38) → (123.45, 149.38), 0.750 mm;
  (123, 140.75) → (123.45, 141.4), 0.791 mm; (117.65, 139.9) → (117.6, 140.57), 0.672 mm;
  (113.75, 135.7) → existing (113.75, 136.45), 0.750 mm; (109.3, 135.7) → (109.7, 135), 0.806 mm;
  north (131.2, 104.8) → existing (130.5, 105.35), 0.890 mm. The old XVS vias (123.98, 151.27),
  (122.9, 152.45), (121.4, 152.6), (120.55, 155.2), and their now-unused return at (121, 155.7) are gone.
- XVS stays outside the actual L1/L2/L3 courtyard polygons on every layer, including its pads and via annular rings.
  U6.9 exits from its right edge (128, 150.75) on B.Cu through (128.15, 150.6) → (128.15, 150.3) →
  (128.5, 149.95) to via (128.5, 149.65), then F.Cu goes (128.23, 149.38) → via (124.2, 149.38), between C34's pads.
  B.Cu goes (123.5, 148.68) → (123.5, 143.1) → (122.85, 142.45) → (122.85, 141.15) → (123, 141) →
  via (123, 140.75). F.Cu passes (122.75, 141) → (121.1, 141) → (120.6, 140.5) → (119, 140.5) →
  (118.4, 139.9) → via (117.65, 139.9). B.Cu passes (116.8, 139.05) → (116.3, 139.05) → (114.55, 137.3) →
  (114.55, 136.5) → via (113.75, 135.7); F.Cu crosses west to via (109.3, 135.7); B.Cu goes NW to
  (105.1, 131.5), then north to (105.1, 108.25) and rejoins the original north run. The detour under D3 is gone.
  Total XVS: 104.25 mm track (F.Cu 29.63, B.Cu 74.62) / 115.11 mm with seven via heights, 9.47 mm longer than
  the original 105.64 mm route. Projected copper-to-courtyard-outline gaps from the queried library polygons are
  L1 2.906 mm, L2 0.048 mm and L3 0.450 mm (to Fab body outlines: 3.252, 0.365 and 0.700 mm).
  L2's courtyard is stepped: its west boundary at U6.9's y range is x 128.36, not the bbox's x 127.39.
  U6.9 ends at x 128.0, 0.36 mm clear; the former bbox-overlap claim was incorrect. No footprint move is needed.
  All-layer projected copper distances (`distance`, no layer filter): BST_SW 2.088 mm, +5V_BST 0.850 mm,
  U10 pads 6.290 mm, C43 pads 8.194 mm, C45 pads 3.777 mm and D3 pads 5.217 mm. The boost hot loop is avoided;
  the +32V branch that used to cross above U6.9 was moved to the bottom edge on 2026-11-03 ([+32V and the ring](#32v-and-the-ring)).
  To open this corridor without moving parts, the B.Cu STATUS_LED/CAM_EN/STROBE/SCCB_SCL/SCCB_SDA fan-outs were
  shifted east; total net lengths with via heights are 21.63/62.06/104.02/60.37/63.13 mm respectively, via counts unchanged.
  PWR_SRC's B.Cu leg now goes (118.95, 139.3) → (118.95, 140.79) → (116.29, 143.45) → (116.29, 144.975), then jogs to
  x 116.4, making room for the (117.6, 140.57) return; total 36.51 mm, three vias. R72.2's own GND via is now (123.45, 141.4),
  on a 0.354 mm F.Cu diagonal, and R71.2's VLED_SENSE branch passes y 141.45 through (122.75, 141.45) →
  (122.9, 141.6) → (122.9, 141.82) → C49.1; total VLED_SENSE 14.54 mm of track (16.09 mm with the via height), one via.
  C35.2's GND via moves (122.655, 144.25) → (122.525, 144.18), U8.2's GND via (124.6775, 146.25) →
  (125.05, 146.3), and C34.2's GND via (122.93, 149.85) → (123.85, 151.1). Their pin-centre-to-via F.Cu stubs are
  0.875, 1.759 and 0.824 mm respectively. U8.2's full stub includes the unchanged 0.800 mm segment plus the
  0.636 mm diagonal and 0.323 mm outer leg; its via is 1.636 mm centre-to-centre from the pin.
  C34.2 is 0.781 mm centre-to-centre from its via, outside its pad.
- U1's exposed pad has 4 GND vias (0.45/0.2) on a 1.0 mm grid; U1.1 (GND corner pin) has its own via.
- Crystal Y1: no GND guard between it and the LED driver copper (IP101G §6.1: "Use guard traces to protect clock traces if
  possible"); there is no room for one (a 0.2 mm guard needs ≥ 0.6 mm, the gap is 0.4 mm). It relies on distance and the
  In1/In2 planes: C2.1 (XTAL_X1) to the filtered +32V track 0.45 mm, Y1.1 to +32V 1.51 mm, the switched LED_K tab 1.42 mm
  from C2.1, the B.Cu LED_K zone behind In1 and In2.
- Crystal loop: XTAL_X1 runs U1.2 → Y1.1 for 7.47 mm, round Y1's north side ((156.1, 129.8) → (155.6, 126.8) → (153.1,
  126.55) → Y1.1), against 1.49 mm for XTAL_X2 (U1.3 → Y1.3); the two traces enclose ≈ 3.7 × 3.6 mm (x 152.95–156.65,
  y 126.55–130.2) directly over In1 GND. Y1's pins 1 and 3 sit on opposite corners of the 3225 package, so turning Y1 only
  moves the long leg to X2. Low risk at 25 MHz over a solid plane; accepted (user decision 2026-11-03: "Accept and
  document").
- Tracks under the crystal circuit (Y1, C2, C5, XTAL_X1/X2 and U1.2/U1.3), all on B.Cu with the In1 and In2 GND planes
  between them and the crystal (IP101G §6.1 p.56: "Avoid signals path parallel to clock signals path … such as MDC
  and X1"):
  - MDC runs west under U1 at y 129.7, under the U1.2/U1.3 (X1/X2) pins, then diagonally (155.95, 129.7) → via
    (153.05, 132.6) past Y1.3's corner and C5.1's, parallel to the XTAL_X2 diagonal ≈ 0.14 mm away in plan for ≈ 0.85 mm.
  - MDIO runs at y 129.15 under the XTAL_X1 vertical (x 155.6) and Y1.3's north edge, then diagonally to (151.3, 132.9),
    branching to R14.2 at via (153.75, 130.95).
  - The +3V3 feed to R14 runs diagonally (156.25, 126.3) → (152.45, 130.1) under Y1's body.
  - ETH_LED_G crosses only the X1 link C2.1–Y1.1, at right angles at (152.0, 127.6), 0.19 mm in plan from C2.1 and
    0.25 mm from Y1.1.

  MDC and MDIO cannot leave the area. U1's management pins (22, 23) are on its east side and U6.43/U6.70 lie to the
  south-west. On B.Cu the only way west from under U1 lies between the crystal and the tops of the RMII fan-out (COL via
  (155.65, 130.75), TX_EN (155.4, 131.75), TXD1 (155.95, 132.9), TXD0 (156.5, 133.45), REF_CLK (157.45, 133.75)): the
  strip below them (x 152.75–157.3) runs south to y ≈ 138.75, the MAC-bound RMII diagonals close the way round it, and
  REF_CLK keeps ≥ 0.6 mm (3W). East lie ETH_LED_Y's B.Cu run at x 162.85 and the board edge (x 164); north the MDI and
  J1's pin field; west the Q1 drain zone (x ≤ 151.0). F.Cu round U1 holds the pins, the crystal traces and the resistor
  rows. MDC (≤ 3.33 MHz) and MDIO are slow, and two GND planes separate them from the crystal: accepted
  ([Accepted deviations](#accepted-deviations)).
- JLCPCB recommends 1.0 mm from a QFN to any part ("Minimum Spacing Requirements for SMD Components",
  https://jlcpcb.com/help/article/minimum-spacing-for-smd-components, read 2026-10-31). At U1 every part keeps it:
  closest Y1.3–U1.1 1.000 mm, C9.1–U1.13 1.020, R7.1–U1.9 1.022, C7.1–U1.32 1.028, R1.2–U1.10 and R13.1–U1.16 1.030,
  R12.1–U1.16 1.067 mm.
- The Ethernet +3V3 loads (U1, pull-ups, LED anodes, ≈ 0.1 A, taken as 0.15 A) take the 0.65 mm class width to R38.1 and
  0.4 mm after it (ΔT ≈ 0.15 °C); 0.2–0.3 mm only where nothing wider fits.
- ETH_LED_G (25.46 mm, 3 vias) leaves J1.14 on B.Cu along y 125.2, north of the crystal, comes down at x 152.0 between C2
  and Y1 to via (151.75, 128.87) and runs on F.Cu west of the crystal (x 151.35) towards R4; ETH_LED_Y runs east (B.Cu,
  1 via). ETH_SHIELD and ETH_SHIELD_R are 0.5 mm (chassis/ESD path). R2 is turned so its ETH_SHIELD_R pad faces R3:
  ETH_SHIELD_R is one 1.675 mm F.Cu link R2.2 → R3.1, and ETH_SHIELD reaches R2.1 from the south-west under J1.12
  (2026-11-02; ETH_SHIELD_R ran 13.3 mm round R2 with two vias before).

### PHY-side decoupling

| Pair | Max | Reason |
|---|---|---|
| C9.1–U1.13 | 3 mm | IP101G §6.1 p.56: VDDIO decoupling |
| C6.1–U1.28 | 3 mm | IP101G §6.1 and pin 28 p.18: REGOUT 0.1 µF |
| C4–C6 | 3 mm | IP101G pin 28 p.18: REGOUT 10 µF beside the 0.1 µF |
| C7.1–U1.31 | 3 mm | IP101G §6.1 p.56: AVDD33 decoupling |

The `near` entries measure straight copper edge to edge (C6.1–U1.28 2.32 mm), not the copper path. REGOUT's path is longer:
U1.28 → C6.1 is 4.59 mm of track with two vias, F.Cu north to via (158.8, 126.9), B.Cu to via (160.9, 128.2), then the
F.Cu pour "C6/C4 PHY_REGOUT" that holds C6.1 and C4.1 (8.51 mm with the via heights and the pour as a straight line;
U1.28 → C4.1 9.33 mm), because pin 28 lies between the two MDI pairs and its track
can leave only northwards inside "U1 MDI escape". A first-board check scopes REGOUT at C6
([ethernet](ethernet.md#first-board-checks)); accepted ([Accepted deviations](#accepted-deviations)).

### PoE isolation

The four PoE cable nets (POE_VA1, POE_VA2, POE_VB1, POE_VB2: from J1's centre taps to U2's input pins 1–4) carry the
cable side. The layout is meant to give some isolation between them and the rest of the board, with the geometry below
(2 mm same-layer rule, the In1 lane, ≈ 1.0 mm sideways on F.Cu); no isolation level is rated or tested (user decision
2026-11-02).

Layer plan:

- The PoE tracks run on In1 only, 0.4 mm wide with 0.2 mm gaps (4 × 0.4 + 3 × 0.2 = 2.2 mm), in one lane. In1 is GND
  elsewhere; In1 tracks are allowed only inside the lane (rule area "In1 GND plane, PoE lane excepted"); In2 is solid
  GND with no tracks (rule area "In2 GND plane").
- Heating and drop: each net carries up to a pair's 350 mA (HanRun HR871150C p.1). In1 is 0.5 oz (0.0152 mm, 0.6 mil),
  so a 0.4 mm track is 9.4 mil². IPC-2221's fit for internal conductors (I = 0.024 · ΔT^0.44 · A^0.725, A in mil²) gives
  ΔT ≈ 11 °C at 350 mA; it ignores the GND copper of In1 and In2 round the lane, so the real rise is lower. Resistance
  2.83 mΩ/mm (ρ 1.72 × 10⁻⁸ Ω·m): 38.2–42.2 mm per net (`kicad_query lengths`) is 0.108–0.119 Ω, ≤ 42 mV and ≤ 15 mW
  per net at 350 mA.
- Lane on In1 (≥ 2.3 mm wide everywhere): U2 end x 108.0–111.75, y 110.6–120.5; west band y 115.7–118.0; a 45° step to
  the middle band y 114.2–116.5 (x 123.8–133.4) under the ring top; a 45° staircase (steps 0.6 and 2.0 mm with a 0.8 mm
  level) to the east band y 116.8–119.1; 45° walls into the J1 end x 143.0–147.6, y 119.05–123.4. Every bend is 45°.
- In1 GND keeps 2 mm from the PoE copper (rule "PoE isolation", DRC clean). PoE copper to other nets' via annular rings
  on In1: ≥ 2.004 mm (+3V3 via (122.1, 112.9)); GND vias ≥ 2.025 mm.
- Every other net crosses the lane on B.Cu over In2, with vias ≥ 2 mm from the lane copper. +32V, +5V and +3V3 never run on
  In1 or In2.
- The "PoE U2 pins" rule area (x 106.3–112.1, y 108.76–122.18, all copper layers) keeps vias and pours away from U2's
  cable-side pins.

Across layers (F.Cu over In1), which KiCad does not check: no F.Cu copper of another net lies over the In1 PoE copper or
within 1.0 mm of it sideways. Closest (to the routed PoE copper): TP10.1 (CAM_3V3) exactly 1.000 mm from POE_VA1 (TP10
cannot move: R53, J4, C25/U5 and C32 box it in); the F.Cu pours "R41/R42 +3V3" 1.025 mm, "GND pour 92" (U5.2) and
"R53/C32/C30 CAM_3V3" 1.03 mm (outlines clipped to keep 1.0 mm); U5.1–3 1.05 mm, D6.1 and D7.2 1.09 mm, R52 1.10 mm,
D5.1 1.17 mm, R41.1 and R43.2 1.18 mm, C32.2 1.27 mm, C3.2 1.36 mm; F.Cu tracks beside the lane: RING_2 1.256 mm, GND
(128.15, 112.8)–(128.95, 112) 1.30 mm. One 0.2104 mm 7628 ply lies between F.Cu and In1; a 2 mm sideways
keep-off does not fit.

J1: the rule exempts pad pairs inside J1, the jack's own pin field (HanRun rates it 2250 V dc, cable side to chip side,
HR871150C p.1). J1's pads are trimmed to Ø1.45 mm on Ø0.89 mm holes (signal) and Ø2.35 mm on Ø1.63 mm (shield), so pin 5
(chip side) to pin 7 (cable side) is 1.09 mm and the shield to pin 10 1.97 mm.

U2: the DP1435's input is isolated from its output (1.5 kVrms, DS p.1, p.5 item 9). Its cable-side pins U2.1–U2.4 have no
capacitor to board GND: the datasheet's typical application (p.4) wires them straight from the jack, and a capacitor to
GND would bridge the module's isolation. That is why the decoupling check exempts U2.1–U2.4.

The top silk marks "POE 57V" beside J1 and U2.

## IMU

Parts: U12 MEMSensing MSA3S02 (LGA-12, 2 × 2 mm, 0.5 mm pitch) at (137.2, 158.4) on the top side above U6, C50 100 nF,
R73/R74 I²C pull-ups. Circuit: [imu](imu.md).

- Land pattern: the eight corner lands are 0.025 mm shorter at their inner end (0.375 mm) so every gap between pads of
  different nets inside U12 is 0.15 mm (JLCPCB SMD pad-to-pad minimum); rule "U12 LGA pads".
- IMU_INT1 leaves pin 5 north to a tented via under U12's body centre (137.2, 158.4) on a 0.15 mm track (≥ 0.32 mm from
  every pad) and runs on B.Cu to U6.69. No other vias lie under the LGA pads; the datasheet gives no via or trace keep-out
  under the package. GND pin 9 and NC pin 8 (to GND, as the datasheet recommends) run to a GND via at (138.1, 156.2); NC
  pin 4 joins C50.2 on F.Cu.
- `near` C50.1–U12.7 ≤ 3 mm (MSA3S02 DS V1.0 p.30 Fig. 8: 100 nF at VDD): placed 1.15 mm. C50.2 has no GND via of its own
  (U6's bottom pads below and the +32V F.Cu that ran round it left no site; since the 2026-11-03 reroute the nearest +32V
  copper is the bottom-edge run 1.66 mm south, and the via was not re-sited); the F.Cu pour "C50/U12 GND" joins it and NC pin
  U12.4 to the GND via (134.69, 159.18), which satisfies `decoupling_placement`. Its loop to U12's GND pin 9 is C50.2 →
  pour → GND via (134.69, 159.18) → In1 → GND via (138.1, 156.2) → pour "U12 GND" → U12.9, 9.08 mm with 2 vias (pours
  as straight lines; U12.4 is an NC pin, not a ground return). Harmless for a 110 µA sensor (DS p.7).
- I²C away from the buck switch node (TI SLVSDG1C §10.1 p.13: "Keep these traces away from SW nodes"): IMU_SCL and
  IMU_SDA run on B.Cu under L2 (with In1 and In2 GND between them and L2's switch pad) and come up to F.Cu only east of it,
  at vias (137.82, 152.95) and (137.28, 153.6). F.Cu distance to SW_3V3 copper: SCL 1.43 mm, SDA 1.015 mm. SDA crosses the
  corner of L2's courtyard for ≈ 0.7 mm, 0.69 mm from the body corner (it must pass U12.1 to reach U12.2).
- JLCPCB's 1.0 mm from a QFN/LGA to other parts holds for C50 (1.02 mm, C50.1–U12.5) and R37 (1.33 mm); R73 is 3.2 mm away. It does not hold for R74: R74.2–U12.2
  0.64 mm. R74.1 (+3V3) had to keep 0.4 mm from the +32V F.Cu at x 133.35 ("LED_PWR pad spacing (other pads)"); that
  copper went with the 2026-11-03 reroute (R74.1 is now 4.18 mm from +32V), but R74 was not moved, and a vertical R74
  still hits the C50 GND via or L2's courtyard. Accepted.

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
- C43.2, C36.2 and the exposed pad (U10.3, U10.7) share the F.Cu GND pour "C36/C43/U10 GND" with four GND vias, so both
  capacitor loops close on the top layer as in TI Fig. 23. C45.2 keeps its own via: D3.2 (BST_SW) lies between it and
  C43.2.
- The switch node U10.4 → L3.2/D3.2 is the F.Cu pour "BST_SW pour 120", 0.4 mm off U10's other pads, so pin 4 keeps
  TI's land gaps (0.35 mm to pin 5, 0.25 mm to the exposed pad) and the node stays small (§11.1).
- +5V_BST: U10.6, C36.1, C38.1, L3.1 and U8.6 share the F.Cu pour "U10/C36/L3 +5V_BST".

### +32V and the ring

- +32V goes from the boost output (D3's cathode) to C47.1, on to C46.1, then north through a corridor east of Q1 to D4.2
  (`kicad_query lengths` paths, via heights counted, pours as straight lines): D3.1 → D4.2 89.9 mm (84.1 mm of track,
  2 vias), C45.1 → D4.2 92.9 mm, of which C45.1 → C47.1 30.8 mm (no via), C47.1 → C46.1 37.2 mm (two 0.6/0.3 vias, B.Cu
  east of U6's outline) and C46.1 → D4.2 24.9 mm (F.Cu, no via, ending in the pour "+32V pour 107" over D4.2's two pads).
  D3.1, C43.1, C45.1 and TP13.1 share the F.Cu pour "D3/C45/C43 +32V". The whole +32V net is 115.7 mm (112.6 mm of track
  plus via heights), which adds the 0.3 mm R65/R71/TP13 feeds (20.9 mm) and short stubs.
- No +32V copper lies under U6's body (user decision 2026-11-03: "Reroute +32V out of U6"; whether the module's
  underside has exposed copper is not known, [mcu](mcu.md#accepted-risks)). The boost-to-C47 leg runs on F.Cu only: from D3.1
  south, east under SW1's body between its pads at y 160.6, down a 45° diagonal between SW1.2's east pad and C29.1
  (0.43 mm to each, against the 0.4 mm "LED_PWR pad spacing (other pads)"), then east at y 162.85 along the bottom edge
  into the run at (137.36, 163.35) to C47.1. It is 0.4 mm wide for 21.9 mm: it carries only the boost's recharge current
  (≤ 0.1 A average, ≈ 0.22 A peak = 1.44 A ILIM × 5 V / 32.6 V), ΔT ≈ 0.3 °C at the peak, ≈ 27 mΩ. It replaced a 0.4 mm
  F.Cu run north of L2 and a 0.6 mm B.Cu hop between vias (128.7, 151.25) and (132.15, 155.5) under the module.
- The pulse current comes from C46: 12.9 mm at 0.45 mm (1.09 mΩ/mm) plus 11.7 mm at 0.6 mm (0.82 mΩ/mm) ≈ 24 mΩ,
  ≈ 22 mV at 0.915 A (35 µm copper, ρ 1.72 × 10⁻⁸ Ω·m). ΔT ≈ 3.5 °C at 0.915 A DC for 0.6 mm. East of Q1 it runs
  0.45 mm wide for 12.9 mm at x 150.625 (ΔT ≈ 4.8 °C at 0.915 A DC; the pulses run at ≈ 6 % duty), held 0.4 mm from
  Q1's tab and from C2/C8 by the pad-spacing rules.
- The ring links and the LED_K neck are 0.5 mm (1.0 mm breaks the 0.4 mm pad spacing to each LED's other pad): ΔT
  ≈ 4.2 °C at 0.915 A DC. Each link leaves and enters its LED pads along the pad's own axis (the pads sit at
  15° + k · 30° round the ring), then turns onto 45° legs; these axis stubs and their 15°–60° bends are the one
  exception to the 45° rules ([Routing style](#routing-style)). Each LED's two pads of one node are joined centre to
  centre. LED_K: Q1.2 and D15.1 share the F.Cu pour "LED_K pour 121".
- Exposed pads of LED_PWR/BST_SW keep 0.4 mm to other nets, and other exposed pads keep 0.4 mm to LED_PWR/BST_SW copper
  (the two "LED_PWR pad spacing" rules). TH1 (the NTC) at (145.2, 127.8) is 0.42 mm from Q1's tab, which only tightens its
  thermal coupling to Q1.
- PHY_RST_N (76.4 mm, 5 vias) and NTC_SENSE (≈ 59 mm, mostly B.Cu) route round the lens inside the LED ring, PHY_RST_N ≈ 41 mm of
  it at 0.5–1.1 mm from the ring pads and links. Both are slow lines: PHY_RST_N is static with R8 pulling it down,
  NTC_SENSE has C48 at the ADC pin.

### Current sense

- ISNS: Q1.3 and R69.1 share the F.Cu pour "ISNS pour 122" (2.63 mm pad to pad); NT1 takes the Kelvin tap from R69.1;
  TP14 by 0.25 mm.
- R69.2 (the 0.915 A return) sits in the F.Cu pour "GND pour 80" with C44.2 and three GND vias, (148.5375, 142.1),
  (148.18, 142.45) and (147.65, 142.2); a 0.4 mm F.Cu link reaches the GND via (147.5, 138.85). No via fits at the pad
  (U6's bottom pads 34–38 lie under R69). Return resistance is below the ≈ 0.8 mΩ estimated for the former three
  0.4 mm links (0.8 % of R69), which only lowers the LED current.
- C46.2 has four GND vias: three 0.6/0.3 (hole 0.15 mm outside the pad) and one 0.45/0.2 at (161.45, 141.6) (hole 0.25 mm
  outside the pad).

### Thermal

Q1 dissipates up to 0.73 W average at the 0.5 ms / 120 fps limit ([led](led.md#thermal)). IRLR024N datasheet p.1: RθJA
110 °C/W on the minimum pad, 50 °C/W on 1 in² (645 mm²), RθJC 3.3 °C/W, TJ max 175 °C.

- Drain copper (`copperArea` "Q1 drain copper", net LED_K, F.Cu + B.Cu within 15 mm of Q1: ≥ 100 mm², ≥ 12 vias): Q1's
  6.4 × 5.8 mm tab with 16 thermal vias 0.6/0.3 (4 × 4), the F.Cu pour "LED_K pour 121" round the tab and D15.1 (47.4 mm²
  filled, the tab included), and the B.Cu zone "Q1 drain B.Cu"
  (x 144.8–151.0, y 126.7–137.65, solid pad connection, 61.96 mm² filled). That is the ≈ 110–120 mm² that fits; 1 in²
  does not: `kicad_query board_free_space` finds no free 7.5 × 11.5 mm area on the top side for Q1 to move to.
- The zone stays clear of the crystal (Y1/C5), the RMII B.Cu strip (x 152.75–157.3), the CSI bus (1.04 mm from CSI_D1_P,
  ≥ 0.54 mm) and U6's courtyard, and never reaches the ring side.
- Estimate at 30 °C ambient: TJ ≤ 30 + 0.73 × 110 = 110 °C on any copper at least the minimum pad (the datasheet gives
  nothing between that and 1 in²); case ≤ 108 °C. Both are inside TJ 175 °C.
- Q1's tab is 1.74 mm from D15 and 1.79 mm from D4, C46 7.27 mm, C47 22.3 mm. The LED current is set for only 1.35 °C of
  LED heating above ambient, which a tab at up to ≈ 108 °C nearby almost certainly exceeds; C46/C47 are rated to 105 °C.
  Accepted pending measurement (user decision 2026-11-01: "Measure first boards").
- The Ethernet crystal Y1 sits 2.35 mm from Q1's tab (Q1.2 to Y1.2) and 1.35 mm sideways from the B.Cu zone "Q1 drain
  B.Cu" (its east edge x 151.0 to Y1.1, on the other side of the board); Y1 is rated −40…+85 °C, and its ±20 ppm holds only
  over that range (Epson TSX-3225 X1E0000210139 p.1). U1 (IP101GRI, 85 °C ambient, DS p.57) is 6.25 mm from the tab and
  5.25 mm from the zone. No figure bounds their temperature next to a tab at up to ≈ 108 °C; measured on the first boards.

First-board check: thermocouples on D15 and D4 cases, on C46 and on Y1's case at 0.5 ms / 120 fps for 30 min; if the LEDs
run above the derating curve, lower the LED current; Y1 must stay ≤ 80 °C, else lower the LED current or duty, or pull the
B.Cu zone's east edge back from x 151.0 ([led](led.md#first-board-checks)).

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
  - U2: through-hole from the bottom, soldered from the top.
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
flashing cable) latch and enumerate ([mcu](mcu.md#first-board-checks)); voids under U1's and U10's exposed pads and Q1's
tab, where the thermal vias are open ([Fabrication rules](#fabrication-rules)): X-ray if available, else check the
backside fillets in the vias and U1/U10/Q1 temperatures in the thermal runs.

## Accepted deviations

Every warning class in the released check pass (`kicad_check`: erc, style, footprints, fields, pulls, rules, drc,
decoupling_placement, layout clean; unrouted 0), with its reason. A new warning not in these lists is a regression.

### track_widths (58 warnings)

| Nets and widths | Where | Reason |
|---|---|---|
| +3V3 0.4 mm (F.Cu 98.4 mm, B.Cu 83.8 mm) | Ethernet branch after R38.1, north/west feed loops, the B.Cu feed to them | ≤ 0.15 A ([Power](#power), [Ethernet and PoE isolation](#ethernet-and-poe-isolation)) |
| +3V3 0.5 mm (B.Cu 9.1 mm) | R41/R42 branch feed from via (122.1, 112.9) | mA loads |
| +3V3 0.3 mm (F.Cu 6.1 mm, B.Cu 8.3 mm) | Short branches to pull-ups and U9's supply (among them R20's branch beside the VBUS via (112.5, 143.94)); the B.Cu feed beside the GND via (121.9, 153.075); the R41/R42 feed beside CAM_IO1's via; U1's branch at the PHY_REGOUT via | mA loads; < 17 mV drop at R53.1 for the whole feed ([Power](#power)) |
| +3V3 0.2 mm (F.Cu 12.0 mm, B.Cu 8.1 mm) | U12.7's feed, the link under Y1, U5.5 → C25.1 round U5.4 | µA–mA (IMU, R14 pull-up, U5's clamp supply) |
| +3V3 0.25 mm (0.63 mm) | R73.1 to its via | Pull-up, ≤ 0.7 mA |
| +3V3 0.15 mm (2.4 mm) | U1's +3V3 pin stubs (0.4 mm pitch) | Pin pitch |
| +5V 0.6 mm (F.Cu 0.6 mm, B.Cu 2.0 mm) | Exit east from the U8.1 pour; between the buck trunk vias (128.9, 147.9)/(129.7, 147.9) | Buck branch ≤ 0.52 A peak; the common 1.96 A node is poured ([Power](#power)) |
| +5V 0.8 mm (B.Cu 10.2 mm) | Via (140.3, 150.8) → R63/C44/U11 at (149.45, 145.1) | < 1 mA (U11 supply and the R63 divider) |
| +5V 0.5 mm (F.Cu 0.6 mm) | Link at the west end of U4's OUT pour | Short neck |
| +5V 0.4 mm (F.Cu 6.5 mm, B.Cu 1.9 mm) | U3 EN feed from via (129.7, 147.9) to U3.1; U11/R63 supply stub | Enable pin and op-amp supply, mA |
| +5V 0.3 mm (F.Cu 0.9 mm) | U4 pin necks | Necks inside U4's courtyard |
| +5V_POE_F 0.8 mm (B.Cu 12.2 mm) | Filter vias (106.6, 138.45)/(106.6, 139.45) → U4's IN1 vias | 1.0 mm does not pass the VBUS IN2 via; 11.4 °C at 1.96 A |
| +5V_POE_F 0.25 / 0.4 mm (F.Cu 0.9, B.Cu 2.7 mm) | Feed to R18 (PR1/OV divider) | µA |
| VBUS 0.25 mm (1.8 mm) | J2.A4/B9 stub to via (114.85, 105.9) | Only exit between J2's pegs and pads; ≈ 11 °C at 1.0 A, 1.8 mm long |
| VBUS 0.5 / 0.4 mm (F.Cu 3.2 / 2.9 mm) | J2.A4 → D2.1 and D2.1's flow-through exit; U4's IN2 pad entry | Short necks; the added 0.866 mm exit is no narrower than the existing 0.5 mm TVS approach |
| VBUS 0.3 mm (F.Cu 5.3 mm, B.Cu 7.7 mm) | Feeds to R24 and R27 (VBUS_SENSE divider) | µA |
| VBUS 0.8 mm (B.Cu 9.5 mm) | Source link from the J2 VBUS pour's via (110, 105.65) to via (114.85, 105.9) and protected return from D2's via to the 1.0 mm trunk | 1.0 mm does not fit; the return goes around the USB D− escape without bypassing D2.1 |
| +32V 0.6 mm (F.Cu 51.2 mm, B.Cu 5.6 mm) | C47 → C46 → ring | 0.915 A pulses at ≈ 6 % duty, ΔT ≈ 3.5 °C ([LED driver and ring](#led-driver-and-ring)) |
| +32V 0.45 mm (12.9 mm) | Corridor east of Q1 | Pad-spacing rules; ΔT ≈ 4.8 °C at 0.915 A DC |
| +32V 0.4 mm (21.9 mm) | D3.1 → C47 along the bottom edge, off the copper under U6 | Recharge ≤ 0.1 A average, ≈ 0.22 A peak; ΔT ≈ 0.3 °C |
| +32V 0.3 mm (20.9 mm) | Feeds to R65 (boost FB divider), R71 (VLED sense divider) and TP13 | µA |
| RING_1–RING_11 0.5 mm, LED_K 0.5 mm (1.2 mm) | Ring links and the LED_K neck | 1.0 mm breaks the 0.4 mm LED pad spacing; ΔT ≈ 4.2 °C at 0.915 A DC |
| ISNS 0.25 mm (8.8 mm) | The TP14 branch and the tap to NT1 | No load current; Q1.3 → R69.1 is the pour "ISNS pour 122" |
| USB_C_D_P/N 0.2 mm (F.Cu 12.1 mm, B.Cu 119.6 mm); USB_C_D pair coupled at 0.26–0.30 mm | Connector side J2 → U5 → R31/R32 | Full speed is electrically short; impedance not controlled ([MCU and USB](#mcu-and-usb)) |
| USB_D pair coupled at 0.257 mm (1.57 mm) | Into R31/R32 | Resistor pad pitch |
| CAM_D1 pair coupled at 0.285 / 0.415 mm (0.62 mm); CAM_CLK pair at 0.42 mm (0.59 mm) | CAM_D1's bump flat and via approach; CAM_CLK_P's F.Cu tail into its via | Skew-compensation and via-entry geometry, not coupled runs |
| ETH_TD pair at 0.22 / 0.29 mm (1.64 mm); ETH_RD pair at 0.22 / 0.29 / 0.428 mm (1.89 mm) | U1's MDI pin escapes; the P legs leaving the swap vias radially | 0.4 mm pin pitch; via entries |
| RMII_REF_CLK, RMII_REF_CLK_SRC 0.15 mm (1.1 / 2.1 mm) | U1.10's pin stub and where 0.2 mm does not fit | Pin pitch |
| IMU_INT1 0.15 mm (F.Cu 0.9 mm, B.Cu 3.1 mm) | From U12.5 to the via under U12 | Between 0.5 mm-pitch LGA pads |

### return_path (8 warnings)

| Track | Where | Reason |
|---|---|---|
| USB_C_D_P, 1.24, 1.13 and 1.15 mm on F.Cu | U5 link at (113.05, 110.5–114.0), split at the via tee | Crosses the In1 gap of the PoE lane (2 mm from the PoE copper); In2 is solid GND below. Full-speed USB, electrically short |
| ETH_TD_N 2.30 mm; ETH_RD_N 2.30 mm (x 160.77 / 159.26, y 120.4–122.8) | Beside the P/N swap vias | The swap vias' In1 antipads (r 0.775 mm) reach under the N track; the hop sits where IP101G §6.2 allows vias |
| ETH_TD_P 1.78 mm, ETH_RD_N 1.82 mm, ETH_TD_N 0.69 mm | TD_P passing over J1.2 into J1.1, RD_N passing under J1.4 into J1.5, TD_N leaving J1.2 | J1's MDI antipads in In1 (pad plus the 0.55 mm MDI spacing); a track leaving or passing a pin there crosses them |

The count fell from 13 when the 2026-11-03 cleanup merged collinear segments: the check reports per segment, and the
uncovered stretches are the same.

### via_return (3 warnings)

| Net | Vias | Reason |
|---|---|---|
| ETH_RD_P | (159.728, 121.165), (158.788, 122.105) | A GND via must keep ≥ 1.0 mm from an MDI via ("MDI spacing" 0.55 + 2 × 0.225 mm), and J1.13/J1.14 take the only spots at exactly 1.0 mm. MDI is short at 100 Mbit/s |
| ETH_TD_P | (160.298, 122.055), (161.238, 121.115) | Same |
| USB_C_D_N | (114.1, 109.0) | No site within 1 mm (D2, the VBUS link and the D+ track surround it); nearest GND via 2.66 mm, In2 solid below; full-speed edges ≥ 4 ns |

### Pad entries (32 findings on 19 pads and vias)

The pad-entry audit's remaining findings ([Routing style](#routing-style)). Each one needs a part or a neighbouring via
moved; placement is unchanged.

| Pad or via | Net | Finding | Why no compliant entry |
|---|---|---|---|
| R1.2 → R4.1 | PHY_LED0 | Ends off-centre; R1.2's exit at 45° | R1.2 is boxed in by R7.1 (0.41 mm), R1.1 (0.48 mm) and the PHY_LED3 track; only the corner diagonal fits |
| R7.1 | PHY_LED3 | End off-centre | R4.1 0.31 mm below, R7.2 and U1's pins / REF_CLK above |
| R10.1, vias (155.65, 130.75) and (153.9, 132.5) | COL/RMII (U1.4) | Ends off-centre, bends inside or close to the vias | U1.4's axis runs between XTAL_X2 (a via there lands 0.075 mm from it) and RMII_TX_EN, with MDC parallel on B.Cu; R10.1 is boxed in by C5.2, R10.2 and a GND via 0.3 mm below |
| U9.4 | LED_ON | End off-centre | A via on its axis lands 0.1 mm from R61.2 (GND) or 0.025 mm from U9.4 inside its mask opening; R61's 0.48 mm pad gap is too narrow |
| C49.1 | VLED_SENSE | Exit at 45°, end off-centre | Only an exit to the east fits; R71.2's link comes from the west |
| U6.16 (B.Cu) | LED_PWR_EN | End off-centre | A centre exit passes 0.19 mm from the VLED_SENSE via, which the USB_C_D_P B.Cu diagonal holds in place |
| Via (115.95, 131.95), B.Cu | STROBE | Bend inside the via, end off-centre | Its diagonal runs 0.41 mm from the parallel STATUS_LED and CAM_EN diagonals; a via there needs 0.525 mm |
| U11.1; MDIO via (145, 143.05) | SINK_DRV, MDIO | Ends off-centre; MDIO bends inside its via | The MDIO hop over the USB_C_D_P diagonal sits 0.48 mm east of U11.1; moving it brings MDIO within 0.1 mm of USB_C_D_N |
| U5.3, U5.4 | USB_C_D_N | Ends off-centre | A straight on-axis link passes 0.0025 mm from U5's GND via (114.6, 112.95) |
| D2.1, via (115.65, 109.2) | VBUS | Ends off-centre, bend inside the via | Moving the via brings its 0.8 mm B.Cu leg 0.14 mm from the GND via (116.6, 109.9) |
| J1.7 (In1) | POE_VA1 | End off-centre | A radial approach leaves the PoE lane rule area or comes within 1.6 mm of J1.5 (2 mm isolation) |
| J1.5 | ETH_RD_N | End off-centre | A compliant entry comes 1.73 mm from J1.7 (POE_VA1, 2 mm rule); raising the lane collides with RD_P |
| J2.B7 | USB_C_D_N | Last bend 0.03 mm outside the pad (margin 0.04 mm) | CC1 runs 0.2 mm above the crossover |

### Other accepted deviations

| Deviation | Accepted by | Section |
|---|---|---|
| No impedance control ordered; MIPI 88–114 Ω at the width extremes | User decision 2026-11-01 | [Impedance](#impedance) |
| Stock silk 0.085–0.131 mm from pads, JLCPCB clips it | User decision 2026-11-02 | [Fabrication rules](#fabrication-rules) |
| CSI In2 neck 6.19 mm vs 6.83 mm; 72.5 mil a side with the 45° crossing | User decision 2026-11-01 | [Camera and CSI-2](#camera-and-csi-2) |
| CAM pairs 54–89 mil from the plane edge at the FFC notch (90 mil asked) | User decision 2026-11-01 | [Camera and CSI-2](#camera-and-csi-2) |
| No GND guard round the MIPI CLK pairs | User decision 2026-11-01 | [Camera and CSI-2](#camera-and-csi-2) |
| Planes solid under the link and J4 pads except where the MIPI via antipads (r 0.585 mm) reach: up to 0.215 mm into the aligned south link pads R45–R47, R49, R50 (≈ 34 % area), part of R51's west edge (≈ 20 % area), ≈ 0.06 mm off J4's MIPI pads 2, 3, 5, 6; adjacent antipads join into slot voids along each via row | Design review 2026-11-01; geometry corrected 2026-11-02 | [Camera and CSI-2](#camera-and-csi-2) |
| MDI 3W waived inside "U1 MDI escape" | Design review 2026-11-01 (0.4 mm pitch) | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| Eight RMII/strobe layer changes without a GND via within 1.1 mm (seven with none within 1.6 mm); every XVS layer change now has a return within 0.67–0.90 mm | Design review; XVS routing corrected 2026-11-02 | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| No GND guard at the crystal | Design review (IP101G: "if possible") | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| Crystal loop: XTAL_X1 7.47 mm round Y1 against XTAL_X2 1.49 mm, ≈ 3.7 × 3.6 mm over In1 GND | User decision 2026-11-03 | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| Thermal vias open inside the exposed pads of U1 (4) and U10 (2) and Q1's tab (16): solder wicking and voids possible | User decision 2026-11-03 | [Fabrication rules](#fabrication-rules) |
| MDC, MDIO and the +3V3 feed to R14 cross under Y1 and its X1/X2 traces on B.Cu, behind the In1 and In2 GND planes; MDC runs ≈ 0.85 mm parallel to the XTAL_X2 diagonal, ≈ 0.14 mm away in plan. ETH_LED_G crosses the X1 link C2.1–Y1.1 once, at right angles | Design review 2026-11-02: no other B.Cu way west from U1's management pins (RMII fan-out, MDI, Q1 drain zone, board edge) | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| REGOUT path U1.28 → C6 4.59 mm of track with two vias, then the C6/C4 pour (8.51 mm with the via heights): pin 28 lies between the MDI pairs | Design review; REGOUT ripple checked on the first boards | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| F.Cu 1.0 mm (not 2 mm) from the In1 PoE copper; TP10.1 at exactly 1.000 mm | Design intent: some isolation only, no level rated or tested (user decision 2026-11-02) | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| Q1 drain copper ≈ 100–120 mm² instead of 1 in², Q1 1.7 mm from D15/D4 and 2.35 mm from Y1 | User decision 2026-11-01: measure first boards | [LED driver and ring](#led-driver-and-ring) |
| C15/C23–U4 and R74–U12 closer than JLCPCB's 1.0 mm QFN spacing | Design review | [Assembly](#assembly) |
| J1 lands 0.565–0.717 mm from reflowed SMD pads | First-board soldering trial | [Assembly](#assembly) |
| J2 shell 0.77 mm further in than HRO's recommended overhang | User decision 2026-11-01 | [MCU and USB](#mcu-and-usb) |
| Buttons on the bottom edge | User decision 2026-11-01 | [MCU and USB](#mcu-and-usb) |
