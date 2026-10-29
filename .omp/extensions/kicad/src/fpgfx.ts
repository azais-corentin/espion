/**
 * Footprint drawings as plain shapes, grouped by layer: `{"F.SilkS": [{type: "line", start, end,
 * width}, …], …}` in footprint coordinates (mm, y down). One format for `lib_footprint`'s output
 * and the `graphics` input of the footprint ops, so a query result fed back reproduces the drawings.
 */
import type { Pt } from "./geom";
import { child, children, isList, type List, L, num, Sym, str, text } from "./sexpr";
import { type Field, type Fields, validate } from "./spec";

/** A drawing. Queries always give width (text: size, thickness), and fill, rot, justify when not the default. */
export type FpShape =
	| { type: "line"; start: Pt; end: Pt; width?: number }
	| { type: "rect"; start: Pt; end: Pt; width?: number; fill?: boolean }
	| { type: "circle"; center: Pt; radius: number; width?: number; fill?: boolean }
	| { type: "arc"; start: Pt; mid: Pt; end: Pt; width?: number }
	| { type: "poly"; points: Pt[]; width?: number; fill?: boolean }
	| { type: "text"; text: string; at: Pt; size?: number; thickness?: number; rot?: number; justify?: "left" | "center" | "right" };

export type FpGraphics = Record<string, FpShape[]>;

/** Drawing items of a footprint (fp_text only as "user" text; Reference/Value are properties). */
const DRAWINGS = ["fp_line", "fp_rect", "fp_circle", "fp_arc", "fp_poly", "fp_curve", "fp_text", "fp_text_box"];

const point: Field = { t: "point", req: true, doc: "[x, y] mm" };
const width: Field = { t: "number", doc: "stroke width in mm (default by layer)" };
const fill: Field = { t: "boolean", doc: "filled (default false)" };

/** Keys of each shape type, checked with the same spec validation as op arguments. */
const SHAPE_FIELDS: Record<FpShape["type"], Fields> = {
	line: { start: point, end: point, width },
	rect: { start: point, end: point, width, fill },
	circle: { center: point, radius: { t: "number", req: true, doc: "mm" }, width, fill },
	arc: { start: point, mid: point, end: point, width },
	poly: { points: { t: "points", req: true, doc: "[[x, y], …] mm" }, width, fill },
	text: {
		text: { t: "string", req: true, doc: "text" },
		at: point,
		size: { t: "number", doc: "glyph size in mm (default 1)" },
		thickness: { t: "number", doc: "stroke in mm (default 0.15)" },
		rot: { t: "number", doc: "degrees" },
		justify: { t: "string", values: ["left", "center", "right"], doc: "default center" },
	},
};

export const GRAPHICS_DOC =
	'{layer: [shape, …]}: replaces every drawing on each given layer ([] clears it). Shapes, mm, y down: {type:"line", start, end} | {type:"rect", start, end} | {type:"circle", center, radius} | {type:"arc", start, mid, end} | {type:"poly", points} | {type:"text", text, at, size? (1), thickness? (0.15), rot?, justify? left|center|right}; every non-text shape takes width? (default SilkS 0.12, Fab 0.1, CrtYd 0.05, others 0.1), closed ones fill? (false). lib_footprint with graphics: true returns this format';

/** KiCad 10 board layer names usable for footprint drawings. */
const LAYER = /^(([FB]|In\d{1,2})\.Cu|[FB]\.(Adhes|Paste|SilkS|Mask|CrtYd|Fab)|Dwgs\.User|Cmts\.User|Eco[12]\.User|Edge\.Cuts|Margin|User\.\d{1,2})$/;

function pt(l: List | undefined): Pt {
	return [num(l, 1), num(l, 2)];
}

/** A board length atom: footprints resolve 1 nm (up to 6 decimals), unlike schematics (sexpr `fmt`). */
export function mm(v: number): Sym {
	const s = v.toFixed(6).replace(/\.?0+$/, "");
	return new Sym(s === "-0" || s === "" ? "0" : s);
}

/** `(key x y)` in board units. */
export function xy(key: string, p: readonly number[]): List {
	return L(key, mm(p[0]!), mm(p[1]!));
}

/** The shape of one drawing item, or undefined for items outside the shape format (Bezier curves, text boxes). */
function decode(item: List): FpShape | undefined {
	const filled = text(child(item, "fill")) === "yes" ? ({ fill: true } as const) : {};
	const stroke = child(item, "stroke");
	const width = stroke ? num(child(stroke, "width")) : 0;
	switch (text(item, 0)) {
		case "fp_line":
			return { type: "line", start: pt(child(item, "start")), end: pt(child(item, "end")), width };
		case "fp_rect":
			return { type: "rect", start: pt(child(item, "start")), end: pt(child(item, "end")), width, ...filled };
		case "fp_circle": {
			const c = pt(child(item, "center"));
			const e = pt(child(item, "end"));
			// rounded to 1 nm: KiCad's own resolution, without float noise from the square root
			const radius = Math.round(Math.hypot(e[0] - c[0], e[1] - c[1]) * 1e6) / 1e6;
			return { type: "circle", center: c, radius, width, ...filled };
		}
		case "fp_arc":
			return { type: "arc", start: pt(child(item, "start")), mid: pt(child(item, "mid")), end: pt(child(item, "end")), width };
		case "fp_poly":
			return { type: "poly", points: children(child(item, "pts") ?? [], "xy").map(pt), width, ...filled };
		case "fp_text": {
			if (text(item) !== "user") return undefined;
			const at = child(item, "at");
			const effects = child(item, "effects") ?? [];
			const font = child(effects, "font") ?? [];
			const j = (child(effects, "justify") ?? []).map(a => (isList(a) ? "" : a.v));
			const rot = at && at.length > 3 ? num(at, 3) : 0;
			const justify = j.includes("left") ? "left" : j.includes("right") ? "right" : undefined;
			return {
				type: "text",
				text: text(item, 2) ?? "",
				at: pt(at),
				size: num(child(font, "size")),
				thickness: num(child(font, "thickness")),
				...(rot ? { rot } : {}),
				...(justify ? { justify } : {}),
			};
		}
		default:
			return undefined;
	}
}

function isDrawing(item: List): boolean {
	const h = text(item, 0);
	return !!h && DRAWINGS.includes(h) && (h !== "fp_text" || text(item) === "user");
}

/** The footprint's drawings by layer, in file order. */
export function decodeGraphics(fp: List): FpGraphics {
	const out: FpGraphics = {};
	for (const item of fp) {
		if (!isList(item) || !isDrawing(item)) continue;
		const shape = decode(item);
		if (shape) (out[text(child(item, "layer")) ?? ""] ??= []).push(shape);
	}
	return out;
}

/** Validate a `graphics` argument: known layers, lists of shapes in the format above. */
export function graphicsIn(raw: Record<string, unknown>, where = "graphics"): FpGraphics {
	const out: FpGraphics = {};
	for (const [layer, list] of Object.entries(raw)) {
		if (!LAYER.test(layer)) throw new Error(`${where}: "${layer}" is not a KiCad layer name (e.g. F.SilkS, F.Fab, F.CrtYd, Dwgs.User)`);
		if (!Array.isArray(list)) throw new Error(`${where}.${layer} must be a list of shapes`);
		out[layer] = list.map((s: unknown, i) => {
			const at = `${where}.${layer}[${i}]`;
			if (typeof s !== "object" || s === null || Array.isArray(s) || !("type" in s)) throw new Error(`${at}: expected a shape object with "type"`);
			const fields = typeof s.type === "string" ? Object.entries(SHAPE_FIELDS).find(([t]) => t === s.type)?.[1] : undefined;
			if (!fields) throw new Error(`${at}: type must be one of ${Object.keys(SHAPE_FIELDS).join(", ")}, got ${JSON.stringify(s.type)}`);
			const v = validate(at, { ...s }, fields, ["type"]);
			if (s.type === "poly" && (v.points as Pt[]).length < 3) throw new Error(`${at}: a poly needs at least 3 points`);
			if (s.type === "circle" && !((v.radius as number) > 0)) throw new Error(`${at}: radius must be positive`);
			return v as FpShape;
		});
	}
	return out;
}

/** One shape as a KiCad 10 drawing item on `layer`. */
export function encodeShape(s: FpShape, layer: string): List {
	const lay = L("layer", str(layer));
	if (s.type === "text") {
		const size = s.size ?? 1;
		// KiCad mirrors text on the back layers
		const just = [...(s.justify && s.justify !== "center" ? [s.justify] : []), ...(layer.startsWith("B.") ? ["mirror"] : [])];
		return L(
			"fp_text",
			"user",
			str(s.text),
			L("at", mm(s.at[0]), mm(s.at[1]), s.rot ?? 0),
			lay,
			L("effects", L("font", L("size", mm(size), mm(size)), L("thickness", mm(s.thickness ?? 0.15))), ...(just.length ? [L("justify", ...just)] : [])),
		);
	}
	const w = s.width ?? (layer.endsWith(".SilkS") ? 0.12 : layer.endsWith(".CrtYd") ? 0.05 : 0.1);
	const st = L("stroke", L("width", mm(w)), L("type", "solid"));
	const fl = L("fill", "fill" in s && s.fill === true);
	switch (s.type) {
		case "line":
			return L("fp_line", xy("start", s.start), xy("end", s.end), st, lay);
		case "rect":
			return L("fp_rect", xy("start", s.start), xy("end", s.end), st, fl, lay);
		case "circle":
			return L("fp_circle", xy("center", s.center), xy("end", [s.center[0] + s.radius, s.center[1]]), st, fl, lay);
		case "arc":
			return L("fp_arc", xy("start", s.start), xy("mid", s.mid), xy("end", s.end), st, lay);
		case "poly":
			return L("fp_poly", L("pts", ...s.points.map(p => xy("xy", p))), st, fl, lay);
	}
}

/** Remove every drawing on `layers` and add `items` where KiCad keeps drawings (before the pads). */
export function replaceDrawings(fp: List, layers: Iterable<string>, items: List[]): void {
	const drop = new Set(layers);
	for (let i = fp.length - 1; i >= 0; i--) {
		const c = fp[i]!;
		if (isList(c) && isDrawing(c) && drop.has(text(child(c, "layer")) ?? "")) fp.splice(i, 1);
	}
	const at = fp.findIndex(c => isList(c) && ["pad", "zone", "group", "embedded_fonts", "model"].includes(text(c, 0) ?? ""));
	fp.splice(at < 0 ? fp.length : at, 0, ...items);
}

/** Replace the drawings of each layer of `g` with its shapes. */
export function applyGraphics(fp: List, g: FpGraphics): void {
	replaceDrawings(
		fp,
		Object.keys(g),
		Object.entries(g).flatMap(([layer, shapes]) => shapes.map(s => encodeShape(s, layer))),
	);
}
