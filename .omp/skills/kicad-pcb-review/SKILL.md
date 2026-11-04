---
name: kicad-pcb-review
description: Use when reviewing or verifying a KiCad board layout in this repo (design review, checking a placement or routing change, checking a region against datasheets or docs/hardware/layout.md, a fab release check). Report-only unless the user asks for fixes.
---

# Reviewing a KiCad board layout

The items to verify are in [checklist.md](skill://kicad-pcb-review/checklist.md) (IDs such as `RET-2`), with the renders to look at (views `V1`…`V10`) and the fab release items. This file is the process: modes, phases, who checks what, depth and output.

## Rules
- **Report-only.** The review edits nothing: no `kicad_edit`, no change to `.omp/kicad.json`, `docs/` or any KiCad file. The one file-writing call is `kicad_export` in the fab release items (REL), which writes only the gitignored `fab/` outputs. Fixes only when the user asks (then see [Fix loop](#fix-loop-only-when-asked)).
- **Independent.** A reviewer never reviews a change it made. If you made the change in this session, every review step goes to subagents.
- **Recompute, don't trust.** `docs/hardware/layout.md`, the author's report and earlier reviews are claims to check. Open the datasheet yourself.
- **Numbers come from queries, not pictures.** A render shows where to look; every distance, length, width or area in a finding comes from `kicad_query` (`distance`, `lengths`, `copper`, `zones`, `routing`, `footprints`) or a check.
- The design intent is `docs/hardware/layout.md` (per block, with its sources); the rules it describes are in the net classes, the `.kicad_dru` and the project's `layout` section of `.omp/kicad.json`.
- Circuits, values and ratings belong to `kicad-schematic-review`; library parts (symbols, footprints, 3D models in `hardware/lib/`) to `kicad-library-review`; stock, price and second sources to `kicad-part-sourcing`. Never fetch EasyEDA/LCSC symbol, footprint or model data (AGENTS.md).

## Design assumptions (not findings)
A finding is something that fails to build, to assemble or to work in normal operation with the fab and every part inside their published tolerances, or a written claim that is wrong. The review takes the following as given. Don't report against them, don't spend calls proving them, and don't list them as `unverified`:
- **The fab meets its published capabilities.** JLCPCB holds its stack-up, its ±10 % impedance tolerance and the board-setup minimums (their sources in `layout.md`, Fabrication rules). The stack-up's Dk and thicknesses are JLCPCB's data; the impedance targets and method in `layout.md` (Impedance) rest on them.
- **The rules are the contract for what they cover.** DRC, `track_widths`, `return_path`, `via_return`, `decoupling_placement` and `layout` prove the board against the net classes, the `.kicad_dru` and the `layout` config. The review checks those rules against their sources (GEN-3…5, ISO-1) and judges what no rule covers; it doesn't re-derive each spacing by hand.
- **The schematic is reviewed.** Connections, part values and ratings are as the schematic says; a layout finding is about where parts and copper sit and how they connect. DRC parity proves the board matches the schematic.
- **The project's own accessories and enclosure work.** The camera module, the FFC, the pigtail and the 3D-printed enclosure match the dimensions `layout.md` and `camera.md` give. The enclosure model is not in the project: fit beyond those dimensions is the user's check on the STEP export (REL-4), not a finding.
- **Guidelines are the bar, not simulation.** CSI-2, USB 2.0 HS and 100BASE-TX are judged against the vendor layout guidelines (Espressif, TI, the PHY's datasheet) and the datasheets' layout examples. No field solver, crosstalk, eye or thermal simulation; IPC-2152 widths and thermal figures worked out from the formulas in `layout.md` and the block docs are estimates, and a margin that holds at the estimate is not a finding.
- **Worst case = one tolerance at a time.** Etched width ±10 % or Dk as published, not every fab tolerance stacked at once.
- **JLCPCB standard PCBA.** Parts with an LCSC field are machine-placed; the parts in `assembly.hand` (`.omp/kicad.json`; reasons in `layout.md`, Assembly) are soldered by hand after reflow.

A concern these assumptions exclude stays out of the report even when its fix looks cheap. The accepted risks and open points in `layout.md` (e.g. library footprints that break the board minimums "to settle at placement") also stand unless the review shows them wrong.

## Mode
Ask (with `ask`) when the request doesn't settle it, together with the scope (whole board or some blocks or nets, which groups to skip, whether to run the fab release items):

| Mode | Scope | Derivations | Budget |
|---|---|---|---|
| `change` | The region, parts or nets the user names. For a change: the parts and nets in the `connectivity` diff vs the baseline plus every part within ≈ 5 mm of them (`footprints`, `distance`). A copper-only change doesn't show in that diff: ask which region or nets changed unless the request says | Recompute the ones the change touches | ≈ 20 calls per region |
| `standard` | Every block and topic, all groups but REL | Spot-check: at least the two that matter most per block (a hot loop, a current width, an impedance from the stack-up) | ≈ 30 per block, ≈ 30 per topic |
| `full` | Every block and topic, the **(full)** items and REL | Recompute all of them; compare every regulator's layout with its datasheet example | ≈ 50 per block, ≈ 40 per topic |

Checklist items marked **(full)** run only in `full` mode. REL runs in `full` mode or when the user asks for a release check.

**Stage.** `kicad_query board` (footprints inside the outline, `unrouted`) and `routing` (unrouted total) tell whether the board is placed, routed or partly routed. On a board not routed yet, the items that judge copper (tracks, vias, pours) are `na`; the report says the stage once. Placement items still apply, including routing room for the pairs and the hot loops.

## Phase 0: machine checks (one call)
`kicad_check` with `checks: ["drc", "track_widths", "return_path", "via_return", "decoupling_placement", "placement", "layout", "netclasses", "rules"]`:
- `drc`: KiCad DRC with zones refilled in memory and schematic parity; exclusions are info with their comment; unrouted connections are one info finding with their count (`args: {unconnected: true}` lists each);
- `track_widths`: tracks under their class width, diff pairs coupled at the wrong gap;
- `return_path`: impedance-class tracks without a ground fill on the adjacent layer;
- `via_return`: impedance-class signal vias without a ground via within `viaReturnMaxDistance`;
- `decoupling_placement`: the nearest decoupling capacitor per power pin within `decouplingMaxDistance`, and a ground via or fill at its ground pad;
- `placement`: pads outside the outline or in a cutout, courtyards in footprint keep-outs, counts of placed, bottom-side, locked and unplaced footprints;
- `layout`: the project's `layout` config (near, lengthGroups, paths, viaParity, copperArea, heightZones), each finding quoting its `why`; a ref, pad, net or layer the board doesn't have is an error (stale config);
- `netclasses`, `rules`: diff pairs in their classes, the `.kicad_dru` loads.

The check docs (`kicad_check` tool description) give the exact semantics and thresholds. These results are the baseline of the report: a reviewer reads the result for an item marked **Auto:** in the checklist and doesn't redo it by hand.

## Phase 1: board brief (yourself)
Write `local://review-board.md` from `kicad_query` `board`, `stats`, `netclasses`, `rules`, `footprints` and `layout.md`:
- the stage; board size, outline, cutouts, layers and stack-up;
- the blocks: per schematic sheet, its parts (`footprints {sheet}`) with their courtyard bbox, which gives each reviewer its `box`;
- the net classes and custom rules with their values, the impedance classes, and the `layout` config entries (`.omp/kicad.json`, project `layout`);
- the phase 0 findings, grouped by block and topic;
- the datasheet with a layout section for each regulator, PHY and the P4 module (Espressif PCB layout guidelines).

Then do GEN and STK yourself. In `change` mode, cover only what the change touches.

## Phase 2: reviewers (fan out)
Spawn `task` subagents in one batch (default agent type, so the kicad_* tools are available; don't restrict their tools). In `change` mode, spawn one reviewer for the whole scope: it covers every item that applies to the parts and nets in scope.

| Reviewer | Checklist items |
|---|---|
| One per block (schematic sheet; LED ring joins LED driver, IMU joins MCU) | FTP; PLC-3…7; DEC; PWR (Power and LED driver); THM; TST; GEN-6 for the block's section of `layout.md` |
| Topic: high-speed & return paths | IMP; RET; EMC |
| Topic: isolation & mechanical | ISO; MEC; PLC-1, PLC-2, PLC-8, PLC-9 |
| Topic: production | DFM; DFA; SLK |
| You, before and after the reviewers | GEN; STK; REL (`full` or when asked); merging |

Give each reviewer:
- the mode, the stage, its scope (block with its parts and `box`, or topic) and the block's section of `layout.md`;
- the path of this skill's `checklist.md`, its item IDs and the views (V1…V10) that serve them;
- `local://review-board.md` and the phase 0 findings for its scope;
- the parts with their datasheet links (`kicad_query symbols` Datasheet field);
- the Rules, Design assumptions and Depth limits sections of this file, verbatim;
- the output format below.

Tools for reviewers: `kicad_query` (board: `board`, `footprints`, `footprint`, `copper`, `routing`, `lengths`, `zones`, `distance`, `board_free_space`, `board_graphics`, `stats`; project: `netclasses`, `rules`, `net`, `symbols`, `lib_footprint`), `kicad_check`, `kicad_render`, `kicad_datasheet` (`find`/`pages` for text, pages and crops as images), `read`, `grep`, `web_search`. No edits, no `kicad_export`.

## Depth limits (every reviewer, including subagents)
A review ends; it does not chase every number to the last digit.
- Keep to the mode's budget. When a fact would need more digging, record it under `unverified` with what to check and how, then move on.
- Datasheets: read the layout guidelines and layout example, the land pattern, the thermal data (R_θJA and the copper it assumes) and the pins the layout cares about, through `kicad_datasheet` (crop a figure with `box` and a higher `dpi`). Don't extract PDF vectors or measure drawings pixel by pixel.
- Footprints: for stock footprints of ICs and connectors in scope (standard passive packages exempt; in `change` mode only new or changed footprints), compare `kicad_query lib_footprint` with the datasheet's land pattern. Project library parts are reviewed with `kicad-library-review`, not here.
- Renders: one overview of the scope, then a `box` per spot. Don't re-render to measure; query.
- Finished when every item in scope is in `findings`, `passed`, `unverified` or `na`. Then return; don't keep refining. An item may sit in `passed` and in `findings` when part of it passed. `na` only holds items that are in no other list.

## Reviewer output
Return exactly one JSON object:
```json
{"scope": "block:Power | topic:high-speed | topic:isolation-mechanical | topic:production | change",
 "verdict": "approve | changes_requested",
 "findings": [{"severity": "blocker | major | minor", "item": "PWR-1", "where": "C10, U3 pin 4 (131.2, 118.4)",
               "issue": "…", "evidence": "datasheet p. N figure, or tool output", "fix": "concrete change"}],
 "passed": [{"item": "RET-1", "evidence": "…"}],
 "unverified": [{"item": "STK-2", "what": "CSI CLK guard gap not routed yet", "how": "lengths and distance once routed"}],
 "na": ["FTP-2"]}
```
`where` names the references, pads or nets and a board position in mm.

Severity:
- **blocker**: the board can't be made or assembled as ordered, won't work, or a part, the user or a connected device can be damaged (short or open, wrong footprint or orientation, broken isolation gap, a part in the lens view or colliding with the enclosure);
- **major**: it fails or runs out of spec in some normal operating condition (judged by [Design assumptions](#design-assumptions-not-findings)), needs rework, or the fab or assembler would stop and ask (a pair off its impedance or over a broken reference, a hot loop or a part hotter than its rating, a refused export, a part left out of the assembly by mistake);
- **minor**: a wrong or missing statement in `layout.md`, silkscreen, or a best-practice deviation with a concrete consequence. A thin margin is minor only when it fails at the worst case the Design assumptions define.

Every finding has evidence and a concrete fix. Every datasheet number cites the document and page; every claim about the board cites a `kicad_query` / `kicad_check` result. A remark that needs no change is not a finding: put it in `passed` with its evidence. `approve` only when `findings` is empty.

## Merge and report (yourself)
1. Collect the JSON objects. Drop findings that rest on a case the [Design assumptions](#design-assumptions-not-findings) exclude (and their `unverified` entries). Merge findings with the same `where` and `item`, keeping the strongest evidence and the highest severity.
2. Do your own items (GEN, STK, and REL when it runs) and the cross-block checks: nets that cross block boundaries (CSI from U6 to J4 past the LED ring, RMII between U1 and U6, power feeds) were judged by one reviewer, not dropped between two.
3. Report:
   - the stage, and a summary table: block/topic × blocker/major/minor/unverified;
   - coverage: items passed / with findings / unverified / na;
   - findings grouped by block/topic, worst first, one line each: `- [blocker|major|minor] ITEM <REF/net @ x,y>: <issue> — evidence: … — fix: …`;
   - open questions for the user (the `unverified` entries that need a decision, a measurement or a JLCPCB preview).

   Put long material (the merged JSON, long derivations) in `local://` files and reference them.

## Fix loop (only when asked)
1. Give the findings the user accepts to an author subagent, which follows `kicad-pcb-layout` (and `kicad-schematic-edit` only when a finding needs a schematic change), updates `docs/hardware/layout.md` and the `layout` config when a constraint changes, and reports what it moved or rerouted (references, nets, region).
2. A fresh reviewer (never the author) checks only those findings and the changed region, in `change` mode, with this skill.
3. Repeat until it approves. After three rounds without approval, stop and ask the user.
