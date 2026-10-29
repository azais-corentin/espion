/**
 * Schematic edit ops: symbols and their fields, wires, labels, power symbols, junctions,
 * no-connects, text and annotation, on one sheet at a time.
 *
 * New items take the shape KiCad 10 writes (see the espion sheets); field placement follows the
 * kisch.py autoplace (Reference/Value stacked on a pin-free side, text horizontal on the sheet).
 */
import { CHAR_W } from "../analysis";
import { nextRef, placementRefs, refPrefix } from "../annotate";
import { dragWireEnds, gridPt, sheetArg } from "../edit-util";
import { bbox, type Box, isOrthogonal, key, type Mirror, onInterior, type Pt, samePt, type Seg, touches, xform } from "../geom";
import { describeLibSymbol } from "../libs";
import type { FoundSymbol, SheetInst } from "../project";
import { at, isHidden, type LabelKind, libGeom, newUuid, prop, propValue, type Sch } from "../sch";
import { child, children, clone, head, isList, L, type List, print, removeChild, setChild, str, text } from "../sexpr";
import type { Field } from "../spec";
import type { OpContext, OpDef } from "../types";

// ---- argument specs ----------------------------------------------------------------------------

const SHEET: Field = { t: "string", req: true, doc: "sheet: name (e.g. \"Power\"), file name or name path" };
const AT: Field = { t: "point", req: true, doc: "position in mm, on the connection grid" };
const ROT: Field = { t: "integer", doc: "rotation in degrees: 0, 90, 180 or 270 (default 0)" };
const REF: Field = { t: "string", req: true, doc: "reference, e.g. \"R12\"" };
const SHAPES = ["input", "output", "bidirectional", "tri_state", "passive"] as const;
const ATTRS = ["dnp", "in_bom", "on_board", "exclude_from_sim"] as const;
/** Fields every symbol keeps (set them to "" instead of removing them). */
const MANDATORY: Record<string, true> = { Reference: true, Value: true, Footprint: true, Datasheet: true, Description: true };
const LABEL_NODE: Record<string, LabelKind> = { local: "label", global: "global_label", hierarchical: "hierarchical_label" };
const LABEL_KIND: Record<LabelKind, string> = { label: "local", global_label: "global", hierarchical_label: "hierarchical" };
/** Gap between the body and a field placed beside it, plus half a text height (kisch.py). */
const FIELD_GAP_X = 0.6 + 0.63;
const FIELD_GAP_Y = 1.905;
const FIELD_PITCH = 2.54;

// ---- small helpers -----------------------------------------------------------------------------

function rotArg(v: unknown): number {
	if (v === undefined) return 0;
	const r = Number(v);
	if (![0, 90, 180, 270].includes(r)) throw new Error(`rot must be 0, 90, 180 or 270, got ${v}`);
	return r;
}

function textArg(v: unknown, what: string): string {
	const s = String(v);
	if (!s.trim() || s !== s.trim() || /[\r\n]/.test(s)) throw new Error(`${what} ${JSON.stringify(s)} must be non-empty, one line, without surrounding spaces`);
	return s;
}

function fmtPt(p: Pt): string {
	return `(${p[0]}, ${p[1]})`;
}

function units(ctx: OpContext, ref: unknown): FoundSymbol[] {
	const hits = ctx.project.findUnits(String(ref));
	if (!hits.length) throw new Error(`no symbol with reference ${ref}`);
	return hits;
}

/** The one unit an op acts on: `unit` picks among the units of a multi-unit part. */
function oneUnit(ctx: OpContext, ref: unknown, unit: unknown): FoundSymbol {
	const hits = units(ctx, ref);
	if (unit !== undefined) {
		const hit = hits.find(h => h.sheet.sch.unit(h.sym) === unit);
		if (!hit) throw new Error(`${ref} has no unit ${unit}; units placed: ${hits.map(h => h.sheet.sch.unit(h.sym)).join(", ")}`);
		return hit;
	}
	if (hits.length > 1) throw new Error(`${ref} has ${hits.length} units placed; pass unit: one of ${hits.map(h => h.sheet.sch.unit(h.sym)).join(", ")}`);
	return hits[0]!;
}

/** Body, pins and visible fields of a symbol. */
function symBox(sch: Sch, sym: List): Box {
	const b = sch.bodyBox(sym);
	const pts: Pt[] = [[b[0], b[1]], [b[2], b[3]]];
	for (const p of children(sym, "property")) if (!isHidden(p)) pts.push(at(p).p);
	return bbox(pts);
}

/** Every instance of the sheet file `inst` belongs to (one per placement of a reused sheet). */
function instancesOf(ctx: OpContext, inst: SheetInst): SheetInst[] {
	return ctx.project.hierarchy().filter(i => i.file === inst.file);
}

/** Replace a list's `(at …)` keeping its place among the children. */
function setAt(node: List, x: number, y: number, rot: number): void {
	setChild(node, L("at", x, y, rot));
}

function justifyOf(field: List): string[] {
	const j = child(child(field, "effects") ?? [], "justify");
	return j ? j.slice(1).map((_, i) => text(j, i + 1) ?? "") : [];
}

function setJustify(field: List, just: string[]): void {
	const eff = child(field, "effects");
	if (!eff) return;
	removeChild(eff, "justify");
	if (just.length) eff.push(L("justify", ...just));
}

function setHidden(field: List, hidden: boolean): void {
	removeChild(field, "hide");
	if (!hidden) return;
	const i = field.findIndex(c => isList(c) && head(c) === "at");
	field.splice(i + 1, 0, L("hide", true));
}

function defaultEffects(): List {
	return L("effects", L("font", L("size", 1.27, 1.27)));
}

function fieldNode(name: string, value: string, p: Pt, hidden: boolean): List {
	const n = L("property", str(name), str(value), L("at", p[0], p[1], 0));
	if (hidden) n.push(L("hide", true));
	n.push(L("show_name", false), L("do_not_autoplace", false), defaultEffects());
	return n;
}

/** Flip left/right for symbols KiCad draws with mirrored justification (rot 90/180; mirror y = mirror x + 180). */
function justFor(just: string, rot: number, mirror: Mirror): string {
	const r = mirror === "y" ? (rot + 180) % 360 : rot % 360;
	if (r === 90 || r === 180) return just === "left" ? "right" : "left";
	return just;
}

/**
 * kisch.py `_autoplace`: visible fields (Reference, Value, then the others) stacked on the first
 * pin-free side of the body (right, top, bottom, left), text horizontal on the sheet; hidden
 * fields go to the symbol origin.
 */
function autoplace(sch: Sch, sym: List, grid: number): void {
	const { x, y, rot, mirror } = sch.pose(sym);
	const body = sch.bodyBox(sym);
	const sides = new Set(sch.pins(sym).map(p => p.dir));
	const props = children(sym, "property");
	const order = (n: string | undefined) => (n === "Reference" ? 0 : n === "Value" ? 1 : 2);
	const vis = props.filter(p => !isHidden(p)).sort((a, b) => order(text(a)) - order(text(b)));
	const n = vis.length;
	const ang = rot === 90 || rot === 270 ? 90 : 0;
	const side = (["R", "U", "D", "L"] as const).find(s => !sides.has(s)) ?? "U";
	const snap = (v: number) => Math.round(v / grid) * grid;
	const cx = (body[0] + body[2]) / 2;
	const cy = (body[1] + body[3]) / 2;
	vis.forEach((p, i) => {
		const stackY = cy + (i - (n - 1) / 2) * FIELD_PITCH;
		const [fx, fy, just] =
			side === "R"
				? [snap(body[2] + FIELD_GAP_X), stackY, "left"]
				: side === "L"
					? [snap(body[0] - FIELD_GAP_X), stackY, "right"]
					: side === "U"
						? [cx, body[1] - FIELD_GAP_Y - (n - 1 - i) * FIELD_PITCH, ""]
						: [cx, body[3] + FIELD_GAP_Y + i * FIELD_PITCH, ""];
		setAt(p, fx, fy, ang);
		setJustify(p, just ? [justFor(just, rot, mirror)] : []);
	});
	for (const p of props) if (isHidden(p)) setAt(p, x, y, at(p).rot);
	const fa = child(sym, "fields_autoplaced");
	if (fa) setChild(sym, L("fields_autoplaced", true));
}

/** The library's own field positions (power symbols: value above a rail, below a ground). */
function libFields(sch: Sch, sym: List): void {
	const { x, y, rot, mirror } = sch.pose(sym);
	const lib = sch.lib(sym);
	for (const p of children(sym, "property")) {
		const lp = prop(lib, text(p) ?? "");
		if (!lp) {
			setAt(p, x, y, at(p).rot);
			continue;
		}
		const la = at(lp);
		const q = xform(la.p[0], la.p[1], x, y, rot, mirror);
		setAt(p, q[0], q[1], la.rot);
		setJustify(p, justifyOf(lp));
	}
}

function placeFields(ctx: OpContext, sch: Sch, sym: List): void {
	if (child(sch.lib(sym), "power")) libFields(sch, sym);
	else autoplace(sch, sym, ctx.project.settings.grid);
}

function checkFootprint(ctx: OpContext, fp: string): void {
	if (fp) ctx.libs.footprintFile(fp);
}

/** Put a flattened library symbol into the sheet's lib_symbols, in name order. */
function insertLib(sch: Sch, flat: List): List {
	const ls = sch.libSymbols();
	const id = text(flat) ?? "";
	const i = ls.findIndex((c, k) => k > 0 && isList(c) && head(c) === "symbol" && (text(c) ?? "") > id);
	ls.splice(i > 0 ? i : ls.length, 0, flat);
	return flat;
}

/** The lib_symbols entry for `libId`, copied from the library when the sheet doesn't have it. */
function ensureLib(ctx: OpContext, sch: Sch, libId: string): List {
	return sch.libSymbol(libId) ?? insertLib(sch, ctx.libs.flatSymbol(libId));
}

/** A placed symbol's field shaped after the library's field `lp` (visibility, effects), at `p`. */
function libFieldNode(lp: List, value: string, p: Pt): List {
	const n = L("property", str(text(lp) ?? ""), str(value), L("at", p[0], p[1], at(lp).rot));
	if (isHidden(lp)) n.push(L("hide", true));
	n.push(clone(child(lp, "show_name") ?? L("show_name", false)));
	n.push(clone(child(lp, "do_not_autoplace") ?? L("do_not_autoplace", false)));
	n.push(clone(child(lp, "effects") ?? defaultEffects()));
	return n;
}

/** The `(pin "n" (uuid …))` entries of one unit, reusing the uuids in `keep` (by pin number). */
function pinNodes(lib: List, unit: number, keep: Map<string, string> = new Map()): List[] {
	const seen = new Set<string>();
	const out: List[] = [];
	for (const p of libGeom(lib).pins) {
		if ((p.unit !== 0 && p.unit !== unit) || (p.style !== 0 && p.style !== 1) || seen.has(p.num)) continue;
		seen.add(p.num);
		out.push(L("pin", str(p.num), L("uuid", str(keep.get(p.num) ?? newUuid()))));
	}
	return out;
}

function pinNote(sch: Sch, sym: List): string {
	return sch
		.pins(sym)
		.map(p => `${p.num}${p.name && p.name !== p.num && p.name !== "~" ? ` ${p.name}` : ""} ${fmtPt(p.at)} ${p.dir}`)
		.join(", ");
}

interface Placement {
	libId: string;
	at: Pt;
	rot: number;
	mirror: Mirror;
	unit: number;
	ref?: string;
	value?: string;
	footprint?: string;
	fields?: Record<string, string>;
}

/** Add a symbol instance shaped like KiCad 10's; returns it. */
function placeSymbol(ctx: OpContext, inst: SheetInst, o: Placement): List {
	const sch = inst.sch;
	const project = ctx.project;
	const lib = ensureLib(ctx, sch, o.libId);
	const info = describeLibSymbol(lib);
	if (o.unit < 1 || o.unit > info.units) throw new Error(`${o.libId} has units 1..${info.units}, not ${o.unit}`);
	const prefix = refPrefix(propValue(lib, "Reference") ?? "U");
	let ref = o.ref;
	if (ref !== undefined) {
		ref = textArg(ref, "ref");
		if (!ref.endsWith("?")) {
			const same = project.findUnits(ref);
			const clash = same.find(s => s.sheet.sch.libId(s.sym) !== o.libId || s.sheet.sch.unit(s.sym) === o.unit);
			if (clash) throw new Error(`reference ${ref} is already used (${clash.sheet.sch.libId(clash.sym)} unit ${clash.sheet.sch.unit(clash.sym)} on ${clash.sheet.name})`);
		}
	}
	const refs = placementRefs(project, prefix, inst, new Set(), ref);
	if (o.footprint !== undefined) checkFootprint(ctx, o.footprint);

	const [x, y] = o.at;
	const node = L("symbol", L("lib_id", str(o.libId)), L("at", x, y, o.rot));
	if (o.mirror) node.push(L("mirror", o.mirror));
	node.push(L("unit", o.unit), L("body_style", 1));
	for (const f of ["exclude_from_sim", "in_bom", "on_board", "in_pos_files"]) node.push(clone(child(lib, f) ?? L(f, f !== "exclude_from_sim")));
	node.push(L("dnp", false), L("fields_autoplaced", true), L("uuid", str(newUuid())));
	const given: Record<string, string | undefined> = { Reference: refs[0]!.ref, Value: o.value, Footprint: o.footprint };
	for (const lp of children(lib, "property")) {
		const name = text(lp) ?? "";
		if (name.startsWith("ki_")) continue;
		node.push(libFieldNode(lp, given[name] ?? o.fields?.[name] ?? text(lp, 2) ?? "", o.at));
	}
	for (const [name, value] of Object.entries(o.fields ?? {})) {
		if (name === "Reference") throw new Error("fields: set the reference with ref, not fields");
		if (!prop(node, name)) node.push(fieldNode(name, value, o.at, true));
	}
	node.push(...pinNodes(lib, o.unit));
	node.push(
		L(
			"instances",
			L("project", str(project.name), ...refs.map(r => L("path", str(r.inst.path), L("reference", str(r.ref)), L("unit", o.unit)))),
		),
	);
	sch.insert(node);
	placeFields(ctx, sch, node);
	project.touch(sch);
	ctx.touch(inst, symBox(sch, node));
	return node;
}

function junctionNode(p: Pt): List {
	return L("junction", L("at", p[0], p[1]), L("diameter", 0), L("color", 0, 0, 0, 0), L("uuid", str(newUuid())));
}

/**
 * kisch.py `auto_junctions`, limited to the points the new wires touch: a junction where 3+
 * connections meet (Sch.junctionDegree) on a new wire's end, or where a wire end or pin lands on
 * a new wire's interior.
 */
function autoJunctions(sch: Sch, added: readonly Seg[]): Pt[] {
	const c = sch.junctionContext();
	const have = new Set(sch.points("junction").map(j => key(j.at)));
	const cands = new Map<string, Pt>();
	const onNew = (p: Pt) => added.some(s => onInterior(p, s));
	for (const s of added) for (const p of [s.a, s.b]) cands.set(key(p), p);
	for (const w of c.wires) for (const p of [w.a, w.b]) if (onNew(p)) cands.set(key(p), p);
	for (const s of sch.symbols()) for (const p of sch.pins(s)) if (onNew(p.at)) cands.set(key(p.at), p.at);
	for (const b of sch.sheets()) for (const p of b.pins) if (onNew(p.at)) cands.set(key(p.at), p.at);
	const out: Pt[] = [];
	for (const [k, p] of cands) {
		if (have.has(k) || sch.junctionDegree(p, c) < 3) continue;
		sch.insert(junctionNode(p));
		out.push(p);
	}
	return out;
}

function findPoint(sch: Sch, kind: "junction" | "no_connect", p: Pt): List | undefined {
	return sch.points(kind).find(j => samePt(j.at, p))?.node;
}

/**
 * Junctions on these segments (ends or interior, e.g. the wires before an edit removed or moved
 * them) or at these points that no longer join 3+ connections are removed; returns where.
 */
function dropStaleJunctions(sch: Sch, segs: readonly Seg[], pts: readonly Pt[] = []): Pt[] {
	const c = sch.junctionContext();
	const out: Pt[] = [];
	for (const j of sch.points("junction")) {
		if (!segs.some(s => touches(j.at, s)) && !pts.some(p => samePt(p, j.at))) continue;
		if (sch.junctionDegree(j.at, c) >= 3) continue;
		sch.remove(j.node);
		out.push(j.at);
	}
	return out;
}

function droppedNote(pts: readonly Pt[]): string {
	return pts.length ? `junction(s) no longer needed removed at ${pts.map(fmtPt).join(", ")}` : "";
}

/**
 * Pins, labels, sheet pins and wire ends on `seg` that connect to nothing any more (after an edit
 * removed wiring there), each with its position; references as on sheet instance `inst`. Points
 * with a no-connect flag are left out.
 */
function leftOpen(inst: SheetInst, seg: Seg): string[] {
	const wires = inst.sch.wires();
	const conn = inst.sch.connectionPoints(inst.path);
	const pts = new Map<string, Pt>();
	for (const k of conn.keys()) {
		const p = k.split(",").map(Number) as [number, number];
		if (touches(p, seg)) pts.set(k, p);
	}
	for (const w of wires) for (const e of [w.a, w.b]) if (touches(e, seg)) pts.set(key(e), e);
	const out: string[] = [];
	for (const [k, p] of pts) {
		const items = conn.get(k) ?? [];
		if (items.includes("no-connect")) continue;
		const things = items.filter(x => x !== "junction");
		const ends = wires.filter(w => samePt(w.a, p) || samePt(w.b, p)).length;
		const through = wires.some(w => onInterior(p, w));
		if (!through && things.length + ends === 1) out.push(`${things[0] ?? "wire end"} ${fmtPt(p)}`);
	}
	return out;
}

function labelNode(kind: LabelKind, name: string, p: Pt, rot: number, shape: string): List {
	const right = rot === 180 || rot === 270;
	const font = L("font", L("size", 1.27, 1.27));
	if (kind === "label") {
		return L("label", str(name), L("at", p[0], p[1], rot), L("effects", font, L("justify", right ? "right" : "left", "bottom")), L("uuid", str(newUuid())));
	}
	const just = L("justify", right ? "right" : "left");
	if (kind === "hierarchical_label") {
		return L("hierarchical_label", str(name), L("shape", shape), L("at", p[0], p[1], rot), L("effects", font, just), L("uuid", str(newUuid())));
	}
	// the intersheet reference sits past the end of the flag, in the direction the text runs
	const len = name.length * CHAR_W + 2 * 1.27;
	const dir: Record<number, Pt> = { 0: [1, 0], 90: [0, -1], 180: [-1, 0], 270: [0, 1] };
	const d = dir[rot]!;
	return L(
		"global_label",
		str(name),
		L("shape", shape),
		L("at", p[0], p[1], rot),
		L("fields_autoplaced", true),
		L("effects", font, just),
		L("uuid", str(newUuid())),
		L(
			"property",
			str("Intersheetrefs"),
			str("${INTERSHEET_REFS}"),
			L("at", p[0] + d[0] * len, p[1] + d[1] * len, rot),
			L("hide", true),
			L("show_name", false),
			L("do_not_autoplace", false),
			L("effects", clone(font), clone(just)),
		),
	);
}

// ---- ops ---------------------------------------------------------------------------------------

const setFields: OpDef = {
	op: "set_fields",
	group: "schematic",
	doc: "Set, add or remove (null) fields on every unit of a symbol. New fields are hidden at the symbol origin unless listed in show; show/hide toggle visibility of any field.",
	fields: {
		ref: REF,
		fields: { t: "object", req: true, doc: "{Name: value | null}; null removes the field (Reference: use rename_ref)" },
		show: { t: "strings", doc: "fields to make visible (placed below the lowest visible field)" },
		hide: { t: "strings", doc: "fields to hide" },
	},
	apply(ctx, args) {
		const fields = args.fields as Record<string, unknown>;
		const show = (args.show as string[] | undefined) ?? [];
		const hide = (args.hide as string[] | undefined) ?? [];
		const both = show.filter(s => hide.includes(s));
		if (both.length) throw new Error(`${both.join(", ")} both in show and hide`);
		for (const [name, v] of Object.entries(fields)) {
			if (name === "Reference") throw new Error("the reference can't be set here; use rename_ref");
			if (v !== null && typeof v !== "string") throw new Error(`fields.${name} must be a string or null, got ${JSON.stringify(v)}`);
			if (v === null && MANDATORY[name]) throw new Error(`${name} is a mandatory field; set it to "" instead of removing it`);
			if (name === "Footprint" && typeof v === "string") checkFootprint(ctx, v);
		}
		for (const { sheet, sym } of units(ctx, args.ref)) {
			const sch = sheet.sch;
			const origin = at(sym).p;
			for (const [name, v] of Object.entries(fields)) {
				const p = prop(sym, name);
				if (v === null) {
					if (!p) throw new Error(`${args.ref} has no field ${name}`);
					sym.splice(sym.indexOf(p), 1);
				} else if (p) p[2] = str(v as string);
				else sym.push(fieldNode(name, v as string, origin, true));
			}
			for (const name of [...show, ...hide]) {
				if (!prop(sym, name)) throw new Error(`${args.ref} has no field ${name}; fields: ${children(sym, "property").map(p => text(p)).join(", ")}`);
			}
			for (const name of hide) setHidden(prop(sym, name)!, true);
			for (const name of show) {
				const p = prop(sym, name)!;
				if (!isHidden(p)) continue;
				const vis = children(sym, "property").filter(q => !isHidden(q));
				setHidden(p, false);
				if (!vis.length) {
					autoplace(sch, sym, ctx.project.settings.grid);
					continue;
				}
				const low = vis.reduce((a, b) => (at(b).p[1] > at(a).p[1] ? b : a));
				const la = at(low);
				setAt(p, la.p[0], la.p[1] + FIELD_PITCH, la.rot);
				setJustify(p, justifyOf(low));
			}
			ctx.project.touch(sch);
			ctx.touch(sheet, symBox(sch, sym));
		}
		const what = Object.entries(fields).map(([k, v]) => (v === null ? `-${k}` : `${k}=${JSON.stringify(v)}`));
		if (show.length) what.push(`shown ${show.join(", ")}`);
		if (hide.length) what.push(`hidden ${hide.join(", ")}`);
		ctx.notes.push(`${args.ref}: ${what.join(", ")}`);
	},
};

const renameRef: OpDef = {
	op: "rename_ref",
	group: "schematic",
	doc: "Change a symbol's reference (all units); refuses a reference already in use.",
	fields: { ref: REF, to: { t: "string", req: true, doc: "new reference" } },
	apply(ctx, args) {
		const to = textArg(args.to, "to");
		if (ctx.project.allRefs().has(to)) throw new Error(`reference ${to} is already used`);
		for (const { sheet, sym } of units(ctx, args.ref)) {
			sheet.sch.setRef(sym, sheet.path, ctx.project.name, to);
			ctx.project.touch(sheet.sch);
			ctx.touch(sheet, symBox(sheet.sch, sym));
		}
		ctx.notes.push(`${args.ref} → ${to}`);
	},
};

const setAttrs: OpDef = {
	op: "set_attrs",
	group: "schematic",
	doc: "Set symbol attributes on every unit: dnp (do not populate), in_bom, on_board, exclude_from_sim.",
	fields: {
		ref: REF,
		dnp: { t: "boolean", doc: "do not populate" },
		in_bom: { t: "boolean", doc: "listed in the BOM" },
		on_board: { t: "boolean", doc: "exported to the PCB" },
		exclude_from_sim: { t: "boolean", doc: "excluded from simulation" },
	},
	apply(ctx, args) {
		const set = ATTRS.filter(a => args[a] !== undefined);
		if (!set.length) throw new Error(`give at least one of ${ATTRS.join(", ")}`);
		for (const { sheet, sym } of units(ctx, args.ref)) {
			for (const a of set) setChild(sym, L(a, Boolean(args[a])));
			ctx.project.touch(sheet.sch);
			ctx.touch(sheet, symBox(sheet.sch, sym));
		}
		ctx.notes.push(`${args.ref}: ${set.map(a => `${a}=${args[a] ? "yes" : "no"}`).join(", ")}`);
	},
};

const addSymbol: OpDef = {
	op: "add_symbol",
	group: "schematic",
	doc: "Place a library symbol (origin at `at`). The reference defaults to the next free number (the sheet's refBase block, a new sheet getting the next free block; else project-wide; the next_ref query previews it); Reference/Value are auto-placed on a pin-free side. The note lists the pin positions and the side each wire leaves. To replace a placed part by another symbol (or refresh it from the library) use change_symbol, not remove + add.",
	fields: {
		sheet: SHEET,
		lib_id: { t: "string", req: true, doc: "\"Library:Symbol\"" },
		at: { ...AT, doc: "symbol origin in mm, on the connection grid" },
		rot: ROT,
		mirror: { t: "string", values: ["x", "y"], doc: "mirror about the x or y axis" },
		unit: { t: "integer", doc: "unit of a multi-unit part (default 1)" },
		ref: { t: "string", doc: "reference (default: next free); an existing reference adds another unit of that part" },
		value: { t: "string", doc: "Value (default: the library's)" },
		footprint: { t: "string", doc: "\"Library:Footprint\" (default: the library's)" },
		fields: { t: "object", doc: "{Name: value} for further fields (new ones hidden)" },
	},
	apply(ctx, args) {
		const inst = sheetArg(ctx, args.sheet);
		const fields = (args.fields as Record<string, unknown> | undefined) ?? {};
		for (const [k, v] of Object.entries(fields)) if (typeof v !== "string") throw new Error(`fields.${k} must be a string`);
		const node = placeSymbol(ctx, inst, {
			libId: String(args.lib_id),
			at: gridPt(ctx, args.at, "at"),
			rot: rotArg(args.rot),
			mirror: args.mirror as Mirror,
			unit: (args.unit as number | undefined) ?? 1,
			ref: args.ref as string | undefined,
			value: args.value as string | undefined,
			footprint: args.footprint as string | undefined,
			fields: fields as Record<string, string>,
		});
		const sch = inst.sch;
		ctx.notes.push(
			`added ${sch.refOf(node, inst.path)} (${args.lib_id}, ${propValue(node, "Value")}) on ${inst.name} at ${fmtPt(at(node).p)}; pins: ${pinNote(sch, node)}`,
		);
	},
};

const removeSymbol: OpDef = {
	op: "remove_symbol",
	group: "schematic",
	doc: "Delete a symbol (all units, or one unit) and the no-connects on its pins. Wires on its pins stay; the note says how many wire ends were left open. To swap a part for another library symbol keeping its place and sourcing fields, use change_symbol.",
	fields: { ref: REF, unit: { t: "integer", doc: "remove only this unit" } },
	apply(ctx, args) {
		const hits = args.unit === undefined ? units(ctx, args.ref) : [oneUnit(ctx, args.ref, args.unit)];
		let open = 0;
		let ncs = 0;
		const dropped: Pt[] = [];
		for (const { sheet, sym } of hits) {
			const sch = sheet.sch;
			const box = symBox(sch, sym);
			const pinPts = sch.pins(sym).map(p => p.at);
			const pins = new Set(pinPts.map(key));
			const libId = sch.libId(sym);
			sch.remove(sym);
			for (const w of sch.wires()) for (const e of [w.a, w.b]) if (pins.has(key(e))) open++;
			for (const nc of sch.points("no_connect")) {
				if (!pins.has(key(nc.at))) continue;
				sch.remove(nc.node);
				ncs++;
			}
			if (!sch.symbols().some(s => sch.libId(s) === libId)) {
				const lib = sch.libSymbol(libId);
				if (lib) sch.libSymbols().splice(sch.libSymbols().indexOf(lib), 1);
			}
			dropped.push(...dropStaleJunctions(sch, [], pinPts));
			ctx.project.touch(sch);
			ctx.touch(sheet, box);
		}
		const extra = [open ? `${open} wire end(s) left open at its pins` : "", ncs ? `${ncs} no-connect(s) on its pins removed` : "", droppedNote(dropped)].filter(Boolean);
		ctx.notes.push(`removed ${args.ref}${args.unit !== undefined ? ` unit ${args.unit}` : ""}${extra.length ? `; ${extra.join("; ")}` : ""}`);
	},
};

/** What change_symbol reports about the wiring at the changed symbols' pins (nothing is rewired). */
interface PinReport {
	offPin: string[];
	bare: string[];
	midWire: string[];
	oldPts: Pt[];
}

/**
 * Wiring left at the old pin points of `before` that no longer meets a pin, and pins of
 * `syms` (as now drawn) with nothing on them or sitting on the middle of a wire.
 */
function pinReport(sch: Sch, inst: SheetInst, before: Map<List, { ref: string; pins: { num: string; at: Pt }[] }>, syms: List[]): PinReport {
	const conn = sch.connectionPoints(inst.path);
	const wires = sch.wires();
	const pinKeys = new Set<string>();
	for (const s of sch.symbols()) for (const p of sch.pins(s)) pinKeys.add(key(p.at));
	for (const b of sch.sheets()) for (const p of b.pins) pinKeys.add(key(p.at));
	const r: PinReport = { offPin: [], bare: [], midWire: [], oldPts: [] };
	const seen = new Set<string>();
	for (const { ref, pins } of before.values()) {
		for (const pin of pins) {
			const k = key(pin.at);
			if (seen.has(k)) continue;
			seen.add(k);
			r.oldPts.push(pin.at);
			if (pinKeys.has(k)) continue;
			const here = [
				...wires.filter(w => samePt(w.a, pin.at) || samePt(w.b, pin.at)).map(() => "wire end"),
				...(conn.get(k) ?? []).filter(x => x !== "junction"),
			];
			for (const h of here) r.offPin.push(`${h} ${fmtPt(pin.at)} (was ${ref}.${pin.num})`);
		}
	}
	for (const s of syms) {
		const ref = sch.refOf(s, inst.path);
		const pins = sch.pins(s);
		for (const pin of pins) {
			if (pin.hidden) continue;
			const name = pin.name && pin.name !== pin.num && pin.name !== "~" ? ` ${pin.name}` : "";
			// anything at its point besides the symbol's own pins there (stacked pins), or a wire through it
			const own = pins.filter(q => samePt(q.at, pin.at)).length;
			const others = (conn.get(key(pin.at)) ?? []).filter(x => x !== "junction").length - own;
			if (!others && !wires.some(w => touches(pin.at, w))) r.bare.push(`${ref}.${pin.num}${name} ${fmtPt(pin.at)}`);
			if (wires.some(w => onInterior(pin.at, w))) r.midWire.push(`${ref}.${pin.num}${name} ${fmtPt(pin.at)}`);
		}
	}
	return r;
}

const changeSymbol: OpDef = {
	op: "change_symbol",
	group: "schematic",
	doc:
		"Swap a placed part for another library symbol, or (without lib_id) refresh it from the current library: KiCad's Change / Update Symbol, on every unit. " +
		"Reference, position, rotation, mirror, unit, attributes (dnp, in_bom, …) and the fields the new library part doesn't define (Manufacturer, MPN, LCSC, …) stay. " +
		"On a swap, Value, Footprint, Datasheet, Description and any other field the new part defines come from it (one it leaves empty keeps the placed value); " +
		"on a refresh a field follows the library only where it still held the old library's value. value/footprint/fields override. Fields are re-placed. " +
		"The sheet's cached copy of the symbol is replaced, so other symbols of that lib_id on the sheet are refreshed too (the note names them). " +
		"Nothing is rewired: the note lists wire ends, labels and no-connects left off a pin, pins with nothing on them, and the new pin positions.",
	fields: {
		ref: REF,
		lib_id: { t: "string", doc: "\"Library:Symbol\" to swap to (default: the placed lib_id, refreshed from the library)" },
		unit: { t: "integer", doc: "unit of the new part, for a part with one unit placed (default: unchanged)" },
		value: { t: "string", doc: "Value (default: see above)" },
		footprint: { t: "string", doc: "\"Library:Footprint\" (default: see above)" },
		fields: { t: "object", doc: "{Name: value | null}: set or add fields (new ones hidden), null removes one (Reference: use rename_ref)" },
	},
	apply(ctx, args) {
		const ref = String(args.ref);
		const hits = units(ctx, ref);
		// a reused sheet lists the same symbol once per instance
		const placed = hits.filter((h, i) => hits.findIndex(o => o.sym === h.sym) === i);
		const oldId = placed[0]!.sheet.sch.libId(placed[0]!.sym);
		const newId = args.lib_id === undefined ? oldId : textArg(args.lib_id, "lib_id");
		const refresh = newId === oldId;
		const fresh = ctx.libs.flatSymbol(newId);
		const nUnits = describeLibSymbol(fresh).units;
		const unitArg = args.unit as number | undefined;
		if (unitArg !== undefined) {
			if (placed.length > 1) throw new Error(`${ref} has ${placed.length} units placed; unit only applies to a part with one unit placed`);
			if (unitArg < 1 || unitArg > nUnits) throw new Error(`${newId} has units 1..${nUnits}, not ${unitArg}`);
		} else {
			const extra = placed.map(h => h.sheet.sch.unit(h.sym)).filter(u => u > nUnits);
			if (extra.length) throw new Error(`${newId} has ${nUnits} unit(s) but ${ref} has unit(s) ${extra.join(", ")} placed; remove those first (remove_symbol with unit)`);
		}
		const over: Record<string, string | null> = {};
		for (const [name, v] of Object.entries((args.fields as Record<string, unknown> | undefined) ?? {})) {
			if (name === "Reference") throw new Error("the reference can't be set here; use rename_ref");
			if (v !== null && typeof v !== "string") throw new Error(`fields.${name} must be a string or null, got ${JSON.stringify(v)}`);
			if (v === null && MANDATORY[name]) throw new Error(`${name} is a mandatory field; set it to "" instead of removing it`);
			over[name] = v;
		}
		for (const [arg, name] of [["value", "Value"], ["footprint", "Footprint"]] as const) {
			if (args[arg] === undefined) continue;
			if (name in over) throw new Error(`${name} given both as ${arg} and in fields`);
			over[name] = String(args[arg]);
		}
		if (typeof over.Footprint === "string") checkFootprint(ctx, over.Footprint);

		const bySheet = new Map<string, FoundSymbol[]>();
		for (const h of placed) bySheet.set(h.sheet.file, [...(bySheet.get(h.sheet.file) ?? []), h]);
		const changes = new Set<string>();
		const kept = new Set<string>();
		const notes: string[] = [];
		const pinLists: string[] = [];
		for (const list of bySheet.values()) {
			const inst = list[0]!.sheet;
			const sch = inst.sch;
			const targets = list.map(h => h.sym);
			const cached = sch.libSymbol(newId);
			const libCopy = clone(fresh);
			// symbols sharing a cache entry that changes are redrawn from the new copy too
			const sharers = cached && print(cached) !== print(libCopy) ? sch.symbols().filter(s => !targets.includes(s) && sch.libId(s) === newId) : [];
			const affected = [...targets, ...sharers];
			const before = new Map(affected.map(s => [s, { ref: sch.refOf(s, inst.path), pins: sch.pins(s), box: symBox(sch, s) }]));
			const oldLibs = new Map(targets.map(s => [s, sch.lib(s)]));
			if (cached) sch.libSymbols().splice(sch.libSymbols().indexOf(cached), 1, libCopy);
			else insertLib(sch, libCopy);

			for (const sym of targets) {
				setChild(sym, L("lib_id", str(newId)));
				if (unitArg !== undefined) {
					setChild(sym, L("unit", unitArg));
					for (const proj of children(child(sym, "instances") ?? [], "project")) for (const p of children(proj, "path")) setChild(p, L("unit", unitArg));
				}
				const keep = new Map(children(sym, "pin").map(n => [text(n) ?? "", text(child(n, "uuid")) ?? newUuid()]));
				for (const n of children(sym, "pin")) sym.splice(sym.indexOf(n), 1);
				const pinsAt = sym.findIndex(c => isList(c) && head(c) === "instances");
				sym.splice(pinsAt >= 0 ? pinsAt : sym.length, 0, ...pinNodes(libCopy, sch.unit(sym), keep));

				const origin = at(sym).p;
				const addField = (n: List) => {
					const last = children(sym, "property").at(-1);
					sym.splice(last ? sym.indexOf(last) + 1 : sym.length, 0, n);
				};
				const oldVals = new Map(children(oldLibs.get(sym)!, "property").map(p => [text(p) ?? "", text(p, 2) ?? ""]));
				const libProps = new Map(children(libCopy, "property").map(p => [text(p) ?? "", p]));
				for (const [name, lp] of libProps) {
					if (name === "Reference" || name.startsWith("ki_") || name in over) continue;
					const libVal = text(lp, 2) ?? "";
					const p = prop(sym, name);
					if (!p) {
						addField(libFieldNode(lp, libVal, origin));
						changes.add(`+${name}=${JSON.stringify(libVal)}`);
						continue;
					}
					const cur = text(p, 2) ?? "";
					if (cur === libVal) continue;
					if (refresh ? cur === (oldVals.get(name) ?? "") : libVal !== "") {
						p[2] = str(libVal);
						changes.add(`${name} ${JSON.stringify(cur)} → ${JSON.stringify(libVal)}`);
					} else kept.add(`${name}=${JSON.stringify(cur)}`);
				}
				for (const [name, v] of Object.entries(over)) {
					const p = prop(sym, name);
					if (v === null) {
						if (!p) throw new Error(`${ref} has no field ${name}`);
						sym.splice(sym.indexOf(p), 1);
						changes.add(`-${name}`);
					} else if (p) {
						if (text(p, 2) !== v) changes.add(`${name} ${JSON.stringify(text(p, 2) ?? "")} → ${JSON.stringify(v)}`);
						p[2] = str(v);
					} else {
						const lp = libProps.get(name);
						addField(lp ? libFieldNode(lp, v, origin) : fieldNode(name, v, origin, true));
						changes.add(`+${name}=${JSON.stringify(v)}`);
					}
				}
				if (!refresh) {
					for (const p of children(sym, "property")) {
						const name = text(p) ?? "";
						if (!MANDATORY[name] && !libProps.has(name) && !(name in over)) kept.add(`${name}=${JSON.stringify(text(p, 2) ?? "")}`);
					}
				}
				placeFields(ctx, sch, sym);
			}
			if (!refresh && !sch.symbols().some(s => sch.libId(s) === oldId)) {
				const old = sch.libSymbol(oldId);
				if (old) sch.libSymbols().splice(sch.libSymbols().indexOf(old), 1);
			}

			const rep = pinReport(sch, inst, before, affected);
			const dropped = dropStaleJunctions(sch, [], rep.oldPts);
			if (sharers.length) notes.push(`also redrew ${sharers.map(s => sch.refOf(s, inst.path)).join(", ")} on ${inst.name} (they use the sheet's copy of ${newId})`);
			if (rep.offPin.length) notes.push(`left off a pin on ${inst.name}, not rewired: ${rep.offPin.join(", ")}`);
			if (rep.bare.length) notes.push(`pins with nothing on them: ${rep.bare.join(", ")}`);
			if (rep.midWire.length) notes.push(`pins on the middle of a wire (KiCad connects them): ${rep.midWire.join(", ")}`);
			if (dropped.length) notes.push(droppedNote(dropped));
			for (const sym of targets) pinLists.push(`pins${placed.length > 1 ? ` (unit ${sch.unit(sym)} on ${inst.name})` : ""}: ${pinNote(sch, sym)}`);
			ctx.project.touch(sch);
			const pts: Pt[] = [];
			for (const [s, b] of before) {
				const nb = symBox(sch, s);
				pts.push([b.box[0], b.box[1]], [b.box[2], b.box[3]], [nb[0], nb[1]], [nb[2], nb[3]]);
			}
			ctx.touch(inst, bbox(pts));
		}
		const parts = [refresh ? `refreshed ${ref} from the library (${newId})` : `changed ${ref}: ${oldId} → ${newId}`];
		if (changes.size) parts.push(`fields: ${[...changes].join(", ")}`);
		if (kept.size) {
			parts.push(
				refresh
					? `kept ${[...kept].join(", ")} (they differ from the library)`
					: `kept ${[...kept].join(", ")}: check they fit ${newId} and set them with fields`,
			);
		}
		ctx.notes.push([...parts, ...notes, ...pinLists].join("; "));
	},
};

const moveSymbol: OpDef = {
	op: "move_symbol",
	group: "schematic",
	doc: "Move/rotate/mirror a symbol (origin to `at`). With drag (default), wire ends, labels, junctions and no-connects on its pins follow the pins (only that wire end moves); a wire that would turn diagonal is an error. Junctions left joining fewer than 3 connections go.",
	fields: {
		ref: REF,
		at: { ...AT, doc: "new symbol origin in mm, on the connection grid" },
		rot: { t: "integer", doc: "rotation 0/90/180/270 (default: unchanged)" },
		mirror: { t: "string", values: ["x", "y", "none"], doc: "mirror (default: unchanged)" },
		unit: { t: "integer", doc: "unit, for a multi-unit part" },
		drag: { t: "boolean", doc: "move connected wire ends with the pins (default true)" },
		fields: { t: "string", values: ["auto", "keep"], doc: "auto re-places the fields, keep shifts them with the symbol (default: keep when only moved, auto when rotated/mirrored)" },
	},
	apply(ctx, args) {
		const { sheet, sym } = oneUnit(ctx, args.ref, args.unit);
		const sch = sheet.sch;
		const old = sch.pose(sym);
		const oldPins = sch.pins(sym);
		const oldBox = symBox(sch, sym);
		const p = gridPt(ctx, args.at, "at");
		const rot = args.rot === undefined ? old.rot : rotArg(args.rot);
		const mirror: Mirror = args.mirror === undefined ? old.mirror : args.mirror === "none" ? undefined : (args.mirror as Mirror);
		setAt(sym, p[0], p[1], rot);
		removeChild(sym, "mirror");
		if (mirror) sym.splice(sym.findIndex(c => isList(c) && head(c) === "at") + 1, 0, L("mirror", mirror));
		const reposed = rot !== old.rot || mirror !== old.mirror;
		const mode = (args.fields as string | undefined) ?? (reposed ? "auto" : "keep");
		if (mode === "keep") {
			for (const f of children(sym, "property")) {
				const a = at(f);
				setAt(f, a.p[0] + p[0] - old.x, a.p[1] + p[1] - old.y, a.rot);
			}
		} else placeFields(ctx, sch, sym);
		const newPins = sch.pins(sym);
		const moves = new Map<string, Pt>();
		oldPins.forEach((op, i) => {
			if (!samePt(op.at, newPins[i]!.at)) moves.set(key(op.at), newPins[i]!.at);
		});
		const notes = [`moved ${args.ref} to ${fmtPt(p)} rot ${rot}${mirror ? ` mirror ${mirror}` : ""}`];
		const dragged: Pt[] = [];
		const oldSegs: Seg[] = [];
		if (args.drag !== false && moves.size) {
			for (const w of sch.wires()) {
				if (!moves.has(key(w.a)) && !moves.has(key(w.b))) continue;
				oldSegs.push({ a: w.a, b: w.b });
				for (const e of [w.a, w.b]) if (moves.has(key(e))) dragged.push(e, moves.get(key(e))!);
			}
			const n = dragWireEnds(sch, moves);
			for (const l of sch.labels()) {
				const to = moves.get(key(l.at));
				if (to) setAt(l.node, to[0], to[1], l.rot);
			}
			for (const kind of ["no_connect", "junction"] as const) {
				for (const pnt of sch.points(kind)) {
					const to = moves.get(key(pnt.at));
					if (to) setChild(pnt.node, L("at", to[0], to[1]));
				}
			}
			if (n) notes.push(`dragged ${n} wire(s)`);
		}
		const dropped = dropStaleJunctions(sch, oldSegs, [...oldPins, ...newPins].map(q => q.at));
		if (dropped.length) notes.push(droppedNote(dropped));
		if (mode === "keep" && reposed) notes.push("fields kept their offsets although the symbol turned; use fields: \"auto\" to re-place them");
		notes.push(`pins: ${pinNote(sch, sym)}`);
		ctx.project.touch(sch);
		const pts: Pt[] = [...dragged, [oldBox[0], oldBox[1]], [oldBox[2], oldBox[3]]];
		const nb = symBox(sch, sym);
		pts.push([nb[0], nb[1]], [nb[2], nb[3]]);
		ctx.touch(sheet, bbox(pts));
		ctx.notes.push(notes.join("; "));
	},
};

const addWire: OpDef = {
	op: "add_wire",
	group: "schematic",
	doc: "Draw a wire through grid points (orthogonal segments only). Adds junctions where a new wire end lands on another wire's interior or the new wire passes over a wire end or pin.",
	fields: {
		sheet: SHEET,
		points: { t: "points", req: true, doc: "polyline corners [[x,y],…] in mm, on the connection grid" },
		junctions: { t: "boolean", doc: "add the junctions the new wire needs (default true)" },
	},
	apply(ctx, args) {
		const inst = sheetArg(ctx, args.sheet);
		const sch = inst.sch;
		const pts = (args.points as unknown[]).map((v, i) => gridPt(ctx, v, `points[${i}]`));
		const segs: { a: Pt; b: Pt }[] = [];
		for (let i = 0; i + 1 < pts.length; i++) {
			const a = pts[i]!;
			const b = pts[i + 1]!;
			if (samePt(a, b)) throw new Error(`points[${i}] and points[${i + 1}] are the same point ${fmtPt(a)}`);
			if (!isOrthogonal({ a, b })) {
				throw new Error(`segment ${i} ${fmtPt(a)}-${fmtPt(b)} is diagonal; add a corner point such as ${fmtPt([b[0], a[1]])}`);
			}
			segs.push({ a, b });
		}
		for (const s of segs) {
			sch.insert(
				L("wire", L("pts", L("xy", s.a[0], s.a[1]), L("xy", s.b[0], s.b[1])), L("stroke", L("width", 0), L("type", "default")), L("uuid", str(newUuid()))),
			);
		}
		const js = args.junctions === false ? [] : autoJunctions(sch, segs);
		ctx.project.touch(sch);
		ctx.touch(inst, bbox(pts));
		ctx.notes.push(`wire ${pts.map(fmtPt).join("-")} on ${inst.name}${js.length ? `; junctions at ${js.map(fmtPt).join(", ")}` : ""}`);
	},
};

const removeWire: OpDef = {
	op: "remove_wire",
	group: "schematic",
	doc: "Delete a stretch of wire: the whole segment containing `at`, or from `from` to `to` — two points on one segment, its ends or any grid points inside it (the rest of the segment stays as new wires). Junctions left joining fewer than 3 connections go too; the note names pins, labels and wire ends left unconnected.",
	fields: {
		sheet: SHEET,
		at: { t: "point", doc: "a point on the segment (must identify exactly one); removes the whole segment" },
		from: { t: "point", doc: "one end of the stretch to remove (with to), on the segment" },
		to: { t: "point", doc: "the other end of the stretch (with from), on the same segment" },
	},
	apply(ctx, args) {
		const inst = sheetArg(ctx, args.sheet);
		const sch = inst.sch;
		const wires = sch.wires();
		const show = (ws: typeof wires) => ws.map(w => `${fmtPt(w.a)}-${fmtPt(w.b)}`).join(", ") || "none";
		let hit: (typeof wires)[number];
		let cut: Seg;
		if (args.at !== undefined) {
			if (args.from !== undefined || args.to !== undefined) throw new Error("give either at, or from and to");
			const p = gridPt(ctx, args.at, "at");
			const hits = wires.filter(w => touches(p, w));
			if (!hits.length) throw new Error(`no wire at ${fmtPt(p)} on ${inst.name}`);
			if (hits.length > 1) throw new Error(`${hits.length} wires meet at ${fmtPt(p)}: ${show(hits)}; pick one with from/to or a point inside it`);
			hit = hits[0]!;
			cut = hit;
		} else {
			if (args.from === undefined || args.to === undefined) throw new Error("give either at, or from and to");
			const a = gridPt(ctx, args.from, "from");
			const b = gridPt(ctx, args.to, "to");
			if (samePt(a, b)) throw new Error(`from and to are the same point ${fmtPt(a)}`);
			const exact = wires.find(w => (samePt(w.a, a) && samePt(w.b, b)) || (samePt(w.a, b) && samePt(w.b, a)));
			const hits = exact ? [exact] : wires.filter(w => touches(a, w) && touches(b, w));
			if (!hits.length) {
				throw new Error(
					`${fmtPt(a)} and ${fmtPt(b)} don't lie on one wire segment on ${inst.name}; wires at from: ${show(wires.filter(w => touches(a, w)))}; ` +
						`wires at to: ${show(wires.filter(w => touches(b, w)))}`,
				);
			}
			if (hits.length > 1) throw new Error(`${hits.length} overlapping wires contain ${fmtPt(a)}-${fmtPt(b)}: ${show(hits)}; remove them by their ends`);
			hit = hits[0]!;
			cut = { a, b };
		}
		// what stays of the segment: hit.a up to the nearer cut point, the farther one up to hit.b
		const dist = (p: Pt) => Math.abs(p[0] - hit.a[0]) + Math.abs(p[1] - hit.a[1]);
		const [near, far] = dist(cut.a) <= dist(cut.b) ? [cut.a, cut.b] : [cut.b, cut.a];
		const rest: Seg[] = [];
		if (!samePt(hit.a, near)) rest.push({ a: hit.a, b: near });
		if (!samePt(far, hit.b)) rest.push({ a: far, b: hit.b });
		const nodes = rest.map(r => {
			const n = clone(hit.node);
			setChild(n, L("pts", L("xy", r.a[0], r.a[1]), L("xy", r.b[0], r.b[1])));
			setChild(n, L("uuid", str(newUuid())));
			return n;
		});
		sch.tree.splice(sch.tree.indexOf(hit.node), 1, ...nodes);
		const dropped = dropStaleJunctions(sch, [hit]);
		const open = leftOpen(inst, cut);
		ctx.project.touch(sch);
		ctx.touch(inst, bbox([hit.a, hit.b]));
		const notes = [
			rest.length
				? `removed ${fmtPt(near)}-${fmtPt(far)} of wire ${fmtPt(hit.a)}-${fmtPt(hit.b)} on ${inst.name}, keeping ${rest.map(r => `${fmtPt(r.a)}-${fmtPt(r.b)}`).join(" and ")}`
				: `removed wire ${fmtPt(hit.a)}-${fmtPt(hit.b)} on ${inst.name}`,
		];
		if (open.length) notes.push(`left unconnected: ${open.join(", ")}`);
		if (dropped.length) notes.push(droppedNote(dropped));
		ctx.notes.push(notes.join("; "));
	},
};

const addLabel: OpDef = {
	op: "add_label",
	group: "schematic",
	doc: "Place a net label with its connection point at `at` (rot 0: text runs right, 180: left, 90/270: vertical). Global and hierarchical labels take a shape (signal direction seen from this sheet).",
	fields: {
		sheet: SHEET,
		name: { t: "string", req: true, doc: "net name" },
		at: { ...AT, doc: "connection point in mm, on a wire end or wire" },
		rot: ROT,
		kind: { t: "string", values: ["local", "global", "hierarchical"], doc: "default local" },
		shape: { t: "string", values: SHAPES, doc: "global/hierarchical only (default passive)" },
	},
	apply(ctx, args) {
		const inst = sheetArg(ctx, args.sheet);
		const kindArg = (args.kind as string | undefined) ?? "local";
		const kind = LABEL_NODE[kindArg]!;
		if (kind === "label" && args.shape !== undefined) throw new Error("shape is for global and hierarchical labels only");
		const name = textArg(args.name, "name");
		const p = gridPt(ctx, args.at, "at");
		const rot = rotArg(args.rot);
		const shape = (args.shape as string | undefined) ?? "passive";
		inst.sch.insert(labelNode(kind, name, p, rot, shape));
		ctx.project.touch(inst.sch);
		const len = name.length * CHAR_W;
		ctx.touch(inst, bbox([p, [p[0] + (rot === 0 ? len : rot === 180 ? -len : 0), p[1] + (rot === 90 ? -len : rot === 270 ? len : 0)]]));
		const notes = [`${kindArg} label ${name} at ${fmtPt(p)} on ${inst.name}`];
		if (kind === "hierarchical_label") notes.push("the parent sheet needs a matching sheet pin");
		ctx.notes.push(notes.join("; "));
	},
};

const removeLabel: OpDef = {
	op: "remove_label",
	group: "schematic",
	doc: "Delete a label (any kind) by name; `at` picks one when the sheet has several with that name.",
	fields: { sheet: SHEET, name: { t: "string", req: true, doc: "label text" }, at: { t: "point", doc: "its connection point" } },
	apply(ctx, args) {
		const inst = sheetArg(ctx, args.sheet);
		const all = inst.sch.labels().filter(l => l.name === args.name);
		const p = args.at === undefined ? undefined : gridPt(ctx, args.at, "at");
		const hits = p ? all.filter(l => samePt(l.at, p)) : all;
		if (!hits.length) {
			throw new Error(`no label ${args.name}${p ? ` at ${fmtPt(p)}` : ""} on ${inst.name}${all.length ? `; it is at ${all.map(l => fmtPt(l.at)).join(", ")}` : ""}`);
		}
		if (hits.length > 1) throw new Error(`${hits.length} labels ${args.name} on ${inst.name}, at ${hits.map(l => fmtPt(l.at)).join(", ")}; pass at`);
		const l = hits[0]!;
		inst.sch.remove(l.node);
		ctx.project.touch(inst.sch);
		ctx.touch(inst, bbox([l.at]));
		ctx.notes.push(`removed ${LABEL_KIND[l.kind]} label ${l.name} at ${fmtPt(l.at)} on ${inst.name}`);
	},
};

const renameNet: OpDef = {
	op: "rename_net",
	group: "schematic",
	doc: "Rename a net's labels: local labels on `sheet` (every sheet if omitted), global labels everywhere, hierarchical labels (on `sheet` or everywhere) with the matching sheet pins on the parent. Power nets (power symbols) can't be renamed this way.",
	fields: {
		from: { t: "string", req: true, doc: "current label name" },
		to: { t: "string", req: true, doc: "new name" },
		sheet: { t: "string", doc: "limit local and hierarchical labels to this sheet" },
	},
	apply(ctx, args) {
		const from = String(args.from);
		const to = textArg(args.to, "to");
		if (from === to) throw new Error("from and to are the same");
		const project = ctx.project;
		const all = project.hierarchy();
		const powerRefs: string[] = [];
		for (const s of all) {
			for (const sym of s.sch.symbols()) {
				if (propValue(sym, "Value") === from && child(s.sch.lib(sym), "power")) powerRefs.push(s.sch.refOf(sym, s.path));
			}
		}
		if (powerRefs.length) {
			throw new Error(`${from} is a power net (power symbols ${powerRefs.slice(0, 6).join(", ")}${powerRefs.length > 6 ? ", …" : ""}); replace the power symbols with add_power / remove_symbol instead`);
		}
		const scope = new Set((args.sheet === undefined ? all : instancesOf(ctx, sheetArg(ctx, args.sheet))).map(s => s.file));
		const changed = new Map<string, { inst: SheetInst; pts: Pt[] }>();
		const mark = (inst: SheetInst, p: Pt) => {
			const e = changed.get(inst.file) ?? { inst, pts: [] };
			e.pts.push(p);
			changed.set(inst.file, e);
		};
		const counts: Record<string, number> = { local: 0, global: 0, hierarchical: 0, "sheet pin": 0 };
		const hierFiles = new Set<string>();
		let existed = false;
		const seenFiles = new Set<string>();
		for (const s of all) {
			if (seenFiles.has(s.file)) continue;
			seenFiles.add(s.file);
			for (const l of s.sch.labels()) {
				const inScope = l.kind === "global_label" || scope.has(s.file);
				if (l.name === to && inScope) existed = true;
				if (l.name !== from || !inScope) continue;
				l.node[1] = str(to);
				counts[LABEL_KIND[l.kind]]!++;
				if (l.kind === "hierarchical_label") hierFiles.add(s.file);
				mark(s, l.at);
			}
		}
		for (const s of all) {
			if (!s.parent || !s.block || !hierFiles.has(s.file)) continue;
			for (const pin of s.block.pins) {
				if (pin.name !== from) continue;
				pin.node[1] = str(to);
				counts["sheet pin"]!++;
				mark(s.parent, pin.at);
			}
		}
		if (!changed.size) throw new Error(`no label ${from}${args.sheet === undefined ? "" : ` on ${args.sheet}`} (global labels are searched on every sheet)`);
		for (const { inst, pts } of changed.values()) {
			project.touch(inst.sch);
			ctx.touch(inst, bbox(pts));
		}
		const what = Object.entries(counts)
			.filter(([, n]) => n)
			.map(([k, n]) => `${n} ${k}`);
		ctx.notes.push(`renamed ${from} → ${to}: ${what.join(", ")}${existed ? `; ${to} already existed, so the nets are now joined` : ""}`);
	},
};

const addPower: OpDef = {
	op: "add_power",
	group: "schematic",
	doc: "Place a power symbol whose pin sits exactly at `at` (rot 0 = upright: rails point up, grounds down). lib_id from the project's powerSymbols, else power:<net>; reference from the sheet's #PWR block.",
	fields: { sheet: SHEET, net: { t: "string", req: true, doc: "power net, e.g. \"GND\", \"+3V3\"" }, at: { ...AT, doc: "pin position in mm, on the grid" }, rot: ROT },
	apply(ctx, args) {
		const inst = sheetArg(ctx, args.sheet);
		const net = textArg(args.net, "net");
		const libId = ctx.project.settings.powerSymbols[net] ?? `power:${net}`;
		const p = gridPt(ctx, args.at, "at");
		const rot = rotArg(args.rot);
		const lib = ensureLib(ctx, inst.sch, libId);
		if (!child(lib, "power")) throw new Error(`${libId} is not a power symbol`);
		const pins = libGeom(lib).pins.filter(q => q.unit <= 1 && q.style <= 1);
		if (pins.length !== 1) throw new Error(`${libId} has ${pins.length} pins; a power symbol has one`);
		const off = xform(pins[0]!.lx, pins[0]!.ly, 0, 0, rot, undefined);
		const node = placeSymbol(ctx, inst, { libId, at: [p[0] - off[0], p[1] - off[1]], rot, mirror: undefined, unit: 1, value: net });
		const pinAt = inst.sch.pins(node)[0]!.at;
		ctx.notes.push(`added ${inst.sch.refOf(node, inst.path)} (${net}) on ${inst.name}, pin at ${fmtPt(pinAt)}`);
	},
};

function pointOps(kind: "junction" | "no_connect", what: string): OpDef[] {
	return [
		{
			op: `add_${kind}`,
			group: "schematic",
			doc:
				kind === "junction"
					? "Add a junction dot (where 3+ wires/pins meet, or a wire end lands on a wire)."
					: "Mark a pin as intentionally unconnected (the point must be a symbol or sheet pin).",
			fields: { sheet: SHEET, at: AT },
			apply(ctx, args) {
				const inst = sheetArg(ctx, args.sheet);
				const sch = inst.sch;
				const p = gridPt(ctx, args.at, "at");
				if (findPoint(sch, kind, p)) throw new Error(`there is already a ${what} at ${fmtPt(p)} on ${inst.name}`);
				const notes = [`${what} at ${fmtPt(p)} on ${inst.name}`];
				if (kind === "junction") {
					sch.insert(junctionNode(p));
					const n = sch.junctionDegree(p);
					if (n < 3) notes.push(`only ${n} connection(s) meet there: a redundant junction (style warns)`);
				} else {
					const pinHere =
						sch.symbols().some(s => sch.pins(s).some(q => samePt(q.at, p))) || sch.sheets().some(b => b.pins.some(q => samePt(q.at, p)));
					if (!pinHere) throw new Error(`no pin at ${fmtPt(p)} on ${inst.name}; a no-connect goes on a pin end`);
					sch.insert(L("no_connect", L("at", p[0], p[1]), L("uuid", str(newUuid()))));
				}
				ctx.project.touch(sch);
				ctx.touch(inst, bbox([p]));
				ctx.notes.push(notes.join("; "));
			},
		},
		{
			op: `remove_${kind}`,
			group: "schematic",
			doc: `Delete the ${what} at a point.`,
			fields: { sheet: SHEET, at: AT },
			apply(ctx, args) {
				const inst = sheetArg(ctx, args.sheet);
				const p = gridPt(ctx, args.at, "at");
				const node = findPoint(inst.sch, kind, p);
				if (!node) throw new Error(`no ${what} at ${fmtPt(p)} on ${inst.name}`);
				inst.sch.remove(node);
				ctx.project.touch(inst.sch);
				ctx.touch(inst, bbox([p]));
				ctx.notes.push(`removed ${what} at ${fmtPt(p)} on ${inst.name}`);
			},
		},
	];
}

const addText: OpDef = {
	op: "add_text",
	group: "schematic",
	doc: "Place a text note starting at `at` (left-bottom anchored).",
	fields: {
		sheet: SHEET,
		text: { t: "string", req: true, doc: "the text (\\n for line breaks)" },
		at: { ...AT, doc: "anchor in mm, on the grid" },
		rot: ROT,
		size: { t: "number", doc: "text height in mm (default 1.27)" },
	},
	apply(ctx, args) {
		const inst = sheetArg(ctx, args.sheet);
		const p = gridPt(ctx, args.at, "at");
		const rot = rotArg(args.rot);
		const size = (args.size as number | undefined) ?? 1.27;
		if (size <= 0) throw new Error("size must be positive");
		const t = String(args.text);
		if (!t.trim()) throw new Error("text is empty");
		inst.sch.insert(
			L(
				"text",
				str(t),
				L("exclude_from_sim", false),
				L("at", p[0], p[1], rot),
				L("effects", L("font", L("size", size, size)), L("justify", "left", "bottom")),
				L("uuid", str(newUuid())),
			),
		);
		ctx.project.touch(inst.sch);
		const lines = t.split("\n");
		const w = Math.max(...lines.map(l => l.length)) * CHAR_W * (size / 1.27);
		const h = lines.length * size * 1.6;
		ctx.touch(inst, rot === 0 || rot === 180 ? [p[0], p[1] - h, p[0] + w, p[1]] : [p[0] - h, p[1] - w, p[0], p[1]]);
		ctx.notes.push(`text at ${fmtPt(p)} on ${inst.name}`);
	},
};

const removeText: OpDef = {
	op: "remove_text",
	group: "schematic",
	doc: "Delete text notes whose text equals `text` exactly.",
	fields: { sheet: SHEET, text: { t: "string", req: true, doc: "exact text" } },
	apply(ctx, args) {
		const inst = sheetArg(ctx, args.sheet);
		const hits = inst.sch.items("text").filter(n => text(n) === args.text);
		if (!hits.length) {
			const have = inst.sch.items("text").map(n => JSON.stringify(text(n)));
			throw new Error(`no text ${JSON.stringify(args.text)} on ${inst.name}${have.length ? `; texts: ${have.slice(0, 10).join(", ")}` : ""}`);
		}
		for (const n of hits) {
			const p = at(n).p;
			inst.sch.remove(n);
			ctx.touch(inst, bbox([p]));
		}
		ctx.project.touch(inst.sch);
		ctx.notes.push(`removed ${hits.length} text(s) on ${inst.name}`);
	},
};

const annotate: OpDef = {
	op: "annotate",
	group: "schematic",
	doc: "Number every reference ending in \"?\" (on `sheet`, or all sheets): the sheet's refBase block (a new sheet gets the next free block), else the next number project-wide.",
	fields: { sheet: { t: "string", doc: "only this sheet" } },
	apply(ctx, args) {
		const project = ctx.project;
		const scope = args.sheet === undefined ? project.hierarchy() : [sheetArg(ctx, args.sheet)];
		const taken = new Set<string>();
		const done: string[] = [];
		for (const s of scope) {
			for (const sym of s.sch.symbols()) {
				const r = s.sch.refOf(sym, s.path);
				if (!r.endsWith("?")) continue;
				const nr = nextRef(project, refPrefix(r), s, taken);
				taken.add(nr);
				s.sch.setRef(sym, s.path, project.name, nr);
				project.touch(s.sch);
				ctx.touch(s, symBox(s.sch, sym));
				done.push(`${nr} (${s.sch.libId(sym)} on ${s.name})`);
			}
		}
		ctx.notes.push(done.length ? `annotated ${done.join(", ")}` : "nothing to annotate");
	},
};

export const ops: OpDef[] = [
	setFields,
	renameRef,
	setAttrs,
	addSymbol,
	removeSymbol,
	changeSymbol,
	moveSymbol,
	addWire,
	removeWire,
	addLabel,
	removeLabel,
	renameNet,
	addPower,
	...pointOps("junction", "junction"),
	...pointOps("no_connect", "no-connect"),
	addText,
	removeText,
	annotate,
];
