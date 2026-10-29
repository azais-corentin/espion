/**
 * Library symbol graphics and pins as plain objects, and back to KiCad s-expressions.
 *
 * The object format is the one `lib_symbol` (graphics) returns and `lib_draw_symbol` /
 * `lib_edit_symbol` take: mm relative to the symbol origin in sheet orientation (x right, y down);
 * KiCad stores y up. Sub-symbol items are written in KiCad 10's save order (the comparators of
 * SCH_ITEM / EDA_SHAPE / SCH_TEXT / SCH_PIN), so a KiCad re-save leaves them where they are.
 */
import { DIR_VEC, type Box, type Dir, type Pt, pinDir } from "./geom";
import { child, children, clone, flag, fmt, head, isList, L, type List, num, Str, str, Sym, text } from "./sexpr";

export const PIN_TYPES = [
	"input",
	"output",
	"bidirectional",
	"tri_state",
	"passive",
	"free",
	"unspecified",
	"power_in",
	"power_out",
	"open_collector",
	"open_emitter",
	"no_connect",
] as const;
/** KiCad's GRAPHIC_PINSHAPE tokens, in enum order. */
export const PIN_SHAPES = ["line", "inverted", "clock", "inverted_clock", "input_low", "clock_low", "output_low", "edge_clock_high", "non_logic"] as const;
const DIRS: readonly Dir[] = ["L", "R", "U", "D"];
const FILLS = ["none", "outline", "background"] as const;
const JUSTIFY = ["left", "center", "right"] as const;
/** Library pin angle (pointing from the pin end into the body, y up) for the side the wire leaves. */
const PIN_ANGLE: Record<Dir, number> = { L: 0, R: 180, D: 90, U: 270 };
/** KiCad's pin name offset when a symbol has no (pin_names (offset …)). */
export const PIN_NAME_OFFSET = 0.508;
const FONT = 1.27;

interface Placed {
	/** 0 = common to all units. */
	unit: number;
	/** 0 = common to both body styles, 2 = De Morgan. */
	body_style: number;
}

interface Stroked extends Placed {
	/** Stroke width in mm, 0 = KiCad default. */
	width: number;
	fill: string;
}

export type Shape =
	| (Stroked & { type: "polyline" | "bezier"; points: Pt[] })
	| (Stroked & { type: "rectangle"; start: Pt; end: Pt })
	| (Stroked & { type: "circle"; center: Pt; radius: number })
	| (Stroked & { type: "arc"; start: Pt; mid: Pt; end: Pt })
	| (Placed & { type: "text"; text: string; at: Pt; size: number; rot: number; justify: string });

export interface Pin extends Placed {
	number: string;
	name: string;
	type: string;
	/** Connection end. */
	at: Pt;
	/** Side the wire leaves. */
	dir: Dir;
	length: number;
	shape: string;
	hidden: boolean;
}

/** One drawing item of a symbol with the unit / body style of the sub-symbol it sits in. */
export interface Item {
	unit: number;
	body: number;
	node: List;
}

// ---- reading ---------------------------------------------------------------------------------------

/** Sheet point from library coordinates (`0 - y`, so a 0 stays +0). */
function sheetPt(l: List | undefined, i = 1): Pt {
	return [num(l, i), 0 - num(l, i + 1)];
}

function unitOf(sub: List): [number, number] {
	const m = /_(\d+)_(\d+)$/.exec(text(sub) ?? "");
	return m ? [Number(m[1]), Number(m[2])] : [0, 0];
}

/** The drawing items of a (flattened) symbol and the unit names of its sub-symbols. */
export function symbolItems(sym: List): { items: Item[]; unitNames: Map<number, List> } {
	const items: Item[] = [];
	const unitNames = new Map<number, List>();
	for (const sub of children(sym, "symbol")) {
		const [unit, body] = unitOf(sub);
		for (const n of sub.slice(2)) {
			if (!isList(n)) continue;
			if (head(n) === "unit_name") unitNames.set(unit, n);
			else items.push({ unit, body, node: n });
		}
	}
	return { items, unitNames };
}

/** The item's text string (after an optional `private` keyword). */
function textString(n: List): string {
	const s = n.slice(1).find(x => x instanceof Str);
	return s instanceof Str ? s.v : "";
}

export function readPin(n: List, unit: number, body: number): Pin {
	const at = child(n, "at");
	const hide = child(n, "hide");
	const len = child(n, "length");
	return {
		number: text(child(n, "number")) ?? "",
		name: text(child(n, "name")) ?? "",
		type: text(n, 1) ?? "",
		at: sheetPt(at),
		dir: pinDir(at && at.length > 3 ? num(at, 3) : 0, 0, undefined),
		length: len ? num(len) : 0,
		shape: text(n, 2) ?? "line",
		hidden: !!hide && text(hide) !== "no",
		unit,
		body_style: body,
	};
}

function stroked(n: List, unit: number, body: number): Stroked {
	const w = child(child(n, "stroke") ?? [], "width");
	return { unit, body_style: body, width: w ? num(w) : 0, fill: text(child(child(n, "fill") ?? [], "type")) ?? "none" };
}

/** A shape item in the object format; undefined for items it has no form for (text boxes). */
function readShape(n: List, unit: number, body: number): Shape | undefined {
	const pts = () => children(child(n, "pts") ?? [], "xy").map(p => sheetPt(p));
	switch (head(n)) {
		case "polyline":
		case "bezier":
			return { type: head(n) === "bezier" ? "bezier" : "polyline", points: pts(), ...stroked(n, unit, body) };
		case "rectangle":
			return { type: "rectangle", start: sheetPt(child(n, "start")), end: sheetPt(child(n, "end")), ...stroked(n, unit, body) };
		case "circle":
			return { type: "circle", center: sheetPt(child(n, "center")), radius: num(child(n, "radius")), ...stroked(n, unit, body) };
		case "arc":
			return { type: "arc", start: sheetPt(child(n, "start")), mid: sheetPt(child(n, "mid")), end: sheetPt(child(n, "end")), ...stroked(n, unit, body) };
		case "text": {
			const at = child(n, "at");
			const fx = child(n, "effects");
			const size = child(child(fx ?? [], "font") ?? [], "size");
			const just = child(fx ?? [], "justify")?.slice(1).map(a => (isList(a) ? "" : a.v)) ?? [];
			return {
				type: "text",
				text: textString(n),
				at: sheetPt(at),
				size: size ? num(size) : FONT,
				rot: at && at.length > 3 ? num(at, 3) / 10 : 0,
				justify: just.find(j => j === "left" || j === "right") ?? "center",
				unit,
				body_style: body,
			};
		}
		default:
			return undefined;
	}
}

/** Shapes, pins and pin text settings of a flattened symbol, in the lib_draw_symbol input format. */
export function symbolGraphics(sym: List): {
	pin_name_offset: number;
	hide_pin_names: boolean;
	hide_pin_numbers: boolean;
	pins: Record<string, unknown>[];
	shapes: Record<string, unknown>[];
} {
	const { items } = symbolItems(sym);
	const names = child(sym, "pin_names");
	const offset = names && child(names, "offset");
	const shapes: Record<string, unknown>[] = [];
	for (const it of items) {
		const s = head(it.node) === "pin" ? undefined : readShape(it.node, it.unit, it.body);
		if (s) shapes.push(shapeJson(s));
	}
	return {
		pin_name_offset: offset ? num(offset) : PIN_NAME_OFFSET,
		hide_pin_names: !!names && !!flag(names, "hide"),
		hide_pin_numbers: !!child(sym, "pin_numbers") && !!flag(child(sym, "pin_numbers")!, "hide"),
		pins: items.filter(it => head(it.node) === "pin").map(it => pinJson(readPin(it.node, it.unit, it.body))),
		shapes,
	};
}

/** Object form with the defaults left out (unit 0, body style 1, width 0, fill none, text size 1.27, rot 0, centred). */
function shapeJson(s: Shape): Record<string, unknown> {
	const { unit, body_style, ...rest } = s;
	const out: Record<string, unknown> = { ...rest };
	if (s.type === "text") {
		if (s.size === FONT) delete out.size;
		if (s.rot === 0) delete out.rot;
		if (s.justify === "center") delete out.justify;
	} else {
		if (s.width === 0) delete out.width;
		if (s.fill === "none") delete out.fill;
	}
	if (unit !== 0) out.unit = unit;
	if (body_style !== 1) out.body_style = body_style;
	return out;
}

/** Pin object: always number, name, type, at, dir, length, unit, hidden; shape and body_style when not line / 1. */
function pinJson(p: Pin): Record<string, unknown> {
	const { shape, body_style, ...rest } = p;
	return { ...rest, ...(shape !== "line" ? { shape } : {}), ...(body_style !== 1 ? { body_style } : {}) };
}

// ---- input -----------------------------------------------------------------------------------------

function point(v: unknown, where: string): Pt {
	if (!Array.isArray(v) || v.length !== 2 || !v.every(x => typeof x === "number" && Number.isFinite(x))) throw new Error(`${where} must be [x, y] in mm`);
	return [v[0], v[1]];
}

function numberIn(o: Record<string, unknown>, k: string, where: string, dflt: number, min = -Infinity): number {
	const v = o[k];
	if (v === undefined) return dflt;
	if (typeof v !== "number" || !Number.isFinite(v) || v < min) throw new Error(`${where}: "${k}" must be a number${min > -Infinity ? ` ≥ ${min}` : ""}, got ${JSON.stringify(v)}`);
	return v;
}

function choiceIn<T>(o: Record<string, unknown>, k: string, where: string, values: readonly T[], dflt: T): T {
	const v = o[k];
	if (v === undefined) return dflt;
	const hit = values.find(x => x === v);
	if (hit === undefined) throw new Error(`${where}: "${k}" must be one of ${values.join(", ")}, got ${JSON.stringify(v)}`);
	return hit;
}

function stringIn(o: Record<string, unknown>, k: string, where: string): string | undefined {
	const v = o[k];
	if (v === undefined) return undefined;
	if (typeof v === "number") return String(v);
	if (typeof v !== "string") throw new Error(`${where}: "${k}" must be a string, got ${JSON.stringify(v)}`);
	return v;
}

function checkKeys(o: Record<string, unknown>, keys: readonly string[], where: string): void {
	for (const k of Object.keys(o)) if (!keys.includes(k)) throw new Error(`${where}: unknown key "${k}" (allowed: ${keys.join(", ")})`);
}

const BODY_STYLES = [0, 1, 2] as const;

const SHAPE_KEYS: Record<Shape["type"], readonly string[]> = {
	polyline: ["points"],
	bezier: ["points"],
	rectangle: ["start", "end"],
	circle: ["center", "radius"],
	arc: ["start", "mid", "end"],
	text: ["text", "at", "size", "rot", "justify"],
};
const SHAPE_TYPES = Object.keys(SHAPE_KEYS) as readonly string[];

function unitIn(o: Record<string, unknown>, where: string, dflt: number): number {
	const v = numberIn(o, "unit", where, dflt, 0);
	if (!Number.isInteger(v)) throw new Error(`${where}: "unit" must be an integer (0 = every unit)`);
	return v;
}

/** Validate shape objects (see the lib_draw_symbol field doc). */
export function shapesIn(raw: Record<string, unknown>[], field = "shapes"): Shape[] {
	return raw.map((o, i): Shape => {
		const where = `${field}[${i}]`;
		const type = choiceIn(o, "type", where, SHAPE_TYPES, "");
		if (!type) throw new Error(`${where}: "type" is required (${SHAPE_TYPES.join(", ")})`);
		const t = type as Shape["type"];
		const placed: Placed = { unit: unitIn(o, where, 0), body_style: choiceIn(o, "body_style", where, BODY_STYLES, 1) };
		if (t === "text") {
			checkKeys(o, ["type", "unit", "body_style", ...SHAPE_KEYS.text], where);
			const s = stringIn(o, "text", where);
			if (!s) throw new Error(`${where}: "text" is required`);
			return {
				type: t,
				text: s,
				at: point(o.at, `${where}.at`),
				size: numberIn(o, "size", where, FONT, 0.01),
				rot: choiceIn(o, "rot", where, [0, 90], 0),
				justify: choiceIn(o, "justify", where, JUSTIFY, "center"),
				...placed,
			};
		}
		checkKeys(o, ["type", "unit", "body_style", "width", "fill", ...SHAPE_KEYS[t]], where);
		const base: Stroked = { ...placed, width: numberIn(o, "width", where, 0, 0), fill: choiceIn(o, "fill", where, FILLS, "none") };
		switch (t) {
			case "polyline":
			case "bezier": {
				const pts = o.points;
				if (!Array.isArray(pts)) throw new Error(`${where}: "points" must be a list of [x, y]`);
				const points = pts.map((p, j) => point(p, `${where}.points[${j}]`));
				if (t === "bezier" ? points.length !== 4 : points.length < 2) throw new Error(`${where}: a ${t} needs ${t === "bezier" ? "exactly 4" : "at least 2"} points`);
				return { type: t, points, ...base };
			}
			case "rectangle":
				return { type: t, start: point(o.start, `${where}.start`), end: point(o.end, `${where}.end`), ...base };
			case "circle": {
				const radius = numberIn(o, "radius", where, 0, 0);
				if (!radius) throw new Error(`${where}: "radius" must be a positive number`);
				return { type: t, center: point(o.center, `${where}.center`), radius, ...base };
			}
			case "arc":
				return { type: t, start: point(o.start, `${where}.start`), mid: point(o.mid, `${where}.mid`), end: point(o.end, `${where}.end`), ...base };
		}
	});
}

const PIN_KEYS = ["number", "name", "type", "at", "dir", "length", "shape", "hidden", "unit", "body_style"] as const;

/** Validate full pin objects (lib_draw_symbol). */
export function pinsIn(raw: Record<string, unknown>[]): Pin[] {
	return raw.map((o, i) => {
		const where = `pins[${i}]`;
		checkKeys(o, PIN_KEYS, where);
		const number = stringIn(o, "number", where);
		const name = stringIn(o, "name", where);
		const type = choiceIn(o, "type", where, PIN_TYPES, undefined);
		const dir = choiceIn(o, "dir", where, DIRS, undefined);
		if (!number || name === undefined || !type || !dir) throw new Error(`${where}: "number", "name" ("" for none), "type", "at" and "dir" are required`);
		const hidden = o.hidden;
		if (hidden !== undefined && typeof hidden !== "boolean") throw new Error(`${where}: "hidden" must be true or false`);
		return {
			number,
			name,
			type,
			at: point(o.at, `${where}.at`),
			dir,
			length: numberIn(o, "length", where, 2.54, 0),
			shape: choiceIn(o, "shape", where, PIN_SHAPES, "line"),
			hidden: hidden === true,
			unit: unitIn(o, where, 1),
			body_style: choiceIn(o, "body_style", where, BODY_STYLES, 1),
		};
	});
}

/** One pin edit of lib_edit_symbol: only the given keys change. */
export interface PinEdit {
	number: string;
	name?: string;
	type?: string;
	at?: Pt;
	dir?: Dir;
	length?: number;
	shape?: string;
	hidden?: boolean;
	unit?: number;
	remove: boolean;
}

export function pinEditsIn(raw: Record<string, unknown>[]): PinEdit[] {
	return raw.map((o, i) => {
		const where = `pins[${i}]`;
		checkKeys(o, ["number", "name", "type", "at", "dir", "length", "shape", "hidden", "unit", "remove"], where);
		const number = stringIn(o, "number", where);
		if (!number) throw new Error(`${where}: "number" is required`);
		if (o.remove !== undefined && o.remove !== true) throw new Error(`${where}: "remove" must be true when given`);
		if (o.remove && Object.keys(o).length > 2) throw new Error(`${where}: "remove" takes no other keys than "number"`);
		if (o.hidden !== undefined && typeof o.hidden !== "boolean") throw new Error(`${where}: "hidden" must be true or false`);
		return {
			number,
			name: stringIn(o, "name", where),
			type: choiceIn(o, "type", where, PIN_TYPES, undefined),
			at: o.at === undefined ? undefined : point(o.at, `${where}.at`),
			dir: choiceIn(o, "dir", where, DIRS, undefined),
			length: o.length === undefined ? undefined : numberIn(o, "length", where, 0, 0),
			shape: choiceIn(o, "shape", where, PIN_SHAPES, undefined),
			hidden: typeof o.hidden === "boolean" ? o.hidden : undefined,
			unit: o.unit === undefined ? undefined : unitIn(o, where, 1),
			remove: o.remove === true,
		};
	});
}

// ---- writing ---------------------------------------------------------------------------------------

function xy(key: string, p: Pt): List {
	return L(key, p[0], 0 - p[1]);
}

function strokeFill(s: Stroked): List[] {
	return [L("stroke", L("width", s.width), L("type", "default")), L("fill", L("type", s.fill))];
}

/**
 * KiCad keeps arcs clockwise in library coordinates (y up), i.e. counter-clockwise on the sheet,
 * and swaps start and end of the others on load.
 */
function canonicalArc(start: Pt, mid: Pt, end: Pt): [Pt, Pt] {
	const cross = (mid[0] - start[0]) * (end[1] - mid[1]) - (mid[1] - start[1]) * (end[0] - mid[0]);
	return cross < 0 ? [end, start] : [start, end];
}

export function shapeNode(s: Shape): List {
	switch (s.type) {
		case "polyline":
		case "bezier":
			return L(s.type, L("pts", ...s.points.map(p => xy("xy", p))), ...strokeFill(s));
		case "rectangle":
			return L("rectangle", xy("start", s.start), xy("end", s.end), ...strokeFill(s));
		case "circle":
			return L("circle", xy("center", s.center), L("radius", s.radius), ...strokeFill(s));
		case "arc": {
			const [a, b] = canonicalArc(s.start, s.mid, s.end);
			return L("arc", xy("start", a), xy("mid", s.mid), xy("end", b), ...strokeFill(s));
		}
		case "text":
			return L(
				"text",
				str(s.text),
				L("at", s.at[0], 0 - s.at[1], s.rot * 10),
				L("effects", L("font", L("size", s.size, s.size)), ...(s.justify !== "center" ? [L("justify", s.justify)] : [])),
			);
	}
}

export function pinNode(p: Pin): List {
	const font = L("effects", L("font", L("size", FONT, FONT)));
	return L(
		"pin",
		p.type,
		p.shape,
		L("at", p.at[0], 0 - p.at[1], PIN_ANGLE[p.dir]),
		L("length", p.length),
		...(p.hidden ? [L("hide", true)] : []),
		L("name", str(p.name), font),
		L("number", str(p.number), clone(font)),
	);
}

/** Apply one edit to a pin node in place (unit moves are the caller's). */
export function editPinNode(n: List, e: PinEdit): void {
	if (e.name !== undefined) child(n, "name")![1] = str(e.name);
	if (e.type) n[1] = new Sym(e.type);
	if (e.shape) n[2] = new Sym(e.shape);
	const at = child(n, "at")!;
	if (e.at) {
		at[1] = new Sym(fmt(e.at[0]));
		at[2] = new Sym(fmt(0 - e.at[1]));
	}
	if (e.dir) at[3] = new Sym(fmt(PIN_ANGLE[e.dir]));
	if (e.length !== undefined) child(n, "length")![1] = new Sym(fmt(e.length));
	if (e.hidden !== undefined) {
		const hide = child(n, "hide");
		if (hide) n.splice(n.indexOf(hide), 1);
		if (e.hidden) n.splice(n.indexOf(child(n, "length")!) + 1, 0, L("hide", true));
	}
}

// ---- KiCad's save order ----------------------------------------------------------------------------

/** Internal units (100 nm), y down, as KiCad compares them. */
function iu(l: List | undefined, i = 1): [number, number] {
	return [Math.round(num(l, i) * 10000), Math.round(-num(l, i + 1) * 10000)];
}

const KIND_RANK: Record<string, number> = { polyline: 0, rectangle: 0, circle: 0, arc: 0, bezier: 0, text: 1, text_box: 2, pin: 3 };
const SHAPE_ENUM: Record<string, number> = { rectangle: 1, arc: 2, circle: 3, polyline: 4, bezier: 5 };
const LINE_STYLE: Record<string, number> = { default: -1, solid: 0, dash: 1, dot: 2, dash_dot: 3, dash_dot_dot: 4 };
const FILL_ENUM: Record<string, number> = { none: 1, outline: 2, background: 3, color: 4, hatch: 5, reverse_hatch: 6, cross_hatch: 7 };
const PIN_ORIENT: Record<string, number> = { "0": 0, "180": 1, "90": 2, "270": 3 };

function circumcenter(a: [number, number], b: [number, number], c: [number, number]): [number, number] {
	const d = 2 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]));
	const s = (p: [number, number]) => p[0] * p[0] + p[1] * p[1];
	const x = (s(a) * (b[1] - c[1]) + s(b) * (c[1] - a[1]) + s(c) * (a[1] - b[1])) / d;
	const y = (s(a) * (c[0] - b[0]) + s(b) * (a[0] - c[0]) + s(c) * (b[0] - a[0])) / d;
	return [Math.round(x), Math.round(y)];
}

/** EDA_SHAPE geometry as compared by SCH_SHAPE::compare: position, start, end, extra points, vertices. */
function shapeKey(n: List): { pos: [number, number]; start: [number, number]; end: [number, number]; extra: [number, number][]; count: number; verts: [number, number][] } {
	const pts = children(child(n, "pts") ?? [], "xy").map(p => iu(p));
	const zero: [number, number] = [0, 0];
	switch (head(n)) {
		case "rectangle": {
			const s = iu(child(n, "start"));
			return { pos: s, start: s, end: iu(child(n, "end")), extra: [], count: 0, verts: [] };
		}
		case "circle": {
			const c = iu(child(n, "center"));
			return { pos: c, start: c, end: [c[0] + Math.round(num(child(n, "radius")) * 10000), c[1]], extra: [], count: 0, verts: [] };
		}
		case "arc": {
			const [s, m, e] = [iu(child(n, "start")), iu(child(n, "mid")), iu(child(n, "end"))];
			return { pos: circumcenter(s, m, e), start: s, end: e, extra: [m], count: 0, verts: [] };
		}
		case "bezier":
			return { pos: pts[0] ?? zero, start: pts[0] ?? zero, end: pts[3] ?? zero, extra: [pts[1] ?? zero, pts[2] ?? zero], count: 0, verts: [] };
		default:
			return { pos: pts[0] ?? zero, start: zero, end: zero, extra: [], count: pts.length, verts: pts };
	}
}

/** TEST_PT of EDA_SHAPE::Compare: differences up to 2 IU count as equal. */
function cmpPtE(a: [number, number], b: [number, number]): number {
	if (Math.abs(a[0] - b[0]) > 2) return a[0] - b[0];
	if (Math.abs(a[1] - b[1]) > 2) return a[1] - b[1];
	return 0;
}

function cmpShapes(a: List, b: List): number {
	const ka = shapeKey(a);
	const kb = shapeKey(b);
	if (ka.pos[0] !== kb.pos[0]) return ka.pos[0] - kb.pos[0];
	if (ka.pos[1] !== kb.pos[1]) return ka.pos[1] - kb.pos[1];
	const steps: (() => number)[] = [
		() => cmpPtE(ka.start, kb.start),
		() => cmpPtE(ka.end, kb.end),
		() => SHAPE_ENUM[head(a)!]! - SHAPE_ENUM[head(b)!]!,
		() => ka.extra.reduce((r, p, i) => r || cmpPtE(p, kb.extra[i]!), 0),
		() => ka.count - kb.count,
		() => ka.verts.reduce((r, p, i) => r || cmpPtE(p, kb.verts[i]!), 0),
		() => {
			const w = (n: List) => Math.round(num(child(child(n, "stroke") ?? [], "width") ?? L("width", 0)) * 10000);
			return Math.abs(w(a) - w(b)) > 2 ? w(a) - w(b) : 0;
		},
		() => (LINE_STYLE[text(child(child(a, "stroke") ?? [], "type")) ?? "default"] ?? 0) - (LINE_STYLE[text(child(child(b, "stroke") ?? [], "type")) ?? "default"] ?? 0),
		() => (FILL_ENUM[text(child(child(a, "fill") ?? [], "type")) ?? "none"] ?? 0) - (FILL_ENUM[text(child(child(b, "fill") ?? [], "type")) ?? "none"] ?? 0),
	];
	for (const s of steps) {
		const r = s();
		if (r) return r;
	}
	return 0;
}

/** KiCad's StrNumCmp: digit runs compare as numbers. */
function strNumCmp(a: string, b: string): number {
	let i = 0;
	let j = 0;
	const digit = (c: string | undefined) => c !== undefined && c >= "0" && c <= "9";
	while (i < a.length && j < b.length) {
		let c1 = a[i];
		let c2 = b[j];
		if (digit(c1) && digit(c2)) {
			let n1 = 0;
			let n2 = 0;
			do n1 = n1 * 10 + Number(a[i++]);
			while (i < a.length && digit(a[i]));
			do n2 = n2 * 10 + Number(b[j++]);
			while (j < b.length && digit(b[j]));
			if (n1 !== n2) return n1 < n2 ? -1 : 1;
			c1 = i < a.length ? a[i] : "\0";
			c2 = j < b.length ? b[j] : "\0";
		}
		if (c1! < c2!) return -1;
		if (c1! > c2!) return 1;
		if (i < a.length) i++;
		if (j < b.length) j++;
	}
	if (i >= a.length && j < b.length) return -1;
	if (i < a.length && j >= b.length) return 1;
	return 0;
}

function cmpPins(a: List, b: List): number {
	const r = strNumCmp(text(child(a, "number")) ?? "", text(child(b, "number")) ?? "");
	if (r) return r;
	const [pa, pb] = [iu(child(a, "at")), iu(child(b, "at"))];
	if (pa[0] !== pb[0]) return pa[0] - pb[0];
	if (pa[1] !== pb[1]) return pa[1] - pb[1];
	const len = (n: List) => Math.round(num(child(n, "length")) * 10000);
	if (len(a) !== len(b)) return len(a) - len(b);
	const orient = (n: List) => PIN_ORIENT[text(child(n, "at"), 3) ?? "0"] ?? 0;
	if (orient(a) !== orient(b)) return orient(a) - orient(b);
	const shape = (n: List) => PIN_SHAPES.findIndex(s => s === text(n, 2));
	if (shape(a) !== shape(b)) return shape(a) - shape(b);
	const type = (n: List) => PIN_TYPES.findIndex(s => s === text(n, 1));
	return type(a) - type(b);
}

/** SCH_ITEM ordering of one sub-symbol's items (stable for ties, like KiCad's multiset). */
function cmpItems(a: List, b: List): number {
	const ra = KIND_RANK[head(a) ?? ""] ?? 9;
	const rb = KIND_RANK[head(b) ?? ""] ?? 9;
	if (ra !== rb) return ra - rb;
	switch (ra) {
		case 0:
			return cmpShapes(a, b);
		case 1: {
			const [pa, pb] = [iu(child(a, "at")), iu(child(b, "at"))];
			if (pa[0] !== pb[0]) return pa[0] - pb[0];
			if (pa[1] !== pb[1]) return pa[1] - pb[1];
			const [ta, tb] = [textString(a), textString(b)];
			return ta < tb ? -1 : ta > tb ? 1 : 0;
		}
		case 3:
			return cmpPins(a, b);
		default:
			return 0;
	}
}

/** Sub-symbols `<name>_<unit>_<body>` holding the items, in KiCad's order, with their unit names. */
export function subSymbols(name: string, items: Item[], unitNames: Map<number, List>): List[] {
	const groups = new Map<string, Item[]>();
	for (const it of items) {
		const k = `${it.unit}_${it.body}`;
		groups.set(k, [...(groups.get(k) ?? []), it]);
	}
	return [...groups.values()]
		.sort((a, b) => a[0]!.unit - b[0]!.unit || a[0]!.body - b[0]!.body)
		.map(g => {
			const { unit, body } = g[0]!;
			const un = unitNames.get(unit);
			return L("symbol", str(`${name}_${unit}_${body}`), ...(un ? [un] : []), ...g.map(it => it.node).sort(cmpItems));
		});
}

/** Replace a symbol's sub-symbols (kept before embedded_fonts, after the properties). */
export function setSubSymbols(sym: List, subs: List[]): void {
	const old = children(sym, "symbol");
	const ef = child(sym, "embedded_fonts");
	let at = old.length ? sym.indexOf(old[0]!) : ef ? sym.indexOf(ef) : sym.length;
	for (const s of old) sym.splice(sym.indexOf(s), 1);
	at = Math.min(at, sym.length);
	sym.splice(at, 0, ...subs);
}

// ---- extent ----------------------------------------------------------------------------------------

/** Approximate drawn extent in sheet coordinates (text by its size and a 0.6 em glyph width). */
export function drawExtent(shapes: Shape[], pins: Pin[]): Box {
	const ps: Pt[] = [];
	for (const s of shapes) {
		switch (s.type) {
			case "polyline":
			case "bezier":
				ps.push(...s.points);
				break;
			case "rectangle":
				ps.push(s.start, s.end);
				break;
			case "circle":
				ps.push([s.center[0] - s.radius, s.center[1] - s.radius], [s.center[0] + s.radius, s.center[1] + s.radius]);
				break;
			case "arc":
				ps.push(s.start, s.mid, s.end);
				break;
			case "text": {
				// along the reading direction (rightwards, or upwards at rot 90): from lo to lo + w
				const w = s.text.length * s.size * 0.6;
				const lo = s.justify === "left" ? 0 : s.justify === "right" ? -w : -w / 2;
				const h = s.size / 2;
				if (s.rot === 90) ps.push([s.at[0] - h, s.at[1] - lo - w], [s.at[0] + h, s.at[1] - lo]);
				else ps.push([s.at[0] + lo, s.at[1] - h], [s.at[0] + lo + w, s.at[1] + h]);
				break;
			}
		}
	}
	for (const p of pins) {
		const [dx, dy] = DIR_VEC[p.dir];
		ps.push(p.at, [p.at[0] - dx * p.length, p.at[1] - dy * p.length]);
	}
	let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
	for (const [x, y] of ps) [x0, y0, x1, y1] = [Math.min(x0, x), Math.min(y0, y), Math.max(x1, x), Math.max(y1, y)];
	return [x0, y0, x1, y1];
}

/** drawExtent of a symbol's drawing items (shapes and pins; text boxes are left out). */
export function itemsExtent(items: Item[]): Box {
	const shapes: Shape[] = [];
	const pins: Pin[] = [];
	for (const it of items) {
		if (head(it.node) === "pin") pins.push(readPin(it.node, it.unit, it.body));
		else {
			const s = readShape(it.node, it.unit, it.body);
			if (s) shapes.push(s);
		}
	}
	return drawExtent(shapes, pins);
}
