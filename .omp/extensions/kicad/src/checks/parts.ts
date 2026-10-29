/**
 * Part data checks (`footprints`, `fields`, `bom_lines`): every board part has a footprint that
 * resolves and whose pads match the symbol's pins, every BOM part carries the sourcing fields the
 * project requires, and BOM lines agree (one part number per value and footprint, one value per
 * part number). All read KiCad's netlist, so DNP / BOM / board flags are KiCad's own.
 */
import type { Component } from "../cli";
import type { Project } from "../project";
import { libGeom } from "../sch";
import type { List } from "../sexpr";
import type { CheckContext, CheckDef, Finding } from "../types";

/** Sheet name of a netlist sheet path ("/Power/" → "Power", "/" → "root"). */
export function sheetOfPath(namePath: string): string {
	return namePath.split("/").filter(Boolean).at(-1) ?? "root";
}

/**
 * Pad numbers of one symbol pin: KiCad 10 stacks several pads on one pin as "[1,2,5-7]"
 * (ranges keep a common prefix: "[A1-A3]").
 */
function padNumbers(pin: string): string[] {
	const m = /^\[(.*)\]$/.exec(pin);
	if (!m) return [pin];
	return m[1]!.split(",").flatMap(part => {
		const r = /^(\D*)(\d+)-\1?(\d+)$/.exec(part.trim());
		if (!r) return [part.trim()];
		const out: string[] = [];
		for (let i = Number(r[2]); i <= Number(r[3]); i++) out.push(`${r[1]}${i}`);
		return out;
	});
}

/** Netlist components on the checked sheets. */
async function components(ctx: CheckContext): Promise<Component[]> {
	const all = [...(await ctx.analysis.netlist()).components.values()];
	if (!ctx.sheets) return all;
	const paths = new Set(ctx.sheets.map(s => s.namePath));
	return all.filter(c => paths.has(c.sheetName));
}

/** The lib_symbols entry of every reference (first unit found; all units share it). */
function libSymbols(project: Project): Map<string, List> {
	const out = new Map<string, List>();
	for (const sheet of project.hierarchy()) {
		for (const sym of sheet.sch.symbols()) {
			const ref = sheet.sch.refOf(sym, sheet.path);
			const lib = sheet.sch.libSymbol(sheet.sch.libId(sym));
			if (lib && !out.has(ref)) out.set(ref, lib);
		}
	}
	return out;
}

const footprints: CheckDef = {
	name: "footprints",
	doc: 'Every board part has a footprint that resolves through the lib tables (error); symbol pins (all units) vs footprint pads: pin without pad (error), numbered pad without pin (warning). settings.padMatchExempt skips "REF" or "REF.pin".',
	byDefault: true,
	async run(ctx) {
		const libs = await ctx.project.libs();
		const exempt = new Set(ctx.project.settings.padMatchExempt);
		const symbols = libSymbols(ctx.project);
		const out: Finding[] = [];
		for (const c of await components(ctx)) {
			const ref = c.ref;
			const base = { check: "footprints", refs: [ref], sheet: sheetOfPath(c.sheetName) };
			if (!c.footprint) {
				if (!c.excludeFromBoard) {
					out.push({ ...base, severity: "error", message: `${ref} (${c.value}) has no footprint`, key: `footprints|missing|${ref}` });
				}
				continue;
			}
			let pads: string[];
			try {
				pads = libs.footprintPads(c.footprint).map(p => p.num);
			} catch (e) {
				out.push({
					...base,
					severity: "error",
					message: `${ref} footprint ${c.footprint} does not resolve: ${(e as Error).message}`,
					key: `footprints|unresolved|${ref}`,
				});
				continue;
			}
			const lib = symbols.get(ref);
			if (c.excludeFromBoard || exempt.has(ref) || !lib) continue;
			const pins = new Set(
				libGeom(lib)
					.pins.filter(p => p.style <= 1)
					.flatMap(p => padNumbers(p.num)),
			);
			const padNums = new Set(pads.filter(Boolean));
			for (const p of pins) {
				if (padNums.has(p) || exempt.has(`${ref}.${p}`)) continue;
				out.push({
					...base,
					severity: "error",
					message: `${ref} pin ${p} has no pad in ${c.footprint}`,
					key: `footprints|pin_no_pad|${ref}|${p}`,
				});
			}
			for (const p of padNums) {
				if (pins.has(p) || exempt.has(`${ref}.${p}`)) continue;
				out.push({
					...base,
					severity: "warning",
					message: `${ref} footprint ${c.footprint} pad ${p} has no symbol pin`,
					key: `footprints|pad_no_pin|${ref}|${p}`,
				});
			}
		}
		return out;
	},
};

const fields: CheckDef = {
	name: "fields",
	doc: "BOM parts carry a Value and, unless DNP, every settings.fields.required field (error; fields.exemptPrefixes and fieldExempt skip); DNP parts still in the BOM and BOM-excluded parts with sourcing fields (warning).",
	byDefault: true,
	async run(ctx) {
		const { required, exemptPrefixes } = ctx.project.settings.fields;
		const fieldExempt = ctx.project.settings.fieldExempt;
		const out: Finding[] = [];
		for (const c of await components(ctx)) {
			const ref = c.ref;
			const base = { check: "fields", refs: [ref], sheet: sheetOfPath(c.sheetName) };
			const filled = required.filter(f => (c.fields[f] ?? "").trim());
			if (c.excludeFromBom) {
				if (filled.length) {
					out.push({
						...base,
						severity: "warning",
						message: `${ref} is excluded from the BOM but carries sourcing fields ${filled.join(", ")}`,
						key: `fields|excluded_sourced|${ref}`,
					});
				}
				continue;
			}
			if (!c.value.trim()) out.push({ ...base, severity: "error", message: `${ref} has an empty Value`, key: `fields|empty_value|${ref}` });
			if (c.dnp) {
				out.push({
					...base,
					severity: "warning",
					message: `${ref} is DNP but still in the BOM (set exclude_from_bom, or clear DNP if it is fitted)`,
					key: `fields|dnp_in_bom|${ref}`,
				});
				continue;
			}
			if (exemptPrefixes.some(p => ref.startsWith(p))) continue;
			for (const f of required) {
				if (filled.includes(f) || (fieldExempt[f] ?? []).includes(ref)) continue;
				out.push({ ...base, severity: "error", message: `${ref} (${c.value}) has no ${f}`, key: `fields|missing|${ref}|${f}` });
			}
		}
		return out;
	},
};

const SI: Record<string, number> = { p: 1e-12, n: 1e-9, u: 1e-6, m: 1e-3, R: 1, r: 1, k: 1e3, K: 1e3, M: 1e6, G: 1e9 };

/**
 * Numeric value of an R/C/L Value: SI prefix or RKM notation, optional unit ("4k7" = "4.7k" =
 * "4700", "100n" = "100nF" = "0.1u", "1R5" = "1.5", "10 kΩ"); words after the value (a rating such
 * as "50V", "1%", "X7R") are ignored. Undefined when it doesn't parse.
 */
export function parseValue(value: string): number | undefined {
	const [first = "", second = ""] = value.trim().replace(/[µμ]/g, "u").split(/\s+/);
	// "10 k", "100 nF": a second word that is only a prefix and/or unit belongs to the value
	const s = second && /^[pnumkKMG]?(?:Ω|[Oo]hms?|[FfHh])?$/.test(second) ? first + second : first;
	const m = /^(\d+(?:\.\d+)?|\.\d+)(?:([pnumkKMGRr])(\d*))?(?:Ω|[Oo]hms?|[FfHh])?$/.exec(s);
	if (!m || (m[3] && m[1]!.includes("."))) return undefined;
	const n = Number(m[3] ? `${m[1]}.${m[3]}` : m[1]) * (m[2] ? SI[m[2]]! : 1);
	return Number(n.toPrecision(12));
}

const sortRefs = (refs: string[]) => refs.sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));

interface BomPart {
	c: Component;
	prefix: string;
	/** MPN, or "LCSC <code>" when the MPN is empty. */
	id: string;
	/** Normalized Value: the number for a parseable R/C/L value, else the trimmed Value. */
	norm: string;
	num?: number;
}

/** "100n/0.1u (C1, C2)": the Values as written and the references of a group of parts. */
function listing(parts: BomPart[]): string {
	return `${[...new Set(parts.map(p => p.c.value.trim()))].join("/")} (${sortRefs(parts.map(p => p.c.ref)).join(", ")})`;
}

function groupBy<T>(items: T[], key: (t: T) => string): Map<string, T[]> {
	const m = new Map<string, T[]>();
	for (const it of items) {
		const k = key(it);
		m.set(k, [...(m.get(k) ?? []), it]);
	}
	return m;
}

const bomLines: CheckDef = {
	name: "bom_lines",
	doc: 'BOM parts (not DNP, not excluded from the BOM) with an MPN field (else LCSC; parts with neither are skipped): same reference prefix, normalized Value and footprint but different MPNs (warning); one MPN with different normalized Values: error when all are R/C/L parts with parsed values (a wrong part), else info (Value naming the function, e.g. RESET/BOOT on one switch); R/C/L parts with the same footprint whose adjacent distinct values differ by a ratio <= 2.2 (info, merge candidates). R/C/L Values are compared as numbers: SI prefix or RKM ("4k7" = "4.7k" = "4700", "100n" = "0.1uF", "1R5"), units Ω/ohm/F/H, words after the value (a rating: "50V", "1%", "X7R") ignored; other Values compare as trimmed text.',
	byDefault: true,
	async run(ctx) {
		const nl = await ctx.analysis.netlist();
		const parts: BomPart[] = [];
		for (const c of nl.components.values()) {
			if (c.excludeFromBom || c.dnp) continue;
			const mpn = (c.fields.MPN ?? "").trim();
			const lcsc = (c.fields.LCSC ?? "").trim();
			if (!mpn && !lcsc) continue;
			const prefix = /^\D*/.exec(c.ref)![0];
			const num = ["R", "C", "L"].includes(prefix) ? parseValue(c.value) : undefined;
			parts.push({ c, prefix, id: mpn || `LCSC ${lcsc}`, norm: num === undefined ? c.value.trim() : String(num), num });
		}
		const out: Finding[] = [];
		const add = (severity: Finding["severity"], group: BomPart[], message: string, key: string) =>
			out.push({ check: "bom_lines", severity, message, key, refs: sortRefs(group.map(p => p.c.ref)) });
		for (const group of groupBy(parts, p => `${p.prefix}\0${p.norm}\0${p.c.footprint}`).values()) {
			const byId = groupBy(group, p => p.id);
			if (byId.size < 2) continue;
			const { prefix, norm, c } = group[0]!;
			add(
				"warning",
				group,
				`Value ${[...new Set(group.map(p => p.c.value.trim()))].join("/")} with footprint ${c.footprint} has different part numbers: ${[...byId].map(([id, ps]) => `${id} (${sortRefs(ps.map(p => p.c.ref)).join(", ")})`).join("; ")}`,
				`bom_lines|mpns|${prefix}|${c.footprint}|${norm}`,
			);
		}
		for (const [id, group] of groupBy(parts, p => p.id)) {
			const byValue = groupBy(group, p => p.norm);
			if (byValue.size < 2) continue;
			const quantities = group.every(p => p.num !== undefined);
			add(quantities ? "error" : "info", group, `${id} has different Values: ${[...byValue.values()].map(listing).join("; ")}`, `bom_lines|values|${id}`);
		}
		const rcl = parts.filter(p => p.num !== undefined && p.num > 0);
		for (const group of groupBy(rcl, p => `${p.prefix}\0${p.c.footprint}`).values()) {
			const byValue = [...groupBy(group, p => p.norm).values()].sort((a, b) => a[0]!.num! - b[0]!.num!);
			for (let i = 1; i < byValue.length; i++) {
				const [lo, hi] = [byValue[i - 1]!, byValue[i]!];
				const ratio = hi[0]!.num! / lo[0]!.num!;
				if (ratio > 2.2) continue;
				add(
					"info",
					[...lo, ...hi],
					`merge candidates (${lo[0]!.c.footprint}): ${listing(lo)} and ${listing(hi)} differ by x${Number(ratio.toFixed(2))}`,
					`bom_lines|merge|${lo[0]!.prefix}|${lo[0]!.c.footprint}|${lo[0]!.norm}|${hi[0]!.norm}`,
				);
			}
		}
		if (!ctx.sheets) return out;
		const paths = new Set(ctx.sheets.map(s => s.namePath));
		return out.filter(f => f.refs!.some(r => paths.has(nl.components.get(r)!.sheetName)));
	},
};

export const checks: CheckDef[] = [footprints, fields, bomLines];
