/**
 * Board copper ops: tracks, differential pairs, vias, stitching vias, removing copper and track
 * widths. Board millimetres, x right, y down; KiCad canonical layer names.
 *
 * Items are written as KiCad 10 stores them (copper.ts): `(segment (start) (end) (width) (layer)
 * (net "name") (uuid))`, `(via [blind|micro] (at) (size) (drill) (layers from to) (net) (uuid))`.
 * Widths and via sizes default to the net's class (.kicad_pro net classes, the class KiCad's
 * netlist gives the net; a setting the class leaves out is Default's).
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { type ClassRules, classRules, netSettings } from "../checks/rules";
import * as cli from "../cli";
import { Copper, type CopperItem, type Pad, type Shape, shape, shapeDistance, shapeHas, type Track, type Via } from "../copper";
import { boardOutline, isEdgeItem, pointInPolygon, shapeIn, shapePolygon } from "../edges";
import { mm } from "../fpgfx";
import { type Box, bbox, type Pt } from "../geom";
import type { Project } from "../project";
import { newUuid } from "../sch";
import { child, isList, L, type List, setChild, str, sym, text } from "../sexpr";
import type { OpDef } from "../types";
import { boardSettings } from "./board";

// ---- shared helpers -------------------------------------------------------------------------------

export const fmtMm = (v: number) => `${Math.round(v * 1e4) / 1e4 + 0}`;
export const fmtPt = (p: Pt) => `(${fmtMm(p[0])}, ${fmtMm(p[1])})`;
export const short = (uuid: string) => uuid.slice(0, 8);

const dist = (a: Pt, b: Pt) => Math.hypot(a[0] - b[0], a[1] - b[1]);

/** The box around these points grown by `m`. */
export function boxAround(pts: readonly Pt[], m = 0): Box {
	const b = bbox(pts);
	return [b[0] - m, b[1] - m, b[2] + m, b[3] + m];
}

/** The box around several boxes. */
export function boxOfBoxes(boxes: readonly Box[]): Box | undefined {
	return boxes.length ? bbox(boxes.flatMap((b): Pt[] => [[b[0], b[1]], [b[2], b[3]]])) : undefined;
}

export const xy = (k: string, p: Pt): List => L(k, mm(p[0]), mm(p[1]));

/** A copper layer of the board by its canonical name. */
export function copperLayer(c: Copper, v: unknown, what = "layer"): string {
	const l = String(v);
	if (!c.copperLayers.includes(l)) throw new Error(`${what}: "${l}" is not a copper layer of the board (${c.copperLayers.join(", ")})`);
	return l;
}

const netlists = new WeakMap<Project, Promise<cli.Netlist>>();

/** KiCad's netlist of the project's schematic as it is on disk (once per project). */
function netlistOf(project: Project): Promise<cli.Netlist> {
	let nl = netlists.get(project);
	if (!nl) {
		nl = cli.netlist(project.rootFile, { cwd: project.dir, repoRoot: project.repoRoot });
		netlists.set(project, nl);
	}
	return nl;
}

/** The class KiCad's netlist gives a net ("Default" for nets it doesn't list, e.g. board-only ones), with its settings. */
async function netClass(project: Project, net: string): Promise<ClassRules> {
	const nl = await netlistOf(project);
	return classRules(netSettings(project), nl.nets.find(n => n.name === net)?.netclass ?? "Default");
}

/** Nets of a net class in KiCad's netlist that have copper on the board. */
async function classNets(project: Project, c: Copper, cls: string): Promise<string[]> {
	const nl = await netlistOf(project);
	const of = (n: cli.Net) => (n.netclass ?? "Default").split(",").map(s => s.trim().toLowerCase());
	const classes = new Set(nl.nets.flatMap(n => (n.netclass ?? "Default").split(",").map(s => s.trim())));
	if (![...classes].some(k => k.toLowerCase() === cls.toLowerCase())) throw new Error(`no net class "${cls}" (classes in the netlist: ${[...classes].sort().join(", ")})`);
	return nl.nets.filter(n => of(n).includes(cls.toLowerCase()) && c.hasNet(n.name)).map(n => n.name);
}

/** A board rule from the .kicad_pro (KiCad's own numbers), or `dflt`. */
function boardRule(project: Project, key: string, dflt = 0): number {
	const v = boardSettings(project)?.rules[key];
	return typeof v === "number" ? v : dflt;
}

const netOfItem = (c: Copper, it: CopperItem): string => (it.kind === "fill" ? (c.zones.find(z => z.uuid === it.zone)?.net ?? "") : it.net);

function describeItem(c: Copper, it: CopperItem): string {
	if (it.kind === "pad") return `pad ${it.id}`;
	if (it.kind === "fill") {
		const z = c.zones.find(x => x.uuid === it.zone);
		return `zone ${z?.name ?? short(it.zone)}`;
	}
	return `${it.kind} ${short(it.uuid)}`;
}

/** A pad by "REF.NUMBER" on a copper layer (the one of a repeated number nearest to `near`). */
function padOn(c: Copper, id: string, layer: string, near?: Pt): Pad {
	const pads = c.pad(id);
	const on = pads.filter(p => p.copper.includes(layer));
	if (!on.length) throw new Error(`${id} is on ${[...new Set(pads.flatMap(p => p.copper))].join(", ") || "no copper layer"}, not ${layer}: route it on its own layer or through a via`);
	if (near) on.sort((a, b) => dist(a.at, near) - dist(b.at, near));
	return on[0]!;
}

/**
 * What a track end at p on `layer` lands on for `net`: same-net copper (pad, track, via, zone fill or
 * an unfilled zone of the net), or undefined for a free end. Other-net copper there is an error
 * (other nets' zone fills don't count: KiCad refills them around the track).
 */
function landing(c: Copper, p: Pt, layer: string, net: string, where: string): string | undefined {
	const other = c.itemsAt(p, layer).filter(i => i.kind !== "fill" && i.net !== net);
	if (other.length) throw new Error(`${where} ${fmtPt(p)} lands on ${describeItem(c, other[0]!)} of ${other[0]!.kind === "fill" ? "" : other[0]!.net || "no net"} on ${layer}, not on ${net}`);
	const same = c.itemsAt(p, layer, net)[0];
	if (same) return describeItem(c, same);
	const zone = c.zones.find(z => z.net === net && z.layers.includes(layer) && z.outline.length >= 3 && pointInPolygon(p, z.outline) && !z.holes.some(h => pointInPolygon(p, h)));
	return zone ? `zone ${zone.name ?? short(zone.uuid)} (unfilled)` : undefined;
}

/** The nets of the copper at a point on any of `layers`: tracks, pads and vias first; zone fills only when nothing else is there. */
function netsAt(c: Copper, p: Pt, layers: string[]): string[] {
	const items = layers.flatMap(l => c.itemsAt(p, l));
	const solid = items.filter(i => i.kind !== "fill");
	return [...new Set((solid.length ? solid : items).map(i => netOfItem(c, i)))];
}

function segmentNode(a: Pt, b: Pt, width: number, layer: string, net: string): List {
	return L("segment", xy("start", a), xy("end", b), L("width", mm(width)), L("layer", str(layer)), L("net", str(net)), L("uuid", str(newUuid())));
}

function viaNode(at: Pt, size: number, drill: number, type: "through" | "blind" | "micro", layers: [string, string], net: string): List {
	const node = L("via", xy("at", at), L("size", mm(size)), L("drill", mm(drill)), L("layers", str(layers[0]), str(layers[1])), L("net", str(net)), L("uuid", str(newUuid())));
	if (type !== "through") node.splice(1, 0, sym(type));
	return node;
}

/** Consecutive points as segments; a zero-length one is an error. */
function segmentsOf(points: Pt[], what: string): [Pt, Pt][] {
	const out: [Pt, Pt][] = [];
	for (let i = 1; i < points.length; i++) {
		const [a, b] = [points[i - 1]!, points[i]!];
		if (dist(a, b) < 1e-6) throw new Error(`${what}: points ${i - 1} and ${i} are the same ${fmtPt(a)}`);
		out.push([a, b]);
	}
	return out;
}

const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? "" : "s"}`;

// ---- add_track ------------------------------------------------------------------------------------

/** A track end argument: a pad "U6.75" or a point. */
function endArg(c: Copper, v: unknown, layer: string, where: string, near?: Pt): { at: Pt; pad?: Pad } {
	if (typeof v === "string") {
		const pad = padOn(c, v, layer, near);
		return { at: pad.at, pad };
	}
	if (Array.isArray(v) && v.length === 2 && v.every(x => typeof x === "number" && Number.isFinite(x))) return { at: [v[0], v[1]] };
	throw new Error(`${where}: give a pad "U6.75" or a point [x, y], got ${JSON.stringify(v)}`);
}

const addTrack: OpDef = {
	op: "add_track",
	group: "board",
	doc:
		"Route a track on one copper layer: a polyline of straight segments through `points`, optionally starting at `from` and ending at `to` (a pad \"U6.75\": the track starts at its position; a pad not on `layer` is an error unless through-hole). " +
		"The net is the end pads' (two different nets: error), else `net`, else the copper at the first or last point; a `net` that differs from the pads' is an error. Each end lands on copper of the net on that layer (pad, track, via, zone) or stays free (reported as track_dangling, not gated); at least one end must land and an end on another net's copper is an error. " +
		"Width: the net class's track width unless given. Crossings and clearances are checked by KiCad's DRC in the transaction.",
	fields: {
		layer: { t: "string", req: true, doc: 'copper layer, e.g. "F.Cu"' },
		points: { t: "any", doc: "polyline points [[x, y], …], board mm (between from and to when given; one point is enough with them)" },
		from: { t: "any", doc: 'start: pad "REF.NUMBER" or point [x, y]' },
		to: { t: "any", doc: 'end: pad "REF.NUMBER" or point [x, y]' },
		net: { t: "string", doc: "net name (default: from the end pads or the copper at the ends)" },
		width: { t: "number", doc: "track width, mm (default: the net class's track_width)" },
	},
	async apply(ctx, args) {
		const board = ctx.project.board();
		const c = Copper.of(board);
		const layer = copperLayer(c, args.layer);
		const mid = (args.points as Pt[] | undefined) ?? [];
		if (!Array.isArray(mid) || !mid.every(p => Array.isArray(p) && p.length === 2 && p.every(v => typeof v === "number" && Number.isFinite(v)))) {
			throw new Error(`points: give a list of [x, y] points in mm, got ${JSON.stringify(args.points)}`);
		}
		const from = args.from !== undefined ? endArg(c, args.from, layer, "from", mid[0] ?? (typeof args.to !== "string" ? (args.to as Pt | undefined) : undefined)) : undefined;
		const to = args.to !== undefined ? endArg(c, args.to, layer, "to", mid[mid.length - 1] ?? from?.at) : undefined;
		const points: Pt[] = [...(from ? [from.at] : []), ...mid, ...(to ? [to.at] : [])];
		if (points.length < 2) throw new Error("give at least two points in all (points, from, to)");
		const segs = segmentsOf(points, "points");
		const padNets = [from?.pad, to?.pad].filter((p): p is Pad => !!p);
		for (const p of padNets) if (!p.net) throw new Error(`${p.id} is on no net`);
		if (padNets.length === 2 && padNets[0]!.net !== padNets[1]!.net) throw new Error(`${padNets[0]!.id} is on ${padNets[0]!.net} but ${padNets[1]!.id} is on ${padNets[1]!.net}`);
		let net = padNets[0]?.net;
		if (args.net !== undefined) {
			const given = c.net(String(args.net));
			if (net && given !== net) throw new Error(`net ${given} differs from ${padNets[0]!.id}'s net ${net}`);
			net = given;
		}
		if (!net) {
			const ends = [points[0]!, points[points.length - 1]!].map(p => netsAt(c, p, [layer]).filter(n => n));
			const found = [...new Set(ends.flat())];
			if (found.length > 1) throw new Error(`the ends lie on copper of several nets (${found.join(", ")}): give net`);
			if (!found.length) throw new Error(`no copper at either end on ${layer}: give net, or start or end on a pad, track or via`);
			net = found[0]!;
		}
		const ends = [
			{ p: points[0]!, what: "start" },
			{ p: points[points.length - 1]!, what: "end" },
		].map(e => ({ ...e, on: landing(c, e.p, layer, net!, e.what) }));
		if (!ends.some(e => e.on)) throw new Error(`neither end lands on ${net} copper on ${layer}: start or end on a pad, track, via or zone of the net`);
		const width = (args.width as number | undefined) ?? (await netClass(ctx.project, net)).track_width;
		if (!(width > 0)) throw new Error("width must be positive");
		const nodes = segs.map(([a, b]) => segmentNode(a, b, width, layer, net!));
		board.add(...nodes);
		ctx.project.touchBoard();
		ctx.touchBoard(boxAround(points, width / 2 + 1), undefined, { nets: [net], layers: [layer] });
		const length = segs.reduce((t, [a, b]) => t + dist(a, b), 0);
		const free = ends.filter(e => !e.on);
		ctx.notes.push(
			`track ${net} on ${layer}, ${fmtMm(width)} mm: ${plural(segs.length, "segment")}, ${fmtMm(length)} mm, ${ends.map(e => `${e.what} ${fmtPt(e.p)} ${e.on ? `on ${e.on}` : "free"}`).join(", ")}${free.length ? " (a free end shows as track_dangling until it is routed on)" : ""}; uuids ${nodes.map(n => short(text(child(n, "uuid")) ?? "")).join(", ")}`,
		);
	},
};

// ---- add_diff_pair ---------------------------------------------------------------------------------

/**
 * A polyline offset by d to its left as seen on screen (x right, y down; negative d: right), with
 * mitred joints: each joint where the two offset segments' lines meet. A turn sharper than 120°
 * (a mitre over twice the offset) is an error.
 */
export function offsetPolyline(points: Pt[], d: number): Pt[] {
	const segs = segmentsOf(points, "points");
	const normals = segs.map(([a, b]): Pt => {
		const l = dist(a, b);
		const u: Pt = [(b[0] - a[0]) / l, (b[1] - a[1]) / l];
		return [u[1], -u[0]];
	});
	const out: Pt[] = [[points[0]![0] + normals[0]![0] * d, points[0]![1] + normals[0]![1] * d]];
	for (let k = 1; k < points.length - 1; k++) {
		const [n0, n1] = [normals[k - 1]!, normals[k]!];
		const m: Pt = [n0[0] + n1[0], n0[1] + n1[1]];
		const ml = Math.hypot(m[0], m[1]);
		const cosHalf = ml / 2;
		if (cosHalf < 0.5 - 1e-9) throw new Error(`points[${k}] ${fmtPt(points[k]!)}: the turn is sharper than 120°; a pair can't follow it (split it into two turns)`);
		const s = d / cosHalf / ml;
		out.push([points[k]![0] + m[0] * s, points[k]![1] + m[1] * s]);
	}
	const last = points[points.length - 1]!;
	const nl = normals[normals.length - 1]!;
	out.push([last[0] + nl[0] * d, last[1] + nl[1] * d]);
	return out;
}

/**
 * The two tracks of a pair along a centreline, `pitch` = width + gap apart (centre to centre), and
 * which side P takes: +1 left, -1 right of the direction of travel. With pads at the start P goes
 * to the side nearer its pad; else by the pads at the end; else left. Pads at both ends in the
 * other order would need the tracks to cross: an error.
 */
export function pairPaths(centre: Pt[], pitch: number, from?: [Pt, Pt], to?: [Pt, Pt]): { p: Pt[]; n: Pt[]; side: 1 | -1 } {
	const left = offsetPolyline(centre, pitch / 2);
	const right = offsetPolyline(centre, -pitch / 2);
	const order = (l: Pt, r: Pt, pads: [Pt, Pt]): 1 | -1 => (dist(l, pads[0]) + dist(r, pads[1]) <= dist(r, pads[0]) + dist(l, pads[1]) ? 1 : -1);
	const end = left.length - 1;
	const side = from ? order(left[0]!, right[0]!, from) : to ? order(left[end]!, right[end]!, to) : 1;
	if (from && to && order(left[end]!, right[end]!, to) !== side) {
		throw new Error("the P and N pads are in the opposite order at the end than at the start, so the tracks would have to cross: cross them at vias (two add_diff_pair calls on different layers joined by add_via), or route the centreline around the other way");
	}
	return side === 1 ? { p: left, n: right, side } : { p: right, n: left, side };
}

const addDiffPair: OpDef = {
	op: "add_diff_pair",
	group: "board",
	doc:
		"Route a differential pair on one copper layer along a centreline: two tracks (width + gap) apart centre to centre, offset to either side with mitred joints, plus a fan-in segment from each `from` pad to its track's start and a fan-out from each track's end to its `to` pad. " +
		"p and n must be KiCad diff-pair partners (\"USB_D+\"/\"USB_D-\", \"LVDS_P0\"/\"LVDS_N0\"), p the positive one. P takes the side nearer its pad at the start (else at the end; with no pads the left side, looking along the centreline). " +
		"Pads in the other order at the end are an error: the tracks would cross. Width and gap: the class's diff_pair_width and diff_pair_gap unless given.",
	fields: {
		p: { t: "string", req: true, doc: "positive net" },
		n: { t: "string", req: true, doc: "negative net" },
		layer: { t: "string", req: true, doc: 'copper layer, e.g. "F.Cu"' },
		points: { t: "points", req: true, doc: "centreline [[x, y], …], board mm" },
		from: { t: "strings", doc: '[padP, padN] at the start, e.g. ["U1.12", "U1.13"]' },
		to: { t: "strings", doc: "[padP, padN] at the end" },
		width: { t: "number", doc: "track width, mm (default: the class's diff_pair_width)" },
		gap: { t: "number", doc: "edge-to-edge gap between the tracks, mm (default: the class's diff_pair_gap)" },
	},
	async apply(ctx, args) {
		const board = ctx.project.board();
		const c = Copper.of(board);
		const layer = copperLayer(c, args.layer);
		const p = c.net(String(args.p));
		const n = c.net(String(args.n));
		const dp = c.dpPartner(p);
		if (!dp || dp.partner !== n) throw new Error(`${p} and ${n} are not KiCad diff-pair partners (names ending in +/- or P/N, e.g. USB_D+ and USB_D-)`);
		if (!dp.positive) throw new Error(`${p} is the negative net of the pair: swap p and n`);
		const centre = args.points as Pt[];
		const pads = (v: unknown, where: string, near: Pt): [Pad, Pad] | undefined => {
			if (v === undefined) return undefined;
			const ids = v as string[];
			if (ids.length !== 2) throw new Error(`${where}: give [padP, padN]`);
			const [a, b] = [padOn(c, ids[0]!, layer, near), padOn(c, ids[1]!, layer, near)];
			if (a.net !== p) throw new Error(`${where}[0] ${a.id} is on ${a.net || "no net"}, not ${p}`);
			if (b.net !== n) throw new Error(`${where}[1] ${b.id} is on ${b.net || "no net"}, not ${n}`);
			return [a, b];
		};
		const from = pads(args.from, "from", centre[0]!);
		const to = pads(args.to, "to", centre[centre.length - 1]!);
		const cls = await netClass(ctx.project, p);
		const width = (args.width as number | undefined) ?? cls.diff_pair_width;
		const gap = (args.gap as number | undefined) ?? cls.diff_pair_gap;
		if (!(width > 0) || !(gap > 0)) throw new Error("width and gap must be positive");
		const path = pairPaths(centre, width + gap, from && [from[0].at, from[1].at], to && [to[0].at, to[1].at]);
		const build = (net: string, track: Pt[], start?: Pad, end?: Pad): Pt[] => {
			const pts = [...(start && dist(start.at, track[0]!) > 1e-6 ? [start.at] : []), ...track, ...(end && dist(end.at, track[track.length - 1]!) > 1e-6 ? [end.at] : [])];
			for (const [k, q] of [
				[0, pts[0]!],
				[1, pts[pts.length - 1]!],
			] as const) {
				const pad = k === 0 ? start : end;
				if (!pad) landing(c, q, layer, net, k === 0 ? `${net} start` : `${net} end`);
			}
			return pts;
		};
		const pp = build(p, path.p, from?.[0], to?.[0]);
		const nn = build(n, path.n, from?.[1], to?.[1]);
		const nodes = [...segmentsOf(pp, p).map(([a, b]) => segmentNode(a, b, width, layer, p)), ...segmentsOf(nn, n).map(([a, b]) => segmentNode(a, b, width, layer, n))];
		board.add(...nodes);
		ctx.project.touchBoard();
		ctx.touchBoard(boxAround([...pp, ...nn], width / 2 + 1), undefined, { nets: [p, n], layers: [layer] });
		const len = (pts: Pt[]) => pts.slice(1).reduce((t, q, i) => t + dist(pts[i]!, q), 0);
		ctx.notes.push(
			`diff pair ${p} / ${n} on ${layer}: width ${fmtMm(width)} mm, gap ${fmtMm(gap)} mm (${fmtMm(width + gap)} mm centre to centre), ${p} on the ${path.side === 1 ? "left" : "right"} of the centreline; ${p} ${fmtMm(len(pp))} mm, ${n} ${fmtMm(len(nn))} mm (skew ${fmtMm(len(pp) - len(nn))} mm); ${plural(nodes.length, "segment")}`,
		);
	},
};

// ---- vias -----------------------------------------------------------------------------------------

const VIA_TYPES = ["through", "blind", "micro"] as const;

/** The via's two end layers, checked for its type. */
function viaLayers(c: Copper, type: (typeof VIA_TYPES)[number], v: unknown): [string, string] {
	const cu = c.copperLayers;
	const outer = [cu[0]!, cu[cu.length - 1]!];
	if (type === "through") {
		if (v !== undefined) throw new Error("layers: a through via spans every copper layer; give layers only for blind and micro vias");
		return [outer[0]!, outer[1]!];
	}
	const l = v as string[] | undefined;
	if (!l || l.length !== 2) throw new Error(`a ${type} via needs layers: [from, to], e.g. ["F.Cu", "In1.Cu"]`);
	const [a, b] = [copperLayer(c, l[0], "layers[0]"), copperLayer(c, l[1], "layers[1]")].sort((x, y) => cu.indexOf(x) - cu.indexOf(y)) as [string, string];
	if (a === b) throw new Error("layers: give two different copper layers");
	if (!outer.includes(a) && !outer.includes(b)) throw new Error(`layers: a ${type} via starts on an outer layer (${outer.join(" or ")}); ${a}–${b} would be buried`);
	if (outer.includes(a) && outer.includes(b)) throw new Error(`layers: ${a}–${b} spans the board: use a through via`);
	if (type === "micro" && cu.indexOf(b) - cu.indexOf(a) !== 1) throw new Error(`layers: a micro via joins two adjacent copper layers, not ${a}–${b}`);
	return [a, b];
}

const addVia: OpDef = {
	op: "add_via",
	group: "board",
	doc:
		"Place a via. The net is the copper's at `at` on the layers it spans (tracks, pads and vias first, else a zone fill) unless given; copper of several nets there, or a given net that differs from the copper's, is an error. " +
		"Size and drill: the net class's via_diameter and via_drill (micro vias: microvia_diameter and microvia_drill) unless given.",
	fields: {
		at: { t: "point", req: true, doc: "centre, board mm" },
		net: { t: "string", doc: "net name (default: the copper's at `at`)" },
		size: { t: "number", doc: "pad diameter, mm" },
		drill: { t: "number", doc: "drill diameter, mm" },
		type: { t: "string", values: VIA_TYPES, doc: "through (default), blind or micro" },
		layers: { t: "strings", doc: 'blind and micro: [from, to], one an outer layer, e.g. ["F.Cu", "In1.Cu"]' },
	},
	async apply(ctx, args) {
		const board = ctx.project.board();
		const c = Copper.of(board);
		const at = args.at as Pt;
		const type = (args.type as (typeof VIA_TYPES)[number] | undefined) ?? "through";
		const layers = viaLayers(c, type, args.layers);
		const span = c.copperLayers.slice(c.copperLayers.indexOf(layers[0]), c.copperLayers.indexOf(layers[1]) + 1);
		const here = netsAt(c, at, span);
		const solid = [...new Set(span.flatMap(l => c.itemsAt(at, l)).filter(i => i.kind !== "fill").map(i => netOfItem(c, i)))];
		let net: string;
		if (args.net !== undefined) {
			net = c.net(String(args.net));
			const other = solid.filter(x => x !== net);
			if (other.length) throw new Error(`the copper at ${fmtPt(at)} is on ${other.join(", ") || "no net"}, not ${net}`);
		} else {
			if (here.length > 1) throw new Error(`copper of several nets at ${fmtPt(at)} (${here.map(x => x || "no net").join(", ")}): give net`);
			if (!here.length || !here[0]) throw new Error(`no net's copper at ${fmtPt(at)} on ${span.join(", ")}: give net`);
			net = here[0];
		}
		const cls = await netClass(ctx.project, net);
		const size = (args.size as number | undefined) ?? (type === "micro" ? cls.microvia_diameter : cls.via_diameter);
		const drill = (args.drill as number | undefined) ?? (type === "micro" ? cls.microvia_drill : cls.via_drill);
		if (!(size > 0 && drill > 0)) throw new Error("size and drill must be positive");
		if (drill >= size) throw new Error(`drill ${drill} mm must be smaller than size ${size} mm`);
		const node = viaNode(at, size, drill, type, layers, net);
		board.add(node);
		ctx.project.touchBoard();
		ctx.touchBoard(boxAround([at], size / 2 + 1), undefined, { nets: [net], layers: span });
		ctx.notes.push(`via ${net} at ${fmtPt(at)}: ${fmtMm(size)}/${fmtMm(drill)} mm, ${type} ${layers[0]}–${layers[1]} (class ${cls.name}); uuid ${short(text(child(node, "uuid")) ?? "")}`);
	},
};

/** A drilled hole: vias and drilled pads (plated or not), centre and diameter (an oval's longer side). */
function holes(c: Copper): { at: Pt; d: number }[] {
	const out = c.vias.map(v => ({ at: v.at, d: v.drill }));
	for (const p of c.pads) {
		const dr = child(p.node, "drill");
		if (!dr || (p.info.type !== "thru_hole" && p.info.type !== "np_thru_hole")) continue;
		const vals = dr.slice(1).flatMap(a => (isList(a) || a.v === "oval" ? [] : [Number(a.v)])).filter(Number.isFinite);
		if (vals.length) out.push({ at: p.at, d: Math.max(...vals) });
	}
	return out;
}

/** A region argument: points [[x, y], …], {points} or {rect}, as a polygon. */
function regionArg(v: unknown): Pt[] {
	if (Array.isArray(v)) return shapePolygon(shapeIn({ points: v }, "region", false));
	return shapePolygon(shapeIn(v, "region", false));
}

/** Violation types that only say routing is unfinished: they don't drop a via. */
const UNFINISHED: Record<string, true> = { via_dangling: true, track_dangling: true };

/**
 * Ask KiCad which new vias break a rule: the board with all of them, the .kicad_pro and the
 * .kicad_dru as this transaction leaves them, in a scratch directory; DRC with the zones refilled.
 * Returns each via a violation names with its reason ("clearance (VIN spacing)"); of two new vias
 * in one violation, the later one.
 */
async function drcDrops(project: Project, vias: List[]): Promise<Map<List, string>> {
	const board = project.board();
	const dir = cli.tmpDir("stitch");
	const base = path.join(dir, project.name);
	board.add(...vias);
	try {
		fs.writeFileSync(`${base}.kicad_pcb`, board.serialize());
	} finally {
		for (const v of vias) board.remove(v);
	}
	try {
		for (const [file, ext] of [
			[project.proFile, "kicad_pro"],
			[project.druFile, "kicad_dru"],
		] as const) {
			const t = project.fileText(file);
			if (t !== undefined) fs.writeFileSync(`${base}.${ext}`, t);
		}
		const r = await cli.pcbDrc(`${base}.kicad_pcb`, { cwd: dir, repoRoot: project.repoRoot }, false, true);
		const order = new Map(vias.map((v, i) => [text(child(v, "uuid")) ?? "", i]));
		const out = new Map<List, string>();
		for (const v of r.violations) {
			if (v.excluded || UNFINISHED[v.type]) continue;
			const mine = v.items.map(i => order.get(i.uuid ?? "")).filter((i): i is number => i !== undefined);
			if (!mine.length) continue;
			const via = vias[Math.max(...mine)]!;
			// "Clearance violation (rule 'VIN spacing' clearance 1,5000 mm; …)": the rule's name
			const rule = /\(([^();]*?) (?:clearance|min|max|constraint) /.exec(v.description)?.[1]?.replace(/^rule '(.*)'$/, "$1");
			if (!out.has(via)) out.set(via, `${v.type}${rule ? ` (${rule})` : ""}`);
		}
		return out;
	} finally {
		fs.rmSync(dir, { recursive: true, force: true });
	}
}

/** "10 clearance (VIN spacing), 2 hole_clearance": dropped vias counted by reason. */
function countReasons(dropped: Map<List, string>): string {
	const n = new Map<string, number>();
	for (const r of dropped.values()) n.set(r, (n.get(r) ?? 0) + 1);
	return [...n].map(([r, k]) => `${k} ${r}`).join(", ");
}

const stitchVias: OpDef = {
	op: "stitch_vias",
	group: "board",
	doc:
		"Place through vias of a net in a grid over a region, or in rows on both sides of another net's tracks at `offset` from their centrelines (return or guard vias), `pitch` apart. " +
		"Positions are skipped where the via's copper comes within the clearance of another net's copper (pads, tracks, vias; the larger of the two nets' class clearances and the board's min_clearance) or of any pad, within min_copper_edge_clearance of the outline or a cutout (or outside it), within min_hole_to_hole of another hole, inside a rule area that forbids vias, or nearer than half the pitch to a via placed before. " +
		"The vias left are then checked by KiCad's DRC (zones refilled) with every rule of the project, its custom rules (.kicad_dru) included; those a violation names are dropped. Other nets' zone fills don't block: KiCad refills them around the vias. The note says how many were placed and why the others were skipped.",
	fields: {
		net: { t: "string", req: true, doc: 'net of the vias, e.g. "GND"' },
		region: { t: "any", doc: "grid region: points [[x, y], …], {points} or {rect: {x, y, width, height}}" },
		along: { t: "object", doc: "{net, offset}: rows on both sides of this net's tracks, offset mm from their centrelines" },
		pitch: { t: "number", req: true, doc: "via spacing, mm" },
		size: { t: "number", doc: "via diameter, mm (default: the class's via_diameter)" },
		drill: { t: "number", doc: "drill, mm (default: the class's via_drill)" },
		clearance: { t: "number", doc: "least gap to other nets' copper and pads before KiCad's DRC, mm (default: by the nets' classes and min_clearance)" },
	},
	async apply(ctx, args) {
		if ((args.region === undefined) === (args.along === undefined)) throw new Error("give region or along (one of them)");
		const project = ctx.project;
		const board = project.board();
		const c = Copper.of(board);
		const net = c.net(String(args.net));
		const pitch = args.pitch as number;
		if (!(pitch > 0)) throw new Error("pitch must be positive");
		const cls = await netClass(project, net);
		const size = (args.size as number | undefined) ?? cls.via_diameter;
		const drill = (args.drill as number | undefined) ?? cls.via_drill;
		if (!(size > 0 && drill > 0 && drill < size)) throw new Error("size and drill must be positive, the drill smaller than the size");
		const minClearance = boardRule(project, "min_clearance");
		const given = args.clearance as number | undefined;
		if (given !== undefined && !(given >= 0)) throw new Error("clearance must be >= 0");
		const edgeClr = boardRule(project, "min_copper_edge_clearance");
		const holeGap = boardRule(project, "min_hole_to_hole");
		const r = size / 2;
		const o = boardOutline(board.tree.filter((x): x is List => isList(x) && isEdgeItem(x)));
		if (!o.polygon) throw new Error("the board has no closed outline (Edge.Cuts): set_board_edges draws one");
		const edges = [o.polygon, ...o.holes].flatMap(poly => poly.map((p, i) => shape([p, poly[(i + 1) % poly.length]!])));

		const cands: Pt[] = [];
		let along: string | undefined;
		if (args.region !== undefined) {
			const poly = regionArg(args.region);
			const [x0, y0, x1, y1] = bbox(poly);
			const axis = (a: number, b: number) => {
				const k = Math.floor((b - a) / pitch + 1e-9);
				const s = a + (b - a - k * pitch) / 2;
				return Array.from({ length: k + 1 }, (_, i) => s + i * pitch);
			};
			for (const y of axis(y0, y1)) for (const x of axis(x0, x1)) if (pointInPolygon([x, y], poly)) cands.push([x, y]);
		} else {
			const a = args.along as Record<string, unknown>;
			const extra = Object.keys(a).filter(k => k !== "net" && k !== "offset");
			if (typeof a.net !== "string" || typeof a.offset !== "number" || extra.length) throw new Error("along: give {net, offset}");
			along = c.net(a.net);
			if (along === net) throw new Error("along: give another net than the vias'");
			if (!(a.offset > 0)) throw new Error("along.offset must be positive");
			const tracks = c.tracks.filter(t => t.net === along);
			if (!tracks.length) throw new Error(`along: ${along} has no tracks`);
			for (const t of tracks) {
				const path: Pt[] = t.kind === "track" ? [t.start, t.end] : [t.shapes[0]!.pts[0]!, ...t.shapes.map(s => s.pts[1]!)];
				const lens = path.slice(1).map((q, i) => dist(path[i]!, q));
				const total = lens.reduce((x, y) => x + y, 0);
				const k = Math.floor(total / pitch + 1e-9);
				for (let i = 0; i <= k; i++) {
					let s = (total - k * pitch) / 2 + i * pitch;
					let j = 0;
					while (j < lens.length - 1 && s > lens[j]!) s -= lens[j++]!;
					const [pa, pb] = [path[j]!, path[j + 1]!];
					const u: Pt = [(pb[0] - pa[0]) / lens[j]!, (pb[1] - pa[1]) / lens[j]!];
					const q: Pt = [pa[0] + u[0] * s, pa[1] + u[1] * s];
					for (const side of [1, -1]) cands.push([q[0] + u[1] * a.offset * side, q[1] - u[0] * a.offset * side]);
				}
			}
		}

		const span = c.copperLayers;
		// each blocker's clearance: the larger of both nets' class clearances and min_clearance (KiCad's custom rules come with its DRC below)
		const classClearance = new Map<string, number>();
		for (const n of new Set(["", net, ...c.pads.map(p => p.net), ...c.tracks.map(t => t.net), ...c.vias.map(v => v.net)])) {
			classClearance.set(n, (await netClass(project, n || "Default")).clearance);
		}
		const gapTo = (other: string) => given ?? Math.max(classClearance.get(net)!, classClearance.get(other) ?? 0, minClearance);
		const blockers: { s: Shape; pad: boolean; gap: number }[] = [
			...c.pads.filter(p => p.copper.length).flatMap(p => p.shapes.map(s => ({ s, pad: true, gap: gapTo(p.net) }))),
			...c.tracks.filter(t => t.net !== net).flatMap(t => t.shapes.map(s => ({ s, pad: false, gap: gapTo(t.net) }))),
			...c.vias.filter(v => v.net !== net).map(v => ({ s: v.shapes[0]!, pad: false, gap: gapTo(v.net) })),
		];
		const keepouts = c.ruleAreas
			.filter(a => a.keepout.vias && a.layers.some(l => span.includes(l)) && a.outline.length >= 3)
			.map(a => ({ a, edges: [a.outline, ...a.holes].flatMap(poly => poly.map((p, i) => shape([p, poly[(i + 1) % poly.length]!]))) }));
		const hs = holes(c);
		const placed: Pt[] = [];
		const skipped = { copper: 0, pads: 0, outline: 0, holes: 0, rule_area: 0, spacing: 0 };
		const near = (s: Shape, q: Pt, m: number) => q[0] >= s.box[0] - m && q[0] <= s.box[2] + m && q[1] >= s.box[1] - m && q[1] <= s.box[3] + m;
		for (const q of cands) {
			const disc = shape([q], r);
			if (placed.some(x => dist(x, q) < pitch / 2 - 1e-9)) {
				skipped.spacing++;
				continue;
			}
			if (!pointInPolygon(q, o.polygon) || o.holes.some(h => pointInPolygon(q, h)) || edges.some(e => near(e, q, r + edgeClr) && (shapeDistance(disc, e, edgeClr)?.d ?? Infinity) < edgeClr)) {
				skipped.outline++;
				continue;
			}
			const inArea = ({ a, edges: ae }: (typeof keepouts)[number]) =>
				(pointInPolygon(q, a.outline) && !a.holes.some(h => pointInPolygon(q, h))) || ae.some(e => near(e, q, r) && (shapeDistance(disc, e, 0)?.d ?? Infinity) <= 0);
			if (keepouts.some(inArea)) {
				skipped.rule_area++;
				continue;
			}
			const hit = blockers.find(b => near(b.s, q, r + b.gap) && (shapeDistance(disc, b.s, b.gap)?.d ?? Infinity) < b.gap);
			if (hit) {
				if (hit.pad) skipped.pads++;
				else skipped.copper++;
				continue;
			}
			if ([...hs, ...placed.map(at => ({ at, d: drill }))].some(h => dist(h.at, q) - h.d / 2 - drill / 2 < holeGap)) {
				skipped.holes++;
				continue;
			}
			placed.push(q);
		}
		const why = Object.entries(skipped)
			.filter(([, n]) => n)
			.map(([k, n]) => `${n} ${k.replace("_", " ")}`)
			.join(", ");
		if (!placed.length) throw new Error(`no via position fits (${cands.length} tried${why ? `; skipped: ${why}` : ""})`);
		const nodes = placed.map(q => viaNode(q, size, drill, "through", [span[0]!, span[span.length - 1]!], net));
		const dropped = await drcDrops(project, nodes);
		const kept = nodes.filter(n => !dropped.has(n));
		if (!kept.length) throw new Error(`KiCad's DRC refuses every via position (${placed.length} tried after the geometric checks: ${countReasons(dropped)})`);
		board.add(...kept);
		project.touchBoard();
		ctx.touchBoard(boxAround(placed, r + 1), undefined, { nets: [net, ...(along ? [along] : [])], layers: span });
		ctx.notes.push(
			`stitch_vias ${net}: ${kept.length} placed, ${cands.length - kept.length} skipped${why ? ` (${why})` : ""}${dropped.size ? `, ${dropped.size} of them by KiCad's DRC (${countReasons(dropped)})` : ""}; ${fmtMm(size)}/${fmtMm(drill)} mm, pitch ${fmtMm(pitch)} mm${given !== undefined ? `, clearance ${fmtMm(given)} mm` : ""}${along ? `, along ${along}` : ""}`,
		);
	},
};

// ---- remove_copper ---------------------------------------------------------------------------------

/** The tracks and vias on the shortest copper path between two pads of a net (through junctions; zone fills crossed, not removed). */
function pathItems(c: Copper, from: string, to: string): (Track | Via)[] {
	const a = c.pad(from);
	const b = c.pad(to);
	const net = a[0]!.net;
	if (!net) throw new Error(`${from} is on no net`);
	if (b[0]!.net !== net) throw new Error(`${from} is on ${net} but ${to} is on ${b[0]!.net || "no net"}`);
	const k = c.connectivity(net);
	const adj = k.nodes.map(() => [] as number[]);
	for (const j of k.junctions) {
		adj[j.a]!.push(j.b);
		adj[j.b]!.push(j.a);
	}
	const cost = (i: number) => {
		const it = k.nodes[i]!.item;
		return it.kind === "track" || it.kind === "arc" ? it.length : 0;
	};
	const d = k.nodes.map(() => Infinity);
	const prev = k.nodes.map(() => -1);
	const done = new Set<number>();
	for (const p of a) {
		const i = k.nodes.findIndex(n => n.item === p);
		if (i >= 0) d[i] = 0;
	}
	const targets = new Set(b.map(p => k.nodes.findIndex(n => n.item === p)));
	for (;;) {
		let u = -1;
		for (let i = 0; i < d.length; i++) if (!done.has(i) && d[i]! < Infinity && (u < 0 || d[i]! < d[u]!)) u = i;
		if (u < 0) throw new Error(`${from} and ${to} are not connected by copper`);
		if (targets.has(u)) {
			const out: (Track | Via)[] = [];
			for (let x = u; x >= 0; x = prev[x]!) {
				const it = k.nodes[x]!.item;
				if (it.kind === "track" || it.kind === "arc" || it.kind === "via") out.unshift(it);
			}
			return out;
		}
		done.add(u);
		for (const v of adj[u]!) {
			const nd = d[u]! + cost(v);
			if (nd < d[v]!) {
				d[v] = nd;
				prev[v] = u;
			}
		}
	}
}

function itemBox(it: Track | Via): Box {
	return boxOfBoxes(it.shapes.map(s => s.box))!;
}

/** A region argument [x0, y0, x1, y1] as a rectangle shape. */
function boxArg(v: unknown): Shape | undefined {
	if (v === undefined) return undefined;
	const b = v as number[];
	if (!Array.isArray(b) || b.length !== 4 || !b.every(x => typeof x === "number") || b[0]! > b[2]! || b[1]! > b[3]!) throw new Error(`box must be [x0, y0, x1, y1] with x0 ≤ x1 and y0 ≤ y1, got ${JSON.stringify(v)}`);
	return shape([
		[b[0]!, b[1]!],
		[b[2]!, b[1]!],
		[b[2]!, b[3]!],
		[b[0]!, b[3]!],
	]);
}

const removeCopper: OpDef = {
	op: "remove_copper",
	group: "board",
	doc:
		"Delete tracks, arc tracks and vias by selector: `at` the item under the point (on `layer` when given; several there: the one whose centreline is nearest, all of them at a junction), `box` what touches the region, `net` the net's, `from`+`to` those on the copper path between two pads; filters combine (`layer`, `net`, `kind`). " +
		"A KiCad 10 tuning pattern is removed with its tracks only when every one of them goes; half of one is refused. Locked items go only with locked: true. A selector that matches nothing is an error.",
	fields: {
		at: { t: "point", doc: "point on the item, board mm" },
		box: { t: "any", doc: "region [x0, y0, x1, y1], board mm" },
		net: { t: "string", doc: "only this net" },
		layer: { t: "string", doc: "only this copper layer (vias spanning it)" },
		kind: { t: "string", values: ["track", "via", "all"], doc: "track (with arcs), via, or all (default)" },
		from: { t: "string", doc: 'pad "REF.NUMBER": with `to`, the copper path between them' },
		to: { t: "string", doc: 'pad "REF.NUMBER"' },
		locked: { t: "boolean", doc: "also remove locked items (default false: refused)" },
	},
	apply(ctx, args) {
		const board = ctx.project.board();
		const c = Copper.of(board);
		if ((args.from === undefined) !== (args.to === undefined)) throw new Error("give from and to together");
		if ([args.at, args.box, args.net, args.from].every(v => v === undefined)) throw new Error("give at, box, net and/or from+to");
		const layer = args.layer !== undefined ? copperLayer(c, args.layer) : undefined;
		const net = args.net !== undefined ? c.net(String(args.net)) : undefined;
		const kind = (args.kind as string | undefined) ?? "all";
		const rect = boxArg(args.box);
		let items: (Track | Via)[] = [...c.tracks, ...c.vias];
		if (args.from !== undefined) items = pathItems(c, String(args.from), String(args.to));
		items = items.filter(
			it =>
				(kind === "all" || (kind === "via") === (it.kind === "via")) &&
				(net === undefined || it.net === net) &&
				(!layer || (it.kind === "via" ? it.span.includes(layer) : it.layer === layer)),
		);
		if (rect) items = items.filter(it => it.shapes.some(s => (shapeDistance(s, rect, 0)?.d ?? Infinity) <= 0));
		if (args.at !== undefined) {
			const at = args.at as Pt;
			const under = items.filter(it => it.shapes.some(s => shapeHas(s, at)));
			const centre = (it: Track | Via) => (it.kind === "via" ? dist(it.at, at) : Math.min(...it.shapes.map(s => shapeDistance(shape(s.pts), shape([at]))?.d ?? Infinity)));
			const best = Math.min(...under.map(centre));
			items = under.filter(it => centre(it) <= best + 1e-3);
		}
		if (!items.length) throw new Error("nothing matches: no track or via is selected by these arguments (the copper query lists them)");
		const locked = items.filter(it => it.locked);
		if (locked.length && args.locked !== true) throw new Error(`${plural(locked.length, "locked item")} selected (${locked.map(it => `${it.kind} ${short(it.uuid)}`).join(", ")}): give locked: true to remove them too`);
		const gone = new Set(items.map(it => it.uuid));
		const groups: string[] = [];
		for (const g of c.generated) {
			const hit = g.members.filter(m => gone.has(m)).length;
			if (!hit) continue;
			if (hit < g.members.length) {
				throw new Error(
					`these items include ${hit} of the ${g.members.length} tracks of tuning pattern ${g.name || short(g.uuid)} (${short(g.uuid)}${g.net ? `, ${g.net}` : ""}): select all of them (e.g. net and layer) or none`,
				);
			}
			board.remove(g.node);
			groups.push(g.name || short(g.uuid));
		}
		for (const it of items) board.remove(it.node);
		ctx.project.touchBoard();
		const nets = [...new Set(items.map(it => it.net).filter(n => n))];
		const layers = [...new Set(items.flatMap(it => (it.kind === "via" ? it.span : [it.layer])))];
		ctx.touchBoard(boxOfBoxes(items.map(itemBox)), undefined, { nets, layers });
		const tracks = items.filter(it => it.kind !== "via").length;
		ctx.notes.push(
			`removed ${plural(tracks, "track")} and ${plural(items.length - tracks, "via")} (${nets.join(", ") || "no net"})${groups.length ? `, with tuning pattern${groups.length > 1 ? "s" : ""} ${groups.join(", ")}` : ""}`,
		);
	},
};

// ---- set_track_width -------------------------------------------------------------------------------

const setTrackWidth: OpDef = {
	op: "set_track_width",
	group: "board",
	doc: "Change the width of existing tracks and arc tracks, selected by net, net class (its nets in KiCad's netlist), region and/or layer (filters combine; give at least one of net, class, box).",
	fields: {
		width: { t: "number", req: true, doc: "new width, mm" },
		net: { t: "string", doc: "only this net" },
		class: { t: "string", doc: "only the nets of this net class" },
		box: { t: "any", doc: "only tracks touching the region [x0, y0, x1, y1], board mm" },
		layer: { t: "string", doc: "only this copper layer" },
	},
	async apply(ctx, args) {
		const width = args.width as number;
		if (!(width > 0)) throw new Error("width must be positive");
		if ([args.net, args.class, args.box].every(v => v === undefined)) throw new Error("give net, class and/or box");
		const board = ctx.project.board();
		const c = Copper.of(board);
		const layer = args.layer !== undefined ? copperLayer(c, args.layer) : undefined;
		const net = args.net !== undefined ? c.net(String(args.net)) : undefined;
		const nets = args.class !== undefined ? new Set(await classNets(ctx.project, c, String(args.class))) : undefined;
		const rect = boxArg(args.box);
		const hit = c.tracks.filter(
			t =>
				(net === undefined || t.net === net) &&
				(!nets || nets.has(t.net)) &&
				(!layer || t.layer === layer) &&
				(!rect || t.shapes.some(s => (shapeDistance(s, rect, 0)?.d ?? Infinity) <= 0)),
		);
		if (!hit.length) throw new Error("no track matches these arguments (the copper query lists them)");
		const was = [...new Set(hit.map(t => t.width))].sort((a, b) => a - b);
		for (const t of hit) setChild(t.node, L("width", mm(width)));
		ctx.project.touchBoard();
		ctx.touchBoard(boxOfBoxes(hit.map(itemBox)), undefined, { nets: [...new Set(hit.map(t => t.net).filter(n => n))], layers: [...new Set(hit.map(t => t.layer))] });
		ctx.notes.push(`track width ${fmtMm(width)} mm on ${plural(hit.length, "track")} (was ${was.map(fmtMm).join(", ")} mm)`);
	},
};

export const ops: OpDef[] = [addTrack, addDiffPair, addVia, stitchVias, removeCopper, setTrackWidth];
