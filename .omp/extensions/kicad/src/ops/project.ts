/**
 * Project settings ops: net classes and their patterns, BOM presets, bus aliases, ERC and DRC
 * exclusions (.kicad_pro), and custom design rules (.kicad_dru).
 *
 * .kicad_pro changes go through `proJson` (byte-exact outside the changed keys). The rules file is
 * edited as text: only the named rule's text changes, every other rule keeps its comments and
 * layout.
 */
import { conditionNetclasses, itemProblems, type NetclassJson, netSettings, scanDru } from "../checks/rules";
import { EXCLUSION_KINDS, type ExclusionKind, editableList, selectExclusions } from "../exclusions";
import { key, onInterior, type Pt, type Seg, touches } from "../geom";
import type { Project } from "../project";
import type { LabelKind } from "../sch";
import { child, head, type List, parse, text } from "../sexpr";
import { type Fields, validate } from "../spec";
import type { OpContext, OpDef } from "../types";
import { entrySides } from "./schematic";

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

// ---- bus aliases --------------------------------------------------------------------------------

/** schematic.bus_aliases of the .kicad_pro: alias name → member names, in KiCad's order (by name). */
export function busAliases(project: Project): Record<string, string[]> {
	const sch = project.proJson.data.schematic;
	const aliases = typeof sch === "object" && sch !== null && "bus_aliases" in sch ? sch.bus_aliases : undefined;
	if (typeof aliases !== "object" || aliases === null || Array.isArray(aliases)) {
		throw new Error(`${project.rel(project.proFile)} has no schematic.bus_aliases: open and save the project in KiCad 10 once`);
	}
	// KiCad's own output: every value is the list of member names
	return aliases as Record<string, string[]>;
}

/** Where a label, hierarchical label or sheet pin names a bus alias. */
export interface AliasUse {
	sheet: string;
	kind: "bus label" | "global label" | "hierarchical label" | "sheet pin";
	/** The label or pin text: "{IMU}", or a group naming the alias ("{IMU SPI}", "X{IMU}"). */
	text: string;
	at: Pt;
	/** A sheet pin's sheet block. */
	block?: string;
}

/** A local label on a wire that a bus entry joins to a bus carrying the alias. */
export interface AliasTap {
	sheet: string;
	/** The net the label takes off the bus (its text without a group's "X." prefix): a member, or a bit of a vector member. */
	net: string;
	label: string;
	at: Pt;
}

/** The taps of an alias member: those naming it or, for a vector member (KiCad's `D[0..7]` or `D[7..0]`, text after `]` kept: D0 … D7), one of its bits. */
export function memberTaps(member: string, taps: AliasTap[]): AliasTap[] {
	const v = /^(.*)\[(\d+)\.\.(\d+)\](.*)$/.exec(member);
	if (!v) return taps.filter(t => t.net === member);
	const [pre, post] = [v[1]!, v[4]!];
	const [lo, hi] = [Math.min(Number(v[2]), Number(v[3])), Math.max(Number(v[2]), Number(v[3]))];
	return taps.filter(t => {
		if (t.net.length <= pre.length + post.length || !t.net.startsWith(pre) || !t.net.endsWith(post)) return false;
		const bit = t.net.slice(pre.length, t.net.length - post.length);
		return /^(0|[1-9]\d*)$/.test(bit) && lo <= Number(bit) && Number(bit) <= hi;
	});
}

const USE_KIND: Record<LabelKind, AliasUse["kind"]> = { label: "bus label", global_label: "global label", hierarchical_label: "hierarchical label" };

/** The prefix a bus label text gives the alias's members ("" for "{IMU}" or "{IMU SPI}", "X." for "X{IMU}"); undefined when the text doesn't name the alias. */
function aliasPrefix(label: string, alias: string): string | undefined {
	const m = /^([^{}]*)\{([^{}]*)\}$/.exec(label.trim());
	if (!m || !m[2]!.split(/[\s,]+/).includes(alias)) return undefined;
	return m[1] ? `${m[1]}.` : "";
}

/** Segments joined at their ends and where one of `pts` or another segment's end lands inside one: a point's group. */
function joined(segs: Seg[], pts: Pt[]): (p: Pt) => string {
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
	for (const s of segs) union(key(s.a), key(s.b));
	for (const p of [...pts, ...segs.flatMap(s => [s.a, s.b])]) for (const s of segs) if (onInterior(p, s)) union(key(p), key(s.a));
	return p => find(key(p));
}

/**
 * Every sheet instance's labels, hierarchical labels and sheet pins naming a bus alias (alone or in
 * a group), and the local labels taking members off a bus that carries it: a bus (buses joined at
 * their ends or an end landing inside another) one of those sits on, a bus entry from it, and a
 * local label on the wire at the entry's wire side (or at that point).
 */
export function busAliasUses(project: Project, alias: string): { uses: AliasUse[]; taps: AliasTap[] } {
	const uses: AliasUse[] = [];
	const taps: AliasTap[] = [];
	for (const inst of project.hierarchy()) {
		const sch = inst.sch;
		const named: { use: AliasUse; prefix: string }[] = [];
		for (const l of sch.labels()) {
			const prefix = aliasPrefix(l.name, alias);
			if (prefix !== undefined) named.push({ use: { sheet: inst.name, kind: USE_KIND[l.kind], text: l.name, at: l.at }, prefix });
		}
		for (const b of sch.sheets()) {
			for (const sp of b.pins) {
				const prefix = aliasPrefix(sp.name, alias);
				if (prefix !== undefined) named.push({ use: { sheet: inst.name, kind: "sheet pin", text: sp.name, at: sp.at, block: b.name }, prefix });
			}
		}
		if (!named.length) continue;
		uses.push(...named.map(n => n.use));
		const buses = sch.buses();
		const onBus = (p: Pt) => buses.some(s => touches(p, s));
		const entries = sch.busEntries().map(e => entrySides(sch, e));
		const busOf = joined(buses, [...named.map(n => n.use.at), ...entries.map(e => e.bus)]);
		const prefixes = new Map<string, Set<string>>();
		for (const n of named) {
			if (!onBus(n.use.at)) continue;
			const g = busOf(n.use.at);
			prefixes.set(g, (prefixes.get(g) ?? new Set()).add(n.prefix));
		}
		const tapped = entries.filter(e => onBus(e.bus) && prefixes.has(busOf(e.bus)));
		if (!tapped.length) continue;
		const locals = sch.labels().filter(l => l.kind === "label");
		const wireOf = joined(sch.wires(), [...locals.map(l => l.at), ...tapped.map(e => e.wire)]);
		const seen = new Set<List>();
		for (const e of tapped) {
			const wire = wireOf(e.wire);
			for (const prefix of prefixes.get(busOf(e.bus))!) {
				for (const l of locals) {
					if (seen.has(l.node) || !l.name.startsWith(prefix) || wireOf(l.at) !== wire) continue;
					seen.add(l.node);
					taps.push({ sheet: inst.name, net: l.name.slice(prefix.length), label: l.name, at: l.at });
				}
			}
		}
	}
	return { uses, taps };
}

const setBusAlias: OpDef = {
	op: "set_bus_alias",
	group: "project",
	doc:
		"Create a bus alias (a bus labelled `{NAME}` carries its members) or replace its member list. " +
		"A member removed while still in use is refused: a local label of that name (or of one of its bits, for a vector member such as `D[0..7]`) that no remaining member covers, on a wire that a bus entry joins to a bus carrying the alias " +
		"(a bus label, hierarchical label or sheet pin naming it, alone or in a group such as `{NAME OTHER}`, sits on that bus or on a bus joined to it), on any sheet; remove that label and its bus entry first.",
	fields: {
		name: { t: "string", req: true, doc: "alias name, without braces" },
		members: { t: "strings", req: true, doc: 'member net names in order, e.g. ["IMU_SDA", "IMU_SCL", "IMU_INT1"]' },
	},
	apply(ctx, args) {
		const name = String(args.name);
		if (!name || /[\s{}[\]]/.test(name)) throw new Error(`name: ${JSON.stringify(name)} must be non-empty, without spaces, braces or brackets`);
		const members = (args.members as string[]).map(m => m.trim());
		if (members.length === 0) throw new Error("members: give at least one member");
		const bad = members.find(m => !m || /[\s{}]/.test(m));
		if (bad !== undefined) throw new Error(`members: ${JSON.stringify(bad)} must be non-empty, without spaces or braces`);
		const dup = members.find((m, i) => members.indexOf(m) !== i);
		if (dup !== undefined) throw new Error(`members: ${dup} is given twice`);
		const aliases = busAliases(ctx.project);
		const old = Object.hasOwn(aliases, name) ? aliases[name]! : undefined;
		const removed = (old ?? []).filter(m => !members.includes(m));
		const added = members.filter(m => !old?.includes(m));
		if (removed.length) {
			const taps = busAliasUses(ctx.project, name).taps;
			const kept = new Set(members.flatMap(m => memberTaps(m, taps)));
			const inUse = removed.map(m => ({ m, taps: memberTaps(m, taps).filter(t => !kept.has(t)) })).filter(x => x.taps.length);
			if (inUse.length) {
				const where = inUse.flatMap(x => x.taps.map(t => `local label ${t.label} on ${t.sheet} at (${t.at[0]}, ${t.at[1]})`)).join("; ");
				throw new Error(`bus alias ${name}: can't remove ${inUse.map(x => x.m).join(", ")}, still taken off a {${name}} bus by ${where}: remove that label and its bus entry first`);
			}
		}
		if (old && !added.length && !removed.length && old.every((m, i) => m === members[i])) {
			ctx.notes.push(`bus alias ${name} already has those members`);
			return;
		}
		if (old) aliases[name] = members;
		else {
			// KiCad writes the aliases sorted by name
			const all = Object.entries({ ...aliases, [name]: members }).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
			for (const k of Object.keys(aliases)) delete aliases[k];
			Object.assign(aliases, Object.fromEntries(all));
		}
		const change = old
			? [added.length ? `added ${added.join(", ")}` : "", removed.length ? `removed ${removed.join(", ")}` : "", !added.length && !removed.length ? "members reordered" : ""].filter(Boolean).join("; ")
			: `created with ${members.join(", ")}`;
		ctx.notes.push(`bus alias ${name}: ${change}`);
		ctx.project.touchPro();
	},
};

const removeBusAlias: OpDef = {
	op: "remove_bus_alias",
	group: "project",
	doc: "Delete a bus alias; refused while a bus label, hierarchical label or sheet pin names it (alone or in a group such as `{NAME OTHER}`) on any sheet.",
	fields: { name: { t: "string", req: true, doc: "alias name, without braces" } },
	apply(ctx, args) {
		const name = String(args.name);
		const aliases = busAliases(ctx.project);
		if (!Object.hasOwn(aliases, name)) throw new Error(`no bus alias "${name}"; aliases: ${Object.keys(aliases).join(", ") || "none"}`);
		const { uses } = busAliasUses(ctx.project, name);
		if (uses.length) {
			const where = uses.map(u => `${u.kind} ${u.text}${u.block ? ` of sheet block ${u.block}` : ""} on ${u.sheet} at (${u.at[0]}, ${u.at[1]})`);
			throw new Error(`bus alias ${name} is still used: ${where.join("; ")}; remove those first`);
		}
		const members = aliases[name]!;
		delete aliases[name];
		ctx.notes.push(`bus alias ${name} removed (members: ${members.join(", ")})`);
		ctx.project.touchPro();
	},
};

// ---- ERC and DRC exclusions ---------------------------------------------------------------------

const EXCLUSION_PICK: Fields = {
	kind: { t: "string", req: true, values: EXCLUSION_KINDS, doc: '"erc" (erc.erc_exclusions) or "drc" (board.design_settings.drc_exclusions)' },
	key: { t: "string", doc: "the whole exclusion key, as kicad_query exclusions lists it" },
	match: { t: "string", doc: "instead of key: text in the key or comment (case-insensitive), e.g. a uuid or the comment's start" },
};

const removeExclusion: OpDef = {
	op: "remove_exclusion",
	group: "project",
	doc:
		"Delete ERC or DRC exclusions from the .kicad_pro (kicad_query exclusions lists them, stale ones too). One exclusion unless all. " +
		"A violation the exclusion still hid shows again in ERC/DRC: the edit reports it and still applies (it isn't new); fix it or exclude it again in KiCad.",
	fields: { ...EXCLUSION_PICK, all: { t: "boolean", doc: "remove every exclusion match selects (default false: exactly one must match)" } },
	apply(ctx, args) {
		const kind = args.kind as ExclusionKind;
		const data = ctx.project.proJson.data;
		const list = editableList(data, kind, ctx.project.rel(ctx.project.proFile));
		const hits = selectExclusions(data, kind, { key: args.key as string | undefined, match: args.match as string | undefined, all: args.all === true });
		for (const e of [...hits].reverse()) list.splice(e.index, 1);
		for (const e of hits) ctx.notes.push(`${kind.toUpperCase()} exclusion removed: ${e.rule} @ (${e.at[0]}, ${e.at[1]})${e.comment ? ` "${e.comment}"` : " (no comment)"}`);
		ctx.project.touchPro();
		if (kind === "drc") ctx.touchBoard();
	},
};

const setExclusionComment: OpDef = {
	op: "set_exclusion_comment",
	group: "project",
	doc: "Set the comment (the reason) of one ERC or DRC exclusion in the .kicad_pro; ERC and DRC reports carry it with the excluded violation.",
	fields: { ...EXCLUSION_PICK, comment: { t: "string", req: true, doc: 'why the violation is excluded, with its source (datasheet page); "" removes it (the erc check then warns)' } },
	apply(ctx, args) {
		const kind = args.kind as ExclusionKind;
		const data = ctx.project.proJson.data;
		const list = editableList(data, kind, ctx.project.rel(ctx.project.proFile));
		// selectExclusions throws unless exactly one matches
		const e = selectExclusions(data, kind, { key: args.key as string | undefined, match: args.match as string | undefined })[0]!;
		const comment = String(args.comment);
		const what = `${kind.toUpperCase()} exclusion ${e.rule} @ (${e.at[0]}, ${e.at[1]})`;
		if (e.comment === comment) {
			ctx.notes.push(`${what} already has that comment`);
			return;
		}
		list[e.index] = [e.key, comment];
		ctx.notes.push(`${what}: comment ${comment ? (e.comment ? "replaced" : "added") : "removed"}`);
		ctx.project.touchPro();
	},
};

// ---- design rules -------------------------------------------------------------------------------

/**
 * The `#` comment lines directly above the top-level list starting at `start`: where the first one
 * starts and their text, one line each without the `#` and the space after it. A list that
 * doesn't start its line has none.
 */
function commentAbove(src: string, start: number): { from: number; text: string } {
	let from = src.lastIndexOf("\n", start - 1) + 1;
	if (src.slice(from, start).trim()) return { from: start, text: "" };
	const lines: string[] = [];
	while (from > 0) {
		const prev = src.lastIndexOf("\n", from - 2) + 1;
		const m = /^\s*#[ \t]?(.*?)\s*$/.exec(src.slice(prev, from - 1));
		if (!m) break;
		lines.unshift(m[1]!);
		from = prev;
	}
	return { from, text: lines.join("\n") };
}

/** The rules of a .kicad_dru text, each with the comment lines directly above it (see commentAbove); throws on syntax problems. */
export function druRules(src: string): { node: List; comment?: string }[] {
	const { items, problems } = scanDru(src);
	if (problems.length) throw new Error(problems.join("; "));
	return items.filter(it => head(it.node) === "rule").map(it => ({ node: it.node, comment: commentAbove(src, it.start).text || undefined }));
}

/**
 * The header comment of a .kicad_dru text: the `#` lines after `(version N)`, ending at a blank
 * line before the first rule's own comment (see commentAbove) or at the end of the file. Returns
 * the text up to the line after `(version N)` (`before`), the text from the first rule's comment
 * on (`after`) and the header between them, one line each without the `#` and the space after it
 * (blank lines inside it as empty lines). Undefined without `(version N)`; throws on syntax problems.
 */
function druHeaderSplit(src: string): { before: string; after: string; text: string } | undefined {
	const { items, problems } = scanDru(src);
	if (problems.length) throw new Error(problems.join("; "));
	const ver = items.find(it => head(it.node) === "version");
	if (!ver) return undefined;
	const next = items.find(it => it.start > ver.end);
	const stop = next ? commentAbove(src, next.start).from : src.length;
	const nl = src.indexOf("\n", ver.end);
	// the next list may share the version's line: then there is no header and the cut is right after (version N)
	const cut = nl >= 0 && nl < stop ? nl + 1 : ver.end;
	const before = cut === ver.end ? `${src.slice(0, ver.end)}\n` : src.slice(0, cut);
	const lines = src
		.slice(cut, stop)
		.split("\n")
		.map(l => /^\s*#[ \t]?(.*?)\s*$/.exec(l)?.[1] ?? "");
	while (lines.length && !lines[0]) lines.shift();
	while (lines.length && !lines[lines.length - 1]) lines.pop();
	return { before, after: src.slice(stop), text: lines.join("\n") };
}

/** The header comment of a .kicad_dru text (see druHeaderSplit), undefined when it has none or no `(version N)`. */
export function druHeader(src: string): string | undefined {
	return druHeaderSplit(src)?.text || undefined;
}

const setRule: OpDef = {
	op: "set_rule",
	group: "rules",
	doc:
		"Add a custom design rule to the .kicad_dru (created with (version 1) when missing) or replace the rule with the same name; other rules keep their text and comments. " +
		"A condition on a net names it with A.NetName == '/VIN' (A.Net is the net's number: comparing it with a name matches nothing, and KiCad gives no error).",
	fields: {
		rule: {
			t: "string",
			req: true,
			doc: `one complete rule, e.g. (rule "USB spacing" (condition "A.NetClass == 'USB'") (constraint clearance (min 0.3mm)))`,
		},
		comment: {
			t: "string",
			doc: 'why the rule exists (one or more lines, each written as a "# " line directly above the rule). Replacing a rule: given replaces the comment lines above it ("" removes them), omitted keeps them',
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
		const comment = args.comment as string | undefined;
		const commentLines = (indent: string) =>
			comment
				? comment
						.split(/\r?\n/)
						.map(l => `${`${indent}# ${l}`.trimEnd()}\n`)
						.join("")
				: "";
		const old = ctx.project.fileText(ctx.project.druFile);
		if (old === undefined) {
			ctx.project.setFile(ctx.project.druFile, `(version 1)\n\n${commentLines("")}${src}\n`);
			ctx.notes.push(`created ${ctx.project.rel(ctx.project.druFile)} with rule "${name}"`);
		} else {
			const same = scanDru(old).items.filter(it => head(it.node) === "rule" && text(it.node, 1) === name);
			if (same.length > 1) throw new Error(`${same.length} rules are named "${name}": remove_rule it first, then set_rule once`);
			const hit = same[0];
			let out: string;
			if (!hit) out = `${old.trimEnd()}\n\n${commentLines("")}${src}\n`;
			else if (comment === undefined) out = old.slice(0, hit.start) + src + old.slice(hit.end);
			else {
				// replaces the old comment lines and the rule's indent; a rule not starting its line gets a line break first
				const { from } = commentAbove(old, hit.start);
				const indent = old.slice(old.lastIndexOf("\n", hit.start - 1) + 1, hit.start);
				const lead = indent.trim() ? (comment ? `\n${commentLines("")}` : "") : commentLines(indent) + indent;
				out = old.slice(0, from) + lead + src + old.slice(hit.end);
			}
			ctx.project.setFile(ctx.project.druFile, out);
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
			const nl = src.indexOf("\n", it.end);
			const before = src.slice(0, commentAbove(src, it.start).from);
			let after = nl < 0 ? "" : src.slice(nl + 1);
			if ((before.endsWith("\n\n") || before === "") && after.startsWith("\n")) after = after.slice(1);
			src = before + after;
		}
		ctx.project.setFile(ctx.project.druFile, src);
		ctx.notes.push(`rule "${name}" removed`);
	},
};

const setRulesHeader: OpDef = {
	op: "set_rules_header",
	group: "rules",
	doc: "Set the header comment of the .kicad_dru (written with (version 1) when the file is missing or empty): the # lines after (version N), a blank line before the first rule's own comment. Rules and their comments keep their text.",
	fields: {
		comment: {
			t: "string",
			req: true,
			doc: 'what the rules file is about (one or more lines, each written as a "# " line; an empty line gives a bare "#"). Replaces the existing header; "" removes it',
		},
	},
	apply(ctx, args) {
		const comment = String(args.comment)
			.replace(/^(?:[ \t]*\r?\n)+/, "")
			.trimEnd();
		const lines = comment
			.split(/\r?\n/)
			.map(l => `${`# ${l}`.trimEnd()}\n`)
			.join("");
		const file = ctx.project.rel(ctx.project.druFile);
		const old = ctx.project.fileText(ctx.project.druFile);
		if (!old?.trim()) {
			if (!comment) {
				ctx.notes.push(`${file} ${old === undefined ? "does not exist" : "is empty"}; no header to remove`);
				return;
			}
			ctx.project.setFile(ctx.project.druFile, `(version 1)\n\n${lines}`);
			ctx.notes.push(`${old === undefined ? "created" : "filled the empty"} ${file} with a header comment`);
			return;
		}
		const split = druHeaderSplit(old);
		if (!split) throw new Error(`${file} has no (version N) line to put the header after`);
		const { before, after, text: was } = split;
		if (!comment && !was) {
			ctx.notes.push(`${file} has no header comment to remove`);
			return;
		}
		const body = comment ? `\n${lines}${after ? "\n" : ""}` : after ? "\n" : "";
		ctx.project.setFile(ctx.project.druFile, before + body + after);
		ctx.notes.push(`rules header ${!comment ? "removed" : was ? "replaced" : "added"}`);
	},
};

export const ops: OpDef[] = [
	setNetclass,
	removeNetclass,
	setNetclassPatterns,
	setBomPreset,
	removeBomPreset,
	setBusAlias,
	removeBusAlias,
	removeExclusion,
	setExclusionComment,
	setRule,
	removeRule,
	setRulesHeader,
];
