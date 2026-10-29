/**
 * Project settings ops: net classes and their patterns, BOM presets (.kicad_pro), and custom
 * design rules (.kicad_dru).
 *
 * .kicad_pro changes go through `proJson` (byte-exact outside the changed keys). The rules file is
 * edited as text: only the named rule's text changes, every other rule keeps its comments and
 * layout.
 */
import { conditionNetclasses, itemProblems, type NetclassJson, netSettings, scanDru } from "../checks/rules";
import type { Project } from "../project";
import { child, head, type List, parse, text } from "../sexpr";
import { type Fields, validate } from "../spec";
import type { OpContext, OpDef } from "../types";

// ---- net classes --------------------------------------------------------------------------------

/** set_netclass arguments that are KiCad class settings, with their docs. */
const CLASS_SETTINGS: Fields = {
	clearance: { t: "number", doc: "copper clearance (mm)" },
	track_width: { t: "number", doc: "track width (mm)" },
	via_diameter: { t: "number", doc: "via pad diameter (mm)" },
	via_drill: { t: "number", doc: "via drill (mm)" },
	diff_pair_width: { t: "number", doc: "diff pair track width (mm)" },
	diff_pair_gap: { t: "number", doc: "diff pair gap (mm)" },
	diff_pair_via_gap: { t: "number", doc: "diff pair via gap (mm)" },
	microvia_diameter: { t: "number", doc: "microvia diameter (mm)" },
	microvia_drill: { t: "number", doc: "microvia drill (mm)" },
	wire_width: { t: "integer", doc: "schematic wire width (mils)" },
	bus_width: { t: "integer", doc: "schematic bus width (mils)" },
	priority: { t: "integer", doc: "precedence when a net matches several classes (lower wins; new classes default to after the last one)" },
};

function findClass(project: Project, name: string): NetclassJson {
	const classes = netSettings(project).classes;
	const c = classes.find(x => x.name === name);
	if (!c) throw new Error(`no net class "${name}"; classes: ${classes.map(x => x.name).join(", ")}`);
	return c;
}

const setNetclass: OpDef = {
	op: "set_netclass",
	group: "project",
	doc: "Create a net class (cloned from Default) or update one; only the given settings change.",
	fields: { name: { t: "string", req: true, doc: "class name" }, ...CLASS_SETTINGS },
	apply(ctx, args) {
		const name = String(args.name).trim();
		if (!name) throw new Error("name: give a class name");
		const ns = netSettings(ctx.project);
		let cls = ns.classes.find(c => c.name === name);
		if (cls && name === "Default" && args.priority !== undefined) throw new Error("the Default class always comes last: its priority can't change");
		if (!cls) {
			const def = findClass(ctx.project, "Default");
			const last = Math.max(-1, ...ns.classes.filter(c => c.name !== "Default").map(c => (typeof c.priority === "number" ? c.priority : -1)));
			cls = { ...structuredClone(def), name, priority: last + 1 };
			ns.classes.push(cls);
			ctx.notes.push(`net class ${name} created from Default`);
		}
		for (const k of Object.keys(CLASS_SETTINGS)) {
			const v = args[k];
			if (typeof v !== "number") continue;
			if (v < 0 || (v === 0 && k !== "clearance" && k !== "priority")) throw new Error(`${k}: must be positive, got ${v}`);
			cls[k] = v;
		}
		ctx.project.touchPro();
	},
};

const removeNetclass: OpDef = {
	op: "remove_netclass",
	group: "project",
	doc: "Delete a net class with its netclass patterns and explicit net assignments (not Default).",
	fields: { name: { t: "string", req: true, doc: "class name" } },
	apply(ctx, args) {
		const name = String(args.name);
		if (name === "Default") throw new Error("the Default class can't be removed");
		const cls = findClass(ctx.project, name);
		const ns = netSettings(ctx.project);
		ns.classes.splice(ns.classes.indexOf(cls), 1);
		const patterns = ns.netclass_patterns.length;
		ns.netclass_patterns = ns.netclass_patterns.filter(p => p.netclass !== name);
		ctx.notes.push(`net class ${name} removed with ${patterns - ns.netclass_patterns.length} pattern(s)`);
		const assigned = ns.netclass_assignments ?? {};
		for (const [net, v] of Object.entries(assigned)) {
			if (v === name) delete assigned[net];
			else if (Array.isArray(v) && v.includes(name)) assigned[net] = v.filter(x => x !== name);
		}
		const rules = ctx.project.fileText(ctx.project.druFile);
		if (rules !== undefined) {
			const users = scanDru(rules)
				.items.filter(it => conditionNetclasses(text(child(it.node, "condition"), 1) ?? "").includes(name))
				.map(it => text(it.node, 1));
			if (users.length) ctx.notes.push(`rules still referring to ${name}: ${users.join(", ")} (update or remove_rule them)`);
		}
		ctx.project.touchPro();
	},
};

const setNetclassPatterns: OpDef = {
	op: "set_netclass_patterns",
	group: "project",
	doc: "Replace a class's net name patterns (full hierarchical names; * and ? wildcards or an anchored regex); [] removes them.",
	fields: {
		netclass: { t: "string", req: true, doc: "existing class (not Default)" },
		patterns: { t: "strings", req: true, doc: 'e.g. ["/Camera/CSI_*", "+3V3"]' },
	},
	apply(ctx, args) {
		const name = String(args.netclass);
		if (name === "Default") throw new Error("nets fall into Default when no pattern matches: give patterns to another class");
		findClass(ctx.project, name);
		const patterns = (args.patterns as string[]).map(p => p.trim());
		if (patterns.some(p => !p)) throw new Error("patterns: empty pattern");
		const ns = netSettings(ctx.project);
		const at = ns.netclass_patterns.findIndex(p => p.netclass === name);
		const rest = ns.netclass_patterns.filter(p => p.netclass !== name);
		const fresh = patterns.map(pattern => ({ netclass: name, pattern }));
		rest.splice(at < 0 ? rest.length : at, 0, ...fresh);
		ns.netclass_patterns = rest;
		ctx.project.touchPro();
	},
};

// ---- BOM presets --------------------------------------------------------------------------------

interface BomField {
	group_by: boolean;
	label: string;
	name: string;
	show: boolean;
}

interface BomPreset {
	name: string;
	exclude_dnp: boolean;
	include_excluded_from_bom: boolean;
	filter_string: string;
	group_symbols: boolean;
	sort_asc: boolean;
	sort_field: string;
	fields_ordered: BomField[];
}

const BOM_FIELD: Fields = {
	name: { t: "string", req: true, doc: "field name, or ${QUANTITY} / ${DNP} / ${ITEM_NUMBER}" },
	label: { t: "string", doc: "column header (default: name)" },
	show: { t: "boolean", doc: "column shown (default true)" },
	group_by: { t: "boolean", doc: "rows group on equal values (default false)" },
};

function bomPresets(project: Project): BomPreset[] {
	const sch = project.proJson.data.schematic;
	if (typeof sch !== "object" || sch === null || !("bom_presets" in sch) || !Array.isArray(sch.bom_presets)) {
		throw new Error(`${project.rel(project.proFile)} has no schematic.bom_presets: open and save the project in KiCad 10 once`);
	}
	// KiCad's own output: every entry has the shape of the presets it writes
	const presets: BomPreset[] = sch.bom_presets;
	return presets;
}

const setBomPreset: OpDef = {
	op: "set_bom_preset",
	group: "project",
	doc: "Create or replace a BOM preset (kicad-cli sch export bom --preset <name>); omitted options keep the preset's current values.",
	fields: {
		name: { t: "string", req: true, doc: "preset name" },
		fields: { t: "objects", req: true, doc: "columns in order: {name, label?, show?, group_by?}" },
		sort_field: { t: "string", doc: "field name to sort by (default Reference)" },
		sort_asc: { t: "boolean", doc: "ascending sort (default true)" },
		filter_string: { t: "string", doc: "KiCad row filter (default none)" },
		exclude_dnp: { t: "boolean", doc: "leave DNP parts out (default false)" },
		group_symbols: { t: "boolean", doc: "merge rows with equal group_by fields (default true)" },
		include_excluded_from_bom: { t: "boolean", doc: "list parts excluded from the BOM (default false)" },
	},
	apply(ctx, args) {
		const name = String(args.name).trim();
		if (!name) throw new Error("name: give a preset name");
		const cols = (args.fields as Record<string, unknown>[]).map((f, i): BomField => {
			validate(`set_bom_preset.fields[${i}]`, f, BOM_FIELD);
			const n = String(f.name);
			return { group_by: f.group_by === true, label: typeof f.label === "string" ? f.label : n, name: n, show: f.show !== false };
		});
		if (cols.length === 0) throw new Error("fields: give at least one column");
		const presets = bomPresets(ctx.project);
		const old = presets.find(p => p.name === name);
		const pick = <K extends keyof BomPreset>(k: K, dflt: BomPreset[K]): BomPreset[K] => (args[k] as BomPreset[K] | undefined) ?? old?.[k] ?? dflt;
		const preset: BomPreset = {
			name,
			exclude_dnp: pick("exclude_dnp", false),
			include_excluded_from_bom: pick("include_excluded_from_bom", false),
			filter_string: pick("filter_string", ""),
			group_symbols: pick("group_symbols", true),
			sort_asc: pick("sort_asc", true),
			sort_field: pick("sort_field", "Reference"),
			fields_ordered: cols,
		};
		if (!cols.some(c => c.name === preset.sort_field)) {
			throw new Error(`sort_field "${preset.sort_field}" is not one of the fields: ${cols.map(c => c.name).join(", ")}`);
		}
		if (old) presets[presets.indexOf(old)] = preset;
		else presets.push(preset);
		ctx.notes.push(`BOM preset ${name} ${old ? "replaced" : "created"}`);
		ctx.project.touchPro();
	},
};

const removeBomPreset: OpDef = {
	op: "remove_bom_preset",
	group: "project",
	doc: "Delete a BOM preset.",
	fields: { name: { t: "string", req: true, doc: "preset name" } },
	apply(ctx, args) {
		const presets = bomPresets(ctx.project);
		const i = presets.findIndex(p => p.name === args.name);
		if (i < 0) throw new Error(`no BOM preset "${String(args.name)}"; presets: ${presets.map(p => p.name).join(", ") || "none"}`);
		presets.splice(i, 1);
		ctx.project.touchPro();
	},
};

// ---- design rules -------------------------------------------------------------------------------

const setRule: OpDef = {
	op: "set_rule",
	group: "rules",
	doc: "Add a custom design rule to the .kicad_dru (created with (version 1) when missing) or replace the rule with the same name; other rules keep their text and comments.",
	fields: {
		rule: {
			t: "string",
			req: true,
			doc: `one complete rule, e.g. (rule "USB spacing" (condition "A.NetClass == 'USB'") (constraint clearance (min 0.3mm)))`,
		},
	},
	apply(ctx, args) {
		const src = String(args.rule).trim();
		let node: List;
		try {
			node = parse(src);
		} catch (e) {
			throw new Error(`rule: ${(e as Error).message}`);
		}
		if (head(node) !== "rule" || !src.startsWith("(") || !src.endsWith(")")) throw new Error('rule: give exactly one (rule "name" …) list');
		const problems = itemProblems(node);
		if (problems.length) throw new Error(`rule: ${problems.join("; ")}`);
		const name = text(node, 1)!;
		const old = ctx.project.fileText(ctx.project.druFile);
		if (old === undefined) {
			ctx.project.setFile(ctx.project.druFile, `(version 1)\n\n${src}\n`);
			ctx.notes.push(`created ${ctx.project.rel(ctx.project.druFile)} with rule "${name}"`);
		} else {
			const same = scanDru(old).items.filter(it => head(it.node) === "rule" && text(it.node, 1) === name);
			if (same.length > 1) throw new Error(`${same.length} rules are named "${name}": remove_rule it first, then set_rule once`);
			const hit = same[0];
			ctx.project.setFile(ctx.project.druFile, hit ? old.slice(0, hit.start) + src + old.slice(hit.end) : `${old.trimEnd()}\n\n${src}\n`);
			ctx.notes.push(`rule "${name}" ${hit ? "replaced" : "added"}`);
		}
		const classes = new Set(netSettings(ctx.project).classes.map(c => c.name));
		const unknown = conditionNetclasses(text(child(node, "condition"), 1) ?? "").filter(c => !classes.has(c));
		if (unknown.length) ctx.notes.push(`rule "${name}" names net classes that don't exist: ${unknown.join(", ")}`);
	},
};

const removeRule: OpDef = {
	op: "remove_rule",
	group: "rules",
	doc: "Delete every rule with this name from the .kicad_dru, with the comment lines directly above it.",
	fields: { name: { t: "string", req: true, doc: "rule name" } },
	apply(ctx, args) {
		const name = String(args.name);
		const old = ctx.project.fileText(ctx.project.druFile);
		if (old === undefined) throw new Error(`${ctx.project.rel(ctx.project.druFile)} does not exist`);
		const rules = scanDru(old).items.filter(it => head(it.node) === "rule");
		const hits = rules.filter(it => text(it.node, 1) === name);
		if (hits.length === 0) throw new Error(`no rule "${name}"; rules: ${rules.map(it => text(it.node, 1)).join(", ") || "none"}`);
		let src = old;
		for (const it of hits.reverse()) {
			let start = src.lastIndexOf("\n", it.start - 1) + 1;
			for (;;) {
				const prev = src.lastIndexOf("\n", start - 2) + 1;
				if (start === 0 || !/^\s*#/.test(src.slice(prev, start))) break;
				start = prev;
			}
			const nl = src.indexOf("\n", it.end);
			const before = src.slice(0, start);
			let after = nl < 0 ? "" : src.slice(nl + 1);
			if ((before.endsWith("\n\n") || before === "") && after.startsWith("\n")) after = after.slice(1);
			src = before + after;
		}
		ctx.project.setFile(ctx.project.druFile, src);
		ctx.notes.push(`rule "${name}" removed`);
	},
};

export const ops: OpDef[] = [setNetclass, removeNetclass, setNetclassPatterns, setBomPreset, removeBomPreset, setRule, removeRule];
