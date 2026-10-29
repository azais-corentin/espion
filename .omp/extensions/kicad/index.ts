/**
 * KiCad toolkit for omp: read, edit and review KiCad projects without opening KiCad.
 *
 * Tools: kicad_query (read), kicad_edit (verified transactional writes), kicad_check (review),
 * kicad_render (sheet and library part images), kicad_datasheet (part datasheet pages). A
 * tool_call hook keeps agents from editing KiCad files or running KiCad programs any other way.
 * See .omp/skills/kicad-* for the workflows.
 */
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import { rasterizeSvg } from "@oh-my-pi/pi-natives";
import { findRepoRoot } from "./src/config";
import { type DatasheetRequest, readDatasheet } from "./src/datasheet";
import { compactJson } from "./src/format";
import { blockReason } from "./src/hook";
import { checksDoc, opsDoc, queriesDoc } from "./src/registry";
import { DEFAULT_FP_LAYERS, SIDES } from "./src/render";
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
			"Read a KiCad project: hierarchy, symbols, pins, nets, labels, net classes, rules, BOM and libraries; and LCSC's live part data (stock, prices). Read-only.",
			"Answers come from KiCad itself (kicad-cli netlist) and the schematic files; `lcsc` from lcsc.com.",
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
			"Change a KiCad project in one verified transaction: schematic sheets, hierarchy (new sheets, sheet pins), project settings (net classes, BOM presets), design rules and project libraries.",
			"The ops run in memory, then the result is written to a temporary copy, re-saved by KiCad (kicad-cli upgrade) and checked there. The real files are only replaced when every check passes:",
			'- the connectivity change equals `expect` exactly (default: no connectivity change at all). Declare every net whose pins change, either as its exact final pin list (`{name?, pins: ["R1.2", …]}`) or, for a net that existed before, as a delta (`{name: "GND", added: ["R66.2"], removed: []}`); plus added/removed parts, renamed nets and pins left on no net;',
			"- no new ERC violation; no new drawing-style finding on the touched sheets; a changed rules file parses; changed library parts add no KLC violation.",
			"Otherwise nothing is written and the report says why. Refuses while KiCad has the project open (lock files).",
			"Every dry run and every refused (NOT APPLIED) result ends with the actual connectivity change as a JSON `expect` in exactly this input format. Read it against what the edit is meant to do before passing it back: pasting it blindly declares whatever the ops did, including a pin they moved by mistake.",
			"Library changes (new symbols/footprints/libraries) and their use on a sheet need separate kicad_edit calls.",
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
				.describe("Return images of the touched sheet regions and of every library symbol (all units) and footprint the ops created or changed, each captioned."),
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
			"Review a KiCad project. Runs the named checks (default: all checks marked default) and lists findings with severity, sheet and position.",
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
			"Render a schematic sheet (or a region of it), a library symbol or a library footprint as a PNG image (KiCad's own plot), or a footprint in 3D with its 3D models. Give exactly one of `sheet`, `symbol`, `footprint`. Use it to look at a change, or at a stock part you derive a new one from.",
			'Parts are "Lib:Name" from the project\'s libraries or KiCad\'s stock ones, drawn in KiCad\'s built-in colours. Symbol coordinates are mm from the symbol origin, x right, y down (as lib_symbol reports pins and shapes); without `unit` every unit is drawn, in a grid in reading order. Footprint coordinates are KiCad\'s (mm, y down); by default copper, front silkscreen, courtyard and fab layers are drawn, with pad outlines and pad numbers sketched on the fab layers. The answer names the region shown, so a following `box` can zoom in.',
			'`view: "3d"` (footprints only) places the footprint at the origin of a scratch 1.6 mm board (courtyard + 1 mm) and raytraces it with KiCad\'s 3D viewer, to check that the 3D model lines up with the pads and silkscreen: look from `side`, turn the board with `rotate` ([-45, 0, 45] is isometric), `zoom` in. The answer lists every model path, where it resolves and whether the file exists: a MISSING model draws nothing.',
			CONVENTIONS,
		].join("\n\n"),
		parameters: z.object({
			project,
			sheet: z.string().optional().describe('Sheet name, file or "root".'),
			symbol: z.string().optional().describe('Library symbol "Lib:Name" (project or stock library).'),
			footprint: z.string().optional().describe('Library footprint "Lib:Name" (project or stock library).'),
			refs: z.array(z.string()).optional().describe("Sheets: frame these references (plus margin)."),
			unit: z.number().int().optional().describe("Symbols: draw only this unit (default every unit)."),
			body_style: z.number().int().optional().describe("Symbols: 1, or 2 for the De Morgan body style (default 1)."),
			layers: z
				.array(z.string())
				.optional()
				.describe(`Footprints: layers to draw, bottom first, e.g. ["F.Cu", "F.Fab"] (default ${DEFAULT_FP_LAYERS.join(", ")}). Pad numbers show on the fab layers.`),
			box: z
				.array(z.number())
				.optional()
				.describe("Region [x0, y0, x1, y1] in sheet mm, or in part mm for symbols (applied to each unit) and footprints. Default: the whole sheet, or the part's drawn extent."),
			margin: z.number().optional().describe("Margin in mm around refs/box on sheets (default 8), around box or the drawn extent for parts (default 2)."),
			size: z.number().optional().describe("Longest image side in pixels (default 1600; 3D images are square)."),
			view: z.enum(["2d", "3d"]).optional().describe('Footprints: "3d" for a raytraced image with the 3D models (default "2d").'),
			side: z.enum(SIDES).optional().describe('3D: side to look from (default "top").'),
			rotate: z.array(z.number()).optional().describe("3D: rotate the board [x, y, z] degrees before looking, e.g. [-45, 0, 45] for isometric."),
			zoom: z.number().optional().describe("3D: camera zoom; 1 spans about the board's longest side (default 0.85, or 0.7 with rotate: the whole board with a border)."),
			perspective: z.boolean().optional().describe("3D: perspective instead of orthographic projection."),
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
			"Read a part's datasheet PDF (from a URL, e.g. a symbol's Datasheet field, or a local path). Downloads once into the user cache (outside the repo) and refuses non-PDF answers.",
			"Without `page`, `pages` or `find`: the local PDF path, page count, title and the first lines of every page (find the pinout, package drawing or land pattern page). With `page`: that page's text and an image of the page, or of `box` (fractions of the page, y down) to zoom into a drawing. With `pages` (\"4-18\", \"4,6,10-12\"): the text of those pages, no images. With `find` (regex, case-insensitive): every matching line as `p<page>:<line>:` with a line of context around it, on all pages or on `pages`. The local path can also be read directly.",
		].join("\n\n"),
		parameters: z.object({
			url: z.string().describe("http(s) URL of the PDF, or a local PDF path (repo-relative or absolute)."),
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
}
