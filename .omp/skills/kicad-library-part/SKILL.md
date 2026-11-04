---
name: kicad-library-part
description: Use when a board in this repo needs a symbol, footprint or 3D model that the stock KiCad libraries don't have (new project library part in hardware/lib).
---

# Adding a project library part

Read `hardware/lib/README.md` first: KLC naming, `${KIPRJMOD}`-relative lib tables, the Parts list, the expected KLC messages and the 3D-model rules.

## 1. Data from the datasheet
Pin table (number, name, electrical type per KLC S4.4, unit), package drawing and recommended land pattern (cite pages). `kicad_datasheet {url}` downloads the PDF once and returns its local path and a page index; `{url, pages: "4-18"}` returns the text of several pages (no images), `{url, find: "<regex>", pages?}` returns the matching lines with page numbers, and `{url, page, box?, dpi?}` returns one page's text and an image of the page or a crop (`box` as page fractions), so figures can be read at the resolution they need. Don't run pdftotext yourself. No EasyEDA/LCSC models, footprints or symbols, not even as a cross-check.

## 2. Look at what exists
`kicad_query lib_search` for similar stock parts, then `kicad_render {symbol: "Lib:Name"}` / `{footprint: "Lib:Name"}` to see them. `kicad_query lib_symbol` / `lib_footprint` with `graphics: true` return a part's drawings in the exact format the ops take, so a stock drawing can be read, adapted and written back. Every hit carries `source`: `project`, `stock` or `user`. A `user` part comes from the user's personal global library: it is not in the repo and not KLC-reviewed, so never place or copy one without a review. `lib_footprint` pads include `corner_radius` (with `roundrect_rratio`) and the mask/paste margins (per pad, and the footprint's `overrides`) for comparing with the land pattern.

## 3. Symbol (pick one route)
A new library needs `library_description` (what it holds, e.g. "TI power management ICs"; the toolkit appends "(espion project library)"). Change it later with `lib_set_library {kind, library, description}`.
- **Standard pin box:** `lib_create_symbol` (official kicad-library-tools CSV generator: KLC layout): library name by function per KLC (e.g. `Interface_Ethernet_<Maker>`), pins with side and type, Reference prefix, Footprint, Datasheet, Description, Keywords, fp_filters. Pins that latch a strap at reset carry it in the name (KLC S4.1).
- **Same pinout as a stock symbol:** `lib_copy_symbol {from, library, name, footprint?}`, then `lib_edit_symbol` for the datasheet, description, keywords, fp_filters and any pin changes.
- **Custom drawing** (magnetics, internal schematics, like a stock part's style): `lib_draw_symbol` with explicit `pins` and `shapes` (polyline, rectangle, circle, arc, bezier, text; sheet orientation, y down, mm from the origin, pins on the 2.54 mm grid). Start from a similar stock symbol's `lib_symbol graphics: true` output when one exists. Redraw an existing symbol with `lib_edit_symbol` `shapes` (replaces all graphics) and `pins` entries (move, add with `at` + `dir`, `remove`); Reference and Value are re-placed above and below the drawing after a shape change or pin move (`fields: "keep"` prevents it).
- **Several pads on one pin** (power pins, paralleled outputs): one pin numbered as a KiCad 10 stack, `"[1,8]"` or `"[1-3,5]"`. Never hidden duplicate pins at one point (legacy stacks fail KLC S4.3); when `lib_symbol` reports `legacy_stacks` on a stock part you start from, draw each as one pin numbered `native`.
- **Centre the body on the origin** (KLC S3.1) while you plan the pins: a body from −h to +h in y with pins on the 2.54 mm grid. A failed S3.1 says where the centre is in mm and the shift that centres it.
- **Before the review**, dry-run the placement where the part will go (`kicad_edit` `dry_run` with `add_symbol` or `change_symbol` and its wires, `render: true`): a pinout that crowds the neighbouring parts is cheaper to redraw before the reviewer has checked it.

## 4. Footprint (pick one route)
- **Standard package** (SON, QFN, SOIC, chip, connector family…): `lib_generate_footprint` with the official generator and a spec from the datasheet dimensions. Prefer this.
- **Close to a stock footprint** (same family, different pads): `lib_copy_footprint`, then one `lib_edit_footprint` with `pads` (select by number, or `{number, near}` when numbers repeat; set `number`, `at`, `size`, `drill`, … or `remove`), `add_pads`, `outline` (redraws fab/silk/courtyard around the new pads and body) and `graphics` for anything `outline` doesn't draw. Update the description (datasheet URL) and model.
- **Anything else** (modules, odd connectors): `lib_create_footprint` with the pad table from the land pattern and the body size; `graphics` replaces generated drawings on given layers.
- Footprints that need a 3D model: `model: true` (create) or `model` (edit) with `${KIPRJMOD}/../lib/<Library>.3dshapes/<Name>.step`; `model_offset`, `model_rotation` and `model_scale` place it (stock models usually need none).
- Every value comes from the datasheet (cite pages), not from the part you copied: check each pad against the land pattern, including corner radius and solder-mask margin when the land pattern gives them (pads take `corner_radius` and `solder_mask_margin` in mm, as datasheets state them).

## Licence
Parts copied or derived from KiCad's stock libraries keep KiCad's licence (CC-BY-SA 4.0 with the KiCad library exception); parts drawn from scratch are CERN-OHL-P-2.0. A drawing started from a stock part's `graphics: true` output is derived; one that only looked at a render for style is drawn from scratch. The copy ops say so in their notes; the Parts entry in `hardware/lib/README.md` must say which applies and name the stock source.

## 5. Checks
The edit is rejected when it adds KLC violations other than the expected ones in `.omp/kicad.json`. When a part breaks a rule on purpose because the real part differs (e.g. it has no pin 6), add a waiver for that part only, `klcWaivers: {"Lib:Name": {"S4.5": "<reason, datasheet page>"}}`, rerun the edit, and mention the waiver in your report; never widen `klcExpected` (it applies to every part). Use `render: true` on the `kicad_edit` call to see the new or changed parts, run `kicad_check {checks: ["klc"]}` afterwards to see all messages (its "checked N parts: …" line confirms your part was among them; "nothing checked" means no change was seen, use `args: {all: true}`), and read the part back with `kicad_query lib_symbol` / `lib_footprint`.

## 6. 3D model
Build it with SolidCraft (`solidcraft-modeling` skill) from the package drawing, save the script in `hardware/lib/3d-src/` and the STEP in `hardware/lib/<Library>.3dshapes/`, as `hardware/lib/README.md` describes. Check the alignment in KiCad with `kicad_render {footprint, view: "3d"}` from the top, the bottom and an angle: pins centred in their pads, pin 1 right, body over the F.Fab outline, nothing below the board for SMD; the caption says whether the model file was found.

## 7. Review
Have every new or changed part reviewed by an independent subagent with the `kicad-library-review` skill, and fix its findings, before the part is placed or committed.

## 8. Use it
Place the part with a separate `kicad_edit` call (`add_symbol`, or `change_symbol` when it replaces a placed part; `kicad-schematic-edit` skill). After editing a part that is already placed, refresh the placed copies with `change_symbol {ref}`. Add the Parts entry in `hardware/lib/README.md` in the same commit, and update the board's docs where the part changes a requirement, formula, decision or accepted risk (never its values or part numbers).
