---
name: kicad-part-sourcing
description: Use when choosing or replacing a component for a KiCad board in this repo (finding LCSC parts, checking stock/price, comparing candidates, filling Manufacturer/MPN/LCSC fields).
---

# Sourcing parts

## Requirements first
Write down, before searching: function, electrical limits (voltage, current, power, tolerance, temperature), package/footprint constraints (what's already in the KiCad libraries counts), quantity per board and board count, assembly (JLCPCB basic/extended), and what the user excluded. Ask (`ask`) when a limit is a design decision.

## Search (fan out)
One `task` subagent per part (or per small group of passives). Each:
- searches LCSC (`web_search` to find candidates, then `kicad_query lcsc {codes: ["C…", …]}` for MPN, maker, package, stock, price breaks and datasheet link; quote its fetch date with stock and prices) and the manufacturer's site;
- checks every electrical requirement **in the datasheet** (`kicad_datasheet`; `url` may be the PDF URL or the LCSC code, e.g. `"C9196"`; `find` and `pages` for text, `page` + `box` for figures; cite document and page; numbers from distributor summaries are not evidence);
- checks the footprint: an existing project or stock KiCad footprint that matches the datasheet land pattern (`kicad_query lib_search` / `lib_footprint`: pads, pitch, corner radius; a `source: "user"` hit is the user's personal library, not usable as is); otherwise says a library part is needed (`kicad-library-part` skill). Never use EasyEDA/LCSC CAD models (their terms forbid it);
- returns 2–4 ranked candidates.

## Candidate table
| Rank | MPN | Manufacturer | LCSC | Stock | Price @qty | Package / footprint | Key specs (datasheet p.) | Notes |
Rank by: meets every requirement, then footprint already available, then stock depth, then price. Flag single-source and low-stock parts.

## After the user picks
Update the part with `kicad_edit` `set_fields` (`Value`, `Footprint`, `Manufacturer`, `MPN`, `LCSC`, `Datasheet`; `refs: [...]` for every part using it, found with `kicad_query symbols {fields: {LCSC: "<old code>"}}`), or with `change_symbol {ref, lib_id, fields}` when the new part needs another symbol (`kicad-schematic-edit` skill), and, if the footprint changes, run `kicad_check {checks: ["footprints", "fields"]}`. Update `SCHEMATIC.md` (and the board README parts list) in the same commit.
