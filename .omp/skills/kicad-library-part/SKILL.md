---
name: kicad-library-part
description: Use when a board in this repo needs a symbol, footprint or 3D model that the stock KiCad libraries don't have (new project library part in hardware/lib).
---

# Adding a project library part

Read `hardware/lib/README.md` first: KLC naming, `${KIPRJMOD}`-relative lib tables, the Parts list, the expected KLC messages and the 3D-model rules.

## 1. Data from the datasheet
Pin table (number, name, electrical type per KLC S4.4, unit), package drawing and recommended land pattern (cite pages). `kicad_datasheet {url}` downloads the PDF once and returns its local path and a page index; `{url, page, box?, dpi?}` returns a page's text and an image of the page or a crop (`box` as page fractions), so figures can be read at the resolution they need. No EasyEDA/LCSC models, footprints or symbols, not even as a cross-check.

## 2. Look at what exists
`kicad_query lib_search` for similar stock parts, then `kicad_render {symbol: "Lib:Name"}` / `{footprint: "Lib:Name"}` to see them. `kicad_query lib_symbol` / `lib_footprint` with `graphics: true` return a part's drawings in the exact format the ops take, so a stock drawing can be read, adapted and written back.

## 3. Symbol (pick one route)
- **Standard pin box:** `lib_create_symbol` (official kicad-library-tools CSV generator: KLC layout): library name by function per KLC (e.g. `Interface_Ethernet_<Maker>`), pins with side and type, Reference prefix, Footprint, Datasheet, Description, Keywords, fp_filters. Pins that latch a strap at reset carry it in the name (KLC S4.1).
- **Same pinout as a stock symbol:** `lib_copy_symbol {from, library, name, footprint?}`, then `lib_edit_symbol` for the datasheet, description, keywords, fp_filters and any pin changes.
- **Custom drawing** (magnetics, internal schematics, like a stock part's style): `lib_draw_symbol` with explicit `pins` and `shapes` (polyline, rectangle, circle, arc, bezier, text; sheet orientation, y down, mm from the origin, pins on the 2.54 mm grid). Start from a similar stock symbol's `lib_symbol graphics: true` output when one exists. Redraw an existing symbol with `lib_edit_symbol` `shapes` (replaces all graphics) and `pins` entries (move, add with `at` + `dir`, `remove`).

## 4. Footprint (pick one route)
- **Standard package** (SON, QFN, SOIC, chip, connector family…): `lib_generate_footprint` with the official generator and a spec from the datasheet dimensions. Prefer this.
- **Close to a stock footprint** (same family, different pads): `lib_copy_footprint`, then one `lib_edit_footprint` with `pads` (select by number, or `{number, near}` when numbers repeat; set `number`, `at`, `size`, `drill`, … or `remove`), `add_pads`, `outline` (redraws fab/silk/courtyard around the new pads and body) and `graphics` for anything `outline` doesn't draw. Update the description (datasheet URL) and model.
- **Anything else** (modules, odd connectors): `lib_create_footprint` with the pad table from the land pattern and the body size; `graphics` replaces generated drawings on given layers.
- Footprints that need a 3D model: `model: true` (create) or `model` (edit) with `${KIPRJMOD}/../lib/<Library>.3dshapes/<Name>.step`; `model_offset`, `model_rotation` and `model_scale` place it (stock models usually need none).
- Every value comes from the datasheet (cite pages), not from the part you copied: check each pad against the land pattern.

## Licence
Parts copied or derived from KiCad's stock libraries keep KiCad's licence (CC-BY-SA 4.0 with the KiCad library exception); parts drawn from scratch are CERN-OHL-P-2.0. A drawing started from a stock part's `graphics: true` output is derived; one that only looked at a render for style is drawn from scratch. The copy ops say so in their notes; the Parts entry in `hardware/lib/README.md` must say which applies and name the stock source.

## 5. Checks
The edit is rejected when it adds KLC violations other than the expected ones in `.omp/kicad.json`. When a part breaks a rule on purpose because the real part differs (e.g. it has no pin 6), add a waiver for that part only, `klcWaivers: {"Lib:Name": {"S4.5": "<reason, datasheet page>"}}`, rerun the edit, and mention the waiver in your report; never widen `klcExpected` (it applies to every part). Use `render: true` on the `kicad_edit` call to see the new or changed parts, run `kicad_check {checks: ["klc"]}` afterwards to see all messages, and read the part back with `kicad_query lib_symbol` / `lib_footprint`.

## 6. 3D model
Build it with SolidCraft (`solidcraft-modeling` skill) from the package drawing, save the script in `hardware/lib/3d-src/` and the STEP in `hardware/lib/<Library>.3dshapes/`, as `hardware/lib/README.md` describes. Check the alignment in KiCad with `kicad_render {footprint, view: "3d"}` from the top, the bottom and an angle: pins centred in their pads, pin 1 right, body over the F.Fab outline, nothing below the board for SMD; the caption says whether the model file was found.

## 7. Review
Have every new or changed part reviewed by an independent subagent with the `kicad-library-review` skill, and fix its findings, before the part is placed or committed.

## 8. Use it
Place the part with a separate `kicad_edit` call (`add_symbol`, `kicad-schematic-edit` skill). Add the Parts entry in `hardware/lib/README.md` and update the board's docs in the same commit.
