/**
 * KiCad toolkit for omp: read, edit and review KiCad projects without opening KiCad.
 *
 * Tools: kicad_query (read), kicad_edit (verified transactional writes), kicad_check (review),
 * kicad_render (sheet, board and library part images), kicad_datasheet (part datasheet pages),
 * kicad_export (fabrication outputs for JLCPCB). A tool_call hook keeps agents from editing KiCad
 * files or running KiCad programs any other way.
 * See .omp/skills/kicad-* for the workflows.
 */
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import { rasterizeSvg } from "@oh-my-pi/pi-natives";
import { findRepoRoot } from "./src/config";
import { type DatasheetRequest, readDatasheet } from "./src/datasheet";
import { compactJson } from "./src/format";
import { type ExportRequest, OUTPUTS, runExport } from "./src/export";
import { blockReason } from "./src/hook";
import { checksDoc, opsDoc, queriesDoc } from "./src/registry";
import { DEFAULT_BOARD_LAYERS, DEFAULT_FP_LAYERS, SIDES } from "./src/render";
import { type CheckRequest, CONVENTIONS, type QueryRequest, type RenderToolRequest, runChecks, runQuery, runRender } from "./src/tools";
import { type EditRequest, runEdit } from "./src/txn";

function errorText(e: unknown): string {
	return e instanceof Error ? e.message : String(e);
}

function text(t: string, isError = false) {
	return { content: [{ type: "text" as const, text: t }], isError };
}

export default function kicadToolkit(pi: ExtensionAPI) {
	const z = pi.zod;
	pi.setLabel("KiCad toolkit");

	pi.on("tool_call", event => {
		const reason = blockReason(event.toolName, (event.input ?? {}) as Record<string, unknown>);
		return reason ? { block: true, reason } : undefined;
	});

	const project = z
		.string()
		.optional()
		.describe("KiCad project: .kicad_pro path or its directory, repo-relative. Optional when .omp/kicad.json lists one project.");

	pi.registerTool({
		name: "kicad_query",
		label: "KiCad query",
		description: [
			"Read a KiCad project: hierarchy, symbols, pins, nets, labels, net classes, rules, BOM and libraries; the board (stack-up, board rules, outline, footprints and their pads, copper: tracks, vias, zones, routing state, lengths, distances, free space, drawings, statistics); and LCSC's live part data (stock, prices). Read-only.",
			"Answers come from KiCad itself (kicad-cli netlist) and the schematic and board files; `lcsc` from lcsc.com.",
			CONVENTIONS,
			"Queries (`what`, with `args`):",
			queriesDoc(),
		].join("\n\n"),
		parameters: z.object({
			project,
			what: z.string().describe("Query name, see the list in the description."),
			args: z.record(z.string(), z.unknown()).optional().describe("Arguments of the query."),
		}),
		async execute(_id, params, signal, _onUpdate, ctx) {
			// omp validated params against `parameters`; the zod-like schema doesn't carry the static type
			const req = params as QueryRequest;
			try {
				const out = await runQuery(findRepoRoot(ctx.cwd), req, signal);
				return text(typeof out === "string" ? out : compactJson(out));
			} catch (e) {
				return text(errorText(e), true);
			}
		},
	});

	pi.registerTool({
		name: "kicad_edit",
		label: "KiCad edit",
		description: [
			"Change a KiCad project in one verified transaction: schematic sheets, hierarchy (new sheets, sheet pins), project settings (net classes, BOM presets), design rules, project libraries and the board (stack-up, board rules and setup, outline, footprints from the schematic, placement, tracks, diff pairs, vias and stitching vias, zones, rule areas, board texts, drawings and board-only footprints).",
			"The ops run in memory, then the result is written to a temporary copy, re-saved by KiCad (kicad-cli upgrade) and checked there. The real files are only replaced when every check passes:",
			'- the connectivity change equals `expect` exactly (default: no connectivity change at all). Declare every net whose pins change, either as its exact final pin list (`{name?, pins: ["R1.2", …]}`) or, for a net that existed before, as a delta (`{name: "GND", added: ["R66.2"], removed: []}`); plus added/removed parts, renamed nets and pins left on no net;',
			"- no new ERC violation; no new drawing-style finding on the touched sheets; a changed rules file parses; changed library parts add no KLC violation;",
			"- a changed board (KiCad refills its zones and the fills are kept in the file): no new KiCad DRC violation and no new schematic parity issue (counted like ERC, without coordinates). Not gated, but reported: routing in progress (unconnected items, dangling tracks and vias), violations inside footprints the edit put on the board from the libraries (the library footprint against the board's rules), and violations a board-rules-only change brings up.",
			"Otherwise nothing is written and the report says why. Refuses while KiCad has the project open (lock files).",
			"Every dry run and every refused (NOT APPLIED) result ends with the actual connectivity change as a JSON `expect` in exactly this input format. Read it against what the edit is meant to do before passing it back: pasting it blindly declares whatever the ops did, including a pin they moved by mistake.",
			"Library changes (new symbols/footprints/libraries) and their use on a sheet need separate kicad_edit calls; so do schematic changes and update_from_schematic.",
			CONVENTIONS,
			"Ops (each item of `ops` is {op: name, ...fields}):",
			opsDoc(),
		].join("\n\n"),
		parameters: z.object({
			project,
			ops: z.array(z.object({ op: z.string() }).passthrough()).describe("Edit ops, applied in order."),
			expect: z
				.object({
					added_parts: z.array(z.string()).optional(),
					removed_parts: z.array(z.string()).optional(),
					renamed_nets: z.array(z.object({ from: z.string(), to: z.string() })).optional(),
					nets: z
						.array(
							z.object({
								name: z.string().optional(),
								pins: z.array(z.string()).optional(),
								added: z.array(z.string()).optional(),
								removed: z.array(z.string()).optional(),
							}),
						)
						.optional()
						.describe('Every net whose pin set changes: {name?, pins} with the exact final pins ("R1.2"), or {name, added?, removed?} relative to the net before the edit.'),
					unconnected: z.array(z.string()).optional().describe("Pins of existing parts that end up on no net."),
				})
				.optional()
				.describe("The connectivity change this edit is meant to make. Omit for edits that must not change connectivity."),
			dry_run: z.boolean().optional().describe("Run every check but write nothing."),
			render: z
				.boolean()
				.optional()
				.describe("Return images of the touched sheet regions, of every library symbol (all units) and footprint the ops created or changed, and of the touched board region (outline, copper, courtyards), each captioned."),
		}),
		async execute(_id, params, signal, onUpdate, ctx) {
			// omp validated params against `parameters`; the zod-like schema doesn't carry the static type
			const req = params as EditRequest;
			try {
				onUpdate?.({ content: [{ type: "text", text: "Applying ops and checking the result with KiCad…" }] });
				const r = await runEdit(req, findRepoRoot(ctx.cwd), req.render ? rasterizeSvg : undefined, signal);
				return {
					content: [
						{ type: "text" as const, text: r.report },
						...r.images.flatMap(i => [
							{ type: "text" as const, text: i.label },
							{ type: "image" as const, data: Buffer.from(i.png).toString("base64"), mimeType: "image/png" },
						]),
					],
					isError: !r.applied && !req.dry_run,
				};
			} catch (e) {
				return text(`NOT APPLIED: ${errorText(e)}`, true);
			}
		},
	});

	pi.registerTool({
		name: "kicad_check",
		label: "KiCad check",
		description: [
			"Review a KiCad project: the schematic (ERC, drawing rules, connectivity, parts, circuit checks), design rules, library parts (KLC) and the board (DRC with schematic parity, track widths, return paths, return vias, decoupling and footprint placement, the layout constraints of .omp/kicad.json). Runs the named checks (default: all checks marked default) and lists findings with severity, sheet and position (sheet mm; board findings in board mm).",
			CONVENTIONS,
			"Checks:",
			checksDoc(),
		].join("\n\n"),
		parameters: z.object({
			project,
			checks: z.array(z.string()).optional().describe("Checks to run; default: every default check."),
			sheets: z.array(z.string()).optional().describe("Limit sheet-level checks to these sheets."),
			baseline: z.string().optional().describe("Git ref for checks that compare (connectivity, klc). Default HEAD."),
			args: z.record(z.string(), z.unknown()).optional().describe("Extra check options (see check docs)."),
		}),
		async execute(_id, params, signal, _onUpdate, ctx) {
			// omp validated params against `parameters`; the zod-like schema doesn't carry the static type
			const req = params as CheckRequest;
			try {
				return text((await runChecks(findRepoRoot(ctx.cwd), req, signal)).report);
			} catch (e) {
				return text(errorText(e), true);
			}
		},
	});

	pi.registerTool({
		name: "kicad_render",
		label: "KiCad render",
		description: [
			"Render a schematic sheet (or a region of it), the board, a library symbol or a library footprint as a PNG image (KiCad's own plot), or a footprint or the board in 3D with the 3D models. Give exactly one of `sheet`, `board`, `symbol`, `footprint`. Use it to look at a change, or at a stock part you derive a new one from.",
			'Parts are "Lib:Name" from the project\'s libraries or KiCad\'s stock ones, drawn in KiCad\'s built-in colours. Symbol coordinates are mm from the symbol origin, x right, y down (as lib_symbol reports pins and shapes); without `unit` every unit is drawn, in a grid in reading order. Footprint coordinates are KiCad\'s (mm, y down); by default copper, front silkscreen, courtyard and fab layers are drawn, with pad outlines and pad numbers sketched on the fab layers. The answer names the region shown, so a following `box` can zoom in.',
			`The board (\`board: true\`) is drawn in board mm as seen from the top: by default its outline (plus a margin, or every footprint when there is no outline) with ${DEFAULT_BOARD_LAYERS.top.join(", ")} (\`side: "bottom"\`: the B.* analogues), pad outlines and numbers sketched on the fab layers; \`box\` frames any region, e.g. the footprints update_from_schematic placed beside the outline. \`layers: ["copper"]\` draws the outline and every copper layer, back to front. \`nets\` draws those nets' pads, tracks, arcs, vias, copper drawings and zone fills on the drawn copper layers in yellow over the plot; \`ratsnest: true\` draws the unrouted connections (of \`nets\`, or of every net) as thin white lines between the pads' copper centres.`,
			'`view: "3d"` places a footprint at the origin of a scratch 1.6 mm board (courtyard + 1 mm), or takes the board as it is, and raytraces it with KiCad\'s 3D viewer, to check that 3D models line up with the pads and silkscreen: look from `side`, turn the board with `rotate` ([-45, 0, 45] is isometric), `zoom` in. For a footprint the answer lists every model path, where it resolves and whether the file exists: a MISSING model draws nothing.',
			CONVENTIONS,
		].join("\n\n"),
		parameters: z.object({
			project,
			sheet: z.string().optional().describe('Sheet name, file or "root".'),
			board: z.boolean().optional().describe("true: the project's board."),
			symbol: z.string().optional().describe('Library symbol "Lib:Name" (project or stock library).'),
			footprint: z.string().optional().describe('Library footprint "Lib:Name" (project or stock library).'),
			refs: z.array(z.string()).optional().describe("Sheets: frame these references (plus margin)."),
			unit: z.number().int().optional().describe("Symbols: draw only this unit (default every unit)."),
			body_style: z.number().int().optional().describe("Symbols: 1, or 2 for the De Morgan body style (default 1)."),
			layers: z
				.array(z.string())
				.optional()
				.describe(
					`Footprints and the board: layers to draw, bottom first, e.g. ["F.Cu", "F.Fab"] (footprint default ${DEFAULT_FP_LAYERS.join(", ")}). Pad numbers show on the fab layers. Board: "copper" stands for Edge.Cuts and every copper layer, back to front.`,
				),
			box: z
				.array(z.number())
				.optional()
				.describe("Region [x0, y0, x1, y1] in sheet mm, board mm, or part mm for symbols (applied to each unit) and footprints. Default: the whole sheet, the board outline, or the part's drawn extent."),
			margin: z.number().optional().describe("Margin in mm around refs/box on sheets (default 8), around box or the default extent for the board (default 3) and parts (default 2)."),
			size: z.number().optional().describe("Longest image side in pixels (default 1600; 3D images are square)."),
			view: z.enum(["2d", "3d"]).optional().describe('Footprints and the board: "3d" for a raytraced image with the 3D models (default "2d").'),
			side: z.enum(SIDES).optional().describe('3D: side to look from (default "top"). 2D board: "bottom" draws the back layers by default (default "top").'),
			rotate: z.array(z.number()).optional().describe("3D: rotate the board [x, y, z] degrees before looking, e.g. [-45, 0, 45] for isometric."),
			zoom: z.number().optional().describe("3D: camera zoom; 1 spans about the board's longest side (default 0.85, or 0.7 with rotate: the whole board with a border)."),
			perspective: z.boolean().optional().describe("3D: perspective instead of orthographic projection."),
			nets: z.array(z.string()).optional().describe('2D board: highlight these nets\' copper (pads, tracks, arcs, vias, zone fills), e.g. ["GND", "/Power/VBUS"].'),
			ratsnest: z.boolean().optional().describe("2D board: draw the unrouted connections (of `nets`, or of every net) as thin lines."),
		}),
		async execute(_id, params, signal, _onUpdate, ctx) {
			// omp validated params against `parameters`; the zod-like schema doesn't carry the static type
			const req = params as RenderToolRequest;
			try {
				const img = await runRender(findRepoRoot(ctx.cwd), req, rasterizeSvg, signal);
				return {
					content: [
						{ type: "text" as const, text: img.text },
						{ type: "image" as const, data: Buffer.from(img.png).toString("base64"), mimeType: "image/png" },
					],
				};
			} catch (e) {
				return text(errorText(e), true);
			}
		},
	});

	pi.registerTool({
		name: "kicad_datasheet",
		label: "KiCad datasheet",
		description: [
			"Read a part's datasheet PDF (from a URL, e.g. a symbol's Datasheet field, an LCSC code, or a local path). Downloads once into the user cache (outside the repo) and refuses non-PDF answers. An LCSC code (\"C9196\") or an lcsc.com datasheet/product page resolves to the part's PDF through LCSC's product data (network).",
			"Without `page`, `pages` or `find`: the local PDF path, page count, title and the first lines of every page (find the pinout, package drawing or land pattern page). With `page`: that page's text and an image of the page, or of `box` (fractions of the page, y down) to zoom into a drawing. With `pages` (\"4-18\", \"4,6,10-12\"): the text of those pages, no images. With `find` (regex, case-insensitive): every matching line as `p<page>:<line>:` with a line of context around it, on all pages or on `pages`. The local path can also be read directly.",
		].join("\n\n"),
		parameters: z.object({
			url: z.string().describe('http(s) URL of the PDF, an LCSC code ("C9196") or lcsc.com datasheet/product page URL, or a local PDF path (repo-relative or absolute).'),
			page: z.number().int().optional().describe("Page number, 1-based: its text and image. Omit for the page index."),
			pages: z.string().optional().describe('Text only of these pages: "4-18" or "4,6,10-12". With `find`: search only these pages.'),
			find: z.string().optional().describe("Regex (case-insensitive): matching lines with page and line numbers and one line of context, at most 100 matches."),
			dpi: z.number().optional().describe("Render resolution (default 200, max 600); lowered when the image would exceed `size`."),
			box: z.array(z.number()).optional().describe("Crop [x0, y0, x1, y1] as fractions of the page (0..1, y down). Default: the whole page."),
			size: z.number().int().optional().describe("Longest image side in pixels (default 2000)."),
		}),
		async execute(_id, params, signal, _onUpdate, ctx) {
			// omp validated params against `parameters`; the zod-like schema doesn't carry the static type
			const req = params as DatasheetRequest;
			try {
				const r = await readDatasheet(findRepoRoot(ctx.cwd), req, signal);
				return {
					content: [
						{ type: "text" as const, text: r.text },
						...(r.png ? [{ type: "image" as const, data: Buffer.from(r.png).toString("base64"), mimeType: "image/png" }] : []),
					],
				};
			} catch (e) {
				return text(errorText(e), true);
			}
		},
	});

	pi.registerTool({
		name: "kicad_export",
		label: "KiCad export",
		description: [
			'Write the board\'s fabrication outputs for JLCPCB into `<project dir>/fab/<rev>-<YYYY-MM-DD>/` (gitignored): `rev` as given, else the board\'s title block revision, else "draft". Files of the same name are replaced.',
			"Outputs (`outputs`):",
			'- "gerbers": Gerbers of every copper layer, F/B.Paste, F/B.SilkS, F/B.Mask and Edge.Cuts with JLCPCB\'s recommended KiCad settings (Protel extensions, zones refilled before plotting, solder mask subtracted from silkscreen, reference and value texts plotted, absolute origin), Excellon drill files (mm, decimal, oval holes in the alternate mode, plated and non-plated holes in separate files) and a PDF drill map, in `gerbers/`, all zipped into `<board>-gerbers.zip`: upload that one.',
			'- "assembly": `<board>-bom.csv` (Comment, Designator, Footprint, JLCPCB Part #; one row per LCSC part) and `<board>-cpl.csv` (Designator, Mid X and Mid Y in mm with y up as in the Gerbers, Layer Top/Bottom, Rotation counter-clockwise; a bottom part gets 180° − its board rotation). Both hold the parts with an LCSC field that are fitted, not assembled by hand, in the BOM and on the board; the answer lists every part left out and why (DNP, hand assembly with its reason, excluded from the BOM, no LCSC field, not on the board, excluded from position files) and the board-only footprints. With hand-assembly parts also `<board>-hand-assembly.csv`: one row per fitted part of `assembly.hand` (Designator, Value, MPN, Footprint Lib:Name, Layer Top/Bottom, Mid X and Mid Y in mm in the CPL\'s frame, Rotation as pcbnew shows it, Reason), positioned from KiCad\'s position file, or from the board file for a footprint excluded from position files; listed references that are DNP or not on the board are named in the answer.',
			'- "step": `<board>.step`, the board with its footprints\' 3D models (no model substitution; `${KIPRJMOD}` is the project directory); the answer names every model file that is missing.',
			'- "drawings": assembly drawings `<board>-assembly-top.svg`, and `<board>-assembly-bottom.svg` only when footprints sit on the bottom (mirrored left to right, seen from below, as KiCad\'s mirrored plot): KiCad\'s plot of Edge.Cuts and the side\'s Fab layer (references, pads sketched, DNP parts crossed out) in black and its Silkscreen in grey on white, framed on the outline; each hand-assembly part\'s courtyard filled translucent orange; a legend with the board, side, revision, date and the hand-assembly parts of that side with their reasons. Each also as a PDF of the same size (`.pdf`, rasterised at 300 dpi by the dev shell\'s ImageMagick; SVG only without it). Refused when the board has no outline.',
			"Gate: gerbers and assembly are written only when KiCad's DRC, with zones refilled in memory and the schematic parity test, finds no error-severity violation (exclusions made in KiCad are honoured), no parity issue and no unconnected item, and ERC finds no error; assembly also needs the fields check without errors. Otherwise that output is refused with what failed (counts and the first findings) and the others are still written. step and drawings are never gated.",
			'Placement corrections for the CPL: `assembly.corrections` in .omp/kicad.json (in `defaults` or the project), {"<Lib:Name pattern>": {rot?: degrees counter-clockwise, offset?: [x, y] mm}}: `*`/`?` patterns on the footprint id, case-insensitive, first match wins (the project\'s entries first). rot is added to the CPL rotation; offset is where JLCPCB\'s part centre sits in the footprint\'s own frame (x right, y down, as the footprint editor shows it), turned with the footprint onto the board. Every placed footprint that matches no entry is listed as not confirmed in JLCPCB\'s placement preview; an entry {"rot": 0} records one that was checked and needs no correction.',
			'Hand assembly: `assembly.hand` in .omp/kicad.json (in `defaults` or the project; the project\'s entries over the defaults\'), {"<reference>": "<reason>"}: those parts are left out of the BOM and CPL (reason "hand assembly (assembly.hand): <reason>", before any other but DNP and not on the board), listed in `<board>-hand-assembly.csv` and highlighted in the drawings. A malformed entry fails loading the config, naming it; kicad_check placement flags references the board lacks.',
			"The answer also gives the board size, copper layers and footprint and part counts.",
		].join("\n\n"),
		parameters: z.object({
			project,
			outputs: z.array(z.enum(OUTPUTS)).describe('Outputs to write: "gerbers", "assembly", "step", "drawings".'),
			rev: z.string().optional().describe("Revision for the directory name; default the board's title block revision, else \"draft\"."),
		}),
		async execute(_id, params, signal, onUpdate, ctx) {
			// omp validated params against `parameters`; the zod-like schema doesn't carry the static type
			const req = params as ExportRequest;
			try {
				onUpdate?.({ content: [{ type: "text", text: "Checking the board with KiCad and writing the outputs…" }] });
				const r = await runExport(findRepoRoot(ctx.cwd), req, signal);
				return text(r.text, r.refused.length > 0);
			} catch (e) {
				return text(errorText(e), true);
			}
		},
	});
}
