/**
 * Board queries: the board's setup (layers, stack-up, rules, outline), its footprints and one
 * footprint in full. Board millimetres, x right, y down; rotations counter-clockwise.
 */
import { type Board, courtyardBox, fieldsInfo, fpAttrs, fpId, fpProp, fpRef, padsInfo, placement } from "../board";
import { boardOutline, isEdgeItem, pointInPolygon, type ReadShape } from "../edges";
import type { Box, Pt } from "../geom";
import { describeFootprint } from "../libs";
import { boardSettings } from "../ops/board";
import { children, isList, type List, text } from "../sexpr";
import type { QueryDef } from "../types";

type Inside = "yes" | "no" | "partly";

interface Outline {
	outline?: ReadShape;
	cutouts: ReadShape[];
	open: ReadShape[];
	bbox?: Box;
	area?: number;
	polygon?: Pt[];
	holes: Pt[][];
}

function outlineOf(board: Board): Outline {
	return boardOutline(board.tree.filter((c): c is List => isList(c) && isEdgeItem(c)));
}

/**
 * Where a box lies against the outline: on the board ("yes": every sample point of its edges on
 * the board and no outline or cutout vertex strictly inside it, so no notch or hole cuts into it),
 * off it ("no") or across an edge ("partly").
 */
function insideOutline(box: Box, o: Outline): Inside {
	if (!o.polygon) return "no";
	const [x0, y0, x1, y1] = box;
	const pts: Pt[] = [];
	for (let i = 0; i <= 4; i++) {
		const fx = x0 + ((x1 - x0) * i) / 4;
		const fy = y0 + ((y1 - y0) * i) / 4;
		pts.push([fx, y0], [fx, y1], [x0, fy], [x1, fy]);
	}
	const on = (p: Pt) => pointInPolygon(p, o.polygon!) && !o.holes.some(h => pointInPolygon(p, h));
	const n = pts.filter(on).length;
	const edgeInBox = [o.polygon, ...o.holes].some(poly => poly.some(([x, y]) => x > x0 && x < x1 && y > y0 && y < y1));
	if (edgeInBox) return "partly";
	return n === pts.length ? "yes" : n === 0 ? "no" : "partly";
}

/** A footprint's sheet as the hierarchy names it ("/Power/"). */
const sheetOf = (fp: List) => text(children(fp, "sheetname")[0]) ?? "";

function footprintRow(fp: List, o: Outline) {
	const p = placement(fp);
	const court = courtyardBox(fp);
	return {
		ref: fpRef(fp),
		fp_id: fpId(fp),
		value: fpProp(fp, "Value") ?? "",
		sheet: sheetOf(fp),
		at: p.at,
		rot: p.rot,
		side: p.side,
		locked: p.locked,
		courtyard: court?.box ?? null,
		inside: court ? insideOutline(court.box, o) : "no",
	};
}

const boardQuery: QueryDef = {
	what: "board",
	doc: "The board: file, layers, stack-up (dielectrics, copper, finish, mask, thickness), board-wide rules and DRC severities (.kicad_pro, the keys set_board_rules takes), outline and cutouts (as set_board_edges takes them when they are such shapes, else the raw lines/arcs), outline bbox and area (minus cutouts, mm²), footprint counts (inside the outline, partly, outside) and net count.",
	fields: {},
	async run(ctx) {
		const { project } = ctx;
		const board = project.board();
		const su = board.stackup();
		const ds = boardSettings(project);
		const severities: Record<string, string[]> = {};
		for (const [k, v] of Object.entries(ds?.rule_severities ?? {})) (severities[v] ??= []).push(k);
		const o = outlineOf(board);
		const fps = board.footprints().map(fp => footprintRow(fp, o));
		const nets = new Set(board.footprints().flatMap(fp => padsInfo(fp).flatMap(p => (p.net ? [p.net] : []))));
		return {
			file: project.rel(project.boardFile),
			layers: board.layers().map(l => ({ name: l.name, type: l.type, ...(l.user ? { user_name: l.user } : {}) })),
			copper_layers: board.copperLayers(),
			stackup: {
				dielectrics: su.dielectrics,
				copper: su.copper,
				finish: su.finish,
				mask_thickness: su.mask,
				thickness: su.thickness,
				...(Math.abs(su.thickness - su.total) > 1e-6 ? { stackup_total: su.total, note: "the board thickness differs from the stack-up's total: set_stackup makes them agree" } : {}),
			},
			rules: ds?.rules ?? {},
			severities,
			outline: o.outline ?? null,
			cutouts: o.cutouts,
			...(o.open.length ? { open_edges: o.open } : {}),
			outline_bbox: o.bbox ?? null,
			area: o.area ?? null,
			footprints: {
				total: fps.length,
				inside: fps.filter(f => f.inside === "yes").length,
				partly: fps.filter(f => f.inside === "partly").length,
				outside: fps.filter(f => f.inside === "no").length,
			},
			nets: nets.size,
		};
	},
};

const footprintsQuery: QueryDef = {
	what: "footprints",
	doc: "Footprints on the board, in reference order: ref, fp_id, value, sheet, at, rot, side, locked, courtyard bbox [x0, y0, x1, y1] (F/B.CrtYd, or pads and fab drawings when there is none) and inside the outline (yes/no/partly).",
	fields: {
		ref: { t: "string", doc: "only this reference" },
		sheet: { t: "string", doc: "only footprints of this sheet (name, file or path)" },
		inside: { t: "boolean", doc: "true: only those fully inside the outline; false: only those not fully inside" },
		limit: { t: "integer", doc: "at most this many rows (default 200)" },
	},
	async run(ctx, args) {
		const board = ctx.project.board();
		const o = outlineOf(board);
		const sheet = typeof args.sheet === "string" ? ctx.project.sheet(args.sheet).namePath : undefined;
		if (typeof args.ref === "string") board.footprint(args.ref);
		const rows = board
			.footprints()
			.filter(fp => (args.ref === undefined || fpRef(fp) === args.ref) && (sheet === undefined || sheetOf(fp) === sheet))
			.map(fp => footprintRow(fp, o))
			.filter(r => args.inside === undefined || (r.inside === "yes") === args.inside)
			.sort((a, b) => a.ref.localeCompare(b.ref, undefined, { numeric: true }));
		const limit = typeof args.limit === "number" ? args.limit : 200;
		return { total: rows.length, ...(rows.length > limit ? { shown: limit } : {}), footprints: rows.slice(0, limit) };
	},
};

const footprintQuery: QueryDef = {
	what: "footprint",
	doc: "One footprint on the board in full: placement, symbol link, properties, field texts (name, value, board position, absolute rotation, layer, hidden, mirrored), attributes, pads (number, net, pin function and type, board position, size, layers), courtyard bbox and 3D models.",
	fields: { ref: { t: "string", req: true, doc: "reference" } },
	async run(ctx, args) {
		const board = ctx.project.board();
		const fp = board.footprint(String(args.ref));
		const props: Record<string, string> = {};
		for (const p of children(fp, "property")) props[text(p, 1) ?? ""] = text(p, 2) ?? "";
		const court = courtyardBox(fp);
		return {
			...footprintRow(fp, outlineOf(board)),
			path: text(children(fp, "path")[0]) ?? "",
			sheetfile: text(children(fp, "sheetfile")[0]) ?? "",
			courtyard_source: court?.source ?? null,
			properties: props,
			fields: fieldsInfo(fp),
			attributes: fpAttrs(fp),
			pads: padsInfo(fp),
			models: describeFootprint(fp).models,
		};
	},
};

export const queries: QueryDef[] = [boardQuery, footprintsQuery, footprintQuery];
