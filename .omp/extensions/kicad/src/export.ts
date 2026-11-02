/**
 * Fabrication outputs for JLCPCB (`kicad_export`), written to `<project dir>/fab/<rev>-<YYYY-MM-DD>/`:
 *
 * - gerbers: Gerbers and Excellon drill files with a drill map, zipped for upload;
 * - assembly: the BOM and CPL (pick and place) CSVs for JLCPCB's assembly service, and the list of
 *   the parts assembled by hand (`assembly.hand`), which the BOM and CPL leave out;
 * - step: the board with its 3D models;
 * - drawings: assembly drawings of each side with parts (outline, fab layer, silkscreen; the
 *   bottom mirrored as seen from below) with the hand-assembly parts highlighted, SVG and PDF.
 *
 * Gerbers and assembly are gated: KiCad's DRC with zones refilled in memory, schematic parity and
 * every connection routed, and ERC; assembly also needs the fields check. STEP and the drawings are
 * never gated; the drawings need a board outline.
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
import { type Board, courtyardEdges, fpAttrs, fpId, fpProp, fpRef, placement, toBoard } from "./board";
import { boardIsEmpty, drcFindings } from "./checks/board";
import { wildcards } from "./checks/circuit";
import { ercFindings } from "./checks/connectivity";
import * as cli from "./cli";
import type { AssemblyConfig, PlacementCorrection } from "./config";
import { boardOutline, isEdgeItem } from "./edges";
import type { Box, Pt } from "./geom";
import { kicadVars } from "./libs";
import { Project } from "./project";
import { parseCsv } from "./queries/project";
import { CHECKS } from "./registry";
import { f4, PART_THEME, svgBody } from "./render";
import { child, children, flag, isList, type List, text } from "./sexpr";
import { SpecError } from "./spec";

export const OUTPUTS = ["gerbers", "assembly", "step", "drawings"] as const;
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
 * that are fitted, not assembled by hand (`hand`: ref → reason), in the BOM and carry an LCSC part
 * number (and, given `posRefs`, appear in KiCad's position file). Every other part is left out with
 * its reason; `boardOnly` lists the board footprints without a symbol.
 */
export function assemblyParts(
	components: Iterable<cli.Component>,
	boardIds: ReadonlyMap<string, string>,
	posRefs?: ReadonlySet<string>,
	hand: AssemblyConfig["hand"] = {},
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
					: hand[c.ref] !== undefined
						? `hand assembly (assembly.hand): ${hand[c.ref]}`
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

/** An `assembly.corrections` key that is an LCSC code ("C7519") rather than a footprint pattern. */
const LCSC_KEY = /^C\d+$/i;

/**
 * The correction for a part: the entry keyed by its LCSC code (case-insensitive) if there is one,
 * else the first footprint pattern matching its "Lib:Name" (`*`/`?`, case-insensitive).
 */
export function correctionFor(
	id: string,
	corrections: Readonly<Record<string, PlacementCorrection>>,
	lcsc = "",
): { pattern: string; correction: PlacementCorrection } | undefined {
	const code = lcsc.trim().toUpperCase();
	if (code) for (const [key, correction] of Object.entries(corrections)) if (LCSC_KEY.test(key) && key.toUpperCase() === code) return { pattern: key, correction };
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
 * CPL rows of the assembled parts (`parts`: ref → footprint "Lib:Name" and LCSC code) in natural
 * reference order, the corrected ones (ref → matching key) and, per footprint id of the parts no
 * correction matches, their references.
 */
export function cplRows(
	pos: PosRow[],
	parts: ReadonlyMap<string, Pick<AsmPart, "footprint" | "lcsc">>,
	corrections: Readonly<Record<string, PlacementCorrection>>,
): { rows: CplRow[]; corrected: Map<string, string>; unconfirmed: Map<string, string[]> } {
	const rows: CplRow[] = [];
	const corrected = new Map<string, string>();
	const unconfirmed = new Map<string, string[]>();
	for (const p of [...pos].sort((a, b) => natural(a.ref, b.ref))) {
		const part = parts.get(p.ref);
		if (part === undefined) continue;
		const id = part.footprint;
		const hit = correctionFor(id, corrections, part.lcsc);
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

// ---- assembly: hand-assembly list ---------------------------------------------------------------

/** A part assembled by hand (`assembly.hand`): what it is, where it sits (the CPL's frame) and why. */
export interface HandRow {
	ref: string;
	value: string;
	/** "" when the part has none. */
	mpn: string;
	/** The board footprint's "Lib:Name". */
	footprint: string;
	layer: "Top" | "Bottom";
	/** mm, x right, page origin, as KiCad's position file. */
	x: number;
	/** mm, y up (the board's y negated), as KiCad's position file. */
	y: number;
	/** Degrees, as pcbnew shows it (KiCad's position file). */
	rot: number;
	reason: string;
	/** Position from the board file: the footprint is excluded from position files (or no position file was given). */
	fromBoard: boolean;
	fp: List;
}

/**
 * The hand-assembly parts (`hand`: ref → reason) in natural reference order: one row per fitted
 * one with a footprint, positioned from KiCad's position file `pos`, else from the board file
 * (same frame: page origin, y negated); the DNP ones and the ones with no footprint on the board apart.
 */
export function handParts(
	hand: AssemblyConfig["hand"],
	components: ReadonlyMap<string, cli.Component>,
	fps: readonly List[],
	pos: readonly PosRow[] = [],
): { rows: HandRow[]; dnp: string[]; missing: string[] } {
	const byRef = new Map(fps.map(fp => [fpRef(fp), fp] as const));
	const at = new Map(pos.map(p => [p.ref, p] as const));
	const rows: HandRow[] = [];
	const dnp: string[] = [];
	const missing: string[] = [];
	for (const [ref, reason] of Object.entries(hand).sort(([a], [b]) => natural(a, b))) {
		const fp = byRef.get(ref);
		const c = components.get(ref);
		if (!fp) missing.push(ref);
		else if (c?.dnp || fpAttrs(fp).includes("dnp")) dnp.push(ref);
		else {
			const pl = placement(fp);
			const p = at.get(ref);
			rows.push({
				ref,
				value: c?.value ?? fpProp(fp, "Value") ?? "",
				mpn: (c?.fields.MPN ?? fpProp(fp, "MPN") ?? "").trim(),
				footprint: fpId(fp),
				layer: (p?.side ?? pl.side) === "bottom" ? "Bottom" : "Top",
				x: p ? p.x : pl.at[0] + 0,
				y: p ? p.y : -pl.at[1] + 0,
				rot: p ? p.rot : pl.rot,
				reason,
				fromBoard: !p,
				fp,
			});
		}
	}
	return { rows, dnp, missing };
}

/** The hand-assembly list's columns: mm and rotation as in KiCad's position file. */
export function handCsv(rows: HandRow[]): string {
	const n = (v: number) => String(Math.round(v * 1e4) / 1e4 + 0);
	return csv([
		["Designator", "Value", "MPN", "Footprint", "Layer", "Mid X", "Mid Y", "Rotation", "Reason"],
		...rows.map(r => [r.ref, r.value, r.mpn, r.footprint, r.layer, `${n(r.x)}mm`, `${n(r.y)}mm`, n(r.rot), r.reason]),
	]);
}

/** What the answer says about the hand-assembly list besides the parts written. */
function handLines(h: { dnp: string[]; missing: string[] }): string[] {
	return [
		...(h.dnp.length ? [`In assembly.hand but DNP (not listed, not highlighted): ${h.dnp.join(", ")}.`] : []),
		...(h.missing.length ? [`In assembly.hand but not on the board (stale config, kicad_check placement flags it): ${h.missing.join(", ")}.`] : []),
	];
}

// ---- drawings -----------------------------------------------------------------------------------

/** Layers of a side's assembly drawing: the outline and fab layer in black, the silkscreen in grey. */
export const DRAWING_LAYERS = {
	top: { black: ["Edge.Cuts", "F.Fab"], grey: ["F.Silkscreen"] },
	bottom: { black: ["Edge.Cuts", "B.Fab"], grey: ["B.Silkscreen"] },
} as const;

/** kicad-cli plot options of the drawings' layers. */
const DRAWING_FLAGS = ["--black-and-white", "--crossout-DNP-footprints-on-fab-layers"];
const GREY = "#8C8C8C";
/** Colour of a hand-assembly part's courtyard, filled translucent, in the drawings. */
export const HAND_COLOUR = "#FF5A00";

/** Closed loops of edges given in any order and direction (a courtyard drawn as separate lines); an open chain is closed. */
export function edgeLoops(edges: readonly (readonly [Pt, Pt])[]): Pt[][] {
	const same = (a: Pt, b: Pt) => Math.abs(a[0] - b[0]) < 1e-4 && Math.abs(a[1] - b[1]) < 1e-4;
	const left = edges.map(e => [e[0], e[1]] as [Pt, Pt]);
	const loops: Pt[][] = [];
	while (left.length) {
		const loop: Pt[] = [...left.shift()!];
		for (;;) {
			const end = loop[loop.length - 1]!;
			if (same(loop[0]!, end)) break;
			const i = left.findIndex(e => same(e[0], end) || same(e[1], end));
			if (i < 0) break;
			const [e] = left.splice(i, 1);
			loop.push(same(e![0], end) ? e![1] : e![0]);
		}
		if (loop.length > 2 && same(loop[0]!, loop[loop.length - 1]!)) loop.pop();
		loops.push(loop);
	}
	return loops;
}

const xml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

/** The words of `s` in lines of at most `n` characters (a longer word on a line of its own). */
function wrap(s: string, n: number): string[] {
	const lines: string[] = [];
	for (const w of s.split(/\s+/).filter(Boolean)) {
		const last = lines[lines.length - 1];
		if (last !== undefined && last.length + 1 + w.length <= n) lines[lines.length - 1] = `${last} ${w}`;
		else lines.push(w);
	}
	return lines;
}

export interface DrawingSpec {
	side: "top" | "bottom";
	/** The board outline's bbox, board mm. */
	box: Box;
	/** KiCad's plots (SVG documents, board mm): the black layers and the grey ones. */
	black: string;
	grey: string;
	/** The hand-assembly parts on this side, with their courtyard edges (board mm). */
	hand: { ref: string; value: string; reason: string; edges: [Pt, Pt][] }[];
	title: string;
	/** Lines under the title. */
	info: string[];
}

/**
 * An assembly drawing on white: the board area (outline bbox + 5 mm), scaled up to 4:1 in half
 * steps so its longer side reaches 140 mm, with each hand-assembly part's courtyard filled
 * translucent in HAND_COLOUR under KiCad's plots (silkscreen grey, outline and fab layer black),
 * mirrored left to right for the bottom (seen from below, as KiCad's mirrored plot); below it the
 * legend: title, scale, info lines and the hand-assembly parts with their reasons. SVG units are mm.
 */
export function drawingSvg(d: DrawingSpec): string {
	const m = 5;
	const [x0, y0, x1, y1] = d.box;
	const [bw, bh] = [x1 - x0, y1 - y0];
	const s = Math.min(4, Math.max(1, Math.floor((140 / Math.max(bw, bh)) * 2) / 2));
	const w = Math.max(bw * s + 2 * m, 150);
	const font = 3;
	const cols = Math.max(20, Math.floor((w - 2 * m) / (font * 0.6)));
	const bx = (w - bw * s) / 2;
	const transform = d.side === "top" ? `matrix(${s} 0 0 ${s} ${f4(bx - x0 * s)} ${f4(m - y0 * s)})` : `matrix(${-s} 0 0 ${s} ${f4(bx + x1 * s)} ${f4(m - y0 * s)})`;
	const plot = (svg: string, colour?: string) => {
		const body = svgBody(svg).replace(/<(title|desc)>[\s\S]*?<\/\1>/g, "");
		return colour ? body.replace(/#000000/gi, colour) : body;
	};
	const highlights = d.hand.map(h => {
		const dPath = edgeLoops(h.edges)
			.map(l => `M${l.map(p => `${f4(p[0])},${f4(p[1])}`).join(" L")} Z`)
			.join(" ");
		return `<path data-ref="${xml(h.ref)}" d="${dPath}" fill="${HAND_COLOUR}" fill-opacity="0.35" fill-rule="evenodd" stroke="${HAND_COLOUR}" stroke-width="0.2" stroke-linejoin="round"/>`;
	});
	const legend: string[] = [];
	let y = bh * s + 2 * m + font * 1.4;
	legend.push(`<text x="${f4(m)}" y="${f4(y)}" font-size="${font * 1.3}" font-weight="bold">${xml(d.title)}</text>`);
	for (const l of [`Scale ${s}:1.`, ...d.info].flatMap(t => wrap(t, cols))) {
		y += font * 1.5;
		legend.push(`<text x="${f4(m)}" y="${f4(y)}" font-size="${font}">${xml(l)}</text>`);
	}
	y += font * 1.8;
	legend.push(
		`<rect x="${f4(m)}" y="${f4(y - font * 0.8)}" width="${f4(font * 1.6)}" height="${f4(font)}" fill="${HAND_COLOUR}" fill-opacity="0.35" stroke="${HAND_COLOUR}" stroke-width="0.2"/>`,
		`<text x="${f4(m + font * 2.2)}" y="${f4(y)}" font-size="${font}">${xml(
			d.hand.length ? `Hand assembly (assembly.hand), highlighted: ${d.hand.length} part${d.hand.length > 1 ? "s" : ""} on this side` : "Hand assembly (assembly.hand): none on this side",
		)}</text>`,
	);
	for (const h of d.hand) {
		for (const [i, l] of wrap(`${h.ref} (${h.value}): ${h.reason}`, cols - 4).entries()) {
			y += font * 1.5;
			legend.push(`<text x="${f4(m + font * (i ? 3.4 : 2.2))}" y="${f4(y)}" font-size="${font}"${i ? "" : ' font-weight="bold"'}>${xml(l)}</text>`);
		}
	}
	const h = y + m;
	return [
		`<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" version="1.1" width="${f4(w)}mm" height="${f4(h)}mm" viewBox="0 0 ${f4(w)} ${f4(h)}">`,
		`<title>${xml(d.title)}</title>`,
		`<defs><clipPath id="board-area" clipPathUnits="userSpaceOnUse"><rect x="${f4(x0 - m / s)}" y="${f4(y0 - m / s)}" width="${f4(bw + (2 * m) / s)}" height="${f4(bh + (2 * m) / s)}"/></clipPath></defs>`,
		`<rect x="0" y="0" width="${f4(w)}" height="${f4(h)}" fill="#FFFFFF"/>`,
		`<g id="board" transform="${transform}"><g clip-path="url(#board-area)">`,
		`<g id="hand-assembly">${highlights.join("")}</g>`,
		`<g id="silkscreen">${plot(d.grey, GREY)}</g>`,
		`<g id="outline-fab">${plot(d.black)}</g>`,
		"</g></g>",
		`<g id="legend" font-family="sans-serif" fill="#000000">${legend.join("")}</g>`,
		"</svg>",
	].join("\n");
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
	switch (output) {
		case "gerbers":
			return [at(`${name}-gerbers.zip`), at("gerbers")];
		case "assembly":
			return [at(`${name}-bom.csv`), at(`${name}-cpl.csv`), at(`${name}-hand-assembly.csv`)];
		case "step":
			return [at(`${name}.step`)];
		case "drawings":
			return ["top", "bottom"].flatMap(s => [at(`${name}-assembly-${s}.svg`), at(`${name}-assembly-${s}.pdf`)]);
	}
}

export async function runExport(repoRoot: string, req: ExportRequest, signal?: AbortSignal, now = new Date()): Promise<ExportResult> {
	if (!Array.isArray(req.outputs) || !req.outputs.length) throw new SpecError(`outputs: give one or more of ${OUTPUTS.join(", ")}`);
	for (const o of req.outputs) if (!OUTPUTS.includes(o as Output)) throw new SpecError(`outputs: unknown output "${o}"; outputs: ${OUTPUTS.join(", ")}`);
	const outputs = OUTPUTS.filter(o => req.outputs.includes(o));
	if (req.rev !== undefined && !req.rev.trim()) throw new SpecError("rev: give a revision, or leave it out for the board's title block revision");
	const project = Project.open(req.project, repoRoot);
	const board = project.board();
	const corrections = outputs.includes("assembly") ? checkCorrections(project.settings.assembly.corrections, project.config.file) : {};
	const hand = project.settings.assembly.hand;
	const outline = boardOutline(board.tree.filter((c): c is List => isList(c) && isEdgeItem(c)));
	const fps = board.footprints();
	const bottom = fps.filter(fp => placement(fp).side === "bottom").length;
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
		const gated = outputs.filter(x => x === "gerbers" || x === "assembly");
		const [gate, nl, fields] = await Promise.all([
			gated.length ? boardGate(project, analysis) : Promise.resolve([]),
			analysis.netlist(),
			outputs.includes("assembly") ? CHECKS.get("fields")!.run({ project, analysis, baseline: "HEAD", args: {}, signal }) : Promise.resolve([]),
		]);
		for (const x of gated) {
			const why = [...gate, ...(x === "assembly" ? failed("fields check errors", fields.filter(f => f.severity === "error").map(f => f.message)) : [])];
			if (why.length) refusals.set(x, why);
		}
		if (outputs.includes("drawings") && !outline.bbox) refusals.set("drawings", []);
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

		const boardIds = new Map(fps.map(fp => [fpRef(fp), fpId(fp)] as const));
		const asm = assemblyParts(nl.components.values(), boardIds, undefined, hand);
		let handNoted = false;
		if (ok("assembly")) {
			const pos = parsePos(await cli.pcbPos(project.boardFile, o));
			const { parts, left, boardOnly } = assemblyParts(nl.components.values(), boardIds, new Set(pos.map(p => p.ref)), hand);
			const rows = bomRows(parts);
			const cpl = cplRows(pos, new Map(parts.map(p => [p.ref, p])), corrections);
			const bomFile = path.join(dir, `${project.name}-bom.csv`);
			const cplFile = path.join(dir, `${project.name}-cpl.csv`);
			fs.writeFileSync(bomFile, bomCsv(rows));
			fs.writeFileSync(cplFile, cplCsv(cpl.rows));
			files.push(bomFile, cplFile);
			const byHand = handParts(hand, nl.components, fps, pos);
			const handFile = path.join(dir, `${project.name}-hand-assembly.csv`);
			if (byHand.rows.length) {
				fs.writeFileSync(handFile, handCsv(byHand.rows));
				files.push(handFile);
			}
			const fromBoard = byHand.rows.filter(r => r.fromBoard).map(r => r.ref);
			handNoted = true;
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
			if (byHand.rows.length) {
				notes.push(
					`Hand assembly (assembly.hand): ${byHand.rows.length} parts in ${path.basename(handFile)}, left out of the BOM and CPL:`,
					...byHand.rows.map(r => `- ${r.ref} (${r.value}, ${r.footprint}, ${r.layer}): ${r.reason}`),
					...(fromBoard.length ? [`Positions from the board file (excluded from position files): ${fromBoard.join(", ")}.`] : []),
				);
			}
			notes.push(...handLines(byHand));
		}

		if (ok("drawings")) {
			const sides = (["top", "bottom"] as const).filter(s => s === "top" || bottom > 0);
			const byHand = handParts(hand, nl.components, fps);
			const tmp = cli.tmpDir("drawings");
			let pdfs = 0;
			try {
				const plot = (layers: readonly string[], file: string) => cli.pcbSvg(project.boardFile, [...layers], PART_THEME, path.join(tmp, file), o, DRAWING_FLAGS);
				const plots = await Promise.all(sides.map(s => Promise.all([plot(DRAWING_LAYERS[s].black, `${s}-black.svg`), plot(DRAWING_LAYERS[s].grey, `${s}-grey.svg`)])));
				const [bx0, by0, bx1, by1] = outline.bbox!;
				for (const [k, side] of sides.entries()) {
					const onSide = byHand.rows.filter(r => r.layer === (side === "top" ? "Top" : "Bottom"));
					const svg = drawingSvg({
						side,
						box: outline.bbox!,
						black: fs.readFileSync(plots[k]![0], "utf8"),
						grey: fs.readFileSync(plots[k]![1], "utf8"),
						hand: onSide.map(r => ({ ref: r.ref, value: r.value, reason: r.reason, edges: courtyardEdges(r.fp) })),
						title: `${project.name}: assembly drawing, ${side === "top" ? "top side (seen from above)" : "bottom side, mirrored (seen from below)"}`,
						info: [
							`Rev ${rev}, ${ymd(now)}; board ${round2(bx1 - bx0)} × ${round2(by1 - by0)} mm; footprints on this side: ${side === "top" ? fps.length - bottom : bottom}.`,
							`Outline and ${DRAWING_LAYERS[side].black[1]} (references) in black, ${DRAWING_LAYERS[side].grey[0]} in grey; DNP parts crossed out.`,
						],
					});
					const file = path.join(dir, `${project.name}-assembly-${side}.svg`);
					fs.writeFileSync(file, svg);
					files.push(file);
					const pdf = path.join(dir, `${project.name}-assembly-${side}.pdf`);
					if (await cli.svgToPdf(file, pdf, o)) {
						files.push(pdf);
						pdfs++;
					}
				}
			} finally {
				fs.rmSync(tmp, { recursive: true, force: true });
			}
			const lit = byHand.rows.map(r => `${r.ref} (${r.layer})`);
			notes.push(
				`Drawings: ${sides.join(" and ")} side${sides.length > 1 ? "s" : ""} (${sides.length > 1 ? "bottom mirrored, seen from below; " : "no footprints on the bottom; "}` +
					`${pdfs ? `PDF rasterised at ${cli.PDF_DPI} dpi` : "SVG only: the dev shell has no ImageMagick (magick) for the PDF"}); ` +
					`hand-assembly parts highlighted: ${lit.length ? lit.join(", ") : "none"}.`,
				...(handNoted ? [] : handLines(byHand)),
			);
		}

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
		const g = refusals.get("gerbers");
		const a = refusals.get("assembly");
		const shared = g !== undefined && a !== undefined && g.join("\n") === a.join("\n");
		for (const [x, why] of refusals) {
			if (x === "drawings") lines.push("REFUSED drawings: the board has no outline (Edge.Cuts) to draw them on.");
			else if (shared && x === "gerbers") lines.push("REFUSED gerbers and assembly: the board is not ready for fabrication:", ...why);
			else if (!(shared && x === "assembly")) lines.push(`REFUSED ${x}: the board is not ready for fabrication:`, ...why);
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
