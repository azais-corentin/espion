/**
 * Library lookups across the project and global lib tables: search parts by name, description and
 * keywords; describe one symbol (pins, units) or footprint (pads, attributes, 3D models).
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { describeFootprint, describeLibSymbol, type LibEntry, splitId } from "../libs";
import { child, children, head, isList, parse, text } from "../sexpr";
import { symbolGraphics, symbolItems } from "../symgfx";
import type { QueryDef } from "../types";

interface Part {
	kind: "symbol" | "footprint";
	id: string;
	description: string;
	keywords: string;
}

/** Index rows of one file, cached by modification time (the stock libraries are large). */
const fileCache = new Map<string, { mtime: number; rows: { name: string; description: string; keywords: string }[] }>();

function cached(file: string, build: () => { name: string; description: string; keywords: string }[]) {
	const mtime = fs.statSync(file).mtimeMs;
	const hit = fileCache.get(file);
	if (hit && hit.mtime === mtime) return hit.rows;
	const rows = build();
	fileCache.set(file, { mtime, rows });
	return rows;
}

const QSTR = '"((?:[^"\\\\]|\\\\.)*)"';

function unescape(s: string): string {
	return s.replace(/\\(.)/g, (_, c: string) => (c === "n" ? "\n" : c));
}

/** Top-level symbols of a .kicad_sym file with their Description and ki_keywords (KiCad writes one per tab-indented line). */
function symbolRows(file: string) {
	return cached(file, () => {
		const src = fs.readFileSync(file, "utf8");
		const starts = [...src.matchAll(new RegExp(`^\\t\\(symbol ${QSTR}`, "gm"))];
		return starts.map((m, i) => {
			const seg = src.slice(m.index, starts[i + 1]?.index ?? src.length);
			const field = (name: string) => unescape(new RegExp(`\\(property "${name}" ${QSTR}`).exec(seg)?.[1] ?? "");
			return { name: unescape(m[1]!), description: field("Description"), keywords: field("ki_keywords") };
		});
	});
}

/** descr and tags sit at the top of a .kicad_mod; read the head only unless they run past it. */
function footprintRow(file: string) {
	return cached(file, () => {
		const fd = fs.openSync(file, "r");
		const buf = new Uint8Array(8192);
		const n = fs.readSync(fd, buf, 0, buf.length, 0);
		fs.closeSync(fd);
		let head = new TextDecoder().decode(buf.subarray(0, n));
		if (n === buf.length && !head.includes("(property")) head = fs.readFileSync(file, "utf8");
		const field = (name: string) => unescape(new RegExp(`\\(${name} ${QSTR}`).exec(head)?.[1] ?? "");
		return [{ name: path.basename(file, ".kicad_mod"), description: field("descr"), keywords: field("tags") }];
	})[0]!;
}

function symbolFiles(e: LibEntry): string[] {
	if (!fs.existsSync(e.uri)) return [];
	if (!fs.statSync(e.uri).isDirectory()) return [e.uri];
	return fs
		.readdirSync(e.uri)
		.filter(f => f.endsWith(".kicad_sym"))
		.map(f => path.join(e.uri, f));
}

/** Case-insensitive shell pattern (`*`, `?`) on the whole nickname. */
function libMatcher(pattern: unknown): (s: string) => boolean {
	if (typeof pattern !== "string") return () => true;
	const re = new RegExp(`^${pattern.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".")}$`, "i");
	return s => re.test(s);
}

const libSearch: QueryDef = {
	what: "lib_search",
	doc: "Find symbols and footprints in the project and global libraries whose name, description or keywords contain every word of the query (case-insensitive); name matches first.",
	fields: {
		query: { t: "string", req: true, doc: 'words, e.g. "QFN-32 4x4" or "ethernet phy"' },
		kind: { t: "string", values: ["symbol", "footprint"], doc: "only symbols or only footprints" },
		library: { t: "string", doc: 'library nickname or pattern, e.g. "Package_DFN_QFN" or "Connector*"' },
		limit: { t: "integer", doc: "maximum results (default 30)" },
	},
	async run(ctx, a) {
		const libs = await ctx.libs();
		const words = (a.query as string).toLowerCase().split(/\s+/).filter(Boolean);
		if (!words.length) throw new Error("query: give at least one word");
		const lib = libMatcher(a.library);
		const parts: Part[] = [];
		if (a.kind !== "footprint") {
			for (const e of libs.sym.filter(x => lib(x.name))) {
				for (const f of symbolFiles(e)) for (const r of symbolRows(f)) parts.push({ kind: "symbol", id: `${e.name}:${r.name}`, description: r.description, keywords: r.keywords });
			}
		}
		if (a.kind !== "symbol") {
			for (const e of libs.fp.filter(x => lib(x.name) && fs.existsSync(x.uri))) {
				for (const f of fs.readdirSync(e.uri).filter(x => x.endsWith(".kicad_mod"))) {
					const r = footprintRow(path.join(e.uri, f));
					parts.push({ kind: "footprint", id: `${e.name}:${r.name}`, description: r.description, keywords: r.keywords });
				}
			}
		}
		const q = words.join(" ");
		const rank = (p: Part): number => {
			const name = p.id.slice(p.id.indexOf(":") + 1).toLowerCase();
			if (name === q) return 0;
			if (words.every(w => name.includes(w))) return 1;
			const all = `${p.id} ${p.description} ${p.keywords}`.toLowerCase();
			return words.every(w => all.includes(w)) ? 2 : -1;
		};
		const hits = parts
			.map(p => ({ p, r: rank(p) }))
			.filter(x => x.r >= 0)
			.sort((x, y) => x.r - y.r || x.p.id.localeCompare(y.p.id))
			.map(x => x.p);
		const limit = typeof a.limit === "number" && a.limit > 0 ? a.limit : 30;
		return { total: hits.length, ...(hits.length > limit ? { shown: limit } : {}), parts: hits.slice(0, limit) };
	},
};

const libSymbol: QueryDef = {
	what: "lib_symbol",
	doc: "A library symbol (derived symbols resolved): properties, unit count, power flag and pins with number, name, electrical type, unit (0 = common to all units), hidden flag, `at` (pin end relative to the symbol origin, sheet orientation, rot 0) and `dir` (L|R|U|D: side the wire leaves at rot 0). Use `at` to plan wires before add_symbol: rotate it with the symbol. With graphics: true also the drawing in lib_draw_symbol's input format (shapes, pin length/shape/body_style, pin_name_offset, hide_pin_names, hide_pin_numbers), so the result can be redrawn or adapted; text boxes have no shape form and are left out.",
	fields: {
		lib_id: { t: "string", req: true, doc: 'e.g. "MCU_Module_Waveshare:ESP32-P4-Core"' },
		graphics: { t: "boolean", doc: "add shapes and full pin geometry (default false)" },
	},
	async run(ctx, a) {
		const libs = await ctx.libs();
		const id = a.lib_id as string;
		const sym = libs.flatSymbol(id);
		return { lib_id: id, ...describeLibSymbol(sym), ...(a.graphics === true ? symbolGraphics(sym) : {}) };
	},
};

const libFootprint: QueryDef = {
	what: "lib_footprint",
	doc: "A library footprint: file, description, tags, properties, attributes (smd, through_hole, …), pads (number, type, shape, at [x, y, angle?], size, drill, layers) and 3D models (path, offset [x, y, z] mm, rotation [x, y, z] degrees, scale [x, y, z]: as KiCad's 3D model properties show them and lib_create_footprint / lib_edit_footprint set them); with graphics, its drawings per layer.",
	fields: {
		fp_id: { t: "string", req: true, doc: 'e.g. "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm"' },
		graphics: {
			t: "boolean",
			doc: "add `graphics`: {layer: [shape, …]} with every line, rect, circle, arc, poly and user text, in exactly the format lib_create_footprint / lib_edit_footprint take as `graphics` (mm, y down; width always given, fill/rot/justify when set)",
		},
	},
	async run(ctx, a) {
		const libs = await ctx.libs();
		const file = libs.footprintFile(a.fp_id as string);
		return {
			fp_id: a.fp_id,
			file: path.relative(ctx.project.repoRoot, file).startsWith("..") ? file : path.relative(ctx.project.repoRoot, file),
			...describeFootprint(parse(fs.readFileSync(file, "utf8")), a.graphics === true),
		};
	},
};

/** KiCad's EDA_PATTERN_MATCH_WILDCARD_ANCHORED on the lower-cased pattern: `*`, `?`, the rest literal. */
function wildcard(pattern: string): RegExp {
	const body = [...pattern.toLowerCase()].map(c => (c === "*" ? ".*" : c === "?" ? "." : c.replace(/[.+^${}()|[\]\\/]/, "\\$&"))).join("");
	return new RegExp(`^${body}$`);
}

/** KiCad's ExpandStackedPinNotation: "[1-3,5]" → 1, 2, 3, 5; "[A1-A3]" → A1, A2, A3; others as they are. */
function stackedNumbers(n: string): string[] {
	if (!n.startsWith("[") || !n.endsWith("]")) return [n];
	const out: string[] = [];
	for (const part of n.slice(1, -1).split(",").map(p => p.trim()).filter(Boolean)) {
		const dash = part.indexOf("-");
		if (dash < 0) {
			out.push(part);
			continue;
		}
		const [a, b] = [part.slice(0, dash).trim(), part.slice(dash + 1).trim()].map(t => /^(.*?)(\d+)$/.exec(t));
		if (!a || !b || a[1] !== b[1] || Number(a[2]) > Number(b[2])) return [n];
		for (let i = Number(a[2]); i <= Number(b[2]); i++) out.push(`${a[1]}${i}`);
	}
	return out.length ? out : [n];
}

/** Pad numbers KiCad counts for the pin-count filter: numbered pads on copper, NPTH excluded (FOOTPRINT::GetUniquePadCount). */
function padNumbers(file: string): Set<string> {
	const out = new Set<string>();
	for (const p of children(parse(fs.readFileSync(file, "utf8")), "pad")) {
		const layers = child(p, "layers")?.slice(1).map(l => (isList(l) ? "" : l.v)) ?? [];
		const number = text(p, 1) ?? "";
		if (number && text(p, 2) !== "np_thru_hole" && layers.some(l => l.endsWith(".Cu"))) out.add(number);
	}
	return out;
}

const libFpFilter: QueryDef = {
	what: "lib_fp_filter",
	doc: 'The footprints a symbol\'s footprint filters (ki_fp_filters) accept, as KiCad\'s footprint chooser matches them: case-insensitive `*`/`?` wildcards on the whole footprint name, or on "Lib:Name" when the filter contains ":"; no filters accept every footprint. Each match has its pad count as KiCad counts it (distinct numbers of copper pads, NPTH excluded) to compare with the symbol\'s pin count (distinct pin numbers of all units, body style 1, stacked "[1-3]" expanded); KiCad 10\'s chooser offers that comparison as an extra filter ("Filter by pin count", off by default). Matches with the right pad count come first. Also checks the default footprint.',
	fields: {
		lib_id: { t: "string", req: true, doc: 'symbol "Lib:Name", stock or project' },
		limit: { t: "integer", doc: "maximum footprints listed (default 50)" },
	},
	async run(ctx, a) {
		const libs = await ctx.libs();
		const id = a.lib_id as string;
		const sym = libs.flatSymbol(id);
		const props = describeLibSymbol(sym).properties;
		const filters = (props.ki_fp_filters ?? "").split(/\s+/).filter(Boolean);
		const pins = new Set(
			symbolItems(sym)
				.items.filter(it => head(it.node) === "pin" && it.body <= 1)
				.flatMap(it => stackedNumbers(text(child(it.node, "number")) ?? "")),
		);
		const res = filters.map(f => ({ re: wildcard(f), withLib: f.includes(":") }));
		const accepts = (nick: string, name: string) => !res.length || res.some(r => r.re.test(`${r.withLib ? `${nick.toLowerCase()}:` : ""}${name.toLowerCase()}`));
		const matches: { id: string; pads: number }[] = [];
		for (const e of libs.fp) {
			if (!fs.existsSync(e.uri)) continue;
			for (const name of libs.footprintNames(e.name)) {
				if (accepts(e.name, name)) matches.push({ id: `${e.name}:${name}`, pads: padNumbers(path.join(e.uri, `${name}.kicad_mod`)).size });
			}
		}
		matches.sort((x, y) => Number(y.pads === pins.size) - Number(x.pads === pins.size) || x.id.localeCompare(y.id));
		const limit = typeof a.limit === "number" && a.limit > 0 ? a.limit : 50;
		let dflt: Record<string, unknown> | null = null;
		if (props.Footprint) {
			const [nick, name] = splitId(props.Footprint);
			const lib = libs.fp.find(e => e.name === nick);
			const file = lib ? path.join(lib.uri, `${name}.kicad_mod`) : "";
			const pads = file && fs.existsSync(file) ? padNumbers(file).size : undefined;
			dflt = { footprint: props.Footprint, exists: pads !== undefined, accepted: accepts(nick, name), ...(pads !== undefined ? { pads, pad_count_matches: pads === pins.size } : {}) };
		}
		return {
			lib_id: id,
			filters,
			pin_count: pins.size,
			default_footprint: dflt,
			total: matches.length,
			with_pin_count: matches.filter(m => m.pads === pins.size).length,
			...(matches.length > limit ? { shown: limit } : {}),
			footprints: matches.slice(0, limit),
		};
	},
};

export const queries: QueryDef[] = [libSearch, libSymbol, libFootprint, libFpFilter];
