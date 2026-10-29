---
name: kicad-library-review
description: Use when reviewing a project library part in this repo (symbol, footprint or 3D model in hardware/lib), new, changed or existing: independent check against the datasheet, KLC, symbol↔footprint mapping and 3D alignment. Report-only.
---

# Reviewing a library part

A review is **independent and read-only**: never the agent that made the part, never edits. Open the datasheet yourself and recompute every number; the author's report is a claim to check, not evidence. Fixes go back to an author (`kicad-library-part` skill).

## Inputs
- The part ids (`Lib:Name` of the symbol, footprint, model path) and the board that uses them.
- The datasheet: the symbol's Datasheet field / the footprint description (`kicad_query lib_symbol` / `lib_footprint`), or the Parts entry in `hardware/lib/README.md`.
- For a derived part, its stock source (copy-op note, README Parts entry).
- The author's report, when there is one.

## Checklist
1. **Datasheet.** `kicad_datasheet {url}` gives the local PDF path and a page index; `{url, page, box, dpi}` renders the pin table, package drawing and land pattern as images. Cite pages.
2. **Symbol** (`kicad_query lib_symbol {lib_id, graphics: true}`, `kicad_render {symbol}`): every pin number, name and electrical type against the pin table; units; strap functions in pin names (KLC S4.1); hidden/no-connect pins; readable graphics.
3. **Footprint** (`kicad_query lib_footprint {fp_id, graphics: true}`, `kicad_render {footprint}`): every pad number, position, size, shape and drill against the land pattern; pin 1 location and marking; F.Fab body outline; silk clear of copper; courtyard and its clearance; attributes (smd / through_hole).
4. **Derived parts:** `kicad_query lib_diff {a: "<stock source>", b: "<part>"}`. Every difference must be backed by the datasheet, and everything the datasheet says differs must appear in the diff.
5. **Symbol ↔ footprint:** `kicad_query lib_fp_filter {lib_id}`: the default footprint matches the filters, and pin numbers map 1:1 onto pad numbers (pin count vs distinct pads).
6. **3D model** (when the footprint has one): `kicad_render {footprint, view: "3d"}` from the top, the bottom and an angle: pins/terminals centred in their pads, pin 1 on the right side, body over the F.Fab outline, nothing below the board for SMD, the caption shows the model file exists; offset/rotation/scale are deliberate; the envelope matches the drawing. A SolidCraft model's script in `hardware/lib/3d-src/` must rebuild it (`solidcraft-checking` skill).
7. **KLC:** `kicad_check {checks: ["klc"]}`. Waivers in `.omp/kicad.json` (`klcWaivers`) are per part with a datasheet reason; `klcExpected` only holds rules every part may break. Also review what the checker can't see: field contents, naming (KLC S2/F2), Description, keywords, footprint filters, pin-1 marks.
8. **Provenance and licence:** the Parts entry names the sources and the licence (derived from stock = CC-BY-SA 4.0 with the KiCad library exception; drawn for the project = CERN-OHL-P-2.0). No EasyEDA/LCSC symbol, footprint or model data was used, not even as a cross-check.
9. **Provisional items** are marked in the part (Description / descr) and are truly unresolvable from the documents; if you can resolve one, say how.

## Depth limits
Budget about 40 tool calls per part. When a fact needs more digging, record it as `[unverified]` with what to check. Datasheets through `kicad_datasheet` only (no hand-rolled PDF tooling). Finished when every checklist item has a finding, a pass with evidence, or an `[unverified]` entry.

## Verdict
```json
{"verdict": "approve | changes_requested",
 "findings": [{"severity": "blocker | major | minor", "part": "Lib:Name", "issue": "…", "evidence": "datasheet p. N / tool output", "fix": "concrete change"}],
 "notes": "what was checked and passed, [unverified] items"}
```
`approve` only when no finding needs a change; non-actionable remarks go in `notes`. Every finding has evidence and a concrete fix.

## Review loop (when you orchestrate)
Spawn one reviewer subagent per part (default agent type, so the kicad tools are available) with this skill, the inputs above and the author's report. On `changes_requested`, give the findings to an author (`kicad-library-part`), then review again in a new round with a fresh reviewer. After three rounds without approval, stop and ask the user.
