/**
 * Net classes and custom design rules (`netclasses`, `rules`).
 *
 * kicad-cli silently ignores a .kicad_dru with any error, so `druProblems` proves KiCad loaded the
 * file: a sentinel rule appended to a copy must fire in a DRC run on a board with one track. When
 * it doesn't, the file text is scanned to say where it goes wrong.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { pcbDrc, tmpDir } from "../cli";
import type { Project } from "../project";
import { child, head, isList, type List, parse, Str, text } from "../sexpr";
import type { CheckDef, Finding } from "../types";

// ---- rules file text ----------------------------------------------------------------------------

/** Constraint names KiCad 10 accepts (each verified to parse with kicad-cli 10.0). */
const CONSTRAINTS: readonly string[] = [
	"annular_width",
	"assertion",
	"bridged_mask",
	"clearance",
	"connection_width",
	"courtyard_clearance",
	"creepage",
	"diff_pair_gap",
	"diff_pair_uncoupled",
	"disallow",
	"edge_clearance",
	"hole_clearance",
	"hole_size",
	"hole_to_hole",
	"length",
	"mechanical_clearance",
	"mechanical_hole_clearance",
	"min_resolved_spokes",
	"physical_clearance",
	"physical_hole_clearance",
	"silk_clearance",
	"skew",
	"solder_mask_expansion",
	"solder_mask_sliver",
	"solder_paste_abs_margin",
	"solder_paste_rel_margin",
	"text_height",
	"text_thickness",
	"thermal_relief_gap",
	"thermal_spoke_width",
	"track_angle",
	"track_segment_length",
	"track_width",
	"via_count",
	"via_diameter",
	"zone_connection",
];
const RULE_CLAUSES: readonly string[] = ["condition", "constraint", "layer", "severity"];
const CONSTRAINT_LIMITS: readonly string[] = ["min", "max", "opt", "within_diff_pairs"];

/** One top-level list of a rules file: its text range and the line it starts on. */
export interface DruItem {
	start: number;
	end: number;
	line: number;
	node: List;
}

/**
 * Top-level lists of a rules file, plus syntax problems (unbalanced parentheses, unterminated
 * strings). Lines whose first non-blank character is `#` are comments, as in KiCad.
 */
export function scanDru(src: string): { items: DruItem[]; problems: string[] } {
	const spans: { start: number; end: number; line: number }[] = [];
	const problems: string[] = [];
	let depth = 0;
	let start = 0;
	let startLine = 1;
	let line = 1;
	let lineStart = true;
	for (let i = 0; i < src.length; i++) {
		const c = src[i]!;
		if (c === "\n") {
			line++;
			lineStart = true;
			continue;
		}
		if (c === " " || c === "\t" || c === "\r") continue;
		if (lineStart && c === "#") {
			const nl = src.indexOf("\n", i);
			i = (nl < 0 ? src.length : nl) - 1;
			continue;
		}
		lineStart = false;
		if (c === '"') {
			const open = line;
			let j = i + 1;
			while (j < src.length && src[j] !== '"') {
				if (src[j] === "\\") j++;
				else if (src[j] === "\n") line++;
				j++;
			}
			if (j >= src.length) {
				problems.push(`line ${open}: string never closed (missing '"')`);
				return { items: [], problems };
			}
			i = j;
		} else if (c === "(") {
			if (depth > 0 && /^\(\s*rule[\s(")]/.test(src.slice(i, i + 12))) {
				problems.push(`line ${line}: a rule starts inside the list opened at line ${startLine} (missing ")" before it)`);
				depth = 0;
			}
			if (depth === 0) {
				start = i;
				startLine = line;
			}
			depth++;
		} else if (c === ")") {
			if (depth === 0) problems.push(`line ${line}: unbalanced ")"`);
			else if (--depth === 0) spans.push({ start, end: i + 1, line: startLine });
		} else if (depth === 0) {
			problems.push(`line ${line}: text outside any list`);
			while (i + 1 < src.length && !" \t\r\n()".includes(src[i + 1]!)) i++;
		}
	}
	if (depth > 0) problems.push(`line ${startLine}: list never closed (missing ")" at the end)`);
	const items: DruItem[] = [];
	for (const s of spans) {
		try {
			items.push({ ...s, node: parse(src.slice(s.start, s.end)) });
		} catch (e) {
			problems.push(`line ${s.line}: ${(e as Error).message}`);
		}
	}
	return { items, problems };
}

/** Structural problems of one top-level item: unknown clauses, constraint names or limits. */
export function itemProblems(node: List): string[] {
	const h = head(node);
	if (h === "version") return [];
	if (h !== "rule") return [`unknown top-level item "(${h ?? "?"}": a rules file holds (version 1) and (rule "name" …) items`];
	const name = text(node, 1);
	if (name === undefined) return ["rule without a name: write (rule \"name\" …)"];
	const out: string[] = [];
	const where = `rule "${name}"`;
	for (const c of node.slice(2)) {
		if (!isList(c)) {
			out.push(`${where}: stray token outside a clause`);
			continue;
		}
		const k = head(c) ?? "?";
		if (!RULE_CLAUSES.includes(k)) {
			out.push(`${where}: unknown clause "(${k}"; valid: ${RULE_CLAUSES.join(", ")}`);
		} else if (k === "condition" && !(c[1] instanceof Str)) {
			out.push(`${where}: the condition must be one quoted expression`);
		} else if (k === "constraint") {
			const cn = text(c, 1) ?? "";
			if (!CONSTRAINTS.includes(cn)) out.push(`${where}: unknown constraint "${cn}"; valid: ${CONSTRAINTS.join(", ")}`);
			for (const lim of c.slice(2)) {
				if (isList(lim) && !CONSTRAINT_LIMITS.includes(head(lim) ?? "")) {
					out.push(`${where}: unknown constraint limit "(${head(lim) ?? "?"}"; valid: ${CONSTRAINT_LIMITS.join(", ")}`);
				}
			}
		}
	}
	return out;
}

/** Problems located by reading the file: syntax first, then structure. */
function locateProblems(src: string): string[] {
	const { items, problems } = scanDru(src);
	for (const it of items) for (const p of itemProblems(it.node)) problems.push(`line ${it.line}: ${p}`);
	return problems;
}

const SENTINEL = "__toolkit_sentinel__";
const EMPTY_BOARD = '(kicad_pcb (version 20260206) (generator "pcbnew") (generator_version "10.0")\n)\n';

/**
 * Problems that make KiCad ignore the project's .kicad_dru ([] when KiCad loads it, or when the
 * project has no rules file). Runs a DRC on a temp copy whose board gets one extra track and
 * whose rules get a sentinel rule that flags that track; no sentinel violation = the file failed
 * to parse.
 */
export async function druProblems(project: Project, signal?: AbortSignal): Promise<string[]> {
	if (!fs.existsSync(project.druFile)) return [];
	const src = fs.readFileSync(project.druFile, "utf8");
	const dir = tmpDir("dru");
	try {
		const base = path.join(dir, project.name);
		fs.copyFileSync(project.proFile, `${base}.kicad_pro`);
		const pcb = path.join(project.dir, `${project.name}.kicad_pcb`);
		const board = fs.existsSync(pcb) ? fs.readFileSync(pcb, "utf8") : EMPTY_BOARD;
		const close = board.lastIndexOf(")");
		const track = `\t(segment (start 0 0) (end 1 0) (width 0.1) (layer "F.Cu") (uuid "${crypto.randomUUID()}"))\n`;
		fs.writeFileSync(`${base}.kicad_pcb`, `${board.slice(0, close)}${track}${board.slice(close)}`);
		const sentinel = `(rule "${SENTINEL}"\n\t(condition "A.Type == 'Track'")\n\t(constraint track_width (min 1000mm)))\n`;
		fs.writeFileSync(`${base}.kicad_dru`, `${src}\n${sentinel}`);
		let violations: unknown[];
		try {
			({ violations } = await pcbDrc(`${base}.kicad_pcb`, { cwd: dir, repoRoot: project.repoRoot, signal }));
		} catch (e) {
			return [`${project.rel(project.druFile)}: could not verify the rules, ${(e as Error).message}`];
		}
		const fired = violations.some(
			v => typeof v === "object" && v !== null && "description" in v && typeof v.description === "string" && v.description.includes(SENTINEL),
		);
		if (fired) return [];
		const located = locateProblems(src);
		return [
			`${project.rel(project.druFile)}: KiCad ignores this rules file (a sentinel rule appended to it never fired, so the file doesn't parse)` +
				(located.length ? "" : "; the cause was not located, check each rule against KiCad's custom-rules syntax"),
			...located,
		];
	} finally {
		fs.rmSync(dir, { recursive: true, force: true });
	}
}

/** Net class names a rule condition compares against (`NetClass == '…'`, `hasNetclass('…')`). */
export function conditionNetclasses(condition: string): string[] {
	const out = new Set<string>();
	for (const m of condition.matchAll(/NetClass\s*[!=]=\s*'([^']*)'|hasNetclass\(\s*'([^']*)'\s*\)/g)) out.add((m[1] ?? m[2])!);
	return [...out];
}

// ---- net classes --------------------------------------------------------------------------------

/** A `net_settings.classes` entry: name plus KiCad's per-class settings (mm; wire/bus widths in mils). */
export interface NetclassJson {
	name: string;
	[setting: string]: unknown;
}

export interface PatternJson {
	netclass: string;
	pattern: string;
}

export interface NetSettings {
	classes: NetclassJson[];
	netclass_patterns: PatternJson[];
	netclass_assignments?: Record<string, unknown>;
}

/** `net_settings` of the .kicad_pro (KiCad always writes it; its entries are KiCad's own output). */
export function netSettings(project: Project): NetSettings {
	const ns = project.proJson.data.net_settings;
	if (
		typeof ns !== "object" ||
		ns === null ||
		!("classes" in ns) ||
		!Array.isArray(ns.classes) ||
		!("netclass_patterns" in ns) ||
		!Array.isArray(ns.netclass_patterns)
	) {
		throw new Error(`${project.rel(project.proFile)} has no net_settings.classes / netclass_patterns: open and save the project in KiCad 10 once`);
	}
	return ns as NetSettings;
}

/** A net class's copper settings, mm. */
export interface ClassRules {
	name: string;
	clearance: number;
	track_width: number;
	via_diameter: number;
	via_drill: number;
	microvia_diameter: number;
	microvia_drill: number;
	diff_pair_width: number;
	diff_pair_gap: number;
}

/** KiCad's Default class values, for a .kicad_pro that leaves one out. */
export const KICAD_CLASS_DEFAULTS: Omit<ClassRules, "name"> = {
	clearance: 0.2,
	track_width: 0.2,
	via_diameter: 0.6,
	via_drill: 0.3,
	microvia_diameter: 0.3,
	microvia_drill: 0.1,
	diff_pair_width: 0.2,
	diff_pair_gap: 0.25,
};

/**
 * The settings of a net class by the name KiCad's netlist gives it (several comma-separated: the
 * first the .kicad_pro has; none of them: Default). KiCad writes a class key only when the class
 * sets it, so each one falls back to Default's value, then KiCad's own default.
 */
export function classRules(ns: NetSettings, name: string): ClassRules {
	const def = ns.classes.find(c => c.name === "Default");
	const cls =
		name
			.split(",")
			.map(s => ns.classes.find(c => c.name === s.trim()))
			.find(c => c) ?? def;
	const out = { name: cls?.name ?? "Default" } as ClassRules;
	for (const k of Object.keys(KICAD_CLASS_DEFAULTS) as (keyof typeof KICAD_CLASS_DEFAULTS)[]) {
		const own = cls?.[k];
		const d = def?.[k];
		out[k] = typeof own === "number" ? own : typeof d === "number" ? d : KICAD_CLASS_DEFAULTS[k];
	}
	return out;
}

/** KiCad's net class pattern match: an anchored regular expression or an anchored wildcard (`*`, `?`). */
export function patternMatcher(pattern: string): (net: string) => boolean {
	const wild = new RegExp(`^${pattern.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".")}$`);
	let re: RegExp | undefined;
	try {
		re = new RegExp(`^(?:${pattern})$`);
	} catch {
		re = undefined;
	}
	return net => wild.test(net) || !!re?.test(net);
}

/** Suffix pairs that mark the two halves of a differential pair. */
const PAIR_SUFFIXES: [string, string][] = [
	["_DP", "_DN"],
	["_P", "_N"],
	["+", "-"],
];

const netclasses: CheckDef = {
	name: "netclasses",
	doc: "Nets per net class (info), diff pairs (_P/_N, +/-, _DP/_DN, D+/D-) split across classes or left in Default (warning), .kicad_pro netclass patterns that match no net (warning), classes without nets (info).",
	byDefault: true,
	async run(ctx) {
		const nl = await ctx.analysis.netlist();
		const nets = nl.nets.filter(n => !n.name.startsWith("unconnected-("));
		const classOf = new Map(nets.map(n => [n.name, n.netclass ?? "Default"]));
		const out: Finding[] = [];
		const counts = new Map<string, number>();
		for (const c of classOf.values()) counts.set(c, (counts.get(c) ?? 0) + 1);
		out.push({
			check: "netclasses",
			severity: "info",
			message: `nets per class: ${[...counts].map(([c, n]) => `${c} ${n}`).join(", ")}`,
			key: "netclasses|summary",
		});
		for (const [name, cls] of classOf) {
			for (const [p, n] of PAIR_SUFFIXES) {
				const tail = name.slice(-p.length);
				if (tail.toUpperCase() !== p) continue;
				const partner = name.slice(0, -p.length) + (tail === p ? n : n.toLowerCase());
				const other = classOf.get(partner);
				if (other === undefined) continue;
				if (other !== cls) {
					out.push({
						check: "netclasses",
						severity: "warning",
						message: `diff pair ${name} / ${partner} is split across net classes ${cls} and ${other}`,
						key: `netclasses|pair_split|${name}`,
						net: name,
					});
				} else if (cls === "Default") {
					out.push({
						check: "netclasses",
						severity: "warning",
						message: `diff pair ${name} / ${partner} is in the Default net class (give it a class with its pair geometry)`,
						key: `netclasses|pair_default|${name}`,
						net: name,
					});
				}
				break;
			}
		}
		const ns = netSettings(ctx.project);
		const names = [...classOf.keys()];
		for (const p of ns.netclass_patterns) {
			const m = patternMatcher(p.pattern);
			if (!names.some(m)) {
				out.push({
					check: "netclasses",
					severity: "warning",
					message: `net class pattern "${p.pattern}" (class ${p.netclass}) matches no net`,
					key: `netclasses|pattern_unused|${p.netclass}|${p.pattern}`,
				});
			}
		}
		for (const c of ns.classes) {
			if (c.name !== "Default" && !counts.has(c.name)) {
				out.push({
					check: "netclasses",
					severity: "info",
					message: `net class ${c.name} has no nets`,
					key: `netclasses|class_empty|${c.name}`,
				});
			}
		}
		return out;
	},
};

const rules: CheckDef = {
	name: "rules",
	doc: "The .kicad_dru loads in KiCad (proved with a sentinel rule in a DRC run; parse errors located by line), and rule conditions only name net classes that exist.",
	byDefault: true,
	async run(ctx) {
		const project = ctx.project;
		if (!fs.existsSync(project.druFile)) return [];
		const out: Finding[] = [];
		const problems = await druProblems(project, ctx.signal);
		if (problems.length) out.push({ check: "rules", severity: "error", message: problems.join("\n"), key: "rules|parse" });
		const classes = new Set(netSettings(project).classes.map(c => c.name));
		for (const it of scanDru(fs.readFileSync(project.druFile, "utf8")).items) {
			if (head(it.node) !== "rule") continue;
			const rule = text(it.node, 1) ?? "";
			const cond = child(it.node, "condition");
			for (const cls of conditionNetclasses(text(cond, 1) ?? "")) {
				if (classes.has(cls)) continue;
				out.push({
					check: "rules",
					severity: "warning",
					message: `rule "${rule}" refers to net class "${cls}", which doesn't exist (classes: ${[...classes].join(", ")})`,
					key: `rules|netclass|${rule}|${cls}`,
				});
			}
		}
		return out;
	},
};

export const checks: CheckDef[] = [netclasses, rules];
