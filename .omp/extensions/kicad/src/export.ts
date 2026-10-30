/**
 * Fabrication outputs for JLCPCB (`kicad_export`), written to `<project dir>/fab/<rev>-<YYYY-MM-DD>/`:
 *
 * - gerbers: Gerbers and Excellon drill files with a drill map, zipped for upload;
 * - assembly: the BOM and CPL (pick and place) CSVs for JLCPCB's assembly service;
 * - step: the board with its 3D models.
 *
 * Gerbers and assembly are gated: KiCad's DRC with zones refilled in memory, schematic parity and
 * every connection routed, and ERC; assembly also needs the fields check. STEP is never gated.
 *
 * JLCPCB sources (read 2026-10-30):
 * - Gerbers and drill: https://jlcpcb.com/help/article/how-to-generate-gerber-and-drill-files-in-kicad-8
 *   (layers F/B.Cu and inner copper, F/B.Paste, F/B.Silkscreen, F/B.Mask, Edge.Cuts; plot reference
 *   designators and footprint text; check zone fills; tent vias; Protel extensions; subtract
 *   soldermask from silkscreen; drill: alternate oval mode, absolute origin, mm, decimal zeros; a
 *   drill map; zip the folder).
 * - BOM: https://jlcpcb.com/help/article/bill-of-materials-for-pcb-assembly and its sample
 *   Sample-BOM_JLCSMT.xlsx (columns Comment, Designator, Footprint, "JLCPCB Part #"; designators of
 *   a row comma-separated).
 * - CPL: https://jlcpcb.com/help/article/pick-place-file-for-pcb-assembly and its sample
 *   Sample-CPL_JLCSMT.xlsx (columns Designator, Mid X, Mid Y, Layer, Rotation; mm; rotation
 *   counter-clockwise). KiCad's position file is the source: x right, y up (the board's y negated),
 *   page origin like the Gerbers. Bottom parts get 180° − KiCad's rotation (seen from the part's
 *   top), then the correction, as the KiCad plugins JLCPCB points to do
 *   (https://jlcpcb.com/help/article/how-to-generate-bom-and-centroid-files-from-kicad-8: bennymeg's
 *   Fabrication Toolkit; Bouni's kicad-jlcpcb-tools).
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { deflateRawSync } from "node:zlib";
import { Analysis } from "./analysis";
import { type Board, fpId, fpRef, placement, toBoard } from "./board";
import { boardIsEmpty, drcFindings } from "./checks/board";
import { wildcards } from "./checks/circuit";
import { ercFindings } from "./checks/connectivity";
import * as cli from "./cli";
import type { PlacementCorrection } from "./config";
import { boardOutline, isEdgeItem } from "./edges";
import { kicadVars } from "./libs";
import { Project } from "./project";
import { parseCsv } from "./queries/project";
import { CHECKS } from "./registry";
import { child, children, flag, isList, type List, text } from "./sexpr";
import { SpecError } from "./spec";

export const OUTPUTS = ["gerbers", "assembly", "step"] as const;
export type Output = (typeof OUTPUTS)[number];

export interface ExportRequest {
	project?: string;
	outputs: string[];
	rev?: string;
}

export interface ExportResult {
	text: string;
	/** Files written, repo-relative. */
	files: string[];
	/** Requested outputs that were refused. */
	refused: Output[];
}

/** The board layers JLCPCB asks for besides the copper layers. */
export const FAB_LAYERS = ["F.Paste", "B.Paste", "F.SilkS", "B.SilkS", "F.Mask", "B.Mask", "Edge.Cuts"];

/** Findings shown per failed gate item. */
const FIRST = 5;

const natural = (a: string, b: string) => a.localeCompare(b, undefined, { numeric: true });

// ---- assembly: BOM ------------------------------------------------------------------------------

/** A part JLCPCB places: in the BOM, fitted, on the board, with an LCSC part number. */
export interface AsmPart {
	ref: string;
	value: string;
	/** The board footprint's "Lib:Name". */
	footprint: string;
	lcsc: string;
}

export interface LeftOut {
	ref: string;
	reason: string;
}

/**
 * The parts JLCPCB assembles: schematic parts (not power symbols) whose footprint is on the board,
 * that are fitted, in the BOM and carry an LCSC part number (and, given `posRefs`, appear in
 * KiCad's position file). Every other part is left out with its reason; `boardOnly` lists the
 * board footprints without a symbol.
 */
export function assemblyParts(
	components: Iterable<cli.Component>,
	boardIds: ReadonlyMap<string, string>,
	posRefs?: ReadonlySet<string>,
): { parts: AsmPart[]; left: LeftOut[]; boardOnly: string[] } {
	const parts: AsmPart[] = [];
	const left: LeftOut[] = [];
	const inSchematic = new Set<string>();
	for (const c of components) {
		if (c.ref.startsWith("#")) continue;
		inSchematic.add(c.ref);
		const lcsc = (c.fields.LCSC ?? "").trim();
		const id = boardIds.get(c.ref);
		const reason =
			c.excludeFromBoard || id === undefined
				? "not on the board"
				: c.dnp
					? "DNP"
					: c.excludeFromBom
						? "excluded from the BOM"
						: !lcsc
							? "no LCSC field"
							: posRefs && !posRefs.has(c.ref)
								? "excluded from position files"
								: undefined;
		if (reason) left.push({ ref: c.ref, reason });
		else parts.push({ ref: c.ref, value: c.value, footprint: id!, lcsc });
	}
	const boardOnly = [...boardIds.keys()].filter(r => !inSchematic.has(r)).sort(natural);
	return { parts: parts.sort((a, b) => natural(a.ref, b.ref)), left: left.sort((a, b) => natural(a.ref, b.ref)), boardOnly };
}

export interface BomRow {
	comment: string;
	designators: string[];
	footprint: string;
	lcsc: string;
}

/**
 * One BOM row per LCSC part: its values as the comment and its footprint names (without the
 * library), distinct ones joined by " / ", designators in natural order; rows by first designator.
 */
export function bomRows(parts: AsmPart[]): BomRow[] {
	const groups = new Map<string, AsmPart[]>();
	for (const p of parts) {
		const key = p.lcsc.toUpperCase();
		groups.set(key, [...(groups.get(key) ?? []), p]);
	}
	const distinct = (xs: string[]) => [...new Set(xs)].join(" / ");
	return [...groups.values()]
		.map(g => ({
			comment: distinct(g.map(p => p.value)),
			designators: g.map(p => p.ref).sort(natural),
			footprint: distinct(g.map(p => p.footprint.slice(p.footprint.indexOf(":") + 1))),
			lcsc: g[0]!.lcsc,
		}))
		.sort((a, b) => natural(a.designators[0]!, b.designators[0]!));
}

/** CSV with every cell quoted (RFC 4180). */
function csv(rows: string[][]): string {
	return rows.map(r => r.map(c => `"${c.replace(/"/g, '""')}"`).join(",")).join("\r\n") + "\r\n";
}

/** JLCPCB's BOM columns. */
export function bomCsv(rows: BomRow[]): string {
	return csv([["Comment", "Designator", "Footprint", "JLCPCB Part #"], ...rows.map(r => [r.comment, r.designators.join(","), r.footprint, r.lcsc])]);
}

// ---- assembly: CPL ------------------------------------------------------------------------------

/** A row of KiCad's position file: mm, y up, rotation as pcbnew shows it. */
export interface PosRow {
	ref: string;
	x: number;
	y: number;
	rot: number;
	side: "top" | "bottom";
}

/** KiCad's CSV position file (Ref, Val, Package, PosX, PosY, Rot, Side). */
export function parsePos(src: string): PosRow[] {
	const [header = [], ...rows] = parseCsv(src);
	const col = (name: string) => {
		const i = header.indexOf(name);
		if (i < 0) throw new Error(`KiCad's position file has no ${name} column (header: ${header.join(", ")})`);
		return i;
	};
	const [ref, x, y, rot, side] = ["Ref", "PosX", "PosY", "Rot", "Side"].map(col) as [number, number, number, number, number];
	return rows.map(r => ({ ref: r[ref]!, x: Number(r[x]), y: Number(r[y]), rot: Number(r[rot]), side: r[side] === "bottom" ? "bottom" : "top" }));
}

/** The first correction whose pattern matches a footprint "Lib:Name" (`*`/`?`, case-insensitive). */
export function correctionFor(
	id: string,
	corrections: Readonly<Record<string, PlacementCorrection>>,
): { pattern: string; correction: PlacementCorrection } | undefined {
	for (const [pattern, correction] of Object.entries(corrections)) if (wildcards([pattern])(id)) return { pattern, correction };
	return undefined;
}

/** Reject malformed `assembly.corrections` entries, naming the entry. */
export function checkCorrections(corrections: Readonly<Record<string, unknown>>, where: string): Record<string, PlacementCorrection> {
	for (const [k, v] of Object.entries(corrections)) {
		const at = `${where} assembly.corrections[${JSON.stringify(k)}]`;
		if (!v || typeof v !== "object" || Array.isArray(v)) throw new SpecError(`${at}: must be an object {rot?, offset?}`);
		const c = v as Record<string, unknown>;
		for (const key of Object.keys(c)) if (key !== "rot" && key !== "offset") throw new SpecError(`${at}: unknown key "${key}" (allowed: rot, offset)`);
		if (c.rot !== undefined && (typeof c.rot !== "number" || !Number.isFinite(c.rot))) throw new SpecError(`${at}: rot must be a number of degrees`);
		const o = c.offset;
		if (o !== undefined && !(Array.isArray(o) && o.length === 2 && o.every(n => typeof n === "number" && Number.isFinite(n)))) {
			throw new SpecError(`${at}: offset must be [x, y] in mm`);
		}
	}
	return corrections as Record<string, PlacementCorrection>;
}

export interface CplRow {
	ref: string;
	/** mm, x right. */
	x: number;
	/** mm, y up. */
	y: number;
	/** Degrees counter-clockwise, [0, 360). */
	rot: number;
	layer: "Top" | "Bottom";
}

const deg360 = (a: number) => {
	const x = ((Math.round(a * 1e6) / 1e6) % 360 + 360) % 360;
	return x === 360 ? 0 : x + 0;
};

/**
 * A JLCPCB CPL row from KiCad's position row and a correction: bottom parts turn to 180° − KiCad's
 * rotation; the correction's rotation is added; its offset (footprint frame, y down, mirrored for
 * a bottom footprint as pcbnew flips it) is turned with the footprint onto the board.
 */
export function cplRow(p: PosRow, c: PlacementCorrection = {}): CplRow {
	const [ox, oy] = c.offset ?? [0, 0];
	const d = toBoard({ at: [0, 0], rot: p.rot }, [ox, p.side === "bottom" ? -oy : oy]);
	return {
		ref: p.ref,
		x: Math.round((p.x + d[0]) * 1e6) / 1e6 + 0,
		// the position file's y points up, the board's down
		y: Math.round((p.y - d[1]) * 1e6) / 1e6 + 0,
		rot: deg360((p.side === "bottom" ? 180 - p.rot : p.rot) + (c.rot ?? 0)),
		layer: p.side === "bottom" ? "Bottom" : "Top",
	};
}

/**
 * CPL rows of the assembled parts (`ids`: ref → "Lib:Name") in natural reference order, the
 * corrected ones (ref → pattern) and, per footprint id that no correction matches, its references.
 */
export function cplRows(
	pos: PosRow[],
	ids: ReadonlyMap<string, string>,
	corrections: Readonly<Record<string, PlacementCorrection>>,
): { rows: CplRow[]; corrected: Map<string, string>; unconfirmed: Map<string, string[]> } {
	const rows: CplRow[] = [];
	const corrected = new Map<string, string>();
	const unconfirmed = new Map<string, string[]>();
	for (const p of [...pos].sort((a, b) => natural(a.ref, b.ref))) {
		const id = ids.get(p.ref);
		if (id === undefined) continue;
		const hit = correctionFor(id, corrections);
		if (!hit) unconfirmed.set(id, [...(unconfirmed.get(id) ?? []), p.ref]);
		else if ((hit.correction.rot ?? 0) % 360 !== 0 || (hit.correction.offset ?? [0, 0]).some(v => v !== 0)) corrected.set(p.ref, hit.pattern);
		rows.push(cplRow(p, hit?.correction));
	}
	return { rows, corrected, unconfirmed };
}

/** JLCPCB's CPL columns, coordinates in mm as in JLCPCB's sample. */
export function cplCsv(rows: CplRow[]): string {
	const n = (v: number) => String(Math.round(v * 1e4) / 1e4 + 0);
	return csv([["Designator", "Mid X", "Mid Y", "Layer", "Rotation"], ...rows.map(r => [r.ref, `${n(r.x)}mm`, `${n(r.y)}mm`, r.layer, n(r.rot)])]);
}

// ---- zip ----------------------------------------------------------------------------------------

/** A zip archive (deflate, UTF-8 names) of these entries, dated `date` (local time). */
export function zip(entries: { name: string; data: Uint8Array }[], date = new Date()): Uint8Array {
	const time = (date.getHours() << 11) | (date.getMinutes() << 5) | Math.floor(date.getSeconds() / 2);
	const day = ((Math.max(date.getFullYear(), 1980) - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate();
	const locals: Buffer[] = [];
	const central: Buffer[] = [];
	let offset = 0;
	for (const e of entries) {
		const name = Buffer.from(e.name, "utf8");
		const packed = deflateRawSync(e.data);
		const crc = Bun.hash.crc32(e.data);
		const local = Buffer.alloc(30);
		local.writeUInt32LE(0x04034b50, 0);
		local.writeUInt16LE(20, 4);
		local.writeUInt16LE(0x0800, 6);
		local.writeUInt16LE(8, 8);
		local.writeUInt16LE(time, 10);
		local.writeUInt16LE(day, 12);
		local.writeUInt32LE(crc, 14);
		local.writeUInt32LE(packed.length, 18);
		local.writeUInt32LE(e.data.length, 22);
		local.writeUInt16LE(name.length, 26);
		const dir = Buffer.alloc(46);
		dir.writeUInt32LE(0x02014b50, 0);
		dir.writeUInt16LE(20, 4);
		dir.writeUInt16LE(20, 6);
		dir.writeUInt16LE(0x0800, 8);
		dir.writeUInt16LE(8, 10);
		dir.writeUInt16LE(time, 12);
		dir.writeUInt16LE(day, 14);
		dir.writeUInt32LE(crc, 16);
		dir.writeUInt32LE(packed.length, 20);
		dir.writeUInt32LE(e.data.length, 24);
		dir.writeUInt16LE(name.length, 28);
		dir.writeUInt32LE(offset, 42);
		locals.push(local, name, packed);
		central.push(dir, name);
		offset += local.length + name.length + packed.length;
	}
	const size = central.reduce((s, b) => s + b.length, 0);
	const end = Buffer.alloc(22);
	end.writeUInt32LE(0x06054b50, 0);
	end.writeUInt16LE(entries.length, 8);
	end.writeUInt16LE(entries.length, 10);
	end.writeUInt32LE(size, 12);
	end.writeUInt32LE(offset, 16);
	return Buffer.concat([...locals, ...central, end]);
}

// ---- gate ---------------------------------------------------------------------------------------

/** Why the board is not ready for fabrication: one block per failed item, its count and first findings. */
function failed(what: string, messages: string[]): string[] {
	if (!messages.length) return [];
	const more = messages.length > FIRST ? [`    - … ${messages.length - FIRST} more`] : [];
	return [`- ${what}: ${messages.length}`, ...messages.slice(0, FIRST).map(m => `    - ${m}`), ...more];
}

/** The board gate of gerbers and assembly: KiCad's DRC (zones refilled in memory, parity, routing) and ERC. */
async function boardGate(project: Project, analysis: Analysis): Promise<string[]> {
	if (boardIsEmpty(project.board().tree)) return ["- the board is empty (no footprints, no outline)"];
	const [drc, erc] = await Promise.all([cli.pcbDrc(project.boardFile, analysis.runOpts, true, true), analysis.erc()]);
	const { violations, parity } = drcFindings(drc);
	const unconnected = drc.unconnected.filter(v => !v.excluded);
	return [
		...failed("KiCad DRC errors (zones refilled)", violations.filter(f => f.severity === "error").map(f => f.message)),
		...failed("schematic parity issues", parity.map(f => f.message)),
		...failed("unconnected items (connections not routed)", unconnected.map(v => `${v.description} [${v.items.map(i => i.description).join("; ")}]`)),
		...failed("ERC errors", ercFindings(erc).filter(f => f.severity === "error").map(f => `${f.message}${f.sheet ? ` [${f.sheet}]` : ""}`)),
	];
}

// ---- runner -------------------------------------------------------------------------------------

/** The title block revision of the board, if any. */
function boardRev(board: Board): string | undefined {
	return text(child(child(board.tree, "title_block") ?? [], "rev"))?.trim() || undefined;
}

const ymd = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;

/** Every model path of the board's footprints that doesn't resolve to a file (`${KIPRJMOD}` = the project directory), with its references. */
async function missingModels(project: Project, board: Board): Promise<{ total: number; missing: Map<string, string[]> }> {
	const vars: Record<string, string> = { ...(await kicadVars(project.repoRoot)), KIPRJMOD: project.dir };
	const missing = new Map<string, string[]>();
	let total = 0;
	for (const fp of board.footprints()) {
		for (const m of children(fp, "model")) {
			if (flag(m, "hide") === true) continue;
			const raw = text(m) ?? "";
			total++;
			const resolved = path.resolve(project.dir, raw.replace(/\$\{([A-Za-z0-9_]+)\}/g, (v, k: string) => vars[k] ?? v));
			if (!fs.existsSync(resolved)) missing.set(raw, [...(missing.get(raw) ?? []), fpRef(fp)]);
		}
	}
	return { total, missing };
}

/** The files and directories an output writes into the fab directory of a board named `name`. */
export function outputPaths(dir: string, name: string, output: Output): string[] {
	const at = (f: string) => path.join(dir, f);
	return output === "gerbers" ? [at(`${name}-gerbers.zip`), at("gerbers")] : output === "assembly" ? [at(`${name}-bom.csv`), at(`${name}-cpl.csv`)] : [at(`${name}.step`)];
}

export async function runExport(repoRoot: string, req: ExportRequest, signal?: AbortSignal, now = new Date()): Promise<ExportResult> {
	if (!Array.isArray(req.outputs) || !req.outputs.length) throw new SpecError(`outputs: give one or more of ${OUTPUTS.join(", ")}`);
	for (const o of req.outputs) if (!OUTPUTS.includes(o as Output)) throw new SpecError(`outputs: unknown output "${o}"; outputs: ${OUTPUTS.join(", ")}`);
	const outputs = OUTPUTS.filter(o => req.outputs.includes(o));
	if (req.rev !== undefined && !req.rev.trim()) throw new SpecError("rev: give a revision, or leave it out for the board's title block revision");
	const project = Project.open(req.project, repoRoot);
	const board = project.board();
	const corrections = outputs.includes("assembly") ? checkCorrections(project.settings.assembly.corrections, project.config.file) : {};
	const rev = req.rev?.trim() ?? boardRev(board) ?? "draft";
	const dir = path.join(project.dir, "fab", `${rev.replace(/[^A-Za-z0-9._-]+/g, "_")}-${ymd(now)}`);
	const analysis = new Analysis(project, signal);
	const o = analysis.runOpts;
	const files: string[] = [];
	const notes: string[] = [];
	const refusals = new Map<Output, string[]>();
	// every requested output starts from nothing: a refused one leaves none of an earlier run's files behind
	const removed = outputs.flatMap(x => outputPaths(dir, project.name, x)).filter(p => fs.existsSync(p));
	for (const p of removed) fs.rmSync(p, { recursive: true, force: true });
	try {
		const gated = outputs.filter(x => x !== "step");
		const [gate, nl, fields] = await Promise.all([
			gated.length ? boardGate(project, analysis) : Promise.resolve([]),
			analysis.netlist(),
			outputs.includes("assembly") ? CHECKS.get("fields")!.run({ project, analysis, baseline: "HEAD", args: {}, signal }) : Promise.resolve([]),
		]);
		for (const x of gated) {
			const why = [...gate, ...(x === "assembly" ? failed("fields check errors", fields.filter(f => f.severity === "error").map(f => f.message)) : [])];
			if (why.length) refusals.set(x, why);
		}
		const ok = (x: Output) => outputs.includes(x) && !refusals.has(x);
		if (outputs.some(ok)) fs.mkdirSync(dir, { recursive: true });

		if (ok("step")) {
			const out = path.join(dir, `${project.name}.step`);
			await cli.pcbStep(project.boardFile, out, o);
			files.push(out);
			const models = await missingModels(project, board);
			const missing = [...models.missing].map(([p, refs]) => `${p} (${refs.sort(natural).join(", ")})`);
			notes.push(`STEP: ${models.total} 3D models referenced${missing.length ? `; MISSING (not in the STEP): ${missing.join("; ")}` : ", all found"}.`);
		}

		if (ok("gerbers")) {
			const gdir = path.join(dir, "gerbers");
			await cli.pcbGerbers(project.boardFile, [...board.copperLayers(), ...FAB_LAYERS], gdir, o);
			const plotted = await cli.pcbDrill(project.boardFile, gdir, o);
			const archive = path.join(dir, `${project.name}-gerbers.zip`);
			fs.writeFileSync(archive, zip(plotted.map(f => ({ name: path.basename(f), data: fs.readFileSync(f) })), now));
			files.push(archive, ...plotted);
			notes.push(`Gerbers: ${plotted.length} files (Gerbers, drill, drill map) zipped into ${path.basename(archive)}: upload that one.`);
		}

		const boardIds = new Map(board.footprints().map(fp => [fpRef(fp), fpId(fp)] as const));
		const asm = assemblyParts(nl.components.values(), boardIds);
		if (ok("assembly")) {
			const pos = parsePos(await cli.pcbPos(project.boardFile, o));
			const { parts, left, boardOnly } = assemblyParts(nl.components.values(), boardIds, new Set(pos.map(p => p.ref)));
			const rows = bomRows(parts);
			const cpl = cplRows(pos, new Map(parts.map(p => [p.ref, p.footprint])), corrections);
			const bomFile = path.join(dir, `${project.name}-bom.csv`);
			const cplFile = path.join(dir, `${project.name}-cpl.csv`);
			fs.writeFileSync(bomFile, bomCsv(rows));
			fs.writeFileSync(cplFile, cplCsv(cpl.rows));
			files.push(bomFile, cplFile);
			const mixed = rows.filter(r => r.comment.includes(" / ") || r.footprint.includes(" / "));
			const fixes = [...cpl.corrected].map(([r, p]) => `${r} by "${p}"`);
			const several = mixed.map(r => `${r.lcsc} (${r.designators.join(", ")}): ${r.comment}; ${r.footprint}`);
			notes.push(
				`Assembly: BOM ${rows.length} lines, ${parts.length} parts; CPL ${cpl.rows.length} rows, ${cpl.corrected.size} corrected${fixes.length ? ` (${fixes.join(", ")})` : ""}.`,
				...(several.length ? [`One LCSC part with several values or footprints: ${several.join("; ")}.`] : []),
				...leftOutLines(left, boardOnly),
				...(cpl.unconfirmed.size
					? [
							`Not confirmed in JLCPCB's placement preview (no assembly.corrections entry in ${project.rel(project.config.file)}; ` +
								'check each in the preview, then add an entry, {"rot": 0} when it needs none):',
							...[...cpl.unconfirmed].sort(([a], [b]) => natural(a, b)).map(([id, refs]) => `- ${id}: ${refs.join(", ")}`),
						]
					: ["Every placed footprint is confirmed by an assembly.corrections entry."]),
			);
		}

		const outline = boardOutline(board.tree.filter((c): c is List => isList(c) && isEdgeItem(c)));
		const fps = board.footprints();
		const bottom = fps.filter(fp => placement(fp).side === "bottom").length;
		const comps = [...nl.components.values()].filter(c => !c.ref.startsWith("#"));
		const copper = board.copperLayers();
		const size = outline.bbox ? `${round2(outline.bbox[2] - outline.bbox[0])} × ${round2(outline.bbox[3] - outline.bbox[1])} mm` : "no outline";
		const lines = [
			`Fabrication outputs of ${project.rel(project.dir)}, rev ${rev}, in ${project.rel(dir)}/`,
			`Board: ${size}, ${copper.length} copper layers (${copper.join(", ")}); ${fps.length} footprints (${fps.length - bottom} top, ${bottom} bottom); ` +
				`schematic: ${comps.length} parts, ${comps.filter(c => c.dnp).length} DNP; to assemble: ${asm.parts.length} parts, ${bomRows(asm.parts).length} LCSC part numbers.`,
		];
		if (files.length) lines.push("Written:", ...files.map(f => `- ${project.rel(f)}`));
		const refusedOutputs = [...refusals.keys()];
		if (refusedOutputs.length) {
			const shared = refusedOutputs.length === 2 && refusals.get("gerbers")!.join("\n") === refusals.get("assembly")!.join("\n");
			if (shared) lines.push(`REFUSED gerbers and assembly: the board is not ready for fabrication:`, ...refusals.get("gerbers")!);
			else for (const [x, why] of refusals) lines.push(`REFUSED ${x}: the board is not ready for fabrication:`, ...why);
		}
		const stale = removed.filter(p => refusedOutputs.some(x => outputPaths(dir, project.name, x).includes(p)));
		if (stale.length) lines.push(`Removed the refused outputs' files from an earlier run: ${stale.map(p => project.rel(p)).join(", ")}`);
		lines.push(...notes);
		return { text: lines.join("\n"), files: files.map(f => project.rel(f)), refused: refusedOutputs };
	} finally {
		analysis.dispose();
	}
}

const round2 = (v: number) => Math.round(v * 100) / 100;

/** Parts left out of the BOM and CPL, grouped by reason. */
function leftOutLines(left: LeftOut[], boardOnly: string[]): string[] {
	const by = new Map<string, string[]>();
	for (const l of left) by.set(l.reason, [...(by.get(l.reason) ?? []), l.ref]);
	if (boardOnly.length) by.set("board-only footprints (no symbol)", boardOnly);
	return by.size ? ["Left out of the BOM and CPL:", ...[...by].map(([reason, refs]) => `- ${reason}: ${refs.join(", ")}`)] : [];
}
