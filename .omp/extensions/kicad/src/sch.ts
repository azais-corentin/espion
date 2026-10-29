/**
 * One .kicad_sch file. Accessors read straight from the parsed tree, and edits change the tree
 * in place, so content the toolkit doesn't model survives untouched.
 */
import * as fs from "node:fs";
import { bbox, type Box, type Dir, key, type Mirror, onInterior, pinDir, type Pt, pt, samePt, type Seg, xform } from "./geom";
import {
	child,
	children,
	head,
	isList,
	L,
	type List,
	num,
	parse,
	print,
	str,
	text,
} from "./sexpr";

export const ITEM_KINDS = [
	"junction",
	"no_connect",
	"bus_entry",
	"wire",
	"bus",
	"label",
	"global_label",
	"hierarchical_label",
	"text",
	"text_box",
	"symbol",
	"sheet",
] as const;

export type LabelKind = "label" | "global_label" | "hierarchical_label";

export interface Pose {
	x: number;
	y: number;
	rot: number;
	mirror: Mirror;
}

export interface Pin {
	num: string;
	name: string;
	type: string;
	at: Pt;
	dir: Dir;
	hidden: boolean;
}

export interface Label {
	kind: LabelKind;
	name: string;
	at: Pt;
	rot: number;
	shape?: string;
	node: List;
}

export interface Wire extends Seg {
	node: List;
}

/** Sch.junctionContext: connection points by layer, keyed by `geom.key`. */
export interface JunctionContext {
	pins: Map<string, number>;
	entries: Map<string, number>;
	wires: Wire[];
	buses: Wire[];
}

export interface SheetPin {
	name: string;
	shape: string;
	at: Pt;
	rot: number;
	node: List;
}

export interface SheetBlock {
	name: string;
	file: string;
	uuid: string;
	at: Pt;
	size: Pt;
	pins: SheetPin[];
	node: List;
}

export const PAPER: Record<string, Pt> = {
	A5: [210, 148],
	A4: [297, 210],
	A3: [420, 297],
	A2: [594, 420],
	A1: [841, 594],
	A0: [1189, 841],
	A: [279.4, 215.9],
	B: [431.8, 279.4],
	C: [558.8, 431.8],
	D: [863.6, 558.8],
	E: [1117.6, 863.6],
};

export function prop(node: List, name: string): List | undefined {
	return children(node, "property").find(p => text(p) === name);
}

export function propValue(node: List, name: string): string | undefined {
	return text(prop(node, name), 2);
}

export function at(node: List): { p: Pt; rot: number } {
	const a = child(node, "at");
	if (!a) throw new Error(`no (at ...) in ${head(node)}`);
	return { p: pt(num(a, 1), num(a, 2)), rot: a.length > 3 ? num(a, 3) : 0 };
}

export function isHidden(node: List): boolean {
	const h = child(node, "hide");
	if (h) return text(h) !== "no";
	// older files write hide as a bare keyword inside effects
	const eff = child(node, "effects");
	return !!eff && eff.some(x => !isList(x) && x.v === "hide");
}

export function uuidOf(node: List): string {
	return text(child(node, "uuid")) ?? "";
}

export function newUuid(): string {
	return crypto.randomUUID();
}

/** Library symbol geometry, cached per lib_symbols entry. */
interface LibGeom {
	pins: { unit: number; style: number; num: string; name: string; type: string; lx: number; ly: number; ang: number; hidden: boolean }[];
	/** Graphic primitives; `pin` marks a pin's line (pin end to body), excluded from bodyShapes. */
	shapes: { unit: number; style: number; pin: boolean; pts: Pt[] }[];
}

const geomCache = new WeakMap<List, LibGeom>();

export function libGeom(sym: List): LibGeom {
	const hit = geomCache.get(sym);
	if (hit) return hit;
	const g: LibGeom = { pins: [], shapes: [] };
	for (const u of children(sym, "symbol")) {
		const m = /_(\d+)_(\d+)$/.exec(text(u) ?? "");
		const unit = m ? Number(m[1]) : 0;
		const style = m ? Number(m[2]) : 0;
		for (const item of u) {
			if (!isList(item)) continue;
			const k = head(item);
			const P = (n: List | undefined): Pt => [num(n, 1), num(n, 2)];
			if (k === "pin") {
				const a = child(item, "at")!;
				g.pins.push({
					unit,
					style,
					num: text(child(item, "number")) ?? "",
					name: text(child(item, "name")) ?? "",
					type: text(item, 1) ?? "",
					lx: num(a, 1),
					ly: num(a, 2),
					ang: a.length > 3 ? num(a, 3) : 0,
					hidden: isHidden(item),
				});
				const len = num(child(item, "length"));
				const rad = ((a.length > 3 ? num(a, 3) : 0) * Math.PI) / 180;
				g.shapes.push({ unit, style, pin: true, pts: [P(a), [num(a, 1) + len * Math.cos(rad), num(a, 2) + len * Math.sin(rad)]] });
			} else if (k === "rectangle") {
				g.shapes.push({ unit, style, pin: false, pts: [P(child(item, "start")), P(child(item, "end"))] });
			} else if (k === "polyline" || k === "bezier") {
				g.shapes.push({ unit, style, pin: false, pts: children(child(item, "pts") ?? [], "xy").map(P) });
			} else if (k === "circle") {
				const [cx, cy] = P(child(item, "center"));
				const r = num(child(item, "radius"));
				g.shapes.push({ unit, style, pin: false, pts: [[cx - r, cy - r], [cx + r, cy + r]] });
			} else if (k === "arc") {
				g.shapes.push({ unit, style, pin: false, pts: ["start", "mid", "end"].map(s => P(child(item, s))) });
			}
		}
	}
	geomCache.set(sym, g);
	return g;
}

const forUnit = (u: number, s: number, unit: number, style: number) => (u === 0 || u === unit) && (s === 0 || s === style);

export class Sch {
	constructor(
		public file: string,
		public tree: List,
	) {}

	static load(file: string): Sch {
		return new Sch(file, parse(fs.readFileSync(file, "utf8")));
	}

	static empty(file: string, paper: string, title?: string): Sch {
		const tree: List = L(
			"kicad_sch",
			L("version", 20260306),
			L("generator", str("eeschema")),
			L("generator_version", str("10.0")),
			L("uuid", str(newUuid())),
			L("paper", str(paper)),
		);
		if (title) tree.push(L("title_block", L("title", str(title))));
		tree.push(L("lib_symbols"), L("sheet_instances", L("path", str("/"), L("page", str("1")))), L("embedded_fonts", false));
		return new Sch(file, tree);
	}

	serialize(): string {
		return `${print(this.tree)}\n`;
	}

	get uuid(): string {
		return uuidOf(this.tree);
	}

	paper(): Pt {
		const p = child(this.tree, "paper");
		const name = text(p) ?? "A4";
		if (name === "User") return [num(p, 2), num(p, 3)];
		const size = PAPER[name];
		if (!size) throw new Error(`unknown paper size ${name}`);
		return child(p!, "portrait") || p!.some(x => !isList(x) && x.v === "portrait") ? [size[1], size[0]] : size;
	}

	items(kind: string): List[] {
		return children(this.tree, kind);
	}

	symbols(): List[] {
		return this.items("symbol");
	}

	libSymbols(): List {
		let ls = child(this.tree, "lib_symbols");
		if (!ls) {
			ls = L("lib_symbols");
			this.tree.splice(this.tree.findIndex(x => isList(x) && head(x) === "paper") + 1 || this.tree.length, 0, ls);
		}
		return ls;
	}

	libSymbol(libId: string): List | undefined {
		return children(this.libSymbols(), "symbol").find(s => text(s) === libId);
	}

	/** Add a top-level item before sheet_instances / embedded_fonts (KiCad re-sorts on save). */
	insert(item: List): List {
		const i = this.tree.findIndex(x => isList(x) && (head(x) === "sheet_instances" || head(x) === "embedded_fonts"));
		this.tree.splice(i >= 0 ? i : this.tree.length, 0, item);
		return item;
	}

	remove(item: List): void {
		const i = this.tree.indexOf(item);
		if (i < 0) throw new Error(`item not in ${this.file}`);
		this.tree.splice(i, 1);
	}

	// ---- symbols ---------------------------------------------------------------------------

	libId(sym: List): string {
		return text(child(sym, "lib_id")) ?? "";
	}

	lib(sym: List): List {
		const id = this.libId(sym);
		const l = this.libSymbol(id);
		if (!l) throw new Error(`lib symbol ${id} missing from lib_symbols of ${this.file}`);
		return l;
	}

	/** Reference of a symbol for one sheet instance path (falls back to the Reference field). */
	refOf(sym: List, path?: string): string {
		if (path) {
			for (const proj of children(child(sym, "instances") ?? [], "project")) {
				for (const p of children(proj, "path")) if (text(p) === path) return text(child(p, "reference")) ?? "";
			}
		}
		return propValue(sym, "Reference") ?? "";
	}

	setRef(sym: List, path: string, project: string, ref: string): void {
		prop(sym, "Reference")![2] = str(ref);
		let inst = child(sym, "instances");
		if (!inst) sym.push((inst = L("instances")));
		let proj = children(inst, "project").find(p => text(p) === project);
		if (!proj) inst.push((proj = L("project", str(project))));
		let p = children(proj, "path").find(x => text(x) === path);
		if (!p) proj.push((p = L("path", str(path), L("reference", str(ref)), L("unit", this.unit(sym)))));
		else p[p.findIndex(x => isList(x) && head(x) === "reference")] = L("reference", str(ref));
	}

	pose(sym: List): Pose {
		const { p, rot } = at(sym);
		const m = text(child(sym, "mirror"));
		return { x: p[0], y: p[1], rot: ((rot % 360) + 360) % 360, mirror: m === "x" || m === "y" ? m : undefined };
	}

	unit(sym: List): number {
		const u = child(sym, "unit");
		return u ? num(u) : 1;
	}

	bodyStyle(sym: List): number {
		const b = child(sym, "body_style") ?? child(sym, "convert");
		return b ? num(b) : 1;
	}

	pins(sym: List): Pin[] {
		const { x, y, rot, mirror } = this.pose(sym);
		const unit = this.unit(sym);
		const style = this.bodyStyle(sym);
		return libGeom(this.lib(sym))
			.pins.filter(p => forUnit(p.unit, p.style, unit, style))
			.map(p => ({
				num: p.num,
				name: p.name,
				type: p.type,
				at: xform(p.lx, p.ly, x, y, rot, mirror),
				dir: pinDir(p.ang, rot, mirror),
				hidden: p.hidden,
			}));
	}

	/** Boxes of the body graphics (pins excluded), in sheet coordinates. */
	bodyShapes(sym: List): Box[] {
		const { x, y, rot, mirror } = this.pose(sym);
		const unit = this.unit(sym);
		const style = this.bodyStyle(sym);
		const g = libGeom(this.lib(sym));
		const T = (q: Pt) => xform(q[0], q[1], x, y, rot, mirror);
		const out: Box[] = [];
		for (const s of g.shapes) {
			if (s.pin || !forUnit(s.unit, s.style, unit, style)) continue;
			if (s.pts.length > 2) for (let i = 1; i < s.pts.length; i++) out.push(bbox([T(s.pts[i - 1]!), T(s.pts[i]!)]));
			else out.push(bbox(s.pts.map(T)));
		}
		return out;
	}

	/** Bounding box of body + pins. */
	bodyBox(sym: List): Box {
		const { x, y, rot, mirror } = this.pose(sym);
		const unit = this.unit(sym);
		const style = this.bodyStyle(sym);
		const pts = libGeom(this.lib(sym))
			.shapes.filter(s => forUnit(s.unit, s.style, unit, style))
			.flatMap(s => s.pts.map(q => xform(q[0], q[1], x, y, rot, mirror)));
		return pts.length ? bbox(pts) : [x, y, x, y];
	}

	// ---- wiring ----------------------------------------------------------------------------

	private segs(kind: "wire" | "bus"): Wire[] {
		return this.items(kind).map(w => {
			const xy = children(child(w, "pts") ?? [], "xy");
			return { a: pt(num(xy[0], 1), num(xy[0], 2)), b: pt(num(xy[xy.length - 1], 1), num(xy[xy.length - 1], 2)), node: w };
		});
	}

	wires(): Wire[] {
		return this.segs("wire");
	}

	buses(): Wire[] {
		return this.segs("bus");
	}

	labels(): Label[] {
		const out: Label[] = [];
		for (const kind of ["label", "global_label", "hierarchical_label"] as const) {
			for (const node of this.items(kind)) {
				const { p, rot } = at(node);
				out.push({ kind, name: text(node) ?? "", at: p, rot: ((rot % 360) + 360) % 360, shape: text(child(node, "shape")), node });
			}
		}
		return out;
	}

	points(kind: "junction" | "no_connect"): { at: Pt; node: List }[] {
		return this.items(kind).map(node => ({ at: at(node).p, node }));
	}

	busEntries(): { a: Pt; b: Pt; node: List }[] {
		return this.items("bus_entry").map(node => {
			const { p } = at(node);
			const s = child(node, "size");
			return { a: p, b: pt(p[0] + num(s, 1), p[1] + num(s, 2)), node };
		});
	}

	sheets(): SheetBlock[] {
		return this.items("sheet").map(node => {
			const { p } = at(node);
			const s = child(node, "size");
			return {
				name: propValue(node, "Sheetname") ?? propValue(node, "Sheet name") ?? "",
				file: propValue(node, "Sheetfile") ?? propValue(node, "Sheet file") ?? "",
				uuid: uuidOf(node),
				at: p,
				size: pt(num(s, 1), num(s, 2)),
				pins: children(node, "pin").map(pn => {
					const a = at(pn);
					return { name: text(pn) ?? "", shape: text(pn, 2) ?? "passive", at: a.p, rot: a.rot, node: pn };
				}),
				node,
			};
		});
	}

	/**
	 * Every connection point on the sheet with what sits there, keyed by `geom.key`: "pin U4.7",
	 * "power GND" (a power symbol's pin, by net), "label X" / "global label X" / "hierarchical
	 * label X", "junction", "no-connect", "sheet pin Block/X", "bus entry". References are those
	 * of sheet instance `path` (default: the Reference field).
	 */
	connectionPoints(path?: string): Map<string, string[]> {
		const m = new Map<string, string[]>();
		const add = (p: Pt, what: string) => {
			const l = m.get(key(p));
			if (l) l.push(what);
			else m.set(key(p), [what]);
		};
		for (const s of this.symbols()) {
			const ref = this.refOf(s, path);
			const power = child(this.lib(s), "power") ? `power ${propValue(s, "Value") ?? ref}` : undefined;
			for (const p of this.pins(s)) add(p.at, power ?? `pin ${ref}.${p.num}`);
		}
		for (const l of this.labels()) add(l.at, `${l.kind.replace("_", " ")} ${l.name}`);
		for (const j of this.points("junction")) add(j.at, "junction");
		for (const j of this.points("no_connect")) add(j.at, "no-connect");
		for (const sh of this.sheets()) for (const p of sh.pins) add(p.at, `sheet pin ${sh.name}/${p.name}`);
		for (const e of this.busEntries()) {
			add(e.a, "bus entry");
			add(e.b, "bus entry");
		}
		return m;
	}

	/**
	 * What junctions count, precomputed once per sheet: pins on the wire layer (symbol pins,
	 * sheet-block pins, bus entry ends) and bus entry ends on the bus layer, keyed by `geom.key`.
	 */
	junctionContext(): JunctionContext {
		const pins = new Map<string, number>();
		const entries = new Map<string, number>();
		const add = (m: Map<string, number>, p: Pt) => m.set(key(p), (m.get(key(p)) ?? 0) + 1);
		for (const s of this.symbols()) for (const p of this.pins(s)) add(pins, p.at);
		for (const sh of this.sheets()) for (const p of sh.pins) add(pins, p.at);
		for (const e of this.busEntries()) {
			for (const p of [e.a, e.b]) {
				add(pins, p);
				add(entries, p);
			}
		}
		return { pins, entries, wires: this.wires(), buses: this.buses() };
	}

	/**
	 * Connections a junction at p joins, per layer (wires with pins, buses with bus entries; the
	 * larger counts): each segment end and pin once, a segment passing through twice. Below 3 the
	 * junction is redundant. The edit ops drop junctions and the style check reports them below
	 * this count; see `needsJunction` for where the ops add one.
	 */
	junctionDegree(p: Pt, c: JunctionContext = this.junctionContext()): number {
		const k = key(p);
		const along = (segs: readonly Seg[]) => segs.reduce((n, s) => n + (samePt(s.a, p) || samePt(s.b, p) ? 1 : onInterior(p, s) ? 2 : 0), 0);
		return Math.max((c.pins.get(k) ?? 0) + along(c.wires), (c.entries.get(k) ?? 0) + along(c.buses));
	}

	/**
	 * KiCad's IsExplicitJunctionNeeded: 3+ connections meet at p (junctionDegree) and no bus entry
	 * ends there (a bus entry joins the wire or bus it ends on without a dot).
	 */
	needsJunction(p: Pt, c: JunctionContext = this.junctionContext()): boolean {
		return !c.entries.has(key(p)) && this.junctionDegree(p, c) >= 3;
	}
}
