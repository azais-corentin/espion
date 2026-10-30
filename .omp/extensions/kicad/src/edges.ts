/**
 * Board outline (Edge.Cuts) shapes: the set_board_edges input format, its validation, the KiCad
 * items it is drawn with, and the way back from the items on a board to the same shapes.
 *
 * Board millimetres, x right, y down. A polygon vertex may carry a fillet radius `r` (concave
 * corners too): the corner is drawn as a tangent arc. Polygons are normalised so a read-back is
 * exact: they start at the top-most (then left-most) vertex and run clockwise as seen on screen,
 * whatever order KiCad writes the lines and arcs in.
 */
import { mm, xy } from "./fpgfx";
import { type Box, bbox, type Pt } from "./geom";
import { child, head, L, type List, num, str, text } from "./sexpr";

export type Vertex = [number, number] | [number, number, number];

export interface RectShape {
	rect: { x: number; y: number; width: number; height: number; corner_radius?: number };
}
export interface CircleShape {
	circle: { center: Pt; radius: number };
}
export interface PolyShape {
	points: Vertex[];
}
export type EdgeShape = RectShape | CircleShape | PolyShape;

/** A loop KiCad's items form that isn't one of the shapes (Bezier curves, arcs meeting arcs, open chains). */
export interface RawLoop {
	items: { type: string; points: Pt[] }[];
	closed: boolean;
}

export type ReadShape = EdgeShape | RawLoop;

const EPS = 1e-6;
/** Endpoints closer than this (mm) are joined when chaining KiCad's items: 10 nm. */
const JOIN = 1e-5;
/** A read-back polygon redraws its fillets from vertices rounded to 0.1 µm: arcs closer than this (mm) to KiCad's are the same: 0.5 µm. */
const REDRAW = 5e-4;

const r4 = (v: number) => Math.round(v * 1e4) / 1e4 + 0;
const sub = (a: Pt, b: Pt): Pt => [a[0] - b[0], a[1] - b[1]];
const len = (a: Pt) => Math.hypot(a[0], a[1]);
const unit = (a: Pt): Pt => {
	const l = len(a);
	return [a[0] / l, a[1] / l];
};
const cross = (a: Pt, b: Pt) => a[0] * b[1] - a[1] * b[0];
const dot = (a: Pt, b: Pt) => a[0] * b[0] + a[1] * b[1];

// ---- input validation ---------------------------------------------------------------------------

function finite(v: unknown): v is number {
	return typeof v === "number" && Number.isFinite(v);
}

function ptIn(v: unknown, where: string): Pt {
	if (!Array.isArray(v) || v.length !== 2 || !v.every(finite)) throw new Error(`${where}: expected [x, y] in mm, got ${JSON.stringify(v)}`);
	return [v[0], v[1]];
}

function keysOnly(o: Record<string, unknown>, allowed: string[], where: string): void {
	for (const k of Object.keys(o)) if (!allowed.includes(k)) throw new Error(`${where}: unknown key "${k}" (allowed: ${allowed.join(", ")})`);
}

function obj(v: unknown, where: string): Record<string, unknown> {
	if (typeof v !== "object" || v === null || Array.isArray(v)) throw new Error(`${where}: expected an object, got ${JSON.stringify(v)}`);
	return v as Record<string, unknown>;
}

/** Parse one shape of the op's input ({rect} | {circle} | {points}), checking its own geometry. */
export function shapeIn(raw: unknown, where: string, allowCircle: boolean): EdgeShape {
	const o = obj(raw, where);
	const kinds = ["rect", "points", ...(allowCircle ? ["circle"] : [])];
	const given = Object.keys(o);
	if (given.length !== 1 || !kinds.includes(given[0]!)) throw new Error(`${where}: give exactly one of ${kinds.join(", ")}, got ${JSON.stringify(given)}`);
	if ("rect" in o) {
		const r = obj(o.rect, `${where}.rect`);
		keysOnly(r, ["x", "y", "width", "height", "corner_radius"], `${where}.rect`);
		for (const k of ["x", "y", "width", "height"]) if (!finite(r[k])) throw new Error(`${where}.rect.${k}: expected a number (mm)`);
		const rect = { x: r.x as number, y: r.y as number, width: r.width as number, height: r.height as number };
		if (!(rect.width > 0 && rect.height > 0)) throw new Error(`${where}.rect: width and height must be positive`);
		const cr = r.corner_radius;
		if (cr !== undefined && (!finite(cr) || cr < 0)) throw new Error(`${where}.rect.corner_radius: expected a number >= 0 (mm)`);
		if (cr !== undefined && (cr as number) > Math.min(rect.width, rect.height) / 2 + EPS) {
			throw new Error(`${where}.rect.corner_radius ${cr} mm doesn't fit: at most half the shorter side (${Math.min(rect.width, rect.height) / 2} mm)`);
		}
		return { rect: cr ? { ...rect, corner_radius: cr as number } : rect };
	}
	if ("circle" in o) {
		const c = obj(o.circle, `${where}.circle`);
		keysOnly(c, ["center", "radius"], `${where}.circle`);
		const center = ptIn(c.center, `${where}.circle.center`);
		if (!finite(c.radius) || !(c.radius > 0)) throw new Error(`${where}.circle.radius: expected a positive number (mm)`);
		return { circle: { center, radius: c.radius } };
	}
	if (!Array.isArray(o.points)) throw new Error(`${where}.points: expected [[x, y, r?], …]`);
	const points = o.points.map((p, i): Vertex => {
		const w = `${where}.points[${i}]`;
		if (!Array.isArray(p) || (p.length !== 2 && p.length !== 3) || !p.every(finite)) throw new Error(`${w}: expected [x, y] or [x, y, r] in mm, got ${JSON.stringify(p)}`);
		if (p.length === 3 && p[2] < 0) throw new Error(`${w}: fillet radius must be >= 0`);
		return p.length === 3 && p[2] > 0 ? [p[0], p[1], p[2]] : [p[0], p[1]];
	});
	checkPolygon(points, `${where}.points`);
	return { points };
}

/** Each vertex's corner: the unit directions to the previous and next vertex, and the corner angle (rad, 0..π). */
function corners(pts: Vertex[]): { toPrev: Pt; toNext: Pt; angle: number }[] {
	const n = pts.length;
	return pts.map((p, i) => {
		const prev = pts[(i + n - 1) % n]!;
		const next = pts[(i + 1) % n]!;
		const a = unit(sub([prev[0], prev[1]], [p[0], p[1]]));
		const b = unit(sub([next[0], next[1]], [p[0], p[1]]));
		return { toPrev: a, toNext: b, angle: Math.acos(Math.max(-1, Math.min(1, dot(a, b)))) };
	});
}

/** Distance from a vertex to where its fillet arc meets each edge. */
function tangentLength(r: number, angle: number): number {
	return r ? r / Math.tan(angle / 2) : 0;
}

function segmentsCross(a: Pt, b: Pt, c: Pt, d: Pt): boolean {
	const d1 = cross(sub(b, a), sub(c, a));
	const d2 = cross(sub(b, a), sub(d, a));
	const d3 = cross(sub(d, c), sub(a, c));
	const d4 = cross(sub(d, c), sub(b, c));
	if (((d1 > EPS && d2 < -EPS) || (d1 < -EPS && d2 > EPS)) && ((d3 > EPS && d4 < -EPS) || (d3 < -EPS && d4 > EPS))) return true;
	const on = (p: Pt, q: Pt, r: Pt) => Math.abs(cross(sub(q, p), sub(r, p))) <= EPS && Math.min(p[0], q[0]) - EPS <= r[0] && r[0] <= Math.max(p[0], q[0]) + EPS && Math.min(p[1], q[1]) - EPS <= r[1] && r[1] <= Math.max(p[1], q[1]) + EPS;
	return on(a, b, c) || on(a, b, d) || on(c, d, a) || on(c, d, b);
}

/** ≥ 3 distinct vertices, no straight or folded-back corners, no self-intersection, every fillet fits its two edges. */
function checkPolygon(pts: Vertex[], where: string): void {
	const n = pts.length;
	if (n < 3) throw new Error(`${where}: a polygon needs at least 3 vertices, got ${n}`);
	const P = (i: number): Pt => [pts[i % n]![0], pts[i % n]![1]];
	for (let i = 0; i < n; i++) if (len(sub(P(i + 1), P(i))) < 1e-4) throw new Error(`${where}: vertices ${i} and ${(i + 1) % n} coincide`);
	const cs = corners(pts);
	cs.forEach((c, i) => {
		if (c.angle < 1e-6) throw new Error(`${where}: the edges at vertex ${i} fold back onto each other`);
		if (Math.PI - c.angle < 1e-6 && (pts[i]![2] ?? 0) > 0) throw new Error(`${where}: vertex ${i} is on a straight line: it has no corner to fillet`);
	});
	for (let i = 0; i < n; i++) {
		for (let j = i + 1; j < n; j++) {
			if (j === i + 1 || (i === 0 && j === n - 1)) continue;
			if (segmentsCross(P(i), P(i + 1), P(j), P(j + 1))) {
				throw new Error(`${where}: not a simple polygon, edge ${i}–${(i + 1) % n} crosses or touches edge ${j}–${(j + 1) % n}`);
			}
		}
	}
	for (let i = 0; i < n; i++) {
		const j = (i + 1) % n;
		const need = tangentLength(pts[i]![2] ?? 0, cs[i]!.angle) + tangentLength(pts[j]![2] ?? 0, cs[j]!.angle);
		const have = len(sub(P(j), P(i)));
		if (need > have + 1e-6) {
			throw new Error(
				`${where}: the fillets at vertices ${i} and ${j} need ${r4(need)} mm of edge ${i}–${j}, which is ${r4(have)} mm long: make the radii smaller`,
			);
		}
	}
}

// ---- outline polygons (for containment) ---------------------------------------------------------

/** Points of an arc from start through mid to end, at most `step` radians apart (start and end included). */
export function arcPoints(start: Pt, mid: Pt, end: Pt, step = Math.PI / 90): Pt[] {
	const c = circleThrough(start, mid, end);
	if (!c) return [start, end];
	const ang = (p: Pt) => Math.atan2(p[1] - c.center[1], p[0] - c.center[0]);
	const a0 = ang(start);
	let a1 = ang(end);
	const am = ang(mid);
	// sweep direction: the one that passes the mid point
	const norm = (x: number) => ((x % (2 * Math.PI)) + 2 * Math.PI) % (2 * Math.PI);
	const ccwToMid = norm(am - a0);
	const ccwToEnd = norm(a1 - a0);
	let sweep = ccwToMid <= ccwToEnd ? ccwToEnd : ccwToEnd - 2 * Math.PI;
	if (Math.abs(sweep) < 1e-12) sweep = 2 * Math.PI;
	a1 = a0 + sweep;
	const k = Math.max(2, Math.ceil(Math.abs(sweep) / step));
	const out: Pt[] = [];
	for (let i = 0; i <= k; i++) {
		const a = a0 + (sweep * i) / k;
		out.push(i === 0 ? start : i === k ? end : [c.center[0] + c.r * Math.cos(a), c.center[1] + c.r * Math.sin(a)]);
	}
	return out;
}

export function circleThrough(a: Pt, b: Pt, c: Pt): { center: Pt; r: number } | undefined {
	const d = 2 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]));
	if (Math.abs(d) < 1e-12) return undefined;
	const s = (p: Pt) => p[0] * p[0] + p[1] * p[1];
	const x = (s(a) * (b[1] - c[1]) + s(b) * (c[1] - a[1]) + s(c) * (a[1] - b[1])) / d;
	const y = (s(a) * (c[0] - b[0]) + s(b) * (a[0] - c[0]) + s(c) * (b[0] - a[0])) / d;
	return { center: [x, y], r: Math.hypot(a[0] - x, a[1] - y) };
}

/** The drawn boundary of a fillet polygon: lines and tangent arcs, as segments of the arcs (`step` rad). */
interface Piece {
	kind: "line" | "arc";
	start: Pt;
	end: Pt;
	mid?: Pt;
}

/** Lines and tangent arcs of a polygon with fillets, in vertex order (zero-length lines left out). */
function pieces(pts: Vertex[]): Piece[] {
	const n = pts.length;
	const cs = corners(pts);
	const arcs = pts.map((p, i) => {
		const r = p[2] ?? 0;
		const v: Pt = [p[0], p[1]];
		if (!r) return { start: v, end: v };
		const c = cs[i]!;
		const t = tangentLength(r, c.angle);
		const start: Pt = [v[0] + c.toPrev[0] * t, v[1] + c.toPrev[1] * t];
		const end: Pt = [v[0] + c.toNext[0] * t, v[1] + c.toNext[1] * t];
		const bis = unit([c.toPrev[0] + c.toNext[0], c.toPrev[1] + c.toNext[1]]);
		const d = r / Math.sin(c.angle / 2);
		const center: Pt = [v[0] + bis[0] * d, v[1] + bis[1] * d];
		return { start, end, mid: [center[0] - bis[0] * r, center[1] - bis[1] * r] as Pt };
	});
	const out: Piece[] = [];
	for (let i = 0; i < n; i++) {
		const a = arcs[i]!;
		if (a.mid) out.push({ kind: "arc", start: a.start, mid: a.mid, end: a.end });
		const b = arcs[(i + 1) % n]!;
		if (len(sub(b.start, a.end)) > 1e-7) out.push({ kind: "line", start: a.end, end: b.start });
	}
	return out;
}

/** A shape as a closed polygon (arcs and circles as short chords), for containment tests and areas. */
export function shapePolygon(s: EdgeShape): Pt[] {
	if ("circle" in s) {
		const { center, radius } = s.circle;
		const k = 180;
		return Array.from({ length: k }, (_, i) => [center[0] + radius * Math.cos((2 * Math.PI * i) / k), center[1] + radius * Math.sin((2 * Math.PI * i) / k)] as Pt);
	}
	const out: Pt[] = [];
	for (const p of pieces(polygonOf(s))) {
		if (p.kind === "line") out.push(p.start);
		else out.push(...arcPoints(p.start, p.mid!, p.end).slice(0, -1));
	}
	return out;
}

/** A rect or polygon shape's vertices. */
function polygonOf(s: RectShape | PolyShape): Vertex[] {
	if ("points" in s) return s.points;
	const { x, y, width: w, height: h, corner_radius: r } = s.rect;
	const v = (px: number, py: number): Vertex => (r ? [px, py, r] : [px, py]);
	return [v(x, y), v(x + w, y), v(x + w, y + h), v(x, y + h)];
}

export function polygonArea(p: Pt[]): number {
	let a = 0;
	for (let i = 0; i < p.length; i++) a += cross(p[i]!, p[(i + 1) % p.length]!);
	return Math.abs(a) / 2;
}

export function pointInPolygon(q: Pt, poly: Pt[]): boolean {
	let inside = false;
	for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
		const [xi, yi] = poly[i]!;
		const [xj, yj] = poly[j]!;
		if (yi > q[1] !== yj > q[1] && q[0] < ((xj - xi) * (q[1] - yi)) / (yj - yi) + xi) inside = !inside;
	}
	return inside;
}

function boundariesCross(a: Pt[], b: Pt[]): boolean {
	const ba = bbox(a);
	const bb = bbox(b);
	if (ba[0] > bb[2] || bb[0] > ba[2] || ba[1] > bb[3] || bb[1] > ba[3]) return false;
	for (let i = 0; i < a.length; i++) {
		const a0 = a[i]!;
		const a1 = a[(i + 1) % a.length]!;
		for (let j = 0; j < b.length; j++) if (segmentsCross(a0, a1, b[j]!, b[(j + 1) % b.length]!)) return true;
	}
	return false;
}

/** Cutouts strictly inside the outline and apart from each other. */
export function checkCutouts(outline: EdgeShape, cutouts: EdgeShape[]): void {
	const out = shapePolygon(outline);
	const polys = cutouts.map(shapePolygon);
	polys.forEach((p, i) => {
		if (boundariesCross(out, p) || !p.every(q => pointInPolygon(q, out))) throw new Error(`cutouts[${i}] is not strictly inside the outline`);
		for (let j = 0; j < i; j++) {
			const o = polys[j]!;
			if (boundariesCross(o, p) || pointInPolygon(p[0]!, o) || pointInPolygon(o[0]!, p)) throw new Error(`cutouts[${j}] and cutouts[${i}] overlap or touch`);
		}
	});
}

// ---- normal form --------------------------------------------------------------------------------

/** Start at the top-most, then left-most vertex; clockwise on screen (y down). */
function normalise(pts: Vertex[]): Vertex[] {
	let area = 0;
	for (let i = 0; i < pts.length; i++) area += cross(pts[i]! as unknown as Pt, pts[(i + 1) % pts.length]! as unknown as Pt);
	const ordered = area < 0 ? [...pts].reverse() : [...pts];
	let s = 0;
	ordered.forEach((p, i) => {
		const b = ordered[s]!;
		if (p[1] < b[1] - EPS || (Math.abs(p[1] - b[1]) <= EPS && p[0] < b[0])) s = i;
	});
	return [...ordered.slice(s), ...ordered.slice(0, s)];
}

/** The shape as set_board_edges draws and reads it back (polygons in normal form). */
export function normalShape(s: EdgeShape): EdgeShape {
	return "points" in s ? { points: normalise(s.points) } : s;
}

// ---- KiCad items --------------------------------------------------------------------------------

const EDGE = "Edge.Cuts";

/** The Edge.Cuts items drawing a shape, stroke `width` mm. */
export function encodeEdge(s: EdgeShape, width: number, uuid: () => string): List[] {
	// KiCad's item layout: geometry, stroke, fill (closed shapes), layer, uuid
	const tail = (fill: boolean) => [
		L("stroke", L("width", mm(width)), L("type", "default")),
		...(fill ? [L("fill", "no")] : []),
		L("layer", str(EDGE)),
		L("uuid", str(uuid())),
	];
	if ("circle" in s) {
		const { center, radius } = s.circle;
		return [L("gr_circle", xy("center", center), xy("end", [center[0] + radius, center[1]]), ...tail(true))];
	}
	if ("rect" in s && !s.rect.corner_radius) {
		const { x, y, width: w, height: h } = s.rect;
		return [L("gr_rect", xy("start", [x, y]), xy("end", [x + w, y + h]), ...tail(true))];
	}
	return pieces(normalise(polygonOf(s))).map(p =>
		p.kind === "line"
			? L("gr_line", xy("start", p.start), xy("end", p.end), ...tail(false))
			: L("gr_arc", xy("start", p.start), xy("mid", p.mid!), xy("end", p.end), ...tail(false)),
	);
}

/** Is this a board-level Edge.Cuts drawing? */
export function isEdgeItem(item: List): boolean {
	return /^gr_(line|arc|rect|circle|poly|curve)$/.test(head(item) ?? "") && text(child(item, "layer")) === EDGE;
}

const P = (l: List | undefined): Pt => [num(l, 1), num(l, 2)];

interface Seg {
	type: "line" | "arc" | "curve";
	start: Pt;
	end: Pt;
	/** arc: mid point; curve: control points. */
	via: Pt[];
}

function near(a: Pt, b: Pt): boolean {
	return Math.abs(a[0] - b[0]) <= JOIN && Math.abs(a[1] - b[1]) <= JOIN;
}

/** The fillet vertex an arc stands for: where its end tangents meet, and its radius. */
function arcVertex(s: Seg): Vertex | undefined {
	const c = circleThrough(s.start, s.via[0]!, s.end);
	if (!c) return undefined;
	const t0: Pt = [-(s.start[1] - c.center[1]), s.start[0] - c.center[0]];
	const t1: Pt = [-(s.end[1] - c.center[1]), s.end[0] - c.center[0]];
	const den = cross(t0, t1);
	if (Math.abs(den) < 1e-12) return undefined;
	const k = cross(sub(s.end, s.start), t1) / den;
	return [r4(s.start[0] + t0[0] * k), r4(s.start[1] + t0[1] * k), r4(c.r)];
}

/** A closed chain of lines and arcs back to the polygon with fillets, if every arc sits between two lines' directions. */
function chainPolygon(chain: Seg[]): Vertex[] | undefined {
	if (chain.some(s => s.type === "curve")) return undefined;
	const out: Vertex[] = [];
	for (let i = 0; i < chain.length; i++) {
		const s = chain[i]!;
		const prev = chain[(i + chain.length - 1) % chain.length]!;
		if (s.type === "arc") {
			const v = arcVertex(s);
			if (!v) return undefined;
			out.push(v);
		} else if (prev.type === "line") out.push([r4(s.start[0]), r4(s.start[1])]);
	}
	if (out.length < 3) return undefined;
	// the arcs must be the fillets the polygon would draw: each redrawn arc has the end points (either
	// direction) and mid point of an arc of the chain; else the chain is no fillet polygon (e.g. an arc
	// that isn't tangent to its lines) and reads back raw
	const again = pieces(out).filter(p => p.kind === "arc");
	const arcs = chain.filter(s => s.type === "arc");
	if (arcs.length !== again.length) return undefined;
	const close = (a: Pt, b: Pt) => Math.abs(a[0] - b[0]) <= REDRAW && Math.abs(a[1] - b[1]) <= REDRAW;
	const left = [...arcs];
	for (const p of again) {
		const i = left.findIndex(s => close(s.via[0]!, p.mid!) && ((close(s.start, p.start) && close(s.end, p.end)) || (close(s.start, p.end) && close(s.end, p.start))));
		if (i < 0) return undefined;
		left.splice(i, 1);
	}
	return out;
}

/** The shape a closed chain draws, as close to the input format as can be told. */
function loopShape(chain: Seg[], closed: boolean): ReadShape {
	const raw: RawLoop = {
		items: chain.map(s => ({ type: s.type, points: [s.start, ...s.via, s.end].map(p => [r4(p[0]), r4(p[1])] as Pt) })),
		closed,
	};
	if (!closed) return raw;
	const poly = chainPolygon(chain);
	if (!poly) return raw;
	const n = normalise(poly);
	// four axis-parallel sides with one radius: a rounded rect
	const r = n[0]![2];
	if (n.length === 4 && r && n.every(v => v[2] === r)) {
		const [a, b, c, d] = n as [Vertex, Vertex, Vertex, Vertex];
		if (a[1] === b[1] && b[0] === c[0] && c[1] === d[1] && d[0] === a[0] && b[0] > a[0] && c[1] > b[1]) {
			return { rect: { x: a[0], y: a[1], width: r4(b[0] - a[0]), height: r4(c[1] - b[1]), corner_radius: r } };
		}
	}
	return { points: n };
}

/** Board-level Edge.Cuts items as shapes: closed chains, rects, circles and polygons, each read back. */
export function decodeEdges(items: List[]): ReadShape[] {
	const shapes: ReadShape[] = [];
	const segs: Seg[] = [];
	for (const it of items) {
		switch (head(it)) {
			case "gr_rect": {
				const [a, b] = [P(child(it, "start")), P(child(it, "end"))];
				shapes.push({
					rect: { x: r4(Math.min(a[0], b[0])), y: r4(Math.min(a[1], b[1])), width: r4(Math.abs(b[0] - a[0])), height: r4(Math.abs(b[1] - a[1])) },
				});
				break;
			}
			case "gr_circle": {
				const c = P(child(it, "center"));
				const e = P(child(it, "end"));
				shapes.push({ circle: { center: [r4(c[0]), r4(c[1])], radius: r4(Math.hypot(e[0] - c[0], e[1] - c[1])) } });
				break;
			}
			case "gr_poly": {
				const pts = (child(it, "pts") ?? []).slice(1).filter((x): x is List => Array.isArray(x) && head(x) === "xy");
				shapes.push({ points: normalise(pts.map(p => [r4(num(p, 1)), r4(num(p, 2))] as Vertex)) });
				break;
			}
			case "gr_line":
				segs.push({ type: "line", start: P(child(it, "start")), end: P(child(it, "end")), via: [] });
				break;
			case "gr_arc":
				segs.push({ type: "arc", start: P(child(it, "start")), end: P(child(it, "end")), via: [P(child(it, "mid"))] });
				break;
			case "gr_curve": {
				const pts = (child(it, "pts") ?? []).slice(1).filter((x): x is List => Array.isArray(x) && head(x) === "xy").map(p => P(p));
				segs.push({ type: "curve", start: pts[0]!, end: pts[pts.length - 1]!, via: pts.slice(1, -1) });
				break;
			}
		}
	}
	// chain the segments by their end points, flipping a segment when it runs the other way
	const left = [...segs];
	while (left.length) {
		const chain = [left.shift()!];
		let closed = false;
		for (;;) {
			const tail = chain[chain.length - 1]!.end;
			if (chain.length > 1 && near(tail, chain[0]!.start)) {
				closed = true;
				break;
			}
			if (chain.length === 1 && near(tail, chain[0]!.start) && chain[0]!.type !== "line") {
				closed = true;
				break;
			}
			const i = left.findIndex(s => near(s.start, tail) || near(s.end, tail));
			if (i < 0) break;
			const s = left.splice(i, 1)[0]!;
			chain.push(near(s.start, tail) ? s : { type: s.type, start: s.end, end: s.start, via: [...s.via].reverse() });
		}
		shapes.push(loopShape(chain, closed));
	}
	return shapes;
}

/** A closed shape's boundary as a polygon; undefined for open chains. */
export function readPolygon(s: ReadShape): Pt[] | undefined {
	if ("items" in s) {
		if (!s.closed) return undefined;
		return s.items.flatMap(it => (it.type === "arc" ? arcPoints(it.points[0]!, it.points[1]!, it.points[2]!).slice(0, -1) : it.points.slice(0, -1)));
	}
	return shapePolygon(s);
}

/** Outline (the loop with the largest area) and cutouts (the other closed loops), with bbox and area (outline minus cutouts). */
export function boardOutline(items: List[]): { outline?: ReadShape; cutouts: ReadShape[]; open: ReadShape[]; bbox?: Box; area?: number; polygon?: Pt[]; holes: Pt[][] } {
	const shapes = decodeEdges(items);
	const closed = shapes.map(s => ({ s, poly: readPolygon(s) })).filter((x): x is { s: ReadShape; poly: Pt[] } => !!x.poly);
	const open = shapes.filter(s => !readPolygon(s));
	if (!closed.length) return { cutouts: [], open, holes: [] };
	closed.sort((a, b) => polygonArea(b.poly) - polygonArea(a.poly));
	const [outer, ...rest] = closed;
	// KiCad writes items in its own order: cutouts are listed top to bottom, then left to right
	rest.sort((a, b) => bbox(a.poly)[1] - bbox(b.poly)[1] || bbox(a.poly)[0] - bbox(b.poly)[0]);
	const area = polygonArea(outer!.poly) - rest.reduce((t, c) => t + polygonArea(c.poly), 0);
	const b = bbox(outer!.poly);
	return {
		outline: outer!.s,
		cutouts: rest.map(c => c.s),
		open,
		bbox: [r4(b[0]), r4(b[1]), r4(b[2]), r4(b[3])],
		area: Math.round(area * 100) / 100,
		polygon: outer!.poly,
		holes: rest.map(c => c.poly),
	};
}
