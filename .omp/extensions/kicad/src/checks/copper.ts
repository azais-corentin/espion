/**
 * Board review checks on the copper and the placement (`track_widths`, `return_path`, `via_return`,
 * `decoupling_placement`, `placement`), and what they share with the `layout` check: the board
 * outline, net classes per net, impedance classes, ground nets and the copper with its zones
 * refilled. Board mm, x right, y down. Each check gives one info finding and nothing else when the
 * board has nothing it looks at (no tracks yet, no footprints placed), so default runs on a board
 * still being placed stay quiet.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { Board, courtyardEdges, fpRef, insideEdges, placement } from "../board";
import * as cli from "../cli";
import { Copper, type Fill, type Pad, type Shape, shape, shapeDistance, type Track } from "../copper";
import { boardOutline, isEdgeItem, pointInPolygon } from "../edges";
import { type Box, overlap, type Pt, r3 } from "../geom";
import type { Project } from "../project";
import { isList, type List } from "../sexpr";
import type { CheckContext, CheckDef, Finding } from "../types";
import { decouplingPairs, netLeaf, wildcards } from "./circuit";
import { type ClassRules, classRules, netSettings, patternMatcher } from "./rules";

// ---- shared with the layout check -----------------------------------------------------------------

/** The board outline: polygon, cutouts and every edge as a shape. */
export interface Outline {
	polygon: Pt[];
	holes: Pt[][];
	edges: Shape[];
}

/** The board's closed outline; undefined when it has none. */
export function outlineOf(project: Project): Outline | undefined {
	const o = boardOutline(project.board().tree.filter((c): c is List => isList(c) && isEdgeItem(c)));
	if (!o.polygon) return undefined;
	const edges = [o.polygon, ...o.holes].flatMap(poly => poly.map((p, i) => shape([p, poly[(i + 1) % poly.length]!])));
	return { polygon: o.polygon, holes: o.holes, edges };
}

/** Is the point on the board: inside the outline and not in a cutout (always, without an outline)? */
export function onBoard(o: Outline | undefined, p: Pt): boolean {
	return !o || (pointInPolygon(p, o.polygon) && !o.holes.some(h => pointInPolygon(p, h)));
}

/** Distance from p to a footprint's courtyard outline (courtyardEdges), 0 inside it. */
export function courtyardDistance(edges: [Pt, Pt][], p: Pt): number {
	if (insideEdges(p, edges)) return 0;
	const at = shape([p]);
	return Math.min(...edges.map(e => shapeDistance(at, shape([e[0], e[1]]))!.d));
}

/** Does a footprint's courtyard outline (courtyardEdges) overlap or touch a polygon? */
export function courtyardOverlaps(edges: [Pt, Pt][], poly: Pt[]): boolean {
	const s = shape(poly);
	return edges.some(e => (shapeDistance(shape([e[0], e[1]]), s, 0)?.d ?? Infinity) <= 0) || insideEdges(poly[0]!, edges);
}

/**
 * Each net's class settings as the `netclasses` check resolves its class: KiCad's netlist, else (a
 * net the schematic doesn't have) the first .kicad_pro pattern that matches, else Default; settings
 * a class leaves out fall back to Default's, then KiCad's (classRules).
 */
export async function netClasses(ctx: CheckContext): Promise<(net: string) => ClassRules> {
	const ns = netSettings(ctx.project);
	const nl = await ctx.analysis.netlist();
	const byNet = new Map(nl.nets.map(x => [x.name, x.netclass ?? "Default"]));
	const patterns = ns.netclass_patterns.map(p => ({ cls: p.netclass, match: patternMatcher(p.pattern) }));
	const resolved = new Map<string, ClassRules>();
	return net => {
		const name = byNet.get(net) ?? patterns.find(p => p.match(net))?.cls ?? "Default";
		let c = resolved.get(name);
		if (!c) resolved.set(name, (c = classRules(ns, name)));
		return c;
	};
}

/**
 * Names of the impedance classes (settings.layout.impedanceClasses, default every class but Default
 * whose diff-pair width or gap, after the fallbacks, differs from Default's) and an error finding
 * per configured class the .kicad_pro doesn't have.
 */
export function impedanceClasses(ctx: CheckContext, check: string): { names: Set<string>; stale: Finding[] } {
	const ns = netSettings(ctx.project);
	const classes = ns.classes.map(c => classRules(ns, c.name));
	const known = classes.map(c => c.name);
	const given = ctx.project.settings.layout.impedanceClasses;
	if (given.length) {
		const stale = given
			.filter(n => !known.includes(n))
			.map(
				(n): Finding => ({
					check,
					severity: "error",
					message: `settings.layout.impedanceClasses names net class "${n}", which the .kicad_pro doesn't have (classes: ${known.join(", ")})`,
					key: `${check}|stale|${n}`,
				}),
			);
		return { names: new Set(given.filter(n => known.includes(n))), stale };
	}
	const def = classRules(ns, "Default");
	const names = classes.filter(c => c.name !== "Default" && c.diff_pair_width > 0 && (c.diff_pair_width !== def.diff_pair_width || c.diff_pair_gap !== def.diff_pair_gap)).map(c => c.name);
	return { names: new Set(names), stale: [] };
}

/** Ground nets: leaf name matching settings.groundNets. */
export function groundNets(project: Project): (net: string) => boolean {
	const m = wildcards(project.settings.groundNets);
	return net => net !== "" && m(netLeaf(net));
}

const filled = new WeakMap<Project, Promise<Copper>>();

/**
 * The board's copper with every zone refilled by KiCad, so stale or missing fills don't decide:
 * the project's files are copied to a temp directory, the board refilled and saved there (the
 * project keeps its fills). Done once per request; a board without zones is read as it is.
 */
export function filledCopper(ctx: CheckContext): Promise<Copper> {
	let p = filled.get(ctx.project);
	if (!p) {
		p = refill(ctx);
		filled.set(ctx.project, p);
	}
	return p;
}

async function refill(ctx: CheckContext): Promise<Copper> {
	const board = ctx.project.board();
	const c = Copper.of(board);
	if (!c.zones.length) return c;
	const dir = cli.tmpDir("refill");
	try {
		for (const f of fs.readdirSync(ctx.project.dir, { withFileTypes: true })) if (f.isFile()) fs.copyFileSync(path.join(ctx.project.dir, f.name), path.join(dir, f.name));
		const file = path.join(dir, path.basename(ctx.project.boardFile));
		// the board as this request sees it
		fs.writeFileSync(file, board.serialize());
		await cli.pcbRefillAndSave(file, { cwd: dir, repoRoot: ctx.project.repoRoot, signal: ctx.signal });
		return Copper.of(Board.load(file));
	} finally {
		fs.rmSync(dir, { recursive: true, force: true });
	}
}

/** "(12.345, 6.7)" */
export const fmtPt = (p: Pt) => `(${r3(p[0])}, ${r3(p[1])})`;

/** Placed footprints by reference (their origin on the board) and the count of those not placed yet. */
export function placedRefs(project: Project, o: Outline | undefined): { placed: Map<string, List>; unplaced: number } {
	const placed = new Map<string, List>();
	let unplaced = 0;
	for (const fp of project.board().footprints()) {
		if (onBoard(o, placement(fp).at)) placed.set(fpRef(fp), fp);
		else unplaced++;
	}
	return { placed, unplaced };
}

/** Edge-to-edge distance between two sets of copper shapes, with the closest points. */
export function nearest(a: Shape[], b: Shape[]): { d: number; pa: Pt; pb: Pt } | undefined {
	let best: { d: number; pa: Pt; pb: Pt } | undefined;
	for (const x of a) {
		for (const y of b) {
			const r = shapeDistance(x, y, best?.d ?? Infinity);
			if (r && (!best || r.d < best.d)) best = r;
		}
	}
	return best;
}

// ---- intervals along a segment ----------------------------------------------------------------------

type Span = [number, number];

/** Sorted, merged spans (touching ones joined). */
function union(spans: Span[]): Span[] {
	const s = [...spans].sort((x, y) => x[0] - y[0]);
	const out: Span[] = [];
	for (const [a, b] of s) {
		const last = out[out.length - 1];
		if (last && a <= last[1] + 1e-9) last[1] = Math.max(last[1], b);
		else out.push([a, b]);
	}
	return out;
}

/** a minus b (both merged). */
function subtract(a: Span[], b: Span[]): Span[] {
	const out: Span[] = [];
	for (const [s0, s1] of a) {
		let pieces: Span[] = [[s0, s1]];
		for (const [c0, c1] of b) pieces = pieces.flatMap(([p0, p1]): Span[] => (c1 <= p0 || c0 >= p1 ? [[p0, p1]] : ([[p0, c0], [c1, p1]] as Span[]).filter(([x, y]) => y - x > 1e-9)));
		out.push(...pieces);
	}
	return out;
}

/** The parts of segment ab (fractions along it) that lie inside no fill. */
function outsideFills(a: Pt, b: Pt, fills: Fill[]): Span[] {
	const dx = b[0] - a[0];
	const dy = b[1] - a[1];
	const box: Box = [Math.min(a[0], b[0]), Math.min(a[1], b[1]), Math.max(a[0], b[0]), Math.max(a[1], b[1])];
	const near = fills.filter(f => overlap(f.shape.box, box, 1e-6));
	const ts = [0, 1];
	for (const f of near) {
		const poly = f.shape.pts;
		for (let i = 0; i < poly.length; i++) {
			const p = poly[i]!;
			const q = poly[(i + 1) % poly.length]!;
			const ex = q[0] - p[0];
			const ey = q[1] - p[1];
			const den = dx * ey - dy * ex;
			if (Math.abs(den) < 1e-12) continue;
			const wx = p[0] - a[0];
			const wy = p[1] - a[1];
			const t = (wx * ey - wy * ex) / den;
			const u = (wx * dy - wy * dx) / den;
			if (t > 0 && t < 1 && u >= 0 && u <= 1) ts.push(t);
		}
	}
	ts.sort((x, y) => x - y);
	const out: Span[] = [];
	for (let i = 1; i < ts.length; i++) {
		const [t0, t1] = [ts[i - 1]!, ts[i]!];
		if (t1 - t0 < 1e-9) continue;
		const m: Pt = [a[0] + (dx * (t0 + t1)) / 2, a[1] + (dy * (t0 + t1)) / 2];
		if (!near.some(f => pointInPolygon(m, f.shape.pts))) out.push([t0, t1]);
	}
	return union(out);
}

/** The fractions of segment ab closer than r to c. */
function withinCircle(a: Pt, b: Pt, c: Pt, r: number): Span | undefined {
	const d: Pt = [b[0] - a[0], b[1] - a[1]];
	const f: Pt = [a[0] - c[0], a[1] - c[1]];
	const A = d[0] * d[0] + d[1] * d[1];
	const B = 2 * (f[0] * d[0] + f[1] * d[1]);
	const C = f[0] * f[0] + f[1] * f[1] - r * r;
	const disc = B * B - 4 * A * C;
	if (A < 1e-18 || disc <= 0) return undefined;
	const s = Math.sqrt(disc);
	const t0 = Math.max(0, (-B - s) / (2 * A));
	const t1 = Math.min(1, (-B + s) / (2 * A));
	return t1 > t0 ? [t0, t1] : undefined;
}

// ---- track_widths -----------------------------------------------------------------------------------

/** Parallel stretches of two straight pieces: overlap length along a, centreline distance, and the overlap's middle on a. */
function parallelRun(a0: Pt, a1: Pt, b0: Pt, b1: Pt): { length: number; distance: number; mid: Pt } | undefined {
	const la = Math.hypot(a1[0] - a0[0], a1[1] - a0[1]);
	const lb = Math.hypot(b1[0] - b0[0], b1[1] - b0[1]);
	if (la < 1e-6 || lb < 1e-6) return undefined;
	const u: Pt = [(a1[0] - a0[0]) / la, (a1[1] - a0[1]) / la];
	const v: Pt = [(b1[0] - b0[0]) / lb, (b1[1] - b0[1]) / lb];
	// within 2°
	if (Math.abs(u[0] * v[1] - u[1] * v[0]) > Math.sin((2 * Math.PI) / 180)) return undefined;
	const along = (p: Pt) => (p[0] - a0[0]) * u[0] + (p[1] - a0[1]) * u[1];
	const s0 = Math.max(0, Math.min(along(b0), along(b1)));
	const s1 = Math.min(la, Math.max(along(b0), along(b1)));
	if (s1 - s0 < 1e-6) return undefined;
	const sm = (s0 + s1) / 2;
	const mid: Pt = [a0[0] + u[0] * sm, a0[1] + u[1] * sm];
	// distance from the overlap's middle to b's line
	const distance = Math.abs((mid[0] - b0[0]) * v[1] - (mid[1] - b0[1]) * v[0]);
	return { length: s1 - s0, distance, mid };
}

/** The straight pieces of a track: itself, or an arc's chords. */
const pieces = (t: Track): [Pt, Pt][] => t.shapes.map(s => [s.pts[0]!, s.pts[s.pts.length - 1]!]);

const trackWidths: CheckDef = {
	name: "track_widths",
	doc:
		"Tracks narrower than their net class's track width (warning per net, layer and width; within 1 µm counts as equal; a diff-pair net, KiCad's _P/_N or +/- naming with its partner on the board, may also run at the class's diff_pair_width), and diff pairs whose P and N tracks run coupled at a gap other than the class's diff_pair_gap (more than 10 µm off; warning per pair and layer with the stretches): coupled means straight pieces (arcs as chords) of P and N on one layer, parallel within 2°, overlapping along their length and with an edge gap under 3× the class gap. " +
		"Net classes as the netclasses check resolves them (KiCad's netlist, else the .kicad_pro patterns). No tracks: one info finding.",
	byDefault: true,
	async run(ctx) {
		const c = Copper.of(ctx.project.board());
		if (!c.tracks.some(t => t.net)) return [{ check: "track_widths", severity: "info", message: "no tracks on the board yet: nothing to check", key: "track_widths|none" }];
		const classOf = await netClasses(ctx);
		const out: Finding[] = [];
		const byNet = new Map<string, Track[]>();
		for (const t of c.tracks) if (t.net) byNet.set(t.net, [...(byNet.get(t.net) ?? []), t]);
		for (const [net, tracks] of byNet) {
			const cls = classOf(net);
			const pair = c.dpPartner(net);
			const min = pair && cls.diff_pair_width > 0 ? Math.min(cls.track_width, cls.diff_pair_width) : cls.track_width;
			const narrow = new Map<string, Track[]>();
			for (const t of tracks) {
				if (t.width >= min - 0.001) continue;
				const k = `${t.layer}|${r3(t.width)}`;
				narrow.set(k, [...(narrow.get(k) ?? []), t]);
			}
			for (const [k, ts] of narrow) {
				const [layer, w] = k.split("|");
				const len = ts.reduce((s, t) => s + t.length, 0);
				const t0 = ts[0]!;
				out.push({
					check: "track_widths",
					severity: "warning",
					message: `${net} (class ${cls.name}, track width ${r3(cls.track_width)} mm${min !== cls.track_width ? `, diff-pair width ${r3(min)} mm` : ""}): ${ts.length} track${ts.length > 1 ? "s" : ""} ${w} mm wide on ${layer}, ${r3(len)} mm in all, first from ${fmtPt(t0.start)} to ${fmtPt(t0.end)}`,
					key: `track_widths|width|${net}|${layer}|${w}`,
					at: [r3((t0.start[0] + t0.end[0]) / 2), r3((t0.start[1] + t0.end[1]) / 2)],
					net,
				});
			}
			if (!pair?.positive || !(cls.diff_pair_gap > 0)) continue;
			const partner = byNet.get(pair.partner) ?? [];
			const want = cls.diff_pair_gap;
			// per layer: mismatched coupled stretches
			const bad = new Map<string, { gap: number; length: number; mid: Pt }[]>();
			for (const p of tracks) {
				for (const n of partner) {
					if (n.layer !== p.layer) continue;
					for (const [a0, a1] of pieces(p)) {
						for (const [b0, b1] of pieces(n)) {
							const run = parallelRun(a0, a1, b0, b1);
							if (!run) continue;
							const gap = run.distance - (p.width + n.width) / 2;
							if (gap < 0 || gap >= 3 * want || Math.abs(gap - want) <= 0.01 || run.length < 0.01) continue;
							bad.set(p.layer, [...(bad.get(p.layer) ?? []), { gap, length: run.length, mid: run.mid }]);
						}
					}
				}
			}
			for (const [layer, runs] of bad) {
				const len = runs.reduce((s, r) => s + r.length, 0);
				const gaps = [...new Set(runs.map(r => r3(r.gap)))].sort((a, b) => a - b);
				const longest = runs.reduce((a, b) => (b.length > a.length ? b : a));
				out.push({
					check: "track_widths",
					severity: "warning",
					message: `diff pair ${net} / ${pair.partner} (class ${cls.name}, gap ${r3(want)} mm) on ${layer}: coupled at ${gaps.join(", ")} mm over ${r3(len)} mm (${runs.length} stretch${runs.length > 1 ? "es" : ""}, longest ${r3(longest.length)} mm at ${fmtPt(longest.mid)})`,
					key: `track_widths|gap|${net}|${layer}`,
					at: [r3(longest.mid[0]), r3(longest.mid[1])],
					net,
				});
			}
		}
		return out;
	},
};

// ---- return_path ------------------------------------------------------------------------------------

const returnPath: CheckDef = {
	name: "return_path",
	doc:
		"Tracks of the impedance classes (settings.layout.impedanceClasses; default every class but Default whose diff-pair width or gap differs from Default's) need a ground plane on the adjacent copper layer (stack order; an inner layer's tracks: either neighbour): every stretch longer than 0.5 mm where the track's copper (its centreline and both edges) is not over a fill of a ground-net zone (settings.groundNets) is a warning with its position and length. " +
		"The clearance holes around the net's own vias and through-hole pads don't count (via_return checks the return via there). Zones are refilled first on a copy of the project, so the file's fills don't decide. A class with tracks on a layer whose neighbours have no ground zone at all: one warning per class and layer. No such tracks: one info finding.",
	byDefault: true,
	async run(ctx) {
		const board = Copper.of(ctx.project.board());
		const none = (msg: string): Finding[] => [{ check: "return_path", severity: "info", message: msg, key: "return_path|none" }];
		const { names, stale } = impedanceClasses(ctx, "return_path");
		if (!board.tracks.some(t => t.net)) return [...stale, ...none("no tracks on the board yet: nothing to check")];
		if (!names.size) return [...stale, ...none("no impedance classes (settings.layout.impedanceClasses, or classes with their own diff-pair settings)")];
		const classOf = await netClasses(ctx);
		if (!board.tracks.some(t => t.net && names.has(classOf(t.net).name))) return [...stale, ...none(`no tracks of the impedance classes (${[...names].join(", ")}) yet`)];
		const c = await filledCopper(ctx);
		const ground = groundNets(ctx.project);
		const layers = c.copperLayers;
		const out: Finding[] = [...stale];
		const noPlane = new Set<string>();
		for (const t of c.tracks) {
			if (!t.net) continue;
			const cls = classOf(t.net);
			if (!names.has(cls.name)) continue;
			const i = layers.indexOf(t.layer);
			const adjacent = [layers[i - 1], layers[i + 1]].filter((l): l is string => !!l);
			const zones = c.zones.filter(z => ground(z.net) && z.layers.some(l => adjacent.includes(l)));
			if (!zones.length) {
				const k = `${cls.name}|${t.layer}`;
				if (!noPlane.has(k)) {
					noPlane.add(k);
					out.push({
						check: "return_path",
						severity: "warning",
						message: `class ${cls.name} has tracks on ${t.layer} but no ground zone on ${adjacent.join(" or ")} (the adjacent copper): no return plane under them`,
						key: `return_path|no_plane|${cls.name}|${t.layer}`,
						at: [r3(t.start[0]), r3(t.start[1])],
					});
				}
				continue;
			}
			const fills = zones.flatMap(z => z.fills.filter(f => adjacent.includes(f.layer)));
			const clearance = Math.max(cls.clearance, ...zones.map(z => z.settings.clearance));
			const hw = t.width / 2;
			// the net's own vias and through-hole pads: their clearance holes in the plane
			const holes: { at: Pt; r: number }[] = [
				...c.vias.filter(v => v.net === t.net && v.span.some(l => adjacent.includes(l))).map(v => ({ at: v.at, r: v.size / 2 + clearance + hw })),
				...c.pads
					.filter((p: Pad) => p.net === t.net && p.info.type === "thru_hole")
					.map(p => ({ at: p.at, r: Math.max(...(p.info.size ?? [0, 0])) / 2 + clearance + hw })),
			];
			const spans: Span[] = [];
			const segs: { a: Pt; b: Pt; from: number; len: number }[] = [];
			let offset = 0;
			for (const [a, b] of pieces(t)) {
				const len = Math.hypot(b[0] - a[0], b[1] - a[1]);
				if (len < 1e-9) continue;
				const n: Pt = [(-(b[1] - a[1]) / len) * hw, ((b[0] - a[0]) / len) * hw];
				const lines: [Pt, Pt][] = [
					[a, b],
					[
						[a[0] + n[0], a[1] + n[1]],
						[b[0] + n[0], b[1] + n[1]],
					],
					[
						[a[0] - n[0], a[1] - n[1]],
						[b[0] - n[0], b[1] - n[1]],
					],
				];
				const bare = union(lines.flatMap(([p, q]) => outsideFills(p, q, fills)));
				const excused = union(holes.map(h => withinCircle(a, b, h.at, h.r)).filter((s): s is Span => !!s));
				for (const [t0, t1] of subtract(bare, excused)) spans.push([offset + t0 * len, offset + t1 * len]);
				segs.push({ a, b, from: offset, len });
				offset += len;
			}
			const at = (s: number): Pt => {
				const g = segs.find(x => s <= x.from + x.len + 1e-9) ?? segs[segs.length - 1]!;
				const f = Math.max(0, Math.min(1, (s - g.from) / g.len));
				return [r3(g.a[0] + (g.b[0] - g.a[0]) * f), r3(g.a[1] + (g.b[1] - g.a[1]) * f)];
			};
			union(spans)
				.filter(([s0, s1]) => s1 - s0 > 0.5)
				.forEach(([s0, s1], k) => {
					out.push({
						check: "return_path",
						severity: "warning",
						message: `${t.net} (class ${cls.name}) on ${t.layer}: ${r3(s1 - s0)} mm of track not over a ground zone fill on ${adjacent.join(" or ")}, from ${fmtPt(at(s0))} to ${fmtPt(at(s1))}`,
						key: `return_path|${t.net}|${t.layer}|${t.uuid.slice(0, 8)}|${k}`,
						at: at((s0 + s1) / 2),
						net: t.net,
					});
				});
		}
		return out;
	},
};

// ---- via_return -------------------------------------------------------------------------------------

const viaReturn: CheckDef = {
	name: "via_return",
	doc: "Every via of an impedance-class net (see return_path) that joins tracks on two different copper layers needs a return within settings.layout.viaReturnMaxDistance (default 1 mm, centre to centre): a ground-net via (settings.groundNets) whose layer span overlaps the signal via's, or a through-hole ground pad; a warning per net lists the vias without one. No such vias: one info finding.",
	byDefault: true,
	async run(ctx) {
		const c = Copper.of(ctx.project.board());
		const none = (msg: string): Finding[] => [{ check: "via_return", severity: "info", message: msg, key: "via_return|none" }];
		const { names, stale } = impedanceClasses(ctx, "via_return");
		if (!c.vias.some(v => v.net)) return [...stale, ...none("no vias on the board yet: nothing to check")];
		const classOf = await netClasses(ctx);
		const max = ctx.project.settings.layout.viaReturnMaxDistance;
		const ground = groundNets(ctx.project);
		const returns = [
			...c.vias.filter(v => ground(v.net)).map(v => ({ at: v.at, span: v.span })),
			...c.pads.filter(p => ground(p.net) && p.info.type === "thru_hole").map(p => ({ at: p.at, span: p.copper })),
		];
		const out: Finding[] = [...stale];
		let checked = 0;
		for (const net of c.nets()) {
			if (ground(net) || !names.has(classOf(net).name)) continue;
			const conn = c.connectivity(net);
			const lacking: Pt[] = [];
			conn.nodes.forEach((n, i) => {
				if (n.item.kind !== "via") return;
				const layers = new Set<string>();
				for (const j of conn.junctions) {
					if (j.a !== i && j.b !== i) continue;
					const o = conn.nodes[j.a === i ? j.b : j.a]!.item;
					if (o.kind === "track" || o.kind === "arc") layers.add(j.layer);
				}
				if (layers.size < 2) return;
				checked++;
				const { at, span } = n.item;
				if (!returns.some(g => g.span.some(l => span.includes(l)) && Math.hypot(g.at[0] - at[0], g.at[1] - at[1]) <= max + 1e-9)) lacking.push(at);
			});
			if (!lacking.length) continue;
			const cls = classOf(net).name;
			out.push({
				check: "via_return",
				severity: "warning",
				message: `${net} (class ${cls}): ${lacking.length} via${lacking.length > 1 ? "s" : ""} changing layers with no ground via or through-hole ground pad within ${max} mm: ${lacking.map(fmtPt).join(", ")}`,
				key: `via_return|${net}`,
				at: [r3(lacking[0]![0]), r3(lacking[0]![1])],
				net,
			});
		}
		if (!checked && !stale.length) return none(names.size ? `no layer-changing vias on nets of the impedance classes (${[...names].join(", ")}) yet` : "no impedance classes");
		return out;
	},
};

// ---- decoupling_placement ---------------------------------------------------------------------------

const decouplingPlacement: CheckDef = {
	name: "decoupling_placement",
	doc:
		"Placement of the decoupling capacitors the decoupling check pairs with each power pin (fitted capacitors from the pin's net to ground on the part's sheet): the nearest one's pad on that net, edge to edge to the power pin's pad, above settings.layout.decouplingMaxDistance (default 3 mm) is a warning per pin (one near capacitor satisfies the pin; the others may sit anywhere). " +
		"The nearest capacitor's ground pad also needs a ground via, or a ground zone fill on its layer, within 1 mm (edge to edge; zones refilled first): a warning per capacitor otherwise, skipped while the board has no ground vias or zones. Pins exempt in settings.decouplingExempt are skipped; parts or capacitors not placed on the board yet (origin outside the outline) are counted in one info finding. " +
		"Board and schematic out of step is a warning: a part missing from the board, a power pin without its pad, or capacitors none of which has a pad on the pin's net.",
	byDefault: true,
	async run(ctx) {
		const o = outlineOf(ctx.project);
		const info = (message: string, key: string): Finding => ({ check: "decoupling_placement", severity: "info", message, key: `decoupling_placement|${key}` });
		if (!o) return [info("the board has no outline yet: placement not checked", "none")];
		const { placed } = placedRefs(ctx.project, o);
		if (!placed.size) return [info("no footprints placed on the board yet: nothing to check", "none")];
		const nl = await ctx.analysis.netlist();
		const ground = groundNets(ctx.project);
		const { decouplingMaxDistance: max } = ctx.project.settings.layout;
		const exempt = ctx.project.settings.decouplingExempt;
		const c = await filledCopper(ctx);
		const padsOf = (ref: string, pred: (p: Pad) => boolean) => c.pads.filter(p => p.ref === ref && p.copper.length && pred(p));
		const onBoardRefs = new Set(ctx.project.board().footprints().map(fpRef));
		const out: Finding[] = [];
		const drift = (key: string, message: string, refs: string[], net: string) =>
			out.push({ check: "decoupling_placement", severity: "warning", message: `${message} (board and schematic out of step: update the board from the schematic)`, key: `decoupling_placement|drift|${key}`, refs, net });
		const missing = new Set<string>();
		let unplaced = 0;
		let checked = 0;
		const nearestCaps = new Map<string, string>();
		for (const g of decouplingPairs(nl, ground)) {
			if (!g.caps.length) continue;
			for (const pin of g.pins) {
				if (exempt[`${g.ref}.${pin.pin}`] !== undefined || exempt[g.ref] !== undefined) continue;
				for (const r of [g.ref, ...g.caps]) {
					if (onBoardRefs.has(r) || missing.has(r)) continue;
					missing.add(r);
					drift(r, `${r} is in the schematic but not on the board`, [r], g.net);
				}
				if (!onBoardRefs.has(g.ref)) continue;
				const caps = g.caps.filter(r => placed.has(r));
				if (!placed.has(g.ref) || !caps.length) {
					unplaced++;
					continue;
				}
				const pinPads = padsOf(g.ref, p => p.number === pin.pin);
				if (!pinPads.length) {
					drift(`${g.ref}.${pin.pin}`, `${g.ref}.${pin.pin} (power pin on ${g.net}) has no pad on the board`, [g.ref], g.net);
					continue;
				}
				if (!caps.some(cap => padsOf(cap, p => p.net === g.net).length)) {
					drift(`${g.ref}.${pin.pin}|${g.net}`, `${g.ref}.${pin.pin} on ${g.net}: none of its capacitors on the board (${caps.join(", ")}) has a pad on ${g.net}`, [g.ref, ...caps], g.net);
					continue;
				}
				checked++;
				let best: { ref: string; d: number } | undefined;
				for (const cap of caps) {
					const r = nearest(
						pinPads.flatMap(p => p.shapes),
						padsOf(cap, p => p.net === g.net).flatMap(p => p.shapes),
					);
					if (r && (!best || r.d < best.d)) best = { ref: cap, d: r.d };
				}
				if (!best) continue;
				nearestCaps.set(best.ref, g.net);
				if (best.d <= max + 1e-9) continue;
				const fn = pin.pinfunction ? ` (${pin.pinfunction})` : "";
				out.push({
					check: "decoupling_placement",
					severity: "warning",
					message: `${g.ref}.${pin.pin}${fn} on ${g.net}: the nearest decoupling capacitor, ${best.ref}, is ${r3(best.d)} mm away (pad edge to pad edge; max ${max} mm; capacitors on this net and sheet: ${caps.join(", ")})`,
					key: `decoupling_placement|${g.ref}.${pin.pin}|${g.net}`,
					at: [r3(pinPads[0]!.at[0]), r3(pinPads[0]!.at[1])],
					refs: [g.ref, best.ref],
					net: g.net,
				});
			}
		}
		const gVias = c.vias.filter(v => ground(v.net));
		const gFills = c.zones.filter(z => ground(z.net)).flatMap(z => z.fills);
		if (nearestCaps.size && !gVias.length && !gFills.length) out.push(info("no ground vias or zone fills on the board yet: the capacitors' ground connections are not checked", "no_ground"));
		else {
			for (const [cap, net] of nearestCaps) {
				for (const pad of padsOf(cap, p => ground(p.net))) {
					const via = gVias.some(v => v.span.some(l => pad.copper.includes(l)) && pad.shapes.some(s => (shapeDistance(s, v.shapes[0]!, 1)?.d ?? Infinity) <= 1 + 1e-9));
					const fill = gFills.some(f => pad.copper.includes(f.layer) && pad.shapes.some(s => (shapeDistance(s, f.shape, 1)?.d ?? Infinity) <= 1 + 1e-9));
					if (via || fill) continue;
					out.push({
						check: "decoupling_placement",
						severity: "warning",
						message: `${cap} (decoupling ${net}): its ground pad ${pad.id} (${pad.net}) has no ground via or ground zone fill within 1 mm`,
						key: `decoupling_placement|ground|${cap}`,
						at: [r3(pad.at[0]), r3(pad.at[1])],
						refs: [cap],
						net: pad.net,
					});
				}
			}
		}
		if (unplaced) out.push(info(`${unplaced} power pin${unplaced > 1 ? "s" : ""} not checked: the part or all its decoupling capacitors are not placed on the board yet`, "unplaced"));
		if (!checked && !unplaced) out.push(info("no power pins with decoupling capacitors in the netlist: nothing to check", "none"));
		return out;
	},
};

// ---- placement --------------------------------------------------------------------------------------

const placementCheck: CheckDef = {
	name: "placement",
	doc:
		"Footprint placement against the board: a placed footprint (origin on the board) with a pad outside the outline, in a cutout or crossing an edge (warning per footprint); a courtyard (its outline, rotation applied; pads and body box without one) overlapping a rule area that keeps footprints out on the footprint's side (error; a footprint's own rule areas, such as a tooling hole's keep-out ring, don't count); one info finding with the counts (placed, on the bottom side, locked) and one with the footprints not placed yet (origin off the board). " +
		"Courtyard overlaps between footprints are DRC's (drc). No footprints: one info finding.",
	byDefault: true,
	async run(ctx) {
		const board = ctx.project.board();
		const fps = board.footprints();
		const info = (message: string, key: string): Finding => ({ check: "placement", severity: "info", message, key: `placement|${key}` });
		if (!fps.length) return [info("no footprints on the board: nothing to check", "none")];
		const o = outlineOf(ctx.project);
		if (!o) return [info(`the board has no outline yet: placement of its ${fps.length} footprints not checked`, "none")];
		const c = Copper.of(board);
		const first = c.copperLayers[0]!;
		const last = c.copperLayers[c.copperLayers.length - 1]!;
		const areas = c.ruleAreas.filter(a => a.keepout.footprints && a.outline.length >= 3);
		const out: Finding[] = [];
		const unplaced: string[] = [];
		let bottom = 0;
		let locked = 0;
		let placedCount = 0;
		for (const fp of fps) {
			const ref = fpRef(fp);
			const pl = placement(fp);
			if (!onBoard(o, pl.at)) {
				unplaced.push(ref);
				continue;
			}
			placedCount++;
			if (pl.side === "bottom") bottom++;
			if (pl.locked) locked++;
			const off = c.pads
				.filter(p => p.ref === ref && p.copper.length)
				.filter(p => p.shapes.some(s => !onBoard(o, s.pts[0]!) || o.edges.some(e => (shapeDistance(s, e, 0)?.d ?? Infinity) <= 0)));
			if (off.length) {
				out.push({
					check: "placement",
					severity: "warning",
					message: `${ref}: pad${off.length > 1 ? "s" : ""} ${[...new Set(off.map(p => p.number))].join(", ")} outside the board outline or crossing its edge`,
					key: `placement|outside|${ref}`,
					at: [r3(pl.at[0]), r3(pl.at[1])],
					refs: [ref],
				});
			}
			const court = courtyardEdges(fp);
			if (!court.length) continue;
			const sideLayer = pl.side === "top" ? first : last;
			for (const a of areas) {
				// a footprint's own rule area (a tooling hole's keep-out ring, an antenna keepout) doesn't keep it out
				if (a.footprint === ref || !a.layers.includes(sideLayer) || !courtyardOverlaps(court, a.outline)) continue;
				const name = a.name ?? a.uuid.slice(0, 8);
				out.push({
					check: "placement",
					severity: "error",
					message: `${ref}: its courtyard overlaps rule area ${name}, which keeps footprints out on ${sideLayer}`,
					key: `placement|rule_area|${ref}|${name}`,
					at: [r3(pl.at[0]), r3(pl.at[1])],
					refs: [ref],
				});
			}
		}
		out.push(info(`${placedCount} of ${fps.length} footprints placed on the board: ${placedCount - bottom} top, ${bottom} bottom; ${locked} locked`, "summary"));
		if (unplaced.length) {
			const sorted = unplaced.sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
			out.push(info(`${sorted.length} footprints not placed yet (origin off the board): ${sorted.slice(0, 30).join(", ")}${sorted.length > 30 ? ", …" : ""}`, "unplaced"));
		}
		return out;
	},
};

export const checks: CheckDef[] = [trackWidths, returnPath, viaReturn, decouplingPlacement, placementCheck];
