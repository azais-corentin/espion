/** Runners behind kicad_query, kicad_check and kicad_render (kicad_edit is txn.ts). */
import { Analysis } from "./analysis";
import type { Box } from "./geom";
import { Project } from "./project";
import { CHECKS, QUERIES } from "./registry";
import { type ModelInfo, type Rasterize, renderBoard, renderBoard3d, renderFootprint3d, renderPart, renderSheet, SIDES } from "./render";
import { SpecError, validate } from "./spec";
import type { Finding } from "./types";

export const CONVENTIONS = `Conventions: coordinates are sheet millimetres, x right, y down, on the 1.27 mm grid. Rotations are 0/90/180/270 degrees counter-clockwise; mirror is "x" or "y". A pin's "dir" is the side its wire leaves (L/R/U/D). Sheets are named by sheet name ("Power"), file ("power.kicad_sch") or "root". Net names are as in KiCad's netlist ("/Power/POE_VA1", "+3V3"). Board coordinates are board millimetres, x right, y down (as pcbnew shows them); board rotations are any angle in degrees, counter-clockwise, as pcbnew shows them; board layers go by KiCad's canonical names ("F.Cu", "Edge.Cuts"). "project" is the .kicad_pro path or its directory, relative to the repo; it may be omitted when .omp/kicad.json lists one project.`;

export interface QueryRequest {
	project?: string;
	what: string;
	args?: Record<string, unknown>;
}

export interface CheckRequest {
	project?: string;
	checks?: string[];
	sheets?: string[];
	baseline?: string;
	args?: Record<string, unknown>;
}

export interface RenderToolRequest {
	project?: string;
	sheet?: string;
	symbol?: string;
	footprint?: string;
	board?: boolean;
	refs?: string[];
	unit?: number;
	body_style?: number;
	layers?: string[];
	view?: string;
	side?: string;
	rotate?: number[];
	zoom?: number;
	perspective?: boolean;
	box?: number[];
	margin?: number;
	size?: number;
	nets?: string[];
	ratsnest?: boolean;
}

/** One caption line per 3D model: where it resolves, whether the file is there, non-default placement. */
function modelLine(m: ModelInfo): string {
	const place = [
		m.offset.some(v => v !== 0) ? `offset [${m.offset.join(", ")}] mm` : "",
		m.rotation.some(v => v !== 0) ? `rotate [${m.rotation.join(", ")}]°` : "",
		m.scale.some(v => v !== 1) ? `scale [${m.scale.join(", ")}]` : "",
		m.hidden ? "hidden" : "",
	].filter(Boolean);
	const where = m.resolved === m.path ? "" : ` → ${m.resolved}`;
	return `- ${m.path}${where}: ${m.exists ? "found" : "MISSING (no such file; nothing of it is drawn)"}${place.length ? `; ${place.join(", ")}` : ""}`;
}

/** kicad_render's answer: a caption naming what is shown and the region, and the image. */
export interface RenderToolResult {
	text: string;
	png: Uint8Array;
}

export async function runQuery(repoRoot: string, req: QueryRequest, signal?: AbortSignal): Promise<unknown> {
	const def = QUERIES.get(req.what);
	if (!def) throw new SpecError(`unknown query "${req.what}"; queries: ${[...QUERIES.keys()].join(", ")}`);
	const project = Project.open(req.project, repoRoot);
	const analysis = new Analysis(project, signal);
	try {
		return await def.run({ project, analysis, libs: () => project.libs() }, validate(`query ${req.what}`, req.args ?? {}, def.fields));
	} finally {
		analysis.dispose();
	}
}

export async function runChecks(repoRoot: string, req: CheckRequest, signal?: AbortSignal): Promise<{ findings: Finding[]; report: string }> {
	const names = req.checks?.length ? req.checks : [...CHECKS.values()].filter(c => c.byDefault).map(c => c.name);
	for (const n of names) if (!CHECKS.has(n)) throw new SpecError(`unknown check "${n}"; checks: ${[...CHECKS.keys()].join(", ")}`);
	const project = Project.open(req.project, repoRoot);
	const analysis = new Analysis(project, signal);
	const sheets = req.sheets?.length ? req.sheets.map(s => project.sheet(s)) : undefined;
	const results = await Promise.all(
		names.map(async n => {
			try {
				return { n, findings: await CHECKS.get(n)!.run({ project, analysis, sheets, baseline: req.baseline ?? "HEAD", args: req.args ?? {}, signal }) };
			} catch (e) {
				return { n, error: (e as Error).message, findings: [] as Finding[] };
			}
		}),
	).finally(() => analysis.dispose());
	const lines: string[] = [];
	const all: Finding[] = [];
	for (const r of results) {
		all.push(...r.findings);
		const by = (s: string) => r.findings.filter(f => f.severity === s).length;
		lines.push(
			"error" in r
				? `## ${r.n}: FAILED TO RUN: ${r.error}`
				: `## ${r.n}: ${r.findings.length ? `${by("error")} errors, ${by("warning")} warnings, ${by("info")} info` : "clean"}`,
		);
		for (const f of r.findings) {
			const where = [f.sheet, f.at ? `(${f.at[0]}, ${f.at[1]})` : undefined].filter(Boolean).join(" ");
			lines.push(`- ${f.severity}: ${f.message}${where ? ` [${where}]` : ""}`);
		}
	}
	return { findings: all, report: lines.join("\n") };
}

export async function runRender(repoRoot: string, req: RenderToolRequest, rasterize: Rasterize, signal?: AbortSignal): Promise<RenderToolResult> {
	if (req.board === false) throw new SpecError("board: give true to render the board, or leave it out");
	const targets = (["sheet", "symbol", "footprint", "board"] as const).filter(k => req[k] !== undefined);
	if (targets.length !== 1) throw new SpecError(`give exactly one of sheet, symbol, footprint, board${targets.length ? ` (got ${targets.join(" and ")})` : ""}`);
	for (const [field, target] of [["refs", "sheet"], ["unit", "symbol"], ["body_style", "symbol"]] as const) {
		if (req[field] !== undefined && req[target] === undefined) throw new SpecError(`${field} applies to a ${target} only`);
	}
	if (req.layers !== undefined && req.footprint === undefined && !req.board) throw new SpecError("layers applies to a footprint or the board only");
	if ((req.nets !== undefined || req.ratsnest !== undefined) && !req.board) throw new SpecError("nets and ratsnest apply to the board only");
	const view = req.view ?? "2d";
	if (view !== "2d" && view !== "3d") throw new SpecError('view must be "2d" or "3d"');
	if (view === "3d" && req.footprint === undefined && !req.board) throw new SpecError("view 3d applies to a footprint or the board only");
	// a 2D board image takes a side too: it picks that side's default layers
	const only3d = (["side", "rotate", "zoom", "perspective"] as const).filter(k => req[k] !== undefined && !(k === "side" && req.board));
	const only2d = (["layers", "box", "margin", "nets", "ratsnest"] as const).filter(k => req[k] !== undefined);
	if (view === "2d" && only3d.length) throw new SpecError(`${only3d.join(", ")}: for view "3d" only`);
	if (view === "3d" && only2d.length) throw new SpecError(`${only2d.join(", ")}: for view "2d" only`);
	if (req.box && req.box.length !== 4) throw new SpecError("box must be [x0, y0, x1, y1]");
	const box: Box | undefined = req.box ? [req.box[0]!, req.box[1]!, req.box[2]!, req.box[3]!] : undefined;
	const fmtBox = (b: Box) => `[${b.map(v => v.toFixed(2)).join(", ")}]`;
	const project = Project.open(req.project, repoRoot);
	const side = SIDES.find(s => s === req.side);
	if (req.side !== undefined && !side) throw new SpecError(`side must be one of ${SIDES.join(", ")}`);
	const camera = (zoom: number) =>
		[`from ${side ?? "top"}`, req.rotate ? `board rotated [${req.rotate.join(", ")}]°` : "", `zoom ${zoom}`, req.perspective ? "perspective" : ""].filter(Boolean).join(", ");
	if (req.board) {
		const o = { cwd: project.dir, repoRoot, signal };
		const rel = project.rel(project.board().file);
		if (view === "3d") {
			const img = await renderBoard3d(project.boardFile, project.dir, { side, rotate: req.rotate, zoom: req.zoom, perspective: req.perspective, size: req.size }, o);
			return { text: `Board ${rel} in 3D, ${camera(img.zoom)}, with its footprints' 3D models.`, png: img.png };
		}
		if (side !== undefined && side !== "top" && side !== "bottom") throw new SpecError('side: a 2D board image is "top" or "bottom"');
		const img = await renderBoard(project.boardFile, { layers: req.layers, side, box, margin: req.margin, size: req.size, nets: req.nets, ratsnest: req.ratsnest }, rasterize, o);
		const extra = [
			img.highlighted ? `nets ${img.highlighted.nets.join(", ")} highlighted in yellow (${img.highlighted.items} pads, tracks, vias and zone fills)` : "",
			img.ratsnest !== undefined ? `${img.ratsnest} unrouted connections drawn as thin white lines${req.nets?.length ? " (of these nets)" : ""}` : "",
		].filter(Boolean);
		return {
			text: `Board ${rel}, layers ${img.layers.join(", ")}, region ${fmtBox(img.box)} mm (board coordinates, seen from the top)${extra.length ? `; ${extra.join("; ")}` : ""}`,
			png: img.png,
		};
	}
	if (view === "3d") {
		const id = req.footprint!;
		const img = await renderFootprint3d(
			await project.libs(),
			{ id, side, rotate: req.rotate, zoom: req.zoom, perspective: req.perspective, size: req.size },
			{ cwd: project.dir, repoRoot, signal },
		);
		const models = img.models.length ? ["3D models:", ...img.models.map(modelLine)] : ["3D models: none (the footprint references no model)"];
		const missing = img.models.filter(m => !m.exists).length;
		return {
			text: [
				`Footprint ${id} in 3D, ${camera(img.zoom)}, on a 1.6 mm scratch board ${fmtBox(img.board)} mm (courtyard + 1 mm, footprint coordinates).`,
				...models,
				...(missing ? [`WARNING: ${missing} of ${img.models.length} model(s) missing, not drawn: the image shows the board and the footprint without them.`] : []),
			].join("\n"),
			png: img.png,
		};
	}
	if (req.sheet !== undefined) {
		const analysis = new Analysis(project, signal);
		try {
			const img = await renderSheet(analysis, { sheet: req.sheet, refs: req.refs, box, margin: req.margin, size: req.size }, rasterize);
			return { text: `Sheet ${img.sheet}, region ${fmtBox(img.box)} mm`, png: img.png };
		} finally {
			analysis.dispose();
		}
	}
	const kind = req.symbol !== undefined ? "symbol" : "footprint";
	const id = req.symbol ?? req.footprint!;
	const img = await renderPart(
		await project.libs(),
		{ kind, id, unit: req.unit, body_style: req.body_style, layers: req.layers, box, margin: req.margin, size: req.size },
		rasterize,
		{ cwd: project.dir, repoRoot, signal },
	);
	if (kind === "footprint") return { text: `Footprint ${id}, region ${fmtBox(img.frames[0]!.box)} mm (footprint coordinates)`, png: img.png };
	const frames = img.frames.map(f => `unit ${f.unit} ${fmtBox(f.box)}`).join(", ");
	return {
		text: `Symbol ${id}, regions in symbol mm (y down): ${frames}${img.frames.length > 1 ? " (units in reading order)" : ""}`,
		png: img.png,
	};
}
