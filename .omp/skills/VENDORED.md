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

## SolidCraft

These skills are copied unmodified from
[tangency-labs/solidcraft](https://github.com/tangency-labs/solidcraft) `skills/` at tag
`v0.25.1` (commit `37a586dd61d4873842604c2036c05a2890e7d773`). They're MIT OR
Apache-2.0; see `LICENSE-solidcraft` (MIT).

- `solidcraft-modeling`
- `solidcraft-editing`
- `solidcraft-checking`
- `solidcraft-mechanisms`
- `solidcraft-reporting`

They are the modelling skills for the `solidcraft` MCP server in `../mcp.json`. That server
comes from the `solidcraft` input in `flake.nix` and may only write under `hardware/lib`,
where the footprints' 3D models live. The contributor skills from the same folder are for
work on SolidCraft itself and are not copied. When you bump the flake input, re-copy these
skills from the matching tag.
