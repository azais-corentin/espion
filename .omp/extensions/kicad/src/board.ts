/**
 * A KiCad 10 board (.kicad_pcb, file version 20260206): layers, stack-up, footprints and their
 * geometry in board millimetres (x right, y down, rotations counter-clockwise as pcbnew shows them).
 *
 * The board format as KiCad 10.0.7 writes it (`kicad-cli pcb upgrade` re-saves of its demo boards
 * and of boards the toolkit wrote, checked with `kicad-cli pcb drc --schematic-parity`):
 * - A footprint's children are stored footprint-relative and unrotated (pads, texts, drawings),
 *   but the angle in a pad's or text's `(at x y angle)` is absolute: footprint rotation plus the
 *   item's own. Zones inside a footprint are stored in board coordinates.
 * - A back-side footprint's children are stored already flipped (B.* layers, mirrored y).
 * - The link to the symbol: `(path "/<sheet uuid>/…/<symbol uuid>")` without the root sheet's uuid
 *   (`"/<symbol uuid>"` on the root sheet; the first unit's uuid for multi-unit symbols),
 *   `(sheetname "/Power/")` (the netlist's sheet path names) and `(sheetfile "power.kicad_sch")`.
 * - No net table: each pad names its net, `(net "GND")`, with `(pinfunction "…")` and
 *   `(pintype "…")` exactly as the netlist's node gives them (KiCad 10 writes a named pin's
 *   function with its number, "K_1"). Unconnected symbol pins get KiCad's `unconnected-(…)` nets
 *   from the netlist like any other.
 * - `(locked yes)` right after the footprint's library id; `(attr smd|through_hole [board_only]
 *   [exclude_from_pos_files] [exclude_from_bom] [allow_missing_courtyard] [dnp] …)`. The symbol
 *   sets dnp and exclude_from_bom (netlist properties); exclude_from_pos_files stays the library
 *   footprint's (the netlist doesn't carry it).
 * - Fields the symbol doesn't have (e.g. a stock footprint's KiLib_Generator) go: parity compares
 *   the footprint's fields with the symbol's.
 * - Every footprint carries the Datasheet and Description fields (hidden, F.Fab); the symbol's
 *   other fields become hidden F.Fab properties at the footprint origin; the symbol's footprint
 *   filters go in `(property ki_fp_filters "…")`. The Footprint field is not written.
 * - Text angles (footprint fields and texts) are absolute and KiCad re-saves them in [0, 360)
 *   ("-90" becomes "270"); pad angles stay in (-180, 180]. A field is `(property "Name" "value"
 *   (at x y angle) [(unlocked yes)] (layer "F.SilkS" [knockout]) [(hide yes)] (uuid …) (effects
 *   (font …) [(justify [left|right] [top|bottom] [mirror])]))`; visible fields carry no hide.
 * - gr_rect has no corner radius: rounded outlines are gr_line + gr_arc.
 */
import * as fs from "node:fs";
import { mm } from "./fpgfx";
import { arcPoints } from "./edges";
import { type Box, bbox, type Pt } from "./geom";
import { child, children, clone, flag, head, isList, L, type List, num, parse, print, removeChild, Str, str, Sym, text } from "./sexpr";

/** pcbnew's "Board Setup > Board Stackup" finish list (KiCad 10, stackup_predefined_prms.cpp). */
export const COPPER_FINISHES = [
	"None",
	"ENIG",
	"ENEPIG",
	"HAL SnPb",
	"HAL lead-free",
	"Hard gold",
	"Immersion tin",
	"Immersion nickel",
	"Immersion silver",
	"Immersion gold",
	"HT_OSP",
	"OSP",
] as const;

export interface BoardLayer {
	/** KiCad's ordinal (F.Cu 0, B.Cu 2, In1.Cu 4, …). */
	id: number;
	name: string;
	type: string;
	/** The name the user gave the layer, when KiCad stores one. */
	user?: string;
}

export interface Dielectric {
	thickness: number;
	type: "prepreg" | "core";
	material: string;
	epsilon_r: number;
	loss_tangent: number;
}

export interface Stackup {
	dielectrics: Dielectric[];
	/** Copper thickness per copper layer, F.Cu first. */
	copper: number[];
	finish: string;
	/** Solder mask thickness (front, back). */
	mask: [number, number];
	/** `(general (thickness …))`: the board thickness KiCad uses. */
	thickness: number;
	/** The sum of the stack-up's layers (copper, dielectrics, masks), as pcbnew computes the board thickness from it. */
	total: number;
}

export interface Placement {
	at: Pt;
	/** Degrees, counter-clockwise. */
	rot: number;
	side: "top" | "bottom";
	locked: boolean;
}

/** nm resolution: KiCad's board unit. */
export const r6 = (v: number) => Math.round(v * 1e6) / 1e6 + 0;

/** An angle in KiCad's range for footprint and pad orientations, (-180, 180]. */
export function normAngle(a: number): number {
	let x = r6(a % 360);
	if (x <= -180) x += 360;
	if (x > 180) x -= 360;
	return x + 0;
}

/** A text angle as KiCad writes it for footprint fields and texts, [0, 360). */
export function textAngle(a: number): number {
	const x = ((r6(a) % 360) + 360) % 360;
	return x === 360 ? 0 : x + 0;
}

export class Board {
	private constructor(
		readonly file: string,
		readonly tree: List,
	) {}

	static load(file: string): Board {
		return new Board(file, parse(fs.readFileSync(file, "utf8")));
	}

	serialize(): string {
		return `${print(this.tree)}\n`;
	}

	layers(): BoardLayer[] {
		return (child(this.tree, "layers") ?? []).filter(isList).map(l => ({
			id: num(l, 0),
			name: text(l, 1) ?? "",
			type: text(l, 2) ?? "",
			...(text(l, 3) ? { user: text(l, 3) } : {}),
		}));
	}

	/** Copper layer names, front to back (KiCad lists them in stack order). */
	copperLayers(): string[] {
		return this.layers()
			.filter(l => l.name.endsWith(".Cu"))
			.map(l => l.name);
	}

	footprints(): List[] {
		return children(this.tree, "footprint");
	}

	/** The footprint with this reference; throws listing a few when there is none. */
	footprint(ref: string): List {
		const all = this.footprints();
		const hit = all.find(f => fpRef(f) === ref);
		if (!hit) {
			const refs = all.map(fpRef).sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
			throw new Error(`no footprint ${ref} on the board${refs.length ? ` (${refs.length} footprints: ${refs.slice(0, 20).join(", ")}${refs.length > 20 ? ", …" : ""})` : ": it has no footprints"}`);
		}
		return hit;
	}

	/** Insert items where KiCad keeps board items: before the trailing `(embedded_fonts …)`. */
	add(...items: List[]): void {
		const at = this.tree.findIndex(c => isList(c) && head(c) === "embedded_fonts");
		this.tree.splice(at < 0 ? this.tree.length : at, 0, ...items);
	}

	remove(item: List): void {
		const i = this.tree.indexOf(item);
		if (i >= 0) this.tree.splice(i, 1);
	}

	// ---- stack-up -----------------------------------------------------------------------------

	stackup(): Stackup {
		const su = child(child(this.tree, "setup") ?? [], "stackup") ?? [];
		const layers = children(su, "layer");
		const thick = (l: List | undefined) => (l && child(l, "thickness") ? num(child(l, "thickness")) : 0);
		const byName = (n: string) => layers.find(l => text(l, 1) === n);
		const dielectrics = layers
			.filter(l => /^dielectric \d+$/.test(text(l, 1) ?? ""))
			.map(
				(l): Dielectric => ({
					// sub-layers (addsublayer) add up; the first one's material speaks for the layer
					thickness: r6(children(l, "thickness").reduce((t, c) => t + num(c), 0)),
					type: text(child(l, "type")) === "prepreg" ? "prepreg" : "core",
					material: text(child(l, "material")) ?? "FR4",
					epsilon_r: child(l, "epsilon_r") ? num(child(l, "epsilon_r")) : 4.5,
					loss_tangent: child(l, "loss_tangent") ? num(child(l, "loss_tangent")) : 0.02,
				}),
			);
		const copper = this.copperLayers().map(n => thick(byName(n)));
		const mask: [number, number] = [thick(byName("F.Mask")), thick(byName("B.Mask"))];
		const total = r6(layers.reduce((t, l) => t + children(l, "thickness").reduce((s, c) => s + num(c), 0), 0));
		const general = child(child(this.tree, "general") ?? [], "thickness");
		return {
			dielectrics,
			copper,
			finish: text(child(su, "copper_finish")) ?? "None",
			mask,
			thickness: general ? num(general) : 1.6,
			total,
		};
	}

	/**
	 * Replace the stack-up with these dielectrics, copper and mask thicknesses and finish, in
	 * KiCad's layer order (silk, paste, mask, copper/dielectric …, mask, paste, silk), keeping the
	 * other settings of existing entries (colours, mask material); `(general (thickness))` becomes
	 * the stack-up's total.
	 */
	setStackup(s: { dielectrics: Dielectric[]; copper: number[]; mask: [number, number]; finish: string }): number {
		const setup = child(this.tree, "setup") ?? L("setup");
		if (!this.tree.includes(setup)) this.tree.splice(this.tree.findIndex(c => isList(c) && head(c) === "layers") + 1, 0, setup);
		const old = child(setup, "stackup");
		const oldLayers = new Map((old ? children(old, "layer") : []).map(l => [text(l, 1) ?? "", l]));
		const keep = (name: string, type: string): List => oldLayers.get(name) ?? L("layer", str(name), L("type", str(type)));
		const withThickness = (l: List, t: number): List => {
			const out = l.filter(c => !(isList(c) && head(c) === "thickness"));
			out.splice(Math.min(3, out.length), 0, L("thickness", mm(t)));
			return out;
		};
		const copper = this.copperLayers();
		const list: List[] = [keep("F.SilkS", "Top Silk Screen"), keep("F.Paste", "Top Solder Paste"), withThickness(keep("F.Mask", "Top Solder Mask"), s.mask[0])];
		copper.forEach((name, i) => {
			list.push(withThickness(keep(name, "copper"), s.copper[i]!));
			const d = s.dielectrics[i];
			if (d) {
				list.push(
					L(
						"layer",
						str(`dielectric ${i + 1}`),
						L("type", str(d.type)),
						L("thickness", mm(d.thickness)),
						L("material", str(d.material)),
						L("epsilon_r", d.epsilon_r),
						L("loss_tangent", d.loss_tangent),
					),
				);
			}
		});
		list.push(withThickness(keep("B.Mask", "Bottom Solder Mask"), s.mask[1]), keep("B.Paste", "Bottom Solder Paste"), keep("B.SilkS", "Bottom Silk Screen"));
		const rest = old ? old.slice(1).filter(c => !(isList(c) && (head(c) === "layer" || head(c) === "copper_finish"))) : [L("dielectric_constraints", false)];
		const stackup = L("stackup", ...list, L("copper_finish", str(s.finish)), ...rest);
		if (old) setup[setup.indexOf(old)] = stackup;
		else setup.splice(1, 0, stackup);
		const total = r6(s.mask[0] + s.mask[1] + s.copper.reduce((a, b) => a + b, 0) + s.dielectrics.reduce((a, d) => a + d.thickness, 0));
		const general = child(this.tree, "general");
		if (general) {
			removeChild(general, "thickness");
			general.splice(1, 0, L("thickness", mm(total)));
		} else this.tree.splice(this.tree.findIndex(c => isList(c) && head(c) === "generator_version") + 1, 0, L("general", L("thickness", mm(total))));
		return total;
	}
}

// ---- footprints -----------------------------------------------------------------------------------

export function fpRef(fp: List): string {
	return fpProp(fp, "Reference") ?? "";
}

/** A property's value; `ki_fp_filters` and other keyword-named properties included. */
export function fpProp(fp: List, name: string): string | undefined {
	const p = children(fp, "property").find(x => text(x, 1) === name);
	return p ? (text(p, 2) ?? "") : undefined;
}

export function fpId(fp: List): string {
	return text(fp, 1) ?? "";
}

export function fpAttrs(fp: List): string[] {
	return (child(fp, "attr") ?? []).slice(1).flatMap(a => (isList(a) ? [] : [a.v]));
}

export function placement(fp: List): Placement {
	const at = child(fp, "at");
	return {
		at: [num(at, 1), num(at, 2)],
		rot: at && at.length > 3 ? num(at, 3) : 0,
		side: text(child(fp, "layer")) === "B.Cu" ? "bottom" : "top",
		locked: flag(fp, "locked") === true || fp.some(c => c instanceof Sym && c.v === "locked"),
	};
}

/** Board point of a footprint-relative point. */
export function toBoard(p: Pick<Placement, "at" | "rot">, l: Pt): Pt {
	const t = (p.rot * Math.PI) / 180;
	const [c, s] = [Math.cos(t), Math.sin(t)];
	return [r6(p.at[0] + l[0] * c + l[1] * s), r6(p.at[1] - l[0] * s + l[1] * c)];
}

/** Footprint-relative point of a board point. */
export function toLocal(p: Pick<Placement, "at" | "rot">, b: Pt): Pt {
	const t = (p.rot * Math.PI) / 180;
	const [c, s] = [Math.cos(t), Math.sin(t)];
	const [dx, dy] = [b[0] - p.at[0], b[1] - p.at[1]];
	return [dx * c - dy * s, dx * s + dy * c];
}

const DRAWING = /^fp_(line|rect|circle|arc|poly|curve)$/;

const pt = (l: List | undefined): Pt => [num(l, 1), num(l, 2)];

/** Points a drawing passes through or bounds (arcs and circles as points along them), footprint-relative. */
function drawingPoints(item: List): Pt[] {
	switch (head(item)) {
		case "fp_line":
			return [pt(child(item, "start")), pt(child(item, "end"))];
		case "fp_rect": {
			const [a, b] = [pt(child(item, "start")), pt(child(item, "end"))];
			return [a, [b[0], a[1]], b, [a[0], b[1]]];
		}
		case "fp_circle": {
			const c = pt(child(item, "center"));
			const e = pt(child(item, "end"));
			const r = Math.hypot(e[0] - c[0], e[1] - c[1]);
			return Array.from({ length: 72 }, (_, i) => [c[0] + r * Math.cos((i * Math.PI) / 36), c[1] + r * Math.sin((i * Math.PI) / 36)] as Pt);
		}
		case "fp_arc":
			return arcPoints(pt(child(item, "start")), pt(child(item, "mid")), pt(child(item, "end")));
		case "fp_poly":
		case "fp_curve":
			return (child(item, "pts") ?? []).filter(isList).flatMap(p => (head(p) === "xy" ? [pt(p)] : head(p) === "arc" ? arcPoints(pt(child(p, "start")), pt(child(p, "mid")), pt(child(p, "end"))) : []));
		default:
			return [];
	}
}

/** Corners of a pad's size rectangle, footprint-relative (pad angles in the file are absolute). */
function padCorners(pad: List, fpRot: number): Pt[] {
	const at = child(pad, "at");
	const c = pt(at);
	const own = ((at && at.length > 3 ? num(at, 3) : 0) - fpRot) * (Math.PI / 180);
	const size = child(pad, "size");
	const [w, h] = size ? [num(size, 1) / 2, num(size, 2) / 2] : [0, 0];
	const [co, si] = [Math.cos(own), Math.sin(own)];
	return [
		[-w, -h],
		[w, -h],
		[w, h],
		[-w, h],
	].map(([x, y]) => [c[0] + x! * co + y! * si, c[1] - x! * si + y! * co] as Pt);
}

/**
 * Courtyard box of a footprint, board mm: its F/B.CrtYd drawings, or its pads and fab drawings
 * when it has no courtyard (`source` says which).
 */
export function courtyardBox(fp: List): { box: Box; source: "courtyard" | "pads+body" } | undefined {
	const p = placement(fp);
	const drawings = fp.filter((c): c is List => isList(c) && DRAWING.test(head(c) ?? ""));
	const court = drawings.filter(d => /\.CrtYd$/.test(text(child(d, "layer")) ?? ""));
	let local: Pt[];
	let source: "courtyard" | "pads+body";
	if (court.length) {
		local = court.flatMap(drawingPoints);
		source = "courtyard";
	} else {
		local = [...children(fp, "pad").flatMap(pad => padCorners(pad, p.rot)), ...drawings.filter(d => /\.Fab$/.test(text(child(d, "layer")) ?? "")).flatMap(drawingPoints)];
		source = "pads+body";
	}
	if (!local.length) return undefined;
	const b = bbox(local.map(q => toBoard(p, q)));
	return { box: [r6(b[0]), r6(b[1]), r6(b[2]), r6(b[3])], source };
}

/**
 * Courtyard outline of a footprint as edges, board mm: its F/B.CrtYd lines, rects, polygons, arcs
 * and circles (arcs and circles as short chords), the footprint's rotation applied; the four edges
 * of courtyardBox when it has no courtyard drawings. A loop drawn as separate lines works too:
 * `insideEdges` counts crossings.
 */
export function courtyardEdges(fp: List): [Pt, Pt][] {
	const p = placement(fp);
	const court = fp.filter((c): c is List => isList(c) && DRAWING.test(head(c) ?? "") && /\.CrtYd$/.test(text(child(c, "layer")) ?? ""));
	if (!court.length) {
		const b = courtyardBox(fp)?.box;
		if (!b) return [];
		const c: Pt[] = [
			[b[0], b[1]],
			[b[2], b[1]],
			[b[2], b[3]],
			[b[0], b[3]],
		];
		return c.map((q, i) => [q, c[(i + 1) % 4]!]);
	}
	return court.flatMap(d => {
		const pts = drawingPoints(d).map(q => toBoard(p, q));
		const closed = head(d) === "fp_rect" || head(d) === "fp_circle" || head(d) === "fp_poly";
		return Array.from({ length: Math.max(0, closed ? pts.length : pts.length - 1) }, (_, i): [Pt, Pt] => [pts[i]!, pts[(i + 1) % pts.length]!]);
	});
}

/** Is p inside the closed outline these edges draw (even-odd crossings, so the edges may come in any order)? */
export function insideEdges(p: Pt, edges: readonly (readonly [Pt, Pt])[]): boolean {
	let inside = false;
	for (const [a, b] of edges) {
		if (a[1] > p[1] !== b[1] > p[1] && p[0] < ((b[0] - a[0]) * (p[1] - a[1])) / (b[1] - a[1]) + a[0]) inside = !inside;
	}
	return inside;
}

/** Corners of a visible text's box (stroke font: about one glyph size per character), footprint-relative. */
function textCorners(item: List, fpRot: number, vars: { ref: string; value: string }): Pt[] {
	const at = child(item, "at");
	const effects = child(item, "effects");
	const font = child(effects ?? [], "font");
	const size = child(font ?? [], "size");
	const [w0, h0] = size ? [num(size, 2), num(size, 1)] : [1, 1];
	const thick = child(font ?? [], "thickness") ? num(child(font!, "thickness")) : 0.15;
	// what KiCad draws: the reference and value variables expanded
	const shown = (text(item, 2) ?? "").replace(/\$\{REFERENCE\}/g, vars.ref).replace(/\$\{VALUE\}/g, vars.value);
	const w = Math.max(1, shown.length) * w0 + thick;
	const h = h0 + thick;
	const just = (child(effects ?? [], "justify") ?? []).flatMap(a => (isList(a) ? [] : [a.v]));
	const x0 = just.includes("left") ? 0 : just.includes("right") ? -w : -w / 2;
	const y0 = just.includes("top") ? 0 : just.includes("bottom") ? -h : -h / 2;
	const c = pt(at);
	const own = ((at && at.length > 3 ? num(at, 3) : 0) - fpRot) * (Math.PI / 180);
	const [co, si] = [Math.cos(own), Math.sin(own)];
	return [
		[x0, y0],
		[x0 + w, y0],
		[x0 + w, y0 + h],
		[x0, y0 + h],
	].map(([x, y]) => [c[0] + x! * co + y! * si, c[1] - x! * si + y! * co] as Pt);
}

/** Box of everything a footprint draws on copper, silkscreen, fab and courtyard layers (visible texts included), board mm. */
export function extentBox(fp: List): Box | undefined {
	const p = placement(fp);
	const vars = { ref: fpRef(fp), value: fpProp(fp, "Value") ?? "" };
	const layerOk = (c: List) => /\.(Cu|SilkS|Fab|CrtYd)$/.test(text(child(c, "layer")) ?? "");
	const local: Pt[] = [
		...children(fp, "pad").flatMap(pad => padCorners(pad, p.rot)),
		...fp.filter((c): c is List => isList(c) && DRAWING.test(head(c) ?? "") && layerOk(c)).flatMap(drawingPoints),
		...fp
			.filter((c): c is List => isList(c) && (head(c) === "property" || head(c) === "fp_text") && layerOk(c) && flag(c, "hide") !== true && !!child(c, "at"))
			.flatMap(t => textCorners(t, p.rot, vars)),
	];
	if (!local.length) return undefined;
	const b = bbox(local.map(q => toBoard(p, q)));
	return [r6(b[0]), r6(b[1]), r6(b[2]), r6(b[3])];
}

export interface PadInfo {
	number: string;
	net?: string;
	pinfunction?: string;
	pintype?: string;
	/** Board position, mm. */
	at: Pt;
	/** Absolute pad angle, degrees (when not 0). */
	rot?: number;
	size?: [number, number];
	type: string;
	shape: string;
	layers: string[];
}

export function padsInfo(fp: List): PadInfo[] {
	const p = placement(fp);
	return children(fp, "pad").map(pad => {
		const at = child(pad, "at");
		const size = child(pad, "size");
		const rot = at && at.length > 3 ? num(at, 3) : 0;
		const net = text(child(pad, "net"));
		return {
			number: text(pad, 1) ?? "",
			...(net ? { net } : {}),
			...(text(child(pad, "pinfunction")) ? { pinfunction: text(child(pad, "pinfunction")) } : {}),
			...(text(child(pad, "pintype")) ? { pintype: text(child(pad, "pintype")) } : {}),
			at: toBoard(p, pt(at)),
			...(rot ? { rot } : {}),
			...(size ? { size: [num(size, 1), num(size, 2)] as [number, number] } : {}),
			type: text(pad, 2) ?? "",
			shape: text(pad, 3) ?? "",
			layers: (child(pad, "layers") ?? []).slice(1).flatMap(a => (isList(a) ? [] : [a.v])),
		};
	});
}

/** Items of a footprint that carry `(at x y angle)` with an absolute angle. */
const ANGLED: Record<string, true> = { pad: true, property: true, fp_text: true };

/** Every `(xy x y)` of a footprint's zones (stored in board coordinates). */
function zonePoints(fp: List): List[] {
	const out: List[] = [];
	const walk = (l: List) => {
		for (const c of l) {
			if (!isList(c)) continue;
			if (head(c) === "xy") out.push(c);
			else walk(c);
		}
	};
	for (const z of children(fp, "zone")) walk(z);
	return out;
}

/**
 * Move and rotate a footprint as pcbnew does: its children keep their footprint-relative
 * positions, pad and text angles turn with it, its zones (board coordinates) move with it.
 */
export function setPlacement(fp: List, at: Pt, rot: number): void {
	const old = placement(fp);
	const nrot = normAngle(rot);
	const delta = nrot - old.rot;
	for (const xy of zonePoints(fp)) {
		const q = toBoard({ at, rot: nrot }, toLocal(old, [num(xy, 1), num(xy, 2)]));
		xy[1] = mm(q[0]);
		xy[2] = mm(q[1]);
	}
	if (delta) {
		for (const c of fp) {
			if (!isList(c) || !ANGLED[head(c) ?? ""]) continue;
			const a = child(c, "at");
			if (!a) continue;
			const own = (a.length > 3 ? num(a, 3) : 0) + delta;
			if (head(c) === "pad") {
				const ang = normAngle(own);
				a.splice(3, a.length - 3, ...(ang ? [mm(ang)] : []));
			} else a.splice(3, a.length - 3, mm(textAngle(own)));
		}
	}
	const atNode = L("at", mm(at[0]), mm(at[1]), ...(nrot ? [mm(nrot)] : []));
	const i = fp.findIndex(c => isList(c) && head(c) === "at");
	if (i >= 0) fp[i] = atNode;
	else fp.splice(headerEnd(fp), 0, atNode);
}

/** Index after the footprint's header (`locked`, `layer`, `uuid`). */
function headerEnd(fp: List): number {
	let i = 2;
	while (i < fp.length && (fp[i] instanceof Sym || (isList(fp[i]) && ["locked", "placed", "layer", "uuid"].includes(head(fp[i] as List) ?? "")))) i++;
	return i;
}

export function setLocked(fp: List, locked: boolean): void {
	for (let i = fp.length - 1; i >= 2; i--) {
		const c = fp[i]!;
		if ((c instanceof Sym && c.v === "locked") || (isList(c) && head(c) === "locked")) fp.splice(i, 1);
	}
	if (locked) fp.splice(2, 0, L("locked", true));
}

// ---- footprint content ----------------------------------------------------------------------------

/** Footprint children that get their own uuid on a board. */
const ITEMS = /^(property|fp_text|fp_text_box|fp_line|fp_rect|fp_circle|fp_arc|fp_poly|fp_curve|pad|zone|dimension|group|image|barcode|table)$/;

/**
 * A library footprint made into a board footprint at the origin, unrotated, on the front: the
 * library header dropped, the board id set, a fresh uuid for it and each of its items (group
 * members follow their items' new uuids).
 */
export function instantiate(lib: List, id: string, uuid: () => string): List {
	const fp: List = [lib[0]!, str(id)];
	const renamed = new Map<string, string>();
	for (const c of lib.slice(2)) {
		if (isList(c) && ["version", "generator", "generator_version", "uuid", "at"].includes(head(c) ?? "")) continue;
		const item = clone(c);
		if (isList(item) && ITEMS.test(head(item) ?? "")) {
			const fresh = uuid();
			const i = item.findIndex(x => isList(x) && head(x) === "uuid");
			if (i >= 0) renamed.set(text(item[i] as List) ?? "", fresh);
			if (i >= 0) item[i] = L("uuid", str(fresh));
			else item.push(L("uuid", str(fresh)));
		}
		fp.push(item);
	}
	for (const g of children(fp, "group")) {
		const members = child(g, "members");
		for (let i = 1; members && i < members.length; i++) {
			const m = members[i];
			if (m instanceof Str && renamed.has(m.v)) members[i] = str(renamed.get(m.v)!);
		}
	}
	if (!child(fp, "layer")) fp.splice(2, 0, L("layer", str("F.Cu")));
	const layerAt = fp.findIndex(c => isList(c) && head(c) === "layer");
	fp.splice(layerAt + 1, 0, L("uuid", str(uuid())), L("at", 0, 0));
	return fp;
}

/** Lists whose second atom is a y coordinate a flip mirrors (drill `offset` included; 3D model placement is left alone). */
const POINT: Record<string, true> = { at: true, start: true, end: true, mid: true, center: true, xy: true, offset: true };

/** Texts a flip mirrors and turns as KiCad's PCB_TEXT::Flip does. */
const TEXT: Record<string, true> = { property: true, fp_text: true, fp_text_box: true };

/**
 * Flip an unplaced footprint (origin, rotation 0) to the other side as pcbnew's top/bottom flip
 * (FOOTPRINT::Flip, TOP_BOTTOM) does: y mirrored; layers flipped as BOARD::FlipLayer does (F.* and
 * B.* swapped, the footprint's own layer too; inner copper layers In1 ↔ In(n−2) when `copper`, the
 * board's copper layers front to back, is given); pad angles negated; text angles a → 180° − a,
 * texts on side-specific (F.*, B.*) layers mirrored; trapezoid deltas and chamfered corners mirrored.
 * pcbnew's default left/right flip is this one turned by 180° (the footprint's rotation 180° − rot).
 */
export function flipToBack(fp: List, copper?: readonly string[]): void {
	const swap = (s: string) => {
		const k = copper?.indexOf(s) ?? -1;
		if (k >= 0) return copper![copper!.length - 1 - k]!;
		return s.startsWith("F.") ? `B.${s.slice(2)}` : s.startsWith("B.") ? `F.${s.slice(2)}` : s;
	};
	const topBottom = (s: string) => (s.startsWith("top_") ? `bottom_${s.slice(4)}` : s.startsWith("bottom_") ? `top_${s.slice(7)}` : s);
	const toggleMirror = (eff: List) => {
		const j = child(eff, "justify");
		if (!j) return eff.push(L("justify", "mirror"));
		const k = j.findIndex(a => a instanceof Sym && a.v === "mirror");
		if (k >= 0) j.splice(k, 1);
		else j.push(new Sym("mirror"));
	};
	const walk = (l: List, inText: boolean) => {
		const h = head(l) ?? "";
		if (POINT[h] && l.length > 2 && !isList(l[2])) {
			l[2] = mm(-num(l, 2));
			if (h === "at" && inText) l.splice(3, l.length - 3, mm(textAngle(180 - (l.length > 3 && !isList(l[3]) ? num(l, 3) : 0))));
			else if (h === "at" && l.length > 3 && !isList(l[3])) l[3] = mm(normAngle(-num(l, 3)));
		}
		if (h === "angle" && inText) l[1] = mm(textAngle(180 - num(l, 1)));
		if (h === "rect_delta") l[2] = mm(-num(l, 2));
		if (h === "layer" || h === "layers") for (let i = 1; i < l.length; i++) if (l[i] instanceof Str) l[i] = str(swap((l[i] as Str).v));
		if (h === "chamfer") for (let i = 1; i < l.length; i++) if (l[i] instanceof Sym) l[i] = new Sym(topBottom((l[i] as Sym).v));
		for (const c of l) if (isList(c) && head(c) !== "model" && head(c) !== "effects") walk(c, inText);
	};
	for (const c of fp.slice(2)) {
		if (!isList(c) || head(c) === "model") continue;
		const isText = TEXT[head(c) ?? ""] === true;
		const layer = text(child(c, "layer")) ?? "";
		if (isText && head(c) === "fp_text_box" && !child(c, "angle")) {
			const after = c.reduce((k, x, i) => (isList(x) && ["start", "end", "pts"].includes(head(x) ?? "") ? i : k), 1);
			c.splice(after + 1, 0, L("angle", 0));
		}
		walk(c, isText);
		// KiCad mirrors a flipped text only on a side-specific layer
		const eff = isText ? child(c, "effects") : undefined;
		if (eff && (layer.startsWith("F.") || layer.startsWith("B."))) toggleMirror(eff);
	}
}

/** Set a property's value, or add it as pcbnew adds a symbol field: hidden, on the fab layer, at the footprint origin. */
export function setProperty(fp: List, name: string, value: string, font: { size: [number, number]; thickness: number }, uuid: () => string): boolean {
	const p = children(fp, "property").find(x => text(x, 1) === name);
	if (p) {
		if (text(p, 2) === value) return false;
		p[2] = str(value);
		return true;
	}
	const pl = placement(fp);
	const node = L(
		"property",
		/^[A-Za-z_][A-Za-z0-9_]*$/.test(name) && name.startsWith("ki_") ? new Sym(name) : str(name),
		str(value),
		L("at", 0, 0, mm(textAngle(pl.rot))),
		...(name === "Datasheet" || name === "Description" ? [L("unlocked", true)] : []),
		L("layer", str(pl.side === "bottom" ? "B.Fab" : "F.Fab")),
		L("hide", true),
		L("uuid", str(uuid())),
		L("effects", L("font", L("size", mm(font.size[0]), mm(font.size[1])), L("thickness", mm(font.thickness))), ...(pl.side === "bottom" ? [L("justify", "mirror")] : [])),
	);
	const last = fp.reduce((k, c, i) => (isList(c) && head(c) === "property" ? i : k), -1);
	fp.splice(last >= 0 ? last + 1 : headerEnd(fp), 0, node);
	return true;
}

export function removeProperty(fp: List, name: string): boolean {
	const i = fp.findIndex(c => isList(c) && head(c) === "property" && text(c, 1) === name);
	if (i < 0) return false;
	fp.splice(i, 1);
	return true;
}

/** A footprint field as the board draws it. */
export interface FieldInfo {
	name: string;
	value: string;
	/** Board position of the text anchor, mm. */
	at: Pt;
	/** Absolute rotation, degrees counter-clockwise, as KiCad writes it: [0, 360). */
	rot: number;
	layer: string;
	hidden: boolean;
	/** Drawn mirrored (back-layer text). */
	mirrored: boolean;
}

const justifyAtoms = (prop: List): string[] => (child(child(prop, "effects") ?? [], "justify") ?? []).slice(1).flatMap(a => (isList(a) ? [] : [a.v]));

/** The fields of a footprint that are texts on the board (keyword fields such as ki_fp_filters have no position), in file order. */
export function fieldsInfo(fp: List): FieldInfo[] {
	const p = placement(fp);
	return children(fp, "property").flatMap((prop): FieldInfo[] => {
		const at = child(prop, "at");
		if (!at) return [];
		return [
			{
				name: text(prop, 1) ?? "",
				value: text(prop, 2) ?? "",
				at: toBoard(p, pt(at)),
				rot: at.length > 3 ? num(at, 3) : 0,
				layer: text(child(prop, "layer")) ?? "",
				hidden: flag(prop, "hide") === true,
				mirrored: justifyAtoms(prop).includes("mirror"),
			},
		];
	});
}

export interface FieldPose {
	/** Board position, mm. */
	at?: Pt;
	/** Absolute board rotation, degrees counter-clockwise. */
	rot?: number;
	hidden?: boolean;
	layer?: string;
}

/**
 * Move, turn, show or hide a footprint field, or put it on another layer, as pcbnew's field
 * properties do: the position is stored footprint-relative, the angle absolute; justification
 * stays; a field put on a back layer is mirrored and one put on a front layer is not, as KiCad
 * draws them. True when the field changed.
 */
export function setFieldPose(fp: List, prop: List, pose: FieldPose): boolean {
	const before = print(prop);
	const at = child(prop, "at");
	if (!at) throw new Error(`field ${text(prop, 1)} is not drawn on the board (no position)`);
	const local = pose.at ? toLocal(placement(fp), pose.at) : undefined;
	const angle = textAngle(pose.rot ?? (at.length > 3 ? num(at, 3) : 0));
	prop[prop.indexOf(at)] = L("at", local ? mm(local[0]) : at[1]!, local ? mm(local[1]) : at[2]!, mm(angle));
	if (pose.layer !== undefined) {
		const layer = child(prop, "layer");
		if (layer) layer[1] = str(pose.layer);
		else prop.splice(prop.indexOf(child(prop, "at")!) + 1, 0, L("layer", str(pose.layer)));
		let effects = child(prop, "effects");
		if (!effects) prop.push((effects = L("effects")));
		const atoms = justifyAtoms(prop).filter(a => a !== "mirror");
		if (pose.layer.startsWith("B.")) atoms.push("mirror");
		removeChild(effects, "justify");
		if (atoms.length) effects.push(L("justify", ...atoms.map(a => new Sym(a))));
	}
	if (pose.hidden !== undefined) {
		removeChild(prop, "hide");
		if (pose.hidden) prop.splice(prop.indexOf(child(prop, "layer") ?? child(prop, "at")!) + 1, 0, L("hide", true));
	}
	return print(prop) !== before;
}

/** Set (or add before `attr`) the symbol link: path, sheetname, sheetfile. */
export function setLink(fp: List, link: { path: string; sheetname: string; sheetfile: string }): boolean {
	let changed = false;
	const anchor = () => {
		const a = fp.findIndex(c => isList(c) && head(c) === "attr");
		return a >= 0 ? a : fp.findIndex(c => isList(c) && /^(fp_|pad|model|embedded_fonts)/.test(head(c) ?? ""));
	};
	for (const [k, v] of [
		["path", link.path],
		["sheetname", link.sheetname],
		["sheetfile", link.sheetfile],
	] as const) {
		const c = child(fp, k);
		if (c && text(c) === v) continue;
		changed = true;
		if (c) c[1] = str(v);
		else {
			const at = anchor();
			fp.splice(at < 0 ? fp.length : at, 0, L(k, str(v)));
		}
	}
	return changed;
}

/** Set or clear attribute flags (dnp, exclude_from_bom, …); the footprint type stays. */
export function setAttrFlags(fp: List, flags: Record<string, boolean>): boolean {
	let attr = child(fp, "attr");
	const have = fpAttrs(fp);
	const want = new Set(have);
	for (const [k, on] of Object.entries(flags)) {
		if (on) want.add(k);
		else want.delete(k);
	}
	if (want.size === have.length && have.every(a => want.has(a))) return false;
	// KiCad's attribute order: type first, then the flags in the order it writes them
	const order = ["smd", "through_hole", "board_only", "exclude_from_pos_files", "exclude_from_bom", "allow_missing_courtyard", "dnp", "allow_soldermask_bridges"];
	const atoms = [...want].sort((a, b) => (order.indexOf(a) + 1 || 99) - (order.indexOf(b) + 1 || 99)).map(a => new Sym(a));
	if (!attr) {
		attr = L("attr");
		const at = fp.findIndex(c => isList(c) && /^(fp_|pad|model|embedded_fonts|duplicate_pad)/.test(head(c) ?? ""));
		fp.splice(at < 0 ? fp.length : at, 0, attr);
	}
	attr.splice(1, attr.length - 1, ...atoms);
	return true;
}

/** Give a pad its net and pin data (or none); true when something changed. */
export function setPadNet(pad: List, net?: { name: string; pinfunction?: string; pintype?: string }): boolean {
	const before = [text(child(pad, "net")), text(child(pad, "pinfunction")), text(child(pad, "pintype"))].join("\u0000");
	for (const k of ["net", "pinfunction", "pintype"]) removeChild(pad, k);
	if (net) {
		const at = pad.findIndex(c => isList(c) && /^(uuid|teardrops|die_length|zone_connect|thermal|clearance|solder_|options|primitives)/.test(head(c) ?? ""));
		const items = [L("net", str(net.name)), ...(net.pinfunction ? [L("pinfunction", str(net.pinfunction))] : []), ...(net.pintype ? [L("pintype", str(net.pintype))] : [])];
		pad.splice(at < 0 ? pad.length : at, 0, ...items);
	}
	return before !== [text(child(pad, "net")), text(child(pad, "pinfunction")), text(child(pad, "pintype"))].join("\u0000");
}
