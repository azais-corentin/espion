---
name: kicad-schematic-review
description: Use when reviewing or verifying a KiCad schematic in this repo (design review, checking a change, checking a sheet against datasheets or the design doc). Report-only unless the user asks for fixes.
---

# Reviewing a KiCad schematic

## Scope first
Ask (with `ask`) what to verify and what to skip when the request is open-ended: whole board or some sheets, electrical design vs drawing style, datasheet checks, doc conformance (`SCHEMATIC.md`), BOM/sourcing. A review is **report-only** unless the user asks for fixes; fixes then follow the `kicad-schematic-edit` skill.

## Machine checks (one call)
`kicad_check` with no `checks` runs every default check: `erc`, `connectivity` (vs git HEAD; pass `baseline` for another ref), `style` (drawing rules), `footprints` (footprints resolve, pins ↔ pads), `fields` (sourcing fields), `netclasses`, `rules` (.kicad_dru loads). `klc` (library parts) runs when named. Use these results as the baseline of the report; don't repeat by hand what a check proves.

## Per-sheet review (fan out)
For more than two sheets, spawn one `task` subagent per sheet (default agent type, so the kicad_* tools are available; don't restrict their tools). Give each:
- the sheet name and its purpose from `SCHEMATIC.md`,
- the parts on it with datasheet links (from `kicad_query symbols` / `symbol` Datasheet fields),
- what to verify (below) and the findings format,
- that it may only use `kicad_query`, `kicad_check`, `kicad_render`, `kicad_datasheet` (datasheet pages and crops as images, plus the local PDF path), `read`, `web_search`; no edits,
- the depth limits below, verbatim.

What a sheet reviewer verifies:
1. Every IC pin against the datasheet pin table: function, required pull-ups/downs, strapping, decoupling (value, count, placement near the pin it serves), absolute maximum ratings vs the nets on it.
2. Power: input range, output setting resistors (compute the result), inductor/capacitor ratings, current paths, enable/sequence, protection.
3. Signals: direction and level of every net leaving the sheet (hierarchical labels), pairs (`_P/_N`) and their net class, termination.
4. Parts: value/footprint/rating fit, DNP intent, sourcing fields.
5. Drawing: readability problems the `style` check can't see (crowding, misleading placement); look with `kicad_render`.

## Depth limits (every reviewer, including subagents)
A review ends; it does not chase every number to the last digit.
- Budget about 30 tool calls per sheet. When a fact would need more digging, write the finding as `[unverified]` with what to check and move on; it goes under open questions.
- Datasheets: read the pin table, the application circuit, the absolute maximum ratings and the package drawing pages through `kicad_datasheet` (crop a figure with `box` and a higher `dpi` instead of scripting). Don't extract PDF vectors or measure drawings pixel by pixel.
- Footprints: compare `kicad_query lib_footprint` (pads, sizes, positions) with the datasheet's recommended land pattern. Never fetch EasyEDA/LCSC footprint or symbol data (AGENTS.md: their terms forbid using it). Library parts themselves are reviewed with the `kicad-library-review` skill.
- Second sources, price and stock belong to the `kicad-part-sourcing` skill, not to a review, unless the user asked for them.
- Finished when every item of "What a sheet reviewer verifies" has a finding, a pass with evidence, or an `[unverified]` entry. Then report; don't keep refining.

## Findings format
One line per finding, grouped by sheet, worst first:
`- [error|warning|note] <REF/net>: <what is wrong> — evidence: <tool output or datasheet page, table/figure> — fix: <concrete change>`
- Every number from a datasheet cites the document and page. Every claim about the design cites a `kicad_query` / `kicad_check` result. Unverified → say so (`[unverified]`).
- No finding without evidence; no style nits disguised as errors.

## Cross-sheet pass (yourself, after the subagents)
- Hierarchical/global nets that cross sheets: names, directions, levels match on both sides (`kicad_query net`).
- Total power budget vs the supply, rail sequencing.
- `SCHEMATIC.md` vs reality: buses, hierarchical pins, parts, values (`kicad_query hierarchy`, `symbols`, `netclasses`). List every mismatch; the doc or the schematic is wrong, say which you think.

## Report
Summary table (sheet × errors/warnings/notes), then the findings, then open questions for the user. Write longer findings to `local://` files and reference them.
