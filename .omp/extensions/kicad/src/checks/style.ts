/**
 * Drawing-style and wiring checks (`style`): the user's drawing rules (pin stubs, power-symbol
 * placement, label underlines) plus overlaps, grid, frame and wiring sanity, per sheet.
 *
 * Keys name the rule and the subject (references, pins, label or net names), never coordinates,
 * so moving a drawing keeps its findings and the edit gate only sees real new problems.
 */
import { type Analysis, CHAR_W, TEXT_H } from "../analysis";
import { bbox, type Box, type Dir, DIR_VEC, key, onGrid, onInterior, overlap, type Pt, pt, r3, samePt, type Seg, touches, xform } from "../geom";
import type { SheetInst } from "../project";
import { isHidden, type Label, libGeom, type Sch, uuidOf } from "../sch";
import { child, children, isList, type List, num, text } from "../sexpr";
import type { CheckDef, Finding, Severity } from "../types";

interface PinRef {
	ref: string;
	num: string;
	dir: Dir;
}

interface SymInfo {
	sym: List;
	ref: string;
	lib: List;
}

/** Connection geometry of one sheet instance; point maps are keyed by `geom.key`. */
interface Geo {
	sheet: SheetInst;
	sch: Sch;
	stub: number;
	grid: number;
	widths: Map<string, number>;
	syms: SymInfo[];
	wires: Seg[];
	buses: Seg[];
	pins: Map<string, { at: Pt; list: PinRef[] }>;
	labels: Label[];
	junctions: Map<string, Pt>;
	ncs: Map<string, Pt>;
	entryEnds: Map<string, Pt>;
	/** Sheet-block pins (root and other parent sheets): "Block/pin". */
	sheetPins: Map<string, { at: Pt; name: string }>;
	/** Points that join a wire when they sit on its interior: pins, junctions, labels, sheet pins. */
	extras: Pt[];
}

const STYLE_RULES: Record<string, (g: Geo) => Finding[]> = {
	pin_stub: pinStub,
	power,
	underline,
	overlap: overlaps,
	off_grid: offGrid,
	frame,
	wiring,
};

const WIRING_SEVERITY: Record<string, Severity> = {
	pin_on_wire: "error",
	missing_junction: "error",
	nc_on_wired_pin: "error",
	pin_collision: "error",
	wire_end_on_bus: "error",
	bus_entry_unattached: "error",
	unlabeled_bus_wire: "error",
	dangling: "warning",
	wire_over_body: "warning",
	redundant_junction: "warning",
};

/** Page border inset and title block size (mm) of KiCad's default worksheet. */
const FRAME_INSET = 12.7;
const TITLE_BLOCK: Pt = [125, 47];
/** Clearance (mm) between drawn graphics (body primitives, pin lines) of two symbols. */
const GRAPHIC_MARGIN = 0.1;
/** Clearance (mm) between a visible field's text and anything of another symbol. */
const TEXT_MARGIN = 0.3;
/** Tolerance (mm) for a wire grazing a body edge (touching a pin tip at the outline is fine). */
const BODY_EDGE = 0.05;

/** Findings of every rule enabled in `style.rules` on these sheet instances. */
export async function styleFindings(analysis: Analysis, sheets: SheetInst[]): Promise<Finding[]> {
	const { grid, style } = analysis.project.settings;
	const unknown = style.rules.filter(r => !STYLE_RULES[r]);
	if (unknown.length) throw new Error(`style.rules: unknown rule(s) ${unknown.join(", ")}; rules: ${Object.keys(STYLE_RULES).join(", ")}`);
	if (!sheets.length) return [];
	const widths = await analysis.textWidths();
	const out: Finding[] = [];
	for (const sheet of sheets) {
		const g = geometry(sheet, style.pinStub, grid, widths);
		for (const r of style.rules) out.push(...STYLE_RULES[r]!(g));
	}
	return out;
}

export const checks: CheckDef[] = [
	{
		name: "style",
		doc:
			"Drawing rules per sheet (those listed in style.rules of .omp/kicad.json): pin_stub (every wired pin leaves on ≥ 1.27 mm of straight wire, no connection inside it), " +
			"power (upright power symbols, straight or L-shaped 1.27 mm off a single pin), underline (a horizontal local label sits on a wire running under its whole text), " +
			"overlap (bodies, pins, visible fields of different symbols), off_grid (connection points on the grid), frame (inside the border, off the title block) and " +
			"wiring (pins on wire interiors, missing junctions on wires and buses, pin collisions, no-connects on wired pins, bus entries and labels; dangling ends, wires over bodies and redundant junctions — joining fewer than 3 connections — as warnings).",
		byDefault: true,
		run: ctx => styleFindings(ctx.analysis, ctx.sheets ?? ctx.project.hierarchy()),
	},
];

// ---- geometry ---------------------------------------------------------------------------------

function geometry(sheet: SheetInst, stub: number, grid: number, widths: Map<string, number>): Geo {
	const sch = sheet.sch;
	const syms = sch.symbols().map(sym => ({ sym, ref: sch.refOf(sym, sheet.path), lib: sch.lib(sym) }));
	const pins = new Map<string, { at: Pt; list: PinRef[] }>();
	for (const s of syms) {
		for (const p of sch.pins(s.sym)) {
			const k = key(p.at);
			const e = pins.get(k) ?? { at: p.at, list: [] };
			e.list.push({ ref: s.ref, num: p.num, dir: p.dir });
			pins.set(k, e);
		}
	}
	const byKey = (ps: Pt[]) => new Map(ps.map(p => [key(p), p]));
	const sheetPins = new Map<string, { at: Pt; name: string }>();
	for (const b of sch.sheets()) for (const p of b.pins) sheetPins.set(key(p.at), { at: p.at, name: `${b.name}/${p.name}` });
	const labels = sch.labels();
	const junctions = byKey(sch.points("junction").map(j => j.at));
	return {
		sheet,
		sch,
		stub,
		grid,
		widths,
		syms,
		wires: sch.wires(),
		buses: sch.buses(),
		pins,
		labels,
		junctions,
		ncs: byKey(sch.points("no_connect").map(j => j.at)),
		entryEnds: byKey(sch.busEntries().flatMap(e => [e.a, e.b])),
		sheetPins,
		extras: [...[...pins.values()].map(p => p.at), ...junctions.values(), ...labels.map(l => l.at), ...[...sheetPins.values()].map(p => p.at)],
	};
}

function touching(p: Pt, segs: readonly Seg[]): Seg[] {
	return segs.filter(s => touches(p, s));
}

/** Points and segments joined to `start` through these segments (and the points on their interiors). */
function component(g: Geo, start: Pt, segs: readonly Seg[]): { pts: Map<string, Pt>; segs: Set<Seg> } {
	const pts = new Map<string, Pt>();
	const found = new Set<Seg>();
	const stack: Pt[] = [start];
	while (stack.length) {
		const q = stack.pop()!;
		const k = key(q);
		if (pts.has(k)) continue;
		pts.set(k, q);
		for (const s of segs) {
			if (found.has(s) || !touches(q, s)) continue;
			found.add(s);
			stack.push(s.a, s.b, ...g.extras.filter(p => onInterior(p, s)));
		}
	}
	return { pts, segs: found };
}

/** Coordinate-free name of the wiring a point belongs to: its first label, else its first pin or sheet pin. */
function subject(g: Geo, start: Pt, segs: readonly Seg[]): string {
	const { pts } = component(g, start, segs);
	const labels = g.labels.filter(l => pts.has(key(l.at))).map(l => l.name);
	if (labels.length) return `net ${labels.sort()[0]}`;
	const pins = [...pts.keys()].flatMap(k => (g.pins.get(k)?.list ?? []).map(p => `${p.ref}.${p.num}`));
	if (pins.length) return `wire at ${pins.sort()[0]}`;
	const sp = [...pts.keys()].flatMap(k => g.sheetPins.get(k)?.name ?? []);
	if (sp.length) return `wire at sheet pin ${sp.sort()[0]}`;
	return "unconnected wire";
}

function finding(g: Geo, rule: string, severity: Severity, subj: string, message: string, at?: Pt, refs?: string[]): Finding {
	return { check: "style", severity, key: `style|${rule}|${g.sheet.name}|${subj}`, message, sheet: g.sheet.name, at, refs };
}

// ---- R1 pin_stub ------------------------------------------------------------------------------

function pinStub(g: Geo): Finding[] {
	const out: Finding[] = [];
	const stops = [...g.extras, ...g.entryEnds.values()];
	for (const [k, { at: p, list }] of g.pins) {
		if (g.ncs.has(k)) continue;
		for (const ref of [...new Set(list.map(x => x.ref))].sort()) {
			const mine = list.find(x => x.ref === ref)!;
			const why = stubProblem(g, p, mine.dir, list.find(x => x.ref !== ref), stops);
			if (!why) continue;
			out.push(
				finding(
					g,
					"pin_stub",
					"error",
					`${ref}.${mine.num}`,
					`pin ${ref}.${mine.num}: ${why}; a wired pin needs ${g.stub} mm of straight wire in its own direction before any junction, bend or other connection`,
					p,
					[ref],
				),
			);
		}
	}
	return out;
}

function stubProblem(g: Geo, p: Pt, dir: Dir, other: PinRef | undefined, stops: Pt[]): string | undefined {
	if (other) return `connects pin-to-pin with ${other.ref}.${other.num}`;
	const ws = touching(p, g.wires);
	if (!ws.length) return undefined;
	if (ws.some(s => onInterior(p, s))) return "the pin sits on a wire interior";
	if (ws.length > 1 || g.junctions.has(key(p))) return "a junction or branch at the pin";
	const [dx, dy] = DIR_VEC[dir];
	let cur = p;
	let left = g.stub;
	let seg = ws[0]!;
	for (;;) {
		const far = samePt(seg.a, cur) ? seg.b : seg.a;
		const vx = far[0] - cur[0];
		const vy = far[1] - cur[1];
		if (Math.sign(vx) !== dx || Math.sign(vy) !== dy) {
			return samePt(cur, p) ? "the wire leaves at an angle to the pin" : `bend ${r3(g.stub - left)} mm from the pin`;
		}
		const len = Math.abs(vx) + Math.abs(vy);
		const stop = pt(cur[0] + dx * Math.min(len, left), cur[1] + dy * Math.min(len, left));
		const part = { a: cur, b: stop };
		const ends = g.wires.filter(s => s !== seg).flatMap(s => [s.a, s.b]);
		const hit = [...stops, ...ends].find(q => onInterior(q, part));
		if (hit) return `connection ${r3(Math.abs(hit[0] - p[0]) + Math.abs(hit[1] - p[1]))} mm from the pin`;
		if (len >= left - 1e-6) return undefined;
		left -= len;
		cur = far;
		const next = touching(cur, g.wires).filter(s => s !== seg);
		if (g.pins.has(key(cur)) || g.junctions.has(key(cur)) || next.length !== 1) return `branch or end ${r3(g.stub - left)} mm from the pin`;
		seg = next[0]!;
	}
}

// ---- R2 power ---------------------------------------------------------------------------------

function power(g: Geo): Finding[] {
	const out: Finding[] = [];
	const stub = g.stub;
	for (const s of g.syms) {
		// power symbols, not PWR_FLAGs
		if (!child(s.lib, "power") || /(^|:)PWR_FLAG$/.test(text(s.lib) ?? "")) continue;
		const pin = g.sch.pins(s.sym)[0];
		if (!pin) continue;
		const { rot } = g.sch.pose(s.sym);
		const at = pin.at;
		const bad = (msg: string, where: Pt = at) =>
			out.push(finding(g, "power", "error", s.ref, `power symbol ${s.ref} (${text(s.lib)}): ${msg}`, where, [s.ref]));
		const comp = component(g, at, g.wires);
		const others: (PinRef & { at: Pt })[] = [];
		for (const [k, q] of comp.pts) for (const x of g.pins.get(k)?.list ?? []) if (x.ref !== s.ref) others.push({ ...x, at: q });
		const single =
			new Set(others.map(o => key(o.at))).size === 1 &&
			!g.labels.some(l => comp.pts.has(key(l.at))) &&
			![...comp.pts.keys()].some(k => g.junctions.has(k) || g.sheetPins.has(k));
		if (!single) {
			if (rot) bad(`rotated ${rot}°; power symbols are upright (rotation 0)`);
			continue;
		}
		const c = others.sort((a, b) => `${a.ref}.${a.num}`.localeCompare(`${b.ref}.${b.num}`))[0]!;
		const cname = `${c.ref}.${c.num}`;
		// orientation of the upright symbol: its pin leaves downward on supplies, upward on grounds
		const lp = libGeom(s.lib).pins[0]!;
		const supply = Math.round(lp.ang) === 90;
		const segs = [...comp.segs];
		const straight = segs.length === 1 && isEndPair(segs[0]!, c.at, at);
		const horizontal = c.dir === "L" || c.dir === "R";
		const [sx, sy] = DIR_VEC[c.dir];
		if (rot) {
			// sideways exception: a wired pin of the same part in the same column, where the L would go
			const sg = supply ? -1 : 1;
			const nbWired = [2, 3, 4].some(n => {
				const nb = pt(c.at[0], c.at[1] + sg * n * stub);
				return !!g.pins.get(key(nb))?.list.some(x => x.ref === c.ref) && touching(nb, g.wires).length > 0;
			});
			if (!(horizontal && nbWired && straight && samePt(at, pt(c.at[0] + stub * sx, c.at[1])))) {
				bad(
					`rotated ${rot}° on pin ${cname}; a sideways power symbol is only allowed on a horizontal pin whose wired neighbour pin blocks the L-shape, on a straight ${stub} mm wire`,
					c.at,
				);
			}
			continue;
		}
		if (!horizontal) {
			const exp = pt(c.at[0], c.at[1] + sy * stub);
			if ((c.dir === "D") === supply) bad(`pin ${cname} faces ${c.dir === "D" ? "down" : "up"} but the symbol points the other way`, c.at);
			else if (!(straight && samePt(at, exp))) bad(`must sit ${stub} mm straight off pin ${cname} on one wire`, c.at);
			continue;
		}
		const corner = pt(c.at[0] + sx * stub, c.at[1]);
		const exp = pt(corner[0], c.at[1] + (supply ? -stub : stub));
		const ok =
			segs.length === 2 &&
			samePt(at, exp) &&
			segs.some(w => isEndPair(w, c.at, corner)) &&
			segs.some(w => isEndPair(w, corner, exp));
		if (!ok) bad(`on horizontal pin ${cname} it must sit ${stub} mm out and ${stub} mm ${supply ? "up" : "down"} on an L-shaped wire`, c.at);
	}
	return out;
}

function isEndPair(s: Seg, a: Pt, b: Pt): boolean {
	return (samePt(s.a, a) && samePt(s.b, b)) || (samePt(s.a, b) && samePt(s.b, a));
}

// ---- R3 underline -----------------------------------------------------------------------------

const BUS_NAME = /\[\d+\.\.\d+\]|\{.*\}/;

function underline(g: Geo): Finding[] {
	const bad: { label: Label; why: string }[] = [];
	for (const l of g.labels) {
		if (l.kind !== "label") continue;
		const segs = BUS_NAME.test(l.name) ? g.buses : g.wires;
		const on = touching(l.at, segs);
		const horiz = on.filter(s => s.a[1] === s.b[1]);
		const vert = on.filter(s => s.a[0] === s.b[0]);
		if (l.rot === 90 || l.rot === 270) {
			if (!vert.length) bad.push({ label: l, why: "vertical label not on a vertical wire" });
			continue;
		}
		if (!horiz.length) {
			if (!vert.length) bad.push({ label: l, why: "label not on a wire" });
			continue;
		}
		const w = g.widths.get(l.name) ?? l.name.length * CHAR_W;
		const y = l.at[1];
		const hs = segs.filter(s => s.a[1] === y && s.b[1] === y);
		const sgn = l.rot === 0 ? 1 : -1;
		let x = l.at[0];
		let cov = 0;
		for (;;) {
			const ends = hs
				.filter(s => Math.min(s.a[0], s.b[0]) - 1e-6 <= x && x <= Math.max(s.a[0], s.b[0]) + 1e-6)
				.map(s => (sgn > 0 ? Math.max(s.a[0], s.b[0]) : Math.min(s.a[0], s.b[0])))
				.filter(e => e * sgn > x * sgn + 1e-6);
			if (!ends.length) break;
			const far = sgn > 0 ? Math.max(...ends) : Math.min(...ends);
			cov += Math.abs(far - x);
			x = far;
		}
		if (cov + 1e-6 < w) {
			bad.push({
				label: l,
				why: `the wire under the text covers ${r3(cov)} of ${r3(w)} mm (${sgn > 0 ? "right" : "left"} of the anchor)`,
			});
		}
	}
	// same-named labels: number the failing ones in uuid order, which moves don't change
	const seen = new Map<string, number>();
	return bad
		.sort((a, b) => uuidOf(a.label.node).localeCompare(uuidOf(b.label.node)))
		.map(({ label, why }) => {
			const n = (seen.get(label.name) ?? 0) + 1;
			seen.set(label.name, n);
			return finding(g, "underline", "error", `${label.name}|${n}`, `label ${label.name}: ${why}`, label.at);
		});
}

// ---- overlap / frame --------------------------------------------------------------------------

/** KiCad draws horizontal field text with left/right flipped on symbols turned 90° or 180° (mirror y = mirror x + 180°). */
function justFor(just: string, rot: number, mirror: string | undefined): string {
	const r = mirror === "y" ? (rot + 180) % 360 : rot;
	if (r !== 90 && r !== 180) return just;
	return just === "left" ? "right" : just === "right" ? "left" : just;
}

/** Boxes of the visible fields as KiCad draws them. */
function fieldBoxes(g: Geo, s: SymInfo): Box[] {
	const { rot, mirror } = g.sch.pose(s.sym);
	const out: Box[] = [];
	for (const p of children(s.sym, "property")) {
		if (isHidden(p)) continue;
		const name = text(p) ?? "";
		const value = name === "Reference" ? s.ref : (text(p, 2) ?? "");
		if (!value) continue;
		const eff = child(p, "effects");
		const size = child(child(eff ?? [], "font") ?? [], "size");
		const scale = size ? num(size, 2) / 1.27 : 1;
		const just = (child(eff ?? [], "justify") ?? []).slice(1).flatMap(a => (isList(a) ? [] : [justFor(a.v, rot, mirror)]));
		const a = child(p, "at");
		const [x, y] = [num(a, 1), num(a, 2)];
		const vertical = Math.round(((a && a.length > 3 ? num(a, 3) : 0) + rot) / 90) % 2 === 1;
		let w = g.widths.get(value) ?? Math.max(value.length, 1) * CHAR_W * scale;
		let h = TEXT_H * scale;
		if (vertical) {
			[w, h] = [h, w];
			out.push([x - w / 2, y - h / 2, x + w / 2, y + h / 2]);
		} else if (just.includes("left")) out.push([x, y - h / 2, x + w, y + h / 2]);
		else if (just.includes("right")) out.push([x - w, y - h / 2, x, y + h / 2]);
		else out.push([x - w / 2, y - h / 2, x + w / 2, y + h / 2]);
	}
	return out;
}

/** What a symbol draws, one box each: body primitives and visible pin lines, and visible field text. */
function drawn(g: Geo, s: SymInfo): { graphics: Box[]; texts: Box[] } {
	const { x, y, rot, mirror } = g.sch.pose(s.sym);
	const unit = g.sch.unit(s.sym);
	const style = g.sch.bodyStyle(s.sym);
	const lib = libGeom(s.lib);
	// libGeom pushes one pin shape per pin, in pin order
	const lines = lib.shapes.filter(sh => sh.pin);
	const pinLines = lib.pins.flatMap((p, i) =>
		p.hidden || !(p.unit === 0 || p.unit === unit) || !(p.style === 0 || p.style === style)
			? []
			: [bbox(lines[i]!.pts.map(q => xform(q[0], q[1], x, y, rot, mirror)))],
	);
	return { graphics: [...g.sch.bodyShapes(s.sym), ...pinLines], texts: fieldBoxes(g, s) };
}

function overlaps(g: Geo): Finding[] {
	const syms = g.syms.map(s => ({ s, ...drawn(g, s) }));
	const hit = (as: Box[], bs: Box[], margin: number): Box | undefined => {
		for (const a of as) for (const b of bs) if (overlap(a, b, margin)) return a;
		return undefined;
	};
	const out: Finding[] = [];
	for (let i = 0; i < syms.length; i++) {
		for (let j = i + 1; j < syms.length; j++) {
			const A = syms[i]!;
			const B = syms[j]!;
			if (A.s.ref === B.s.ref) continue;
			const graphic = hit(A.graphics, B.graphics, GRAPHIC_MARGIN);
			const where =
				graphic ??
				hit(A.texts, [...B.graphics, ...B.texts], TEXT_MARGIN) ??
				hit(B.texts, A.graphics, TEXT_MARGIN);
			if (!where) continue;
			const [r1, r2] = [A.s.ref, B.s.ref].sort() as [string, string];
			out.push(
				finding(
					g,
					"overlap",
					graphic ? "error" : "warning",
					`${r1}|${r2}`,
					graphic
						? `${r1} and ${r2} overlap: their bodies or pins come within ${GRAPHIC_MARGIN} mm`
						: `${r1} and ${r2} overlap: a visible field comes within ${TEXT_MARGIN} mm of the other symbol`,
					[r3((where[0] + where[2]) / 2), r3((where[1] + where[3]) / 2)],
					[r1, r2],
				),
			);
		}
	}
	return out;
}

function frame(g: Geo): Finding[] {
	const [W, H] = g.sch.paper();
	const border: Box = [FRAME_INSET, FRAME_INSET, W - FRAME_INSET, H - FRAME_INSET];
	const title: Box = [W - TITLE_BLOCK[0], H - TITLE_BLOCK[1], W, H];
	const outside = (b: Box) => b[0] < border[0] || b[1] < border[1] || b[2] > border[2] || b[3] > border[3] || overlap(b, title);
	const out: Finding[] = [];
	for (const s of g.syms) {
		const bs = [g.sch.bodyBox(s.sym), ...fieldBoxes(g, s)];
		const b = bs.find(outside);
		if (b) out.push(finding(g, "frame", "error", s.ref, `${s.ref} reaches outside the drawing frame or onto the title block`, [b[0], b[1]], [s.ref]));
	}
	for (const b of g.sch.sheets()) {
		const box: Box = [b.at[0], b.at[1], b.at[0] + b.size[0], b.at[1] + b.size[1]];
		if (outside(box)) out.push(finding(g, "frame", "error", `sheet ${b.name}`, `sheet block ${b.name} reaches outside the drawing frame or onto the title block`, b.at));
	}
	return out;
}

// ---- off_grid ---------------------------------------------------------------------------------

function offGrid(g: Geo): Finding[] {
	const on = (p: Pt) => onGrid(p[0], g.grid) && onGrid(p[1], g.grid);
	const out: Finding[] = [];
	const bad = (subj: string, what: string, at: Pt, refs?: string[]) =>
		out.push(finding(g, "off_grid", "error", subj, `${what} is off the ${g.grid} mm grid`, at, refs));
	// wiring items have no name of their own: name them after the net they're on
	const badOn = (kind: string, what: string, at: Pt, segs: readonly Seg[]) => {
		const subj = subject(g, at, segs);
		bad(`${kind}|${subj}`, `${what} (${subj})`, at);
	};
	for (const s of g.syms) {
		const p = g.sch.pins(s.sym).find(q => !on(q.at));
		if (p) bad(s.ref, `pin ${s.ref}.${p.num}`, p.at, [s.ref]);
	}
	for (const [kind, segs] of [["wire", g.wires], ["bus", g.buses]] as const) {
		for (const s of segs) {
			const p = [s.a, s.b].find(q => !on(q));
			if (p) badOn(kind, `a ${kind} end`, p, segs);
		}
	}
	for (const l of g.labels) if (!on(l.at)) bad(`label|${l.name}`, `${l.kind.replace("_", " ")} ${l.name}`, l.at);
	for (const [kind, pts] of [["junction", g.junctions], ["no_connect", g.ncs]] as const) {
		for (const p of pts.values()) if (!on(p)) badOn(kind, `a ${kind.replace("_", "-")}`, p, g.wires);
	}
	for (const p of g.entryEnds.values()) if (!on(p)) badOn("bus_entry", "a bus entry end", p, g.wires);
	for (const { at, name } of g.sheetPins.values()) if (!on(at)) bad(`sheet_pin|${name}`, `sheet pin ${name}`, at);
	return out;
}

// ---- wiring -----------------------------------------------------------------------------------

function wiring(g: Geo): Finding[] {
	const out: Finding[] = [];
	const add = (kind: string, subj: string, message: string, at?: Pt, refs?: string[]) =>
		out.push(finding(g, `wiring|${kind}`, WIRING_SEVERITY[kind]!, subj, message, at, refs));
	const ends = new Map<string, { at: Pt; n: number }>();
	for (const s of g.wires) {
		for (const p of [s.a, s.b]) {
			const e = ends.get(key(p)) ?? { at: p, n: 0 };
			e.n++;
			ends.set(key(p), e);
		}
	}
	const onWireInterior = (p: Pt) => g.wires.some(s => onInterior(p, s));

	for (const [k, { at, list }] of g.pins) {
		for (const x of list) {
			const pin = `${x.ref}.${x.num}`;
			if (onWireInterior(at)) add("pin_on_wire", pin, `pin ${pin} sits on the middle of a wire (KiCad connects it)`, at, [x.ref]);
			if (g.ncs.has(k) && ends.has(k)) add("nc_on_wired_pin", pin, `pin ${pin} has a no-connect flag and a wire`, at, [x.ref]);
		}
		const refs = [...new Set(list.map(x => x.ref))].sort();
		if (refs.length > 1) {
			const names = refs.map(r => `${r}.${list.find(x => x.ref === r)!.num}`);
			add("pin_collision", names.join("|"), `pins ${names.join(" and ")} sit on the same point and connect without a wire`, at, refs);
		}
	}

	const labelPts = new Set(g.labels.map(l => key(l.at)));
	for (const [k, { at, n }] of ends) {
		const interior = onWireInterior(at);
		if (
			n === 1 &&
			!g.pins.has(k) &&
			!labelPts.has(k) &&
			!g.junctions.has(k) &&
			!g.ncs.has(k) &&
			!g.entryEnds.has(k) &&
			!g.sheetPins.has(k) &&
			!interior
		) {
			const subj = subject(g, at, g.wires);
			add("dangling", subj, `a wire end connects to nothing (${subj})`, at);
		}
		if (interior && !g.junctions.has(k)) {
			const subj = subject(g, at, g.wires);
			add("missing_junction", subj, `a wire ends on the middle of another wire without a junction (${subj})`, at);
		}
	}
	const busTees = new Map<string, Pt>();
	for (const s of g.buses) for (const p of [s.a, s.b]) if (!g.junctions.has(key(p)) && g.buses.some(b => onInterior(p, b))) busTees.set(key(p), p);
	for (const p of busTees.values()) {
		const subj = `bus ${subject(g, p, g.buses)}`;
		add("missing_junction", subj, `a bus ends on the middle of another bus without a junction (${subj})`, p);
	}

	const jc = g.sch.junctionContext();
	for (const j of g.junctions.values()) {
		const n = g.sch.junctionDegree(j, jc);
		if (n >= 3) continue;
		const subj = subject(g, j, g.wires);
		add("redundant_junction", subj, `a junction joins only ${n} connection(s) (${subj}); remove it`, j);
	}

	for (const s of g.syms) {
		const prims = g.sch.bodyShapes(s.sym);
		for (const w of g.wires) {
			const sb = bbox([w.a, w.b]);
			if (!prims.some(pb => overlap(sb, pb, -BODY_EDGE))) continue;
			const subj = subject(g, w.a, g.wires);
			add("wire_over_body", `${s.ref}|${subj}`, `a wire (${subj}) crosses the body of ${s.ref}`, w.a, [s.ref]);
		}
	}

	const onAny = (p: Pt, segs: readonly Seg[]) => segs.some(s => touches(p, s));
	for (const e of g.sch.busEntries()) {
		if ((onAny(e.a, g.buses) && onAny(e.b, g.wires)) || (onAny(e.b, g.buses) && onAny(e.a, g.wires))) continue;
		const wireEnd = [e.a, e.b].find(p => onAny(p, g.wires));
		const subj = wireEnd ? subject(g, wireEnd, g.wires) : `bus ${subject(g, e.a, g.buses)}`;
		add("bus_entry_unattached", subj, `a bus entry doesn't join a bus to a wire (${subj})`, e.a);
	}
	for (const s of g.wires) {
		for (const p of [s.a, s.b]) {
			if (!onAny(p, g.buses)) continue;
			const subj = subject(g, p, g.wires);
			add("wire_end_on_bus", subj, `a wire touches a bus directly instead of through a bus entry (${subj})`, p);
		}
	}
	for (const p of g.entryEnds.values()) {
		if (!touching(p, g.wires).some(s => samePt(s.a, p) || samePt(s.b, p))) continue;
		const { pts } = component(g, p, g.wires);
		if (g.labels.some(l => pts.has(key(l.at)))) continue;
		const subj = subject(g, p, g.wires);
		add("unlabeled_bus_wire", subj, `a wire from a bus entry carries no net label naming the bus member (${subj})`, p);
	}
	return out;
}
