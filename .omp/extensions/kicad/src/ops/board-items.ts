/**
 * Board items that aren't copper routing: board texts and drawings, and board-only footprints
 * (no symbol: fiducials, logos, test pads). Board millimetres, x right, y down; rotations
 * counter-clockwise degrees; KiCad canonical layer names.
 *
 * Written as KiCad 10 stores them: `(gr_text "…" (at x y rot) (layer "F.SilkS" [knockout]) (uuid)
 * (effects (font (size h w) (thickness t)) [(justify … [mirror])]))`; `(gr_line|gr_rect|gr_circle|
 * gr_arc|gr_poly … (stroke (width w) (type solid)) [(fill yes|no)] (layer) (uuid))`.
 */
import * as fs from "node:fs";
import { courtyardBox, flipToBack, fpAttrs, fpRef, instantiate, setPlacement, setProperty, textAngle } from "../board";
import { Copper, type BoardText, type Drawing } from "../copper";
import { arcPoints, pointInPolygon } from "../edges";
import { mm } from "../fpgfx";
import { type Box, bbox, type Pt } from "../geom";
import type { Project } from "../project";
import { newUuid } from "../sch";
import { child, children, head, isList, L, type List, parse, str, Sym, text } from "../sexpr";
import type { OpDef } from "../types";
import { boardDefaults, boardSettings } from "./board";
import { boxAround, fmtMm, fmtPt, short, xy } from "./copper";

/** A board layer by its canonical name or its user name ("F.Silkscreen" → "F.SilkS"). */
function layerArg(project: Project, v: unknown): string {
	const name = String(v);
	const layers = project.board().layers();
	const hit = layers.find(l => l.name === name) ?? layers.find(l => l.user === name);
	if (!hit) throw new Error(`layer: the board has no layer "${name}" (layers: ${layers.map(l => l.name).join(", ")})`);
	if (hit.name === "Edge.Cuts") throw new Error("layer Edge.Cuts: the outline belongs to set_board_edges");
	return hit.name;
}

/**
 * pcbnew's defaults for new texts and lines on a layer (Board Setup > Text & Graphics). On the
 * layers KiCad's DRC checks text on (silkscreen and copper), default texts are raised to the
 * board's min_text_height and min_text_thickness, so a default never breaks the board's own rules.
 */
function layerDefaults(project: Project, layer: string): { size: [number, number]; thickness: number; line: number } {
	const settings = boardSettings(project);
	const d = settings?.defaults ?? {};
	const n = (k: string, dflt: number) => (typeof d[k] === "number" ? (d[k] as number) : dflt);
	const kind = /\.SilkS$/.test(layer) ? "silk" : /\.Cu$/.test(layer) ? "copper" : /\.Fab$/.test(layer) ? "fab" : /\.CrtYd$/.test(layer) ? "courtyard" : "other";
	const textKind = kind === "courtyard" ? "other" : kind;
	const big = kind === "copper";
	let size: [number, number] = [n(`${textKind}_text_size_h`, big ? 1.5 : 1), n(`${textKind}_text_size_v`, big ? 1.5 : 1)];
	let thickness = n(`${textKind}_text_thickness`, big ? 0.3 : 0.15);
	if (kind === "silk" || kind === "copper") {
		const { min_text_height: minH, min_text_thickness: minT } = settings?.rules ?? {};
		if (typeof minH === "number" && size[1] < minH) size = [(size[0] * minH) / size[1], minH];
		if (typeof minT === "number") thickness = Math.max(thickness, minT);
	}
	return { size, thickness, line: n(`${kind}_line_width`, kind === "courtyard" ? 0.05 : big ? 0.2 : 0.1) };
}

const isBack = (layer: string) => layer.startsWith("B.");

// ---- texts ----------------------------------------------------------------------------------------

const JUSTIFY = ["left", "right", "top", "bottom"] as const;

const addBoardText: OpDef = {
	op: "add_board_text",
	group: "board",
	doc: "Add a text to the board (not to a footprint). Size and thickness default to Board Setup > Text & Graphics for the layer, raised on silkscreen and copper to the board's min_text_height and min_text_thickness; a text on a back layer is mirrored, as pcbnew places it. The board_graphics query lists texts in the same form.",
	fields: {
		text: { t: "string", req: true, doc: "the text" },
		at: { t: "point", req: true, doc: "anchor, board mm" },
		layer: { t: "string", req: true, doc: 'layer, e.g. "F.SilkS", "B.Cu", "F.Fab"' },
		size: { t: "any", doc: "glyph size, mm: a number, or [width, height]" },
		thickness: { t: "number", doc: "stroke thickness, mm" },
		rot: { t: "number", doc: "rotation, degrees counter-clockwise (default 0)" },
		justify: { t: "strings", values: JUSTIFY, doc: "anchor side: left|right and/or top|bottom (default centred)" },
		knockout: { t: "boolean", doc: "knocked out of a filled box (default false)" },
	},
	apply(ctx, args) {
		const project = ctx.project;
		const board = project.board();
		const content = String(args.text);
		if (!content.trim()) throw new Error("text is empty");
		const layer = layerArg(project, args.layer);
		const dflt = layerDefaults(project, layer);
		const sz = args.size;
		let size: [number, number] = dflt.size;
		if (typeof sz === "number") size = [sz, sz];
		else if (Array.isArray(sz) && sz.length === 2 && sz.every(x => typeof x === "number")) size = sz as [number, number];
		else if (sz !== undefined) throw new Error(`size: give a number or [width, height] in mm, got ${JSON.stringify(sz)}`);
		if (!(size[0] > 0 && size[1] > 0)) throw new Error("size must be positive");
		const thickness = (args.thickness as number | undefined) ?? dflt.thickness;
		if (!(thickness > 0)) throw new Error("thickness must be positive");
		const justify = (args.justify as string[] | undefined) ?? [];
		if (justify.includes("left") && justify.includes("right")) throw new Error("justify: left or right, not both");
		if (justify.includes("top") && justify.includes("bottom")) throw new Error("justify: top or bottom, not both");
		const at = args.at as Pt;
		const rot = textAngle((args.rot as number | undefined) ?? 0);
		const jl = [...justify.filter(j => j === "left" || j === "right"), ...justify.filter(j => j === "top" || j === "bottom"), ...(isBack(layer) ? ["mirror"] : [])];
		const uuid = newUuid();
		board.add(
			L(
				"gr_text",
				str(content),
				L("at", mm(at[0]), mm(at[1]), mm(rot)),
				L("layer", str(layer), ...(args.knockout === true ? [new Sym("knockout")] : [])),
				L("uuid", str(uuid)),
				L("effects", L("font", L("size", mm(size[1]), mm(size[0])), L("thickness", mm(thickness))), ...(jl.length ? [L("justify", ...jl)] : [])),
			),
		);
		project.touchBoard();
		const r = Math.max(...size) * (content.length + 1);
		ctx.touchBoard(boxAround([at], r));
		ctx.notes.push(`text "${content}" at ${fmtPt(at)} on ${layer}${isBack(layer) ? " (mirrored)" : ""}, ${fmtMm(size[0])} × ${fmtMm(size[1])} mm, ${fmtMm(rot)}°; uuid ${short(uuid)}`);
	},
};

// ---- drawings -------------------------------------------------------------------------------------

const SHAPE_KEYS: Record<string, string[]> = {
	line: ["start", "end"],
	rect: ["start", "end"],
	circle: ["center"],
	arc: ["start", "mid", "end"],
	poly: ["points"],
};

const isPt = (v: unknown): v is Pt => Array.isArray(v) && v.length === 2 && v.every(x => typeof x === "number" && Number.isFinite(x));

const addBoardGraphic: OpDef = {
	op: "add_board_graphic",
	group: "board",
	doc:
		"Add a drawing to the board (not to a footprint, not on Edge.Cuts: set_board_edges draws the outline), in the form the board_graphics query lists them: " +
		"{type: line|rect|circle|arc|poly, start, end (line, rect: corners), center with radius or end (circle), start, mid, end (arc), points (poly), width?, fill?}. Width defaults to Board Setup > Text & Graphics for the layer; 0 draws no outline (a filled shape).",
	fields: {
		layer: { t: "string", req: true, doc: 'layer, e.g. "F.SilkS", "Dwgs.User", "F.Cu"' },
		shape: { t: "object", req: true, doc: "{type, …geometry, width?, fill?}" },
	},
	apply(ctx, args) {
		const project = ctx.project;
		const board = project.board();
		const layer = layerArg(project, args.layer);
		const s = args.shape as Record<string, unknown>;
		const type = String(s.type);
		const need = SHAPE_KEYS[type];
		if (!need) throw new Error(`shape.type: one of ${Object.keys(SHAPE_KEYS).join(", ")}, got ${JSON.stringify(s.type)}`);
		const allowed = ["type", "width", "fill", ...need, ...(type === "circle" ? ["radius", "end"] : [])];
		for (const k of Object.keys(s)) if (!allowed.includes(k)) throw new Error(`shape: "${k}" doesn't apply to a ${type} (keys: ${allowed.join(", ")})`);
		for (const k of need.filter(k => k !== "points")) if (!isPt(s[k])) throw new Error(`shape.${k}: give [x, y] in mm`);
		const width = s.width === undefined ? layerDefaults(project, layer).line : Number(s.width);
		if (!(width >= 0)) throw new Error("shape.width must be >= 0");
		const fill = s.fill === true;
		if (fill && (type === "line" || type === "arc")) throw new Error(`shape.fill: a ${type} has no inside`);
		if (!fill && width === 0) throw new Error("shape: width 0 needs fill: true (it would draw nothing)");
		const pts: Pt[] = [];
		let geometry: List[];
		if (type === "circle") {
			const center = s.center as Pt;
			if ((s.radius === undefined) === (s.end === undefined)) throw new Error("shape: a circle takes radius or end (a point on it)");
			const end: Pt = s.end !== undefined ? (s.end as Pt) : [center[0] + Number(s.radius), center[1]];
			if (s.end !== undefined && !isPt(s.end)) throw new Error("shape.end: give [x, y] in mm");
			const r = Math.hypot(end[0] - center[0], end[1] - center[1]);
			if (!(r > 0)) throw new Error("shape: the radius must be positive");
			geometry = [xy("center", center), xy("end", end)];
			pts.push([center[0] - r, center[1] - r], [center[0] + r, center[1] + r]);
		} else if (type === "poly") {
			const p = s.points as unknown[];
			if (!Array.isArray(p) || p.length < 3 || !p.every(isPt)) throw new Error("shape.points: give at least three [x, y] points");
			geometry = [L("pts", ...(p as Pt[]).map(q => xy("xy", q)))];
			pts.push(...(p as Pt[]));
		} else {
			geometry = need.map(k => xy(k, s[k] as Pt));
			pts.push(...need.map(k => s[k] as Pt));
			const [a, b] = [s.start as Pt, s.end as Pt];
			if (Math.hypot(a[0] - b[0], a[1] - b[1]) < 1e-6) throw new Error("shape: start and end are the same point");
			if (type === "rect" && (a[0] === b[0] || a[1] === b[1])) throw new Error("shape: a rect needs width and height");
		}
		const uuid = newUuid();
		board.add(
			L(`gr_${type}`, ...geometry, L("stroke", L("width", mm(width)), L("type", "solid")), ...(type === "line" || type === "arc" ? [] : [L("fill", fill)]), L("layer", str(layer)), L("uuid", str(uuid))),
		);
		project.touchBoard();
		ctx.touchBoard(boxAround(pts, width / 2 + 1));
		ctx.notes.push(`${type} on ${layer}${fill ? ", filled" : ""}, width ${fmtMm(width)} mm; uuid ${short(uuid)}`);
	},
};

/** Is p on a drawing's stroke (within tol of it), or inside it when filled? */
function drawingHas(d: Drawing, p: Pt, tol: number): boolean {
	const near = (a: Pt, b: Pt) => {
		const [dx, dy] = [b[0] - a[0], b[1] - a[1]];
		const l2 = dx * dx + dy * dy;
		const t = l2 ? Math.max(0, Math.min(1, ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2)) : 0;
		return Math.hypot(p[0] - a[0] - t * dx, p[1] - a[1] - t * dy) <= tol;
	};
	const loop = (pts: Pt[]) => pts.some((q, i) => near(q, pts[(i + 1) % pts.length]!)) || (d.fill && pointInPolygon(p, pts));
	switch (d.type) {
		case "line":
			return near(d.start!, d.end!);
		case "rect":
			return loop([d.start!, [d.end![0], d.start![1]], d.end!, [d.start![0], d.end![1]]]);
		case "circle": {
			const r = Math.hypot(p[0] - d.center![0], p[1] - d.center![1]);
			return Math.abs(r - d.radius!) <= tol || (d.fill && r <= d.radius!);
		}
		case "arc": {
			const pts = arcPoints(d.start!, d.mid!, d.end!);
			return pts.slice(1).some((q, i) => near(pts[i]!, q));
		}
		default:
			return loop(d.points ?? []);
	}
}

function drawingBox(d: Drawing): Box {
	const pts: Pt[] = d.type === "circle" ? [[d.center![0] - d.radius!, d.center![1] - d.radius!], [d.center![0] + d.radius!, d.center![1] + d.radius!]] : d.points ?? [d.start!, d.end!, ...(d.mid ? [d.mid] : [])];
	const b = bbox(pts);
	return [b[0] - d.width, b[1] - d.width, b[2] + d.width, b[3] + d.width];
}

const removeBoardItem: OpDef = {
	op: "remove_board_item",
	group: "board",
	doc: "Delete one board text or drawing (not of a footprint, not on Edge.Cuts), picked by uuid (or its first characters, the board_graphics id), by its exact text, or by layer and a point on it (on a drawing's stroke or inside a filled one; within a glyph size of a text's anchor). Several matches are an error that lists them.",
	fields: {
		uuid: { t: "string", doc: "uuid or its first characters" },
		text: { t: "string", doc: "a text's exact content" },
		layer: { t: "string", doc: "with at: the layer" },
		at: { t: "point", doc: "with layer: a point on the item, board mm" },
	},
	apply(ctx, args) {
		const project = ctx.project;
		const board = project.board();
		const c = Copper.of(board);
		if ((args.layer === undefined) !== (args.at === undefined)) throw new Error("give layer and at together");
		const ways = [args.uuid, args.text, args.at].filter(v => v !== undefined).length;
		if (ways !== 1) throw new Error("give exactly one of uuid, text, layer + at");
		type Item = { kind: "text"; t: BoardText } | { kind: "drawing"; d: Drawing };
		const all: Item[] = [...c.texts.map(t => ({ kind: "text" as const, t })), ...c.drawings.filter(d => d.layer !== "Edge.Cuts").map(d => ({ kind: "drawing" as const, d }))];
		const uuidOf = (i: Item) => (i.kind === "text" ? i.t.uuid : i.d.uuid);
		let hits: Item[];
		if (args.uuid !== undefined) {
			const u = String(args.uuid);
			if (c.drawings.some(d => d.layer === "Edge.Cuts" && d.uuid.startsWith(u))) throw new Error("that is an Edge.Cuts drawing: the outline belongs to set_board_edges");
			hits = all.filter(i => uuidOf(i).startsWith(u));
		} else if (args.text !== undefined) hits = all.filter(i => i.kind === "text" && i.t.text === args.text);
		else {
			const layer = layerArg(project, args.layer);
			const at = args.at as Pt;
			hits = all.filter(i =>
				i.kind === "text"
					? i.t.layer === layer && Math.hypot(at[0] - i.t.at[0], at[1] - i.t.at[1]) <= Math.max(...i.t.size)
					: i.d.layer === layer && drawingHas(i.d, at, Math.max(i.d.width / 2, 0.05)),
			);
		}
		const label = (i: Item) => (i.kind === "text" ? `text "${i.t.text}" on ${i.t.layer} (${short(i.t.uuid)})` : `${i.d.type} on ${i.d.layer} (${short(i.d.uuid)})`);
		if (!hits.length) throw new Error("no board text or drawing matches (the board_graphics query lists them)");
		if (hits.length > 1) throw new Error(`${hits.length} items match: ${hits.slice(0, 10).map(label).join(", ")}; give uuid`);
		const hit = hits[0]!;
		board.remove(hit.kind === "text" ? hit.t.node : hit.d.node);
		project.touchBoard();
		ctx.touchBoard(hit.kind === "text" ? boxAround([hit.t.at], Math.max(...hit.t.size) * (hit.t.text.length + 1)) : drawingBox(hit.d));
		ctx.notes.push(`removed ${label(hit)}`);
	},
};

// ---- board-only footprints ------------------------------------------------------------------------

const addBoardFootprint: OpDef = {
	op: "add_board_footprint",
	group: "board",
	doc: "Add a footprint that has no symbol (a fiducial, logo, mounting or test pad): pcbnew's \"Not in schematic\" (board_only) attribute set, no pad nets. On the bottom it is flipped as pcbnew flips footprints. Its own violations against the board rules are reported, not gated.",
	fields: {
		fp_id: { t: "string", req: true, doc: '"Library:Name", e.g. "Fiducial:Fiducial_1mm_Mask2mm"' },
		ref: { t: "string", req: true, doc: 'reference, unique on the board and in the schematic, e.g. "FID1"' },
		at: { t: "point", req: true, doc: "footprint origin, board mm" },
		rot: { t: "number", doc: "rotation, degrees counter-clockwise (default 0)" },
		side: { t: "string", values: ["top", "bottom"], doc: "board side (default top)" },
		value: { t: "string", doc: "Value field (default: the library footprint's)" },
	},
	apply(ctx, args) {
		const project = ctx.project;
		const board = project.board();
		const id = String(args.fp_id);
		const ref = String(args.ref);
		if (!/^[A-Za-z#_][\w#+.-]*$/.test(ref)) throw new Error(`ref "${ref}": letters, digits and _ # + . - only, starting with a letter`);
		if (board.footprints().some(f => fpRef(f) === ref)) throw new Error(`${ref} is on the board already`);
		if (project.allRefs().has(ref)) throw new Error(`${ref} is a symbol's reference in the schematic: pick another`);
		const fp = instantiate(parse(fs.readFileSync(ctx.libs.footprintFile(id), "utf8")), id, newUuid);
		const font = boardDefaults(project).fabFont;
		setProperty(fp, "Reference", ref, font, newUuid);
		if (args.value !== undefined) setProperty(fp, "Value", String(args.value), font, newUuid);
		const attr = child(fp, "attr");
		if (attr && !fpAttrs(fp).includes("board_only")) attr.push(new Sym("board_only"));
		else if (!attr) {
			// KiCad keeps attr after the fields and before the drawings and pads
			const first = fp.findIndex(c => isList(c) && /^(fp_|pad$|zone$|group$|model$|embedded_)/.test(head(c) ?? ""));
			fp.splice(first < 0 ? fp.length : first, 0, L("attr", "board_only"));
		}
		if (args.side === "bottom") flipToBack(fp, board.copperLayers());
		setPlacement(fp, args.at as Pt, (args.rot as number | undefined) ?? 0);
		board.add(fp);
		project.touchBoard();
		const b = courtyardBox(fp)?.box;
		ctx.touchBoard(b ? [b[0] - 1, b[1] - 1, b[2] + 1, b[3] + 1] : boxAround([args.at as Pt], 3), [ref]);
		ctx.notes.push(`${ref} (${id}, board only) at ${fmtPt(args.at as Pt)}, ${fmtMm((args.rot as number | undefined) ?? 0)}°, ${args.side === "bottom" ? "bottom" : "top"}`);
	},
};

const removeBoardFootprint: OpDef = {
	op: "remove_board_footprint",
	group: "board",
	doc: "Delete a footprint that has no symbol link (board-only: fiducial, logo, test pad). A footprint linked to a symbol is refused: take the symbol off the board in the schematic and run update_from_schematic with remove_extra.",
	fields: { ref: { t: "string", req: true, doc: "reference" } },
	apply(ctx, args) {
		const board = ctx.project.board();
		const ref = String(args.ref);
		const fp = board.footprint(ref);
		const link = text(children(fp, "path")[0]);
		if (link) throw new Error(`${ref} is linked to a symbol (${link}): take the symbol off the board in the schematic, then update_from_schematic with remove_extra`);
		const b = courtyardBox(fp)?.box;
		board.remove(fp);
		ctx.project.touchBoard();
		ctx.touchBoard(b ? [b[0] - 1, b[1] - 1, b[2] + 1, b[3] + 1] : undefined);
		ctx.notes.push(`removed ${ref} (${text(fp, 1)})`);
	},
};

export const ops: OpDef[] = [addBoardText, addBoardGraphic, removeBoardItem, addBoardFootprint, removeBoardFootprint];
