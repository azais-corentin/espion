/**
 * lib_diff: what differs between two library symbols or two library footprints (stock or project),
 * so a part derived from another can be checked against its source. Drawings are compared in the
 * lib_symbol / lib_footprint `graphics: true` formats.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { describeFootprint, describeLibSymbol, type FpPadInfo, type Libraries, splitId } from "../libs";
import { head, parse } from "../sexpr";
import { type Pin, readPin, symbolGraphics, symbolItems } from "../symgfx";
import type { QueryDef } from "../types";

/** Keys whose values differ (compared as JSON): {key: [a, b]}, a missing value as null. */
function changed(a: Record<string, unknown>, b: Record<string, unknown>, keys = [...new Set([...Object.keys(a), ...Object.keys(b)])]): Record<string, [unknown, unknown]> {
	const out: Record<string, [unknown, unknown]> = {};
	for (const k of keys) if (JSON.stringify(a[k]) !== JSON.stringify(b[k])) out[k] = [a[k] ?? null, b[k] ?? null];
	return out;
}

/** Multiset difference of two lists of plain values (equal = same JSON). */
function multisetDiff<T>(a: T[], b: T[]): { added: T[]; removed: T[] } {
	const left = new Map<string, number>();
	for (const x of a) left.set(JSON.stringify(x), (left.get(JSON.stringify(x)) ?? 0) + 1);
	const added = b.filter(y => {
		const k = JSON.stringify(y);
		const n = left.get(k) ?? 0;
		left.set(k, n - 1);
		return n <= 0;
	});
	const removed = a.filter(x => {
		const k = JSON.stringify(x);
		const n = left.get(k) ?? 0;
		left.set(k, n - 1);
		return n > 0;
	});
	return { added, removed };
}

/**
 * Pair the items of `a` and `b` with the same key, cheapest pairs first (several items may share a
 * key, e.g. pad numbers); unpaired items of `a` were removed, of `b` added.
 */
function match<T>(a: T[], b: T[], key: (t: T) => string, cost: (x: T, y: T) => number): { pairs: [T, T][]; removed: T[]; added: T[] } {
	const byKey = new Map<string, number[]>();
	b.forEach((y, j) => byKey.set(key(y), [...(byKey.get(key(y)) ?? []), j]));
	const cands = a.flatMap((x, i) => (byKey.get(key(x)) ?? []).map(j => ({ i, j, c: cost(x, b[j]!) }))).sort((p, q) => p.c - q.c || p.i - q.i || p.j - q.j);
	const toB = new Map<number, number>();
	const usedB = new Set<number>();
	for (const { i, j } of cands) {
		if (toB.has(i) || usedB.has(j)) continue;
		toB.set(i, j);
		usedB.add(j);
	}
	return {
		pairs: a.flatMap((x, i) => (toB.has(i) ? [[x, b[toB.get(i)!]!] as [T, T]] : [])),
		removed: a.filter((_, i) => !toB.has(i)),
		added: b.filter((_, j) => !usedB.has(j)),
	};
}

function dist(p: readonly number[] | undefined, q: readonly number[] | undefined): number {
	return Math.hypot((p?.[0] ?? 0) - (q?.[0] ?? 0), (p?.[1] ?? 0) - (q?.[1] ?? 0));
}

/** Changed entries of matched pairs: {number, near? (a's position when the number repeats in a), field: [a, b], …}. */
function changedPairs<T extends { number: string; at?: readonly number[] }>(pairs: [T, T][], all: T[], fields: readonly string[]): Record<string, unknown>[] {
	return pairs.flatMap(([x, y]) => {
		const diff = changed({ ...x }, { ...y }, [...fields]);
		if (!Object.keys(diff).length) return [];
		const repeated = all.filter(p => p.number === x.number).length > 1;
		return [{ number: x.number, ...(repeated ? { near: x.at } : {}), ...diff }];
	});
}

/** Drop empty sections so an unchanged aspect doesn't show. */
function compact(o: Record<string, unknown>): Record<string, unknown> {
	return Object.fromEntries(
		Object.entries(o).filter(([, v]) => {
			if (Array.isArray(v)) return v.length > 0;
			if (v && typeof v === "object") return Object.values(v).some(x => (Array.isArray(x) ? x.length > 0 : x !== undefined));
			return true;
		}),
	);
}

const PIN_FIELDS = ["name", "type", "at", "dir", "length", "unit", "body_style", "hidden", "shape"] as const;

function diffSymbols(libs: Libraries, a: string, b: string): Record<string, unknown> {
	const [sa, sb] = [libs.flatSymbol(a), libs.flatSymbol(b)];
	const [da, db] = [describeLibSymbol(sa), describeLibSymbol(sb)];
	const [ga, gb] = [symbolGraphics(sa), symbolGraphics(sb)];
	const pins = (s: typeof sa): Pin[] =>
		symbolItems(s)
			.items.filter(it => head(it.node) === "pin")
			.map(it => readPin(it.node, it.unit, it.body));
	const [pa, pb] = [pins(sa), pins(sb)];
	// a pin that changed unit or body style still pairs with its old self, but after any exact match
	const m = match(pa, pb, p => p.number, (x, y) => dist(x.at, y.at) + (x.unit !== y.unit ? 1e6 : 0) + (x.body_style !== y.body_style ? 1e6 : 0));
	const settings = (d: typeof da, g: typeof ga) => ({
		units: d.units,
		power: d.power,
		pin_name_offset: g.pin_name_offset,
		hide_pin_names: g.hide_pin_names,
		hide_pin_numbers: g.hide_pin_numbers,
	});
	return compact({
		properties: changed(da.properties, db.properties),
		...changed(settings(da, ga), settings(db, gb)),
		pins: compact({ added: m.added, removed: m.removed, changed: changedPairs(m.pairs, pa, PIN_FIELDS) }),
		shapes: compact(multisetDiff(ga.shapes, gb.shapes)),
	});
}

const PAD_FIELDS = ["type", "shape", "at", "size", "drill", "layers"] as const;

function diffFootprints(libs: Libraries, a: string, b: string): Record<string, unknown> {
	const read = (id: string) => describeFootprint(parse(fs.readFileSync(libs.footprintFile(id), "utf8")), true);
	const fa = read(a);
	const fb = read(b);
	const m = match<FpPadInfo>(fa.pads, fb.pads, p => p.number, (x, y) => dist(x.at, y.at));
	const graphics: Record<string, unknown> = {};
	for (const layer of new Set([...Object.keys(fa.graphics ?? {}), ...Object.keys(fb.graphics ?? {})])) {
		const d = multisetDiff(fa.graphics?.[layer] ?? [], fb.graphics?.[layer] ?? []);
		if (d.added.length || d.removed.length) graphics[layer] = compact(d);
	}
	return compact({
		properties: changed(fa.properties, fb.properties),
		...changed({ ...fa }, { ...fb }, ["description", "tags", "attributes", "models"]),
		pads: compact({ added: m.added, removed: m.removed, changed: changedPairs(m.pairs, fa.pads, PAD_FIELDS) }),
		graphics,
	});
}

/** Whether "Lib:Name" is a symbol or a footprint of the project or global libraries. */
function kindOf(libs: Libraries, id: string): "symbol" | "footprint" {
	const [nick, name] = splitId(id);
	const isSym = libs.sym.some(e => e.name === nick) && libs.symbolNames(nick).includes(name);
	const isFp = libs.fp.some(e => e.name === nick) && fs.existsSync(path.join(libs.fpLib(nick).uri, `${name}.kicad_mod`));
	if (isSym && isFp) throw new Error(`"${id}" names both a symbol and a footprint; lib_diff can't tell which to compare`);
	if (!isSym && !isFp) throw new Error(`"${id}" is neither a symbol nor a footprint of the project or global libraries`);
	return isSym ? "symbol" : "footprint";
}

const libDiff: QueryDef = {
	what: "lib_diff",
	doc: 'What differs from library part `a` to `b` (two symbols or two footprints, stock or project; e.g. a derived part against its source). Every change is [a, b]; unchanged sections are left out, `same` says whether anything differs. Symbols: properties, units/power/pin text settings, pins added/removed/changed (matched by number, then unit, body style and position; fields name, type, at, dir, length, unit, body_style, hidden, shape), shapes added/removed (lib_symbol graphics format). Footprints: properties, description, tags, attributes, models, pads added/removed/changed (matched by number, nearest position among repeats, which also get `near`; fields type, shape, at, size, drill, layers; a renumbered pad shows as removed + added), graphics added/removed per layer (lib_footprint graphics format).',
	fields: {
		a: { t: "string", req: true, doc: '"Library:Name", e.g. the source "Connector_RJ:RJ45_Abracon_ARJP11A-MA_Horizontal"' },
		b: { t: "string", req: true, doc: '"Library:Name" of the same kind, e.g. the derived "Connector_RJ_HanRun:RJ45_HanRun_HR871150C_Horizontal"' },
	},
	async run(ctx, args) {
		const libs = await ctx.libs();
		const a = args.a as string;
		const b = args.b as string;
		const kind = kindOf(libs, a);
		if (kindOf(libs, b) !== kind) throw new Error(`"${a}" is a ${kind} but "${b}" is not; lib_diff compares two symbols or two footprints`);
		const diff = kind === "symbol" ? diffSymbols(libs, a, b) : diffFootprints(libs, a, b);
		return { kind, a, b, same: !Object.keys(diff).length, ...diff };
	},
};

export const queries: QueryDef[] = [libDiff];
