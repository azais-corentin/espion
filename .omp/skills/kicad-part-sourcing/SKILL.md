---
name: kicad-part-sourcing
description: Use when choosing or replacing a component for a KiCad board in this repo (finding LCSC parts, checking stock/price, comparing candidates, filling Manufacturer/MPN/LCSC fields).
---

# Sourcing parts

## Requirements first
Write down, before searching: function, electrical limits (voltage, current, power, tolerance, temperature), package/footprint constraints (what's already in the KiCad libraries counts), quantity per board and board count, assembly (JLCPCB basic/extended), and what the user excluded. Ask (`ask`) when a limit is a design decision.

## Search (fan out)
One `task` subagent per part (or per small group of passives). Each:
- searches LCSC (`web_search`, then `read https://www.lcsc.com/product-detail/<Cnumber>.html` for stock, price breaks, package, datasheet link) and the manufacturer's site;
- checks every electrical requirement **in the datasheet** (`kicad_datasheet`; cite document and page; numbers from distributor summaries are not evidence);
- checks the footprint: an existing project or stock KiCad footprint that matches the datasheet land pattern (`kicad_query lib_search` / `lib_footprint`: pads, pitch); otherwise says a library part is needed (`kicad-library-part` skill). Never use EasyEDA/LCSC CAD models (their terms forbid it);
- returns 2–4 ranked candidates.

## Candidate table
| Rank | MPN | Manufacturer | LCSC | Stock | Price @qty | Package / footprint | Key specs (datasheet p.) | Notes |
Rank by: meets every requirement, then footprint already available, then stock depth, then price. Flag single-source and low-stock parts.

## After the user picks
Update the part with `kicad_edit` `set_fields` (`Value`, `Footprint`, `Manufacturer`, `MPN`, `LCSC`, `Datasheet`) and, if the footprint changes, run `kicad_check {checks: ["footprints", "fields"]}`. Update `SCHEMATIC.md` (and the board README parts list) in the same commit.
