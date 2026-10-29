---
name: kicad-schematic-edit
description: Use when changing anything in a KiCad project in this repo (schematic sheets, parts, wiring, labels, new sheets, net classes, design rules, BOM presets). KiCad files are toolkit-only: kicad_query, kicad_edit, kicad_check, kicad_render.
---

# Editing a KiCad project

## Rules
- KiCad files are **toolkit-only**. Never edit `.kicad_*` files or lib tables with write/edit/eval/shell, and never run `kicad-cli`, `kicad`, `eeschema` or `pcbnew` yourself. A hook blocks it.
- If the toolkit can't do what the task needs, **stop**: tell the user exactly what is missing (which op or query, with which arguments), then use the `ask` tool to ask whether it should be added to the toolkit. Don't work around it.
- `kicad_edit` refuses while KiCad has the project open (lock files). Ask the user to close KiCad; don't delete lock files.
- A schematic change updates `hardware/<board>/SCHEMATIC.md` in the same commit (repo rule), and `README.md`/`BOM` notes when a part changes.

## Workflow
1. **Read first.** `kicad_query` with `hierarchy`, `sheet`, `symbol {ref}` (pins with positions, directions and nets), `net {name}`, `labels`, `free_space`. Look at the area with `kicad_render {sheet, refs}`. Don't guess pin positions: take them from `symbol`.
2. **Work the change out in numbers.** Every point on the 1.27 mm grid, wires orthogonal, each new wire end on a pin, wire, label or junction. Apply the drawing rules below while you plan, not afterwards.
3. **One `kicad_edit` per coherent change**, with `expect` stating the connectivity change exactly:
   - `added_parts` / `removed_parts`: references.
   - `nets`: every net whose pin set changes. A new or small net: `{name?, pins}` with the **complete** final pin list (`"R62.1"`). A net that existed before (GND, rails, big buses): `{name, added, removed}` relative to its pins before. Leaf names like `"VBUS_SENSE"` match `/MCU/VBUS_SENSE`.
   - `renamed_nets`: `{from, to}` for nets that only change name.
   - `unconnected`: pins of existing parts that end up on no net.
   - Omit `expect` for edits that must not change connectivity (moves, fields, cosmetics).
   Use `dry_run: true` for large edits and `render: true` to see the result.
4. **When it says NOT APPLIED**, nothing was written. Read every listed problem. If connectivity differs from `expect`, the drawing is wrong or the expectation is: find out which (render, `net`, `symbol`) before changing either. Never copy the actual result into `expect` without understanding it.
5. **After applying**: `kicad_check {checks: ["erc", "style", "connectivity"]}` (connectivity compares with git HEAD), then update the docs.

## Drawing rules (checked on every edit; new violations reject the edit)
- **R1 pin stub**: every wired pin leaves with at least 1.27 mm of straight wire in the pin's direction, with no junction, branch, bend or other connection inside that first 1.27 mm (exactly at 1.27 mm is fine).
- **R2 power symbols**: upright (rot 0). A power symbol serving one pin sits 1.27 mm straight out from a vertical pin, or 1.27 mm out then 1.27 mm up (rails) or down (GND) from a horizontal pin. Sideways only on a horizontal pin whose wired neighbour pin blocks the L, on a straight 1.27 mm wire.
- **R3 label underline**: a horizontal local label sits on a horizontal wire that runs under its whole text (text width ≈ 1.35 mm per character at 1.27 mm text; `labels` and `free_space` help). Labels on vertical wires are exempt.
- On the 1.27 mm grid, no overlapping symbols/fields, nothing outside the frame or on the title block, no dangling wire ends.

## Op notes
- `add_symbol` places by symbol origin; fields are auto-placed on a pin-free side. Plan its wires from `kicad_query lib_symbol` (each pin's `at` offset and `dir` at rot 0; rotate them with the symbol). The edit report lists the placed pins' positions.
- `add_power {net, at}` puts the power pin exactly on `at`: put `at` at the end of the 1.27 mm stub.
- `add_wire` adds junctions where a wire end lands on another wire; points must be orthogonal.
- `move_symbol` drags the wire ends sitting on its pins; a move that would make a wire diagonal is refused: move along the wire's axis or remove and redraw the wire.
- New sheet: `create_sheet`, hierarchical labels in the child (`add_label kind: "hierarchical"`), then `add_sheet_pin` (or `sync_sheet_pins`) and wires on the parent, all in one call.
- References: omit `ref` (or end it with `?`) to annotate; power symbols follow the per-sheet `#PWR` blocks in `.omp/kicad.json`, and a new sheet gets the next free block automatically (no config edit needed).
- Library parts (new symbols/footprints) are made with the `kicad-library-part` skill, in a separate `kicad_edit` call before they're placed.

## Example: a new sheet with two resistors on a net and on GND
This exact call passes every gate on the espion-cam project (it is the toolkit's transaction test). `Device:R` pins are at (0, −3.81) "U" and (0, 3.81) "D" from the origin.
```json
{"ops": [
  {"op": "create_sheet", "parent": "root", "name": "Test", "file": "test.kicad_sch", "at": [137.16, 144.78], "size": [25.4, 10.16]},
  {"op": "add_symbol", "sheet": "Test", "lib_id": "Device:R", "at": [101.6, 101.6], "value": "10k"},
  {"op": "add_symbol", "sheet": "Test", "lib_id": "Device:R", "at": [111.76, 101.6], "value": "10k"},
  {"op": "add_wire", "sheet": "Test", "points": [[101.6, 97.79], [101.6, 95.25], [111.76, 95.25], [111.76, 97.79]]},
  {"op": "add_label", "sheet": "Test", "name": "A", "at": [104.14, 95.25]},
  {"op": "add_wire", "sheet": "Test", "points": [[101.6, 105.41], [101.6, 106.68]]},
  {"op": "add_wire", "sheet": "Test", "points": [[111.76, 105.41], [111.76, 106.68]]},
  {"op": "add_power", "sheet": "Test", "net": "GND", "at": [101.6, 106.68]},
  {"op": "add_power", "sheet": "Test", "net": "GND", "at": [111.76, 106.68]}
 ],
 "expect": {"added_parts": ["R66", "R67"],
            "nets": [{"name": "A", "pins": ["R66.1", "R67.1"]}, {"name": "GND", "added": ["R66.2", "R67.2"]}]}}
```
