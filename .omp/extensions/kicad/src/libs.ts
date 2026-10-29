/**
 * Symbol and footprint libraries: the project's sym-lib-table / fp-lib-table first, then the
 * user's global tables (KiCad 10 nests the stock libraries as a `Table` entry). `${VAR}` paths
 * expand from the environment, the kicad-cli wrapper's defaults (KICAD10_SYMBOL_DIR, …) and
 * KIPRJMOD.
 */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { devEnv } from "./cli";
import { decodeGraphics, type FpGraphics } from "./fpgfx";
import { child, children, clone, head, type List, num, parse, str, text } from "./sexpr";
import { type Pin, readPin, symbolItems } from "./symgfx";

export interface LibEntry {
	name: string;
	type: string;
	uri: string;
	descr: string;
	/** Table file this entry came from. */
	table: string;
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

function readTable(file: string, vars: Record<string, string>, seen: Set<string>): LibEntry[] {
	if (seen.has(file) || !fs.existsSync(file)) return [];
	seen.add(file);
	const out: LibEntry[] = [];
	for (const lib of children(parse(fs.readFileSync(file, "utf8")), "lib")) {
		const e: LibEntry = {
			name: text(child(lib, "name")) ?? "",
			type: text(child(lib, "type")) ?? "",
			uri: expand(text(child(lib, "uri")) ?? "", vars),
			descr: text(child(lib, "descr")) ?? "",
			table: file,
		};
		if (child(lib, "disabled")) continue;
		if (e.type === "Table") out.push(...readTable(e.uri, vars, seen));
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
			const project = readTable(path.join(projectDir, `${kind}-lib-table`), vars, seen);
			const names = new Set(project.map(e => e.name));
			// project entries shadow global ones with the same nickname, as in KiCad
			return [...project, ...readTable(globalTable(kind), vars, seen).filter(e => !names.has(e.name))];
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
	pads: FpPadInfo[];
	models: FpModelInfo[];
	/** Drawings by layer (fpgfx.ts format), when asked for. */
	graphics?: FpGraphics;
}

/** Library footprint summary for queries. */
export function describeFootprint(fp: List, graphics = false): FpInfo {
	const properties: Record<string, string> = {};
	for (const p of children(fp, "property")) properties[text(p) ?? ""] = text(p, 2) ?? "";
	const pads = children(fp, "pad").map(p => {
		const at = child(p, "at");
		const dv = atoms(child(p, "drill")).filter(x => x !== "oval" && !Number.isNaN(Number(x))).map(Number);
		return {
			number: text(p, 1) ?? "",
			type: text(p, 2) ?? "",
			shape: text(p, 3) ?? "",
			at: at && at.length > 3 ? [num(at, 1), num(at, 2), num(at, 3)] : pair(at),
			size: pair(child(p, "size")),
			...(dv.length ? { drill: dv.length === 1 ? dv[0] : dv } : {}),
			layers: atoms(child(p, "layers")),
		};
	});
	return {
		description: text(child(fp, "descr")) ?? "",
		tags: text(child(fp, "tags")) ?? "",
		properties,
		attributes: atoms(child(fp, "attr")),
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
