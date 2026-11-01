/**
 * A board's copper and drawings as KiCad 10 stores them, and what they connect: tracks, arc
 * tracks, vias, zones with their fills, rule areas, board drawings and texts, tuning patterns,
 * the footprints' pads with their copper shapes; per net the connected clusters, the unrouted
 * pad pairs, routed lengths and the copper path length between two pads. Board mm, x right,
 * y down; rotations counter-clockwise degrees, as pcbnew shows them.
 *
 * The format (KiCad 10.0.7, board version 20260206; checked against KiCad's demo boards and
 * `kicad-cli pcb upgrade` re-saves):
 * - Items name their net: `(net "GND")`. Boards from KiCad 9 and older number them, `(net 3)`,
 *   with a top-level net table `(net 3 "GND")` (zones add `(net_name "GND")`); both are read.
 * - `(segment (start x y) (end x y) (width w) (layer "F.Cu") (net …) (uuid …))`; `(arc (start)
 *   (mid) (end) …)` the same with the arc's mid point.
 * - `(via [blind|buried|micro] (at x y) (size d) (drill d) (layers "F.Cu" "B.Cu") … (net …))`:
 *   the two layers are the ends of the span.
 * - `(zone (net …) (layer "B.Cu") | (layers …) (uuid) (name "…") (hatch …) (priority n)
 *   (connect_pads [yes|no|thru_hole_only] (clearance c)) (min_thickness t) [(keepout …)]
 *   (fill [yes] [(mode hatch)] (thermal_gap g) (thermal_bridge_width w) (island_removal_mode n)
 *   [(island_area_min a)] …) (polygon (pts …)) … (filled_polygon (layer "B.Cu") (pts …)) …)`. No
 *   connect_pads mode is thermal reliefs; `fill yes` marks a filled zone. The first polygon is
 *   the outline, further ones are holes in it. A filled polygon is one region, fractured as KiCad
 *   stores it (holes joined to the outline by zero-width slits), so its shoelace area and
 *   point-in-polygon test are the region's.
 * - A rule area is a zone with `(keepout (tracks not_allowed) (vias …) (pads …) (copperpour …)
 *   (footprints …))`.
 * - `(generated (uuid) (type tuning_pattern) (name) (layer) … (last_netname "…") (tuning_mode
 *   "…") (members uuid …))`: the tracks and arcs a tuning pattern made.
 * - Pads: `(pad "1" smd roundrect (at x y [angle]) (size w h) [(drill d [(offset x y)])] (layers …)
 *   [(roundrect_rratio r)] [(chamfer_ratio r) (chamfer top_left …)] [(rect_delta dx dy)]
 *   [(options (anchor rect|circle)) (primitives (gr_poly …) (gr_line …) …)] (net …) …)`; the
 *   position is footprint-relative and unrotated, the angle absolute (see board.ts). "*.Cu" is
 *   every copper layer; a through-hole pad is on every copper layer.
 *
 * Connections follow KiCad's connectivity: two items of a net on a shared copper layer connect
 * when an anchor of one (a track's or arc's ends, a via's or pad's position) lies on the other's
 * copper shape; a zone's fill connects the tracks whose ends lie in it and the pads and vias its
 * fill touches, and other fills of the net it touches. A via joins every copper layer of its span.
 */
import { type Board, type PadInfo, fpRef, padsInfo, placement, type Stackup, toBoard } from "./board";
import { arcPoints, circleThrough, pointInPolygon, polygonArea } from "./edges";
import { type Box, bbox, type Pt } from "./geom";
import type { Project } from "./project";
import { child, children, head, isList, type List, num, Str, Sym, text } from "./sexpr";

// ---- geometry --------------------------------------------------------------------------------------

/**
 * A copper shape: the points' hull grown by `r`. One point is a disc (a via, a round pad), two are
 * a stadium (a track), three or more a closed polygon (rounded by r when r > 0: a roundrect pad).
 */
export interface Shape {
	pts: Pt[];
	r: number;
	box: Box;
}

export function shape(pts: Pt[], r = 0): Shape {
	const b = bbox(pts);
	return { pts, r, box: [b[0] - r, b[1] - r, b[2] + r, b[3] + r] };
}

const EPS = 1e-6;

const dist = (a: Pt, b: Pt) => Math.hypot(a[0] - b[0], a[1] - b[1]);
const r4 = (v: number) => Math.round(v * 1e4) / 1e4 + 0;

/** The closest point to p on segment ab and its parameter t in [0, 1]. */
function closestOnSeg(p: Pt, a: Pt, b: Pt): { q: Pt; t: number } {
	const d: Pt = [b[0] - a[0], b[1] - a[1]];
	const l2 = d[0] * d[0] + d[1] * d[1];
	const t = l2 < 1e-18 ? 0 : Math.max(0, Math.min(1, ((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / l2));
	return { q: [a[0] + d[0] * t, a[1] + d[1] * t], t };
}

function segsCross(a: Pt, b: Pt, c: Pt, d: Pt): boolean {
	const o = (p: Pt, q: Pt, r: Pt) => (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]);
	const d1 = o(c, d, a);
	const d2 = o(c, d, b);
	const d3 = o(a, b, c);
	const d4 = o(a, b, d);
	return ((d1 > 0 && d2 < 0) || (d1 < 0 && d2 > 0)) && ((d3 > 0 && d4 < 0) || (d3 < 0 && d4 > 0));
}

/** Closest points of two segments. */
function segSeg(a: Pt, b: Pt, c: Pt, d: Pt): { d: number; p: Pt; q: Pt } {
	if (segsCross(a, b, c, d)) {
		// the crossing point
		const den = (a[0] - b[0]) * (c[1] - d[1]) - (a[1] - b[1]) * (c[0] - d[0]);
		const t = ((a[0] - c[0]) * (c[1] - d[1]) - (a[1] - c[1]) * (c[0] - d[0])) / den;
		const x: Pt = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])];
		return { d: 0, p: x, q: x };
	}
	let best = { d: Infinity, p: a, q: c };
	for (const [p, s0, s1, flip] of [
		[a, c, d, false],
		[b, c, d, false],
		[c, a, b, true],
		[d, a, b, true],
	] as const) {
		const { q } = closestOnSeg(p, s0, s1);
		const e = dist(p, q);
		if (e < best.d) best = flip ? { d: e, p: q, q: p } : { d: e, p, q };
	}
	return best;
}

const isPoly = (s: Shape) => s.pts.length >= 3;

/** Edge i of a shape's skeleton: a point, the segment, or the polygon's closed edge i. */
function edgeCount(s: Shape): number {
	return s.pts.length >= 3 ? s.pts.length : 1;
}

function edgeAt(s: Shape, i: number): [Pt, Pt] {
	if (s.pts.length === 1) return [s.pts[0]!, s.pts[0]!];
	if (s.pts.length === 2) return [s.pts[0]!, s.pts[1]!];
	return [s.pts[i]!, s.pts[(i + 1) % s.pts.length]!];
}

/** Polygons with more vertices than this get a grid of their edges (zone fills, board outlines). */
const GRID_MIN = 64;

/** A uniform grid over a polygon's bbox: per cell, the edges whose bbox meets it. */
interface EdgeGrid {
	x0: number;
	y0: number;
	cell: number;
	nx: number;
	ny: number;
	cells: number[][];
	/** Visit stamps per edge, so an edge in several cells is looked at once per query. */
	seen: Uint32Array;
	stamp: number;
}

const grids = new WeakMap<Shape, EdgeGrid>();

function gridOf(s: Shape): EdgeGrid | undefined {
	if (s.pts.length < GRID_MIN) return undefined;
	let g = grids.get(s);
	if (g) return g;
	const [x0, y0, x1, y1] = bbox(s.pts);
	const n = s.pts.length;
	// about two edges per cell
	const cell = Math.max(Math.sqrt(((x1 - x0) * (y1 - y0)) / (n / 2)), (x1 - x0) / 1024, (y1 - y0) / 1024, 1e-3);
	const nx = Math.max(1, Math.ceil((x1 - x0) / cell) + 1);
	const ny = Math.max(1, Math.ceil((y1 - y0) / cell) + 1);
	const cells: number[][] = Array.from({ length: nx * ny }, () => []);
	for (let i = 0; i < n; i++) {
		const [a, b] = edgeAt(s, i);
		const cx0 = Math.floor((Math.min(a[0], b[0]) - x0) / cell);
		const cx1 = Math.floor((Math.max(a[0], b[0]) - x0) / cell);
		const cy0 = Math.floor((Math.min(a[1], b[1]) - y0) / cell);
		const cy1 = Math.floor((Math.max(a[1], b[1]) - y0) / cell);
		for (let cy = cy0; cy <= cy1; cy++) for (let cx = cx0; cx <= cx1; cx++) cells[cy * nx + cx]!.push(i);
	}
	g = { x0, y0, cell, nx, ny, cells, seen: new Uint32Array(n), stamp: 0 };
	grids.set(s, g);
	return g;
}

/** Indices of the edges whose bbox may meet the box (every edge when the shape has no grid). */
function edgesNear(s: Shape, box: Box): number[] {
	const g = gridOf(s);
	if (!g) return Array.from({ length: edgeCount(s) }, (_, i) => i);
	const cx0 = Math.max(0, Math.floor((box[0] - g.x0) / g.cell));
	const cx1 = Math.min(g.nx - 1, Math.floor((box[2] - g.x0) / g.cell));
	const cy0 = Math.max(0, Math.floor((box[1] - g.y0) / g.cell));
	const cy1 = Math.min(g.ny - 1, Math.floor((box[3] - g.y0) / g.cell));
	const out: number[] = [];
	const stamp = ++g.stamp;
	for (let cy = cy0; cy <= cy1; cy++) {
		for (let cx = cx0; cx <= cx1; cx++) {
			for (const i of g.cells[cy * g.nx + cx]!) {
				if (g.seen[i] === stamp) continue;
				g.seen[i] = stamp;
				out.push(i);
			}
		}
	}
	return out;
}

/** Point in a polygon shape's points (even-odd), through its grid when it has one: a ray to +x crosses only the edges of the cells right of p. */
function insidePoly(s: Shape, p: Pt): boolean {
	if (p[0] < s.box[0] || p[0] > s.box[2] || p[1] < s.box[1] || p[1] > s.box[3]) return false;
	const g = gridOf(s);
	if (!g) return pointInPolygon(p, s.pts);
	let inside = false;
	for (const i of edgesNear(s, [p[0], p[1], s.box[2], p[1]])) {
		const [[xi, yi], [xj, yj]] = edgeAt(s, i);
		if (yi > p[1] !== yj > p[1] && p[0] < ((xj - xi) * (p[1] - yi)) / (yj - yi) + xi) inside = !inside;
	}
	return inside;
}

/** Is p on the shape (inside or on its edge, within `tol`)? */
export function shapeHas(s: Shape, p: Pt, tol = EPS): boolean {
	if (p[0] < s.box[0] - tol || p[0] > s.box[2] + tol || p[1] < s.box[1] - tol || p[1] > s.box[3] + tol) return false;
	if (isPoly(s) && insidePoly(s, p)) return true;
	const reach = s.r + tol;
	for (const i of edgesNear(s, [p[0] - reach, p[1] - reach, p[0] + reach, p[1] + reach])) {
		const [a, b] = edgeAt(s, i);
		if (dist(p, closestOnSeg(p, a, b).q) <= reach) return true;
	}
	return false;
}

/**
 * Edge-to-edge distance between two shapes (0 when they touch or overlap) and the closest points
 * (on each shape's edge; one shared point when they overlap). Undefined when they are further
 * apart than `within`; a finite `within` lets big polygons (zone fills) look only at their edges near the other shape.
 */
export function shapeDistance(a: Shape, b: Shape, within = Infinity): { d: number; pa: Pt; pb: Pt } | undefined {
	if (Math.hypot(Math.max(0, a.box[0] - b.box[2], b.box[0] - a.box[2]), Math.max(0, a.box[1] - b.box[3], b.box[1] - a.box[3])) > within) return undefined;
	if (isPoly(a) && insidePoly(a, b.pts[0]!)) return { d: 0, pa: b.pts[0]!, pb: b.pts[0]! };
	if (isPoly(b) && insidePoly(b, a.pts[0]!)) return { d: 0, pa: a.pts[0]!, pb: a.pts[0]! };
	const reach = within + a.r + b.r;
	const grow = (x: Box): Box => [x[0] - reach, x[1] - reach, x[2] + reach, x[3] + reach];
	const ea = Number.isFinite(reach) ? edgesNear(a, grow(b.box)) : Array.from({ length: edgeCount(a) }, (_, i) => i);
	const eb = Number.isFinite(reach) ? edgesNear(b, grow(a.box)) : Array.from({ length: edgeCount(b) }, (_, i) => i);
	let best = { d: Infinity, p: a.pts[0]!, q: b.pts[0]! };
	search: for (const i of ea) {
		const [p0, p1] = edgeAt(a, i);
		for (const j of eb) {
			const [q0, q1] = edgeAt(b, j);
			// edges whose boxes are further apart than the best so far can't do better
			if (Math.max(Math.min(p0[0], p1[0]) - Math.max(q0[0], q1[0]), Math.min(q0[0], q1[0]) - Math.max(p0[0], p1[0]), Math.min(p0[1], p1[1]) - Math.max(q0[1], q1[1]), Math.min(q0[1], q1[1]) - Math.max(p0[1], p1[1])) >= best.d) continue;
			const r = segSeg(p0, p1, q0, q1);
			if (r.d < best.d) {
				best = r;
				// touching: no closer pair matters
				if (best.d <= a.r + b.r) break search;
			}
		}
	}
	if (!Number.isFinite(best.d)) return undefined;
	const gap = best.d - a.r - b.r;
	if (gap <= 0) {
		// overlapping: a point inside both
		const t = best.d > 0 ? a.r / (a.r + b.r || 1) : 0;
		const m: Pt = [best.p[0] + (best.q[0] - best.p[0]) * t, best.p[1] + (best.q[1] - best.p[1]) * t];
		return { d: 0, pa: m, pb: m };
	}
	if (gap > within) return undefined;
	const u: Pt = [(best.q[0] - best.p[0]) / best.d, (best.q[1] - best.p[1]) / best.d];
	return { d: gap, pa: [best.p[0] + u[0] * a.r, best.p[1] + u[1] * a.r], pb: [best.q[0] - u[0] * b.r, best.q[1] - u[1] * b.r] };
}

/** Do two shapes touch or overlap? */
export function shapesTouch(a: Shape, b: Shape): boolean {
	return (shapeDistance(a, b, EPS)?.d ?? Infinity) <= EPS;
}

/** Area of a shape (a disc, stadium or polygon; a rounded polygon's corners as quarter circles). */
export function shapeArea(s: Shape): number {
	if (s.pts.length === 1) return Math.PI * s.r * s.r;
	if (s.pts.length === 2) return dist(s.pts[0]!, s.pts[1]!) * 2 * s.r + Math.PI * s.r * s.r;
	const perimeter = s.pts.reduce((t, p, i) => t + dist(p, s.pts[(i + 1) % s.pts.length]!), 0);
	return polygonArea(s.pts) + perimeter * s.r + Math.PI * s.r * s.r;
}

/** The arc's centre, radius, signed sweep (radians, positive = increasing screen angle) and length. */
export function arcGeometry(start: Pt, mid: Pt, end: Pt): { center: Pt; radius: number; sweep: number; length: number } | undefined {
	const c = circleThrough(start, mid, end);
	if (!c) return undefined;
	const ang = (p: Pt) => Math.atan2(p[1] - c.center[1], p[0] - c.center[0]);
	const norm = (x: number) => ((x % (2 * Math.PI)) + 2 * Math.PI) % (2 * Math.PI);
	const toMid = norm(ang(mid) - ang(start));
	const toEnd = norm(ang(end) - ang(start));
	const sweep = toMid <= toEnd ? toEnd : toEnd - 2 * Math.PI;
	return { center: c.center, radius: c.r, sweep, length: c.r * Math.abs(sweep) };
}

// ---- layers ----------------------------------------------------------------------------------------

/** Copper layers of a layer list: "*.Cu" is every copper layer, "F&B.Cu" the outer two. */
export function copperOf(layers: readonly string[], copper: readonly string[]): string[] {
	const out = new Set<string>();
	for (const l of layers) {
		if (l === "*.Cu") for (const c of copper) out.add(c);
		else if (l === "F&B.Cu") for (const c of [copper[0], copper[copper.length - 1]]) c && out.add(c);
		else if (copper.includes(l)) out.add(l);
	}
	return copper.filter(c => out.has(c));
}

/**
 * Height between the middles of two copper layers through the stack-up, as KiCad's
 * BOARD_STACKUP::GetLayerDistance: half of each end copper, the dielectrics and copper between.
 * A board without a stack-up gets KiCad's default one: 0.035 mm copper, 0.01 mm masks and the
 * rest of the board thickness split evenly between the dielectrics.
 */
export function layerDistance(su: Stackup, copper: readonly string[], a: string, b: string): number {
	let i = copper.indexOf(a);
	let j = copper.indexOf(b);
	if (i < 0 || j < 0) throw new Error(`not a copper layer of the board: ${i < 0 ? a : b}`);
	if (i > j) [i, j] = [j, i];
	if (i === j) return 0;
	const n = copper.length;
	const known = su.dielectrics.length === n - 1 && su.copper.length === n && su.copper.every(c => c > 0);
	const cu = known ? su.copper : copper.map(() => 0.035);
	const diel = known ? su.dielectrics.map(d => d.thickness) : Array.from({ length: n - 1 }, () => Math.max(0, su.thickness - 0.035 * n - 0.02) / (n - 1));
	let h = cu[i]! / 2 + cu[j]! / 2;
	for (let k = i; k < j; k++) h += diel[k]!;
	for (let k = i + 1; k < j; k++) h += cu[k]!;
	return h;
}

/** The .kicad_pro's `use_height_for_length_calcs` (KiCad's default: true). */
export function useHeightForLength(project: Project): boolean {
	const board = project.proJson.data.board as { design_settings?: { rules?: Record<string, unknown> } } | undefined;
	const v = board?.design_settings?.rules?.use_height_for_length_calcs;
	return typeof v === "boolean" ? v : true;
}

// ---- items -----------------------------------------------------------------------------------------

export interface Track {
	kind: "track" | "arc";
	uuid: string;
	/** "" when the track has no net. */
	net: string;
	layer: string;
	width: number;
	start: Pt;
	end: Pt;
	/** Arcs: the mid point, centre, radius and signed sweep (radians). */
	mid?: Pt;
	center?: Pt;
	radius?: number;
	sweep?: number;
	/** Centreline length, mm (an arc's along the arc). */
	length: number;
	locked: boolean;
	/** The tuning pattern (generated group uuid) the track belongs to. */
	tuning?: string;
	/** Copper shape: one stadium, or a chain of them along an arc. */
	shapes: Shape[];
	node: List;
}

export type ViaType = "through" | "blind" | "buried" | "micro";

export interface Via {
	kind: "via";
	uuid: string;
	net: string;
	at: Pt;
	size: number;
	drill: number;
	type: ViaType;
	/** The two layers the file names: the ends of the span. */
	layers: [string, string];
	/** Every copper layer the via spans, front to back. */
	span: string[];
	locked: boolean;
	shapes: Shape[];
	node: List;
}

/** A footprint pad with its copper. */
export interface Pad {
	kind: "pad";
	ref: string;
	number: string;
	/** "R13.2". */
	id: string;
	net: string;
	/** Board position (the hole's centre for drilled pads), mm: KiCad's pad position, where tracks end. */
	at: Pt;
	/** Centre of the pad's copper shape (the position moved by the drill offset): KiCad's ShapePos, where ratsnest lines end. */
	centre: Pt;
	/** Pad-to-die length (`die_length`), mm; 0 when none. */
	dieLength: number;
	/** Copper layers the pad is on (every one for through-hole pads; none for NPTH holes). */
	copper: string[];
	info: PadInfo;
	shapes: Shape[];
	fp: List;
	node: List;
}

export type ConnectPads = "thermal" | "solid" | "none" | "thru_hole_only";

export interface ZoneSettings {
	/** Clearance to other nets' copper, mm. */
	clearance: number;
	min_thickness: number;
	connect_pads: ConnectPads;
	thermal_gap: number;
	/** Thermal spoke width, mm. */
	thermal_spoke_width: number;
	/** 0: remove every island, 1: keep them, 2: remove those below island_min_area. */
	island_removal: "always" | "never" | "below_area";
	island_min_area?: number;
	fill_mode: "solid" | "hatch";
	hatch_thickness?: number;
	hatch_gap?: number;
	hatch_orientation?: number;
}

export interface Fill {
	kind: "fill";
	/** The zone's uuid. */
	zone: string;
	layer: string;
	/** One filled region (fractured polygon). */
	shape: Shape;
	area: number;
}

export interface Zone {
	kind: "zone";
	uuid: string;
	name?: string;
	net: string;
	layers: string[];
	priority: number;
	outline: Pt[];
	holes: Pt[][];
	settings: ZoneSettings;
	/** `fill yes`: KiCad filled the zone when it saved it. */
	filled: boolean;
	fills: Fill[];
	/** Filled area per layer, mm². */
	filledArea: Record<string, number>;
	/** A teardrop KiCad generated. */
	teardrop: boolean;
	locked: boolean;
	node: List;
	/** The footprint the zone belongs to (zones inside footprints are stored in board coordinates). */
	footprint?: string;
}

export interface Keepout {
	tracks: boolean;
	vias: boolean;
	pads: boolean;
	copperpour: boolean;
	footprints: boolean;
}

/** A rule area (keepout zone): true in `keepout` means not allowed. */
export interface RuleArea {
	kind: "rule_area";
	uuid: string;
	name?: string;
	layers: string[];
	outline: Pt[];
	holes: Pt[][];
	keepout: Keepout;
	locked: boolean;
	node: List;
	/** The footprint the rule area belongs to (an antenna keepout, …). */
	footprint?: string;
}

export type DrawingType = "line" | "rect" | "circle" | "arc" | "poly" | "curve";

/** A board-level drawing, in the form the board graphics ops take. */
export interface Drawing {
	uuid: string;
	type: DrawingType;
	layer: string;
	/** Stroke width, mm (0: no outline, e.g. a filled polygon). */
	width: number;
	fill: boolean;
	start?: Pt;
	end?: Pt;
	mid?: Pt;
	center?: Pt;
	radius?: number;
	/** Polygons: the vertices; curves: the four Bézier control points. */
	points?: Pt[];
	net?: string;
	locked: boolean;
	node: List;
}

/** A board-level text or text box, in the form the board graphics ops take. */
export interface BoardText {
	uuid: string;
	type: "text" | "textbox";
	text: string;
	/** Anchor (texts) or top-left corner (text boxes), mm. */
	at: Pt;
	/** Text boxes: the opposite corner (unrotated boxes) or the four corners. */
	end?: Pt;
	corners?: Pt[];
	/** [width, height] of a glyph, mm. */
	size: [number, number];
	thickness: number;
	rot: number;
	justify: string[];
	layer: string;
	knockout: boolean;
	mirrored: boolean;
	hidden: boolean;
	locked: boolean;
	node: List;
}

/** A KiCad 10 generated group: a tuning pattern and the tracks it made. */
export interface Generated {
	uuid: string;
	type: string;
	name: string;
	layer: string;
	/** Member uuids (tracks and arcs). */
	members: string[];
	/** The net the pattern was last tuned on. */
	net?: string;
	/** Scalar settings as KiCad writes them (tuning_mode, target_length, last_status, …). */
	settings: Record<string, string | number | boolean>;
	node: List;
}

export type CopperItem = Track | Via | Pad | Fill;

/** A board drawing with a net on a copper layer: copper the connectivity counts, as KiCad does. */
export interface CopperDrawing {
	kind: "drawing";
	drawing: Drawing;
	net: string;
	layer: string;
	shapes: Shape[];
}

// ---- reading ---------------------------------------------------------------------------------------

const P = (l: List | undefined): Pt => [num(l, 1), num(l, 2)];
const atoms = (l: List | undefined): string[] => (l ?? []).slice(1).flatMap(a => (isList(a) ? [] : [a.v]));
const uuidOf = (l: List) => text(child(l, "uuid")) ?? text(child(l, "tstamp")) ?? "";
const lockedOf = (l: List) => child(l, "locked") !== undefined ? text(child(l, "locked")) !== "no" : l.some(c => c instanceof Sym && c.v === "locked");

/** `(pts (xy …) (arc (start) (mid) (end)) …)` as points, arcs as short chords. */
function ptsOf(l: List | undefined): Pt[] {
	return (child(l ?? [], "pts") ?? []).filter(isList).flatMap(p => (head(p) === "xy" ? [P(p)] : head(p) === "arc" ? arcPoints(P(child(p, "start")), P(child(p, "mid")), P(child(p, "end")), Math.PI / 36).slice(0, -1) : []));
}

/** Net names by item: the name in KiCad 10, a number into the net table in older boards. */
function netReader(tree: List): (item: List) => string {
	const table = new Map<string, string>();
	for (const n of children(tree, "net")) if (n.length >= 3) table.set(text(n, 1) ?? "", text(n, 2) ?? "");
	return item => {
		const n = child(item, "net");
		if (!n) return text(child(item, "net_name")) ?? "";
		const v = n[1];
		if (v instanceof Str) return v.v;
		if (v instanceof Sym) return table.get(v.v) ?? text(child(item, "net_name")) ?? "";
		return "";
	};
}

/** A stadium chain along an arc. */
function arcShapes(start: Pt, mid: Pt, end: Pt, r: number): Shape[] {
	const pts = arcPoints(start, mid, end, Math.PI / 36);
	return pts.slice(1).map((p, i) => shape([pts[i]!, p], r));
}

/** Pad-local outline points of a pad's main shape (centred on the origin, unrotated). */
function padOutline(pad: List, w: number, h: number): { pts: Pt[]; r: number } {
	const kind = text(pad, 3) ?? "rect";
	const hw = w / 2;
	const hh = h / 2;
	if (kind === "circle") return { pts: [[0, 0]], r: hw };
	if (kind === "oval") return w >= h ? { pts: [[-(hw - hh), 0], [hw - hh, 0]], r: hh } : { pts: [[0, -(hh - hw)], [0, hh - hw]], r: hw };
	if (kind === "trapezoid") {
		const d = child(pad, "rect_delta");
		const [dx, dy] = d ? [num(d, 1) / 2, num(d, 2) / 2] : [0, 0];
		// KiCad's corner order (PAD::BuildPadPolygon)
		return { pts: [[-hw - dy, hh + dx], [hw + dy, hh - dx], [hw - dy, -hh + dx], [-hw + dy, -hh - dx]], r: 0 };
	}
	const rr = kind === "roundrect" ? Math.min(w, h) * (child(pad, "roundrect_rratio") ? num(child(pad, "roundrect_rratio")) : 0) : 0;
	// KiCad writes a chamfered rectangle as a roundrect with chamfered corners
	const which = new Set(kind === "roundrect" ? atoms(child(pad, "chamfer")) : []);
	if (which.size) {
		const c = Math.min(w, h) * (child(pad, "chamfer_ratio") ? num(child(pad, "chamfer_ratio")) : 0);
		const ix = hw - rr;
		const iy = hh - rr;
		const cc = Math.max(0, c - rr * (2 - Math.SQRT2));
		const corner = (name: string, x: number, y: number, first: Pt, second: Pt): Pt[] => (which.has(name) && cc > 0 ? [first, second] : [[x, y]]);
		// clockwise on screen from the top-left corner; a chamfered corner becomes two points
		return {
			pts: [
				...corner("top_left", -ix, -iy, [-ix, -iy + cc], [-ix + cc, -iy]),
				...corner("top_right", ix, -iy, [ix - cc, -iy], [ix, -iy + cc]),
				...corner("bottom_right", ix, iy, [ix, iy - cc], [ix - cc, iy]),
				...corner("bottom_left", -ix, iy, [-ix + cc, iy], [-ix, iy - cc]),
			],
			r: rr,
		};
	}
	return { pts: [[-(hw - rr), -(hh - rr)], [hw - rr, -(hh - rr)], [hw - rr, hh - rr], [-(hw - rr), hh - rr]], r: rr };
}

/** A custom pad's primitives, pad-local. */
function primitiveShapes(pad: List): { pts: Pt[]; r: number }[] {
	const out: { pts: Pt[]; r: number }[] = [];
	for (const g of (child(pad, "primitives") ?? []).filter(isList)) {
		const w = child(g, "width") ? num(child(g, "width")) : child(child(g, "stroke") ?? [], "width") ? num(child(child(g, "stroke")!, "width")) : 0;
		switch (head(g)) {
			case "gr_poly":
				out.push({ pts: ptsOf(g), r: w / 2 });
				break;
			case "gr_line":
				out.push({ pts: [P(child(g, "start")), P(child(g, "end"))], r: w / 2 });
				break;
			case "gr_rect": {
				const [a, b] = [P(child(g, "start")), P(child(g, "end"))];
				out.push({ pts: [a, [b[0], a[1]], b, [a[0], b[1]]], r: w / 2 });
				break;
			}
			case "gr_circle": {
				const c = P(child(g, "center"));
				out.push({ pts: [c], r: dist(c, P(child(g, "end"))) + w / 2 });
				break;
			}
			case "gr_arc": {
				const pts = arcPoints(P(child(g, "start")), P(child(g, "mid")), P(child(g, "end")), Math.PI / 36);
				for (let i = 1; i < pts.length; i++) out.push({ pts: [pts[i - 1]!, pts[i]!], r: w / 2 });
				break;
			}
		}
	}
	return out;
}

/** A pad's copper shapes in board coordinates. */
export function padShapes(fp: List, pad: List): Shape[] {
	return padGeometry(fp, pad).shapes;
}

/** A pad's copper shapes and their centre (KiCad's ShapePos: the pad position moved by the turned drill offset), board coordinates. */
function padGeometry(fp: List, pad: List): { centre: Pt; shapes: Shape[] } {
	const p = placement(fp);
	const at = child(pad, "at");
	const pos = toBoard(p, P(at));
	const angle = at && at.length > 3 ? num(at, 3) : 0;
	const size = child(pad, "size");
	const [w, h] = size ? [num(size, 1), num(size, 2)] : [0, 0];
	const off = child(child(pad, "drill") ?? [], "offset");
	const o: Pt = off ? P(off) : [0, 0];
	const place = (q: Pt): Pt => toBoard({ at: pos, rot: angle }, [q[0] + o[0], q[1] + o[1]]);
	const parts: { pts: Pt[]; r: number }[] = [];
	if (text(pad, 3) === "custom") {
		const anchor = text(child(child(pad, "options") ?? [], "anchor")) ?? "circle";
		parts.push(anchor === "rect" ? { pts: [[-w / 2, -h / 2], [w / 2, -h / 2], [w / 2, h / 2], [-w / 2, h / 2]], r: 0 } : { pts: [[0, 0]], r: w / 2 });
		parts.push(...primitiveShapes(pad));
	} else parts.push(padOutline(pad, w, h));
	return { centre: place([0, 0]), shapes: parts.filter(s => s.pts.length).map(s => shape(s.pts.map(place), s.r)) };
}

function zoneSettings(z: List): ZoneSettings {
	const cp = child(z, "connect_pads");
	const mode = cp?.[1] instanceof Sym ? cp[1].v : undefined;
	const fill = child(z, "fill") ?? [];
	const n = (l: List | undefined, k: string) => (child(l ?? [], k) ? num(child(l!, k)) : undefined);
	const island = n(fill, "island_removal_mode") ?? 0;
	const hatch = text(child(fill, "mode")) === "hatch";
	return {
		clearance: n(cp, "clearance") ?? 0,
		min_thickness: n(z, "min_thickness") ?? 0,
		connect_pads: mode === "yes" ? "solid" : mode === "no" ? "none" : mode === "thru_hole_only" ? "thru_hole_only" : "thermal",
		thermal_gap: n(fill, "thermal_gap") ?? 0,
		thermal_spoke_width: n(fill, "thermal_bridge_width") ?? 0,
		island_removal: island === 1 ? "never" : island === 2 ? "below_area" : "always",
		...(island === 2 && n(fill, "island_area_min") !== undefined ? { island_min_area: n(fill, "island_area_min") } : {}),
		fill_mode: hatch ? "hatch" : "solid",
		...(hatch
			? { hatch_thickness: n(fill, "hatch_thickness") ?? 0, hatch_gap: n(fill, "hatch_gap") ?? 0, hatch_orientation: n(fill, "hatch_orientation") ?? 0 }
			: {}),
	};
}

function zoneLayers(z: List, all: readonly string[]): string[] {
	const l = child(z, "layers") ?? child(z, "layer");
	const names = atoms(l);
	if (names.some(n => n.includes("*") || n.includes("&"))) return copperOf(names, all);
	return names;
}

function drawingOf(g: List, net: (l: List) => string): Drawing | undefined {
	const m = /^gr_(line|rect|circle|arc|poly|curve)$/.exec(head(g) ?? "");
	if (!m) return undefined;
	const type = m[1] as DrawingType;
	const stroke = child(g, "stroke");
	const width = stroke && child(stroke, "width") ? num(child(stroke, "width")) : child(g, "width") ? num(child(g, "width")) : 0;
	const f = child(g, "fill");
	const fill = f ? ["yes", "solid"].includes(text(f) ?? "") || text(child(f, "type")) === "solid" : false;
	const d: Drawing = { uuid: uuidOf(g), type, layer: text(child(g, "layer")) ?? "", width, fill, locked: lockedOf(g), node: g };
	if (type === "line" || type === "rect" || type === "arc") {
		d.start = P(child(g, "start"));
		d.end = P(child(g, "end"));
		if (type === "arc") {
			d.mid = P(child(g, "mid"));
			const a = arcGeometry(d.start, d.mid, d.end);
			if (a) {
				d.center = [r4(a.center[0]), r4(a.center[1])];
				d.radius = r4(a.radius);
			}
		}
	} else if (type === "circle") {
		d.center = P(child(g, "center"));
		d.end = P(child(g, "end"));
		d.radius = r4(dist(d.center, d.end));
	} else d.points = children(child(g, "pts") ?? [], "xy").map(P);
	const n = net(g);
	if (n) d.net = n;
	return d;
}

/** The copper of a drawing on a copper layer: filled shapes as areas, outlines as strokes of its width. */
function drawingShapes(d: Drawing): Shape[] {
	const r = d.width / 2;
	const stroke = (pts: Pt[], closed: boolean): Shape[] => {
		const out = pts.slice(1).map((p, i) => shape([pts[i]!, p], r));
		if (closed && pts.length > 2) out.push(shape([pts[pts.length - 1]!, pts[0]!], r));
		return out;
	};
	switch (d.type) {
		case "line":
			return [shape([d.start!, d.end!], r)];
		case "arc":
			return arcShapes(d.start!, d.mid!, d.end!, r);
		case "rect": {
			const [a, b] = [d.start!, d.end!];
			const corners: Pt[] = [a, [b[0], a[1]], b, [a[0], b[1]]];
			return d.fill ? [shape(corners, r)] : stroke(corners, true);
		}
		case "circle": {
			if (d.fill) return [shape([d.center!], d.radius! + r)];
			const ring = Array.from({ length: 72 }, (_, i): Pt => [d.center![0] + d.radius! * Math.cos((i * Math.PI) / 36), d.center![1] + d.radius! * Math.sin((i * Math.PI) / 36)]);
			return stroke(ring, true);
		}
		case "poly":
			return d.fill ? [shape(d.points!, r)] : stroke(d.points!, true);
		case "curve": {
			const [p0, p1, p2, p3] = d.points!;
			if (!p3) return stroke(d.points!, false);
			const bez = Array.from({ length: 33 }, (_, i): Pt => {
				const t = i / 32;
				const [a, b, c, e] = [(1 - t) ** 3, 3 * (1 - t) ** 2 * t, 3 * (1 - t) * t * t, t ** 3];
				return [a * p0![0] + b * p1![0] + c * p2![0] + e * p3[0], a * p0![1] + b * p1![1] + c * p2![1] + e * p3[1]];
			});
			return stroke(bez, false);
		}
	}
}

function textOf(t: List): BoardText | undefined {
	const h = head(t);
	if (h !== "gr_text" && h !== "gr_text_box") return undefined;
	const effects = child(t, "effects");
	const font = child(effects ?? [], "font");
	const size = child(font ?? [], "size");
	const at = child(t, "at");
	const justify = atoms(child(effects ?? [], "justify"));
	const layer = child(t, "layer");
	const out: BoardText = {
		uuid: uuidOf(t),
		type: h === "gr_text" ? "text" : "textbox",
		text: text(t, 1) ?? "",
		at: at ? P(at) : child(t, "start") ? P(child(t, "start")) : ptsOf(t)[0] ?? [0, 0],
		size: size ? [num(size, 2), num(size, 1)] : [1, 1],
		thickness: child(font ?? [], "thickness") ? num(child(font!, "thickness")) : 0,
		rot: at && at.length > 3 ? num(at, 3) : child(t, "angle") ? num(child(t, "angle")) : 0,
		justify: justify.filter(j => j !== "mirror"),
		layer: text(layer) ?? "",
		knockout: atoms(layer).includes("knockout"),
		mirrored: justify.includes("mirror"),
		hidden: (child(t, "hide") !== undefined && text(child(t, "hide")) !== "no") || child(effects ?? [], "hide") !== undefined,
		locked: lockedOf(t),
		node: t,
	};
	if (h === "gr_text_box") {
		if (child(t, "end")) out.end = P(child(t, "end"));
		else out.corners = ptsOf(t);
	}
	return out;
}

function generatedOf(g: List): Generated {
	const settings: Record<string, string | number | boolean> = {};
	for (const c of g.filter(isList)) {
		const k = head(c) ?? "";
		if (["uuid", "type", "name", "layer", "members"].includes(k) || c.length !== 2 || isList(c[1])) continue;
		const v = c[1]!;
		settings[k] = v instanceof Str ? v.v : v.v === "yes" ? true : v.v === "no" ? false : Number.isNaN(Number(v.v)) ? v.v : Number(v.v);
	}
	const members = (child(g, "members") ?? []).slice(1).flatMap(a => (isList(a) ? [] : [a.v]));
	const net = text(child(g, "last_netname"));
	return {
		uuid: uuidOf(g),
		type: text(child(g, "type")) ?? "",
		name: text(child(g, "name")) ?? "",
		layer: text(child(g, "layer")) ?? "",
		members,
		...(net ? { net } : {}),
		settings,
		node: g,
	};
}

// ---- connectivity --------------------------------------------------------------------------------

/** One copper item of a net, as the connectivity sees it. */
export interface CopperNode {
	item: CopperItem | CopperDrawing;
	layers: string[];
	shapes: Shape[];
	box: Box;
	/** Points other items connect to (track ends, via and pad positions); none for fills. */
	anchors: Pt[];
}

/** Where two items join (indices into `nodes`): a point on each (for tracks, on the centreline), on a layer. */
export interface Junction {
	a: number;
	pa: Pt;
	b: number;
	pb: Pt;
	layer: string;
}

export interface Cluster {
	pads: Pad[];
	tracks: Track[];
	vias: Via[];
	fills: Fill[];
	/** Board drawings with the net on a copper layer. */
	drawings: Drawing[];
}

/** An unrouted connection: the nearest pads of two clusters of a net (distance between their copper centres, as KiCad's ratsnest). */
export interface RatLine {
	from: Pad;
	to: Pad;
	distance: number;
}

export interface NetConnectivity {
	net: string;
	pads: Pad[];
	tracks: Track[];
	vias: Via[];
	fills: Fill[];
	drawings: CopperDrawing[];
	/** Connected groups of copper, those with pads first. */
	clusters: Cluster[];
	/** Pad clusters joined by the nearest pad pairs (KiCad's ratsnest): clusters with pads − 1 lines. */
	unrouted: RatLine[];
	/** The net's copper items and where they join, for paths. */
	nodes: CopperNode[];
	junctions: Junction[];
}

/**
 * A net's length as KiCad's length rules measure it (length_delay_calculation.cpp with the
 * matched-length DRC's optimisations): tracks and arcs with the part inside the pad or via they
 * end in counted as the straight line from its centre to where they leave it, via heights between
 * the outermost layers joined at each via (use_height_for_length_calcs), and pad-to-die lengths.
 */
export interface NetLength {
	net: string;
	/** Tracks and arcs, mm. */
	track: number;
	perLayer: Record<string, number>;
	vias: number;
	/** Via heights counted (0 unless use_height_for_length_calcs), mm. */
	viaHeight: number;
	/** Pad-to-die lengths of the net's pads, mm. */
	padToDie: number;
	/** track + viaHeight + padToDie. */
	total: number;
}

export interface PathLength {
	from: string;
	to: string;
	/** Copper length pad to pad (tracks + via heights + straight lines across zone fills + the two pads' pad-to-die lengths), mm. */
	length: number;
	track: number;
	viaHeight: number;
	vias: number;
	perLayer: Record<string, number>;
	/** Zones the path crosses (their length is the straight line between the entry and exit points). */
	zones: string[];
	/** Pad-to-die lengths of the two end pads, mm. */
	padToDie: number;
}

export interface LengthOptions {
	/** Count via heights from the stack-up (the .kicad_pro's use_height_for_length_calcs). */
	useHeight: boolean;
}

/**
 * The diff-pair partner of a net by KiCad's naming (DRC_ENGINE::MatchDpSuffix): the last of
 * "+", "-", "P", "N" before trailing digits and underscores, swapped ("USB_D+" ↔ "USB_D-",
 * "LVDS_P0" ↔ "LVDS_N0"); `positive` is true for the + / P side.
 */
export function dpPartnerName(net: string): { partner: string; positive: boolean } | undefined {
	let count = 0;
	for (let i = net.length - 1; i >= 0; i--, count++) {
		const ch = net[i]!;
		if ((ch >= "0" && ch <= "9") || ch === "_") continue;
		const swap: Record<string, [string, boolean]> = { "+": ["-", true], "-": ["+", false], P: ["N", true], N: ["P", false] };
		const s = swap[ch];
		if (!s) return undefined;
		return { partner: net.slice(0, i) + s[0] + net.slice(i + 1), positive: s[1] };
	}
	return undefined;
}

class UnionFind {
	private p: number[];
	constructor(n: number) {
		this.p = Array.from({ length: n }, (_, i) => i);
	}
	find(i: number): number {
		while (this.p[i] !== i) {
			this.p[i] = this.p[this.p[i]!]!;
			i = this.p[i]!;
		}
		return i;
	}
	union(a: number, b: number): void {
		this.p[this.find(a)] = this.find(b);
	}
}

/** A point's position on a track's centreline: its closest point and the fraction along it. */
function onTrack(t: Track, p: Pt): { q: Pt; t: number } {
	if (t.kind === "track") return closestOnSeg(p, t.start, t.end);
	const c = t.center!;
	const a0 = Math.atan2(t.start[1] - c[1], t.start[0] - c[0]);
	let da = Math.atan2(p[1] - c[1], p[0] - c[0]) - a0;
	const sw = t.sweep!;
	// the angle from the start in the sweep's direction, clamped to the arc
	if (sw > 0) da = ((da % (2 * Math.PI)) + 2 * Math.PI) % (2 * Math.PI);
	else da = -((((-da) % (2 * Math.PI)) + 2 * Math.PI) % (2 * Math.PI));
	let f = da / sw;
	if (f > 1) f = Math.abs(da - sw) < Math.abs(2 * Math.PI - Math.abs(da)) ? 1 : 0;
	f = Math.max(0, Math.min(1, f));
	const a = a0 + sw * f;
	return { q: [c[0] + t.radius! * Math.cos(a), c[1] + t.radius! * Math.sin(a)], t: f };
}

/** The board's copper, drawings and texts, read once; connectivity and lengths computed per net on demand. */
export class Copper {
	readonly copperLayers: string[];
	readonly tracks: Track[] = [];
	readonly vias: Via[] = [];
	readonly pads: Pad[] = [];
	readonly zones: Zone[] = [];
	readonly ruleAreas: RuleArea[] = [];
	readonly drawings: Drawing[] = [];
	readonly texts: BoardText[] = [];
	readonly generated: Generated[] = [];
	/** Board drawings with a net on a copper layer: copper the connectivity counts. */
	readonly copperDrawings: CopperDrawing[] = [];
	private readonly byNet = new Map<string, { pads: Pad[]; tracks: Track[]; vias: Via[]; fills: Fill[]; drawings: CopperDrawing[] }>();
	private readonly conn = new Map<string, NetConnectivity>();
	/** Per footprint, groups of pad numbers KiCad joins as jumpers (duplicate_pad_numbers_are_jumpers, jumper_pad_groups). */
	private readonly jumpers = new Map<string, string[][]>();

	private constructor(readonly board: Board) {
		this.copperLayers = board.copperLayers();
		const tree = board.tree;
		const net = netReader(tree);
		const copper = this.copperLayers;
		for (const c of tree) {
			if (!isList(c)) continue;
			switch (head(c)) {
				case "segment":
				case "arc": {
					const start = P(child(c, "start"));
					const end = P(child(c, "end"));
					const width = num(child(c, "width"));
					const base = { uuid: uuidOf(c), net: net(c), layer: text(child(c, "layer")) ?? "", width, start, end, locked: lockedOf(c), node: c };
					if (head(c) === "segment") this.tracks.push({ kind: "track", ...base, length: dist(start, end), shapes: [shape([start, end], width / 2)] });
					else {
						const mid = P(child(c, "mid"));
						const g = arcGeometry(start, mid, end);
						this.tracks.push({
							kind: "arc",
							...base,
							mid,
							...(g ? { center: g.center, radius: g.radius, sweep: g.sweep } : {}),
							length: g ? g.length : dist(start, end),
							shapes: arcShapes(start, mid, end, width / 2),
						});
					}
					break;
				}
				case "via": {
					const ends = atoms(child(c, "layers"));
					const [a, b] = [ends[0] ?? copper[0]!, ends[1] ?? copper[copper.length - 1]!];
					const [i, j] = [copper.indexOf(a), copper.indexOf(b)].sort((x, y) => x - y) as [number, number];
					const flags = c.slice(1).flatMap(x => (x instanceof Sym ? [x.v] : []));
					const type: ViaType = flags.includes("micro") ? "micro" : flags.includes("blind") ? "blind" : flags.includes("buried") ? "buried" : "through";
					const at = P(child(c, "at"));
					const size = num(child(c, "size"));
					this.vias.push({
						kind: "via",
						uuid: uuidOf(c),
						net: net(c),
						at,
						size,
						drill: child(c, "drill") ? num(child(c, "drill")) : 0,
						type,
						layers: [a, b],
						span: i >= 0 && j >= 0 ? copper.slice(i, j + 1) : copper.filter(l => l === a || l === b),
						locked: lockedOf(c),
						shapes: [shape([at], size / 2)],
						node: c,
					});
					break;
				}
				case "zone":
					this.readZone(c, net(c));
					break;
				case "generated":
					this.generated.push(generatedOf(c));
					break;
				default: {
					const d = drawingOf(c, net);
					if (d) {
						this.drawings.push(d);
						if (d.net && copper.includes(d.layer)) this.copperDrawings.push({ kind: "drawing", drawing: d, net: d.net, layer: d.layer, shapes: drawingShapes(d) });
					} else {
						const t = textOf(c);
						if (t) this.texts.push(t);
					}
				}
			}
		}
		for (const fp of board.footprints()) {
			const ref = fpRef(fp);
			const nodes = children(fp, "pad");
			padsInfo(fp).forEach((info, k) => {
				const node = nodes[k]!;
				const cu = info.type === "np_thru_hole" ? [] : info.type === "thru_hole" ? [...copper] : copperOf(info.layers, copper);
				const g = padGeometry(fp, node);
				this.pads.push({
					kind: "pad",
					ref,
					number: info.number,
					id: `${ref}.${info.number}`,
					// net(node), not info.net: older boards number a pad's net, `(net 3 "GND")`
					net: net(node),
					at: info.at,
					centre: g.centre,
					dieLength: child(node, "die_length") ? num(child(node, "die_length")) : 0,
					copper: cu,
					info,
					shapes: g.shapes,
					fp,
					node,
				});
			});
			// zones and keepouts of a footprint are stored in board coordinates
			for (const z of children(fp, "zone")) this.readZone(z, net(z), ref);
			// `(jumper_pad_groups ("1" "2") …)`: lists of pad numbers without a head
			const groups = (child(fp, "jumper_pad_groups") ?? []).slice(1).filter(isList).map(g => g.flatMap(a => (isList(a) ? [] : [a.v])));
			if (text(child(fp, "duplicate_pad_numbers_are_jumpers")) === "yes") for (const n of new Set(nodes.map(p => text(p, 1) ?? ""))) groups.push([n]);
			if (groups.length) this.jumpers.set(ref, groups);
		}
		const tuning = new Map<string, string>();
		for (const g of this.generated) for (const m of g.members) tuning.set(m, g.uuid);
		for (const t of this.tracks) {
			const g = tuning.get(t.uuid);
			if (g) t.tuning = g;
		}
		const slot = (n: string) => {
			let s = this.byNet.get(n);
			if (!s) this.byNet.set(n, (s = { pads: [], tracks: [], vias: [], fills: [], drawings: [] }));
			return s;
		};
		for (const p of this.pads) if (p.net && p.copper.length) slot(p.net).pads.push(p);
		for (const t of this.tracks) if (t.net) slot(t.net).tracks.push(t);
		for (const v of this.vias) if (v.net) slot(v.net).vias.push(v);
		for (const z of this.zones) if (z.net) slot(z.net).fills.push(...z.fills);
		for (const d of this.copperDrawings) slot(d.net).drawings.push(d);
	}

	static of(board: Board): Copper {
		return new Copper(board);
	}

	private readZone(z: List, net: string, footprint?: string): void {
		const polys = children(z, "polygon").map(ptsOf);
		const outline = polys[0] ?? [];
		const holes = polys.slice(1);
		const layers = zoneLayers(z, this.copperLayers);
		const keep = child(z, "keepout");
		if (keep) {
			const no = (k: string) => text(child(keep, k)) === "not_allowed";
			this.ruleAreas.push({
				kind: "rule_area",
				uuid: uuidOf(z),
				...(text(child(z, "name")) ? { name: text(child(z, "name")) } : {}),
				layers,
				outline,
				holes,
				keepout: { tracks: no("tracks"), vias: no("vias"), pads: no("pads"), copperpour: no("copperpour"), footprints: no("footprints") },
				locked: lockedOf(z),
				node: z,
				...(footprint ? { footprint } : {}),
			});
			return;
		}
		const fills: Fill[] = children(z, "filled_polygon").map(f => {
			const pts = ptsOf(f);
			return { kind: "fill", zone: uuidOf(z), layer: text(child(f, "layer")) ?? layers[0] ?? "", shape: shape(pts), area: polygonArea(pts) };
		});
		const filledArea: Record<string, number> = {};
		for (const f of fills) filledArea[f.layer] = (filledArea[f.layer] ?? 0) + f.area;
		const attr = child(z, "attr");
		this.zones.push({
			kind: "zone",
			uuid: uuidOf(z),
			...(text(child(z, "name")) ? { name: text(child(z, "name")) } : {}),
			net,
			layers,
			priority: child(z, "priority") ? num(child(z, "priority")) : 0,
			outline,
			holes,
			settings: zoneSettings(z),
			filled: text(child(z, "fill")) === "yes",
			fills,
			filledArea,
			teardrop: !!attr && !!child(attr, "teardrop"),
			locked: lockedOf(z),
			node: z,
			...(footprint ? { footprint } : {}),
		});
	}

	/** Every net with copper on the board (pads, tracks, vias, zones), sorted. */
	nets(): string[] {
		return [...this.byNet.keys()].sort();
	}

	hasNet(net: string): boolean {
		return this.byNet.has(net);
	}

	/** The net as the board names it; an unknown name throws, listing the nets that contain it. */
	net(name: string): string {
		if (this.byNet.has(name)) return name;
		const low = name.toLowerCase().replace(/^\//, "");
		const like = this.nets().filter(n => n.toLowerCase().includes(low));
		throw new Error(`no net "${name}" on the board${like.length ? `; nets like it: ${like.slice(0, 12).join(", ")}` : ""}`);
	}

	/** The pads of "R13.2" (several when the footprint repeats the number); throws when there are none. */
	pad(id: string): Pad[] {
		const dot = id.lastIndexOf(".");
		if (dot <= 0) throw new Error(`pad "${id}": give it as REF.NUMBER, e.g. "R13.2"`);
		const ref = id.slice(0, dot);
		const number = id.slice(dot + 1);
		const fpPads = this.pads.filter(p => p.ref === ref);
		if (!fpPads.length) this.board.footprint(ref);
		const hit = fpPads.filter(p => p.number === number);
		if (!hit.length) throw new Error(`${ref} has no pad ${number} (pads: ${[...new Set(fpPads.map(p => p.number))].join(", ")})`);
		return hit;
	}

	/** The diff-pair partner of a net when the board has it. */
	dpPartner(net: string): { partner: string; positive: boolean } | undefined {
		const m = dpPartnerName(net);
		return m && this.byNet.has(m.partner) ? m : undefined;
	}

	/** The fills of zones on this layer whose copper touches the shape (any net unless given). */
	fillsTouching(s: Shape, layer: string, net?: string): { zone: Zone; fill: Fill }[] {
		const out: { zone: Zone; fill: Fill }[] = [];
		for (const z of this.zones) {
			if (net !== undefined && z.net !== net) continue;
			for (const f of z.fills) if (f.layer === layer && shapesTouch(f.shape, s)) out.push({ zone: z, fill: f });
		}
		return out;
	}

	/** Copper at a point on a layer: pads, tracks, vias and zone fills whose copper holds it (any net unless given). */
	itemsAt(p: Pt, layer: string, net?: string): CopperItem[] {
		const ok = (n: string) => net === undefined || n === net;
		return [
			...this.pads.filter(x => ok(x.net) && x.copper.includes(layer) && x.shapes.some(s => shapeHas(s, p))),
			...this.tracks.filter(x => ok(x.net) && x.layer === layer && x.shapes.some(s => shapeHas(s, p))),
			...this.vias.filter(x => ok(x.net) && x.span.includes(layer) && shapeHas(x.shapes[0]!, p)),
			...this.zones.filter(z => ok(z.net)).flatMap(z => z.fills.filter(f => f.layer === layer && shapeHas(f.shape, p))),
		];
	}

	/** Connected clusters and unrouted pad pairs of a net (cached). */
	connectivity(net: string): NetConnectivity {
		const hit = this.conn.get(net);
		if (hit) return hit;
		const items = this.byNet.get(net) ?? { pads: [], tracks: [], vias: [], fills: [], drawings: [] };
		const nodes: CopperNode[] = [
			...items.pads.map(p => ({ item: p as CopperItem | CopperDrawing, layers: p.copper, shapes: p.shapes, anchors: [p.at] })),
			...items.tracks.map(t => ({ item: t, layers: [t.layer], shapes: t.shapes, anchors: [t.start, t.end] })),
			...items.vias.map(v => ({ item: v, layers: v.span, shapes: v.shapes, anchors: [v.at] })),
			...items.fills.map(f => ({ item: f, layers: [f.layer], shapes: [f.shape], anchors: [] as Pt[] })),
			...items.drawings.map(d => ({
				item: d,
				layers: [d.layer],
				shapes: d.shapes,
				anchors: d.drawing.type === "line" || d.drawing.type === "arc" ? [d.drawing.start!, d.drawing.end!] : ([] as Pt[]),
			})),
		].map(n => ({ ...n, box: bbox(n.shapes.flatMap(s => [[s.box[0], s.box[1]] as Pt, [s.box[2], s.box[3]] as Pt])) }));
		const junctions: Junction[] = [];
		const uf = new UnionFind(nodes.length);
		const join = (a: number, pa: Pt, b: number, pb: Pt, layer: string) => {
			junctions.push({ a, pa, b, pb, layer });
			uf.union(a, b);
		};
		for (let i = 0; i < nodes.length; i++) {
			const a = nodes[i]!;
			for (let j = i + 1; j < nodes.length; j++) {
				const b = nodes[j]!;
				if (a.box[0] > b.box[2] + EPS || b.box[0] > a.box[2] + EPS || a.box[1] > b.box[3] + EPS || b.box[1] > a.box[3] + EPS) continue;
				const common = a.layers.filter(l => b.layers.includes(l));
				if (!common.length) continue;
				const fa = a.item.kind === "fill";
				const fb = b.item.kind === "fill";
				if (fa && fb) {
					const p = fillsTouch(a.item as Fill, b.item as Fill);
					if (p) join(i, p, j, p, common[0]!);
				} else if (fa || fb) {
					const [f, o, fi, oi] = fa ? [a, b, i, j] : [b, a, j, i];
					const p = fillJoins(f.shapes[0]!, o);
					if (p) join(oi, pointOn(o, p), fi, p, common[0]!);
				} else {
					// tracks, arcs, vias, pads, drawings: joined where their copper overlaps on a shared layer
					const p = itemsTouch(a, b);
					if (!p) continue;
					// a pad joins a via on the pad's own side (KiCad's via length takes that layer)
					const pad = a.item.kind === "pad" ? a.item : b.item.kind === "pad" ? b.item : undefined;
					const side = pad ? this.padSide(pad) : undefined;
					const layers = side && (a.item.kind === "via" || b.item.kind === "via") ? [common.includes(side) ? side : common[0]!] : common;
					const single = a.item.kind === "track" || a.item.kind === "arc" || b.item.kind === "track" || b.item.kind === "arc";
					for (const l of single ? layers.slice(0, 1) : layers) join(i, pointOn(a, p), j, pointOn(b, p), l);
				}
			}
		}
		// pads a footprint joins as jumpers
		const padIndex = new Map(nodes.map((n, i) => [n.item, i] as const));
		for (const [ref, groups] of this.jumpers) {
			for (const g of groups) {
				const members = items.pads.filter(p => p.ref === ref && g.includes(p.number));
				const head = members[0];
				for (const p of members.slice(1)) join(padIndex.get(head!)!, head!.at, padIndex.get(p)!, p.at, head!.copper.find(l => p.copper.includes(l)) ?? p.copper[0] ?? "");
			}
		}
		const groups = new Map<number, Cluster>();
		nodes.forEach((n, i) => {
			const r = uf.find(i);
			let c = groups.get(r);
			if (!c) groups.set(r, (c = { pads: [], tracks: [], vias: [], fills: [], drawings: [] }));
			const it = n.item;
			if (it.kind === "fill") c.fills.push(it);
			else if (it.kind === "pad") c.pads.push(it);
			else if (it.kind === "via") c.vias.push(it);
			else if (it.kind === "drawing") c.drawings.push(it.drawing);
			else c.tracks.push(it);
		});
		const clusters = [...groups.values()].sort((x, y) => (y.pads.length ? 1 : 0) - (x.pads.length ? 1 : 0));
		const out: NetConnectivity = { net, ...items, clusters, unrouted: ratsnest(clusters.filter(c => c.pads.length)), nodes, junctions };
		this.conn.set(net, out);
		return out;
	}

	/** The copper layer on a pad's own side: an SMD pad's first copper layer, the footprint's side for a through-hole pad. */
	private padSide(p: Pad): string {
		if (p.info.type !== "thru_hole") return p.copper[0] ?? this.copperLayers[0]!;
		return placement(p.fp).side === "bottom" ? this.copperLayers[this.copperLayers.length - 1]! : this.copperLayers[0]!;
	}

	/**
	 * The net's tracks and arcs as KiCad's length rules see them: lines merged where exactly two
	 * ends meet on a layer, then the part inside a via they end in, and inside a pad they end on
	 * the position of, replaced by the straight line from the centre to where they leave it.
	 */
	private chains(net: string): Chain[] {
		const c = this.connectivity(net);
		const lines: Chain[] = c.tracks.map(chainOf);
		const key = (layer: string, p: Pt) => `${layer}|${Math.round(p[0] * 1e6)},${Math.round(p[1] * 1e6)}`;
		const ends = new Map<string, Chain[]>();
		for (const l of lines) {
			for (const p of [l.pts[0]!, l.pts[l.pts.length - 1]!]) {
				const k = key(l.layer, p);
				ends.set(k, [...(ends.get(k) ?? []), l]);
			}
		}
		const merged = new Set<Chain>();
		const out: Chain[] = [];
		for (const first of lines) {
			if (merged.has(first)) continue;
			merged.add(first);
			let chain = first;
			for (let grew = true; grew; ) {
				grew = false;
				for (const atStart of [true, false]) {
					const there = ends.get(key(chain.layer, atStart ? chain.pts[0]! : chain.pts[chain.pts.length - 1]!)) ?? [];
					const other = there.find(l => !merged.has(l));
					// two ends on a layer are a joint; more are a branch
					if (there.length !== 2 || !other) continue;
					merged.add(other);
					chain = joinChains(chain, other, atStart);
					grew = true;
				}
			}
			out.push(chain);
		}
		for (const v of c.vias) {
			const inVia = (p: Pt) => dist(p, v.at) <= v.size / 2 + EPS;
			for (const ch of out) {
				if (!v.span.includes(ch.layer)) continue;
				const [s, e] = [ch.pts[0]!, ch.pts[ch.pts.length - 1]!];
				const forward = inVia(s) ? true : inVia(e) ? false : undefined;
				if (forward !== undefined) ch.clips.push({ by: v, at: forward ? s : e, track: forward ? ch.first : ch.last });
				clipChain(ch, inVia, v.at, forward);
			}
		}
		for (const p of c.pads) {
			const inPad = (q: Pt) => p.shapes.some(sh => shapeHas(sh, q));
			for (const ch of out) {
				const [s, e] = [ch.pts[0]!, ch.pts[ch.pts.length - 1]!];
				// only lines ending on the pad's position
				if (!p.copper.includes(ch.layer) || (dist(s, p.at) > EPS && dist(e, p.at) > EPS)) continue;
				const forward = inPad(s) ? true : inPad(e) ? false : undefined;
				if (forward !== undefined) ch.clips.push({ by: p, at: forward ? s : e, track: forward ? ch.first : ch.last });
				clipChain(ch, inPad, p.at, forward);
			}
		}
		return out;
	}

	/**
	 * Each via's height as KiCad's length rules count it: between the outermost layers of the
	 * tracks and arcs that end in it and of the first pad it touches (that pad's own side); 0 for a
	 * stub or when the project doesn't count via heights.
	 */
	private viaHeights(net: string, opts: LengthOptions): Map<Via, number> {
		const c = this.connectivity(net);
		const out = new Map<Via, number>();
		const su = opts.useHeight ? this.board.stackup() : undefined;
		c.nodes.forEach((n, i) => {
			const v = n.item;
			if (v.kind !== "via") return;
			const layers = new Set<string>();
			for (const t of c.tracks) if (dist(t.start, v.at) <= v.size / 2 + EPS || dist(t.end, v.at) <= v.size / 2 + EPS) layers.add(t.layer);
			const j = c.junctions.find(x => (x.a === i && c.nodes[x.b]!.item.kind === "pad") || (x.b === i && c.nodes[x.a]!.item.kind === "pad"));
			if (j) layers.add(this.padSide(c.nodes[j.a === i ? j.b : j.a]!.item as Pad));
			const idx = [...layers].map(l => this.copperLayers.indexOf(l)).filter(k => k >= 0).sort((x, y) => x - y);
			out.set(v, su && idx.length >= 2 ? layerDistance(su, this.copperLayers, this.copperLayers[idx[0]!]!, this.copperLayers[idx[idx.length - 1]!]!) : 0);
		});
		return out;
	}

	/** Routed length of a net as KiCad's length rules measure it (see NetLength). */
	netLength(net: string, opts: LengthOptions): NetLength {
		const c = this.connectivity(net);
		const perLayer: Record<string, number> = {};
		let track = 0;
		for (const ch of this.chains(net)) {
			const l = ch.lens.reduce((x, y) => x + y, 0);
			perLayer[ch.layer] = (perLayer[ch.layer] ?? 0) + l;
			track += l;
		}
		const viaHeight = [...this.viaHeights(net, opts).values()].reduce((x, y) => x + y, 0);
		const padToDie = c.pads.reduce((x, p) => x + p.dieLength, 0);
		return { net, track, perLayer, vias: c.vias.length, viaHeight, padToDie, total: track + viaHeight + padToDie };
	}

	/**
	 * The shortest copper path between two pads of one net ("R13.2" → "U1.10"): along tracks and
	 * arcs (from where one meets another), through vias (their height between the layers left and
	 * entered when `useHeight`; a pad joins a via on its own side), across zone fills as straight
	 * lines, plus the two pads' pad-to-die lengths. Track ends inside a pad or via count as KiCad's
	 * length rules count them (see NetLength). Undefined when the pads aren't connected.
	 */
	path(from: string, to: string, opts: LengthOptions): PathLength | undefined {
		const a = this.pad(from);
		const b = this.pad(to);
		const net = a[0]!.net;
		if (!net) throw new Error(`${from} is on no net`);
		if (b[0]!.net !== net) throw new Error(`${from} is on ${net} but ${to} is on ${b[0]!.net || "no net"}`);
		const c = this.connectivity(net);
		const su = opts.useHeight ? this.board.stackup() : undefined;
		// what KiCad's in-pad and in-via clipping takes off each chain end, put on the track at that end
		const trim = new Map<Track, [number, number]>();
		// a pad or via that clipped a chain joins it only at the clipped end (on the end track, wherever
		// KiCad's connectivity saw them touch): its other junctions with the chain would skip the clipped length
		const clipped = new Map<Pad | Via, { tracks: Set<Track>; at: Pt; track: Track; layer: string }[]>();
		for (const ch of this.chains(net)) {
			const [d0, d1] = ch.trimmed;
			const startAtTrackStart = dist(ch.first.start, ch.pts[0]!) <= dist(ch.first.end, ch.pts[0]!);
			const endAtTrackEnd = dist(ch.last.end, ch.pts[ch.pts.length - 1]!) <= dist(ch.last.start, ch.pts[ch.pts.length - 1]!);
			const add = (t: Track, atStart: boolean, d: number) => {
				const cur = trim.get(t) ?? [0, 0];
				cur[atStart ? 0 : 1] += d;
				trim.set(t, cur);
			};
			if (d0) add(ch.first, startAtTrackStart, d0);
			if (d1) add(ch.last, !endAtTrackEnd, d1);
			const tracks = new Set(ch.tracks);
			for (const cl of ch.clips) clipped.set(cl.by, [...(clipped.get(cl.by) ?? []), { tracks, at: cl.at, track: cl.track, layer: ch.layer }]);
		}
		const skip = (j: Junction): boolean => {
			const [x, y] = [c.nodes[j.a]!.item, c.nodes[j.b]!.item];
			const [by, t] = x.kind === "pad" || x.kind === "via" ? [x, y] : [y, x];
			if (by.kind !== "pad" && by.kind !== "via") return false;
			return (clipped.get(by) ?? []).some(cl => cl.tracks.has(t as Track));
		};
		const g = new Graph();
		const key = (i: number, p: Pt, layer: string): string => {
			const it = c.nodes[i]!.item;
			if (it.kind === "fill") return `f${i}:${r4(p[0])},${r4(p[1])}`;
			if (it.kind === "via") return `v${i}:${layer}`;
			if (it.kind === "pad") return `p${i}`;
			if (it.kind === "drawing") return `d${i}`;
			return `t${i}:${Math.round(onTrack(it, p).t * 1e9)}`;
		};
		// points along each track where something joins it, in order, joined by the track's length
		const stops = new Map<number, Map<string, number>>();
		const addStop = (i: number, p: Pt, layer: string) => {
			const it = c.nodes[i]!.item;
			const k = key(i, p, layer);
			if (it.kind === "fill") g.member(`f${i}`, k, p, this.zones.find(z => z.uuid === it.zone));
			else if (it.kind === "track" || it.kind === "arc") {
				let m = stops.get(i);
				if (!m) stops.set(i, (m = new Map()));
				m.set(k, onTrack(it, p).t);
			}
			return k;
		};
		c.nodes.forEach((n, i) => {
			const it = n.item;
			if (it.kind === "track" || it.kind === "arc") {
				addStop(i, it.start, it.layer);
				addStop(i, it.end, it.layer);
			}
		});
		for (const j of c.junctions) if (!skip(j)) g.edge(addStop(j.a, j.pa, j.layer), addStop(j.b, j.pb, j.layer), { len: 0 });
		const index = new Map(c.nodes.map((n, i) => [n.item, i] as const));
		for (const [by, cls] of clipped) {
			for (const cl of cls) g.edge(addStop(index.get(by)!, by.at, cl.layer), addStop(index.get(cl.track)!, cl.at, cl.layer), { len: 0 });
		}
		for (const [i, m] of stops) {
			const t = c.nodes[i]!.item as Track;
			const [t0, t1] = trim.get(t) ?? [0, 0];
			const s = [...m].sort((x, y) => x[1] - y[1]);
			for (let k = 1; k < s.length; k++) {
				const end = (k === 1 ? t0 : 0) + (k === s.length - 1 ? t1 : 0);
				g.edge(s[k - 1]![0], s[k]![0], { len: Math.max(0, t.length * (s[k]![1] - s[k - 1]![1]) + end), layer: t.layer });
			}
		}
		c.nodes.forEach((n, i) => {
			const it = n.item;
			if (it.kind !== "via") return;
			for (let k = 1; k < it.span.length; k++) {
				const h = su ? layerDistance(su, this.copperLayers, it.span[k - 1]!, it.span[k]!) : 0;
				g.edge(`v${i}:${it.span[k - 1]}`, `v${i}:${it.span[k]}`, { len: h, height: h, via: i });
			}
		});
		const keyOf = new Map(c.nodes.map((n, i) => [n.item, `p${i}`] as const));
		const r = g.shortest(
			a.map(p => keyOf.get(p)!),
			new Set(b.map(p => keyOf.get(p)!)),
		);
		if (!r) return undefined;
		const perLayer: Record<string, number> = {};
		let track = 0;
		let viaHeight = 0;
		const vias = new Set<number>();
		const zones = new Set<string>();
		for (const e of r.edges) {
			if (e.layer) {
				perLayer[e.layer] = (perLayer[e.layer] ?? 0) + e.len;
				track += e.len;
			}
			if (e.via !== undefined) {
				vias.add(e.via);
				viaHeight += e.height ?? 0;
			}
			if (e.zone) zones.add(e.zone);
		}
		const padToDie = (a.find(p => keyOf.get(p) === r.from)?.dieLength ?? 0) + (b.find(p => keyOf.get(p) === r.to)?.dieLength ?? 0);
		const length = r.edges.reduce((t, e) => t + e.len, 0) + padToDie;
		return { from, to, length, track, viaHeight, vias: vias.size, perLayer, zones: [...zones], padToDie };
	}
}

/** A point of an item for a junction: the closest point on a track's centreline, a via's or pad's position, the point itself elsewhere. */
function pointOn(n: CopperNode, p: Pt): Pt {
	const it = n.item;
	if (it.kind === "via" || it.kind === "pad") return it.at;
	if (it.kind === "track" || it.kind === "arc") return onTrack(it, p).q;
	return p;
}

/**
 * Where two non-zone items touch (KiCad's CN_VISITOR: their copper shapes collide on a shared
 * layer): an anchor of one on the other's copper when there is one, else a point in the overlap.
 */
function itemsTouch(a: CopperNode, b: CopperNode): Pt | undefined {
	for (const [x, y] of [
		[a, b],
		[b, a],
	] as const) {
		for (const p of x.anchors) if (y.shapes.some(s => shapeHas(s, p))) return p;
	}
	for (const sa of a.shapes) {
		for (const sb of b.shapes) {
			const d = shapeDistance(sa, sb, EPS);
			if (d && d.d <= EPS) return d.pa;
		}
	}
	return undefined;
}

/**
 * Where a zone fill joins an item (KiCad's checkZoneItemConnection): an anchor of the item inside
 * the fill, else any of its copper touching the fill (a pad through its thermal spokes).
 */
function fillJoins(fill: Shape, o: CopperNode): Pt | undefined {
	for (const p of o.anchors) if (insidePoly(fill, p)) return p;
	for (const s of o.shapes) {
		const d = shapeDistance(fill, s, EPS);
		if (d && d.d <= EPS) return d.pb;
	}
	return undefined;
}

/** Two fills of one net joined (KiCad's checkZoneZoneConnection): a vertex of one inside the other. One zone's fill regions never touch. */
function fillsTouch(a: Fill, b: Fill): Pt | undefined {
	if (a.zone === b.zone || a.layer !== b.layer) return undefined;
	for (const [x, y] of [
		[a.shape, b.shape],
		[b.shape, a.shape],
	] as const) {
		for (const p of x.pts) if (p[0] >= y.box[0] && p[0] <= y.box[2] && p[1] >= y.box[1] && p[1] <= y.box[3] && insidePoly(y, p)) return p;
	}
	return undefined;
}

/** A track as a line for length rules: its vertices (arcs every degree) and the length of each piece (arcs exact). */
interface Chain {
	layer: string;
	pts: Pt[];
	lens: number[];
	/** Its tracks in order; the ones at its two ends, and how much clipping changed its length there. */
	tracks: Track[];
	first: Track;
	last: Track;
	trimmed: [number, number];
	/** The vias and pads that clipped an end, that end before clipping and the track at it. */
	clips: { by: Via | Pad; at: Pt; track: Track }[];
}

function chainOf(t: Track): Chain {
	const one = { layer: t.layer, tracks: [t], first: t, last: t, trimmed: [0, 0] as [number, number], clips: [] };
	if (t.kind === "track" || !t.center) return { ...one, pts: [t.start, t.end], lens: [t.length] };
	const pts = arcPoints(t.start, t.mid!, t.end, Math.PI / 180);
	const n = pts.length - 1;
	return { ...one, pts, lens: Array.from({ length: n }, () => t.length / n) };
}

/** `a` with `b` joined at a's start (or end); b turned around as needed. */
function joinChains(a: Chain, b: Chain, atStart: boolean): Chain {
	const rev = (c: Chain): Chain => ({ ...c, pts: [...c.pts].reverse(), lens: [...c.lens].reverse(), tracks: [...c.tracks].reverse(), first: c.last, last: c.first });
	const join = (x: Chain, y: Chain): Chain => ({
		layer: a.layer,
		pts: [...x.pts, ...y.pts.slice(1)],
		lens: [...x.lens, ...y.lens],
		tracks: [...x.tracks, ...y.tracks],
		first: x.first,
		last: y.last,
		trimmed: [0, 0],
		clips: [],
	});
	if (atStart) return join(dist(b.pts[b.pts.length - 1]!, a.pts[0]!) < 1e-6 ? b : rev(b), a);
	return join(a, dist(b.pts[0]!, a.pts[a.pts.length - 1]!) < 1e-6 ? b : rev(b));
}

/**
 * KiCad's clipLineToPad / clipLineToVia: from the end inside the copper, drop the vertices inside
 * it and run a straight line from the centre to where the chain leaves it. `forward`: the start is
 * inside (undefined: neither end is). A chain that never leaves stays as it is.
 */
function clipChain(ch: Chain, inside: (p: Pt) => boolean, centre: Pt, forward: boolean | undefined): void {
	if (forward === undefined) return;
	if (!forward) {
		ch.pts.reverse();
		ch.lens.reverse();
	}
	const k = ch.pts.findIndex((p, i) => i > 0 && !inside(p));
	if (k > 0) {
		// where the piece from the last vertex inside to the first outside leaves the copper
		const [p0, p1] = [ch.pts[k - 1]!, ch.pts[k]!];
		let [lo, hi] = [0, 1];
		for (let it = 0; it < 50; it++) {
			const m = (lo + hi) / 2;
			if (inside([p0[0] + (p1[0] - p0[0]) * m, p0[1] + (p1[1] - p0[1]) * m])) lo = m;
			else hi = m;
		}
		const x: Pt = [p0[0] + (p1[0] - p0[0]) * hi, p0[1] + (p1[1] - p0[1]) * hi];
		const before = ch.lens.slice(0, k).reduce((s, l) => s + l, 0);
		const pieces = [dist(centre, x), dist(x, p1)];
		ch.pts.splice(0, k, centre, x);
		ch.lens.splice(0, k, ...pieces);
		ch.trimmed[forward ? 0 : 1] += pieces[0]! + pieces[1]! - before;
	}
	if (!forward) {
		ch.pts.reverse();
		ch.lens.reverse();
	}
}

/** The nearest pad pairs joining pad clusters (Prim's algorithm over the clusters), between the pads' copper centres. */
function ratsnest(clusters: Cluster[]): RatLine[] {
	if (clusters.length < 2) return [];
	const best = (x: Cluster, y: Cluster): RatLine => {
		let r: RatLine = { from: x.pads[0]!, to: y.pads[0]!, distance: Infinity };
		for (const p of x.pads) {
			for (const q of y.pads) {
				const d = dist(p.centre, q.centre);
				if (d < r.distance) r = { from: p, to: q, distance: d };
			}
		}
		return r;
	};
	const rest = new Set(clusters.map((_, i) => i).slice(1));
	// cheapest link from the tree to each cluster outside it
	const link = new Map<number, RatLine>();
	for (const i of rest) link.set(i, best(clusters[0]!, clusters[i]!));
	const out: RatLine[] = [];
	while (rest.size) {
		let pick = -1;
		for (const i of rest) if (pick < 0 || link.get(i)!.distance < link.get(pick)!.distance) pick = i;
		out.push(link.get(pick)!);
		rest.delete(pick);
		for (const i of rest) {
			const l = best(clusters[pick]!, clusters[i]!);
			if (l.distance < link.get(i)!.distance) link.set(i, l);
		}
	}
	return out;
}

interface Edge {
	len: number;
	layer?: string;
	via?: number;
	height?: number;
	zone?: string;
}

/**
 * A weighted graph with Dijkstra's shortest path from several sources to any of several targets.
 * A zone fill is a group: any two of its members are joined by the straight line between them,
 * relaxed when a member is reached rather than stored as k² edges.
 */
class Graph {
	private adj = new Map<string, { to: string; e: Edge }[]>();
	private groupOf = new Map<string, string>();
	private groups = new Map<string, { name: string; members: Map<string, Pt> }>();

	edge(a: string, b: string, e: Edge): void {
		if (a === b) return;
		for (const [x, y] of [
			[a, b],
			[b, a],
		] as const) {
			let l = this.adj.get(x);
			if (!l) this.adj.set(x, (l = []));
			l.push({ to: y, e });
		}
	}

	member(group: string, node: string, at: Pt, zone: Zone | undefined): void {
		let g = this.groups.get(group);
		if (!g) this.groups.set(group, (g = { name: zone?.name ?? zone?.uuid.slice(0, 8) ?? "zone", members: new Map() }));
		g.members.set(node, at);
		this.groupOf.set(node, group);
	}

	shortest(from: string[], to: Set<string>): { edges: Edge[]; from: string; to: string } | undefined {
		const d = new Map<string, number>();
		const prev = new Map<string, { from: string; e: Edge }>();
		const done = new Set<string>();
		const heap = new MinHeap();
		for (const f of from) {
			d.set(f, 0);
			heap.push(0, f);
		}
		const relax = (u: string, du: number, v: string, e: Edge) => {
			const nd = du + e.len;
			if (nd < (d.get(v) ?? Infinity) - 1e-12) {
				d.set(v, nd);
				prev.set(v, { from: u, e });
				heap.push(nd, v);
			}
		};
		for (;;) {
			const top = heap.pop();
			if (!top) return undefined;
			const [du, u] = top;
			if (done.has(u) || du > (d.get(u) ?? Infinity)) continue;
			if (to.has(u)) {
				const edges: Edge[] = [];
				let x = u;
				for (; prev.has(x); x = prev.get(x)!.from) edges.unshift(prev.get(x)!.e);
				return { edges, from: x, to: u };
			}
			done.add(u);
			for (const { to: v, e } of this.adj.get(u) ?? []) relax(u, du, v, e);
			const g = this.groups.get(this.groupOf.get(u) ?? "");
			if (g) {
				const at = g.members.get(u)!;
				for (const [v, p] of g.members) if (v !== u && !done.has(v)) relax(u, du, v, { len: dist(at, p), zone: g.name });
			}
		}
	}
}

/** A binary min-heap of [priority, key]. */
class MinHeap {
	private h: [number, string][] = [];

	push(p: number, k: string): void {
		const h = this.h;
		h.push([p, k]);
		for (let i = h.length - 1; i > 0; ) {
			const up = (i - 1) >> 1;
			if (h[up]![0] <= h[i]![0]) break;
			[h[up], h[i]] = [h[i]!, h[up]!];
			i = up;
		}
	}

	pop(): [number, string] | undefined {
		const h = this.h;
		if (!h.length) return undefined;
		const top = h[0]!;
		const last = h.pop()!;
		if (h.length) {
			h[0] = last;
			for (let i = 0; ; ) {
				const [l, r] = [2 * i + 1, 2 * i + 2];
				let m = i;
				if (l < h.length && h[l]![0] < h[m]![0]) m = l;
				if (r < h.length && h[r]![0] < h[m]![0]) m = r;
				if (m === i) break;
				[h[m], h[i]] = [h[i]!, h[m]!];
				i = m;
			}
		}
		return top;
	}
}
