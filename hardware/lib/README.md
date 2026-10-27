# Hardware library

Project-local KiCad libraries shared by the boards in `hardware/`. Not populated yet.

Planned layout:

| Path | Content |
|---|---|
| `espion.kicad_sym` | Symbols (one library file) |
| `espion.pretty/` | Footprints |
| `3dmodels/` | STEP models referenced by the footprints (`${KIPRJMOD}`-relative paths) |

Register the libraries in each project's `sym-lib-table` / `fp-lib-table` with
project-relative paths, so the project opens on any machine. Record where each part comes
from (datasheet, vendor model) and its license in this file.
