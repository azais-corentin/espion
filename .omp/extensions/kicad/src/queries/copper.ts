/**
 * Board copper queries: tracks, vias and zones in a region, routing and lengths per net, zones and
 * rule areas, free space for a footprint, distances between copper, board drawings and texts, and
 * KiCad's board statistics. Board millimetres, x right, y down; rotations counter-clockwise.
 */
import { courtyardBox, courtyardEdges, fpRef, insideEdges, padsInfo, placement } from "../board";
import * as cli from "../cli";
import { Copper, type Pad, type PathLength, type Shape, shape, shapeDistance, type Track, useHeightForLength } from "../copper";
import { boardOutline, isEdgeItem, pointInPolygon } from "../edges";
import { type Box, bbox, overlap, type Pt } from "../geom";
import { boardSettings } from "../ops/board";
import type { Project } from "../project";
import { isList, type List } from "../sexpr";
import type { QueryDef } from "../types";

const r4 = (v: number) => Math.round(v * 1e4) / 1e4 + 0;
const rp = (p: Pt): Pt => [r4(p[0]), r4(p[1])];
const short = (uuid: string) => uuid.slice(0, 8);

/** Keep the first `limit` rows and say how many there were. */
function page<T>(rows: T[], limit: number, key: string): Record<string, unknown> {
	return rows.length > limit ? { total: rows.length, shown: limit, [key]: rows.slice(0, limit) } : { total: rows.length, [key]: rows };
}

function boxArg(v: unknown): Box | undefined {
	if (v === undefined) return undefined;
	const b = v as number[];
	if (!Array.isArray(b) || b.length !== 4 || !b.every(x => typeof x === "number") || b[0]! > b[2]! || b[1]! > b[3]!) {
		throw new Error(`box must be [x0, y0, x1, y1] with x0 ≤ x1 and y0 ≤ y1, got ${JSON.stringify(v)}`);
	}
	return [b[0]!, b[1]!, b[2]!, b[3]!];
}

function layerArg(c: Copper, layer: unknown): string | undefined {
	if (layer === undefined) return undefined;
	if (!c.copperLayers.includes(String(layer))) throw new Error(`layer "${layer}" is not a copper layer of the board (${c.copperLayers.join(", ")})`);
	return String(layer);
}

/** Nets of a net class, from KiCad's netlist. */
async function classNets(ctx: { analysis: { netlist(): Promise<cli.Netlist> } }, cls: string, c: Copper): Promise<string[]> {
	const nl = await ctx.analysis.netlist();
	const classes = new Set(nl.nets.map(n => n.netclass ?? "Default"));
	if (![...classes].some(k => k.toLowerCase() === cls.toLowerCase())) throw new Error(`no net class "${cls}" (classes in the netlist: ${[...classes].sort().join(", ")})`);
	return nl.nets.filter(n => (n.netclass ?? "Default").toLowerCase() === cls.toLowerCase() && c.hasNet(n.name)).map(n => n.name).sort();
}

function trackRow(t: Track) {
	return {
		id: short(t.uuid),
		kind: t.kind,
		net: t.net,
		layer: t.layer,
		width: t.width,
		start: rp(t.start),
		end: rp(t.end),
		...(t.mid ? { mid: rp(t.mid) } : {}),
		length: r4(t.length),
		...(t.locked ? { locked: true } : {}),
		...(t.tuning ? { tuning_pattern: short(t.tuning) } : {}),
	};
}

const copperQuery: QueryDef = {
	what: "copper",
	doc:
		"Board copper in a region: tracks and arc tracks (id = uuid prefix, net, layer, width, start, end, an arc's mid point, centreline length, locked, tuning pattern), vias (net, at, size, drill, type through/blind/buried/micro, span of copper layers) and a summary of the zones touching it (name, net, layers, priority, filled, filled area per layer in mm²). " +
		"Filters combine; `box` keeps what touches the region.",
	fields: {
		net: { t: "string", doc: 'Only this net ("/VIN", "GND").' },
		layer: { t: "string", doc: 'Only this copper layer ("F.Cu").' },
		box: { t: "any", doc: "Region [x0, y0, x1, y1] in board mm: only items touching it." },
		kind: { t: "string", doc: "Only tracks (straight), arcs, vias or zones.", values: ["track", "arc", "via", "zone"] },
		limit: { t: "integer", doc: "Most track and via rows to return (default 200 each)." },
	},
	async run({ project }, args) {
		const c = Copper.of(project.board());
		const net = args.net === undefined ? undefined : c.net(String(args.net));
		const layer = layerArg(c, args.layer);
		const box = boxArg(args.box);
		const kind = args.kind as string | undefined;
		const limit = typeof args.limit === "number" && args.limit > 0 ? args.limit : 200;
		const touches = (b: Box) => !box || (b[0] <= box[2] && box[0] <= b[2] && b[1] <= box[3] && box[1] <= b[3]);
		const out: Record<string, unknown> = {};
		if (!kind || kind === "track" || kind === "arc") {
			const rows = c.tracks
				.filter(t => (!kind || t.kind === kind) && (net === undefined || t.net === net) && (!layer || t.layer === layer) && touches(bbox(t.shapes.flatMap((s): Pt[] => [[s.box[0], s.box[1]], [s.box[2], s.box[3]]]))))
				.map(trackRow);
			Object.assign(out, page(rows, limit, "tracks"));
		}
		if (!kind || kind === "via") {
			const rows = c.vias
				.filter(v => (net === undefined || v.net === net) && (!layer || v.span.includes(layer)) && touches(v.shapes[0]!.box))
				.map(v => ({
					id: short(v.uuid),
					net: v.net,
					at: rp(v.at),
					size: v.size,
					drill: v.drill,
					type: v.type,
					span: v.span.length > 1 ? `${v.span[0]}..${v.span[v.span.length - 1]}` : (v.span[0] ?? ""),
					...(v.locked ? { locked: true } : {}),
				}));
			const p = page(rows, limit, "vias");
			out.vias = p.vias;
			if (p.shown) out.vias_total = p.total;
		}
		if (!kind || kind === "zone") {
			out.zones = c.zones
				.filter(z => (net === undefined || z.net === net) && (!layer || z.layers.includes(layer)) && z.outline.length > 0 && touches(bbox(z.outline)))
				.map(z => ({
					id: short(z.uuid),
					...(z.name ? { name: z.name } : {}),
					net: z.net,
					layers: z.layers,
					priority: z.priority,
					filled: z.fills.length > 0,
					filled_area: Object.fromEntries(Object.entries(z.filledArea).map(([l, a]) => [l, r4(a)])),
				}));
		}
		return out;
	},
};

const padRow = (p: Pad, all: number) => ({ pad: p.id, at: rp(p.at), layers: p.copper.length === all && all > 2 ? "all copper" : p.copper.join(" ") });

function lengthRow(c: Copper, net: string, useHeight: boolean) {
	const l = c.netLength(net, { useHeight });
	return {
		net,
		length: r4(l.total),
		track: r4(l.track),
		per_layer: Object.fromEntries(Object.entries(l.perLayer).map(([k, v]) => [k, r4(v)])),
		vias: l.vias,
		...(useHeight ? { via_height: r4(l.viaHeight) } : {}),
		...(l.padToDie ? { pad_to_die: r4(l.padToDie) } : {}),
	};
}

/** Partner and skew (this net's length minus the partner's) of a diff-pair net. */
function pairOf(c: Copper, net: string, useHeight: boolean) {
	const m = c.dpPartner(net);
	if (!m) return undefined;
	return { partner: m.partner, skew: r4(c.netLength(net, { useHeight }).total - c.netLength(m.partner, { useHeight }).total) };
}

async function selectNets(ctx: Parameters<QueryDef["run"]>[0], c: Copper, args: Record<string, unknown>, key: "net" | "nets"): Promise<string[] | undefined> {
	if (args[key] !== undefined && args.class !== undefined) throw new Error(`give ${key} or class, not both`);
	if (args.class !== undefined) return classNets(ctx, String(args.class), c);
	if (args[key] === undefined) return undefined;
	return (Array.isArray(args[key]) ? (args[key] as string[]) : [String(args[key])]).map(n => c.net(n));
}

const routingQuery: QueryDef = {
	what: "routing",
	doc:
		"Routing state per net, from the board's copper (tracks, arcs, vias, filled zones and copper drawings with the net, joined as KiCad's connectivity joins them: copper that overlaps on a shared layer). With net or class, per net: pads (ref.pin, at, copper layers), routed length (as `lengths` gives it), clusters (connected groups: their pads, track, via, drawing and zone counts), unrouted connections (the nearest pads between clusters, KiCad's ratsnest, with their copper centres and distance) and, for a diff-pair net (KiCad's naming: _P/_N, +/-), its partner and the length skew (this net minus the partner). " +
		"Without either: one row per net [net, pads, length, vias, clusters, unrouted] and the board's unrouted total. Zones count only once filled (KiCad fills them on refill).",
	fields: {
		net: { t: "string", doc: "One net." },
		class: { t: "string", doc: "Every net of this net class (KiCad's netlist)." },
		limit: { t: "integer", doc: "Most nets to detail (default 40) or rows (default 300)." },
	},
	async run(ctx, args) {
		const c = Copper.of(ctx.project.board());
		const useHeight = useHeightForLength(ctx.project);
		const nets = await selectNets(ctx, c, args, "net");
		const all = c.copperLayers.length;
		if (!nets) {
			const rows = c.nets().map(n => {
				const k = c.connectivity(n);
				return [n, k.pads.length, r4(c.netLength(n, { useHeight }).total), k.vias.length, k.clusters.length, k.unrouted.length] as const;
			});
			return {
				columns: ["net", "pads", "length", "vias", "clusters", "unrouted"],
				unrouted: rows.reduce((t, r) => t + r[5], 0),
				...page(rows, typeof args.limit === "number" && args.limit > 0 ? args.limit : 300, "nets"),
			};
		}
		const detail = nets.map(n => {
			const k = c.connectivity(n);
			const pair = pairOf(c, n, useHeight);
			return {
				net: n,
				pads: k.pads.map(p => padRow(p, all)),
				routed: lengthRow(c, n, useHeight),
				clusters: k.clusters.map(cl => ({
					pads: cl.pads.map(p => p.id),
					tracks: cl.tracks.length,
					vias: cl.vias.length,
					...(cl.drawings.length ? { drawings: cl.drawings.length } : {}),
					...(cl.fills.length ? { zones: [...new Set(cl.fills.map(f => c.zones.find(z => z.uuid === f.zone)?.name ?? short(f.zone)))] } : {}),
				})),
				unrouted: k.unrouted.map(r => ({ from: r.from.id, to: r.to.id, from_at: rp(r.from.centre), to_at: rp(r.to.centre), distance: r4(r.distance) })),
				...(pair ? { pair } : {}),
			};
		});
		const limit = typeof args.limit === "number" && args.limit > 0 ? args.limit : 40;
		return { via_heights_counted: useHeight, ...page(detail, limit, "nets") };
	},
};

function pathRow(p: PathLength | undefined, from: string, to: string, useHeight: boolean) {
	if (!p) return { from, to, unrouted: true };
	return {
		from,
		to,
		length: r4(p.length),
		track: r4(p.track),
		per_layer: Object.fromEntries(Object.entries(p.perLayer).map(([k, v]) => [k, r4(v)])),
		vias: p.vias,
		...(useHeight ? { via_height: r4(p.viaHeight) } : {}),
		...(p.padToDie ? { pad_to_die: r4(p.padToDie) } : {}),
		...(p.zones.length ? { through_zones: p.zones } : {}),
	};
}

const lengthsQuery: QueryDef = {
	what: "lengths",
	doc:
		"Routed lengths, mm, as KiCad's length rules measure them: per net the total, tracks (and arcs; the part inside the pad or via a line ends in counts as the straight line from its centre to where it leaves), per layer, via count, via height (between the outermost layers joined at each via, from the stack-up, when the project counts via heights: use_height_for_length_calcs), pad-to-die lengths when pads have them and, for diff-pair nets (_P/_N, +/-), the partner and the skew (this net minus the partner). " +
		'`paths` gives the copper length between two pads of one net instead ({from: "R13.2", to: "U1.10"}): the shortest way along tracks and arcs, through vias (a pad joins a via on its own side), across filled zones as a straight line (named in through_zones), plus the two pads\' pad-to-die lengths; `unrouted` when no copper joins them. ' +
		"Without nets, class or paths: every net that has tracks.",
	fields: {
		nets: { t: "strings", doc: "These nets." },
		class: { t: "string", doc: "Every net of this net class (KiCad's netlist)." },
		paths: { t: "objects", doc: '[{from: "R13.2", to: "U1.10"}, …]: pad to pad copper lengths.' },
	},
	async run(ctx, args) {
		const c = Copper.of(ctx.project.board());
		const useHeight = useHeightForLength(ctx.project);
		const out: Record<string, unknown> = { via_heights_counted: useHeight };
		if (args.paths !== undefined) {
			out.paths = (args.paths as Record<string, unknown>[]).map((p, i) => {
				if (typeof p.from !== "string" || typeof p.to !== "string" || Object.keys(p).some(k => k !== "from" && k !== "to")) {
					throw new Error(`paths[${i}] must be {from: "REF.PIN", to: "REF.PIN"}, got ${JSON.stringify(p)}`);
				}
				return pathRow(c.path(p.from, p.to, { useHeight }), p.from, p.to, useHeight);
			});
			if (args.nets === undefined && args.class === undefined) return out;
		}
		const nets = (await selectNets(ctx, c, args, "nets")) ?? [...new Set(c.tracks.map(t => t.net).filter(Boolean))].sort();
		out.nets = nets.map(n => {
			const pair = pairOf(c, n, useHeight);
			return { ...lengthRow(c, n, useHeight), ...(pair ? { pair } : {}) };
		});
		return out;
	},
};

const zonesQuery: QueryDef = {
	what: "zones",
	doc:
		"Zones and rule areas in full. Zones: id, name, net, layers, priority, settings (clearance, min_thickness, connect_pads thermal/solid/none/thru_hole_only, thermal_gap, thermal_spoke_width, island_removal always/never/below_area with island_min_area mm², fill_mode solid/hatch with hatch settings), outline and holes, filled (KiCad filled it when it saved the board), filled area per layer (mm²) and fill region count, teardrop. " +
		"Rule areas: id, name, layers, keepout (what is not allowed: tracks, vias, pads, copperpour, footprints), outline and holes. Zones and rule areas of a footprint (an antenna keepout) are included with its reference in footprint.",
	fields: {},
	async run({ project }) {
		const c = Copper.of(project.board());
		const poly = (p: Pt[]) => p.map(rp);
		return {
			zones: c.zones.map(z => ({
				id: short(z.uuid),
				...(z.name ? { name: z.name } : {}),
				net: z.net,
				layers: z.layers,
				priority: z.priority,
				settings: z.settings,
				outline: poly(z.outline),
				...(z.holes.length ? { holes: z.holes.map(poly) } : {}),
				filled: z.filled && z.fills.length > 0,
				filled_area: Object.fromEntries(Object.entries(z.filledArea).map(([l, a]) => [l, r4(a)])),
				fill_regions: z.fills.length,
				...(z.teardrop ? { teardrop: true } : {}),
				...(z.locked ? { locked: true } : {}),
				...(z.footprint ? { footprint: z.footprint } : {}),
			})),
			rule_areas: c.ruleAreas.map(a => ({
				id: short(a.uuid),
				...(a.name ? { name: a.name } : {}),
				layers: a.layers,
				keepout: Object.entries(a.keepout)
					.filter(([, no]) => no)
					.map(([k]) => k),
				outline: poly(a.outline),
				...(a.holes.length ? { holes: a.holes.map(poly) } : {}),
				...(a.footprint ? { footprint: a.footprint } : {}),
			})),
		};
	},
};

function outlineOf(project: Project) {
	return boardOutline(project.board().tree.filter((c): c is List => isList(c) && isEdgeItem(c)));
}

const freeSpaceQuery: QueryDef = {
	what: "board_free_space",
	doc:
		"Empty boxes of a given size on the board nearest to a point, for a footprint or a group: inside the outline at least the board's min_copper_edge_clearance from it and from cutouts, clear of the courtyards (their outlines, rotation applied) of the footprints on that side (both sides without `side`), of the through-hole pads of footprints on the other side, and of rule areas that keep footprints out. Results don't overlap each other.",
	fields: {
		near: { t: "point", doc: "Where the space is wanted (board mm).", req: true },
		size: { t: "point", doc: "[width, height] of the space in mm.", req: true },
		side: { t: "string", doc: "Only footprints on this side block (default: both sides).", values: ["top", "bottom"] },
		clearance: { t: "number", doc: "Gap to courtyards, through-hole pads and rule areas (mm, default 0: courtyards already hold the assembly margin)." },
		step: { t: "number", doc: "Grid the boxes' corners lie on (mm, default 0.5)." },
		limit: { t: "integer", doc: "How many boxes (default 3)." },
	},
	async run({ project }, args) {
		const board = project.board();
		const [nx, ny] = args.near as Pt;
		const [w, h] = args.size as Pt;
		if (!(w > 0 && h > 0)) throw new Error(`size must be positive, got ${JSON.stringify(args.size)}`);
		const step = typeof args.step === "number" ? args.step : 0.5;
		if (!(step > 0)) throw new Error("step must be > 0");
		const clear = typeof args.clearance === "number" ? args.clearance : 0;
		const side = args.side as "top" | "bottom" | undefined;
		const limit = typeof args.limit === "number" && args.limit > 0 ? args.limit : 3;
		const o = outlineOf(project);
		if (!o.polygon || !o.bbox) throw new Error("the board has no closed outline (Edge.Cuts): set_board_edges draws one");
		const rules = boardSettings(project)?.rules ?? {};
		const edge = typeof rules.min_copper_edge_clearance === "number" ? rules.min_copper_edge_clearance : 0;
		const blocked: Box[] = [];
		const courts: { box: Box; edges: [Pt, Pt][]; shapes: Shape[] }[] = [];
		for (const fp of board.footprints()) {
			const pl = placement(fp);
			if (!side || pl.side === side) {
				const edges = courtyardEdges(fp);
				if (edges.length) courts.push({ box: bbox(edges.flat()), edges, shapes: edges.map(e => shape([e[0], e[1]])) });
			} else {
				for (const p of padsInfo(fp)) {
					if (p.type !== "thru_hole" && p.type !== "np_thru_hole") continue;
					const r = Math.hypot(...(p.size ?? [0, 0])) / 2;
					blocked.push([p.at[0] - r, p.at[1] - r, p.at[0] + r, p.at[1] + r]);
				}
			}
		}
		// a box touching a courtyard is free (clearance 0); one overlapping it, inside it or around it is not
		const onCourtyard = (b: Box) => {
			const e = 1e-6;
			const s = shape([
				[b[0] + e, b[1] + e],
				[b[2] - e, b[1] + e],
				[b[2] - e, b[3] - e],
				[b[0] + e, b[3] - e],
			]);
			const centre: Pt = [(b[0] + b[2]) / 2, (b[1] + b[3]) / 2];
			return courts.some(k => overlap(b, k.box, clear) && (insideEdges(centre, k.edges) || k.shapes.some(x => (shapeDistance(s, x, clear)?.d ?? Infinity) < clear + 5e-7)));
		};
		const c = Copper.of(board);
		const sideCu = side === "top" ? ["F.Cu"] : side === "bottom" ? [c.copperLayers[c.copperLayers.length - 1]!] : [c.copperLayers[0]!, c.copperLayers[c.copperLayers.length - 1]!];
		const areas = c.ruleAreas.filter(a => a.keepout.footprints && a.layers.some(l => sideCu.includes(l)) && a.outline.length >= 3).map(a => shape(a.outline));
		const edges = [o.polygon, ...o.holes].flatMap(poly => poly.map((p, i) => shape([p, poly[(i + 1) % poly.length]!])));
		const fits = (b: Box) => {
			const centre: Pt = [(b[0] + b[2]) / 2, (b[1] + b[3]) / 2];
			if (!pointInPolygon(centre, o.polygon!) || o.holes.some(hp => pointInPolygon(centre, hp))) return false;
			const s = shape([
				[b[0], b[1]],
				[b[2], b[1]],
				[b[2], b[3]],
				[b[0], b[3]],
			]);
			// the outline and cutouts: no edge through the box or closer than the edge clearance
			if (
				edges.some(e => {
					const d = shapeDistance(s, e, edge)?.d;
					return d !== undefined && (d === 0 || d < edge);
				})
			)
				return false;
			return !areas.some(a => (shapeDistance(s, a, clear)?.d ?? Infinity) <= clear);
		};
		const [bx0, by0, bx1, by1] = o.bbox;
		const cands: { x: number; y: number; d: number }[] = [];
		for (let i = Math.ceil(bx0 / step); i * step + w <= bx1 + 1e-9; i++) {
			for (let k = Math.ceil(by0 / step); k * step + h <= by1 + 1e-9; k++) {
				const x = i * step;
				const y = k * step;
				cands.push({ x, y, d: Math.hypot(x + w / 2 - nx, y + h / 2 - ny) });
			}
		}
		cands.sort((a, b) => a.d - b.d);
		const found: Box[] = [];
		for (const cand of cands) {
			const b: Box = [r4(cand.x), r4(cand.y), r4(cand.x + w), r4(cand.y + h)];
			if (found.some(f => overlap(f, b)) || blocked.some(x => overlap(b, x, clear)) || onCourtyard(b) || !fits(b)) continue;
			found.push(b);
			if (found.length >= limit) break;
		}
		if (!found.length) throw new Error(`no free ${w} × ${h} mm area on the board${side ? ` (${side} side)` : ""}`);
		return { ...(side ? { side } : {}), edge_clearance: edge, clearance: clear, boxes: found.map(b => ({ box: b, center: [r4((b[0] + b[2]) / 2), r4((b[1] + b[3]) / 2)] })) };
	},
};

interface Target {
	label: string;
	/** Each copper shape with its item's centre (none for zone fills) and a name for it. */
	parts: { s: Shape; centre?: Pt; what: string }[];
	/** A footprint's origin, a pad's position, a point. */
	centre?: Pt;
	courtyard?: Box;
}

function target(c: Copper, v: unknown, layer: string | undefined, name: string): Target {
	const onLayer = (layers: string[]) => !layer || layers.includes(layer);
	if (Array.isArray(v) && v.length === 2 && v.every(x => typeof x === "number")) {
		const p: Pt = [v[0] as number, v[1] as number];
		return { label: `(${p[0]}, ${p[1]})`, parts: [{ s: shape([p]), centre: p, what: "point" }], centre: p };
	}
	if (v && typeof v === "object" && !Array.isArray(v)) {
		const o = v as Record<string, unknown>;
		if (typeof o.net !== "string" || Object.keys(o).length !== 1) throw new Error(`${name}: give a pad "U6.75", a footprint "C29", a point [x, y] or {net: "GND"}`);
		const net = c.net(o.net);
		const k = c.connectivity(net);
		return {
			label: `net ${net}`,
			parts: [
				...k.pads.filter(p => onLayer(p.copper)).flatMap(p => p.shapes.map(s => ({ s, centre: p.at, what: `pad ${p.id}` }))),
				...k.tracks
					.filter(t => onLayer([t.layer]))
					.flatMap(t => t.shapes.map(s => ({ s, centre: [(t.start[0] + t.end[0]) / 2, (t.start[1] + t.end[1]) / 2] as Pt, what: `${t.kind} ${short(t.uuid)}` }))),
				...k.vias.filter(x => onLayer(x.span)).map(x => ({ s: x.shapes[0]!, centre: x.at, what: `via ${short(x.uuid)}` })),
				...k.fills.filter(f => onLayer([f.layer])).map(f => ({ s: f.shape, what: `zone ${c.zones.find(z => z.uuid === f.zone)?.name ?? short(f.zone)}` })),
				...k.drawings.filter(d => onLayer([d.layer])).flatMap(d => d.shapes.map(s => ({ s, what: `${d.drawing.type} ${short(d.drawing.uuid)}` }))),
			],
		};
	}
	if (typeof v !== "string") throw new Error(`${name}: give a pad "U6.75", a footprint "C29", a point [x, y] or {net: "GND"}`);
	if (!c.board.footprints().some(fp => fpRef(fp) === v) && v.includes(".")) {
		const pads = c.pad(v).filter(p => onLayer(p.copper));
		return { label: `pad ${v}`, parts: pads.flatMap(p => p.shapes.map(s => ({ s, centre: p.at, what: `pad ${p.id}` }))), centre: pads[0]?.at };
	}
	const fp = c.board.footprint(v);
	const pads = c.pads.filter(p => p.ref === v && onLayer(p.copper));
	return {
		label: `footprint ${v}`,
		parts: pads.flatMap(p => p.shapes.map(s => ({ s, centre: p.at, what: `pad ${p.id}` }))),
		centre: placement(fp).at,
		courtyard: courtyardBox(fp)?.box,
	};
}

const distanceQuery: QueryDef = {
	what: "distance",
	doc:
		'Distance between two things on the board: a pad "U6.75", a footprint "C29" (its pads; also the gap between two footprints\' courtyard bboxes), a point [x, y] or a net {net: "GND"} (its pads, tracks, vias and zone fills). ' +
		"Answers the copper edge-to-edge distance (0 when they touch or overlap) with the closest items and points, and the centre-to-centre distance (pad positions, footprint origins, points; for a net the centre of its closest item).",
	fields: {
		a: { t: "any", doc: "First: pad, footprint, point or {net}.", req: true },
		b: { t: "any", doc: "Second: pad, footprint, point or {net}.", req: true },
		layer: { t: "string", doc: "Only copper on this layer." },
	},
	async run({ project }, args) {
		const c = Copper.of(project.board());
		const layer = layerArg(c, args.layer);
		const a = target(c, args.a, layer, "a");
		const b = target(c, args.b, layer, "b");
		for (const t of [a, b]) if (!t.parts.length) throw new Error(`${t.label} has no copper${layer ? ` on ${layer}` : ""}`);
		let best: { d: number; pa: Pt; pb: Pt; x: Target["parts"][number]; y: Target["parts"][number] } | undefined;
		for (const x of a.parts) {
			for (const y of b.parts) {
				const r = shapeDistance(x.s, y.s, best ? best.d : Infinity);
				if (r && (!best || r.d < best.d)) best = { ...r, x, y };
			}
		}
		const hit = best!;
		const ca = a.centre ?? hit.x.centre ?? hit.pa;
		const cb = b.centre ?? hit.y.centre ?? hit.pb;
		const courtyardGap =
			a.courtyard && b.courtyard
				? r4(Math.hypot(Math.max(0, a.courtyard[0] - b.courtyard[2], b.courtyard[0] - a.courtyard[2]), Math.max(0, a.courtyard[1] - b.courtyard[3], b.courtyard[1] - a.courtyard[3])))
				: undefined;
		return {
			a: a.label,
			b: b.label,
			edge: r4(hit.d),
			...(hit.d === 0 ? { overlap: true } : {}),
			closest: { a: hit.x.what, b: hit.y.what, points: [rp(hit.pa), rp(hit.pb)] },
			centre: r4(Math.hypot(ca[0] - cb[0], ca[1] - cb[1])),
			centres: [rp(ca), rp(cb)],
			...(courtyardGap !== undefined ? { courtyard_gap: courtyardGap } : {}),
		};
	},
};

const graphicsQuery: QueryDef = {
	what: "board_graphics",
	doc:
		"Board-level drawings and texts (not those of footprints), as the board graphics ops take them. drawings: id, type line/rect/circle/arc/poly/curve, start, end, mid (arcs), center and radius (circles, arcs), points (polygons; a curve's four Bézier control points), width (stroke, mm), fill, layer, net (copper), locked. " +
		"texts: id, type text/textbox, text, at (a text box's top-left corner, with end or corners), size [width, height] of a glyph, thickness, rot, justify, layer, knockout, mirrored, hidden, locked.",
	fields: {
		layer: { t: "string", doc: 'Only this layer ("F.SilkS", "Edge.Cuts").' },
		kind: { t: "string", doc: "Only drawings of this type, or texts.", values: ["line", "rect", "circle", "arc", "poly", "curve", "text"] },
	},
	async run({ project }, args) {
		const c = Copper.of(project.board());
		const layer = args.layer as string | undefined;
		const kind = args.kind as string | undefined;
		const pts = (p: Pt | undefined) => (p ? rp(p) : undefined);
		const drawings =
			kind === "text"
				? []
				: c.drawings
						.filter(d => (!layer || d.layer === layer) && (!kind || d.type === kind))
						.map(d => ({
							id: short(d.uuid),
							type: d.type,
							start: pts(d.start),
							end: pts(d.end),
							mid: pts(d.mid),
							center: pts(d.center),
							radius: d.radius,
							points: d.points?.map(rp),
							width: d.width,
							fill: d.fill,
							layer: d.layer,
							net: d.net,
							...(d.locked ? { locked: true } : {}),
						}));
		const texts =
			kind && kind !== "text"
				? []
				: c.texts
						.filter(t => !layer || t.layer === layer)
						.map(t => ({
							id: short(t.uuid),
							type: t.type,
							text: t.text,
							at: rp(t.at),
							end: pts(t.end),
							corners: t.corners?.map(rp),
							size: t.size,
							thickness: t.thickness,
							rot: t.rot,
							justify: t.justify,
							layer: t.layer,
							knockout: t.knockout,
							mirrored: t.mirrored,
							...(t.hidden ? { hidden: true } : {}),
							...(t.locked ? { locked: true } : {}),
						}));
		return { drawings, texts };
	},
};

/** A stats value: KiCad writes "90,0000 mm" (decimal comma in some locales) or numbers. */
function statValue(v: unknown): unknown {
	if (typeof v !== "string") return v;
	const m = /^(-?[\d.,]+)\s*(mm²|mm|in²|in)?$/.exec(v.trim());
	if (!m) return v;
	const n = Number(m[1]!.includes(".") ? m[1]!.replace(/,/g, "") : m[1]!.replace(",", "."));
	return Number.isNaN(n) ? v : n;
}

const statsQuery: QueryDef = {
	what: "stats",
	doc:
		"KiCad's board statistics (kicad-cli pcb export stats): board size and area (mm, mm²), copper and footprint areas and densities per side, minimum track width and clearance (null when there are no tracks), minimum drill, thickness; pad counts by type, via counts by type, component counts (THT, SMD, unspecified per side) and the drill table [count, shape, x size, y size, plated, source, start layer, stop layer].",
	fields: {},
	async run({ project, analysis }) {
		project.board();
		const s = await cli.pcbStats(project.boardFile, analysis.runOpts);
		const board: Record<string, unknown> = {};
		for (const [k, v] of Object.entries((s.board ?? {}) as Record<string, unknown>)) {
			const x = statValue(v);
			// KiCad's "no tracks" is its integer maximum in mm
			board[k] = typeof x === "number" && x > 2000 && /^min_track/.test(k) ? null : x;
		}
		const comps = (s.components ?? {}) as Record<string, { front: number; back: number; total: number }>;
		const holes = (s.drill_holes ?? []) as Record<string, unknown>[];
		return {
			board,
			pads: s.pads,
			vias: s.vias,
			components: Object.fromEntries(Object.entries(comps).map(([k, v]) => [k, [v.front, v.back, v.total]])),
			components_columns: ["front", "back", "total"],
			drill_holes: holes.map(h => [h.count, h.shape, statValue(h.x_size), statValue(h.y_size), h.plated, h.source, h.start_layer, h.stop_layer]),
		};
	},
};

export const queries: QueryDef[] = [copperQuery, routingQuery, lengthsQuery, zonesQuery, freeSpaceQuery, distanceQuery, graphicsQuery, statsQuery];
