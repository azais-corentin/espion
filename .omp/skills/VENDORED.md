# Vendored skills

The skills in this directory are copied unmodified from
[oaslananka/kicad-mcp-pro](https://github.com/oaslananka/kicad-mcp-pro) `skills/` at tag
`mcp-server-v4.4.0` (commit `285be423edd2d16c799f4cb20b60858a4a729010`). They're
MIT-licensed; see `LICENSE-kicad-mcp-pro`.

- `kicad-design-review`
- `pcb-design`
- `drc-check`
- `schematic-review`
- `fabrication-output`
- `wired-subcircuit-design`

They match the `kicad` MCP server pinned in `../mcp.json`. When you bump that version,
re-copy these skills from the matching tag.

The server runs with the `default` profile in `readonly` mode, so skill steps that call
write, routing or manufacturing tools fail with a mode error. That's intended:
`hardware/` files are only changed through KiCad (see `AGENTS.md`).
