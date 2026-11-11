---
name: kicad-schematic-edit
description: Use when changing anything in a KiCad project in this repo (schematic sheets, parts, wiring, labels, new sheets, net classes, design rules, BOM presets, board set-up: stack-up, board rules, outline, update from schematic). Board layout (placement, zones, routing, fab output) is kicad-pcb-layout. KiCad files are toolkit-only: kicad_query, kicad_edit, kicad_check, kicad_render.
---

# Editing a KiCad project

## Rules
- KiCad files are **toolkit-only for changes**. Never write `.kicad_*` files or lib tables with write/edit/eval/shell, and never run `kicad-cli`, `kicad`, `eeschema` or `pcbnew` yourself. A hook blocks the file tools and the shell; eval isn't checked, so the rule is yours to keep. Reading them is allowed, but the queries below answer better.
- If the toolkit can't do what the task needs, **stop**: tell the user exactly what is missing (which op or query, with which arguments), then use the `ask` tool to ask whether it should be added to the toolkit. Don't work around it.
- `kicad_edit` checks for lock files itself. It ignores a stale lock (this host and user, no KiCad running) and says so in its report; it refuses while KiCad runs or the lock belongs to another host or user. Then ask the user to close KiCad; never delete lock files.
- The KiCad files are the source of every value; the board's design docs (`docs/hardware/`) hold requirements, decisions, formulas, accepted risks, layout constraints, firmware rules and first-board checks, never part values, part numbers, measured board figures or results calculated from them (repo rule). A schematic change updates them in the same commit when it changes one of those, and the board `README.md` list when it adds or removes a block or changes an IC or module family. Connections and parts lists are not repeated there.

## Workflow
1. **Read first.** `kicad_query` with `hierarchy`, `sheet`, `symbol {ref}` (pins with positions, directions and nets), `symbols` (filter on any field with `fields: {LCSC: "C25744"}`, add field columns with `show: ["MPN", "LCSC"]`; never one `symbol` call per part), `net {name}`, `labels`, `wires {sheet, box}` (every segment with what sits at each end and along it, junctions, no-connects, dangling ends, buses and bus entries), `free_space`, and `next_ref {prefix, sheet, count}` for the references `add_symbol` will assign (so `expect` can name them). Look at the area with `kicad_render {sheet, refs}`. Don't guess pin positions or wire geometry from a render: take them from `symbol` and `wires`.
2. **Work the change out in numbers.** Every point on the 1.27 mm grid, wires orthogonal, each new wire end on a pin, wire, label or junction. Apply the drawing rules below while you plan, not afterwards.
3. **One `kicad_edit` per coherent change**, with `expect` stating the connectivity change exactly:
   - `added_parts` / `removed_parts`: references.
   - `nets`: every net whose pin set changes. A new or small net: `{name?, pins}` with the **complete** final pin list (`"R62.1"`). A net that existed before (GND, rails, big buses): `{name, added, removed}` relative to its pins before. Leaf names like `"VBUS_SENSE"` match `/MCU/VBUS_SENSE`.
   - `renamed_nets`: `{from, to}` for nets that only change name.
   - `unconnected`: pins of existing parts that end up on no net.
   - Omit `expect` for edits that must not change connectivity (moves, fields, cosmetics).
   Use `dry_run: true` for large edits and `render: true` to see the result. Every dry run and every NOT APPLIED report ends with the actual change as a JSON `expect` block. A good flow for a large edit: dry run, check that block line by line against what the edit is meant to do, then send the real run with the reviewed block.
4. **When it says NOT APPLIED**, nothing was written. Read every listed problem. If connectivity differs from `expect`, the drawing is wrong or the expectation is: find out which (render, `net`, `wires`, `symbol`) before changing either. An unintended pin move in the printed block is a bug to fix, not something to declare.
5. **After applying**: `kicad_check {checks: ["erc", "style", "connectivity", "decoupling", "pulls"], sheets: [<touched sheets>]}` (connectivity compares with git HEAD; it is compact by default, renames are counted not listed, `args: {detail: true}` lists everything), then update the docs where a requirement, formula, decision or accepted risk changed.
6. **Before finishing**: walk the items of the review checklist (`skill://kicad-schematic-review/checklist.md`) that the change touches, within the review skill's **Design assumptions** (no protection, margin or firmware rule for cases they exclude). For a new part that means at least IC-1…4, RST-3, the PWR/PASS items for its supply and parts, and BUS-5 for every signal it shares with another device. Recompute each value you changed or added from the formulas in the block's doc in `docs/hardware/` and check it meets the requirement there; report the results, don't write them in the doc. Update the doc when the change needs a new requirement, formula, decision or accepted risk. Fix what you find. An independent review of the change is `kicad-schematic-review` in `change` mode, run by another agent.

## Drawing rules (checked on every edit; new violations reject the edit)
- **R1 pin stub**: every wired pin leaves with at least 1.27 mm of straight wire in the pin's direction, with no junction, branch, bend or other connection inside that first 1.27 mm (exactly at 1.27 mm is fine).
- **R2 power symbols**: upright (rot 0). A power symbol serving one pin sits 1.27 mm straight out from a vertical pin, or 1.27 mm out then 1.27 mm up (rails) or down (GND) from a horizontal pin. Sideways only on a horizontal pin whose wired neighbour pin blocks the L, on a straight 1.27 mm wire.
- **R3 label underline**: a horizontal local label sits on a horizontal wire that runs under its whole text (text width ≈ 1.35 mm per character at 1.27 mm text; `labels` and `free_space` help). Labels on vertical wires are exempt.
- On the 1.27 mm grid, no overlapping symbols/fields, nothing outside the frame or on the title block, no dangling wire ends, no redundant junctions (a junction joining fewer than 3 connections is a warning).

## Op notes
- `add_symbol` places by symbol origin; fields are auto-placed on a pin-free side. Plan its wires from `kicad_query lib_symbol` (each pin's `at` offset and `dir` at rot 0; rotate them with the symbol). The edit report lists the placed pins' positions.
- `add_power {net, at}` puts the power pin exactly on `at`: put `at` at the end of the 1.27 mm stub.
- `add_wire` adds junctions where a wire end lands on another wire; points must be orthogonal.
- `remove_wire {from, to}` removes any stretch inside one segment and keeps the rest; `{at}` or the exact ends remove the whole segment. Junctions left joining fewer than 3 connections go automatically (ends and middle). Read the note's "left unconnected" list.
- `move_symbol` drags the wire ends and junctions sitting on its pins; a move that would make a wire diagonal is refused: move along the wire's axis or remove and redraw the wire.
- **A field text in the way** (a Value across a wire): `set_symbol_field {ref, field, at | offset, rot?, justify?, hidden?}` places that one text, as it looks on the sheet; `move_symbol fields: "auto"` re-places them all and may land one on a wire.
- **Swapping a part** (another symbol, same reference): `change_symbol {ref, lib_id, value?, footprint?, fields?}`, not remove + add. It keeps the reference, pose, attributes and the sourcing fields (Manufacturer, MPN, LCSC): set the new part's ones with `fields`. Nothing is rewired: the note lists the wire ends, labels and no-connects left off a pin and the new pins with nothing on them; rewire from that list in the same call.
- **After a library edit** (`lib_edit_symbol` on a placed part): placed symbols keep their cached copy until `change_symbol {ref}` (no `lib_id`) refreshes them; other symbols with that lib_id on the same sheet are redrawn too.
- **Same fields on many parts** (a board-wide part swap): one `set_fields {refs: [...], fields: {...}}`.
- **Buses**: a pin joins a bus member through `add_bus` (a branch, when needed) → `add_bus_entry {at: point on the bus, dir: UR|DR|UL|DL}` → `add_wire` from the entry's wire side → `add_label` with the member name (e.g. `IMU_SDA`) on that wire. Never end a wire directly on a bus. `remove_bus` works like `remove_wire`; `remove_bus_entry` takes either end. A bus ending on another bus's middle needs a junction.
- **Bus aliases** (the members a `{NAME}` bus carries, kept in the `.kicad_pro`): `kicad_query bus_aliases` lists each alias's members, where it is used and its `unused_members`; `set_bus_alias {name, members}` creates it or replaces its member list, `remove_bus_alias {name}` deletes an alias nothing names any more. A net taken off a bus (its label and bus entry removed) also leaves the alias: drop the member with `set_bus_alias` in the same call; a member still labelled on a wire from that bus is refused.
- **Design rules**: `set_rule {rule, comment}` with a comment saying why the rule exists (datasheet or standard reference); on replace, omitting `comment` keeps the old one and `""` removes it. The file's own header comment (the `#` block after `(version N)`, a blank line above the first rule's comment) is set with `set_rules_header {comment}`; `""` removes it. `kicad_query rules` shows the header and each rule's comment.
- **ERC/DRC exclusions** (made in KiCad, kept in the `.kicad_pro`): `kicad_query exclusions` lists each with rule, position, comment, the items it names and whether it still hides a violation (`hides`) or is `stale` (the design changed under it). `remove_exclusion {kind: "erc"|"drc", key | match, all?}` deletes them (`match`: text in the key or comment; one hit unless `all`); a violation it still hid shows again and is reported, not gated. `set_exclusion_comment {kind, key | match, comment}` gives an exclusion its reason (datasheet reference); the `erc` check warns about exclusions without one.
- New sheet: `create_sheet`, hierarchical labels in the child (`add_label kind: "hierarchical"`), then `add_sheet_pin` (or `sync_sheet_pins`) and wires on the parent, all in one call.
- References: omit `ref` (or end it with `?`) to annotate; power symbols follow the per-sheet `#PWR` blocks in `.omp/kicad.json`, and a new sheet gets the next free block automatically (no config edit needed).
- Library parts (new symbols/footprints) are made with the `kicad-library-part` skill, in a separate `kicad_edit` call before they're placed.
- **Board set-up**: the `board` ops of `kicad_edit` set the stack-up (`set_stackup`), board-wide rule minimums and DRC severities (`set_board_rules`), the outline and cutouts (`set_board_edges`, fillets per vertex), and bring footprints and nets over from the schematic (`update_from_schematic`, in its own call after a schematic change; new footprints land beside the outline; the copper of a renamed or merged net follows it, a split net's or orphaned copper is reported, not moved). Read with `kicad_query` `board`, `footprints`, `footprint`; check with `kicad_check {checks: ["drc"]}` (DRC + schematic parity); look with `kicad_render {board: true}`. A board edit is refused when it adds a DRC violation or a parity issue. Placement, rule areas, zones, routing, stitching, board text and fab output (`kicad_export`) follow the `kicad-pcb-layout` skill.

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
