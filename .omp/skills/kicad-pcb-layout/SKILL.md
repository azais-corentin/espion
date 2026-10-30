---
name: kicad-pcb-layout
description: Use when laying out the board of a KiCad project in this repo: placement, rule areas, zones, routing, stitching, board text, fab output (kicad_export). KiCad files are toolkit-only: kicad_query, kicad_edit, kicad_check, kicad_render, kicad_export.
---

# Laying out a KiCad board

## Rules
- KiCad files are **toolkit-only for changes**. Never write `.kicad_*` files with write/edit/eval/shell, and never run `kicad-cli`, `kicad` or `pcbnew` yourself. A hook blocks the file tools and the shell; eval isn't checked, so the rule is yours to keep. If the toolkit can't do what the layout needs, **stop**: tell the user exactly what is missing (which op or query, with which arguments), then use the `ask` tool to ask whether it should be added. Don't work around it.
- `kicad_edit` refuses while KiCad has the project open. Ask the user to close it; never delete lock files.
- The user may route or tune by hand in pcbnew between toolkit edits. After any pause, or when the board's counts differ from what you expect, **read the board again** (`board`, `routing`, `copper {box}`) before the next edit. Never edit from an old read.
- `docs/hardware/layout.md` is the design intent: board, stack-up, net classes, custom rules, impedance, and per block what to place near what and how to route it. Read the sections for the block you work on before you start it.
- The machine-checked constraints are in `.omp/kicad.json` under `layout` (`near`, `lengthGroups`, `paths`, `viaParity`, `copperArea`, `heightZones`, `decouplingMaxDistance`, `viaReturnMaxDistance`, `impedanceClasses`; every entry has a `why`). A change to a constraint updates `layout.md` and that `layout` section in the same commit. Net classes and custom rules are changed with `set_netclass`, `set_netclass_patterns` and `set_rule` (see `kicad-schematic-edit`), and `layout.md` with them.
- Board edits don't change the schematic's connectivity: omit `expect`.

## How the gate treats a board edit
Every board edit is re-saved by KiCad, every zone is refilled and the fills are kept in the file. Then KiCad's DRC with schematic parity runs on the result.
- **Refused** (NOT APPLIED, nothing written): a new DRC violation (clearance, custom rule, keepout, courtyard overlap, track crossing) or a new parity issue. Fix the cause; don't loosen a rule to get an edit through.
- **Reported, not gated**: unconnected items and dangling tracks or vias (routing in progress), violations inside footprints the edit placed from the libraries, and those a board-rules-only change brings up. A route can be built in several edits: a track with one free end is fine until the next edit lands on it.
- `dry_run: true` runs every check and writes nothing. `render: true` returns the touched board region (outline, copper, courtyards) with the nets the copper ops touched highlighted. Use both for any route you haven't drawn before, then send the same ops for real.

## Workflow
1. **Read first.**
   - `kicad_query board` (stack-up, rules, outline, copper counts, `zones_filled`, unrouted total), `netclasses`, `rules`, `zones`.
   - `footprints` (`{inside: false}`: not placed yet; `{sheet}`: one block) and `footprint {ref}` (pads with net, board position, size and layers).
   - `routing` (no args: one row per net with length, vias, clusters, unrouted; `{net}` or `{class}`: the clusters and the unrouted pad pairs with their positions).
   - Look with `kicad_render {board: true, box, ratsnest: true}` (`nets` highlights nets, `layers: ["copper"]` draws every copper layer, `side: "bottom"` the back).
   - Take pad positions from `footprint` and `copper`, never from a render.
2. **Read the region before each edit.** `copper {box}` (tracks, arcs, vias, zones there; add `net`, `layer` or `kind` to filter), `routing {net}`, `lengths {nets}` or `lengths {paths: [{from, to}]}`, and `distance {a, b}` (pad, footprint, point or `{net}`; edge to edge and centre to centre) for the gaps that matter.
3. **Placement.** Locked parts were placed at set-up: leave them, or ask before moving one (`place_footprint` moves it only with `locked: false`). Place the parts with constraints first (`heightZones`, `near`, the block rules in `layout.md`), then decoupling capacitors next to their power pins, then the rest. `place_footprint {ref, at, rot?, side?, locked?}`, several in one call. `board_free_space {near, size, side?}` finds room. Tidy reference texts with `set_footprint_field`. Check with `kicad_check {checks: ["placement", "decoupling_placement", "layout"]}` and a ratsnest render: short, uncrossed ratsnest lines mean an easier route.
4. **Rule areas.** `add_rule_area {name, layers, outline, forbid}` for areas to keep free (cable notch, antenna, isolation gaps); `forbid` from `tracks`, `vias`, `pads`, `copperpour`, `footprints`. `set_rule_area` and `remove_rule_area` change and delete them. The gate refuses anything forbidden inside one.
5. **Planes and pours.** `add_zone {net, layers, outline, name, priority?, …}`. `outline: "board"` with `margin` follows the board edge. A higher `priority` fills first: give a local pour inside a plane a higher priority than the plane. `set_zone` changes one; `remove_zone` deletes it. Fills need no manual step: the gate refills them. Planes go in before the critical routes, because `return_path` checks those against the ground plane under them.
6. **Critical routes first**, in the order `layout.md` gives:
   - diff pairs of the pair classes with `add_diff_pair` (width and gap default to the class's);
   - clocks and guarded tracks;
   - switching and power loops.
   When a pair or impedance track changes layer, put a GND via within `viaReturnMaxDistance` of each signal via (`add_via {at, net: "GND"}`).
7. **The rest.** `add_track {layer, from?, to?, points?}` (pads as `"U6.75"`; width defaults to the class's) and `add_via {at}` (the net comes from the copper there). `remove_copper` (by `at`, `box`, `net` or `from`+`to`) and `set_track_width` fix what is there.
8. **Stitching and return vias.** `stitch_vias {net: "GND", region, pitch}` fills a region with a grid of vias; `{along: {net, offset}, pitch}` puts rows on both sides of a net's tracks. Its `clearance` defaults to the net class's clearance and doesn't know the custom rules: next to a net with a custom spacing rule, give that spacing, or the gate refuses the whole op. The note says how many were placed and why the others were skipped.
9. **Board text and setup.**
   - `add_board_text` (layer `F.SilkS`), `add_board_graphic`, `remove_board_item`.
   - `add_board_footprint` and `remove_board_footprint` for fiducials, logos and test pads.
   - `set_board_setup {title, rev, date}`: `kicad_export` names its folder after `rev`. It also sets the solder mask, paste, via tenting and teardrops.
10. **Checks.** `kicad_check {}` runs every default check. For the board:
    - `drc`: DRC with parity; unconnected items are one count, `args: {unconnected: true}` lists them;
    - `track_widths`, `return_path`, `via_return`, `decoupling_placement`, `placement`;
    - `layout`: the `.omp/kicad.json` constraints.
    Every warning is fixed, or written in `layout.md` as an accepted risk with its reason.
11. **Review.** Another agent reviews the layout with the `kicad-pcb-review` skill. Fix what it finds.
12. **Fab output.** `kicad_export {outputs: ["gerbers", "assembly", "step"], rev?}` writes into `fab/` (gitignored). It refuses gerbers and assembly while DRC has an error, parity an issue, any item is unconnected, or ERC has an error. Each footprint the answer lists as not confirmed in JLCPCB's placement preview needs an entry in `assembly.corrections` in `.omp/kicad.json` once checked.

## Length tuning
Meanders are drawn by the user in pcbnew; the toolkit doesn't draw them. Measure with `lengths` (per net, the diff-pair skew, `paths` from pad to pad). Check with `kicad_check {checks: ["layout"]}` (`lengthGroups`, `paths`) and `drc` (skew rules in the `.kicad_dru`). `copper` shows tracks that belong to a tuning pattern. `remove_copper` removes a pattern only with all of its tracks.

## Example: a CSI lane from the FFC connector to its 0 Ω links, with GND return vias
`/Camera/CAM_D0_P` runs J4.3 → R47.2 and `/Camera/CAM_D0_N` runs J4.2 → R51.2 (`kicad_query nets {class: "MIPI_CSI"}`). The coordinates here are made up: take yours from `footprint {ref}` and `copper {box}`.
1. Read `routing {class: "MIPI_CSI"}`, then `copper {box}` around the route.
2. Dry-run the pair:
```json
{"ops": [
  {"op": "add_diff_pair", "p": "/Camera/CAM_D0_P", "n": "/Camera/CAM_D0_N", "layer": "F.Cu",
   "from": ["J4.3", "J4.2"], "to": ["R47.2", "R51.2"],
   "points": [[162.5, 176.0], [162.5, 170.0], [160.0, 167.5], [160.0, 160.0]]}
 ],
 "dry_run": true, "render": true}
```
   The tracks run at the class's 0.18 mm width and 0.15 mm gap on each side of the centreline. P takes the side nearer J4.3, and the joints are mitred.
3. Send the pair for real.
4. Stitch GND return vias along both tracks. Use an 0.8 mm offset, and the 0.36 mm `MIPI_CSI spacing` rule as clearance:
```json
{"ops": [
  {"op": "stitch_vias", "net": "GND", "along": {"net": "/Camera/CAM_D0_P", "offset": 0.8}, "pitch": 2, "clearance": 0.36},
  {"op": "stitch_vias", "net": "GND", "along": {"net": "/Camera/CAM_D0_N", "offset": 0.8}, "pitch": 2, "clearance": 0.36}
 ],
 "render": true}
```
   The rows between the two tracks are too close to the partner and are skipped. Each note says how many vias were placed.
5. Check: `lengths {nets: ["/Camera/CAM_D0_P", "/Camera/CAM_D0_N"]}` for the skew, then `kicad_check {checks: ["drc", "track_widths", "return_path", "via_return", "layout"]}`.
