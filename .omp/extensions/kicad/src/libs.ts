/**
 * Symbol and footprint libraries: the project's sym-lib-table / fp-lib-table first, then the
 * user's global tables (KiCad 10 nests the stock libraries as a `Table` entry). `${VAR}` paths
 * expand from the environment, the kicad-cli wrapper's defaults (KICAD10_SYMBOL_DIR, …) and
 * KIPRJMOD. Every entry says where it comes from: the project, KiCad's stock libraries, or the
 * user's own global libraries.
 */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { devEnv } from "./cli";
import { decodeGraphics, type FpGraphics } from "./fpgfx";
import type { Pt } from "./geom";
import { child, children, clone, fmt, head, type List, num, parse, str, text } from "./sexpr";
import { type Pin, readPin, symbolItems } from "./symgfx";

/**
 * Where a library comes from: the project's lib table, KiCad's stock libraries, or any other
 * global entry (the user's own libraries: not in the repo and not KLC-reviewed).
 */
export type LibSource = "project" | "stock" | "user";

export interface LibEntry {
	name: string;
	type: string;
	uri: string;
	descr: string;
	/** Table file this entry came from. */
	table: string;
	source: LibSource;
}

/** Stock library directory variables (KICAD10_SYMBOL_DIR, KICAD_FOOTPRINT_DIR, …). */
const STOCK_VAR = /^KICAD\d*_(?:SYMBOL|FOOTPRINT)_DIR$/;
/** Unexpanded stock variables, and the nix store paths of the stock symbol / footprint packages. */
const STOCK_URI = /\$\{KICAD\d*_(?:SYMBOL|FOOTPRINT)_DIR\}|\/nix\/store\/[^/]+-kicad-(?:symbols|footprints)-[^/]*\//;

/**
 * The source of a global table entry (`raw` uri as written, `uri` expanded): stock when it lies in
 * KiCad's stock library directories, else the user's.
 */
function globalSource(raw: string, uri: string, vars: Record<string, string>): LibSource {
	if (STOCK_URI.test(raw) || STOCK_URI.test(uri)) return "stock";
	if (uri.includes("${")) return "user";
	for (const [k, v] of Object.entries(vars)) {
		if (!STOCK_VAR.test(k) || !v) continue;
		const rel = path.relative(path.resolve(v), uri);
		if (!rel.startsWith("..") && !path.isAbsolute(rel)) return "stock";
	}
	return "user";
}

let wrapperVars: Promise<Record<string, string>> | undefined;

/** Defaults the nix kicad-cli wrapper exports (`export KICAD10_X=${KICAD10_X-'…'}`). */
async function kicadWrapperVars(repoRoot: string): Promise<Record<string, string>> {
	const env = await devEnv(repoRoot);
	const out: Record<string, string> = {};
	const exe = Bun.which("kicad-cli", { PATH: env.PATH ?? "" });
	if (exe) {
		const real = fs.realpathSync(exe);
		const src = fs.readFileSync(real, "latin1");
		if (src.startsWith("#!")) {
			for (const m of src.matchAll(/export (KICAD\d*_[A-Z0-9_]+)=\$\{\1-'([^']*)'\}/g)) out[m[1]!] = m[2]!;
		}
	}
	for (const [k, v] of Object.entries(env)) if (k.startsWith("KICAD")) out[k] = v;
	return out;
}

export async function kicadVars(repoRoot: string): Promise<Record<string, string>> {
	wrapperVars ??= kicadWrapperVars(repoRoot);
	return wrapperVars;
}

/** Expand `${VAR}`s; a fully expanded path is normalised (".../espion-cam/../lib" → ".../lib"). */
function expand(uri: string, vars: Record<string, string>): string {
	const out = uri.replace(/\$\{([A-Za-z0-9_]+)\}/g, (m, k: string) => vars[k] ?? m);
	return out.includes("${") ? out : path.resolve(out);
}

function globalTable(kind: "sym" | "fp"): string {
	const cfg = process.env.XDG_CONFIG_HOME || path.join(os.homedir(), ".config");
	return path.join(cfg, "kicad", "10.0", `${kind}-lib-table`);
}

/** Entries of a lib table, nested `Table` entries expanded; `project` tags them all as the project's. */
function readTable(file: string, vars: Record<string, string>, seen: Set<string>, project: boolean): LibEntry[] {
	if (seen.has(file) || !fs.existsSync(file)) return [];
	seen.add(file);
	const out: LibEntry[] = [];
	for (const lib of children(parse(fs.readFileSync(file, "utf8")), "lib")) {
		const raw = text(child(lib, "uri")) ?? "";
		const uri = expand(raw, vars);
		const e: LibEntry = {
			name: text(child(lib, "name")) ?? "",
			type: text(child(lib, "type")) ?? "",
			uri,
			descr: text(child(lib, "descr")) ?? "",
			table: file,
			source: project ? "project" : globalSource(raw, uri, vars),
		};
		if (child(lib, "disabled")) continue;
		if (e.type === "Table") out.push(...readTable(e.uri, vars, seen, project));
		else out.push(e);
	}
	return out;
}

export class Libraries {
	private symCache = new Map<string, { mtime: number; tree: List }>();

	private constructor(
		readonly projectDir: string,
		readonly sym: LibEntry[],
		readonly fp: LibEntry[],
	) {}

	static async load(projectDir: string, repoRoot: string): Promise<Libraries> {
		const vars = { ...(await kicadVars(repoRoot)), KIPRJMOD: projectDir };
		const tables = (kind: "sym" | "fp") => {
			const seen = new Set<string>();
			const project = readTable(path.join(projectDir, `${kind}-lib-table`), vars, seen, true);
			const names = new Set(project.map(e => e.name));
			// project entries shadow global ones with the same nickname, as in KiCad
			return [...project, ...readTable(globalTable(kind), vars, seen, false).filter(e => !names.has(e.name))];
		};
		return new Libraries(projectDir, tables("sym"), tables("fp"));
	}

	symLib(nick: string): LibEntry {
		const e = this.sym.find(x => x.name === nick);
		if (!e) throw new Error(`symbol library "${nick}" is not in the project or global sym-lib-table`);
		return e;
	}

	fpLib(nick: string): LibEntry {
		const e = this.fp.find(x => x.name === nick);
		if (!e) throw new Error(`footprint library "${nick}" is not in the project or global fp-lib-table`);
		return e;
	}

	symLibTree(nick: string): List {
		const file = this.symLib(nick).uri;
		const mtime = fs.statSync(file).mtimeMs;
		const hit = this.symCache.get(file);
		if (hit && hit.mtime === mtime) return hit.tree;
		const tree = parse(fs.readFileSync(file, "utf8"));
		this.symCache.set(file, { mtime, tree });
		return tree;
	}

	symbolNames(nick: string): string[] {
		return children(this.symLibTree(nick), "symbol").map(s => text(s) ?? "");
	}

	/**
	 * A library symbol flattened for a schematic's lib_symbols cache: `extends` resolved against
	 * its parent, the top-level name set to the full lib_id, unit names renamed to match.
	 */
	flatSymbol(libId: string): List {
		const [nick, name] = splitId(libId);
		const s = flattenSymbol(this.symLibTree(nick), name, nick);
		s[1] = str(libId);
		return s;
	}

	footprintFile(fpId: string): string {
		const [nick, name] = splitId(fpId);
		const f = path.join(this.fpLib(nick).uri, `${name}.kicad_mod`);
		if (!fs.existsSync(f)) throw new Error(`footprint ${fpId} not found (${f})`);
		return f;
	}

	footprintNames(nick: string): string[] {
		const dir = this.fpLib(nick).uri;
		return fs
			.readdirSync(dir)
			.filter(f => f.endsWith(".kicad_mod"))
			.map(f => f.slice(0, -".kicad_mod".length))
			.sort();
	}

	/** Pads of a footprint: number, type, shape. Unnumbered pads (mechanical) are included with num "". */
	footprintPads(fpId: string): { num: string; type: string; shape: string; at: [number, number]; size: [number, number] }[] {
		const tree = parse(fs.readFileSync(this.footprintFile(fpId), "utf8"));
		return children(tree, "pad").map(p => {
			const a = child(p, "at");
			const s = child(p, "size");
			return {
				num: text(p, 1) ?? "",
				type: text(p, 2) ?? "",
				shape: text(p, 3) ?? "",
				at: [num(a, 1), num(a, 2)],
				size: [num(s, 1), num(s, 2)],
			};
		});
	}
}

export function splitId(id: string): [string, string] {
	const i = id.indexOf(":");
	if (i <= 0) throw new Error(`expected "Library:Name", got "${id}"`);
	return [id.slice(0, i), id.slice(i + 1)];
}

/**
 * A copy of symbol `name` of a library tree with `extends` resolved against its parent (header
 * flags and unit sub-symbols from the parent, renamed `<name>_<unit>_<body>`).
 */
export function flattenSymbol(lib: List, name: string, nick: string): List {
	const find = (n: string) => {
		const s = children(lib, "symbol").find(x => text(x) === n);
		if (!s) throw new Error(`symbol "${n}" not found in library ${nick}`);
		return s;
	};
	const flat = (n: string): List => {
		const s = clone(find(n));
		const ext = child(s, "extends");
		if (!ext) return s;
		const parent = flat(text(ext)!);
		const parentName = text(parent)!;
		const out: List = [s[0]!, str(n)];
		// root-symbol header in KiCad's order (derived symbols don't write these)
		for (const f of [
			"power",
			"body_styles",
			"pin_numbers",
			"pin_names",
			"exclude_from_sim",
			"in_bom",
			"on_board",
			"in_pos_files",
			"duplicate_pin_numbers_are_jumpers",
			"jumper_pin_groups",
		]) {
			const c = child(s, f) ?? child(parent, f);
			if (c) out.push(clone(c));
		}
		out.push(...children(s, "property"));
		for (const u of children(parent, "symbol")) {
			const cu = clone(u);
			cu[1] = str(n + (text(u) ?? "").slice(parentName.length));
			out.push(cu);
		}
		const ef = child(s, "embedded_fonts") ?? child(parent, "embedded_fonts");
		if (ef) out.push(clone(ef));
		return out;
	};
	return flat(name);
}

/** A library pin as lib_symbol reports it without graphics. */
export type LibPinInfo = Pick<Pin, "number" | "name" | "type" | "unit" | "hidden" | "at" | "dir">;

/** Library symbol summary for queries. */
export function describeLibSymbol(sym: List): { properties: Record<string, string>; units: number; power: boolean; pins: LibPinInfo[] } {
	const properties: Record<string, string> = {};
	for (const p of children(sym, "property")) properties[text(p) ?? ""] = text(p, 2) ?? "";
	const units = Math.max(1, ...children(sym, "symbol").map(u => Number(/_(\d+)_\d+$/.exec(text(u) ?? "")?.[1] ?? 0)));
	const pins = symbolItems(sym)
		.items.filter(it => head(it.node) === "pin")
		.map(it => {
			const { number, name, type, unit, hidden, at, dir } = readPin(it.node, it.unit, it.body);
			return { number, name, type, unit, hidden, at, dir };
		});
	return { properties, units, power: !!child(sym, "power"), pins };
}

/**
 * Pins stacked the legacy way: hidden pins at the point of a visible pin drawn with it (same unit
 * and body style, or common to all units / both body styles), and no other visible pin there.
 */
export interface LegacyStack {
	unit: number;
	body_style: number;
	at: Pt;
	visible: string;
	hidden: string[];
	/** The same stack as one KiCad 10 pin, e.g. "[2,7]". */
	native: string;
}

export const LEGACY_STACK_NOTE =
	'legacy_stacks: hidden duplicate pins stacked on a visible one fail KLC S4.3. When adapting this drawing (lib_draw_symbol / lib_edit_symbol), draw each stack as one pin numbered as its `native` (KiCad 10 stacked pin, e.g. "[2,7]") and drop the hidden duplicates.';

export function legacyStacks(sym: List): LegacyStack[] {
	const byPoint = new Map<string, Pin[]>();
	for (const it of symbolItems(sym).items) {
		if (head(it.node) !== "pin") continue;
		const p = readPin(it.node, it.unit, it.body);
		const k = `${fmt(p.at[0])}|${fmt(p.at[1])}`;
		byPoint.set(k, [...(byPoint.get(k) ?? []), p]);
	}
	// 0 = common to all units / both body styles
	const together = (p: Pin, q: Pin) => (!p.unit || !q.unit || p.unit === q.unit) && (!p.body_style || !q.body_style || p.body_style === q.body_style);
	const out: LegacyStack[] = [];
	for (const pins of byPoint.values()) {
		for (const v of pins.filter(p => !p.hidden)) {
			const stack = pins.filter(p => p !== v && together(p, v));
			if (!stack.length || stack.some(p => !p.hidden)) continue;
			const numbers = [v, ...stack].map(p => p.number).sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
			out.push({ unit: v.unit, body_style: v.body_style, at: v.at, visible: v.number, hidden: stack.map(p => p.number), native: `[${numbers.join(",")}]` });
		}
	}
	return out;
}

function pair(l: List | undefined): [number, number] | undefined {
	return l ? [num(l, 1), num(l, 2)] : undefined;
}

/** The atoms after a list's head, as text (layer names, attributes, drill sizes). */
function atoms(l: List | undefined): string[] {
	const out: string[] = [];
	for (let i = 1; l && i < l.length; i++) {
		const t = text(l, i);
		if (t !== undefined) out.push(t);
	}
	return out;
}

/** `(key (xyz x y z))` of a model, or `dflt` when KiCad left it out. */
function xyz(model: List, key: string, dflt: [number, number, number]): [number, number, number] {
	const v = child(child(model, key) ?? [], "xyz");
	return v ? [num(v, 1), num(v, 2), num(v, 3)] : dflt;
}

/** Optional pad settings lib_footprint reports when the footprint sets them (KiCad's tokens). */
export const PAD_DETAILS = [
	"roundrect_rratio",
	"corner_radius",
	"chamfer_ratio",
	"chamfer",
	"solder_mask_margin",
	"solder_paste_margin",
	"solder_paste_margin_ratio",
	"clearance",
	"zone_connect",
	"thermal_bridge_width",
	"thermal_gap",
	"thermal_bridge_angle",
] as const;

export interface FpPadInfo {
	number: string;
	type: string;
	shape: string;
	/** [x, y] or [x, y, angle] mm. */
	at: number[] | undefined;
	size: [number, number] | undefined;
	/** Diameter, or [w, h] of an oval hole. */
	drill?: number | number[];
	layers: string[];
	roundrect_rratio?: number;
	/** Corner radius in mm: roundrect_rratio × the smaller pad side, as KiCad draws it. */
	corner_radius?: number;
	chamfer_ratio?: number;
	/** Chamfered corners: top_left, top_right, bottom_left, bottom_right. */
	chamfer?: string[];
	/** Pad overrides of the footprint / board settings, mm (ratio: fraction of the pad size). */
	solder_mask_margin?: number;
	solder_paste_margin?: number;
	solder_paste_margin_ratio?: number;
	clearance?: number;
	/** 0 none, 1 thermal relief, 2 solid, 3 thermal relief for through-hole pads only. */
	zone_connect?: number;
	thermal_bridge_width?: number;
	thermal_gap?: number;
	thermal_bridge_angle?: number;
}

/** Footprint-level overrides of the board settings (KiCad's footprint properties, Clearance Overrides). */
export interface FpOverrides {
	solder_mask_margin?: number;
	solder_paste_margin?: number;
	solder_paste_margin_ratio?: number;
	clearance?: number;
	zone_connect?: number;
	thermal_bridge_width?: number;
	thermal_gap?: number;
}

/** `(key value)` children present in `l` as numbers, the first of each key's (current, legacy) tokens. */
function numbers(l: List, keys: Record<string, readonly string[]>): Record<string, number> {
	const out: Record<string, number> = {};
	for (const [k, tokens] of Object.entries(keys)) {
		const c = tokens.map(t => child(l, t)).find(x => x);
		if (c) out[k] = num(c);
	}
	return out;
}

const PAD_NUMBERS: Record<string, readonly string[]> = {
	solder_mask_margin: ["solder_mask_margin"],
	solder_paste_margin: ["solder_paste_margin"],
	solder_paste_margin_ratio: ["solder_paste_margin_ratio"],
	clearance: ["clearance"],
	zone_connect: ["zone_connect"],
	thermal_bridge_width: ["thermal_bridge_width", "thermal_width"],
	thermal_gap: ["thermal_gap"],
	thermal_bridge_angle: ["thermal_bridge_angle"],
};

const FP_NUMBERS: Record<string, readonly string[]> = {
	solder_mask_margin: ["solder_mask_margin"],
	solder_paste_margin: ["solder_paste_margin"],
	// pads write solder_paste_margin_ratio, footprints solder_paste_ratio
	solder_paste_margin_ratio: ["solder_paste_ratio", "solder_paste_margin_ratio"],
	clearance: ["clearance"],
	zone_connect: ["zone_connect"],
	thermal_bridge_width: ["thermal_width", "thermal_bridge_width"],
	thermal_gap: ["thermal_gap"],
};

/** Roundrect and chamfer settings of a pad, with the corner radius KiCad draws. */
function padCorners(p: List, size: [number, number] | undefined): Partial<FpPadInfo> {
	const out: Partial<FpPadInfo> = {};
	const rr = child(p, "roundrect_rratio");
	if (rr && text(p, 3) === "roundrect") {
		out.roundrect_rratio = num(rr);
		if (size) out.corner_radius = Math.round(num(rr) * Math.min(...size) * 1e6) / 1e6;
	}
	const cr = child(p, "chamfer_ratio");
	const corners = atoms(child(p, "chamfer"));
	if (cr && corners.length) {
		out.chamfer_ratio = num(cr);
		out.chamfer = corners;
	}
	return out;
}

/** A footprint's 3D model as KiCad stores it (and its 3D model properties dialog shows it). */
export interface FpModelInfo {
	path: string;
	/** mm. */
	offset: [number, number, number];
	/** Degrees about x, y, z. */
	rotation: [number, number, number];
	scale: [number, number, number];
}

export interface FpInfo {
	description: string;
	tags: string;
	properties: Record<string, string>;
	attributes: string[];
	/** Footprint-level solder mask / paste / clearance overrides, when the footprint sets any. */
	overrides?: FpOverrides;
	pads: FpPadInfo[];
	models: FpModelInfo[];
	/** Drawings by layer (fpgfx.ts format), when asked for. */
	graphics?: FpGraphics;
}

/** Library footprint summary for queries. */
export function describeFootprint(fp: List, graphics = false): FpInfo {
	const properties: Record<string, string> = {};
	for (const p of children(fp, "property")) properties[text(p) ?? ""] = text(p, 2) ?? "";
	const pads = children(fp, "pad").map((p): FpPadInfo => {
		const at = child(p, "at");
		const size = pair(child(p, "size"));
		const dv = atoms(child(p, "drill")).filter(x => x !== "oval" && !Number.isNaN(Number(x))).map(Number);
		return {
			number: text(p, 1) ?? "",
			type: text(p, 2) ?? "",
			shape: text(p, 3) ?? "",
			at: at && at.length > 3 ? [num(at, 1), num(at, 2), num(at, 3)] : pair(at),
			size,
			...(dv.length ? { drill: dv.length === 1 ? dv[0] : dv } : {}),
			layers: atoms(child(p, "layers")),
			...padCorners(p, size),
			...numbers(p, PAD_NUMBERS),
		};
	});
	const overrides = numbers(fp, FP_NUMBERS);
	return {
		description: text(child(fp, "descr")) ?? "",
		tags: text(child(fp, "tags")) ?? "",
		properties,
		attributes: atoms(child(fp, "attr")),
		...(Object.keys(overrides).length ? { overrides } : {}),
		pads,
		models: children(fp, "model").map(m => ({
			path: text(m) ?? "",
			offset: xyz(m, "offset", [0, 0, 0]),
			rotation: xyz(m, "rotate", [0, 0, 0]),
			scale: xyz(m, "scale", [1, 1, 1]),
		})),
		...(graphics ? { graphics: decodeGraphics(fp) } : {}),
	};
}
