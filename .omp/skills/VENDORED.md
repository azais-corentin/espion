# Vendored skills

The skills in this directory are copied unmodified from
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
