/**
 * Board zone ops: copper zones and rule areas (keepouts). Board millimetres, x right, y down; KiCad
 * canonical layer names.
 *
 * Written as KiCad 10 stores them (copper.ts): a zone's first `(polygon)` is its outline, further
 * ones are holes. Zones are written unfilled; the transaction has KiCad fill every zone and saves
 * its fills with the board, so the committed file carries them.
 */
import { Copper, type ConnectPads, type RuleArea, type Zone, type ZoneSettings } from "../copper";
import { boardOutline, isEdgeItem, pointInPolygon, shapeIn, shapePolygon } from "../edges";
import { mm } from "../fpgfx";
import { bbox, type Pt } from "../geom";
import type { Project } from "../project";
import { newUuid } from "../sch";
import { child, head, isList, L, type List, removeChild, setChild, str, Sym } from "../sexpr";
import type { Fields } from "../spec";
import type { OpContext, OpDef } from "../types";
import { boardSettings } from "./board";
import { copperLayer, fmtMm, short, xy } from "./copper";

// ---- outlines -------------------------------------------------------------------------------------

/** Twice the signed area (shoelace): positive for counter-clockwise in x right, y up terms. */
function signedArea2(p: Pt[]): number {
	let a = 0;
	for (let i = 0; i < p.length; i++) {
		const [x0, y0] = p[i]!;
		const [x1, y1] = p[(i + 1) % p.length]!;
		a += x0 * y1 - x1 * y0;
	}
	return a;
}

function properCross(a: Pt, b: Pt, c: Pt, d: Pt): boolean {
	const o = (p: Pt, q: Pt, r: Pt) => (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]);
	const [d1, d2, d3, d4] = [o(c, d, a), o(c, d, b), o(a, b, c), o(a, b, d)];
	return ((d1 > 0 && d2 < 0) || (d1 < 0 && d2 > 0)) && ((d3 > 0 && d4 < 0) || (d3 < 0 && d4 > 0));
}

/**
 * A simple polygon offset by d inward (negative d: outward), each vertex where its two edges'
 * offset lines meet. An offset that folds the polygon over itself is an error.
 */
export function offsetPolygon(poly: Pt[], d: number): Pt[] {
	const pts = poly.filter((p, i) => Math.hypot(p[0] - poly[(i + 1) % poly.length]![0], p[1] - poly[(i + 1) % poly.length]![1]) > 1e-9);
	if (d === 0) return pts;
	const s = signedArea2(pts) > 0 ? 1 : -1;
	const n = pts.length;
	const dirs = pts.map((p, i): Pt => {
		const q = pts[(i + 1) % n]!;
		const l = Math.hypot(q[0] - p[0], q[1] - p[1]);
		return [(q[0] - p[0]) / l, (q[1] - p[1]) / l];
	});
	const inward = dirs.map((u): Pt => [-u[1] * s, u[0] * s]);
	const out = pts.map((p, k): Pt => {
		const j = (k + n - 1) % n;
		const [u0, u1] = [dirs[j]!, dirs[k]!];
		const [n0, n1] = [inward[j]!, inward[k]!];
		const cr = u0[0] * u1[1] - u0[1] * u1[0];
		if (Math.abs(cr) < 1e-9) return [p[0] + n1[0] * d, p[1] + n1[1] * d];
		// line j: p + n0 d + t u0; line k: p + n1 d + r u1; solve for t
		const w: Pt = [(n1[0] - n0[0]) * d, (n1[1] - n0[1]) * d];
		const t = (w[0] * u1[1] - w[1] * u1[0]) / cr;
		return [p[0] + n0[0] * d + u0[0] * t, p[1] + n0[1] * d + u0[1] * t];
	});
	// an edge that turned round, or two edges that cross: the offset folded the outline
	const folded =
		out.some((a, i) => {
			const b = out[(i + 1) % n]!;
			return (b[0] - a[0]) * dirs[i]![0] + (b[1] - a[1]) * dirs[i]![1] <= 0;
		}) || out.some((a, i) => out.some((c, k) => k > i + 1 && !(i === 0 && k === n - 1) && properCross(a, out[(i + 1) % n]!, c, out[(k + 1) % n]!)));
	if (folded) throw new Error(`a margin of ${fmtMm(Math.abs(d))} mm folds the outline over itself: give a smaller margin or an explicit outline`);
	return out;
}

/** A zone or rule area outline argument: {points}, {rect} or "board" (the board outline with its cutouts as holes), inset by margin. */
function outlineArg(project: Project, v: unknown, margin: number): { outline: Pt[]; holes: Pt[][] } {
	if (v === "board") {
		const o = boardOutline(project.board().tree.filter((x): x is List => isList(x) && isEdgeItem(x)));
		if (!o.polygon) throw new Error('outline "board": the board has no closed outline (Edge.Cuts): set_board_edges draws one');
		return { outline: offsetPolygon(o.polygon, margin), holes: o.holes.map(h => offsetPolygon(h, -margin)) };
	}
	if (margin) throw new Error('margin applies to outline "board" only');
	return { outline: shapePolygon(shapeIn(v, "outline", false)), holes: [] };
}

// ---- zone nodes -----------------------------------------------------------------------------------

/** Layers as KiCad writes them: one `(layer)`, several `(layers)`, placed where the old ones were. */
function setLayers(z: List, layers: string[]): void {
	const node = layers.length === 1 ? L("layer", str(layers[0]!)) : L("layers", ...layers.map(str));
	const i = z.findIndex(c => isList(c) && (head(c) === "layer" || head(c) === "layers"));
	if (i >= 0) z.splice(i, 1, node);
	else z.splice(z.findIndex(c => isList(c) && head(c) === "uuid"), 0, node);
	for (let k = z.length - 1; k >= 0; k--) {
		const c = z[k]!;
		if (c !== node && isList(c) && (head(c) === "layer" || head(c) === "layers")) z.splice(k, 1);
	}
}

/** Remove a zone's fills: it changed, and KiCad refills it in the transaction. */
function dropFills(z: List): void {
	for (let k = z.length - 1; k >= 0; k--) {
		const c = z[k]!;
		if (isList(c) && (head(c) === "filled_polygon" || head(c) === "fill_segments")) z.splice(k, 1);
	}
	const fill = child(z, "fill");
	const yes = fill ? fill.findIndex(a => a instanceof Sym && a.v === "yes") : -1;
	if (yes >= 0) fill!.splice(yes, 1);
}

/** Replace the outline and holes. */
function setPolygons(z: List, outline: Pt[], holes: Pt[][]): void {
	for (let k = z.length - 1; k >= 0; k--) {
		const c = z[k]!;
		if (isList(c) && head(c) === "polygon") z.splice(k, 1);
	}
	z.push(...[outline, ...holes].map(p => L("polygon", L("pts", ...p.map(q => xy("xy", q))))));
}

const CONNECT: Record<ConnectPads, string | undefined> = { thermal: undefined, solid: "yes", none: "no", thru_hole_only: "thru_hole_only" };
const ISLAND = { always: 0, never: 1, below_area: 2 } as const;

/** Put a zone's settings into its node, keeping what it has besides them. */
function setSettings(z: List, s: ZoneSettings): void {
	const mode = CONNECT[s.connect_pads];
	setChild(z, L("connect_pads", ...(mode ? [new Sym(mode)] : []), L("clearance", mm(s.clearance))));
	setChild(z, L("min_thickness", mm(s.min_thickness)));
	const fill = child(z, "fill") ?? L("fill");
	if (!z.includes(fill)) z.push(fill);
	const put = (k: string, v: number | undefined) => (v === undefined ? removeChild(fill, k) : setChild(fill, L(k, mm(v))));
	removeChild(fill, "mode");
	if (s.fill_mode === "hatch") fill.splice(fill[1] instanceof Sym ? 2 : 1, 0, L("mode", "hatch"));
	put("thermal_gap", s.thermal_gap);
	put("thermal_bridge_width", s.thermal_spoke_width);
	setChild(fill, L("island_removal_mode", ISLAND[s.island_removal]));
	put("island_area_min", s.island_removal === "below_area" ? (s.island_min_area ?? 0) : undefined);
	put("hatch_thickness", s.fill_mode === "hatch" ? s.hatch_thickness : undefined);
	put("hatch_gap", s.fill_mode === "hatch" ? s.hatch_gap : undefined);
	put("hatch_orientation", s.fill_mode === "hatch" ? s.hatch_orientation : undefined);
}

/** pcbnew's new-zone settings: the project's zone defaults (Board Setup > Zones), else KiCad's own. */
function zoneDefaults(project: Project): ZoneSettings {
	const raw = boardSettings(project)?.defaults?.zones;
	const d = (typeof raw === "object" && raw !== null ? raw : {}) as Record<string, unknown>;
	const n = (k: string, dflt: number) => (typeof d[k] === "number" ? (d[k] as number) : dflt);
	const conn = n("pad_connection", 1);
	const island = n("remove_islands", 0);
	return {
		clearance: n("min_clearance", 0.5),
		min_thickness: n("min_thickness", 0.25),
		connect_pads: conn === 2 ? "solid" : conn === 0 ? "none" : conn === 3 ? "thru_hole_only" : "thermal",
		thermal_gap: n("thermal_relief_gap", 0.5),
		thermal_spoke_width: n("thermal_relief_spoke_width", 0.5),
		island_removal: island === 1 ? "never" : island === 2 ? "below_area" : "always",
		island_min_area: n("min_island_area", 10),
		fill_mode: n("fill_mode", 0) === 1 ? "hatch" : "solid",
		hatch_thickness: n("hatch_thickness", 1),
		hatch_gap: n("hatch_gap", 1.5),
		hatch_orientation: n("hatch_orientation", 0),
	};
}

// ---- arguments ------------------------------------------------------------------------------------

const SETTINGS: Fields = {
	priority: { t: "integer", doc: "fill priority (higher fills first; default 0)" },
	clearance: { t: "number", doc: "clearance to other nets' copper, mm" },
	min_thickness: { t: "number", doc: "minimum fill width, mm" },
	connect_pads: { t: "string", values: ["thermal", "solid", "none", "thru_hole_only"], doc: "pad connection (thru_hole_only: thermal reliefs for through-hole pads only)" },
	thermal_gap: { t: "number", doc: "thermal relief gap, mm" },
	thermal_spoke_width: { t: "number", doc: "thermal spoke width, mm" },
	island_removal: { t: "string", values: ["always", "never", "below_area"], doc: "remove isolated islands" },
	island_min_area: { t: "number", doc: "below_area: islands smaller than this go, mm²" },
	fill_mode: { t: "string", values: ["solid", "hatch"], doc: "solid or hatched fill (hatch settings from Board Setup > Zones)" },
};

const OUTLINE_DOC = '{points: [[x, y], …]}, {rect: {x, y, width, height}} or "board" (the board outline, its cutouts as holes)';

/** The given settings over `base`, checked. */
function settingsArg(args: Record<string, unknown>, base: ZoneSettings): ZoneSettings {
	const s: ZoneSettings = { ...base };
	for (const k of ["clearance", "min_thickness", "thermal_gap", "thermal_spoke_width", "island_min_area"] as const) {
		const v = args[k] as number | undefined;
		if (v === undefined) continue;
		if (!(v >= 0)) throw new Error(`${k} must be >= 0`);
		s[k] = v;
	}
	if (args.connect_pads !== undefined) s.connect_pads = args.connect_pads as ConnectPads;
	if (args.island_removal !== undefined) s.island_removal = args.island_removal as ZoneSettings["island_removal"];
	if (args.fill_mode !== undefined) s.fill_mode = args.fill_mode as ZoneSettings["fill_mode"];
	if (!(s.min_thickness > 0)) throw new Error("min_thickness must be positive");
	if (args.island_min_area !== undefined && s.island_removal !== "below_area") throw new Error('island_min_area applies with island_removal "below_area" only');
	return s;
}

function layersArg(c: Copper, v: unknown): string[] {
	const l = v as string[];
	if (!l.length) throw new Error("layers: give at least one copper layer");
	return [...new Set(l.map((x, i) => copperLayer(c, x, `layers[${i}]`)))].sort((a, b) => c.copperLayers.indexOf(a) - c.copperLayers.indexOf(b));
}

function checkName(c: Copper, name: string, self?: string): void {
	if (!name.trim()) throw new Error("name is empty");
	const other = [...c.zones, ...c.ruleAreas].find(z => z.name === name && z.uuid !== self);
	if (other) throw new Error(`a ${other.kind === "zone" ? "zone" : "rule area"} named "${name}" exists already (${short(other.uuid)})`);
}

/** The one board-level zone or rule area a {name | uuid | at} selector picks; one a footprint owns is refused. */
function pick<T extends Zone | RuleArea>(items: T[], args: Record<string, unknown>, what: string): T {
	const given = ["name", "uuid", "at"].filter(k => args[k] !== undefined);
	if (given.length !== 1) throw new Error(`give exactly one of name, uuid, at to pick the ${what}`);
	const list = (l: T[]) => l.map(z => `${z.name ? `"${z.name}" ` : ""}${short(z.uuid)}`).join(", ");
	let hits: T[];
	if (args.name !== undefined) hits = items.filter(z => z.name === args.name);
	else if (args.uuid !== undefined) hits = items.filter(z => z.uuid.startsWith(String(args.uuid)));
	else {
		const at = args.at as Pt;
		hits = items.filter(z => z.outline.length >= 3 && pointInPolygon(at, z.outline) && !z.holes.some(h => pointInPolygon(at, h)));
	}
	// by name or uuid a footprint's own is refused; by a point only when nothing of the board's is there
	const own = hits.filter(z => z.footprint);
	if (own.length && (args.at === undefined || own.length === hits.length)) {
		throw new Error(`the ${what} ${list(own.slice(0, 1))} belongs to footprint ${own[0]!.footprint}: change it in the footprint's library part`);
	}
	hits = hits.filter(z => !z.footprint);
	const board = items.filter(z => !z.footprint);
	if (!hits.length) throw new Error(`no ${what} matches${board.length ? ` (${what}s: ${list(board)})` : `: the board has no ${what}s of its own`}`);
	if (hits.length > 1) throw new Error(`several ${what}s match (${list(hits)}): give name or uuid`);
	return hits[0]!;
}

const SELECT: Fields = {
	name: { t: "string", doc: "pick by name" },
	uuid: { t: "string", doc: "pick by uuid (or its first characters, as queries show it)" },
	at: { t: "point", doc: "pick the one whose outline holds this point" },
};

function touchZone(ctx: OpContext, outline: Pt[], net: string | undefined, layers: string[]): void {
	ctx.project.touchBoard();
	const b = bbox(outline);
	ctx.touchBoard([b[0] - 1, b[1] - 1, b[2] + 1, b[3] + 1], undefined, { nets: net ? [net] : [], layers });
}

const describeSettings = (s: ZoneSettings) =>
	`clearance ${fmtMm(s.clearance)}, min thickness ${fmtMm(s.min_thickness)}, pads ${s.connect_pads}${s.connect_pads === "thermal" || s.connect_pads === "thru_hole_only" ? ` (gap ${fmtMm(s.thermal_gap)}, spokes ${fmtMm(s.thermal_spoke_width)})` : ""}, islands ${s.island_removal}${s.island_removal === "below_area" ? ` < ${fmtMm(s.island_min_area ?? 0)} mm²` : ""}, ${s.fill_mode} fill`;

// ---- zones ----------------------------------------------------------------------------------------

const addZone: OpDef = {
	op: "add_zone",
	group: "board",
	doc:
		"Add a copper zone (pour) of a net on one or more copper layers. Settings not given are pcbnew's new-zone defaults from Board Setup > Zones. " +
		'Outline "board" follows the board outline inset by `margin` (cutouts become holes, grown by it). The transaction has KiCad fill every zone and keeps the fills in the file.',
	fields: {
		net: { t: "string", req: true, doc: 'net, e.g. "GND"' },
		layers: { t: "strings", req: true, doc: 'copper layers, e.g. ["In1.Cu"]' },
		outline: { t: "any", req: true, doc: OUTLINE_DOC },
		margin: { t: "number", doc: 'outline "board": inset from the edge, mm (default 0)' },
		name: { t: "string", doc: "zone name (unique on the board)" },
		...SETTINGS,
	},
	apply(ctx, args) {
		const project = ctx.project;
		const board = project.board();
		const c = Copper.of(board);
		const net = c.net(String(args.net));
		const layers = layersArg(c, args.layers);
		const margin = (args.margin as number | undefined) ?? 0;
		if (margin < 0) throw new Error("margin must be >= 0");
		const { outline, holes } = outlineArg(project, args.outline, margin);
		const name = args.name as string | undefined;
		if (name !== undefined) checkName(c, name);
		const s = settingsArg(args, zoneDefaults(project));
		const priority = (args.priority as number | undefined) ?? 0;
		if (priority < 0) throw new Error("priority must be >= 0");
		const uuid = newUuid();
		const z: List = L("zone", L("net", str(net)), L("uuid", str(uuid)), ...(name ? [L("name", str(name))] : []), L("hatch", "edge", mm(0.5)), ...(priority ? [L("priority", priority)] : []));
		setLayers(z, layers);
		setSettings(z, s);
		setPolygons(z, outline, holes);
		board.add(z);
		touchZone(ctx, outline, net, layers);
		ctx.notes.push(
			`zone ${name ? `"${name}" ` : ""}${net} on ${layers.join(", ")}: ${outline.length}-point outline${holes.length ? ` with ${holes.length} hole${holes.length > 1 ? "s" : ""}` : ""}${args.outline === "board" ? ` (board outline${margin ? `, ${fmtMm(margin)} mm in` : ""})` : ""}, priority ${priority}; ${describeSettings(s)}; uuid ${short(uuid)}`,
		);
	},
};

const setZone: OpDef = {
	op: "set_zone",
	group: "board",
	doc: "Change a copper zone picked by name, uuid or a point inside it: its net, layers, outline, name, priority or fill settings; only the given keys change. Its old fill goes; the transaction refills it.",
	fields: {
		...SELECT,
		rename: { t: "string", doc: "new name" },
		net: { t: "string", doc: "new net" },
		layers: { t: "strings", doc: "new copper layers" },
		outline: { t: "any", doc: `new outline: ${OUTLINE_DOC}` },
		margin: { t: "number", doc: 'outline "board": inset from the edge, mm (default 0)' },
		...SETTINGS,
	},
	apply(ctx, args) {
		const project = ctx.project;
		const c = Copper.of(project.board());
		const zone = pick(
			c.zones.filter(z => !z.teardrop),
			args,
			"zone",
		);
		const z = zone.node;
		const changed: string[] = [];
		if (args.rename !== undefined) {
			checkName(c, String(args.rename), zone.uuid);
			setChild(z, L("name", str(String(args.rename))));
			changed.push(`name "${args.rename}"`);
		}
		let net = zone.net;
		if (args.net !== undefined) {
			net = c.net(String(args.net));
			setChild(z, L("net", str(net)));
			removeChild(z, "net_name");
			changed.push(`net ${net}`);
		}
		let layers = zone.layers;
		if (args.layers !== undefined) {
			layers = layersArg(c, args.layers);
			setLayers(z, layers);
			changed.push(`layers ${layers.join(", ")}`);
		}
		if (args.priority !== undefined) {
			const p = args.priority as number;
			if (p < 0) throw new Error("priority must be >= 0");
			if (p) setChild(z, L("priority", p));
			else removeChild(z, "priority");
			changed.push(`priority ${p}`);
		}
		const s = settingsArg(args, { ...zoneDefaults(project), ...zone.settings });
		if (Object.keys(SETTINGS).some(k => k !== "priority" && args[k] !== undefined)) {
			setSettings(z, s);
			changed.push(describeSettings(s));
		}
		let outline = zone.outline;
		if (args.outline !== undefined) {
			const o = outlineArg(project, args.outline, (args.margin as number | undefined) ?? 0);
			outline = o.outline;
			setPolygons(z, o.outline, o.holes);
			changed.push(`outline ${o.outline.length} points${o.holes.length ? `, ${o.holes.length} holes` : ""}`);
		} else if (args.margin !== undefined) throw new Error('margin goes with outline "board"');
		if (!changed.length) throw new Error("give something to change");
		dropFills(z);
		touchZone(ctx, [...zone.outline, ...outline], net, [...new Set([...zone.layers, ...layers])]);
		ctx.notes.push(`zone ${zone.name ? `"${zone.name}" ` : ""}${short(zone.uuid)}: ${changed.join("; ")}`);
	},
};

const removeZone: OpDef = {
	op: "remove_zone",
	group: "board",
	doc: "Delete a copper zone picked by name, uuid or a point inside it.",
	fields: SELECT,
	apply(ctx, args) {
		const board = ctx.project.board();
		const zone = pick(
			Copper.of(board).zones.filter(z => !z.teardrop),
			args,
			"zone",
		);
		board.remove(zone.node);
		touchZone(ctx, zone.outline, zone.net, zone.layers);
		ctx.notes.push(`removed zone ${zone.name ? `"${zone.name}" ` : ""}${zone.net} on ${zone.layers.join(", ")} (${short(zone.uuid)})`);
	},
};

// ---- rule areas -----------------------------------------------------------------------------------

const FORBID = ["tracks", "vias", "pads", "copperpour", "footprints"] as const;

function setKeepout(z: List, forbid: readonly string[]): void {
	setChild(z, L("keepout", ...FORBID.map(k => L(k, forbid.includes(k) ? "not_allowed" : "allowed"))));
}

const addRuleArea: OpDef = {
	op: "add_rule_area",
	group: "board",
	doc: "Add a rule area (keepout) on copper layers that forbids tracks, vias, pads, copper pours and/or footprints inside its outline; KiCad's DRC refuses an edit that puts forbidden items there.",
	fields: {
		name: { t: "string", req: true, doc: "rule area name (unique on the board)" },
		layers: { t: "strings", req: true, doc: 'copper layers, e.g. ["F.Cu", "B.Cu"]' },
		outline: { t: "any", req: true, doc: OUTLINE_DOC },
		margin: { t: "number", doc: 'outline "board": inset from the edge, mm (default 0)' },
		forbid: { t: "strings", req: true, values: FORBID, doc: "what may not be inside" },
	},
	apply(ctx, args) {
		const project = ctx.project;
		const board = project.board();
		const c = Copper.of(board);
		const name = String(args.name);
		checkName(c, name);
		const layers = layersArg(c, args.layers);
		const forbid = args.forbid as string[];
		if (!forbid.length) throw new Error("forbid: give at least one of tracks, vias, pads, copperpour, footprints");
		const { outline, holes } = outlineArg(project, args.outline, (args.margin as number | undefined) ?? 0);
		const uuid = newUuid();
		const z: List = L("zone", L("uuid", str(uuid)), L("name", str(name)), L("hatch", "edge", mm(0.5)), L("connect_pads", L("clearance", 0)), L("min_thickness", mm(0.25)));
		setLayers(z, layers);
		setKeepout(z, forbid);
		z.push(L("placement", L("enabled", false), L("sheetname", str(""))), L("fill", L("thermal_gap", mm(0.5)), L("thermal_bridge_width", mm(0.5))));
		setPolygons(z, outline, holes);
		board.add(z);
		touchZone(ctx, outline, undefined, layers);
		ctx.notes.push(`rule area "${name}" on ${layers.join(", ")}: no ${forbid.join(", ")}; ${outline.length}-point outline; uuid ${short(uuid)}`);
	},
};

const setRuleArea: OpDef = {
	op: "set_rule_area",
	group: "board",
	doc: "Change a rule area picked by name, uuid or a point inside it: its name, layers, outline or what it forbids; only the given keys change.",
	fields: {
		...SELECT,
		rename: { t: "string", doc: "new name" },
		layers: { t: "strings", doc: "new copper layers" },
		outline: { t: "any", doc: `new outline: ${OUTLINE_DOC}` },
		margin: { t: "number", doc: 'outline "board": inset from the edge, mm (default 0)' },
		forbid: { t: "strings", values: FORBID, doc: "what may not be inside (replaces the list)" },
	},
	apply(ctx, args) {
		const project = ctx.project;
		const c = Copper.of(project.board());
		const area = pick(c.ruleAreas, args, "rule area");
		const z = area.node;
		const changed: string[] = [];
		if (args.rename !== undefined) {
			checkName(c, String(args.rename), area.uuid);
			setChild(z, L("name", str(String(args.rename))));
			changed.push(`name "${args.rename}"`);
		}
		let layers = area.layers;
		if (args.layers !== undefined) {
			layers = layersArg(c, args.layers);
			setLayers(z, layers);
			changed.push(`layers ${layers.join(", ")}`);
		}
		if (args.forbid !== undefined) {
			const forbid = args.forbid as string[];
			if (!forbid.length) throw new Error("forbid: give at least one of tracks, vias, pads, copperpour, footprints (remove_rule_area deletes it)");
			setKeepout(z, forbid);
			changed.push(`no ${forbid.join(", ")}`);
		}
		let outline = area.outline;
		if (args.outline !== undefined) {
			const o = outlineArg(project, args.outline, (args.margin as number | undefined) ?? 0);
			outline = o.outline;
			setPolygons(z, o.outline, o.holes);
			changed.push(`outline ${o.outline.length} points`);
		} else if (args.margin !== undefined) throw new Error('margin goes with outline "board"');
		if (!changed.length) throw new Error("give something to change");
		touchZone(ctx, [...area.outline, ...outline], undefined, [...new Set([...area.layers, ...layers])]);
		ctx.notes.push(`rule area ${area.name ? `"${area.name}" ` : ""}${short(area.uuid)}: ${changed.join("; ")}`);
	},
};

const removeRuleArea: OpDef = {
	op: "remove_rule_area",
	group: "board",
	doc: "Delete a rule area picked by name, uuid or a point inside it.",
	fields: SELECT,
	apply(ctx, args) {
		const board = ctx.project.board();
		const area = pick(Copper.of(board).ruleAreas, args, "rule area");
		board.remove(area.node);
		touchZone(ctx, area.outline, undefined, area.layers);
		ctx.notes.push(`removed rule area ${area.name ? `"${area.name}" ` : ""}(${short(area.uuid)})`);
	},
};

export const ops: OpDef[] = [addZone, setZone, removeZone, addRuleArea, setRuleArea, removeRuleArea];
