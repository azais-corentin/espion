---
name: kicad-schematic-review
description: Use when reviewing or verifying a KiCad schematic in this repo (design review, checking a change, checking a sheet against datasheets or the design doc). Report-only unless the user asks for fixes.
---

# Reviewing a KiCad schematic

The items to verify are in [checklist.md](skill://kicad-schematic-review/checklist.md) (IDs such as `PWR-3`). This file is the process: modes, phases, who checks what, depth and output.

## Rules
- **Report-only** unless the user asks for fixes (then see [Fix loop](#fix-loop-only-when-asked)).
- **Independent.** A reviewer never reviews a change it made. If you made the change in this session, every review step goes to subagents.
- **Recompute, don't trust.** `SCHEMATIC.md` "Values and derivations", the author's report and earlier reviews are claims to check. Open the datasheet yourself.
- Never fetch EasyEDA/LCSC symbol, footprint or model data (AGENTS.md). Stock, price and second sources belong to `kicad-part-sourcing`; library parts to `kicad-library-review`.

## Mode
Ask (with `ask`) when the request doesn't settle it, together with the scope (whole board or some sheets, which topics to skip):

| Mode | Scope | Derivations | Budget |
|---|---|---|---|
| `change` | Parts and nets in the `connectivity` diff vs the baseline, plus every part on those nets | Recompute the ones the change touches | ≈ 15 calls per touched sheet |
| `standard` | Every sheet and topic | Spot-check: recompute at least the two that matter most per sheet | ≈ 30 per sheet, ≈ 30 per topic |
| `full` | Every sheet and topic, plus the production topic | Recompute all of them with worst-case tolerances and rail ranges; compare every block with its vendor reference design | ≈ 60 per sheet, ≈ 40 per topic |

Checklist items marked **(full)** run only in `full` mode.

## Phase 0: machine checks (one call)
`kicad_check` with no `checks` runs every default check:
- `erc`: violations; an exclusion without a reason is a warning;
- `connectivity`: vs git HEAD; pass `baseline` for another ref, `sheets` to scope it, `args: {detail: true}` for every rename and full pin lists (compact by default). In `change` mode, its diff defines the scope;
- `style`: drawing rules;
- `footprints`: footprints resolve; pins ↔ pads;
- `fields`: sourcing fields;
- `netclasses`;
- `rules`: `.kicad_dru` loads;
- `decoupling`: a capacitor to ground on the sheet for every IC rail;
- `pulls`: pull-ups on open-drain, I²C and MDIO nets, with both I²C lines on the same rail;
- `bom_lines`: MPN consistency and merge candidates.

`klc` runs only when named. These results are the baseline of the report: a reviewer reads the result for an item marked **Auto:** in the checklist, and doesn't redo it by hand.

## Phase 1: architecture (yourself)
Work through ARCH-1…4:
- the power tree with rail ranges;
- the list of external contacts;
- blocks vs `SCHEMATIC.md` and `docs/architecture.md`;
- one reference design or hardware design guide per major IC (datasheet application circuit, app note, Espressif hardware design guidelines for the P4).

Write the result to `local://review-architecture.md`. Every reviewer gets it. In `change` mode, cover only what the change touches: the rails it draws from or feeds, the external contacts it reaches, its blocks and the reference designs of its new or changed ICs.

## Phase 2: reviewers (fan out)
Spawn `task` subagents in one batch (default agent type, so the kicad_* tools are available; don't restrict their tools). In `change` mode, spawn one reviewer for the whole scope. It covers the sheet items of every sheet the change touches, plus the topic items that the change reaches: a rail, an external contact or an MCU pin. Spawn per-sheet reviewers only when the change spans more than two sheets.

| Reviewer | Checklist items |
|---|---|
| One per sheet | PWR-2, PWR-4, PWR-7; PASS; IC; RST-1, RST-2; BUS-1…4, BUS-6; HS; CLK; ANA; PROT-5; DOC-2; DOC-3 for its own `SCHEMATIC.md` section |
| Topic: power & protection | ARCH-1 checked in depth; PWR-1, PWR-3, PWR-5, PWR-6, PWR-8; PROT-1…4; EMC |
| Topic: firmware & bring-up | RST-3, RST-4; FW-1…5; BUS-1 addresses and recovery; BUS-5 across sheets |
| Topic: production (`full` only) | PROD |
| You, after the reviewers | DOC-1, DOC-3 across sheets (hierarchical pins, buses, net classes), DOC-4, DOC-5; merging |

Give each reviewer:
- the mode, its scope (sheet or topic), and its purpose from `SCHEMATIC.md`;
- the path of this skill's `checklist.md` and its item IDs;
- `local://review-architecture.md` and the phase 0 findings for its scope;
- the parts with their datasheet links (`kicad_query symbols` / `symbol` Datasheet fields);
- the Rules and Depth limits sections of this file, verbatim;
- the output format below.

Tools for reviewers: `kicad_query` (including `wires {sheet, box}` for wiring geometry), `kicad_check`, `kicad_render`, `kicad_datasheet` (`find`/`pages` for text, pages and crops as images, plus the local PDF path), `read`, `grep`, `web_search`. No edits.

## Depth limits (every reviewer, including subagents)
A review ends; it does not chase every number to the last digit.
- Keep to the mode's budget. When a fact would need more digging, record it under `unverified` with what to check and how, then move on.
- Datasheets: read the pin table, the application circuit, the abs max and recommended operating conditions, and the equations the design uses, through `kicad_datasheet` (crop a figure with `box` and a higher `dpi`). Don't extract PDF vectors or measure drawings pixel by pixel.
- Footprints: for stock footprints of ICs and connectors in scope (standard passive packages exempt; in `change` mode only new or changed footprints), compare `kicad_query lib_footprint` with the datasheet's land pattern. Project library parts are reviewed with `kicad-library-review`, not here.
- Finished when every item in scope is in `findings`, `passed`, `unverified` or `na`. Then return; don't keep refining. An item may sit in `passed` and in `findings` when part of it passed. `na` only holds items that are in no other list.

## Reviewer output
Return exactly one JSON object:
```json
{"scope": "sheet:Power | topic:power | topic:firmware | topic:production | change",
 "verdict": "approve | changes_requested",
 "findings": [{"severity": "blocker | major | minor", "item": "PWR-2", "where": "U5 / +3V3",
               "issue": "…", "evidence": "datasheet p. N table/figure, or tool output", "fix": "concrete change"}],
 "passed": [{"item": "PWR-1", "evidence": "…"}],
 "unverified": [{"item": "BUS-1", "what": "bus capacitance unknown", "how": "measure t_r on the first boards"}],
 "na": ["ANA-1"]}
```

Severity:
- **blocker**: the board won't work, or a part, the user or a connected device can be damaged;
- **major**: it works only in some conditions or outside a spec, or needs rework or a firmware workaround;
- **minor**: best practice, margins, documentation, readability.

Every finding has evidence and a concrete fix. Every datasheet number cites the document and page; every claim about the design cites a `kicad_query` / `kicad_check` result. A remark that needs no change is not a finding: put it in `passed` with its evidence. `approve` only when `findings` is empty.

## Merge and report (yourself)
1. Collect the JSON objects. Merge findings with the same `where` and `item`, keeping the strongest evidence and the highest severity.
2. Do your own items (DOC-1, DOC-3…5) and the cross-sheet checks: names, directions and levels match on both ends of every hierarchical and global net (`kicad_query net`).
3. Report:
   - a summary table: sheet/topic × blocker/major/minor/unverified;
   - coverage: items passed / with findings / unverified / na;
   - findings grouped by sheet/topic, worst first, one line each: `- [blocker|major|minor] ITEM <REF/net>: <issue> — evidence: … — fix: …`;
   - open questions for the user (the `unverified` entries that need a decision or a measurement).

   Put long material (the merged JSON, long derivations) in `local://` files and reference them.

## Fix loop (only when asked)
1. Give the findings the user accepts to an author subagent, which follows `kicad-schematic-edit`, updates `SCHEMATIC.md`, and reports its connectivity diff.
2. A fresh reviewer (never the author) checks only those findings and the fix's `connectivity` diff, in `change` mode, with this skill.
3. Repeat until it approves. After three rounds without approval, stop and ask the user.
