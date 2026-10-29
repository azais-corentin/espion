/**
 * Part data checks (`footprints`, `fields`): every board part has a footprint that resolves and
 * whose pads match the symbol's pins, and every BOM part carries the sourcing fields the project
 * requires. Both read KiCad's netlist, so DNP / BOM / board flags are KiCad's own.
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

export const checks: CheckDef[] = [footprints, fields];
