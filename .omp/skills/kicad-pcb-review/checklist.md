# Board layout review checklist

Items carry an ID that findings and passes cite (`"item": "RET-2"`). Every item applies in `change` mode (to the parts and nets in scope) and in `standard` and `full` mode, except those marked **(full)**: those run in `full` mode only. REL runs in `full` mode or when the user asks for a release check. **Auto:** names the `kicad_check` check that already proves the item (for `drc`, with KiCad's violation types): read its result and don't redo it by hand. Names in parentheses (`distance`, `copper {box}`, …) are `kicad_query` queries that give the data; `V1`…`V10` are the views in [Renders](#renders). An item that doesn't apply to the scope is `na`, not a pass; on a board not routed yet, the items that judge copper are `na`.

Limits come from the datasheet (document, page, table/figure), a requirement in `layout.md` (`docs/hardware/layout.md`, with its section) or a rule's cited source; board figures come only from `kicad_query` or a check, never from a doc; computed numbers show the formula and its inputs. The review skill's **Design assumptions** (`skill://kicad-pcb-review`) limit what counts: the fab within its published capability, the rules as the contract for what they cover, the schematic reviewed, the project's accessories and enclosure as documented, vendor guidelines rather than simulation, one tolerance at a time. A case those assumptions exclude is not a finding under any item below. Source keys (`[TI-HS]`) are listed in [Sources](#sources).

## GEN: general (orchestrator)
- **GEN-1** Board and schematic in sync: every symbol has its footprint and every footprint its symbol, values and footprints match, no pad's net differs from the schematic. **Auto:** `drc` (parity). [ALT-CL]
- **GEN-2** DRC is clean with zones refilled: no error, no unrouted connection on a board called routed, and every exclusion's comment says why it is acceptable (an exclusion without one is a finding). **Auto:** `drc`. [ALT-CL][AZ]
- **GEN-3** Board-wide minimums equal the fab's no-extra-charge capabilities (sources in `layout.md`, Fabrication rules), and nothing smaller is in use except the exceptions listed there (`board` rules, `stats` minimum track, clearance and drill). No DRC violation type the design relies on is set to ignore or lowered to warning (`board` severities; e.g. `missing_courtyard` ignored hides a footprint without courtyard from `courtyards_overlap`). [ALT-MFG][AZ]
- **GEN-4** Net classes and custom rules serve the purpose `layout.md` (Net classes, Custom rules) gives them, each value follows from the source it cites (recompute it), and every rule's comment states that source without measured board figures (`netclasses`, `rules` queries). **Auto:** `netclasses`, `rules` prove the classes resolve and the `.kicad_dru` loads.
- **GEN-5** Every measurable constraint in `layout.md` (a part near a pin, length groups, star-point branches, via parity, copper area and vias, the lens cone) is an entry of the project's `layout` config in `.omp/kicad.json` with its `why`; a constraint no check can carry ("never parallel", a minimum distance between blocks) is judged under its item. **Auto:** `layout` (a ref, pad, net or layer it names that the board lacks is an error).
- **GEN-6** The board meets every requirement `layout.md` states (stack-up, mechanical positions, keep-apart distances, the exceptions to the minimums), and `layout.md` states no value the KiCad files hold (positions, measured distances or lengths, counts, rule values): such a statement is a minor finding, fixed by removing it. GPIO swaps made at placement are in the schematic and the GPIO map of `firmware-rules.md`. Say which side is wrong.

## STK: stack-up (orchestrator)
- **STK-1** The board's stack-up (layer count, thicknesses, Dk, copper weights, finish, thickness) is JLCPCB's published data for the stack-up `layout.md` orders (`board` stackup). [ALT-CL][AZ]
- **STK-2** Recompute each impedance class (standard: one differential and one single-ended; **(full)** all) from that stack-up with solder mask over the trace: the class width and gap land within ±10 % of the target. [ESP][TI-ETH]
- **STK-3** L2 (`In1.Cu`) is solid GND: no tracks on it, one GND zone over the whole board, broken only by antipads; L3 is solid under every `B.Cu` impedance track (`copper {layer: "In1.Cu", kind: "track"}`, `zones`, V5). **Auto:** `return_path` for the impedance-class tracks. [ESP][TI-EMI]
- **STK-4** **(full)** Copper is balanced between opposite layers, with no large one-sided copper area, to limit bow and twist at reflow (`stats` copper per side, `zones` fill area per layer). [AIV]

## FTP: footprints (stock footprints; project library parts go to `kicad-library-review`)
- **FTP-1** Each IC and connector footprint is the land pattern of the ordered MPN's package (datasheet or IPC-7351 pattern), pin numbering read from the right view (`lib_footprint`, `kicad_datasheet`). **Auto:** `footprints` proves pins ↔ pads only. [AZ][TI-QFN]
- **FTP-2** FPC/FFC connectors: contact side and pin 1 match the cable that will be fitted (J4: `layout.md` Camera; V10). [AZ]
- **FTP-3** Every part has a 3D model that lines up with its pads, pin 1 and height (`footprint {ref}` lists the models; V10). [ALT-CL][AZ]

## PLC: placement
- **PLC-1** Every footprint is placed, inside the outline, on its intended side (all on top here), with no courtyard overlaps. **Auto:** `placement`, `drc` (`courtyards_overlap`). [ALT-CL]
- **PLC-2** Mechanically fixed parts (holes, connectors, lens-axis parts, LED ring, J4) sit where `layout.md` (Board and stack-up) puts them and are locked (`footprints` at, rot, locked).
- **PLC-3** Interfaces flow one way without wrap-around: connector → ESD → filter → IC, MAC → PHY → magnetics → jack; the ratsnest doesn't cross itself more than the pinouts force (V2). [TI-HS][AIV]
- **PLC-4** Noisy blocks (switchers, the LED pulse loop, the PoE module) sit apart from sensitive ones (crystal, sense nodes, IMU, pairs) by the distances `layout.md` states (`distance`; the `layout` check holds only maximum distances). [TI-EMI][ESP]
- **PLC-5** Each crystal sits at its IC's pins, load caps at the crystal, a GND guard around it, no signal under it or its traces, no via on its clock traces, no inductor nearby (`distance`, `copper {box}`, V7). **Auto:** `layout` (near) where configured. [ESP][TI-ETH]
- **PLC-6** Every part `layout.md` puts at a pin sits within its distance, and on the right side of any branch: series terminations at the driver, ESD/TVS and filters at the connector before the net branches, feedback, compensation, soft-start and bias parts at their pins (`distance`). **Auto:** `layout` (near) where configured. [ALT-CL][TI-HS][TI-BUCK]
- **PLC-7** High-thermal-mass parts (DPAK, large inductors, electrolytics, modules) are not right beside very small parts (`distance`). [ALT-CL]
- **PLC-8** Every part fits its height zone: outside the lens view cone (`layout.md` Lens view cone) and under the enclosure. **Auto:** `layout` (heightZones). [ALT-CL]
- **PLC-9** USB-C, the buttons and the status LED are reachable and visible with the board in its enclosure (V10). [ALT-CL][MF]

## DEC: decoupling
- **DEC-1** Every IC power pin has its nearest decoupling capacitor within `decouplingMaxDistance`, the smallest value closest, at the pin the datasheet names. **Auto:** `decoupling_placement` (nearest capacitor and distance); value order and per-pin assignment are manual. [ALT-CL][TI-ETH][ESP]
- **DEC-2** Each capacitor's ground pad and each IC ground pin has its own short via to the plane (not shared with another capacitor), in pin → capacitor → via order (`copper {box, kind: "via"}`). **Auto:** `decoupling_placement` proves a ground via or fill within 1 mm, not sharing or order. [ALT-CL][AZ]
- **DEC-3** Bulk capacitance sits at each block's power entry (`layout.md` per block, `distance`). [ESP][AIV]

## PWR: power layout
- **PWR-1** Buck: the input capacitor is placed first, on the IC's layer, closest to VIN/PGND, with no via in that loop (V3). **Auto:** `layout` (near) where configured. [TI-BUCK][TI-SMPS]
- **PWR-2** Switch-node copper is no larger than its current needs, the inductor is close, and nothing sensitive runs under or beside it, with solid GND below (`copper {net}`, `distance`, V3). [TI-BUCK][TI-BOOST]
- **PWR-3** Boost: the output capacitor and the diode close the smallest loop to the IC's PGND, on the top layer (V3). [TI-BOOST]
- **PWR-4** The feedback divider sits at FB, its sense trace leaves the output capacitor and runs short and away from SW and the inductor; the output capacitors return to the input capacitor's ground (V3, `distance`). [TI-BUCK][TI-SMPS]
- **PWR-5** Signal ground (FB, COMP, soft start, sense parts) joins power ground at one point, and Kelvin sense leaves its pad through the net tie carrying no load current (`lengths {paths}`, V3). **Auto:** `layout` (paths) where configured. [TI-BOOST][TI-BUCK]
- **PWR-6** Each current-carrying net carries its peak current at the allowed temperature rise (IPC-2152): the class width for class nets, by hand for Default-class power nets (`layout.md` Net classes names them) and pours. **Auto:** `track_widths` proves tracks at or above their class width. [ALT-HV][TI-SMPS][ESP]
- **PWR-7** Layer changes on power nets have enough vias (TI: one per ampere as a start; AN-1149: one per 200 mA) (`copper {net, kind: "via"}`). **Auto:** `layout` (copperArea minVias) where configured. [TI-BUCK][TI-SMPS]
- **PWR-8** Pours and planes are not necked down or slotted along the current path, including rows of antipads that cut L2 or L3 (`zones` fill regions, V5). [AZ][TI-EMI]
- **PWR-9** Each regulator follows its datasheet's layout example, or `layout.md` states the deviation (`kicad_datasheet`). [TI-BUCK][TI-BOOST]

## IMP: impedance and high-speed
- **IMP-1** Every impedance-class net runs at its class width and gap on the layers `layout.md` (Impedance) assumes (`lengths` per layer). **Auto:** `track_widths`. [ESP][TI-HS]
- **IMP-2** Differential pairs run coupled and symmetric; the uncoupled breakout at pads is short (TI: within 0.25 in of the package) (V4). **Auto:** `drc` (`diff_pair_uncoupled_length_too_long`) only where a rule sets a limit. [TI-HS][AZ]
- **IMP-3** Intra-pair skew is within its limit (MIPI ≤ 10 mil, MDI ≤ 50 mil, USB equal lengths), with the tuning at the mismatched end (`lengths` skew). **Auto:** `drc` (`skew_out_of_range`) for classes with a skew rule. [ESP][TI-ETH]
- **IMP-4** Group matching and length limits hold (CSI pair to pair ≤ 30 mil, the REF_CLK branches, MDI < 2 in, RMII < 6 in), and nets take direct routes, meanders only for matching (`lengths`, `distance` for the straight line). **Auto:** `layout` (lengthGroups, paths). [ESP][TI-ETH][ALT-CL]
- **IMP-5** Pairs keep ≥ 3W from other fast signals and never run parallel to them, pairs ≥ 30 mil apart and ≥ 50 mil from clocks where `layout.md` sets no tighter rule (V4, `distance`). **Auto:** `drc` (`clearance`) for the custom spacing rules. [ESP][TI-HS][TI-ETH]
- **IMP-6** Via count is minimal and equal on both legs of a pair and on every lane of a matched bus. **Auto:** `layout` (viaParity). [TI-HS][ESP]
- **IMP-7** No test point or stub on a pair; ESD and series parts sit inline (flow-through), 0402 or smaller (`routing {net}` pads, V4). [TI-HS][ALT-CL]
- **IMP-8** Pairs bend at 135° or gentler, and no track makes an acute angle (V4). [TI-HS][AIV]
- **IMP-9** Pairs keep clear of crystals, switchers, magnetics and mounting holes, and ≥ 90 mil inside the edge of their reference plane (`distance`, V4). **Auto:** `return_path` (plane under the track). [TI-HS][ESP]
- **IMP-10** **(full)** Where a pad on a pair is much wider than the trace (connector pins, 0201 links), the plane under it is voided or the effect is accepted in `layout.md`. [AZ][TI-HS]

## RET: return paths and planes
- **RET-1** Every impedance-class track has a ground plane on the adjacent layer for its whole length. **Auto:** `return_path`. [TI-HS][ESP]
- **RET-2** Every layer change of an impedance-class net has a GND return via beside it. **Auto:** `via_return`. [TI-HS][ESP][TI-ETH]
- **RET-3** Fast nets outside `impedanceClasses` (RMII, REF_CLK, XVS, STROBE, the LED gate) also keep one reference plane and get a GND via at each layer change (V4 with their nets, `copper {net, kind: "via"}`). [TI-HS][TI-EMI]
- **RET-4** GND pours are stitched to the plane across the board, with no islands and no unconnected copper (`zones` island removal). **Auto:** `drc` (`isolated_copper`). [TI-ETH][AIV][AZ]
- **RET-5** Sensitive nets don't run under or beside noisy parts (inductors, switch nodes, the PoE module, `LED_K`) (`distance`, V3). [ALT-CL][TI-BUCK]

## EMC
- **EMC-1** Every high di/dt loop (switcher hot loops, the LED pulse loop) encloses the smallest area the parts allow, on one layer where it can, with the current turning the same way in both switch states (V3). [TI-EMI][TI-SMPS]
- **EMC-2** No track or via ends unconnected. **Auto:** `drc` (`track_dangling`, `via_dangling`). [ALT-CL]
- **EMC-3** On every ESD-protected line the track reaches the TVS pad before the protected part, and the TVS has a short via to GND (V3/V4 with the line's nets). [ALT-CL][TI-HS]
- **EMC-4** Unprotected connector-side traces keep away from protected ones and from other nets (`distance`). [TI-HS]
- **EMC-5** Connectors have enough GND vias for their return current (`copper {box, kind: "via"}`). **Auto:** `layout` (copperArea minVias) where configured. [ALT-CL]

## THM: thermal
- **THM-1** Each dissipating part has the copper area and thermal vias (TI: ≈ 1.0 mm pitch, 0.3 mm drill to start) that its datasheet rating assumes, on the right net and layers (`copper`, `zones`). **Auto:** `layout` (copperArea) where configured. [TI-QFN][TI-BOOST][ESP]
- **THM-2** Plane connections use thermal reliefs except where the pad is a heat path, and two-terminal chip parts see similar copper on both pads against tombstoning (`zones` connect_pads, `lib_footprint` overrides). **Auto:** `drc` (`starved_thermal`). [AZ][ALT-MFG]
- **THM-3** Hot parts are spaced apart, and temperature-sensitive parts (IMU, crystal) keep the distance `layout.md` states from them (`distance`). [AIV]

## ISO: isolation and clearance
- **ISO-1** Every voltage domain has a clearance (and creepage where it applies) from IPC-2221 or the product safety standard, carried as a DRC rule; bare copper counts, mask doesn't (`rules`). **Auto:** `drc` (`clearance`, `creepage`). [ALT-HV][ALT-CL]
- **ISO-2** Cable and shield copper meets board copper only at the bridge parts `layout.md` names, on every layer, with no overlapping planes (`distance {net}`, `zones`, V6). **Auto:** `drc` (`clearance` from the isolation rule). [TI-ETH][TI-HS]
- **ISO-3** No copper under the cable-side pins and magnetics `layout.md` names (J1 pins 7–10, U2 pins 1–4) on any layer (`copper {box}`, `zones` rule areas, V6). [TI-ETH][AZ]
- **ISO-4** Each mounting hole's pad net matches its intent and keeps the isolation distance from other domains (`footprint {ref}`, `distance`). **Auto:** `drc`. [AZ]

## MEC: mechanical
- **MEC-1** The outline is closed, and its size, corner radii, cutouts and notches match `layout.md` (`board` outline and cutouts). **Auto:** `drc` (`invalid_outline`). [ALT-CL][SC]
- **MEC-2** Mounting holes take the chosen screw, with copper and part clearance for the head or washer (Sierra: head ≥ hole + 40 mil) (`distance`, `board_free_space`). [SC][AZ]
- **MEC-3** Connectors sit at the edge with the right setback, orientation and keying, with room for the plug or cable (RJ45 latch, USB-C overmold, FFC bend) (`footprints`, V10). [AZ][SC]
- **MEC-4** No 3D collisions between parts, and part heights fit the enclosure (V10). [ALT-CL][SC]
- **MEC-5** Keep-outs at the lens cutout, the FFC notch and the board edge hold for copper and parts. **Auto:** `drc` (`copper_edge_clearance`), `placement` (rule areas). [ALT-CL][AZ]
- **MEC-6** Stress-sensitive parts (MLCCs, thin QFNs, the IMU) are clear of mounting holes and edges, and large MLCCs lie parallel to the nearest edge (`distance`). [AZ][TI-QFN]

## DFM: fabrication
- **DFM-1** Annular rings, drills, hole-to-hole, hole-to-copper and copper-to-edge are within the board rules. **Auto:** `drc` (`annular_width`, `drill_out_of_range`, `hole_clearance`, `hole_to_hole`, `copper_edge_clearance`). [ALT-MFG][SC]
- **DFM-2** **(full)** Drill sizes are few (`stats` drill table). [ALT-MFG]
- **DFM-3** Solder mask: an opening on every pad, a web between fine-pitch pads at or above the fab minimum or a deliberate gang opening, NSMD pads unless the land pattern says otherwise (`lib_footprint`, V9). **Auto:** `drc` (`solder_mask_bridge`). [ALT-MFG][TI-QFN]
- **DFM-4** No open via in a pad except deliberate thermal vias in exposed pads (`copper {box, kind: "via"}`, V9). [AZ][TI-QFN]

## DFA: assembly (JLCPCB)
- **DFA-1** Body-to-body gaps meet JLCPCB's table (e.g. chip ↔ chip 0.15 mm, chip ↔ SOT 0.2 mm, chip ↔ SOIC 0.4 mm, anything ↔ QFN 1.0 mm) (`distance` between footprints). [JLC-SP]
- **DFA-2** Fiducials, tooling holes and edge rails: either on the board as JLCPCB specifies (three asymmetric fiducials ≥ 3.35 mm from the edge; 1.152 mm NPTH tooling holes; 5 mm rails for parts near an edge), or left to JLCPCB to add, and the order says which (`footprints`, `distance`). [JLC-FID][JLC-TH][JLC-PE]
- **DFA-3** Hand-soldered and reworkable parts (`assembly.hand`; reasons in `layout.md`, Assembly) have room for the iron or nozzle around them (`board_free_space`, `distance`). [ALT-CL][AIV]
- **DFA-4** Paste: exposed pads print as segmented windows, no paste on test points and fiducials (`lib_footprint`, V9). [AZ][TI-QFN]
- **DFA-5** **(full)** Through-hole pads that get soldered are plated (`stats` pad counts, `footprint {ref}`). [ALT-CL]

## SLK: silkscreen
- **SLK-1** Text size, silk clearance and mirroring. **Auto:** `drc` (`text_height`, `text_thickness`, `silk_over_copper`, `silk_overlap`, `silk_edge_clearance`, `nonmirrored_text_on_back_layer`). [AZ][SC]
- **SLK-2** Each designator sits next to its part and names it without ambiguity; in dense areas it moves out or goes, never overlaps (V8). [ALT-CL][AZ]
- **SLK-3** Every IC, diode, LED, polarised capacitor and keyed connector has a pin-1 or polarity mark that stays visible after assembly, outside the body (V8). [JLC-POL][ALT-CL]
- **SLK-4** The board carries name, revision and date; connectors, buttons, LEDs, test points and J4's camera pin 1 and contact side are labelled; the PoE cable side is marked as a hazard (`board_graphics`, V8). [ALT-CL][SC]

## TST: test access
- **TST-1** Every probe point the schematic review's FW-4 relies on (test point or accessible passive pad) is on top, not under a part, and reachable with the board in its enclosure and the camera fitted (`footprints`, V10). [ALT-CL][MF]
- **TST-2** A GND point is within probe-spring reach of each probed signal, and adjacent pads are far enough apart that a probe can't short them (`distance`). **Auto:** `layout` (near) where configured. [ALT-CL][MF]

## REL: fab release (orchestrator; `full` or when asked)
- **REL-1** The board's title block revision is set (else the outputs go to `fab/draft-…`), and `kicad_export {outputs: ["gerbers", "assembly", "step", "drawings"]}` writes them all. A refused output is a finding with the failed list from the answer (DRC errors with zones refilled, parity issues, unconnected items, ERC errors, `fields` errors).
- **REL-2** Every part the answer lists as left out of the assembly has the intended reason: DNP, hand assembly (`assembly.hand`, with its reason; listed in `<board>-hand-assembly.csv` and highlighted in the drawings) for every part soldered by hand (U6), excluded from the BOM, no LCSC field, not on the board. The board-only footprints (holes, net ties, logos) are the expected ones; no `assembly.hand` reference is DNP or missing from the board.
- **REL-3** Every footprint the answer lists as not confirmed in JLCPCB's placement preview goes to `unverified`, how: check it in the preview, then add an `assembly.corrections` entry in `.omp/kicad.json` (`{"rot": 0}` when none is needed). An existing correction that disagrees with the part's pin 1 in V8 is a finding.
- **REL-4** The STEP answer names no missing model; the user checks the STEP against the enclosure (`unverified` until done).
- **REL-5** The order carries what `layout.md` (Board and stack-up) asks: the JLC04161H-7628 stack-up, 1.6 mm, ENIG, mask over the tracks, no impedance control (user decision 2026-11-01). JLCPCB's Gerber viewer shows every layer, the outline, the cutout and the notch, and drill hits on pads; these are the user's checks, listed as open questions. [AZ][SC]

## Renders
`kicad_render` with `board: true`. 2D images are seen from the top in board mm; `box` is `[x0, y0, x1, y1]` (a block's courtyards from `footprints {sheet}`, or the region the previous answer names). `nets` takes full net names as `kicad_query nets` lists them (no patterns) and highlights them only on the layers drawn. 3D takes no `box`, `layers` or `nets`.

| View | Args | Look for | Items |
|---|---|---|---|
| V1 copper | `layers: ["copper"]`, then `box` per block | the whole routing, block by block | STK-3, PLC-3 |
| V2 ratsnest | `ratsnest: true` (with `nets` for one net) | placed: crossings that show a bad placement; routed: no line left | PLC-3, GEN-2 |
| V3 hot loops | `layers: ["F.Cu", "In1.Cu"]`, `nets` of the loop, `box` at the regulator: buck `["/Power/SW_3V3", "+5V", "+3V3", "GND"]`; boost `["/LED driver/BST_SW", "/LED driver/+5V_BST", "+32V", "GND"]`; LED pulse loop `["+32V", "/LED driver/LED_K", "/LED driver/ISNS", "GND"]` plus the `RING_*` nets | small loops on one layer, input (buck) / output (boost) capacitor tight to the IC, small SW copper, solid GND below | PWR-1…5, EMC-1, EMC-3, RET-5 |
| V4 pairs | `layers: ["F.Cu"]`, `nets: ["/Camera/CSI_D0_P", "/Camera/CSI_D0_N"]` (each pair), `box` along it; again with `layers: ["In1.Cu"]` (a `B.Cu` pair: `["B.Cu"]`, then `["In2.Cu"]`) | coupled all the way, gentle bends, nothing parallel close by, no gap under it, GND vias at each layer change | IMP-2, IMP-5, IMP-7…9, RET-3, EMC-3 |
| V5 planes | `layers: ["In1.Cu"]` and `["In2.Cu"]`, `nets: ["GND"]` | unbroken GND, no slot from antipad rows, no thin neck in power pours | STK-3, PWR-8, RET-4 |
| V6 isolation | `layers: ["copper"]`, `nets: ["/Ethernet/POE_VA1", "/Ethernet/POE_VA2", "/Ethernet/POE_VB1", "/Ethernet/POE_VB2", "/Ethernet/ETH_SHIELD", "/Ethernet/ETH_SHIELD_R"]`, `box` around J1 and U2 | a gap continuous on every layer, nothing under the cable-side pins, holes on the right side | ISO-2, ISO-3 |
| V7 crystal | `layers: ["copper"]`, `nets: ["/Ethernet/XTAL_X1", "/Ethernet/XTAL_X2"]`, `box` around Y1 | guard ring, no track under it, load caps at the crystal | PLC-5 |
| V8 silk and courtyards | default layers with `box` (`side: "bottom"` for the back) | pin-1 and polarity marks outside bodies, designators clear, nothing on pads | SLK-2…4, REL-3, PLC-1 |
| V9 mask and paste | `layers: ["F.Mask"]`, then `["F.Paste"]`, `box` at fine-pitch parts and exposed pads | mask webs, segmented paste, no paste on test points | DFM-3, DFM-4, DFA-4 |
| V10 3D | `view: "3d"`; `side: "bottom"`; `rotate: [-45, 0, 45]` (isometric); `side: "front"` / `"left"` with `zoom` for edge-on | connector fronts at the edge and plug room, heights against the lens cone and enclosure, J4's contact side and pin 1, models on their pads, probe points free | FTP-2, FTP-3, PLC-9, MEC-3, MEC-4, TST-1 |

## Sources
- **ALT-CL** M. Harris, "PCB Design Review Checklist", Altium, 2021/2026: https://resources.altium.com/p/pcb-design-and-review-checklist
- **ALT-MFG** Z. Peterson, "A PCB Layout Review Checklist Gets You to Manufacturing Quickly", Altium, 2020/2026: https://resources.altium.com/p/pcb-design-review-checklist-gets-you-manufacturing-quickly
- **ALT-HV** Z. Peterson, "Using an IPC-2221 PCB Clearance Calculator for High Voltage Design", Altium, 2025: https://resources.altium.com/p/using-an-ipc-2221-calculator-for-high-voltage-design
- **AZ** A. Zonenberg, "Layout review checklist" (pcb-checklist): https://github.com/azonenberg/pcb-checklist/blob/master/layout-checklist.md
- **AIV** A. Chen, "PCB Design Review for Beginners: A Step by Step Guide", AIVON, 2026: https://www.aivon.com/blog/pcb-design/pcb-design-review-for-beginners-a-step-by-step-guide/
- **SC** P. Ghosh, "PCB Design Checklist to Build a Flawless Board", Sierra Circuits, 2022: https://www.protoexpress.com/blog/pcb-design-checklist-to-build-flawless-board/
- **ESP** Espressif, "PCB Layout Design – ESP32-P4", ESP Hardware Design Guidelines: https://docs.espressif.com/projects/esp-hardware-design-guidelines/en/latest/esp32p4/pcb-layout-design-esp32p4.html
- **TI-HS** TI SPRAAR7J, "High-Speed Interface Layout Guidelines", 2023: https://www.ti.com/lit/pdf/spraar7
- **TI-ETH** TI SNLA387, "Ethernet PHY PCB Design Layout Checklist", 2021: https://www.ti.com/lit/pdf/snla387
- **TI-BUCK** C. Glaser, "Five steps to a great PCB layout for a step-down converter", TI SLYT614, 2015: https://www.ti.com/lit/pdf/slyt614
- **TI-SMPS** TI SNVA021C, "AN-1149 Layout Guidelines for Switching Power Supplies", 2013: https://www.ti.com/lit/pdf/snva021
- **TI-BOOST** TI SLVA773, "Five Steps to a Good PCB Layout of a Boost Converter", 2016: https://www.ti.com/lit/pdf/slva773
- **TI-QFN** TI SLUA271C, "QFN and SON PCB Attachment", 2023: https://www.ti.com/lit/pdf/slua271
- **TI-EMI** TI SZZA009, "PCB Design Guidelines For Reduced EMI", 1999: https://www.ti.com/lit/pdf/szza009
- **JLC-SP** JLCPCB, "Minimum Spacing Requirements for SMD Components": https://jlcpcb.com/help/article/minimum-spacing-for-smd-components
- **JLC-TH** JLCPCB, "How to add tooling holes for PCB assembly order": https://jlcpcb.com/help/article/how-to-add-tooling-holes-for-pcb-assembly-order
- **JLC-FID** JLCPCB, "How to Add Edge Rails and Fiducials for PCB Assembly Orders": https://jlcpcb.com/help/article/how-to-add-edge-rails-fiducials-for-pcb-assembly-order
- **JLC-PE** JLCPCB, "Specifications for Adding Process Edges and Positioning Holes": https://jlcpcb.com/help/article/specifications-for-adding-process-edges-and-positioning-holes
- **JLC-POL** JLCPCB, "Component Polarity & Orientation Identification Guide": https://jlcpcb.com/help/article/component-polarity-and-orientation-identification-guide
- **MF** M. Schulte, "A Schematic Review Checklist for Firmware Engineers", Interrupt (Memfault), 2024: https://interrupt.memfault.com/blog/schematic-review-checklist
