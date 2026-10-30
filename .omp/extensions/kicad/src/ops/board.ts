/**
 * Board ops: stack-up, board-wide rules, outline and cutouts, KiCad's "Update PCB from Schematic"
 * and footprint placement. Board millimetres, x right, y down, rotations counter-clockwise.
 */
import * as fs from "node:fs";
import {
	COPPER_FINISHES,
	courtyardBox,
	extentBox,
	type Dielectric,
	fieldsInfo,
	flipToBack,
	fpAttrs,
	fpId,
	fpRef,
	instantiate,
	placement,
	removeProperty,
	setAttrFlags,
	setFieldPose,
	setLink,
	setLocked,
	setPadNet,
	setPlacement,
	setProperty,
} from "../board";
import * as cli from "../cli";
import { boardOutline, checkCutouts, type EdgeShape, encodeEdge, isEdgeItem, normalShape, shapeIn, shapePolygon } from "../edges";
import { type Box, bbox, type Pt } from "../geom";
import type { Project } from "../project";
import { newUuid } from "../sch";
import { child, children, isList, type List, parse, text } from "../sexpr";
import { type Fields, validate } from "../spec";
import type { OpDef } from "../types";

/** `board.design_settings` of the .kicad_pro, as KiCad 10 writes it. */
export interface DesignSettings {
	rules: Record<string, unknown>;
	rule_severities: Record<string, string>;
	defaults?: Record<string, unknown>;
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** The .kicad_pro's `board.design_settings`, or undefined when KiCad hasn't written one (with its rules and severities). */
export function boardSettings(project: Project): DesignSettings | undefined {
	const board = project.proJson.data.board;
	const ds = isObject(board) ? board.design_settings : undefined;
	if (!isObject(ds) || !isObject(ds.rules) || !isObject(ds.rule_severities)) return undefined;
	// KiCad's own output: the severities are its strings, the defaults its numbers
	const settings: DesignSettings = { ...ds, rules: ds.rules, rule_severities: ds.rule_severities as Record<string, string>, defaults: isObject(ds.defaults) ? ds.defaults : undefined };
	return settings;
}

function designSettings(project: Project): DesignSettings {
	const ds = boardSettings(project);
	if (!ds) throw new Error(`${project.rel(project.proFile)} has no board.design_settings rules: open and save the board in KiCad 10 once`);
	return ds;
}

/** The board defaults pcbnew styles new items with (.kicad_pro board.design_settings.defaults). */
export function boardDefaults(project: Project): { edgeWidth: number; fabFont: { size: [number, number]; thickness: number } } {
	const d = boardSettings(project)?.defaults ?? {};
	const n = (k: string, dflt: number) => {
		const v = d[k];
		return typeof v === "number" ? v : dflt;
	};
	return {
		edgeWidth: n("board_outline_line_width", 0.05),
		fabFont: { size: [n("fab_text_size_h", 1), n("fab_text_size_v", 1)], thickness: n("fab_text_thickness", 0.15) },
	};
}

const fmtMm = (v: number) => `${Math.round(v * 1e4) / 1e4}`;

// ---- stack-up -------------------------------------------------------------------------------------

const DIELECTRIC: Fields = {
	thickness: { t: "number", req: true, doc: "mm" },
	type: { t: "string", values: ["prepreg", "core"], doc: "default: the current layer's, else prepreg/core alternating from the top (core for a 2-layer board)" },
	material: { t: "string", doc: 'default: the current layer\'s, else "FR4"' },
	epsilon_r: { t: "number", doc: "relative permittivity (default: the current layer's, else 4.5)" },
	loss_tangent: { t: "number", doc: "default: the current layer's, else 0.02" },
};

const setStackup: OpDef = {
	op: "set_stackup",
	group: "board",
	doc: "Set the board's stack-up (Board Setup > Board Stackup) for its copper layers; the board thickness becomes the total of masks, copper and dielectrics, as pcbnew sums it.",
	fields: {
		dielectrics: { t: "objects", req: true, doc: "one per gap between copper layers, top first (copper layers − 1): {thickness, type?: prepreg|core, material?, epsilon_r?, loss_tangent?}" },
		copper: { t: "numbers", doc: "copper thickness per copper layer, F.Cu first, mm (default: the current ones, 0.035 where unset)" },
		finish: { t: "string", values: COPPER_FINISHES, doc: "copper finish (default: the current one)" },
		mask_thickness: { t: "number", doc: "solder mask thickness, both sides, mm (default: the current ones, 0.01 where unset)" },
	},
	apply(ctx, args) {
		const board = ctx.project.board();
		const copper = board.copperLayers();
		const raw = args.dielectrics as Record<string, unknown>[];
		if (raw.length !== copper.length - 1) {
			throw new Error(`dielectrics: the board has ${copper.length} copper layers (${copper.join(", ")}), so give ${copper.length - 1} dielectric(s), got ${raw.length}`);
		}
		const old = board.stackup();
		const dielectrics = raw.map((d, i): Dielectric => {
			validate(`dielectrics[${i}]`, d, DIELECTRIC);
			const was = old.dielectrics[i];
			const out: Dielectric = {
				thickness: d.thickness as number,
				type: (d.type as Dielectric["type"] | undefined) ?? was?.type ?? (raw.length === 1 || i % 2 === 1 ? "core" : "prepreg"),
				material: (d.material as string | undefined) ?? was?.material ?? "FR4",
				epsilon_r: (d.epsilon_r as number | undefined) ?? was?.epsilon_r ?? 4.5,
				loss_tangent: (d.loss_tangent as number | undefined) ?? was?.loss_tangent ?? 0.02,
			};
			if (!(out.thickness > 0)) throw new Error(`dielectrics[${i}].thickness must be positive`);
			if (!(out.epsilon_r >= 1)) throw new Error(`dielectrics[${i}].epsilon_r must be at least 1`);
			if (!(out.loss_tangent >= 0)) throw new Error(`dielectrics[${i}].loss_tangent must be >= 0`);
			if (!out.material.trim()) throw new Error(`dielectrics[${i}].material is empty`);
			return out;
		});
		const cu = (args.copper as number[] | undefined) ?? old.copper.map(t => t || 0.035);
		if (cu.length !== copper.length) throw new Error(`copper: give ${copper.length} thicknesses (${copper.join(", ")}), got ${cu.length}`);
		if (cu.some(t => !(t > 0))) throw new Error("copper: thicknesses must be positive");
		const m = args.mask_thickness as number | undefined;
		if (m !== undefined && !(m > 0)) throw new Error("mask_thickness must be positive");
		const mask: [number, number] = m !== undefined ? [m, m] : [old.mask[0] || 0.01, old.mask[1] || 0.01];
		const finish = (args.finish as string | undefined) ?? old.finish;
		const total = board.setStackup({ dielectrics, copper: cu, mask, finish });
		ctx.project.touchBoard();
		ctx.touchBoard();
		ctx.notes.push(
			`stack-up: ${copper.length} copper layers, dielectrics ${dielectrics.map(d => `${fmtMm(d.thickness)} ${d.type}`).join(" / ")}, finish ${finish}; board thickness ${fmtMm(total)} mm (was ${fmtMm(old.thickness)})`,
		);
	},
};

// ---- board rules ----------------------------------------------------------------------------------

const SEVERITIES = ["error", "warning", "ignore"] as const;

const setBoardRules: OpDef = {
	op: "set_board_rules",
	group: "board",
	doc: "Set board-wide design rule minimums and DRC severities (Board Setup > Constraints / Violation Severity, kept in the .kicad_pro); only the given keys change. Keys are KiCad's own, as the board query lists them.",
	fields: {
		rules: { t: "object", doc: "{<key>: number}: mm (min_resolved_spokes: a count), e.g. min_clearance, min_track_width, min_via_diameter, min_via_annular_width, min_through_hole_diameter, min_hole_to_hole, min_hole_clearance, min_copper_edge_clearance, min_silk_clearance, min_text_height" },
		severities: { t: "object", doc: '{<DRC rule>: "error"|"warning"|"ignore"}, e.g. {"silk_overlap": "ignore"}' },
	},
	apply(ctx, args) {
		const rules = (args.rules ?? {}) as Record<string, unknown>;
		const sev = (args.severities ?? {}) as Record<string, unknown>;
		if (!Object.keys(rules).length && !Object.keys(sev).length) throw new Error("give rules and/or severities");
		const ds = designSettings(ctx.project);
		const numeric = Object.keys(ds.rules).filter(k => typeof ds.rules[k] === "number");
		const changes: string[] = [];
		for (const [k, v] of Object.entries(rules)) {
			if (!numeric.includes(k)) throw new Error(`rules: unknown key "${k}"; KiCad's keys: ${numeric.join(", ")}`);
			if (typeof v !== "number" || !Number.isFinite(v) || v < 0) throw new Error(`rules.${k}: expected a number >= 0, got ${JSON.stringify(v)}`);
			if (k === "min_resolved_spokes" && !(Number.isInteger(v) && v <= 4)) throw new Error(`rules.${k}: a whole number of spokes, 0..4`);
			if (ds.rules[k] !== v) changes.push(`${k} ${ds.rules[k]} → ${v}`);
			ds.rules[k] = v;
		}
		const names = Object.keys(ds.rule_severities);
		for (const [k, v] of Object.entries(sev)) {
			if (!names.includes(k)) throw new Error(`severities: unknown DRC rule "${k}"; KiCad's rules: ${names.join(", ")}`);
			if (typeof v !== "string" || !(SEVERITIES as readonly string[]).includes(v)) throw new Error(`severities.${k}: one of ${SEVERITIES.join(", ")}, got ${JSON.stringify(v)}`);
			if (ds.rule_severities[k] !== v) changes.push(`${k}: ${ds.rule_severities[k]} → ${v}`);
			ds.rule_severities[k] = v;
		}
		if (!changes.length) {
			ctx.notes.push("board rules: already as given");
			return;
		}
		ctx.project.touchPro();
		ctx.touchBoard();
		ctx.notes.push(`board rules: ${changes.join(", ")}`);
	},
};

// ---- outline --------------------------------------------------------------------------------------

function describeShape(s: EdgeShape): string {
	if ("rect" in s) return `rect ${fmtMm(s.rect.width)} × ${fmtMm(s.rect.height)} mm at (${fmtMm(s.rect.x)}, ${fmtMm(s.rect.y)})${s.rect.corner_radius ? `, corners R${fmtMm(s.rect.corner_radius)}` : ""}`;
	if ("circle" in s) return `circle R${fmtMm(s.circle.radius)} at (${fmtMm(s.circle.center[0])}, ${fmtMm(s.circle.center[1])})`;
	return `polygon of ${s.points.length} vertices${s.points.some(p => p[2]) ? ` (${s.points.filter(p => p[2]).length} filleted)` : ""}`;
}

const setBoardEdges: OpDef = {
	op: "set_board_edges",
	group: "board",
	doc:
		"Replace the board outline: every board-level Edge.Cuts drawing (not those of footprints) is removed and the outline plus the cutouts drawn (lines and tangent arcs; rect without corners and circle as one item). " +
		"A polygon vertex [x, y, r] gets a fillet of radius r, concave corners too (e.g. R0.5 mill radius in a notch); each fillet must fit its two edges. " +
		"Polygons are kept in a normal form, as the board query reads them back: starting at the top-most then left-most vertex, clockwise on screen; cutouts read back top to bottom, then left to right.",
	fields: {
		outline: { t: "object", req: true, doc: "{rect: {x, y, width, height, corner_radius?}} (x, y: top-left corner) or {points: [[x, y, r?], …]} (≥ 3 vertices, a simple polygon)" },
		cutouts: { t: "objects", doc: "holes strictly inside the outline, apart from each other: {circle: {center, radius}} | {rect: {…}} | {points: […]}" },
	},
	apply(ctx, args) {
		const outline = shapeIn(args.outline, "outline", false);
		const cutouts = ((args.cutouts as unknown[] | undefined) ?? []).map((c, i) => shapeIn(c, `cutouts[${i}]`, true));
		checkCutouts(outline, cutouts);
		const board = ctx.project.board();
		const old = board.tree.filter((c): c is List => isList(c) && isEdgeItem(c));
		for (const it of old) board.remove(it);
		const width = boardDefaults(ctx.project).edgeWidth;
		board.add(...[outline, ...cutouts].flatMap(s => encodeEdge(normalShape(s), width, newUuid)));
		ctx.project.touchBoard();
		ctx.touchBoard(bbox(shapePolygon(outline)));
		ctx.notes.push(
			`outline: ${describeShape(outline)}${cutouts.length ? `; cutouts: ${cutouts.map(describeShape).join("; ")}` : ""} (replaced ${old.length} Edge.Cuts item${old.length === 1 ? "" : "s"})`,
		);
	},
};

// ---- update from schematic ------------------------------------------------------------------------

/** A footprint's link path for a symbol: the sheet's uuid path ("/" on the root) plus the symbol's uuid. */
export function linkPath(c: cli.Component, uuid: string): string {
	return `${c.sheetPath.endsWith("/") ? c.sheetPath : `${c.sheetPath}/`}${uuid}`;
}

/** Fields every board footprint has (the Footprint field is its library id, not a property). */
const MANDATORY: Record<string, true> = { Reference: true, Value: true, Datasheet: true, Description: true };

type PinNets = Map<string, Map<string, { name: string; pinfunction?: string; pintype?: string }>>;

/** Bring a footprint in line with its symbol; true when anything changed. */
function syncFootprint(fp: List, c: cli.Component, pins: PinNets, font: { size: [number, number]; thickness: number }, missingPads: string[]): boolean {
	let changed = false;
	changed = setProperty(fp, "Reference", c.ref, font, newUuid) || changed;
	changed = setProperty(fp, "Value", c.value, font, newUuid) || changed;
	// the symbol's fields but Reference, Value and Footprint (the footprint id itself); Datasheet and Description always exist
	const fields: Record<string, string> = { Datasheet: c.datasheet, Description: c.description };
	for (const [k, v] of Object.entries(c.fields)) if (k !== "Reference" && k !== "Value" && k !== "Footprint") fields[k] = v;
	for (const [k, v] of Object.entries(fields)) changed = setProperty(fp, k, v, font, newUuid) || changed;
	for (const p of children(fp, "property")) {
		const name = text(p, 1) ?? "";
		if (!MANDATORY[name] && !(name in fields) && !name.startsWith("ki_")) changed = removeProperty(fp, name) || changed;
	}
	changed = (c.fpFilters ? setProperty(fp, "ki_fp_filters", c.fpFilters, font, newUuid) : removeProperty(fp, "ki_fp_filters")) || changed;
	changed = setLink(fp, { path: linkPath(c, c.uuids[0]!), sheetname: c.sheetName, sheetfile: c.sheetFile }) || changed;
	changed = setAttrFlags(fp, { dnp: c.dnp, exclude_from_bom: c.excludeFromBom }) || changed;
	const mine = pins.get(c.ref) ?? new Map();
	const padNumbers = new Set<string>();
	for (const pad of children(fp, "pad")) {
		const n = text(pad, 1) ?? "";
		padNumbers.add(n);
		changed = setPadNet(pad, n ? mine.get(n) : undefined) || changed;
	}
	for (const n of mine.keys()) if (!padNumbers.has(n)) missingPads.push(`${c.ref}.${n}`);
	return changed;
}

const GAP = 1;
const GROUP_GAP = 5;

/**
 * Place new footprints in blocks, one per sheet in hierarchy order, left to right from `start`
 * (top-left); inside a block, rows of footprints by height, GAP mm apart (courtyards, pads,
 * silkscreen and fab drawings and visible texts included, so neighbours don't touch).
 */
function pack(items: { fp: List; sheet: string }[], sheetOrder: string[], start: Pt): Box | undefined {
	const groups = new Map<string, { fp: List; box: Box }[]>();
	const rank = (s: string) => (sheetOrder.includes(s) ? sheetOrder.indexOf(s) : sheetOrder.length);
	for (const it of [...items].sort((a, b) => rank(a.sheet) - rank(b.sheet))) {
		const g = groups.get(it.sheet) ?? [];
		// everything it draws (texts included), or 1 mm around the origin when it draws nothing
		g.push({ fp: it.fp, box: extentBox(it.fp) ?? [-0.5, -0.5, 0.5, 0.5] });
		groups.set(it.sheet, g);
	}
	let gx = start[0];
	const placed: Pt[] = [];
	const r2 = (v: number) => Math.round(v * 100) / 100;
	for (const g of groups.values()) {
		g.sort((a, b) => b.box[3] - b.box[1] - (a.box[3] - a.box[1]) || fpRef(a.fp).localeCompare(fpRef(b.fp), undefined, { numeric: true }));
		const area = g.reduce((t, { box }) => t + (box[2] - box[0] + GAP) * (box[3] - box[1] + GAP), 0);
		const rowW = Math.max(...g.map(({ box }) => box[2] - box[0] + GAP), Math.sqrt(area) * 1.25);
		let [x, y, rowH, usedW] = [0, 0, 0, 0];
		for (const { fp, box } of g) {
			const [w, h] = [box[2] - box[0] + GAP, box[3] - box[1] + GAP];
			if (x > 0 && x + w > rowW) {
				x = 0;
				y += rowH;
				rowH = 0;
			}
			// round the origin up so the courtyard never moves into the previous one's gap
			const at: Pt = [r2(Math.ceil((gx + x - box[0]) * 100) / 100), r2(Math.ceil((start[1] + y - box[1]) * 100) / 100)];
			setPlacement(fp, at, 0);
			placed.push([at[0] + box[0], at[1] + box[1]], [at[0] + box[2], at[1] + box[3]]);
			x += w + 0.01;
			rowH = Math.max(rowH, h + 0.01);
			usedW = Math.max(usedW, x);
		}
		gx += usedW + GROUP_GAP;
	}
	return placed.length ? bbox(placed) : undefined;
}

const updateFromSchematic: OpDef = {
	op: "update_from_schematic",
	group: "board",
	doc:
		"KiCad's Update PCB from Schematic for every symbol on the board (DNP ones too): adds each missing footprint from the libraries with its symbol link, fields (hidden on the fab layer), attributes (dnp, exclude_from_bom), pad nets and 3D models; refreshes existing footprints (fields, attributes, nets); a changed Footprint field replaces the footprint in place (position, rotation, side and lock kept). " +
		"New footprints go outside the outline, to its right (right of x = 0 without one), one block per sheet, courtyards apart. Footprints without a symbol are listed, removed only with remove_extra. Run it in its own kicad_edit call after schematic changes are applied.",
	fields: {
		remove_extra: { t: "boolean", doc: "remove footprints whose symbol is gone or not on the board (default false; board-only footprints stay)" },
		update_existing: { t: "boolean", doc: "refresh and replace existing footprints (default true); false only adds missing ones" },
	},
	async apply(ctx, args) {
		const { project } = ctx;
		const dirtySheets = project.hierarchy().filter(s => project.isDirty(s.file));
		if (dirtySheets.length || project.created.size) {
			throw new Error("this call also changes the schematic: apply the schematic edit first, then update_from_schematic in a separate kicad_edit call");
		}
		const nl = await cli.netlist(project.rootFile, { cwd: project.dir, repoRoot: project.repoRoot });
		const board = project.board();
		const { fabFont } = boardDefaults(project);
		const pins: PinNets = new Map();
		for (const net of nl.nets) {
			for (const n of net.nodes) {
				const m = pins.get(n.ref) ?? new Map();
				m.set(n.pin, { name: net.name, pinfunction: n.pinfunction, pintype: n.pintype });
				pins.set(n.ref, m);
			}
		}
		const byPath = new Map<string, List>();
		for (const fp of board.footprints()) {
			const p = text(children(fp, "path")[0]);
			if (p) byPath.set(p, fp);
		}
		const libFp = (id: string) => parse(fs.readFileSync(ctx.libs.footprintFile(id), "utf8"));
		const added: { fp: List; sheet: string }[] = [];
		const updated: string[] = [];
		const replaced: string[] = [];
		const replacedHow: string[] = [];
		const noFootprint: string[] = [];
		const missingPads: string[] = [];
		const matched = new Set<List>();
		const touchedBoxes: Box[] = [];
		const comps = [...nl.components.values()].filter(c => !c.excludeFromBoard).sort((a, b) => a.ref.localeCompare(b.ref, undefined, { numeric: true }));
		for (const c of comps) {
			if (!c.uuids.length) throw new Error(`${c.ref}: the netlist gives no symbol uuid`);
			if (!c.footprint) {
				noFootprint.push(c.ref);
				continue;
			}
			const existing = c.uuids.map(u => byPath.get(linkPath(c, u))).find(f => f);
			if (!existing) {
				const fp = instantiate(libFp(c.footprint), c.footprint, newUuid);
				syncFootprint(fp, c, pins, fabFont, missingPads);
				board.add(fp);
				added.push({ fp, sheet: c.sheetName });
				continue;
			}
			matched.add(existing);
			if (args.update_existing === false) continue;
			if (fpId(existing) !== c.footprint) {
				const pl = placement(existing);
				const fp = instantiate(libFp(c.footprint), c.footprint, newUuid);
				if (pl.side === "bottom") flipToBack(fp);
				setPlacement(fp, pl.at, pl.rot);
				setLocked(fp, pl.locked);
				syncFootprint(fp, c, pins, fabFont, missingPads);
				board.tree[board.tree.indexOf(existing)] = fp;
				matched.add(fp);
				replaced.push(c.ref);
				replacedHow.push(`${c.ref} (${fpId(existing)} → ${c.footprint})`);
				const b = courtyardBox(fp)?.box;
				if (b) touchedBoxes.push(b);
				continue;
			}
			if (syncFootprint(existing, c, pins, fabFont, missingPads)) {
				updated.push(c.ref);
				const b = courtyardBox(existing)?.box;
				if (b) touchedBoxes.push(b);
			}
		}
		const extra = board.footprints().filter(fp => !matched.has(fp) && !added.some(a => a.fp === fp) && !fpAttrs(fp).includes("board_only"));
		if (args.remove_extra) for (const fp of extra) board.remove(fp);

		if (added.length) {
			// right of the outline, of every footprint already there, and of x = 0
			const outline = boardOutline(board.tree.filter((c): c is List => isList(c) && isEdgeItem(c)));
			const others = board.footprints().filter(fp => !added.some(a => a.fp === fp)).map(fp => courtyardBox(fp)?.box).filter((b): b is Box => !!b);
			const right = Math.max(outline.bbox ? outline.bbox[2] : 0, ...others.map(b => b[2]));
			const top = outline.bbox ? outline.bbox[1] : 0;
			const region = pack(added, project.hierarchy().map(s => s.namePath), [Math.ceil(right + 10), Math.floor(top)]);
			if (region) touchedBoxes.push(region);
		}
		const refs = (l: string[]) => `${l.slice(0, 40).join(", ")}${l.length > 40 ? `, … (${l.length})` : ""}`;
		const addedRefs = added.map(a => fpRef(a.fp)).sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
		const extraRefs = extra.map(fpRef);
		ctx.notes.push(
			`update_from_schematic: ${added.length} added, ${updated.length} updated, ${replaced.length} replaced, ${args.remove_extra ? extra.length : 0} removed`,
			...(added.length ? [`added (outside the outline): ${refs(addedRefs)}`] : []),
			...(updated.length ? [`updated: ${refs(updated)}`] : []),
			...(replaced.length ? [`replaced (Footprint field changed): ${refs(replacedHow)}`] : []),
			...(extraRefs.length ? [`${args.remove_extra ? "removed" : "kept (no symbol; remove_extra removes them)"}: ${refs(extraRefs)}`] : []),
			...(noFootprint.length ? [`no Footprint field, not on the board: ${refs(noFootprint)}`] : []),
			...(missingPads.length ? [`symbol pins without a pad of that number: ${refs(missingPads)}`] : []),
		);
		if (added.length || updated.length || replaced.length || (args.remove_extra && extra.length)) {
			project.touchBoard();
			ctx.touchBoard(touchedBoxes.length ? bbox(touchedBoxes.flatMap(b => [[b[0], b[1]] as Pt, [b[2], b[3]] as Pt])) : undefined, [...addedRefs, ...replaced]);
		}
	},
};

// ---- placement ------------------------------------------------------------------------------------

const placeFootprint: OpDef = {
	op: "place_footprint",
	group: "board",
	doc: "Move and rotate a footprint on its side (no side flip): pads, texts and drawings move with it, texts keeping their pose relative to it. A locked footprint moves only with locked: false. Several footprints: several ops in one call.",
	fields: {
		ref: { t: "string", req: true, doc: "reference" },
		at: { t: "point", req: true, doc: "footprint origin, board mm" },
		rot: { t: "number", doc: "rotation in degrees, counter-clockwise (default: unchanged)" },
		locked: { t: "boolean", doc: "lock or unlock it" },
	},
	apply(ctx, args) {
		const board = ctx.project.board();
		const ref = String(args.ref);
		const fp = board.footprint(ref);
		const old = placement(fp);
		const at = args.at as Pt;
		const rot = (args.rot as number | undefined) ?? old.rot;
		const moves = at[0] !== old.at[0] || at[1] !== old.at[1] || rot !== old.rot;
		if (moves && old.locked && args.locked !== false) throw new Error(`${ref} is locked: give locked: false to unlock and move it`);
		const before = courtyardBox(fp)?.box;
		setPlacement(fp, at, rot);
		if (typeof args.locked === "boolean") setLocked(fp, args.locked);
		const after = courtyardBox(fp)?.box;
		ctx.project.touchBoard();
		const boxes = [before, after].filter((b): b is Box => !!b);
		ctx.touchBoard(boxes.length ? bbox(boxes.flatMap(b => [[b[0], b[1]] as Pt, [b[2], b[3]] as Pt])) : undefined);
		const p = placement(fp);
		ctx.notes.push(`${ref} at (${fmtMm(p.at[0])}, ${fmtMm(p.at[1])}), ${fmtMm(p.rot)}°${p.locked ? ", locked" : ""}`);
	},
};

// ---- field texts ----------------------------------------------------------------------------------

const setFootprintField: OpDef = {
	op: "set_footprint_field",
	group: "board",
	doc:
		"Move, turn, show/hide or re-layer one field text of a footprint (Reference, Value or any other field), as pcbnew's field properties do: position by `at` or `offset` (exactly one when moving), absolute board rotation, justification unchanged. " +
		"A field put on a back layer (B.*) is mirrored and one put on a front layer is not, as KiCad draws them. The footprint query lists each field's position, rotation, layer and hidden flag. Several fields: several ops in one call.",
	fields: {
		ref: { t: "string", req: true, doc: "footprint reference" },
		field: { t: "string", req: true, doc: 'field name, e.g. "Reference", "Value"' },
		at: { t: "point", doc: "text position, board mm" },
		offset: { t: "point", doc: "text position as [dx, dy] mm from the footprint origin, board axes (x right, y down)" },
		rot: { t: "number", doc: "text rotation, absolute board degrees counter-clockwise (default: unchanged)" },
		hidden: { t: "boolean", doc: "hide (true) or show (false) the text" },
		layer: { t: "string", doc: 'board layer, e.g. "F.SilkS", "F.Fab", "B.SilkS"' },
	},
	apply(ctx, args) {
		if (args.at !== undefined && args.offset !== undefined) throw new Error("give at or offset, not both");
		if (["at", "offset", "rot", "hidden", "layer"].every(k => args[k] === undefined)) throw new Error("give at, offset, rot, hidden and/or layer");
		const board = ctx.project.board();
		const ref = String(args.ref);
		const name = String(args.field);
		const fp = board.footprint(ref);
		const props = children(fp, "property").filter(p => child(p, "at"));
		const prop = props.find(p => text(p, 1) === name);
		if (!prop) throw new Error(`${ref} has no field "${name}" (fields: ${props.map(p => text(p, 1)).join(", ")})`);
		const layer = args.layer as string | undefined;
		const layers = board.layers().map(l => l.name);
		if (layer !== undefined && !layers.includes(layer)) throw new Error(`layer: the board has no layer "${layer}" (layers: ${layers.join(", ")})`);
		const p = placement(fp);
		const offset = args.offset as Pt | undefined;
		const at = (args.at as Pt | undefined) ?? (offset ? [p.at[0] + offset[0], p.at[1] + offset[1]] : undefined);
		const before = extentBox(fp);
		if (!setFieldPose(fp, prop, { at, rot: args.rot as number | undefined, hidden: args.hidden as boolean | undefined, layer })) {
			ctx.notes.push(`${ref} ${name}: already as given`);
			return;
		}
		ctx.project.touchBoard();
		const boxes = [before, extentBox(fp)].filter((b): b is Box => !!b);
		ctx.touchBoard(boxes.length ? bbox(boxes.flatMap(b => [[b[0], b[1]] as Pt, [b[2], b[3]] as Pt])) : undefined);
		const f = fieldsInfo(fp).find(x => x.name === name)!;
		ctx.notes.push(`${ref} ${name} at (${fmtMm(f.at[0])}, ${fmtMm(f.at[1])}), ${fmtMm(f.rot)}°, ${f.layer}${f.mirrored ? " (mirrored)" : ""}, ${f.hidden ? "hidden" : "shown"}`);
	},
};

export const ops: OpDef[] = [setStackup, setBoardRules, setBoardEdges, updateFromSchematic, placeFootprint, setFootprintField];
