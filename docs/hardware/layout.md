# espion-cam board layout

How the board is built and why: outline, stack-up, fabrication and design rules, impedance, routing style, the
placement and routing intent of each block, assembly, and the check findings that are accepted on purpose. This page
holds the requirements, decisions, reasons and risks. It holds no coordinates, measured lengths, counts or rule values:
the KiCad files are the source for those.

| What | Where it lives |
|---|---|
| Parts, positions, tracks, vias, pours, rule areas | `hardware/espion-cam/espion-cam.kicad_pcb` (`kicad_query`) |
| Track widths, clearances, diff-pair geometry | the net classes in `espion-cam.kicad_pro` |
| Spacing and fab-limit rules | `espion-cam.kicad_dru` (`kicad_query rules`) |
| Board-setup minimums | the board setup in the `.kicad_pcb` |
| Measurable placement constraints | the `layout` section of `.omp/kicad.json` (`near`, `lengthGroups`, `paths`, `viaParity`, `copperArea`, `heightZones`), enforced by `kicad_check layout` |

Distances are copper edge to edge unless stated (pad edge to pad edge for parts), as `kicad_query distance` measures
them. Where a datasheet says "as close as possible", the `near` entry holds this project's heuristic limit for "close";
it is not a source value.

## Board and stack-up

### Outline and holes

| Item | Requirement | Reason |
|---|---|---|
| Outline | 64 × 64 mm, (100, 100)–(164, 164), corners r 1 mm | Smallest outline that fits the floorplan the user approved (user decisions 2026-10-31: 64 × 64 mm, J1 on the right, DP1435 on the left, J4 at an edge) |
| FFC notch | Top edge, 18 mm wide, 3 mm deep, centred on the lens axis, inner corners rounded | The camera FFC wraps round the top edge into J4 (user decision 2026-10-31: "J4 at an edge", no slot); a 16 mm cable has 1 mm each side |
| Lens cutout | Ø16 mm round cutout; lens axis at (129.7, 130.3) | The camera module sits behind the board (bottom side) with its lens on this axis; see [Camera and CSI-2](#camera-and-csi-2) |
| Mounting holes H1–H4 | M3, Ø3.2 mm plated, Ø6.4 mm GND pad, 3.5 mm in from both edges at the four corners; no via ring round the hole (the plated barrel joins the pads and planes) | User decision 2026-10-31: "4 × M3 corners"; via ring dropped for JLCDFM's via-to-PTH spacing (user decision 2026-11-10) |
| Tooling holes TOOL1–TOOL3 | Ø1.152 mm NPTH, 0.148 mm mask expansion, in three corners | Required for Economic PCBA; see [Assembly](#assembly) |
| Other NPTH | J1 and J2 locating pegs | Part datasheets |

M3 washer clearance: no copper other than GND within the ISO 7089 M3 washer radius (Ø7.0 mm, r 3.5 mm) plus 0.3 mm, so
r 3.8 mm, of a mounting hole's centre. Parts keep their courtyards outside it too. H1 is the tight one: the +3V3 loop
at J2's west shield pads runs closest to it, which is why that loop is routed where it is ([Power](#power)). H2–H4 were
first held to a larger r 4.6 mm radius clear of non-GND copper (an earlier design margin beyond the washer); H3 does not
keep that larger radius, and only r 3.8 mm is required.

The board carries "espion-cam" with its revision and `${ISSUE_DATE}` on the bottom silkscreen. The date text follows the
title block's date, which `kicad_edit` `set_board_setup {date}` sets to the release date before the fab output is made.
Below them, a "JLCJLCJLCJLC" text on the bottom silkscreen is where JLCPCB prints its order number (user decision
2026-11-09: placeholder rather than a paid removal or a spot of JLCPCB's choosing); the order sets "Mark on PCB" to
"Order number (specify position)".

### Stack-up

JLCPCB stack-up JLC04161H-7628, 4 layers, 1.6 mm, ENIG, green mask. Source: JLCPCB "Controlled Impedance PCB Layer
Stackup" (https://jlcpcb.com/impedance) and the impedance calculator (https://jlcpcb.com/pcb-impedance-calculator),
both read 2026-10-31. The dielectric and copper thicknesses are in the board's stack-up setup.

| Layer | Use on this board |
|---|---|
| F.Cu (L1, 1 oz) | Parts (all machine-placed parts), signals, power |
| In1.Cu (L2, 0.5 oz) | GND plane, except the PoE lane ([Ethernet and PoE isolation](#ethernet-and-poe-isolation)) |
| In2.Cu (L3, 0.5 oz) | Unbroken GND plane, no tracks |
| B.Cu (L4, 1 oz) | Bottom parts (U2, U6, J1), signals |

- Solder mask is LPI on both sides and covers every impedance-controlled track.
- JLCPCB's stack-up page labels the finished thickness below 1.6 mm; its capabilities page gives ± 10 %
  (read 2026-11-02). KiCad sums the layers a little above 1.6 mm.
- Why this stack-up: it is JLCPCB's default ("No requirement" on the order page), the only 4-layer 1.6 mm stack-up its
  calculator tags "Standard", it costs no stack-up fee, and Economic PCBA accepts "standard stack-up only"
  (https://jlcpcb.com/capabilities/pcb-assembly-capabilities, read 2026-10-31; JLCPCB does not publish which named
  stack-ups count as standard).
- The loss tangent in KiCad's stack-up is a placeholder; JLCPCB publishes none. It does not affect geometry.
- Via heights count in every length on this board (`use_height_for_length_calcs`).

## Fabrication rules

Board rules in the board setup and the custom rules, with the JLCPCB entry behind each. Source unless stated: JLCPCB PCB
capabilities, https://jlcpcb.com/capabilities/pcb-capabilities, read 2026-11-02 (4-layer, 1 oz outer, 0.5 oz inner; the
page gives no separate figures for 0.5 oz inner copper). The values are in the board setup or the named rule; read them
with `kicad_query rules` and `kicad_query stats`.

| Rule | Held by | JLCPCB entry | Notes |
|---|---|---|---|
| Minimum track width / clearance | Board-setup minimums | Multilayer 0.09 / 0.09 mm; pad to track 0.1 mm | Set slightly stricter than JLCPCB's multilayer minimum; the design's narrowest track and closest copper stay above it |
| Copper to board edge | Board-setup minimum | Routed edge 0.2 mm; PCBA FAQ part 2 (https://jlcpcb.com/help/article/pcb-assembly-faqs-part-2, read 2026-10-31): "edges of the traces and components must be more than 0.3mm away from the edge" (≈ 0.2 mm routing tolerance at depanelling) | Also the plane pull-back at the FFC notch and the lens cutout |
| Via size | Board-setup minimum | "0.2mm or 0.25mm hole size with via diameter less than 0.45mm, will cost more"; preferred minimum via hole 0.2 mm | Signal vias use the minimum; power and thermal vias are larger |
| Via annular ring | Board-setup minimum | "Via diameter should be 0.1mm(0.15mm preferred) larger than Via hole size" (0.05 / 0.075 mm ring) | The signal via's ring is above the minimum |
| Via types | Rule "Through vias only" | Microvias "only available for board thickness ≤1mm"; blind and buried vias are HDI only | |
| Hole to copper | Board-setup minimum; rule "PTH hole to copper" for through-hole pads | Via hole to track 0.2 mm; inner-layer PTH hole to copper 0.3 mm (PTH to track 0.28 mm minimum) | |
| Hole to hole | Board-setup minimum (DRC severity error); rule "Pad hole to hole" for pads | Via hole-to-hole 0.2 mm; pad hole-to-hole 0.45 mm | KiCad's hole-to-hole check skips milled (oval) holes, so J2's slots are not covered |
| PTH annular ring | Rule "PTH annular ring"; the board-setup minimum covers vias | ≥ 0.15 mm absolute (1 oz multilayer), 0.20 mm recommended | J2's shell slots sit exactly at the rule's limit (stock HRO land pattern), under JLCPCB's recommended ring |
| NPTH | Rules "NPTH hole size", "NPTH slot width" | Non-plated holes ≥ 0.50 mm; non-plated slots ≥ 1.0 mm | The board has no non-plated slot |
| Plated slot | Rule "Plated slot width" | "Multi-layer: 0.35mm" and "The minimum plated slot width is 0.5mm"; slot length at least 2 × its width | J2's shell slots are over twice as long as wide. The board setting `min_groove_width` is KiCad's creepage groove width, not a slot check |
| SMD pad to pad, different nets | The net classes; rule "U12 LGA pads" | 0.15 mm | |
| Minimum SMD pad | Rule "Minimum SMD pad"; user decision 2026-11-02: "Use 0.1mm×0.1mm for minimum pad size" | "Minimum SMD pad: 0.25mm × 0.25mm" (a note under SMD pad to pad clearance) | U1's and U4's QFN pads, the manufacturers' land patterns for 0.4 mm pitch, are under JLCPCB's note and kept; the same page makes 0.2–0.25 mm BGA pads with ENIG. Custom-shape pads (U1's corner anchors) are the smallest |
| Solder mask | Board setup (`solder_mask_min_width` and mask-to-copper) | "Pad size:soldermask opening can be 1:1"; keep 0.09 mm between openings and neighbouring traces; minimum pad spacing for a mask bridge 0.10 mm (green, 1 oz) | Openings 1:1 with pads; vias tented on both sides. KiCad flags and merges openings closer than the minimum web |
| Silkscreen | Rule "Silk line width"; text height and thickness (DRC severity error) | Line width ≥ 0.15 mm, text height ≥ 1.0 mm, width:height 1:6 preferred, pad to silkscreen 0.15 mm | All silk lines and texts are at the minimum; that stroke on 1.0 mm text is slightly thinner than the preferred 1:6. The board's defaults for new silk items (Board Setup › Text & Graphics) are still below the minimum, so set new items by hand |
| Silk clearance | Board-setup edge clearance; rules "Board silk to pads" and "Silk to silk" | Pad to silkscreen 0.15 mm; no silkscreen-to-silkscreen figure | See below |
| Tolerances | | Track width ± 20 %, board thickness ± 10 %, hole position ± 0.075 mm, plated holes +0.13 / −0.08 mm | Width tolerance drives [Impedance](#impedance) |

Vias next to pads: the quote page turns "tented" into "plugged" for free, but JLCPCB plugs a via with ink only if it
is ≥ 0.35 mm from any pad opening (https://jlcpcb.com/help/article/pcb-via-covering). Signal, power and decoupling vias
are never placed in pads. Where a via must sit closer than that, its drilled hole stays outside the pad opening by at
least the mask bridge (`solder_mask_min_width`); the vias that do this (C15.2, C37.2, C40.2/U11.2, U6.55 beside C47.2 and
C6.2) are GND vias on the same net as the pad. R72.2's GND via is the one exception: its hole is closer to the pad
opening than the mask bridge. Further out it would meet XVS's via or the VLED_SENSE track, and south the VLED_SENSE
diagonal blocks it (board review 2026-11-04).

A via beside a pad of its own net, but not touching it, keeps a copper gap of at least 0.1 mm to that pad (user
decision 2026-11-10). JLCDFM flags a narrower gap under pad spacing and via to pad: it may not etch, and the sliver
left between the pad opening and the tented via is too thin. Three vias don't keep it, because no site nearby holds
the clearances: C18.2's, held in by a USB_C_D_P B.Cu track and the copper round it; the GND via shared by U9.1 and
U6.27 (B.Cu), which the MIPI_CSI spacing to U6.28 and R56.1 (LED_EN) hold in place; and the C40.2/U11.2 via above.

Thermal vias are the exception: they sit inside the pads they cool, open (unfilled, unplugged) on the pad side and tented
on the far side. They are in U1's exposed pad (GND) and U10's exposed pad (GND; TPS61170 DRV0006A land pattern p.35 draws
them there). Solder paste over them can wick into the holes and leave voids under the pad; that is
accepted (user decision 2026-11-03: "Accept and document"): no windowpane paste, no filled and capped vias. Inspect those
two joints on the first boards ([Assembly](#assembly)). Q1's thermal vias sit in its drain pour beside the pad, not in it.

Silk against pads: KiCad 10's `min_silk_clearance` board setting covers silk to silk and silk to the board edge, and checks
silk against pads only for overlap on the mask layer; a custom `silk_clearance` rule covers more. Rule "Board silk to
pads" holds board-level silk (texts, marks) clear of pads. Footprint silk is exempt: the stock silk stays where it is
closer (0402 resistor and capacitor outlines to their own pads, test-point rings, J4's outline to its mounting pad);
JLCPCB clips legend at pads (user decision 2026-11-02: "Keep stock silk, JLC clips it") and at holes, so stock silk
over tented vias stays too (user decision 2026-11-10). Polarity marks inside
footprints are held ≥ 0.15 mm (JLCPCB pad to silkscreen) from pads by hand ([Assembly](#assembly)). Because JLCPCB gives no silk-to-silk figure, the rule
"Silk to silk" lets two silk items touch; without it, widening the stock silk to JLCPCB's minimum line width put
neighbouring 0402/SOT outlines closer than KiCad's silk clearance, and KiCad would flag every pair.

## Net classes

Track widths and diff-pair geometry come from the net classes in `espion-cam.kicad_pro`; the spacing that depends on what
is next to a net is in [Custom rules](#custom-rules). Where a width is sized for current, the temperature rise uses
Brooks & Adam's curve fit to the IPC-2152 data for an external layer, as coded in KiCad's source
(`track_width_calculations.cpp`, coefficients `EXTERNAL_COEFFS {215.3, 2.0, −1.15, −1.0}`,
https://docs.kicad.org/doxygen/track__width_calculations_8cpp_source.html, read 2026-11-01): ΔT = 215.3 · I² · W^−1.15 ·
Th^−1, W and Th in mil, ΔT in °C. (The released KiCad 9 track-width calculator uses IPC-2221 instead, which gives lower
rises; the fit is the more conservative of the two.)

| Class | Nets | Reason |
|---|---|---|
| Default | Every signal net not listed below | Robust, yet narrow enough to enter 0.4–0.5 mm pitch pins after a short neck |
| PWR_3V3 | +3V3, CAM_3V3 | Espressif ESP32-P4 Hardware Design Guidelines (HDG) §1.4.1: module supply "at least 25 mil"; temperature rise at the +3V3 load current by the fit above |
| PWR_5V | +5V, VBUS, +5V_POE, +5V_POE_F, +5V_BST | Sized for the DP1435's 2.6 A maximum output, which the fit above keeps within a reasonable rise ([power](power.md#5v-loads)) |
| LED_PWR | +32V, LED_K, ISNS, RING_1–RING_11 | Worst-case LED pulses ([led](led.md#current-sink)); the boost output is high enough that exposed pads need extra spacing (rule "LED_PWR pad spacing") |
| POE_CABLE | POE_VA1, POE_VA2, POE_VB1, POE_VB2 | Centre-tap current 350 mA (HanRun HR871150C p.1); routed on In1 only (0.5 oz), so the width is chosen for that copper ([PoE isolation](#poe-isolation)); extra spacing from rule "PoE isolation" |
| MIPI_CSI | CSI_* and CAM_* pairs | 100 Ω differential ([Impedance](#impedance)); spacing from rules "MIPI_CSI spacing" and "MIPI_CSI to clocks and high-speed" |
| MDI | ETH_TD_P/N, ETH_RD_P/N | 100 Ω differential; spacing from rules "MDI spacing" and "MDI pair to pair" |
| USB | USB_C_D_P/N (connector side), USB_D_P/N (module side) | 90 Ω differential (HDG §1.4.3) |

Tracks narrower than their class are allowed only where a reason is recorded; every such case is in
[Accepted deviations](#accepted-deviations).

## Custom rules

The `.kicad_dru` rules (`kicad_query rules`; the clearances, skews and sizes are in the file), with the purpose and
source of each. The JLCPCB fabrication rules are described in [Fabrication rules](#fabrication-rules).

| Rule | Applies to | Purpose | Reason and source |
|---|---|---|---|
| MIPI_CSI spacing | MIPI_CSI copper against everything except its own partner; the two pads of each 0 Ω link R45–R47, R49–R51 exempt | Clearance of 2W | HDG, PCB layout, MIPI: "maintain a minimum spacing of 2W between MIPI trace pairs". Keeps the GND pours 2W away too, which the 100 Ω geometry assumes. The link pads carry the two halves of one lane |
| MDI spacing | MDI copper against non-MDI copper | Clearance of 3W | IP101G datasheet §6.2 p.56 "three times space". Exempt: MDI items touching U1's courtyard or lying wholly inside the F.Cu rule area "U1 MDI escape", where U1's 0.4 mm pitch puts pins 25 (ISET), 28 (REGOUT) and 31 (AVDD33) next to the MDI pads ([Ethernet and PoE isolation](#ethernet-and-poe-isolation)). A segment crossing the area edge is not exempt |
| PoE isolation | POE_CABLE copper against other nets, same layer | Same-layer gap | Keeps the PoE cable side (J1's centre taps to U2.1–U2.4) apart from the rest of the board. The layout is meant to give some isolation only; no isolation level is rated or tested (user decision 2026-11-02). The gap is a chosen value. Pad pairs inside J1 exempt: that pin field is the jack's own (HanRun HR871150C p.1 rates it 2250 V dc, cable side to chip side). Copper across layers is not checked by KiCad: see [PoE isolation](#poe-isolation) |
| MIPI_CSI intra-pair skew | Each MIPI_CSI pair | Maximum P/N skew | HDG §1.4.6 allows 10 mil P/N skew per lane; a lane is two pairs in series (U6 → 0 Ω link → J4), so each gets half |
| U12 LGA pads | Pad pairs inside U12 | Clearance between neighbouring pads | MSA3S02 land pattern keeps JLCPCB's SMD pad-to-pad minimum between different-net pads (corner lands shortened at their inner end for it); JLCPCB's mask bridge sets the lower bound |
| LED_PWR pad spacing | An exposed pad of LED_PWR or BST_SW against any other net (not POE_CABLE or MDI, which have stricter rules) | Pad clearance for the boost voltage | IPC-2221A Table 6-1, 31–50 V row: A6 (external component terminations, uncoated) for exposed pads; B4 (solder mask) 0.13 mm, so mask-covered tracks keep the class clearance. Exempt: pad pairs inside U10 (TI land pattern) and inside each LED D4–D15 (one forward voltage apart) |
| MIPI_CSI to clocks and high-speed | MIPI_CSI against USB, MDI, every /Ethernet/RMII_* and /MCU/RMII_* net, XVS/XVS_CAM and STROBE/STROBE_CAM | Clearance of 3W | Espressif ESP32-P4 HDG, MIPI PCB layout: 3W from other high-speed and fast signals; includes both sides of the sync/strobe series links and all PHY/MAC RMII nets, with no exemptions |
| PTH hole to copper | Through-hole pads | Hole clearance | JLCPCB inner-layer PTH hole to copper 0.3 mm (PTH to track 0.28 mm minimum) |
| Pad hole to hole | Pad against pad | Hole to hole | JLCPCB pad hole-to-hole spacing 0.45 mm (vias: 0.2 mm, the board minimum) |
| Power neck-down at fine-pitch pads | PWR_5V, LED_PWR and +5V_BST tracks touching the courtyard of U4, U8 or U10 | Minimum track width | A class-width track cannot enter U4's fine-pitch pads (0.3 mm gaps, TI TPS2121 land pattern p.39), U8's SOT-23-6 (0.95 mm pitch) or U10's WSON pads (TPS61170 DRV0006A, p.35) at the class clearance; the neck is short enough that its resistance and heat are negligible ([Power](#power)) |
| LED_PWR pad spacing (other pads) | An exposed pad of any other net against LED_PWR or BST_SW copper (tracks and vias included) | Pad clearance, mirrored | The mirror of "LED_PWR pad spacing", which only fires when the exposed pad is the LED_PWR item; same IPC-2221A source. Exempt: U10's own pads against BST_SW copper inside U10's courtyard (pin 4's exit keeps TI's DRV0006A land gaps, 0.35 mm to pin 5 and 0.25 mm to the exposed pad) and ISNS_K against ISNS (the Kelvin tap through net tie NT1, same node, small drop) |
| Silk to silk | Two non-pad items on the same silk layer | Silk clearance of zero | JLCPCB gives no silkscreen-to-silkscreen spacing; see [Fabrication rules](#fabrication-rules) |
| MDI pair to pair | RD pair (ETH_RD_P/N) against TD pair (ETH_TD_P/N); same U1 exemption as "MDI spacing" (either item touching U1's courtyard or wholly inside "U1 MDI escape") | Clearance of 3W | IP101G §6.2 p.56 "three times space" also between the RX and TX pairs. The closest spot is the RD_P/TD_N diagonals west of R6, right at the limit (`kicad_query distance`); DRC passes |
| MDI intra-pair skew | Each MDI pair | Maximum P/N skew | TI SNLA387 p.2 "MDI Traces": length-match the two lines of a pair within 50 mil for 100BASE-TX |
| PTH annular ring | Plated through-hole pads | Annular width | JLCPCB absolute minimum for 1 oz multilayer (a larger ring is recommended) |
| Plated slot width | Plated pads with an oval hole | Hole size | JLCPCB "The minimum plated slot width is 0.5mm" (also "Multi-layer: 0.35mm") |
| NPTH hole size | Round non-plated holes | Hole size | JLCPCB "Min. Non-plated holes 0.50mm" |
| NPTH slot width | Non-plated oval holes | Hole size | JLCPCB "Min. Non-Plated Slots 1.0mm". A guard for future parts: the board has no non-plated slot, so nothing exercises it yet. Its condition joins the two tested ones of "NPTH hole size" (non-plated) and "Plated slot width" (oval hole) |
| Through vias only | Every via | Micro, blind and buried vias disallowed | JLCPCB: microvias only on boards ≤ 1 mm, blind/buried vias HDI only |
| Silk line width | Graphic items on F.SilkS / B.SilkS | Line width assertion | JLCPCB "Minimum Line Width ≥0.15mm"; KiCad has no board setting for it |
| Board silk to pads | Board-level silk (not in a footprint) against pads | Silk clearance | JLCPCB pad to silkscreen; footprint silk exempt by user decision ([Fabrication rules](#fabrication-rules)) |
| 0402 pour pads thermal | C17.1, R17.2, R21.2 | Zone connection: thermal reliefs | Each is an 0402 with one pad inside a solid-connected local pour and the other on a thin track: a relief lets both pads heat alike in reflow (tombstoning; board review 2026-11-04). The other 0402s of the review are joined without a pour instead ([Ground and return](#ground-and-return)) |
| Minimum SMD pad | SMD pads | Size X and Y assertion | User decision 2026-11-02, below JLCPCB's note ([Fabrication rules](#fabrication-rules)). Custom-shape pads are checked on their anchor, which is smaller than the whole pad |

## Impedance

Method: JLCPCB's impedance calculator (https://jlcpcb.com/pcb-impedance-calculator, read 2026-10-31) for stack-up
JLC04161H-7628, cross-checked with an independent 2-D quasi-static field solver (trapezoidal 35 µm track with etch-back,
JLCPCB's conformal mask). Every pair runs on an outer layer over the adjacent GND plane: F.Cu over In1, B.Cu over In2
(both the same distance from their plane, so both give the same geometry). The width and gap in each net class come from
this method.

| Use | Net class | Target |
|---|---|---|
| MIPI_CSI, MDI | MIPI_CSI, MDI | 100 Ω ± 10 % (HDG, MIPI) |
| USB | USB | 90 Ω ± 10 % (HDG §1.4.3) |
| Single-ended reference | none | 50 Ω; not used: RMII is short and runs at the Default width |

- The geometry holds only with solder mask over the pairs and a solid GND plane on the adjacent layer;
  `return_path` checks the plane under every MIPI_CSI, MDI and USB track.
- Propagation delay per mm (v) for the timing budgets: see [ethernet](ethernet.md#timing).
- Where it matters: MIPI CSI-2 at its full lane rate ([camera](camera.md#csi-2-link)). USB full speed (12 Mbit/s, edges
  ≥ 4 ns) and 100BASE-TX MDI are electrically short on this board.
- No impedance control is ordered (user decision 2026-11-01: "No impedance control"). JLCPCB's ± 20 % width tolerance
  puts the MIPI pairs outside the HDG's ± 10 % at the width extremes (field solve at the extremes of width and gap); a
  ± 10 % width spread would stay inside it. Controlled impedance (± 10 %, about USD 33 more) was offered and declined;
  whether Economic PCBA accepts it on this stack-up is not known.
- First-board check: run the CSI link at full rate (2 lanes, 800 Mbit/s per lane) and count frame errors before the design is released
  ([camera](camera.md#csi-2-link)).

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
Reaching a pad centre from a lane a few hundredths of a millimetre off can leave a short jog (two opposite 45° bends with
a diagonal shorter than the track width); the rules allow it.

Joints and stubs:

- every track end lies on a pad, a via, a pour of its net or exactly on another track's centreline (overlapping joints
  are rebuilt on the centreline; a 45° T keeps its angle);
- no track piece lies wholly inside one pad or via of its net with an end that joins nothing; the pours already join
  those pads and vias;
- collinear segments of one width are merged, except the U1.27 escape, whose split keeps the lower segment wholly inside
  "U1 MDI escape" as the "MDI spacing" exemption needs.

Hooks and detours (user instruction 2026-11-02, with example images of CC2 looping round J2's shield pad and XVS_CAM
hooking into TP8): a track runs from its pad or via toward where it goes, never leaving the other way and looping back,
and a via that only serves one stub sits on its pad's axis (or radial ray) so the stub is one straight segment. The
loops that stay are forced, each with its reason: U5.5 → C25.1 (round U5.4), C16.2's GND via (+5V, VBUS and +5V_POE_F
round it), the +5V EN feed into U3.1 (round C18.1),
+32V from C46.1 (MDC), LED_K_SENSE and VLED_SENSE (round C48.2 / R72.2), PHY_RST_N (round R8.2's former pad: kept as
routed when R8 was removed, user decision 2026-11-11), RMII_REF_CLK on B.Cu
(+3V3), and the length-matched diff pairs.

The MDI pin escapes at U1 are 45°-only ([Ethernet and PoE isolation](#ethernet-and-poe-isolation)); the CSI bus crosses
its narrowest plane neck at 45° rather than the ideal 58° ([Camera and CSI-2](#camera-and-csi-2)).

## Power

Parts: U2 SDAPO DP1435-5V PoE PD module (bottom, west edge, hand-soldered), L1/C13/C14 filter on +5V_POE_F, U4 TI
TPS2121 power mux, U3 TI TLV62569 3.3 V buck with L2, U8 TI TPS22918 LED-supply switch. Circuit: [power](power.md).

### Placement constraints

TPS2121 (SLVSEA3F §11 p.33 "as close as possible", §12.2 layout example): `near` entries

| Pair | Reason |
|---|---|
| C15.1–U4.7 | IN1 bypass |
| C16.1–U4.2 | IN2 bypass |
| C20.1–U4.1 | OUT bypass |
| R22.1–U4.10 | R_ILM beside ILM, as the layout example draws it |

U4's periphery: OUT pins 1 and 8 tied straight across; C20 on the west OUT flare, C16 west, C15 east, C21 (SS) and
R22 straight north of U4.11/U4.10, the OV/PR1 divider row (R20, R21, R18, R75, R19) south with a GND via between R21 and
R18.

C22 is `+5V` bulk, not a U4 bypass, and sits south of L1, well away from U4. There is no top-side
site for it closer in: L1 and the C13/C14 filter fill the west and north of U4, the OV/PR1 row and the boost stage the
south, the LED ring the east, and `kicad_query board_free_space` finds no free area for its courtyard on the top side.
Its feed is a F.Cu track at the PWR_5V width from the pour "C20/U4 +5V": west and down a 45° diagonal, under L1's
body between its pads, west of L1.1, then into C22.1.

The west column runs C14, C13, L1 and C22 from north to south. U2.4's through-hole pad holds C14's courtyard at the
top, and C14 is turned 180° so its polarity mark stays on the board, which puts its `+5V_POE_F` pad on the east side;
the filter's `+5V_POE` and `+5V_POE_F` links are tracks at the PWR_5V width.

TLV62569 buck loop (SLVSDG1C §10.1 p.13, Fig. 20; U3 is turned 180° against Fig. 20): `near` entries. Each `max` is the
placed distance rounded up, so the loop shape cannot drift.

| Pair | Reason |
|---|---|
| C10.1–U3.4 | Input capacitor's VIN end at VIN |
| C10.2–U3.5 | C10's GND end away from SW, beside the FB row where the GND copper and vias are |
| C18.2–U3.2 | Output capacitor's GND end at the GND pin (C2 in Fig. 20) |
| C18.1–L2.2 | Output capacitor's +3V3 end toward the inductor output |
| L2.1–U3.3 | Short SW node (two entries: the loop limit and the heuristic "close" limit) |
| R16.2–U3.5, R17.1–U3.5, C17.2–U3.5 | FB divider and feed-forward capacitor at FB |

C10 (the input capacitor) sits at VIN, in the role of the one input capacitor of TI's Fig. 20. The small extra input and
output capacitors C12 and C19 were removed (user decision 2026-11-11, board simplification).

### Vias at the mux

`copperArea` entries, counted on F.Cu near U4's pads (TPS2121 §12.1: vias under the input and output pins;
§12.2 p.33 draws two vias per IN pin and four on OUT; land pattern p.39 "VIA TYP" in pads 1, 2, 7, 8; one via per
ampere of input and output current):

| Entry | Net | Note |
|---|---|---|
| U4 IN1 vias | +5V_POE_F | |
| U4 IN2 vias | VBUS | |
| U4 OUT vias | +5V | Larger vias do not fit beside C15.2 or U4.9 at the class clearance |

+5V_POE_F leaves the L1/C13/C14 filter for the B.Cu trunk through two power vias.

### Track widths

Widths come from the net classes; this lists where a run is not at its class width, or is a pour, and why. Temperature
rises use the fit in [Net classes](#net-classes); the currents are in [power](power.md#5v-loads).

| Run | Width | Reason |
|---|---|---|
| +5V and +5V_POE trunks, +5V_POE_F on F.Cu | PWR_5V class | Common +5V trunk carries the boost-limited and board-peak current; the DP1435 maximum bounds the class |
| +5V_POE_F B.Cu leg, from the filter vias to U4's IN1 vias | Narrower than the class | The class width does not pass the VBUS IN2 via at U4 ([Accepted deviations](#accepted-deviations)) |
| +5V into U3: the B.Cu trunk ends at a via; the F.Cu pour "C10/U3 +5V" joins it, the via of the U11/R63 branch, C10.1 and U3.4 | Pour | Buck input peak |
| +5V B.Cu branch to R63/C44/U11 | Narrow | Only U11's supply and the R63 divider |
| +5V common node at U8.1: the trunk vias, U8.1, C34.1 and TP5.1 in the F.Cu pour "+5V pour 100"; the B.Cu pour "+5V pour 105" brings the common trunk to the via pair | Pours | Boost and buck together, the highest +5V current |
| From the U8.1 pour east to the buck trunk vias (pours "+5V pour 101"/"+5V pour 104" round them) and the B.Cu trunk toward U3.4 | Class width; narrower between the east via pair and leaving the U8.1 pour | Buck branch peak only |
| +3V3 trunks | PWR_3V3 class | |
| Necks inside the U4, U8 and U10 courtyards | The minimum of rule "Power neck-down at fine-pitch pads" | Short; their resistance and heat are negligible |
| Branches carrying no load current (enable pins, dividers, FB sense, op-amp supply, pull-up feeds, short stubs into pins) | Narrow | µA–mA |

The +3V3 feed to the north and west parts (U5, C25, R53 → CAM_3V3/J4, R41/R42, D1/C24, R20) cannot take a shorter
route: the USB pair, the camera bundle, XVS and +5V_POE_F fence it on B.Cu, the PoE lane and U2 on F.Cu. It is fed from
the U3/U6 area by a B.Cu feed (narrowed beside a GND via; R20's pull-up branch off its west end also narrow), a loop along
the west edge and a F.Cu loop round J2's north side, narrow because it carries mA loads only (camera, D1, pull-ups).
Past H1 the loop drops east of J2's north-west shield pad and runs SW between the two west shield pads, then round the
south-west shield pad's top in short 45° steps, outside H1's washer clearance, with CC1 parallel on its south-east side.
There, between H1's washer clearance and that pad, both run as close as their clearances allow and the loop is narrowed
(the tightest gap on the board's top edge). The R41/R42 branch's B.Cu feed from the +3V3 via is wider
except beside CAM_IO1's former via (kept as routed when CAM_IO1 was removed, user decision 2026-11-11). The west-edge loop crosses the filter column on B.Cu from a via north of C14 down to
below C13 (C14's 10 × 10 can leaves no F.Cu way past it), under U2's magnetics ([B.Cu under U2](#bcu-under-u2)). The
drop to R53.1 follows from the network resistance (`kicad_query copper`) and those loads.

### Ground and return

- In2 is one unbroken GND plane; In1 is GND everywhere except the PoE lane. Both pours: clearance raised by the custom
  rules near MIPI, MDI and PoE copper, thermal reliefs, islands removed.
- Local pours (on F.Cu and B.Cu, above the planes in priority) join power pins of one net that sit close together and
  carry the high-current nodes: BST_SW (U10.4, L3.2, D3.2), LED_K (Q1.3 with its thermal vias, D15.1) and ISNS (Q1.2, R69.1;
  NT1 keeps its Kelvin tap from R69.1, in a lobe of the pour that carries no load current). Each pour: clearance 0 (the
  class and custom rules still set the gap; KiCad fills to the larger), solid pad connection (no thermal reliefs),
  islands removed; names "<parts> <net>" or "<net> pour N". The In1/In2 planes keep their thermal reliefs.
- 0402 parts get no solid pour pad beside a thin-track pad (tombstoning): rule "0402 pour pads thermal" gives C17.1, R17.2
  and R21.2 reliefs; C21.2 and R22.2 join their shared GND via by two narrow tracks and C52.2 its via by a narrow track
  (no pour); R16.1 takes +3V3 from the trunk ending on it and a narrow neck from "C17/R16 +3V3", which stops clear of the
  pad.
- Where a rule is waived for parts' own pads, the pours are clipped to what the pads keep: "BST_SW pour 120" stays at the
  "LED_PWR pad spacing" distance off U10's other pads (pin 4 itself keeps TI's land gaps, 0.35 mm to pin 5 and 0.25 mm to the exposed pad), "U1 GND" keeps the exposed-pad
  distance off U1's MDI pins and "MDI spacing" off the MDI tracks. F.Cu pours keep the 1.0 mm sideways distance from the In1
  PoE copper ([PoE isolation](#poe-isolation), Across layers); "LED_K pour 121" stays clear of the crystal copper, so its edge
  stays the closest F.Cu LED_K copper to C2.1.
- Every decoupling capacitor's GND pad has a GND via within 1 mm, never in the pad: on its own short stub or through a
  GND pour that holds pad and via. The buck capacitors C10 and C18 and U3.2 share the F.Cu pour
  "C18/U3 GND" with their GND vias.
- U4.12 has its own GND via on a short F.Cu stub.
- `decoupling_placement` (decoupling capacitor ≤ 3 mm from its power pin, GND via ≤ 1 mm from its GND pad) is clean.

### B.Cu under U2

U2 is a bottom-side module; its flyback magnetics sit over a B.Cu box with no plane between them and the B.Cu tracks.
XVS's west run is kept out of it: B.Cu just west of the box, across on F.Cu behind the In1/In2 planes
([Ethernet and PoE isolation](#rmii)). What stays in the box on B.Cu, because the camera module area, the lens cutout
and the CSI bus leave no other way from J2 and J4 to U6: VBUS (the PWR_5V trunk), USB_C_D_P/N, CAM_EN, SCCB_SDA, a short XVS
hop, STATUS_LED, STROBE and a stub of SCCB_SCL (user decision 2026-11-04: "Partial + document"). Added with the BOM
change of 2026-11-09 (user decision 2026-11-09: "Accept"): the +3V3 west-edge loop, a decoupled DC rail, runs on
B.Cu down the filter column just east of XVS's west run. C14's larger can closed its F.Cu way, F.Cu is full of the C14
and C13 pads beside it, and west of it XVS's run and the board edge leave no B.Cu lane. First-board check
([power](power.md#first-board-checks) 9): at the PoE input's peak load (LEDs 0.5 ms / 120 fps, camera streaming,
Ethernet up), scope XVS and SCCB_SCL/SDA edges at J4 for ringing or extra edges, and run USB enumeration and a bulk
transfer; repeat on USB power alone as the reference.

## MCU and USB

Parts: U6 Waveshare ESP32-P4-Core module on the bottom, hand-soldered with the hand-solder footprint variant
([mcu](mcu.md#module)); J2 HRO TYPE-C-31-M-12 USB-C at the top edge; U5 SRV05-4 and D2 H7VHD3UA ESD protection at J2; SW1
(RESET) and SW2 (BOOT) on the bottom edge, locked like the other edge-bound parts (the enclosure reaches them there).
Circuit: [mcu](mcu.md).

### Placement constraints

`near` entries:

| Pair | Reason |
|---|---|
| C29.1–U6.75 | HDG §1.4.1: capacitor at the power entry (ESP_3V3) |
| R33.2–U6.1, C27.1–U6.1 | HDG §1.3.3: RC at CHIP_PU (ESP_EN), short trace |
| R31.2–U6.38, R32.2–U6.37 | HDG §1.3.12 / §1.4.3: USB series resistors at the chip end (D+ / D−) |
| C48.1–U6.19 | HDG §1.3.10: filter capacitor at an ADC pin (LED_K_SENSE). Smaller than the HDG's 0.1 µF, so the reading settles within an LED pulse ([led](led.md#led_k-sense)) |
| C49.1–U6.17 | HDG §1.3.10: 0.1 µF at an ADC pin (VLED_SENSE, GPIO21 / ADC1_CH5) |
| U5–J2.A6 | TI SLVA680A p.4: protection as near to the connector as design rules allow, short I/O tracks. The limit was raised when J2 moved out to its overhang: U5 cannot follow it north, D2's courtyard abuts it (user decision 2026-11-04: "Raise the limit") |
| D2.1–J2.A4, D2.1–J2.A9 | TI SLVA680A p.4: "place the TVS as near to the connector as design rules allow"; the limit reaches both VBUS pad pairs |
| `paths` "BOOT stub": R36.1 → R39.1 | The BOOT strap R39 hangs on the 50 MHz RMII_TXD1_MCU line; its stub stays short, negligible against the edge |

C29 sits over U6.74–76, beside the +3V3 via pair that feeds pins 74–76 on B.Cu; its ground end joins U6.73's GND via on
F.Cu. BST_EN crosses the +3V3 trunk on B.Cu north of U6.76's pad and hops to F.Cu between two vias to pass the +3V3 B.Cu
feed.

GPIO assignments that moved to make the routing possible are listed in
[firmware rules](firmware-rules.md#gpio-changes-in-this-revision).

### USB

- J2 is locked and machine-placed at the top edge with its shell overhanging the edge by HRO's recommended amount: the
  board edge on the drawing's PCB EDGE line, 5.79 mm from the pegs (HRO drawing p.1), so a plug overmold seats fully
  (user decision 2026-11-04: "HRO recommended", "Keep machine-placed, add an order note"). That puts J2 closer to the
  routed edge than JLCPCB's ≥ 0.3 mm for components (PCBA FAQ part 2); the order carries a note ([Assembly](#assembly)).
  The footprint's silk stops short of that edge line (`hardware/lib/README.md`).
- Connector side (USB_C_D_P/N, J2 → U5 → R31/R32): narrow tracks, mostly uncoupled. Full speed is electrically short here
  (edge ≥ 4 ns), so width, coupling and skew don't matter; impedance is not controlled.
- Both lines flow through U5 (TI SLVA680A p.4: no stub between the TVS and the protected line). D+: J2.A6 → U5.6 on
  F.Cu (no via), a link under the package to U5.1 (SRV05-4 pins 1 and 6 are separate channels, R+O datasheet p.1; this
  link joins them), then a via off U5.1. D−: the J2 run comes up at a via just beside U5.4's pad; a F.Cu link runs under
  the package to U5.3 (pins 3 and 4 are separate channels too; this link joins them), and the run to R32 leaves U5.3
  through a via. Each of those vias has a GND return via beside it.
- Module side (USB_D_P/N, R31/R32 → U6.38/U6.37): short, one via each, sharing a GND return via (HDG §1.4.3: return vias at
  USB via transitions).
- USB vias near U5 stay the PoE-isolation gap away from the In1 PoE copper.
- USB_C_D_N's J2 branch leaves U5's pocket on B.Cu through a via; no GND via fits within 1 mm of it (D2, the VBUS link
  and the D+ track surround it), In2 is solid GND under it. USB_C_D_P's link to U5 crosses the In1 gap of the PoE lane,
  with In2 GND below. Both are accepted for full-speed edges ([Accepted deviations](#accepted-deviations)).
- VBUS: J2.A9/B4 (one pad, west) and a power via share the F.Cu pour "J2 VBUS"; a B.Cu link reaches a second via, and a
  narrow F.Cu stub runs from it into the centre of J2.A4/B9 (one pad, east). From there a F.Cu run goes south into D2.1's
  centre. The protected feed leaves inside D2.1's east side, bends 45°, then reaches a power via. A B.Cu return goes
  north, north-west, west and south-west; only there does the PWR_5V trunk start toward U4's IN2 via. Thus neither J2 VBUS
  contact bypasses D2.1's junction (TI SLVA680A p.4: flow-through TVS routing, no protected-line stub); D2.2's short GND
  via is unchanged. The narrow connector stub is the only exit between J2's pegs and pads ([Accepted deviations](#accepted-deviations)).
- U5.5 (clamp supply) reaches C25.1 on F.Cu; U5.2 has a GND via between the pin rows.
- CC1 and CC2 (Rd R25/R23): CC2 runs from R23.1 east under J2's pad row and 45° up into B5's centre. CC1 leaves A5
  north-west into the corridor between J2's west shield pads, then steps SW round the south-west shield pad, parallel to
  the +3V3 loop on its south-east side ([Power](#power)), down into R25.1.
  R23's GND pad joins J2's west shield pad through the pour "GND pour 96".

### Buttons and boot

- SW1 (RESET) and SW2 (BOOT) stay on the bottom edge (user decision 2026-11-01: "Keep on the bottom edge"); the enclosure
  must give access to both there. Labels "BOOT" and "RST" sit between them.
- BOOT_SW leaves SW2's west pin-1 pad westward on F.Cu directly to a via; SW2's two pin-1 pads keep their F.Cu link. The
  button has no clamp (user decision 2026-11-09: D16 removed, [mcu](mcu.md#reset-and-boot)). On B.Cu the net runs
  south-east, south, then east along the bottom edge below U6's bottom pad row, tented, with copper clear of the edge.

### Module pads

- U6's GND pads 20, 27, 34 and 55 have their own GND vias. U6.55's via touches C47.2's pad edge (same net; the REF_CLK
  F.Cu leaves no other spot), U6.27's via sits against U9.1 (same net).
- Top-side reflowed parts lie above U6's pads (C47, C50, U12, R37 among them). U6 is soldered with an iron only, pad by
  pad, no hot air (user decision 2026-11-01: "Iron only, pad by pad"), so they are not reflowed again.

## Camera and CSI-2

Parts: J4 JUSHUO AFA07-S15FCA-00 (bottom-contact 15-way 1.0 mm FFC connector) at the notch, rotated 180°; the six 0 Ω links
R45–R47, R49–R51; the camera module behind the board on the lens axis. Circuit and cable: [camera](camera.md); cable
orientation: [camera](camera.md#ffc-orientation).

### Keep-outs

Rule areas in the board file (`kicad_query` lists them with their regions):

| Rule area | Layers | Keeps out | Reason |
|---|---|---|---|
| Camera module | B.Cu | Footprints | Module 25 ± 0.2 × 23.86 ± 0.2 mm around the lens axis, FFC edge toward the notch (module drawing WX-1M-9281-79). U2's body face sits close to the module's west edge (it fits at the module tolerance); J1's body starts beyond its east edge |
| FFC lane | B.Cu | Footprints | The cable's path from the notch to the module edge (16 mm cable; width assumed, not on the drawing) |
| FFC notch | F.Cu, B.Cu | Tracks, vias, pours | Cable wrap at the edge |
| FFC notch inner | In1, In2 | Tracks, vias | Same; the In1/In2 GND pours run to the board-edge clearance (user decision 2026-11-01: "Pull planes to 0.3 mm") |
| Lens cutout | All copper | Tracks, vias, pads, pours, footprints | The Ø16 cutout plus margin; the planes end at the board-edge clearance from the cutout |
| CSI link vias south | F.Cu | Footprints | Keeps the CSI via field free |
| CSI link vias north | F.Cu | Footprints | Keeps the CAM via-2 corner free |

### Lens view cone

`heightZones` entry "lens view cone", centred on the lens axis. A top-side part h mm tall must keep its courtyard at
least r0 + slope · h from the lens axis, so it stays out of the lens's field of view:

- slope = tan(79.3° / 2) (lens view angle, module drawing);
- r0 = lens rim 2.5 mm + slope × 5.4 mm: the lens tip may sit up to 5.4 mm below the top face (user decision 2026-10-31:
  "Tip ≤ 5.4 mm below top face"; the module's front face is ≥ 2.1 mm behind the board because its FFC connector, 2.1 mm
  high, is on the lens face);
- bottom-side parts (U2, U6, J1) do not count;
- heights are the current parts' datasheet maxima, in the entry's `heights` map; a new part type needs a height there.

Every top part, the 12 ring LEDs included, passes; `layout` is clean.

### Lane order and links

The ESP32-P4 cannot remap CSI lanes or swap P/N (ESP32-P4 TRM v0.7 Register 42.2 p.2611, §42.5.3.2 p.2603), and the
camera's lane order at J4 differs from U6's:

- U6 pads west → east: D0_N, D0_P, CLK_P, CLK_N, D1_N, D1_P.
- J4 west → east: CLK_P (pin 9), CLK_N (8), D1_P (6), D1_N (5), D0_P (3), D0_N (2).

Each lane is two pairs in series: CAM_* from J4 to a 0 Ω link, CSI_* from the link to U6. The six links stand in U6
order in one row, west → east R51 (D0_N), R47 (D0_P), R45 (CLK_P), R49 (CLK_N), R50 (D1_N), R46 (D1_P). The reordering is
done on B.Cu between J4 and the links, without crossings:

- D1 runs north of J4's pads, east under J4 and back south into its links; the U-turn mirrors the pair, so P (west at J4)
  arrives east, as R50/R46 need.
- CLK runs on F.Cu north under J4 to its vias; on B.Cu N wraps west of the P via to swap sides, then the pair runs east
  and south into its links.
- D0 goes south of J4's pads as a B.Cu serpentine (west and back); the U-turn mirrors the pair, so D0_N (east at J4)
  arrives west, as R51/R47 need.

### Via fields and return vias

Via counts are equal within each pair (`viaParity` entries "CSI lanes, P4 side" and "CAM lanes, FPC side", TI SPRAAR7J
§3.9 p.12), counted per side of the 0 Ω links:

| Segment | Vias per leg | Where |
|---|---|---|
| CAM (J4 → links) | The same on every leg | One under J4's body north of its pad, on the pad axis; one north of the link row. The lane-order crossing happens on this side |
| CSI (links → U6) | The same on every leg | One south of the link row (D0_N's via is fed west from R51.1, because the PoE lane leaves no legal spot for its return via at the link's own axis) |

Every signal via has a GND through via within 1.0 mm centre to centre (`via_return` limit; the D0/D1 via-1s sit on J4's
pad axes; TI SPRAAR7J §3.9 p.12: a return via at each transition, p.8: stitching vias "within 200 mils" of the signal
vias). Neighbouring legs share one where the in-row spots are taken. The GND vias under J4 have F.Cu stubs to J4's GND
pads 10/7/4/1.

### CSI bus and the plane neck

The CSI legs run on B.Cu over In2 to U6.28–U6.33 as a three-pair bus, CLK between D0 and D1. Spacing: 2W to everything but
the partner, 3W to USB, MDI, all RMII, XVS/XVS_CAM and STROBE/STROBE_CAM (rules "MIPI_CSI spacing" and "MIPI_CSI to
clocks and high-speed"). The CLK pairs have no GND guard copper (HDG §1.4.6 p.28 asks for the MIPI CLK pair to be
surrounded by ground copper); a B.Cu guard each side would widen the bus and leave less room at the neck below (user
decision 2026-11-01: "Accept 2W, no guard").

The bus passes through a neck of the In2 reference between the lens plane edge and the antipad of J1.9. The neck is
narrower than TI SPRAAR7J p.9's 90 mil from bus to plane edge on each side asks for. Widening it would mean moving J1
(locked at the edge) or the lens and ring (approved floorplan). The bus crosses the neck at 45° (route rule) rather than
the ideal 58°, which costs a little more plane-edge distance on each side (user decision 2026-11-01: "Use 45°, accept
71.7 mil"; the ideal crossing gave more room).

At the FFC notch the In1/In2 planes end at the board-edge clearance. The CAM pairs' B.Cu run under J4 keeps less than
SPRAAR7J p.9's 90 mil from that plane edge; the D1 pair is the one materially short (CAM_D1_P over a few millimetres of
its run under J4), the CLK pair is close to the limit. Meeting it needs a new crossing plan under J4. Accepted with the
user decision above.

The J1 antipads in In2 are voids, not plane edges, and SPRAAR7J p.9 asks for ≥ 1.5 W from voids. CSI_D1_P's via antipad
joins J1.3's ETH_CT void on In1 and In2 (the web between them is under the pour's minimum width, so it is not filled): the
leg leaves its via inside that void. Further on it passes J1.5's, J1.7's and J1.9's antipads at increasing distance.

The planes stay solid under the wide pads on the lanes except where the MIPI via antipads reach under them, against TI
SPRAAR7J §3.10 p.13 ("voiding the SMD mounting pads of the reference plane by 100%"):

- Link pads: the return vias sit on their pads' axes, so their antipads void part of the south pads of R45, R46, R47, R49
  and R50 (a circular antipad over a rectangular pad, not a uniform strip). R51's west edge is voided separately by the
  D0_N via. The north pads are fully backed.
- J4's MIPI pads 2, 3, 5, 6, 8, 9: the via-1 antipads take a sliver off pads 2, 3, 5 and 6.
- Adjacent antipads join into one slot void along each via row.

The excess capacitance of a voided pad is estimated with the parallel-plate approximation C = ε₀ · Dk · backed area /
dielectric height (Dk and height of the 7628 prepreg in the stack-up), as the part of the pad that is still backed. The
effect is a reflection of a few percent at a 150 ps edge (rectangular-pad/circular-antipad estimate, not measured);
accepted at design review 2026-11-01, covered by the first-board CSI test.

### Lengths and skew

Tracks and gap come from net class MIPI_CSI. Skew compensation uses trapezoid bumps (45° legs, a flat ≥ 0.45 mm, ≥ 0.45 mm
of straight track between same-sense bends, no copper overlap), placed next to the mismatch, so the pad-to-pad copper
path equals the net length. Lengths count via heights; pad to pad equals net length on every MIPI net. Rule "MIPI_CSI
intra-pair skew" holds each pair; read the lengths with `kicad_query lengths`.

CAM_CLK_P takes up its length difference on F.Cu between J4 and its via by a detour north of CAM_CLK_N, so the pair runs
uncoupled there instead of coupled with bumps (user decision 2026-11-04: "Accept + record"; re-coupling needs trapezoid
bumps drawn in pcbnew; [Accepted deviations](#other-accepted-deviations)).

`lengthGroups` "CSI pairs, P4 side" and "CAM pairs, FPC side": the pair-mean spread of each segment stays within half the
HDG §1.4.6 pair-to-pair limit (30 mil end to end; a lane is two segments in series, so half each).

### Other parts near the link field

C3 (ETH_CT) ties its GND end to a GND via by a short F.Cu track; ETH_CT runs on F.Cu from C3.1 to J1.3. J4 is marked "1"
and "CONTACTS DOWN" on the top silk.

First-board check: full-rate CSI frame-error test ([Impedance](#impedance)); FFC fit, bend and insertion depth
([camera](camera.md#ffc-orientation)).

## Ethernet and PoE isolation

Parts: U1 IP101G PHY with its MDI pins north; J1 HanRun HR871150C MagJack on the bottom, rotated −90°, locked,
hand-soldered; Y1 25 MHz crystal; U2 PoE PD module. Circuit: [ethernet](ethernet.md), [power](power.md#poe-input).

### MDI

- U1 sits next to J1 so MDI is short (IP101G §6.2 p.56: keep vias out of MDI except "in close proximity to device"). MDI
  runs on F.Cu only, with no vias, from U1 up the east strip and diagonally between J1's pegs and LED pins to J1 pins 1,
  2, 4, 5.
- The two pairs are swapped at J1 so they reach it without crossing, and each pair is P/N-inverted on purpose so it runs
  straight (U1.30/29 → J1.5/4, U1.27/26 → J1.2/1; net names follow the J1 pin: U1.30 ETH_RD_N, U1.29 ETH_RD_P, U1.27
  ETH_TD_N, U1.26 ETH_TD_P). This relies on the PHY's auto-MDIX, which firmware must never disable, and on its
  polarity handling (IP101G p.27, register 18 bit 8 POLARITY; user decision 2026-11-04)
  ([ethernet](ethernet.md#magjack-and-mdi), [firmware rules](firmware-rules.md#rules)). The inversion needs no swap vias
  and no skew bumps on the N lines. Each pair jogs east once, at 45° as a coupled pair, onto the lanes of the diagonals
  toward J1. The U1.27 escape (ETH_TD_N) keeps its split so its lower segment stays wholly inside "U1 MDI escape".
- Spacing: MDI keeps 3W to other nets (rule "MDI spacing") and RD keeps 3W from TD edge to edge (rule "MDI pair to
  pair"; the closest spot is the RD_P/TD_N diagonals west of R6, right at the limit, which DRC passes). At U1's north
  row the 0.4 mm pitch makes 3W impossible: pins 25 (ISET), 28 (REGOUT) and 31 (AVDD33, +3V3) sit next to the MDI pads and
  their tracks leave between or beside the pairs. Each MDI pin escapes a short way north inside U1's courtyard, then
  45°-only inside the F.Cu rule area "U1 MDI escape", where the rule "MDI spacing" is waived. Inside it only U1's own pin
  nets come closer than 3W: the pads (land pattern), the REGOUT track and its via, ISET's vertical, +3V3, the PHY_RST_N via.
  Every MDI point outside the area meets the rule (DRC; closest are J1.14, R46.2 and J1.13, then the GND via by TD_P's
  jog). This is a deviation from IP101G §6.2's "three times space", which has no exception.
- Lengths (`kicad_query lengths`, U1 pad → J1 pad, no vias; pad to pad equals the net length): each pair is matched
  within TI SNLA387 p.2's 50 mil for 100 Mbit/s, which the rule "MDI intra-pair skew" enforces. J1.1 and J1.5 sit further
  west than J1.2 and J1.4, so TD_P and RD_N have the longer way; TD_N and RD_P carry 45° trapezoid bumps next to the
  mismatch at J1.
- Return path: In1 GND lies under every MDI track except where the tails at J1.2/J1.4 cross J1's own MDI antipads;
  accepted ([Accepted deviations](#accepted-deviations)).

### RMII

| Constraint | Entry | Reason |
|---|---|---|
| Series resistors at the driving pin | `near` R35.1–U6.46, R36.1–U6.47, R37.1–U6.67 (TXD0, TXD1, TX_EN at the MAC); R11.1–U1.18, R12.1–U1.17, R15.1–U1.19 (RXD0, RXD1, CRS_DV at the PHY); R13.1–U1.14 (REF_CLK series resistor, HDG §1.3.14, at the PHY's 50M_CLKO) | The limit is well below any RMII edge (≥ 0.5 ns), so the resistor works as a lumped source termination |
| Long data segments | `lengthGroups` "RMII data, long segments": RXD0/RXD1/CRS_DV after the PHY resistors, TXD0/TXD1/TX_EN after the MAC resistors | The ESP32-P4 publishes no RMII output delay (IP101G p.61 and RMII 1.2 §7.4 give the rest), so the board takes a budget of 5 % of the 20 ns period (a heuristic) for the MAC clock branch plus one TX line: (L_MAC + L_TX) × v ≤ budget, with L_MAC at its `paths` limit, which fixes the per-line limit; no spread limit is needed. IP101G §6.1 "equal" is met within this tolerance. Derivation: [ethernet](ethernet.md#timing) |
| REF_CLK star | `paths` "REF_CLK MAC branch" from R13.2 to U6.68 | U1.10 sits near the source end: its edge waits on a ledge until the reflection from U6.68 returns, 2 × v × L_MAC after it arrives, so the limit is on the MAC branch measured from the star. No ledge while that round trip, the module's internal trace included, is no longer than the fastest RMII edge (1 ns, RMII 1.2 §7.4.3): L_MAC ≤ 1 ns / (2 v), minus the length reserved for the unpublished trace inside the module. The PHY/MAC difference is only a note against IP101G §6.1 "equal" (no tolerance given); RX/TX timing alone allows a large branch difference ([ethernet](ethernet.md#ref_clk-branches)) |

- The RX resistors R15/R11/R12 stand vertical in a row east of U1 with their vias south of them; U1's RX pin order is the
  reverse of U6's, so the B.Cu runs pass north of the vias to their west and arrive in U6 order without a crossing.
- REF_CLK runs ≥ 3W (3 × the Default width) from the RMII data lines wherever their tracks run side by side
  (IP101G §6.1 p.56: "Avoid signals path parallel to clock signals path"). The +3V3 trunk runs beside it for a stretch.
  Closer spots that are not parallel runs: the REF_CLK via beside the CRS_DV B.Cu track and a TXD0 via; R13.2 beside R12.2
  (RXD1) in the resistor row; +3V3 copper at a via; U1.10 beside U1.9 (TXD0) at the 0.4 mm pin pitch. REF_CLK_SRC and
  U1.10's pin stub run narrower where the Default width does not fit.
- GND return vias sit close to most RMII, REF_CLK, XVS and STROBE layer changes (TI SNLA387 p.4: return vias close to
  signal vias). A few layer changes have none close by and no legal spot for one, found by a grid search against every
  track, via, pad, rule area and edge: REF_CLK (two), TXD0, TXD1 and TX_EN in U1's south-west fan-out, TX_EN_MCU, and two
  STROBE vias (the second has a GND via only beyond the search distance). These are 50 MHz RMII and slow strobe lines without an impedance class; the distant returns remain accepted.
  `via_return` does not check them (Default class). Every XVS layer change has a GND via close by.
- XVS stays outside the actual L1/L2/L3 courtyard polygons on every layer, including its pads and via annular rings,
  so the boost hot loop is avoided (the +32V branch runs along the bottom edge, clear of U6.9, see
  [+32V and the ring](#32v-and-the-ring)). Check against the library courtyard polygons, not their bounding boxes: L2's
  courtyard is stepped. The route: U6.9 leaves on B.Cu, changes layer between C34's pads, runs north between the
  inductors and the other boost parts (F.Cu and B.Cu hops), crosses west along F.Cu behind the In1/In2 planes, then goes
  north on B.Cu just west of U2's magnetics outline ([B.Cu under U2](#bcu-under-u2)) and rejoins the north run to J4.
  To open this corridor without moving parts, the B.Cu STATUS_LED/CAM_EN/STROBE/SCCB_SCL/SCCB_SDA fan-outs run further
  east, and PWR_SRC's B.Cu leg makes room for XVS's return via.
- U1's exposed pad has GND vias on a grid; U1.1 (GND corner pin) has its own via.
- Crystal Y1: no GND guard between it and the LED driver copper (IP101G §6.1: "Use guard traces to protect clock traces if
  possible"); there is no room for one (a guard needs the gap to hold three track widths). It relies on distance and the
  In1/In2 planes: C2.1 (XTAL_X1) stays apart from the filtered +32V track, Y1.1 from +32V, the switched LED_K pour from
  C2.1, and the B.Cu LED_K zone sits behind In1 and In2.
- Crystal loop: XTAL_X1 runs U1.2 → Y1.1 round Y1's north side, XTAL_X2 (U1.3 → Y1.3) is short; the two traces enclose a
  small area directly over In1 GND. Y1's pins 1 and 3 sit on opposite corners of the 3225 package, so turning Y1 only
  moves the long leg to X2. Low risk at 25 MHz over a solid plane; accepted (user decision 2026-11-03: "Accept and
  document").
- Tracks under the crystal circuit (Y1, C2, C5, XTAL_X1/X2 and U1.2/U1.3), all on B.Cu with the In1 and In2 GND planes
  between them and the crystal (IP101G §6.1 p.56: "Avoid signals path parallel to clock signals path … such as MDC
  and X1"):
  - MDC runs west under U1 under U1.2's (X1) pad and its X1 stub, crosses the XTAL_X1 diagonal at 45°, then runs
    diagonally to a via: it crosses the XTAL_X2 diagonal at right angles and the X2 vertical at 45°, and passes under the
    corners of the Y1.3 and C5.1 pads. Apart from U1.2's pad and stub it runs parallel to no crystal trace.
  - MDIO runs under the XTAL_X1 vertical and Y1.3's north edge, then diagonally, branching to R14.2 at a via.
  - The +3V3 feed to R14 runs diagonally under Y1's body.
  - ETH_LED_G crosses only the X1 link C2.1–Y1.1, at right angles, close to C2.1 and Y1.1 in plan.
  - The COL/RMII strap (U1.4) runs on B.Cu between its two vias, parallel to MDC's diagonal and under C5.1, crossing the
    X2 vertical at 45° inside C5.1's pad.

  MDC and MDIO cannot leave the area. U1's management pins (22, 23) are on its east side and U6.43/U6.70 lie to the
  south-west. On B.Cu the only way west from under U1 lies between the crystal and the tops of the RMII fan-out (COL via,
  TX_EN, TXD1, TXD0, REF_CLK): the strip below them runs south, the MAC-bound RMII diagonals close the way round it, and
  REF_CLK keeps 3W. East lie ETH_LED_Y's B.Cu run and the board edge; north the MDI and J1's pin field; west the Q1 drain
  zone. F.Cu round U1 holds the pins, the crystal traces and the resistor rows. MDC (≤ 3.33 MHz) and MDIO are slow, and
  two GND planes separate them from the crystal: accepted ([Accepted deviations](#accepted-deviations)).
- JLCPCB recommends 1.0 mm from a QFN to any part ("Minimum Spacing Requirements for SMD Components",
  https://jlcpcb.com/help/article/minimum-spacing-for-smd-components, read 2026-10-31). At U1 every part keeps it
  (`kicad_query distance`; closest are Y1.3–U1.1, C9.1–U1.13 and C7.1–U1.32).
- The Ethernet +3V3 loads (U1, pull-ups, LED anodes) take the PWR_3V3 width to R38.1's former pad (kept as routed when
  R38 was removed, user decision 2026-11-11) and a narrower track after it; the
  narrowest widths only where nothing wider fits.
- ETH_LED_G leaves J1.14 on B.Cu north of the crystal, comes down between C2 and Y1 to a via and runs on F.Cu west of the
  crystal towards R4; ETH_LED_Y runs east on B.Cu with one via. J1's shield pads (J1.SH) are GND: they join the In1/In2
  GND planes directly (user decision 2026-11-09: no shield RC network, [ethernet](ethernet.md#j1)).

### PHY-side decoupling

`near` entries:

| Pair | Reason |
|---|---|
| C9.1–U1.13 | IP101G §6.1 p.56: VDDIO decoupling |
| C6.1–U1.28 | IP101G §6.1 and pin 28 p.18: REGOUT 0.1 µF |
| C4–C6 | IP101G pin 28 p.18: REGOUT 10 µF beside the 0.1 µF |
| C7.1–U1.31 | IP101G §6.1 p.56: AVDD33 decoupling |

The `near` entries measure straight copper edge to edge, not the copper path. REGOUT's path is longer: from U1.28 F.Cu
north to a via, B.Cu to a second via, then the F.Cu pour "C6/C4 PHY_REGOUT" that holds C6.1 and C4.1, because pin 28 lies
between the two MDI pairs and its track can leave only northwards inside "U1 MDI escape". A first-board check scopes
REGOUT at C6 ([ethernet](ethernet.md#first-board-checks)); accepted ([Accepted deviations](#accepted-deviations)).

### PoE isolation

The four PoE cable nets (POE_VA1, POE_VA2, POE_VB1, POE_VB2: from J1's centre taps to U2's input pins 1–4) carry the
cable side. The layout is meant to give some isolation between them and the rest of the board, with the geometry below
(rule "PoE isolation" on the same layer, the In1 lane, a keep-off sideways on F.Cu); no isolation level is rated or
tested (user decision 2026-11-02).

Layer plan:

- The PoE tracks run on In1 only, in one lane (net class POE_CABLE, four tracks side by side). In1 is GND elsewhere; In1
  tracks are allowed only inside the lane (rule area "In1 GND plane, PoE lane excepted"); In2 is solid GND with no tracks
  (rule area "In2 GND plane").
- Heating and drop: each net carries up to a pair's 350 mA (HanRun HR871150C p.1). In1 is 0.5 oz, so the track is a small
  cross-section A; IPC-2221's fit for internal conductors (I = 0.024 · ΔT^0.44 · A^0.725, A in mil²) gives the rise;
  it ignores the GND copper of In1 and In2 round the lane, so the real rise is lower. Resistance follows from ρ
  1.72 × 10⁻⁸ Ω·m, the copper thickness, the width and the net length (`kicad_query lengths`).
- The lane keeps every bend at 45°, and a width wide enough for the four tracks and their gaps everywhere: from U2's end
  west, a 45° step to the middle band under the ring top, a 45° staircase to the east band, and 45° walls into the J1 end.
- In1 GND keeps the rule distance from the PoE copper (rule "PoE isolation", DRC clean); PoE copper to other nets' via
  annular rings on In1 and to GND vias is also checked against it.
- Every other net crosses the lane on B.Cu over In2, with vias at least the rule distance from the lane copper. +32V, +5V
  and +3V3 never run on In1 or In2.
- The "PoE U2 pins" rule area (all copper layers) keeps vias and pours away from U2's cable-side pins.

Across layers (F.Cu over In1), which KiCad does not check: no F.Cu copper of another net lies over the In1 PoE copper or
within 1.0 mm of it sideways. The closest are the F.Cu pours "R41/R42 +3V3", "GND pour 92" (U5.2) and "R53/C32 CAM_3V3"
(outlines clipped to keep 1.0 mm; TP10.1, which sat exactly at the limit, was removed, user decision 2026-11-11), then
U5.1–3, D6.1, D7.2, D5.1, R41.1, R43.2, C32.2, C3.2 and the F.Cu tracks RING_2 and a GND
track beside the lane. One 7628 ply lies between F.Cu and In1; a 2 mm sideways keep-off does not fit.

J1: the rule exempts pad pairs inside J1, the jack's own pin field (HanRun rates it 2250 V dc, cable side to chip side,
HR871150C p.1). J1's pads are trimmed in the footprint to widen the gap between a chip-side pin and the nearest
cable-side pin and between the shield and pin 10.

U2: the DP1435's input is isolated from its output (1.5 kVrms, DS p.1, p.5 item 9). Its cable-side pins U2.1–U2.4 have no
capacitor to board GND: the datasheet's typical application (p.4) wires them straight from the jack, and a capacitor to
GND would bridge the module's isolation. That is why the decoupling check exempts U2.1–U2.4.

The top silk marks "POE 57V" beside J1 and U2.

## IMU

Parts: U12 MEMSensing MSA3S02 (LGA-12, 2 × 2 mm, 0.5 mm pitch) on the top side above U6, C50 (VDD decoupling), R73/R74 I²C
pull-ups. Circuit: [imu](imu.md).

- Land pattern: the eight corner lands are slightly shorter at their inner end so every gap between pads of different
  nets inside U12 meets JLCPCB's SMD pad-to-pad minimum; rule "U12 LGA pads".
- IMU_INT1 leaves pin 5 north to a tented via under U12's body centre on a narrow track (clear of every pad) and runs on
  B.Cu to U6.69. No other vias lie under the LGA pads; the datasheet gives no via or trace keep-out under the package. GND
  pin 9 and NC pin 8 (to GND, as the datasheet recommends) run to a GND via; NC pin 4 joins C50.2 on F.Cu.
- `near` C50.1–U12.7 (MSA3S02 DS V1.0 p.30 Fig. 8: 100 nF at VDD). C50.2 has no GND via of its own (U6's bottom pads below
  leave no site); the F.Cu pour
  "C50/U12 GND" joins it and NC pin U12.4 to a GND via, which satisfies `decoupling_placement`. Its loop to U12's GND pin 9
  is C50.2 → pour → GND via → In1 → second GND via → pour "U12 GND" → U12.9 (pours as straight lines; U12.4 is an NC
  pin, not a ground return). Harmless for a 110 µA sensor (DS p.7).
- I²C away from the buck switch node (TI SLVSDG1C §10.1 p.13: "Keep these traces away from SW nodes"): IMU_SCL and
  IMU_SDA run on B.Cu under L2 (with In1 and In2 GND between them and L2's switch pad) and come up to F.Cu only east of it.
  SDA crosses the corner of L2's courtyard (it must pass U12.1 to reach U12.2).
- JLCPCB's 1.0 mm from a QFN/LGA to other parts holds for C50, R37 and R73. It does not hold for R74 (R74.2–U12.2). R74
  keeps its place because a vertical R74 hits the C50 GND via or L2's courtyard. Accepted.
- U12's courtyard touches L2's (L2 FXL0630, 3.0 mm tall) and is close to C47's (11.0 mm tall): possible reflow
  shadowing; no source gives a number ([imu](imu.md#accepted-risks)).

## LED driver and ring

Parts: 12 Amicc A-SC880IR3C IR LEDs D4–D15 in a ring round the lens axis (30° pitch); U10 TI TPS61170 boost with L3, D3,
C43, C45; C46/C47 bulk capacitors on +32V; Q1 HL2310A sink MOSFET (SOT-23); U11 TLV9061 current loop with R69 (sense resistor) and
net tie NT1; U9 SN74LVC1G97 STROBE gate. Circuit: [led](led.md).

### Placement constraints

`near` entries:

| Pair | Reason |
|---|---|
| C36.1–U10.6, C36.2–U10.3 | TPS61170 SLVS789D §11.1: the input capacitor "needs not only to be close to the VIN pin, but also to the GND pin" |
| C43.1–D3.1, C45.1–D3.1 | §11.1: switch–diode–output capacitor path as short as possible |
| L3.2–U10.4, D3.2–U10.4 | §11.1 p.23: "minimize the length and area of all traces connected to the SW pin"; the limit is the placed distance rounded up |
| R65.2–U10.1, R66.1–U10.1 | Fig. 23: minimise the FB node |
| R62.1–U10.2, C41–U10.2, C39.1–U10.2 | Fig. 23: minimise the COMP node |
| C42–U11.4, R64.1–U11.4 | TLV9061 §8.4.1: feedback parts at the inverting input |
| C37.1–U9.5 | SN74LVC1G97 SCES416N §10: bypass capacitor at VCC |

### Boost stage

- U10's exposed pad has GND vias (`copperArea` "U10 exposed pad vias", counted inside the pad; TPS61170 DRV0006A land
  pattern p.35, §11.3).
- C43.2, C36.2 and the exposed pad (U10.3, U10.7) share the F.Cu GND pour "C36/C43/U10 GND" with GND vias, so both
  capacitor loops close on the top layer as in TI Fig. 23. C45.2 keeps its own via: D3.2 (BST_SW) lies between it and
  C43.2.
- The switch node U10.4 → L3.2/D3.2 is the F.Cu pour "BST_SW pour 120", kept off U10's other pads by the "LED_PWR pad
  spacing" distance, so pin 4 keeps TI's land gaps (0.35 mm to pin 5, 0.25 mm to the exposed pad) and the node stays small (§11.1).
- +5V_BST: U10.6, C36.1, L3.1 and U8.6 share the F.Cu pour "U10/C36/L3 +5V_BST". The small bypass C38 that sat between C36
  and U10 (board review 2026-11-04) was removed (user decision 2026-11-11, board simplification).

### +32V and the ring

- +32V goes from the boost output (D3's cathode) to C47.1, on to C46.1, then north through a corridor east of Q1 to D4.2
  (`kicad_query lengths` paths, via heights counted, pours as straight lines). D3.1, C43.1, C45.1 and TP13.1 share the
  F.Cu pour "D3/C45/C43 +32V"; C46.1 ends in the pour "+32V pour 107" over D4.2's two pads. The R65/R71/TP13 feeds are
  narrow.
- No +32V copper lies under U6's body (user decision 2026-11-03: "Reroute +32V out of U6"; whether the module's
  underside has exposed copper is not known, [mcu](mcu.md#accepted-risks)). The boost-to-C47 leg runs on F.Cu only: from D3.1
  south, east under SW1's body between its pads, down a 45° diagonal between SW1.2's east pad and C29.1 (both kept at
  the "LED_PWR pad spacing (other pads)" distance), then east along the bottom edge into the run to C47.1. It is narrow
  because it carries only the boost's recharge current (low average; peak = ILIM × VIN / VOUT, with ILIM the TPS61170
  switch current limit (SLVS789D, electrical characteristics table), VIN and VOUT the input and set-point voltages).
- The pulse current comes from C46: its path to D4.2 is short and wide enough for the worst-case LED pulse current ([led](led.md#current-sink)).
  East of Q1 it runs narrower than the class, held off the LED_K pour and from C2/C8 by the pad-spacing rules; the pulses run at
  a low duty.
- The ring links are narrower than the class (a class-width link breaks the "LED_PWR pad spacing" to each LED's other
  pad). Each link leaves and enters its LED pads along the pad's own axis (the pads sit at 15° + k · 30° round the
  ring), then turns onto 45° legs; these axis stubs and their 15°–60° bends are the one exception to the 45° rules
  ([Routing style](#routing-style)). Each LED's two pads of one node are joined centre to centre. LED_K: Q1.3 and D15.1
  share the F.Cu pour "LED_K pour 121".
- Exposed pads of LED_PWR/BST_SW keep their spacing to other nets, and other exposed pads keep it to LED_PWR/BST_SW copper
  (the two "LED_PWR pad spacing" rules). R70 (the `LED_K_SENSE` divider's upper resistor) sits where the NTC TH1 was,
  rotated 180°: R70.1 (`LED_K`) joins Q1's drain pour by a short narrow stub and keeps the pad-spacing distance to the +32V
  trunk; R70.2 (`LED_K_SENSE`) keeps it to "LED_K pour 121". R76 (GND leg of the divider) took R70's old place beside C48
  at U6.19, rotated 270°, R76.2 on a straight stub on the pad axis to a GND via.
- PHY_RST_N and LED_K_SENSE route round the lens inside the LED ring, much of PHY_RST_N close to the ring pads and links.
  Both tolerate that: PHY_RST_N is static, driven push-pull by GPIO22 once the firmware runs and held only by the PHY's
  internal pull-up before that, while the LEDs are off ([ethernet](ethernet.md#reset)); LED_K_SENSE is a high-impedance node with C48 at the
  ADC pin, read late in each pulse after the ring's edges have settled ([led](led.md#led_k-sense)).

### Current sense

- ISNS: Q1.2 and R69.1 share the F.Cu pour "ISNS pour 122"; NT1 takes the Kelvin tap from R69.1; TP14 hangs off a narrow
  branch.
- R69.2 (the LED current return) sits in the F.Cu pour "GND pour 80" with C44.2 and its GND vias; a short F.Cu link reaches
  the GND via shared with the USB module-side return. No via fits at the pad (U6's bottom pads 34–38 lie under R69). The
  return resistance is below that of the earlier three-link arrangement, which only lowers the LED current.
- C46.2 has several GND vias with their holes outside the pad opening.

### Thermal

Q1's dissipation at the 0.5 ms / 120 fps limit, with the headroom loop running and in the full-scale start-up burst, is
worked out in [led](led.md#thermal). The headroom loop is Q1's design basis (user decision 2026-11-09); full scale is not
an operating state. HL2310A datasheet p.1: RθJA 104 °C/W (the test board is not stated), TJ max 150 °C; p.4 Fig. 9: the
transient impedance for the pulse peaks.

- Drain copper (`copperArea` "Q1 drain copper", net LED_K, F.Cu + B.Cu near Q1, with a minimum area and a minimum via
  count): Q1's drain pad (Q1.3) on the F.Cu pour "LED_K pour 121", which also holds D15.1 and the thermal vias beside the
  drain pad, and the B.Cu zone "Q1 drain B.Cu" (solid pad connection). The SOT-23's drain lead carries the heat into
  that copper. That is all the copper that fits round the pulse loop.
- The zone stays clear of the crystal (Y1/C5), the RMII B.Cu strip, the CSI bus and U6's courtyard, and never reaches the
  ring side.
- Estimate at 30 °C ambient: TJ peak = T_ambient + P_pulse × Zθ(t_pulse, duty) × RθJA, with RθJA as the datasheet
  gives it; inside TJ 150 °C with the loop running, at about TJ max at full scale ([led](led.md#q1-dissipation)).
- Q1's drain copper is close to D15 and D4 (and further from C46 and C47). The LED current is set for only a little LED
  heating above ambient, which drain copper near its full-scale temperature would exceed; C46/C47 are rated to 105 °C.
  Accepted pending measurement (user decision 2026-11-01: "Measure first boards").
- The Ethernet crystal Y1 sits close to "LED_K pour 121" and to the B.Cu zone "Q1 drain B.Cu" (on the other side of the
  board); Y1 is rated −40…+85 °C, and its ±20 ppm holds only over that range (Epson TSX-3225 p.1). U1 (IP101GRI,
  industrial grade, 85 °C ambient, DS p.57) is further away. No figure bounds their temperature next to hot drain
  copper; measured on the first boards.

First-board check: thermocouples on D15 and D4 cases, on C46 and on Y1's case at 0.5 ms / 120 fps for 30 min, with the
headroom loop running; if the LEDs run above the derating curve, lower the LED current; Y1 must stay ≤ 80 °C,
else lower the LED current or duty, or pull the B.Cu zone's east edge back from Y1
([led](led.md#first-board-checks)).

## Assembly

- JLCPCB Economic PCBA (https://jlcpcb.com/capabilities/pcb-assembly-capabilities, read 2026-10-31): single-sided
  placement, standard stack-up, parts ≥ 0402, IC pitch ≥ 0.4 mm, BGA ≥ 0.5 mm, fixed 255 ± 5 °C reflow peak, no
  castellated holes, edge plating or gold fingers, panels with mouse bites only (no V-cut), edge rails and fiducials not
  needed. Every machine-placed part is on the top side.
- Order note: no mouse bites (no break-off tab) on the east edge beside C4 and the stretch next to U1. C4 (0805 MLCC) has
  its pads close to that edge; a tab there would flex the board at the capacitor when it is snapped off (MLCC flex
  cracking). C4 cannot move in: C6 abuts it on the west and U1 lies south (user decision 2026-11-04: "Note only").
- References: fab layers only. No reference designator is on the silkscreen (user instruction 2026-11-04); the
  assembly drawings from `kicad_export` `drawings` (top and mirrored bottom, SVG and PDF, the hand parts highlighted)
  carry them and ship with every fab package, for JLCPCB's review and for the hand assembly and first-board checks. The
  Value fields are hidden on F.Fab/B.Fab (the BOM carries the values), so the drawings show the references legibly.
- Hand-assembly list (left out of JLCPCB's BOM and placement file; `assembly.hand`): U6, J1, U2 on the bottom side (user
  decisions 2026-10-31: "U6, J1, U2, TP2, TP3, TP7" and "Hand parts on the bottom"; the top-side GND wire loops TP2, TP3
  and TP7 were removed, user decision 2026-11-11).
- Order: JLCPCB reflows the top side; then the hand parts are soldered:
  - U6: iron only, pad by pad, no hot air (user decision 2026-11-01), with the hand-solder footprint variant that has no
    paste apertures ([mcu](mcu.md#module)); top-side parts above its pads must not reflow again.
  - J1: through-hole from the bottom, soldered from the top. Its lands are close to reflowed SMD pads (R46, C3;
    `kicad_query distance`); JLCPCB gives no through-hole-to-SMD figure. Use a conical tip ≤ 1 mm and Kapton over R46
    and C3, then inspect both for bridges and shifted parts. If that fails, open the gaps to ≥ 1.0 mm in the next
    revision.
  - U2: through-hole from the bottom, soldered from the top. Its pads 5 and 6 are close to reflowed SMD pads (C11 and C52
    nearest; then C16.2 and R20.1). Pad 4 sits against C14's can (courtyards touching; the sleeve about 1 mm from the
    land): C14 cannot move away, U2.4 holds the top of the filter column ([Power](#power)). Pads 1–3 have no SMD part
    nearby. Use a conical tip ≤ 1 mm and Kapton over C11, C52 and C14's sleeve, then inspect C11 and C52 for bridges and
    shifted parts and C14's sleeve for heat damage.
- Tooling holes TOOL1–TOOL3: 1.152 mm round NPTH with 0.148 mm mask expansion, far apart in three corners, each with a
  copper/footprint keep-out ring (JLCPCB "How to add tooling holes for PCB assembly order",
  https://jlcpcb.com/help/article/How-to-add-tooling-holes-for-PCB-assembly-order, read 2026-10-31: "For Economic PCBA
  type, tooling holes are a must"; 2–3 holes in opposite corners; 1.152 mm round non-plated, 0.148 mm mask expansion).
- J2 overhangs the top edge ([USB](#usb)), nearer the routed edge than JLCPCB's ≥ 0.3 mm from depanel-routed edges
  (PCBA FAQ part 2). Order note: no mouse bites (no break-off tab), panel rail or neighbouring board within the overhang
  along J2's stretch of the top edge, so the shell clears the panel when it is routed and depanelled.
- Part spacing: JLCPCB's 1.0 mm from a QFN to any part holds at U1 and U10. Exceptions: C15–U4 (TPS2121 bypass "as close
  as possible") and R74.2–U12.2 ([IMU](#imu)).
- Small parts beside big ones (possible reflow shadowing; no source gives a number): R5 and C8 beside C46 (11.0 mm tall),
  courtyards touching; U12 beside L2 ([IMU](#imu)). R34, R37 beside C46/C47:
  [mcu](mcu.md#accepted-risks). R24 sits along C14's south edge, its pads parallel to the can, so both sit about as far
  from the can body.
- Polarity and labels: JLCPCB reads polarity from the silk layer (PCBA FAQ part 2, #10). Marks are held ≥ 0.15 mm (JLCPCB pad to silkscreen) from
  neighbouring pads (rule "Board silk to pads" applies to board-level silk; polarity marks inside footprints are checked on
  the footprint geometry): U8's pin-1 triangle and C46's "+" are the tight ones. J4 carries "1" and "CONTACTS DOWN". Board
  texts at the silk minimum name each probe point's signal: +5V, 3V3, 32V, LED_EN, LED_ON, ISNS, PPS, XVS, STB, PRST
  (TP1, the PHY reset; "RST" is the module reset button), plus "BOOT"/"RST" between the buttons, "USB", "TRIG" and
  "POE 57V". Each keeps ≥ 0.15 mm (JLCPCB pad to silkscreen) from pads and mask openings ("PRST" from J1.14 and J1's NPTH
  opening is the tight one).

First-board checks: J1 iron trial and inspection of R46 and C3; U2 iron soldering and inspection of C11, C52 and C14's sleeve; U6 iron
soldering; visual/AOI of the joints of R5, C8 (beside C46); USB-C cables in use (charger,
flashing cable) latch and enumerate ([mcu](mcu.md#first-board-checks)); voids under U1's and U10's exposed pads, where
the thermal vias are open ([Fabrication rules](#fabrication-rules)): X-ray if available, else check the
backside fillets in the vias and U1/U10/Q1 temperatures in the thermal runs. U5, U7 and Q1 were checked in JLCPCB's
placement preview on 2026-11-09 and have their `assembly.corrections` entries. D1 and the can part of C14, C46 and
C47 have no 3D model in that preview, so it shows neither D1's pin 1 nor the cans' polarity: check them in the
order's DFM Analysis (Order History, a few hours after ordering) before approving production, then add D1's
LCSC-keyed entry (`{"rot": 0}` when it needs none). JLCDFM's SMT check (2026-11-10, run on the `kicad_export` BOM and
CPL) compares pins and pads with its own part models: it flags pin edges flush with pad edges, U1's exposed pad, and
J2's shell legs off-centre in their slots. Check J2 in the placement preview too; if the legs sit off the slots there,
correct J2's `assembly.corrections` offset.

## Accepted deviations

Every warning class in the released check pass (`kicad_check`: erc, style, footprints, fields, pulls, rules, drc,
decoupling_placement, layout clean; unrouted 0), with its reason. A new warning not in these lists is a regression.
Run the named check to list the current spots.

### track_widths

Tracks narrower than their net class, by reason:

| Nets | Where | Reason |
|---|---|---|
| +3V3 | Ethernet branch after R38.1's former pad, the north/west feed loops, the B.Cu feed to them | Light load ([Power](#power), [Ethernet and PoE isolation](#ethernet-and-poe-isolation)) |
| +3V3 | R41/R42 branch feed from its via | mA loads |
| +3V3 | Short branches to pull-ups and U9's supply (among them R20's branch beside the VBUS via); the B.Cu feed beside a GND via; the R41/R42 feed beside CAM_IO1's former via; U1's branch at the PHY_REGOUT via; R16.1's neck from "C17/R16 +3V3" | mA loads; the drop at R53.1 for the whole feed is small ([Power](#power)) |
| +3V3 | U12.7's feed, the link under Y1, U5.5 → C25.1 round U5.4 | µA–mA (IMU, R14 pull-up, U5's clamp supply) |
| +3V3 | R73.1 to its via | Pull-up |
| +3V3 | U1's +3V3 pin stubs | Pin pitch |
| +5V | Exit east from the U8.1 pour; between the buck trunk vias | Buck branch peak only; the common node is poured ([Power](#power)) |
| +5V | Via of the U11/R63 branch → R63/C44/U11 | U11 supply and the R63 divider, < 1 mA |
| +5V | U3 EN feed from the buck trunk via to U3.1; U11/R63 supply stub | Enable pin and op-amp supply, mA |
| +5V_POE_F | Filter vias → U4's IN1 vias | The class width does not pass the VBUS IN2 via; sized by the fit in [Net classes](#net-classes) at the board-peak current |
| +5V_POE_F | Feed to R18 (PR1/OV divider) | µA |
| VBUS | J2.A4/B9 stub to the J2 VBUS via | Only exit between J2's pegs and pads; short, so its heating is limited |
| VBUS | J2.A4 → D2.1 and D2.1's flow-through exit; U4's IN2 pad entry | Short necks; the flow-through exit is no narrower than the existing TVS approach |
| VBUS | Feeds to R24 and R27 (VBUS_SENSE divider) | µA |
| VBUS | Source link from the J2 VBUS pour's via to the second via and the protected return from D2's via to the PWR_5V trunk | The class width does not fit; the return goes around the USB D− escape without bypassing D2.1 |
| +32V | C47 → C46 → ring | The worst-case LED pulse current at a low duty; ΔT by the fit in [Net classes](#net-classes) ([LED driver and ring](#led-driver-and-ring)) |
| +32V | Corridor east of Q1 | Pad-spacing rules; heating by the fit at the worst-case LED current as DC |
| +32V | D3.1 → C47 along the bottom edge, off the copper under U6 | Recharge current only |
| +32V | Feeds to R65 (boost FB divider), R71 (VLED sense divider) and TP13 | µA |
| RING_1–RING_11 | Ring links | A class-width link breaks the LED pad spacing |
| LED_K | R70.1 → Q1's drain pour | `LED_K_SENSE` divider, sub-mA |
| ISNS | The TP14 branch and the tap to NT1 | No load current; Q1.2 → R69.1 is the pour "ISNS pour 122" |
| USB_C_D_P/N, narrower than the USB class | Connector side J2 → U5 → R31/R32 | Full speed is electrically short; impedance not controlled ([MCU and USB](#mcu-and-usb)) |
| USB_D pair, coupled stretch | Into R31/R32 | Resistor pad pitch |
| CAM_D1 pair, CAM_CLK pair | CAM_D1's bump flat and via approach; CAM_CLK_P's F.Cu tail into its via | Skew-compensation and via-entry geometry, not coupled runs |
| ETH_TD pair, ETH_RD pair | U1's MDI pin escapes | 0.4 mm pin pitch |
| RMII_REF_CLK, RMII_REF_CLK_SRC | U1.10's pin stub and where the Default width does not fit | Pin pitch |
| IMU_INT1 | From U12.5 to the via under U12 | Between 0.5 mm-pitch LGA pads |

### return_path

| Track | Where | Reason |
|---|---|---|
| USB_C_D_P, F.Cu, three pieces | U5 link, split at the via tee | Crosses the In1 gap of the PoE lane (the isolation gap from the PoE copper); In2 is solid GND below. Full-speed USB, electrically short |
| ETH_TD_P, ETH_RD_N, ETH_TD_N | TD_P passing over J1.2 into J1.1, RD_N passing under J1.4 into J1.5, TD_N leaving J1.2 | J1's MDI antipads in In1 (pad plus the MDI spacing); a track leaving or passing a pin there crosses them |

### via_return

| Net | Via | Reason |
|---|---|---|
| USB_C_D_N | The via where its J2 branch leaves U5's pocket | No site within 1 mm (D2, the VBUS link and the D+ track surround it); the nearest GND via is further; In2 solid below; full-speed edges ≥ 4 ns |

### Pad entries

The pad-entry audit's remaining findings ([Routing style](#routing-style)). Each one needs a part or a neighbouring via
moved; placement is unchanged.

| Pad or via | Net | Finding | Why no compliant entry |
|---|---|---|---|
| R1.2 → R4.1 | PHY_LED0 | Ends off-centre; R1.2's exit at 45° | Routed while R7.1, R1.1 and the PHY_LED3 track boxed R1.2 in; kept as routed when R7 was removed (user decision 2026-11-11) |
| R10.1, the two COL/RMII vias | COL/RMII (U1.4) | Ends off-centre, bends inside or close to the vias | U1.4's axis runs between XTAL_X2 (a via there lands too close to it) and RMII_TX_EN, with MDC parallel on B.Cu; R10.1 is boxed in by C5.2, R10.2 and a GND via below |
| U9.4 | LED_ON | End off-centre | A via on its axis lands too close to R61.2 (GND) or to U9.4 inside its mask opening; R61's pad gap is too narrow |
| C49.1 | VLED_SENSE | Exit at 45°, end off-centre | Only an exit to the east fits; R71.2's link comes from the west |
| U6.16 (B.Cu) | LED_PWR_EN | End off-centre | A centre exit passes too close to the VLED_SENSE via, which the USB_C_D_P B.Cu diagonal holds in place |
| STROBE via, B.Cu | STROBE | Bend inside the via, end off-centre | Its diagonal runs close to the parallel STATUS_LED and CAM_EN diagonals; a via there needs more room |
| U11.1; MDIO via | SINK_DRV, MDIO | Ends off-centre; MDIO bends inside its via | The MDIO hop over the USB_C_D_P diagonal sits just east of U11.1; moving it brings MDIO too close to USB_C_D_N |
| U5.3, U5.4 | USB_C_D_N | Ends off-centre | A straight on-axis link passes almost on U5's GND via |
| D2.1, the D2.1 exit via | VBUS | Ends off-centre, bend inside the via | Moving the via brings its B.Cu leg too close to the GND via beside it |
| J1.7 (In1) | POE_VA1 | End off-centre | A radial approach leaves the PoE lane rule area or comes too close to J1.5 (PoE isolation) |
| J1.5 | ETH_RD_N | End off-centre | A compliant entry comes too close to J1.7 (POE_VA1, "PoE isolation"); raising the lane collides with RD_P |
| J2.B7 | USB_C_D_N | Last bend just outside the pad | CC1 runs close above the crossover |

### Other accepted deviations

| Deviation | Accepted by | Section |
|---|---|---|
| No impedance control ordered; MIPI outside the HDG's ± 10 % at the width extremes | User decision 2026-11-01 | [Impedance](#impedance) |
| Stock silk closer to pads than the rule allows, and over tented vias; JLCPCB clips it | User decisions 2026-11-02 and 2026-11-10 | [Fabrication rules](#fabrication-rules) |
| CSI In2 neck narrower than SPRAAR7J's 90 mil each side; short of it with the 45° crossing | User decision 2026-11-01 | [Camera and CSI-2](#camera-and-csi-2) |
| CAM pairs closer than 90 mil to the plane edge at the FFC notch | User decision 2026-11-01 | [Camera and CSI-2](#camera-and-csi-2) |
| No GND guard round the MIPI CLK pairs | User decision 2026-11-01 | [Camera and CSI-2](#camera-and-csi-2) |
| Planes solid under the link and J4 pads except where the MIPI via antipads reach: part of the aligned south link pads R45–R47, R49, R50, part of R51's west edge, a sliver off J4's MIPI pads 2, 3, 5, 6; adjacent antipads join into slot voids along each via row | Design review 2026-11-01; geometry corrected 2026-11-02 | [Camera and CSI-2](#camera-and-csi-2) |
| MDI 3W waived inside "U1 MDI escape" | Design review 2026-11-01 (0.4 mm pitch) | [Ethernet and PoE isolation](#ethernet-and-poe-isolation) |
| Both MDI pairs P/N-inverted between U1 and J1 (no swap vias), relying on the IP101G's polarity handling; the link partner receives our transmit pair inverted | User decision 2026-11-04 | [Ethernet and PoE isolation](#mdi) |
| A few RMII/strobe layer changes without a close GND via (REF_CLK, TXD0, TXD1, TX_EN, TX_EN_MCU, two STROBE vias); every XVS layer change has a return close by | Design review; XVS routing corrected 2026-11-02 | [Ethernet and PoE isolation](#rmii) |
| No GND guard at the crystal | Design review (IP101G: "if possible") | [Ethernet and PoE isolation](#rmii) |
| Crystal loop: XTAL_X1 round Y1 against the short XTAL_X2, enclosing a small area over In1 GND | User decision 2026-11-03 | [Ethernet and PoE isolation](#rmii) |
| Thermal vias open inside the exposed pads of U1 and U10: solder wicking and voids possible | User decision 2026-11-03 | [Fabrication rules](#fabrication-rules) |
| MDC, MDIO, the +3V3 feed to R14 and the COL/RMII strap cross under Y1, C5 and the X1/X2 traces on B.Cu, behind the In1 and In2 GND planes; MDC crosses the XTAL_X2 diagonal at right angles and the X2 vertical at 45°, passing under the Y1.3 and C5.1 pad corners, and runs parallel to a crystal trace only under U1.2's pad and X1 stub; the COL/RMII strap runs under C5.1. ETH_LED_G crosses the X1 link C2.1–Y1.1 once, at right angles | Design review 2026-11-02: no other B.Cu way west from U1's management pins (RMII fan-out, MDI, Q1 drain zone, board edge) | [Ethernet and PoE isolation](#rmii) |
| REGOUT path U1.28 → C6 two vias long, then the C6/C4 pour: pin 28 lies between the MDI pairs | Design review; REGOUT ripple checked on the first boards | [Ethernet and PoE isolation](#phy-side-decoupling) |
| F.Cu 1.0 mm (not the rule's same-layer gap) from the In1 PoE copper; the F.Cu pours clipped to that distance are the closest | Design intent: some isolation only, no level rated or tested (user decision 2026-11-02) | [Ethernet and PoE isolation](#poe-isolation) |
| Q1 drain copper limited, Q1 close to D15/D4 and to Y1 | User decision 2026-11-01: measure first boards | [LED driver and ring](#thermal) |
| C15–U4 and R74–U12 closer than JLCPCB's 1.0 mm QFN spacing | Design review | [Assembly](#assembly) |
| Small parts beside big ones: R5/C8 at C46, U12 at L2 | Board review 2026-11-04: first-board inspection | [Assembly](#assembly) |
| J1 lands close to reflowed SMD pads | First-board soldering trial | [Assembly](#assembly) |
| U2.5/U2.6 close to reflowed C11/C52 pads; U2.4 against C14's can (courtyards touching) | First-board soldering trial | [Assembly](#assembly) |
| J2 overhangs the top edge, nearer the routed edge than JLCPCB's component-to-edge limit; order note | User decision 2026-11-04 | [MCU and USB](#usb), [Assembly](#assembly) |
| Buttons on the bottom edge | User decision 2026-11-01 | [MCU and USB](#buttons-and-boot) |
| B.Cu under U2's flyback magnetics: VBUS, USB_C_D_P/N, CAM_EN, SCCB_SDA/SCL, STROBE, STATUS_LED and a short XVS hop; XVS's long west run kept west of it | User decision 2026-11-04: "Partial + document"; first-board check at the PoE peak | [B.Cu under U2](#bcu-under-u2) |
| B.Cu under U2's flyback magnetics: the +3V3 west-edge loop down the filter column (BOM change 2026-11-09, C14's larger can) | User decision 2026-11-09: "Accept" (no F.Cu way past C14, no B.Cu lane west of XVS; decoupled DC rail); first-board check at the PoE peak | [B.Cu under U2](#bcu-under-u2) |
| CAM_CLK_P/N split on F.Cu between J4 and their vias: P detours north of N to take up the length, uncoupled for a stretch | User decision 2026-11-04: "Accept + record" (re-coupling needs trapezoid bumps drawn in pcbnew) | [Lengths and skew](#lengths-and-skew) |
| C4 (0805 MLCC) pads close to the east edge; order without mouse bites beside it | User decision 2026-11-04: "Note only" | [Assembly](#assembly) |
| R72.2's GND via hole closer to the pad opening than the mask bridge | Board review 2026-11-04: no site further out | [Fabrication rules](#fabrication-rules) |
| Vias closer than 0.1 mm to a pad of their own net: C18.2's, the U9.1/U6.27 GND via, C40.2/U11.2's | User decision 2026-11-10: no site that holds the clearances | [Fabrication rules](#fabrication-rules) |

### JLCDFM

JLCDFM's online check (https://jlcdfm.com, run 2026-11-10 on the `kicad_export` Gerbers, BOM and CPL) still reports
these classes. Each one is a decision above or a check that doesn't apply to this board (user decision 2026-11-10).

| Finding | Cause | Why it stays |
|---|---|---|
| Pad spacing (warning) | U12's LGA pads, U1's and J2's pin pitch | Rule "U12 LGA pads"; manufacturers' land patterns |
| Pad spacing, via to pad | The three vias in the row above | No site that holds the clearances |
| Slot width | J2's plated shell slots | At or above JLCPCB's plated-slot minimum (rule "Plated slot width"); stock HRO land pattern |
| Lead to hole | The thermal vias in U1's and U10's exposed pads | User decision 2026-11-03 ([Fabrication rules](#fabrication-rules)) |
| Silkscreen to pad, silkscreen to hole | Stock footprint silk | User decisions 2026-11-02 and 2026-11-10 ([Fabrication rules](#fabrication-rules)) |
| Solder mask opening exposing trace | A track of the pad's own net next to the opening | Same net; KiCad's mask-to-copper clearance keeps other nets clear |
| Negative solder mask expansion, soldermask bridge, silkscreen line width, annular ring, trace spacing | Openings 1:1 with pads, webs between fine-pitch pads, silk at the minimum line width, signal-via ring, diff-pair gaps | At JLCPCB's limits ([Fabrication rules](#fabrication-rules)) |
| Plated through-hole to trace | J2's locating pegs beside VBUS tracks | At the board's hole clearance |
| Missing plated through-hole | Top-side parts over U6's bottom pads | Different parts on opposite sides; no hole intended |
| Component to board edge, clipped by board outline | J2's overhang; C4 and the cans along the edges | J2 and C4: rows above; the rest stays inside JLCPCB's component-to-edge limit |
| Component collision | C13 and C14 cans, courtyards touching | Courtyards (body plus margin) don't overlap |
| Pin edges, lead area overlapping pad, component through-hole | JLCDFM's part models against the land patterns (U10, U1, J2 in its examples) | Manufacturers' land patterns; checked in the placement preview ([Assembly](#assembly)) |
