/**
 * Read-only views of a project for agents: hierarchy, symbols, nets, labels, net classes, rules,
 * BOM, sheet summaries and free space for new parts. Results are small JSON objects (positions as
 * [x, y] mm, nets by netlist name), never raw s-expressions.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { CHAR_W, TEXT_H } from "../analysis";
import { placementRefs, refPrefix } from "../annotate";
import { fpRef } from "../board";
import { boardIsEmpty } from "../checks/board";
import * as cli from "../cli";
import { EXCLUSION_KINDS, type Exclusion, exclusions, hiddenBy } from "../exclusions";
import { type Box, bbox, key, onInterior, overlap, type Pt, pt, r3, type Seg, samePt, xform } from "../geom";
import { druHeader, druRules } from "../ops/project";
import type { Project, SheetInst } from "../project";
import { at, isHidden, type Label, propValue, type Sch } from "../sch";
import { child, children, flag, head, isList, type List, num, text } from "../sexpr";
import type { QueryDef } from "../types";

// ---- shared helpers ----------------------------------------------------------------------------

/** Shell-style pattern (`*`, `?`), case-insensitive, whole string; no pattern matches everything. */
function matcher(pattern: unknown): (s: string) => boolean {
	if (typeof pattern !== "string") return () => true;
	const body = pattern.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".");
	const re = new RegExp(`^${body}$`, "i");
	return s => re.test(s);
}

/** "R12" → ["R", 12] so references sort R2 < R10. */
function refCmp(a: string, b: string): number {
	const [, pa = a, na = ""] = /^(.*?)(\d*)$/.exec(a) ?? [];
	const [, pb = b, nb = ""] = /^(.*?)(\d*)$/.exec(b) ?? [];
	return pa === pb ? Number(na) - Number(nb) : pa < pb ? -1 : 1;
}

function limitArg(v: unknown, dflt: number): number {
	return typeof v === "number" && v > 0 ? v : dflt;
}

/** Keep the first `limit` rows and say how many there were. */
function page<T>(rows: T[], limit: number, key: string): Record<string, unknown> {
	return rows.length > limit ? { total: rows.length, shown: limit, [key]: rows.slice(0, limit) } : { total: rows.length, [key]: rows };
}

const SIDE: Record<number, string> = { 0: "right", 90: "top", 180: "left", 270: "bottom" };

/** Page number of a sheet instance (sheet block instances, or sheet_instances for the root). */
function pageOf(inst: SheetInst, project: Project): string | undefined {
	if (!inst.block || !inst.parent) {
		return text(child(children(child(inst.sch.tree, "sheet_instances") ?? [], "path").find(p => text(p) === "/") ?? [], "page"));
	}
	for (const proj of children(child(inst.block.node, "instances") ?? [], "project")) {
		if (text(proj) !== project.name) continue;
		for (const p of children(proj, "path")) if (text(p) === inst.parent.path) return text(child(p, "page"));
	}
	return undefined;
}

// ---- schematic connectivity (to tie labels, power symbols and sheet pins to netlist nets) --------

interface Group {
	net?: string;
	pins: { ref: string; num: string; name: string; at: Pt }[];
	labels: Label[];
	/** Power symbols by the net they name (their Value). */
	power: { name: string; at: Pt }[];
	sheetPins: { block: string; name: string; at: Pt }[];
}

/** netlist pin → net name, keyed "REF\tPIN". */
function pinNets(nl: cli.Netlist): Map<string, string> {
	const m = new Map<string, string>();
	for (const n of nl.nets) for (const x of n.nodes) m.set(`${x.ref}\t${x.pin}`, n.name);
	return m;
}

/**
 * Connected items of one sheet instance (wires joined at their ends and wherever a point lands on
 * a wire), each group named by the netlist net of a part pin in it, else by its power symbol or
 * label.
 */
function sheetGroups(inst: SheetInst, nl: cli.Netlist, byPin: Map<string, string>): Group[] {
	const sch = inst.sch;
	const parent = new Map<string, string>();
	const find = (k: string): string => {
		const up = parent.get(k);
		if (up === undefined || up === k) return k;
		const r = find(up);
		parent.set(k, r);
		return r;
	};
	const union = (a: string, b: string) => {
		const [ra, rb] = [find(a), find(b)];
		if (ra !== rb) parent.set(ra, rb);
	};
	const wires = sch.wires();
	for (const w of wires) union(key(w.a), key(w.b));

	const groups = new Map<string, Group>();
	const items: { p: Pt; add: (g: Group) => void }[] = [];
	for (const sym of sch.symbols()) {
		const ref = sch.refOf(sym, inst.path);
		const power = ref.startsWith("#");
		for (const pin of sch.pins(sym)) {
			items.push({
				p: pin.at,
				add: g => {
					if (power) {
						const lib = sch.libSymbol(sch.libId(sym));
						if (lib && child(lib, "power")) g.power.push({ name: propValue(sym, "Value") ?? ref, at: pin.at });
					} else g.pins.push({ ref, num: pin.num, name: pin.name, at: pin.at });
				},
			});
		}
	}
	for (const l of sch.labels()) items.push({ p: l.at, add: g => g.labels.push(l) });
	for (const b of sch.sheets()) for (const sp of b.pins) items.push({ p: sp.at, add: g => g.sheetPins.push({ block: b.name, name: sp.name, at: sp.at }) });
	for (const p of [...items.map(i => i.p), ...wires.flatMap(w => [w.a, w.b])]) {
		for (const w of wires) if (onInterior(p, w)) union(key(p), key(w.a));
	}
	for (const it of items) {
		const root = find(key(it.p));
		const g = groups.get(root) ?? { pins: [], labels: [], power: [], sheetPins: [] };
		groups.set(root, g);
		it.add(g);
	}
	const names = new Set(nl.nets.map(n => n.name));
	const has = (n: string) => (names.has(n) ? n : undefined);
	for (const g of groups.values()) {
		g.net =
			g.pins.map(p => byPin.get(`${p.ref}\t${p.num}`)).find(n => n !== undefined) ??
			g.power.map(p => has(p.name)).find(n => n !== undefined) ??
			g.labels.map(l => has(l.kind === "global_label" ? l.name : `${inst.namePath}${l.name}`)).find(n => n !== undefined) ??
			g.sheetPins.map(sp => has(`${inst.namePath}${sp.block}/${sp.name}`)).find(n => n !== undefined);
	}
	return [...groups.values()];
}

// ---- hierarchy, symbols ------------------------------------------------------------------------

const hierarchy: QueryDef = {
	what: "hierarchy",
	doc: "Every sheet instance: name, file, name path, uuid path, page, paper, parent, block position/size on the parent and its sheet pins (name, shape, side, position).",
	fields: {},
	async run({ project }) {
		return {
			sheets: project.hierarchy().map(inst => ({
				name: inst.name,
				file: path.relative(project.dir, inst.file),
				namePath: inst.namePath,
				path: inst.path,
				page: pageOf(inst, project),
				paper: text(child(inst.sch.tree, "paper")),
				parent: inst.parent?.name,
				block: inst.block && { at: inst.block.at, size: inst.block.size },
				pins: inst.block?.pins.map(p => ({ name: p.name, shape: p.shape, side: SIDE[p.rot] ?? String(p.rot), at: p.at })),
			})),
		};
	},
};

const SYMBOL_COLUMNS = ["ref", "value", "lib_id", "footprint", "sheet", "at", "rot", "notes"];

type SymRow = [ref: string, value: string, libId: string, footprint: string, sheet: string, at: Pt, rot: number, notes: string, ...shown: string[]];

/** A placed symbol's field by name (case-insensitive; Reference as on the sheet instance), undefined when it has none. */
function fieldOf(sym: List, name: string, ref: string): string | undefined {
	const n = name.toLowerCase();
	if (n === "reference") return ref;
	const p = children(sym, "property").find(q => text(q)?.toLowerCase() === n);
	return p && (text(p, 2) ?? "");
}

/** Mirror, unit of a multi-unit part, and attributes that differ from a normal part. */
function symNotes(sch: Sch, sym: List, mirror: string | undefined): string {
	const lib = sch.libSymbol(sch.libId(sym));
	const multi = !!lib && children(lib, "symbol").some(u => /_([2-9]|\d{2,})_\d+$/.test(text(u) ?? ""));
	return [
		mirror ? `mirror ${mirror}` : "",
		multi ? `unit ${sch.unit(sym)}` : "",
		flag(sym, "dnp") ? "dnp" : "",
		flag(sym, "in_bom") === false ? "not_in_bom" : "",
		flag(sym, "on_board") === false ? "not_on_board" : "",
	]
		.filter(Boolean)
		.join(", ");
}

const symbols: QueryDef = {
	what: "symbols",
	doc:
		"Placed symbols, one row per unit sorted by reference, as columns ref, value, lib_id, footprint, sheet, at, rot, notes (mirror, unit of a multi-unit part, dnp, not_in_bom, not_on_board), then one column per `show` field. Power symbols and PWR_FLAGs (#PWR, #FLG) are left out unless `power` is true or `ref` starts with #. " +
		"Find parts by any field, e.g. every part with an LCSC number: fields {\"LCSC\": \"C25744\"}, show [\"MPN\", \"LCSC\"].",
	fields: {
		sheet: { t: "string", doc: "Only this sheet." },
		ref: { t: "string", doc: "Reference pattern, e.g. \"R*\" or \"U1?\" (* and ?, case-insensitive)." },
		value: { t: "string", doc: "Value pattern." },
		lib_id: { t: "string", doc: "Library id pattern, e.g. \"Device:C*\"." },
		fields: {
			t: "object",
			doc: "Field patterns {Name: pattern}, e.g. {\"LCSC\": \"C25744\", \"MPN\": \"RC0402*\"}: * and ?, case-insensitive (names too), all must match; a part without the field matches only \"\".",
		},
		show: { t: "strings", doc: "Fields to add as columns after notes, e.g. [\"MPN\", \"LCSC\"] (\"\" where a part has none)." },
		power: { t: "boolean", doc: "Include power symbols and PWR_FLAGs." },
		limit: { t: "integer", doc: "Most rows to return (default 100)." },
	},
	async run({ project }, args) {
		const [refOk, valOk, libOk] = [matcher(args.ref), matcher(args.value), matcher(args.lib_id)];
		const fieldOk = Object.entries((args.fields as Record<string, unknown> | undefined) ?? {}).map(([name, pattern]) => {
			if (typeof pattern !== "string") throw new Error(`fields.${name} must be a pattern string, got ${JSON.stringify(pattern)}`);
			const ok = matcher(pattern);
			return (sym: List, ref: string) => {
				const v = fieldOf(sym, name, ref);
				return v === undefined ? pattern === "" : ok(v);
			};
		});
		const show = (args.show as string[] | undefined) ?? [];
		const withPower = args.power === true || (typeof args.ref === "string" && args.ref.startsWith("#"));
		const rows: SymRow[] = [];
		for (const inst of typeof args.sheet === "string" ? [project.sheet(args.sheet)] : project.hierarchy()) {
			const sch = inst.sch;
			for (const sym of sch.symbols()) {
				const ref = sch.refOf(sym, inst.path);
				const value = propValue(sym, "Value") ?? "";
				const libId = sch.libId(sym);
				if ((ref.startsWith("#") && !withPower) || !refOk(ref) || !valOk(value) || !libOk(libId) || !fieldOk.every(ok => ok(sym, ref))) continue;
				const pose = sch.pose(sym);
				rows.push([
					ref,
					value,
					libId,
					propValue(sym, "Footprint") ?? "",
					inst.name,
					[pose.x, pose.y],
					pose.rot,
					symNotes(sch, sym, pose.mirror),
					...show.map(name => fieldOf(sym, name, ref) ?? ""),
				]);
			}
		}
		rows.sort((a, b) => refCmp(a[0], b[0]) || (a[7] < b[7] ? -1 : a[7] > b[7] ? 1 : 0));
		const limit = limitArg(args.limit, 100);
		return { total: rows.length, shown: Math.min(limit, rows.length), columns: [...SYMBOL_COLUMNS, ...show], rows: rows.slice(0, limit) };
	},
};

const symbol: QueryDef = {
	what: "symbol",
	doc: "One part in full: fields (value, visibility, position), attributes, units with their pose, pins (number, name, type, position, dir, net from the netlist, hidden) and the library description.",
	fields: { ref: { t: "string", doc: "Reference, e.g. \"U1\".", req: true } },
	async run({ project, analysis }, args) {
		const ref = String(args.ref);
		const units = project.findUnits(ref);
		if (!units.length) {
			const prefix = /^[^\d]*/.exec(ref)?.[0] ?? "";
			const near = [...project.allRefs()].filter(r => prefix && r.startsWith(prefix) && !r.startsWith("#")).sort(refCmp);
			throw new Error(`no symbol with reference ${ref}${near.length ? `; ${prefix} parts: ${near.slice(0, 30).join(", ")}` : ""}`);
		}
		const byPin = pinNets(await analysis.netlist());
		const first = units[0]!;
		const sch = first.sheet.sch;
		const lib = sch.lib(first.sym);
		const multi = units.length > 1;
		return {
			ref,
			lib_id: sch.libId(first.sym),
			description: propValue(lib, "Description") ?? "",
			sheet: [...new Set(units.map(u => u.sheet.name))].join(", "),
			fields: children(first.sym, "property").map(p => {
				const name = text(p) ?? "";
				const value = name === "Reference" ? ref : (text(p, 2) ?? "");
				return isHidden(p) || !value ? { name, value } : { name, value, visible: true, at: at(p).p };
			}),
			attributes: Object.fromEntries(
				["in_bom", "on_board", "dnp", "exclude_from_sim", "in_pos_files"].map(k => [k, flag(first.sym, k) ?? false]),
			),
			units: units.map(u => {
				const pose = u.sheet.sch.pose(u.sym);
				return { unit: u.sheet.sch.unit(u.sym), sheet: u.sheet.name, at: [pose.x, pose.y], rot: pose.rot, mirror: pose.mirror };
			}),
			pins: units
				.flatMap(u =>
					u.sheet.sch.pins(u.sym).map(p => ({
						num: p.num,
						name: p.name,
						type: p.type,
						at: p.at,
						dir: p.dir,
						net: byPin.get(`${ref}\t${p.num}`),
						hidden: p.hidden || undefined,
						unit: multi ? u.sheet.sch.unit(u.sym) : undefined,
					})),
				)
				.sort((a, b) => refCmp(a.num, b.num)),
		};
	},
};

// ---- nets, labels --------------------------------------------------------------------------------

/** Leaf of a hierarchical net name: "/MCU/VBUS_SENSE" → "VBUS_SENSE". */
function leaf(name: string): string {
	return name.slice(name.lastIndexOf("/") + 1);
}

const nets: QueryDef = {
	what: "nets",
	doc: "Netlist nets: name, net class, pin count and up to 12 pins (\"REF.PIN\").",
	fields: {
		sheet: { t: "string", doc: "Only nets with a pin on this sheet or named under it." },
		name: { t: "string", doc: "Net name pattern, matched against the full name and the leaf (\"VBUS*\" finds /MCU/VBUS_SENSE)." },
		class: { t: "string", doc: "Only nets of this net class." },
		limit: { t: "integer", doc: "Most nets to return (default 150)." },
	},
	async run({ project, analysis }, args) {
		const nl = await analysis.netlist();
		const nameOk = matcher(args.name);
		const inst = typeof args.sheet === "string" ? project.sheet(args.sheet) : undefined;
		const cls = typeof args.class === "string" ? args.class.toLowerCase() : undefined;
		const rows = nl.nets
			.filter(n => nameOk(n.name) || nameOk(leaf(n.name)))
			.filter(n => !cls || (n.netclass ?? "").toLowerCase() === cls)
			.filter(n => !inst || n.name.startsWith(inst.namePath) || n.nodes.some(x => nl.components.get(x.ref)?.sheetName === inst.namePath))
			.map(n => {
				const pins = [...n.nodes].sort((a, b) => refCmp(a.ref, b.ref) || refCmp(a.pin, b.pin)).map(x => `${x.ref}.${x.pin}`);
				return { name: n.name, class: n.netclass, count: pins.length, pins: pins.slice(0, 12).join(" ") };
			})
			.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
		return page(rows, limitArg(args.limit, 150), "nets");
	},
};

/** A net by full name, or by leaf name when that is unambiguous. */
function resolveNet(nl: cli.Netlist, name: string): cli.Net {
	const exact = nl.nets.find(n => n.name === name);
	if (exact) return exact;
	const want = name.replace(/^\//, "").toLowerCase();
	const hits = nl.nets.filter(n => leaf(n.name).toLowerCase() === want || n.name.toLowerCase().replace(/^\//, "") === want);
	if (hits.length === 1) return hits[0]!;
	if (hits.length > 1) throw new Error(`net "${name}" is ambiguous; candidates: ${hits.map(h => h.name).join(", ")}`);
	const close = nl.nets.filter(n => n.name.toLowerCase().includes(want)).map(n => n.name);
	throw new Error(`no net "${name}"${close.length ? `; similar: ${close.slice(0, 20).join(", ")}` : ""}`);
}

const net: QueryDef = {
	what: "net",
	doc: "One net: class, scope (local, hierarchical or global), its pins (ref, pin, pin name, type, sheet, position) and, per sheet, the labels, power symbols (count, first positions) and sheet pins carrying it. Accepts the full name or an unambiguous leaf name.",
	fields: {
		name: { t: "string", doc: "Net name, e.g. \"/MCU/VBUS_SENSE\", \"VBUS_SENSE\" or \"+3V3\".", req: true },
		limit: { t: "integer", doc: "Most pins to list (default 60); pinCount is always the total." },
	},
	async run({ project, analysis }, args) {
		const nl = await analysis.netlist();
		const n = resolveNet(nl, String(args.name));
		const byPin = pinNets(nl);
		const pinAt = new Map<string, { at: Pt; sheet: string; name: string }>();
		const sheets: Record<string, unknown>[] = [];
		let global = !n.name.startsWith("/");
		for (const inst of project.hierarchy()) {
			const here = sheetGroups(inst, nl, byPin).filter(g => g.net === n.name);
			for (const p of here.flatMap(g => g.pins)) pinAt.set(`${p.ref}\t${p.num}`, { at: p.at, sheet: inst.name, name: p.name });
			const labels = here.flatMap(g => g.labels);
			const power = new Map<string, Pt[]>();
			for (const p of here.flatMap(g => g.power)) power.set(p.name, [...(power.get(p.name) ?? []), p.at]);
			const sheetPins = here.flatMap(g => g.sheetPins);
			if (labels.some(l => l.kind === "global_label") || power.size) global = true;
			if (!labels.length && !power.size && !sheetPins.length) continue;
			sheets.push({
				sheet: inst.name,
				labels: labels.length ? labels.map(l => ({ kind: l.kind, name: l.name, at: l.at })) : undefined,
				power: power.size ? [...power].map(([name, pts]) => ({ name, count: pts.length, at: pts.slice(0, 5) })) : undefined,
				sheetPins: sheetPins.length ? sheetPins.map(p => ({ block: p.block, pin: p.name, at: p.at })) : undefined,
			});
		}
		const pins = n.nodes
			.map(x => {
				const where = pinAt.get(`${x.ref}\t${x.pin}`);
				return {
					ref: x.ref,
					pin: x.pin,
					name: where?.name || x.pinfunction,
					type: x.pintype,
					sheet: where?.sheet ?? nl.components.get(x.ref)?.sheetName,
					at: where?.at,
				};
			})
			.sort((a, b) => refCmp(a.ref, b.ref) || refCmp(a.pin, b.pin));
		const onSheets = new Set([...sheets.map(s => s.sheet), ...pins.map(p => p.sheet)]);
		return {
			name: n.name,
			class: n.netclass,
			scope: global ? "global" : onSheets.size > 1 ? "hierarchical" : "local",
			pinCount: pins.length,
			pins: pins.slice(0, limitArg(args.limit, 60)),
			sheets,
		};
	},
};

const labels: QueryDef = {
	what: "labels",
	doc: "Net labels (local, global, hierarchical) with sheet, position, rotation, shape and the netlist net they name.",
	fields: {
		sheet: { t: "string", doc: "Only this sheet." },
		name: { t: "string", doc: "Label text pattern." },
		kind: { t: "string", doc: "Only this kind.", values: ["label", "global_label", "hierarchical_label"] },
		limit: { t: "integer", doc: "Most labels to return (default 100)." },
	},
	async run({ project, analysis }, args) {
		const nl = await analysis.netlist();
		const byPin = pinNets(nl);
		const nameOk = matcher(args.name);
		const rows: { name: string; kind: string; sheet: string; at: Pt; rot: number; shape?: string; net?: string }[] = [];
		for (const inst of typeof args.sheet === "string" ? [project.sheet(args.sheet)] : project.hierarchy()) {
			const here: typeof rows = [];
			for (const g of sheetGroups(inst, nl, byPin)) {
				for (const l of g.labels) {
					if (!nameOk(l.name) || (args.kind && l.kind !== args.kind)) continue;
					here.push({ name: l.name, kind: l.kind, sheet: inst.name, at: l.at, rot: l.rot, shape: l.shape, net: g.net });
				}
			}
			rows.push(...here.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : a.at[1] - b.at[1] || a.at[0] - b.at[0])));
		}
		return page(rows, limitArg(args.limit, 100), "labels");
	},
};

// ---- wiring ------------------------------------------------------------------------------------

const WIRE_COLUMNS = ["a", "b", "at_a", "at_b", "on"];

type WireRow = [a: Pt, b: Pt, atA: string, atB: string, on: string];

/** "wire end, wire end, junction" → "wire end ×2, junction". */
function tally(items: string[]): string {
	const n = new Map<string, number>();
	for (const i of items) n.set(i, (n.get(i) ?? 0) + 1);
	return [...n].map(([i, c]) => (c > 1 ? `${i} ×${c}` : i)).join(", ");
}

/**
 * One row per segment: what sits at each end (connection points, other segments' ends or middles;
 * "dangling" when nothing) and, per point along its middle, what sits there. `noun` names the
 * segments: "wire" or "bus".
 */
function segRows(segs: Seg[], conn: Map<string, string[]>, pts: Pt[], noun: string): WireRow[] {
	const atEnd = (s: Seg, p: Pt) => {
		const items = [...(conn.get(key(p)) ?? [])];
		for (const o of segs) {
			if (o === s) continue;
			if (samePt(o.a, p) || samePt(o.b, p)) items.push(`${noun} end`);
			else if (onInterior(p, o)) items.push(`${noun} middle`);
		}
		return items.length ? tally(items) : "dangling";
	};
	return segs
		.map((s): WireRow => {
			const on = new Map<string, { p: Pt; items: string[] }>();
			const put = (p: Pt, items: string[]) => {
				const e = on.get(key(p)) ?? { p, items: [] };
				e.items.push(...items);
				on.set(key(p), e);
			};
			for (const p of pts) if (onInterior(p, s)) put(p, conn.get(key(p)) ?? []);
			for (const o of segs) for (const p of [o.a, o.b]) if (o !== s && onInterior(p, s)) put(p, [`${noun} end`]);
			const dist = (p: Pt) => Math.abs(p[0] - s.a[0]) + Math.abs(p[1] - s.a[1]);
			const mid = [...on.values()]
				.sort((x, y) => dist(x.p) - dist(y.p))
				.map(e => `(${e.p[0]}, ${e.p[1]}) ${tally(e.items)}`)
				.join("; ");
			return [s.a, s.b, atEnd(s, s.a), atEnd(s, s.b), mid];
		})
		.sort((x, y) => x[0][1] - y[0][1] || x[0][0] - y[0][0]);
}

const wires: QueryDef = {
	what: "wires",
	doc:
		"Wiring of one sheet. wires: rows [a, b, at_a, at_b, on] per segment — what sits at each end (pin U4.7, power GND, label / global label / hierarchical label NAME, sheet pin Block/NAME, junction, no-connect, bus entry, wire end, wire middle; \"dangling\" when nothing) and on its middle (\"(x, y) junction, wire end; …\" along the segment). " +
		"Also junctions and no_connects; buses (same columns, bus end / bus middle) and bus_entries [a, b] when the sheet has any. `box` keeps what touches the region.",
	fields: {
		sheet: { t: "string", doc: "Sheet name, file or \"root\".", req: true },
		box: { t: "any", doc: "Region [x0, y0, x1, y1] in sheet mm: only items touching it." },
		limit: { t: "integer", doc: "Most wire rows to return (default 200)." },
	},
	async run({ project }, args) {
		const inst = project.sheet(String(args.sheet));
		const sch = inst.sch;
		let box: Box | undefined;
		if (args.box !== undefined) {
			const b = args.box as number[];
			if (!Array.isArray(b) || b.length !== 4 || !b.every(v => typeof v === "number") || b[0]! > b[2]! || b[1]! > b[3]!) {
				throw new Error(`box must be [x0, y0, x1, y1] with x0 ≤ x1 and y0 ≤ y1, got ${JSON.stringify(b)}`);
			}
			box = [b[0]!, b[1]!, b[2]!, b[3]!];
		}
		const hit = (ps: Pt[]) => {
			if (!box) return true;
			const [x0, y0, x1, y1] = bbox(ps);
			return x0 <= box[2] && box[0] <= x1 && y0 <= box[3] && box[1] <= y1;
		};
		const conn = sch.connectionPoints(inst.path);
		const pts = [...conn.keys()].map(k => k.split(",").map(Number) as [number, number]);
		const keep = (rows: WireRow[]) => rows.filter(r => hit([r[0], r[1]]));
		const out: Record<string, unknown> = { sheet: inst.name, box, columns: WIRE_COLUMNS };
		Object.assign(out, page(keep(segRows(sch.wires(), conn, pts, "wire")), limitArg(args.limit, 200), "wires"));
		out.junctions = sch.points("junction").map(j => j.at).filter(p => hit([p]));
		out.no_connects = sch.points("no_connect").map(j => j.at).filter(p => hit([p]));
		const buses = sch.buses();
		const entries = sch.busEntries();
		if (buses.length) out.buses = keep(segRows(buses, conn, pts, "bus"));
		if (entries.length) out.bus_entries = entries.filter(e => hit([e.a, e.b])).map(e => [e.a, e.b]);
		return out;
	},
};

// ---- references ----------------------------------------------------------------------------------

const nextRefQuery: QueryDef = {
	what: "next_ref",
	doc:
		"References add_symbol would assign next for a prefix, without reserving them (same numbering code): the sheet's refBase block from .omp/kicad.json (a sheet without one gets the next free block), else one past the highest in the project. " +
		"`count` consecutive picks, as that many add_symbol calls in a row get them; a reused sheet gets one per placement, keyed by name path. Without sheet: every sheet when the prefix is numbered in blocks, else the project-wide picks.",
	fields: {
		prefix: { t: "string", req: true, doc: "Reference prefix, e.g. \"R\" or \"#PWR\"." },
		sheet: { t: "string", doc: "Sheet name, file or name path." },
		count: { t: "integer", doc: "How many consecutive references (default 1)." },
	},
	async run({ project }, args) {
		const prefix = String(args.prefix);
		if (!prefix || refPrefix(prefix) !== prefix) throw new Error(`prefix ${JSON.stringify(prefix)}: give the letters only, e.g. "R" or "#PWR"`);
		const count = limitArg(args.count, 1);
		const picks = (inst: SheetInst) => {
			const taken = new Set<string>();
			const rows = Array.from({ length: count }, () => placementRefs(project, prefix, inst, taken));
			return rows[0]!.length > 1 ? rows.map(r => Object.fromEntries(r.map(x => [x.inst.namePath, x.ref]))) : rows.map(r => r[0]!.ref);
		};
		if (typeof args.sheet === "string") {
			const inst = project.sheet(args.sheet);
			return { prefix, sheet: inst.name, refs: picks(inst) };
		}
		if (!project.settings.refBase[prefix]) return { prefix, numbering: "project-wide", refs: picks(project.hierarchy()[0]!) };
		// one entry per sheet file: a reused sheet's picks already cover each placement
		const sheets = new Map<string, SheetInst>();
		for (const s of project.hierarchy()) if (!sheets.has(s.file)) sheets.set(s.file, s);
		return { prefix, numbering: "per sheet (refBase blocks)", sheets: Object.fromEntries([...sheets.values()].map(s => [s.name, picks(s)])) };
	},
};

// ---- project settings ----------------------------------------------------------------------------

const CLASS_KEYS = ["priority", "clearance", "track_width", "via_diameter", "via_drill", "diff_pair_width", "diff_pair_gap"];

function record(v: unknown): Record<string, unknown> {
	return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : {};
}

function records(v: unknown): Record<string, unknown>[] {
	return Array.isArray(v) ? v.map(record) : [];
}

const netclasses: QueryDef = {
	what: "netclasses",
	doc: "Net classes from the .kicad_pro: settings (mm), name patterns, explicit assignments and the member nets in KiCad's netlist (comma-separated, first 40 per class).",
	fields: {},
	async run({ project, analysis }) {
		const nl = await analysis.netlist();
		const ns = record(project.proJson.data.net_settings);
		const patterns = records(ns.netclass_patterns);
		const assigned = Object.entries(record(ns.netclass_assignments));
		return {
			classes: records(ns.classes).map(c => {
				const name = String(c.name);
				const members = nl.nets.filter(n => n.netclass === name).map(n => n.name).sort();
				return {
					name,
					settings: Object.fromEntries(CLASS_KEYS.filter(k => k in c).map(k => [k, c[k]])),
					patterns: patterns.filter(p => p.netclass === name).map(p => String(p.pattern)),
					assigned: assigned.filter(([, v]) => v === name || (Array.isArray(v) && v.includes(name))).map(([k]) => k),
					count: members.length,
					nets: members.slice(0, 40).join(", ") + (members.length > 40 ? `, … (${members.length - 40} more)` : ""),
				};
			}),
		};
	},
};

/** Words of a list, flattened: (constraint clearance (min 2mm)) → "clearance min 2mm". */
function words(l: List): string {
	return l
		.slice(1)
		.map(x => (isList(x) ? `${head(x) ?? ""} ${words(x)}`.trim() : text([x], 0)))
		.join(" ");
}

const rules: QueryDef = {
	what: "rules",
	doc: "Custom design rules from the .kicad_dru: the file's header comment (the # lines after (version N)), then each rule's name, comment (the # lines directly above it), layer, condition, severity and constraints (one line each).",
	fields: {},
	async run({ project }) {
		if (!fs.existsSync(project.druFile)) return { file: null, rules: [] };
		const src = fs.readFileSync(project.druFile, "utf8");
		return {
			file: project.rel(project.druFile),
			header: druHeader(src),
			rules: druRules(src).map(({ node: r, comment }) => ({
				name: text(r) ?? "",
				comment,
				layer: text(child(r, "layer")),
				condition: text(child(r, "condition")),
				severity: text(child(r, "severity")),
				constraints: children(r, "constraint").map(words),
			})),
		};
	},
};

/** What a uuid names in the schematic: "U3" (symbol), "U3 pin 5", "label VBUS", else the item's kind; keyed by sheet uuid path, then uuid. */
function schItems(project: Project): Map<string, Map<string, string>> {
	const out = new Map<string, Map<string, string>>();
	for (const inst of project.hierarchy()) {
		const m = new Map<string, string>();
		for (const item of inst.sch.tree) {
			if (!isList(item)) continue;
			const uuid = text(child(item, "uuid"));
			if (head(item) === "symbol") {
				const ref = inst.sch.refOf(item, inst.path);
				if (uuid) m.set(uuid, ref);
				for (const pin of children(item, "pin")) {
					const u = text(child(pin, "uuid"));
					if (u) m.set(u, `${ref} pin ${text(pin) ?? "?"}`);
				}
			} else if (uuid) m.set(uuid, /label$/.test(head(item) ?? "") ? `${head(item)} ${text(item) ?? ""}` : (head(item) ?? "item"));
		}
		out.set(inst.path, m);
	}
	return out;
}

/** What a uuid names on the board: "R1" (footprint), "R1 pad 2", else the item's kind ("segment", "via", "zone"). */
function boardItems(tree: List): Map<string, string> {
	const m = new Map<string, string>();
	for (const item of tree) {
		if (!isList(item)) continue;
		const uuid = text(child(item, "uuid"));
		if (head(item) === "footprint") {
			const ref = fpRef(item);
			if (uuid) m.set(uuid, ref);
			for (const pad of children(item, "pad")) {
				const u = text(child(pad, "uuid"));
				if (u) m.set(u, `${ref} pad ${text(pad) ?? "?"}`);
			}
		} else if (uuid) m.set(uuid, head(item) ?? "item");
	}
	return m;
}

const exclusionsQuery: QueryDef = {
	what: "exclusions",
	doc:
		"ERC and DRC exclusions from the .kicad_pro, each with its kind, rule, marker position (sheet mm and sheet for ERC, board mm and layer when the key has one for DRC), comment, the items its key names (symbol, pin, footprint, pad; \"gone\" when no longer in the design) and its whole key (remove_exclusion and set_exclusion_comment take it or match its text). " +
		'KiCad\'s ERC runs here twice, as is and on a temp copy without the ERC exclusions (the ERC report also holds an excluded violation re-created from every exclusion whose items still exist, found or not), and DRC when there are DRC exclusions: `hides` is the violation KiCad still detects that the exclusion hides; `stale: true` when it hides none any more (the design changed under it; remove_exclusion cleans it up). DRC exclusions on a project without a board, or an empty one, aren\'t checked ("checked": false).',
	fields: { kind: { t: "string", values: EXCLUSION_KINDS, doc: "only this kind (default both)" } },
	async run({ project, analysis }, args) {
		const data = project.proJson.data;
		const kinds = EXCLUSION_KINDS.filter(k => args.kind === undefined || args.kind === k);
		const rows: Record<string, unknown>[] = [];
		for (const kind of kinds) {
			const list = exclusions(data, kind);
			if (!list.length) continue;
			// KiCad's report and the violations it detects (ERC and DRC violations share these fields)
			let report: cli.DrcViolation[] | undefined;
			let detected: cli.DrcViolation[] | undefined;
			let name: (e: Exclusion, uuid: string) => string;
			if (kind === "erc") {
				[report, detected] = await Promise.all([analysis.erc(), analysis.ercDetected()]);
				const bySheet = schItems(project);
				// pins of a sheet used twice share uuids: the key's sheets first, then anywhere
				name = (e, u) => [...(e.sheetPaths ?? []).map(p => bySheet.get(p)), ...bySheet.values()].map(m => m?.get(u)).find(x => x) ?? `${u} (gone)`;
			} else {
				const tree = fs.existsSync(project.boardFile) ? project.board().tree : undefined;
				if (tree && !boardIsEmpty(tree)) {
					const zones = [tree, ...children(tree, "footprint")].some(l => children(l, "zone").length > 0);
					const r = await cli.pcbDrc(project.boardFile, analysis.runOpts, true, zones);
					report = detected = [...r.violations, ...r.parity, ...r.unconnected];
				}
				const items = tree ? boardItems(tree) : undefined;
				name = (_, u) => items?.get(u) ?? `${u}${items ? " (gone)" : ""}`;
			}
			const sheets = new Map(project.hierarchy().map(i => [i.path, i.namePath]));
			for (const e of list) {
				const sheet = e.sheetPaths?.[0];
				const hit = report && hiddenBy(e, report, detected!);
				rows.push({
					kind,
					rule: e.rule,
					at: e.at,
					...(sheet ? { sheet: sheets.get(sheet) ?? `${sheet} (gone)` } : {}),
					...(e.layer ? { layer: e.layer } : {}),
					comment: e.comment,
					items: [...e.items.map(u => name(e, u)), ...(e.text !== undefined ? [`text "${e.text}"`] : [])],
					...(report ? (hit ? { hides: `${hit.description} [${hit.items.map(i => i.description).join("; ")}]` } : { stale: true }) : { checked: false }),
					key: e.key,
				});
			}
		}
		return { file: project.rel(project.proFile), exclusions: rows };
	},
};

/** RFC 4180 CSV as kicad-cli writes it. */
export function parseCsv(src: string): string[][] {
	const rows: string[][] = [];
	let row: string[] = [];
	let cell = "";
	let quoted = false;
	for (let i = 0; i < src.length; i++) {
		const c = src[i]!;
		if (quoted) {
			if (c === '"' && src[i + 1] === '"') {
				cell += '"';
				i++;
			} else if (c === '"') quoted = false;
			else cell += c;
		} else if (c === '"') quoted = true;
		else if (c === ",") {
			row.push(cell);
			cell = "";
		} else if (c === "\n" || c === "\r") {
			if (c === "\r" && src[i + 1] === "\n") i++;
			row.push(cell);
			if (row.some(x => x !== "")) rows.push(row);
			row = [];
			cell = "";
		} else cell += c;
	}
	row.push(cell);
	if (row.some(x => x !== "")) rows.push(row);
	return rows;
}

const bom: QueryDef = {
	what: "bom",
	doc: "Bill of materials from `kicad-cli sch export bom` with a BOM preset (default: the project's first preset, else KiCad's default): header and rows. To find the parts with a given field value (LCSC, MPN), use `symbols` with `fields` and `show` instead.",
	fields: { preset: { t: "string", doc: "BOM preset name from the project's schematic settings." } },
	async run({ project, analysis }, args) {
		const presets = records(record(project.proJson.data.schematic).bom_presets).map(p => String(p.name));
		const preset = typeof args.preset === "string" ? args.preset : presets[0];
		if (typeof args.preset === "string" && !presets.includes(args.preset)) {
			throw new Error(`no BOM preset "${args.preset}"; presets: ${presets.join(", ") || "none"}`);
		}
		const [header = [], ...rows] = parseCsv(await cli.bom(project.rootFile, preset, analysis.runOpts));
		return { preset: preset ?? "(KiCad default)", header, rows };
	},
};

// ---- sheet layout --------------------------------------------------------------------------------

/** KiCad's default drawing sheet: border 10 mm, inner frame 2 mm further in, title block 108 × 32 mm. */
function frameBoxes(paper: Pt): { inner: Box; titleBlock: Box } {
	const [w, h] = paper;
	return { inner: [12, 12, w - 12, h - 12], titleBlock: [w - 120, h - 44, w - 12, h - 12] };
}

/** Points that mark used area, for the sheet's bounding box. */
function usedPoints(inst: SheetInst): Pt[] {
	const sch = inst.sch;
	const pts: Pt[] = [];
	for (const s of sch.symbols()) {
		const b = sch.bodyBox(s);
		pts.push([b[0], b[1]], [b[2], b[3]]);
	}
	for (const w of [...sch.wires(), ...sch.buses()]) pts.push(w.a, w.b);
	for (const l of sch.labels()) pts.push(l.at);
	for (const k of ["text", "text_box", "junction", "no_connect"]) for (const n of sch.items(k)) if (child(n, "at")) pts.push(at(n).p);
	for (const b of sch.sheets()) pts.push(b.at, [b.at[0] + b.size[0], b.at[1] + b.size[1]]);
	return pts;
}

const sheet: QueryDef = {
	what: "sheet",
	doc: "One sheet: file, paper and size, title block, counts of symbols/wires/labels/junctions/no-connects/texts, sheet blocks and the bounding box of the used area.",
	fields: { sheet: { t: "string", doc: "Sheet name, file or \"root\".", req: true } },
	async run({ project }, args) {
		const inst = project.sheet(String(args.sheet));
		const sch = inst.sch;
		const tb = child(sch.tree, "title_block");
		const syms = sch.symbols();
		const power = syms.filter(s => sch.refOf(s, inst.path).startsWith("#")).length;
		const labels = sch.labels();
		const pts = usedPoints(inst);
		return {
			name: inst.name,
			file: path.relative(project.dir, inst.file),
			paper: { name: text(child(sch.tree, "paper")), size: sch.paper() },
			titleBlock: tb
				? Object.fromEntries(
						tb
							.filter(isList)
							.map(x => [head(x) === "comment" ? `comment${text(x)}` : (head(x) ?? ""), head(x) === "comment" ? text(x, 2) : text(x)]),
					)
				: undefined,
			counts: {
				parts: syms.length - power,
				power,
				wires: sch.wires().length,
				buses: sch.buses().length,
				labels: labels.filter(l => l.kind === "label").length,
				globalLabels: labels.filter(l => l.kind === "global_label").length,
				hierarchicalLabels: labels.filter(l => l.kind === "hierarchical_label").length,
				junctions: sch.points("junction").length,
				noConnects: sch.points("no_connect").length,
				texts: sch.items("text").length + sch.items("text_box").length,
			},
			blocks: sch.sheets().map(b => ({ name: b.name, file: b.file, at: b.at, size: b.size, pins: b.pins.length })),
			used: pts.length ? bbox(pts) : undefined,
		};
	},
};

// ---- free space ----------------------------------------------------------------------------------

type Widths = Map<string, number>;

/** Rendered size of a text: width from KiCad's SVG when known, else estimated; height for its font. */
function textSize(s: string, node: List, widths: Widths): [number, number] {
	const font = child(child(node, "effects") ?? [], "font");
	const size = font && child(font, "size") ? num(child(font, "size"), 1) : 1.27;
	const scale = size / 1.27;
	return [widths.get(s) ?? s.length * CHAR_W * scale, TEXT_H * scale];
}

function justify(node: List): { h: string; v: string } {
	const j = child(child(node, "effects") ?? [], "justify") ?? [];
	const has = (k: string) => j.some(x => text([x], 0) === k);
	return { h: has("left") ? "left" : has("right") ? "right" : "center", v: has("top") ? "top" : has("bottom") ? "bottom" : "center" };
}

function span(anchor: number, len: number, start: boolean, end: boolean): [number, number] {
	return start ? [anchor, anchor + len] : end ? [anchor - len, anchor] : [anchor - len / 2, anchor + len / 2];
}

/**
 * A symbol field's text box on the sheet. KiCad lays the text out in the symbol's own frame
 * (field angle and justification), then applies the symbol's rotation and mirror.
 */
function fieldBox(sch: Sch, sym: List, field: List, s: string, widths: Widths): Box {
	const { x, y, rot, mirror } = sch.pose(sym);
	// symbol frame → sheet: columns of the orthogonal transform (sheet y down)
	const T = (vx: number, vy: number): Pt => xform(vx, -vy, 0, 0, rot, mirror);
	const [ex, ey] = [T(1, 0), T(0, 1)];
	const { p, rot: frot } = at(field);
	const [rx, ry] = [p[0] - x, p[1] - y];
	const [ax, ay] = [rx * ex[0] + ry * ex[1], rx * ey[0] + ry * ey[1]];
	const [w, h] = textSize(s, field, widths);
	const j = justify(field);
	const a = ((Math.round(frot / 90) % 4) + 4) % 4;
	// reading direction and glyph "down" in the symbol frame
	let box: [number, number, number, number];
	if (a % 2 === 0) {
		const [x0, x1] = span(ax, w, a === 0 ? j.h === "left" : j.h === "right", a === 0 ? j.h === "right" : j.h === "left");
		const [y0, y1] = span(ay, h, a === 0 ? j.v === "top" : j.v === "bottom", a === 0 ? j.v === "bottom" : j.v === "top");
		box = [x0, y0, x1, y1];
	} else {
		const [y0, y1] = span(ay, w, a === 1 ? j.h === "right" : j.h === "left", a === 1 ? j.h === "left" : j.h === "right");
		const [x0, x1] = span(ax, h, a === 1 ? j.v === "top" : j.v === "bottom", a === 1 ? j.v === "bottom" : j.v === "top");
		box = [x0, y0, x1, y1];
	}
	const corners = [T(box[0], box[1]), T(box[2], box[3])];
	return bbox(corners.map(c => [x + c[0], y + c[1]]));
}

/** Label text box: local labels sit just above (or left of) the wire; global/hierarchical ones add their outline. */
function labelBox(l: Label, widths: Widths): Box {
	const [w, h] = textSize(l.name, l.node, widths);
	const [x, y] = l.at;
	const r = ((Math.round(l.rot / 90) % 4) + 4) % 4;
	if (l.kind === "label") {
		const off = 0.3;
		if (r === 0) return [x, y - h - off, x + w + off, y];
		if (r === 2) return [x - w - off, y - h - off, x, y];
		if (r === 1) return [x - h - off, y - w - off, x, y];
		return [x - h - off, y, x, y + w + off];
	}
	const len = w + 2 * h;
	const half = h;
	if (r === 0) return [x, y - half, x + len, y + half];
	if (r === 2) return [x - len, y - half, x, y + half];
	if (r === 1) return [x - half, y - len, x + half, y];
	return [x - half, y, x + half, y + len];
}

/** Everything drawn on a sheet that a new part must not cover. */
function obstacles(inst: SheetInst, widths: Widths): Box[] {
	const sch = inst.sch;
	const out: Box[] = [];
	for (const sym of sch.symbols()) {
		out.push(sch.bodyBox(sym));
		for (const f of children(sym, "property")) {
			const name = text(f) ?? "";
			const s = name === "Reference" ? sch.refOf(sym, inst.path) : (text(f, 2) ?? "");
			if (!s || isHidden(f)) continue;
			out.push(fieldBox(sch, sym, f, s, widths));
		}
	}
	for (const w of [...sch.wires(), ...sch.buses(), ...sch.busEntries()]) out.push(bbox([w.a, w.b]));
	for (const l of sch.labels()) out.push(labelBox(l, widths));
	for (const j of [...sch.points("junction"), ...sch.points("no_connect")]) out.push([j.at[0] - 0.6, j.at[1] - 0.6, j.at[0] + 0.6, j.at[1] + 0.6]);
	for (const t of sch.items("text")) {
		const s = text(t) ?? "";
		const [w, h] = textSize(s, t, widths);
		const { p, rot } = at(t);
		out.push(rot % 180 ? [p[0] - h, p[1] - w, p[0] + h, p[1] + w] : [p[0] - w, p[1] - h, p[0] + w, p[1] + h]);
	}
	for (const tb of sch.items("text_box")) {
		const { p } = at(tb);
		const s = child(tb, "size");
		out.push([p[0], p[1], p[0] + num(s, 1), p[1] + num(s, 2)]);
	}
	for (const b of sch.sheets()) {
		out.push([b.at[0], b.at[1] - TEXT_H * 1.5, b.at[0] + b.size[0], b.at[1] + b.size[1] + TEXT_H * 1.5]);
		for (const sp of b.pins) {
			const [w] = textSize(sp.name, sp.node, widths);
			out.push(sp.rot === 180 ? [sp.at[0] - 2.5, sp.at[1] - 1, sp.at[0] + w + 1, sp.at[1] + 1] : [sp.at[0] - w - 1, sp.at[1] - 1, sp.at[0] + 2.5, sp.at[1] + 1]);
		}
	}
	return out;
}

const freeSpace: QueryDef = {
	what: "free_space",
	doc: "Empty grid-aligned boxes of a given size nearest to a point, clear of symbol bodies, pins and visible fields, wires, labels (rendered text width), junctions, sheet blocks, the frame border and the title block. Results don't overlap each other.",
	fields: {
		sheet: { t: "string", doc: "Sheet name, file or \"root\".", req: true },
		near: { t: "point", doc: "Where the space is wanted (sheet mm).", req: true },
		size: { t: "point", doc: "[width, height] of the space in mm, rounded up to whole grid steps.", req: true },
		clearance: { t: "number", doc: "Minimum gap to anything drawn (mm, default one grid step)." },
		limit: { t: "integer", doc: "How many boxes (default 3)." },
	},
	async run({ project, analysis }, args) {
		const inst = project.sheet(String(args.sheet));
		const [nx, ny] = args.near as Pt;
		const size = args.size as Pt;
		if (!(size[0] > 0 && size[1] > 0)) throw new Error(`size must be positive, got ${JSON.stringify(size)}`);
		const g = project.settings.grid;
		const w = Math.ceil(size[0] / g - 1e-6) * g;
		const h = Math.ceil(size[1] / g - 1e-6) * g;
		const clear = typeof args.clearance === "number" ? args.clearance : g;
		const limit = limitArg(args.limit, 3);
		const { inner, titleBlock } = frameBoxes(inst.sch.paper());
		const blocked = [...obstacles(inst, await analysis.textWidths()), titleBlock];
		const cands: { x: number; y: number; d: number }[] = [];
		for (let i = Math.ceil(inner[0] / g); (i * g + w) <= inner[2]; i++) {
			for (let k = Math.ceil(inner[1] / g); (k * g + h) <= inner[3]; k++) {
				const x = i * g;
				const y = k * g;
				cands.push({ x, y, d: Math.hypot(x + w / 2 - nx, y + h / 2 - ny) });
			}
		}
		cands.sort((a, b) => a.d - b.d);
		const found: Box[] = [];
		for (const c of cands) {
			const box: Box = [r3(c.x), r3(c.y), r3(c.x + w), r3(c.y + h)];
			if (found.some(f => overlap(f, box)) || blocked.some(b => overlap(box, b, clear))) continue;
			found.push(box);
			if (found.length >= limit) break;
		}
		if (!found.length) throw new Error(`no free ${w} × ${h} mm area on sheet ${inst.name}`);
		return {
			sheet: inst.name,
			boxes: found.map(b => ({ box: b, center: pt(Math.round((b[0] + b[2]) / 2 / g) * g, Math.round((b[1] + b[3]) / 2 / g) * g) })),
		};
	},
};

export const queries: QueryDef[] = [hierarchy, symbols, symbol, nets, net, labels, wires, nextRefQuery, netclasses, rules, exclusionsQuery, bom, sheet, freeSpace];
