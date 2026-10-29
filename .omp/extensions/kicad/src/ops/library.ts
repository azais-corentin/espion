/**
 * Project library edits: symbols from kicad-library-utils' CSV symbol generator, footprints from
 * the official footprint generators (kicad-library-tools `src/generators`) or, for parts no
 * generator family covers, a KLC-style footprint drawn here. New libraries go into
 * `settings.libDir` and the project's sym-lib-table / fp-lib-table with `${KIPRJMOD}` paths.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import * as cli from "../cli";
import { applyGraphics, encodeShape, GRAPHICS_DOC, graphicsIn, mm, replaceDrawings, xy } from "../fpgfx";
import type { Box, Pt } from "../geom";
import { flattenSymbol, type LibSource, splitId } from "../libs";
import type { Project } from "../project";
import { prop, propValue } from "../sch";
import { child, children, clone, head, inline, isList, type Item, L, type List, num, parse, print, removeChild, setChild, str, Sym, text } from "../sexpr";
import { type Field, type Fields, validate } from "../spec";
import {
	drawExtent,
	editPinNode,
	itemsExtent,
	PIN_NAME_OFFSET,
	PIN_SHAPES,
	PIN_TYPES,
	pinEditsIn,
	pinNode,
	pinsIn,
	setSubSymbols,
	shapeNode,
	shapesIn,
	type Item as SymItem,
	subSymbols,
	symbolItems,
} from "../symgfx";
import type { OpContext, OpDef } from "../types";

type Kind = "sym" | "fp";

const TABLE: Record<Kind, { head: string; file: string; ext: string; what: string }> = {
	sym: { head: "sym_lib_table", file: "sym-lib-table", ext: ".kicad_sym", what: "symbol" },
	fp: { head: "fp_lib_table", file: "fp-lib-table", ext: ".pretty", what: "footprint" },
};

/** Library edits of one transaction, so later ops see what earlier ones did. */
interface LibState {
	tables: Partial<Record<Kind, List>>;
	/** "sym:Nick" / "fp:Nick" → library path, for libraries added in this transaction. */
	added: Map<string, string>;
	symLibs: Map<string, List>;
	/** .kicad_mod path → content (null: removed). */
	footprints: Map<string, List | null>;
}

const states = new WeakMap<Project, LibState>();

function stateOf(project: Project): LibState {
	let s = states.get(project);
	if (!s) {
		s = { tables: {}, added: new Map(), symLibs: new Map(), footprints: new Map() };
		states.set(project, s);
	}
	return s;
}

const NICK = /^[A-Za-z0-9][A-Za-z0-9_.+-]*$/;

/** The `library_description` argument of every op that can create a library. */
const LIBRARY_DESCRIPTION: Field = {
	t: "string",
	doc: 'what the library holds, e.g. "TI power multiplexers"; required when `library` doesn\'t exist yet (the lib table entry gets it plus the project suffix, libraryDescriptionSuffix in .omp/kicad.json); ignored for an existing library (lib_set_library changes it)',
};

/** A lib table description: `description` with the configured project suffix appended (once). */
function libDescription(project: Project, description: string): string {
	const d = description.trim();
	const suffix = project.settings.libraryDescriptionSuffix.trim();
	return !suffix || d.endsWith(suffix) ? d : `${d} ${suffix}`;
}

/** How a refusal names where a library comes from. */
const ORIGIN: Record<LibSource, string> = { project: "a project", stock: "a stock KiCad", user: "the user's own global (not a project)" };

/**
 * Path of a project library by nickname. With `create`, a library missing from the project table
 * is added (file under settings.libDir, `${KIPRJMOD}`-relative uri, `description` plus the project
 * suffix as its table description).
 */
function projectLib(ctx: OpContext, kind: Kind, nick: string, create: boolean, description?: unknown): string {
	const st = stateOf(ctx.project);
	const t = TABLE[kind];
	const added = st.added.get(`${kind}:${nick}`);
	if (added) return added;
	const tableFile = path.join(ctx.project.dir, t.file);
	const entries = kind === "sym" ? ctx.libs.sym : ctx.libs.fp;
	const own = entries.find(e => e.name === nick && e.table === tableFile);
	if (own) {
		if (typeof description === "string") ctx.notes.push(`${t.what} library ${nick} exists: library_description ignored (lib_set_library changes it)`);
		return path.resolve(own.uri);
	}
	const ownNames = entries.filter(e => e.table === tableFile).map(e => e.name);
	if (!create) throw new Error(`"${nick}" is not a ${t.what} library of the project; project ${t.what} libraries: ${ownNames.join(", ") || "none"}`);
	const global = entries.find(e => e.name === nick);
	if (global) {
		throw new Error(`"${nick}" is ${ORIGIN[global.source]} ${t.what} library; project libraries need their own KLC name (e.g. "${nick}_<Vendor>")`);
	}
	if (!NICK.test(nick)) throw new Error(`library name "${nick}" must be letters, digits and _ . + - (KLC S1.1 / F1.1), e.g. "Connector_HanRun"`);
	if (typeof description !== "string" || !description.trim()) {
		throw new Error(
			`${t.what} library "${nick}" doesn't exist yet: give library_description, what it holds (e.g. "TI power multiplexers"); its ${t.file} entry becomes "${libDescription(ctx.project, "<library_description>")}"`,
		);
	}
	const file = path.join(ctx.project.repoRoot, ctx.project.settings.libDir, `${nick}${t.ext}`);
	const uri = `\${KIPRJMOD}/${path.relative(ctx.project.dir, file).split(path.sep).join("/")}`;
	const descr = libDescription(ctx.project, description);
	libTable(ctx.project, kind).push(
		L("lib", L("name", str(nick)), L("type", str("KiCad")), L("uri", str(uri)), L("options", str("")), L("descr", str(descr))),
	);
	saveTable(ctx.project, kind);
	st.added.set(`${kind}:${nick}`, file);
	ctx.notes.push(`new ${t.what} library ${nick} (${ctx.project.rel(file)}), registered in ${t.file} as ${uri}, description "${descr}"`);
	return file;
}

function libTable(project: Project, kind: Kind): List {
	const st = stateOf(project);
	const file = path.join(project.dir, TABLE[kind].file);
	st.tables[kind] ??= fs.existsSync(file) ? parse(fs.readFileSync(file, "utf8")) : L(TABLE[kind].head, L("version", 7));
	return st.tables[kind];
}

/** Write the project's lib table as KiCad does: one line per library. */
function saveTable(project: Project, kind: Kind): void {
	const t = TABLE[kind];
	const table = libTable(project, kind);
	project.setFile(path.join(project.dir, t.file), `(${t.head}\n${table.slice(1).map(c => `\t${inline(c)}`).join("\n")}\n)\n`);
}

const setLibrary: OpDef = {
	op: "lib_set_library",
	group: "library",
	doc: "Set the description of a project library in the project's sym-lib-table or fp-lib-table (the project suffix, libraryDescriptionSuffix in .omp/kicad.json, is appended). Stock and the user's global libraries are refused.",
	fields: {
		kind: { t: "string", req: true, values: ["symbol", "footprint"], doc: "symbol or footprint library" },
		library: { t: "string", req: true, doc: "project library nickname" },
		description: { t: "string", req: true, doc: 'what the library holds, e.g. "TI power multiplexers"' },
	},
	apply(ctx, a) {
		const kind: Kind = a.kind === "symbol" ? "sym" : "fp";
		const t = TABLE[kind];
		const library = a.library as string;
		const description = (a.description as string).trim();
		if (!description) throw new Error("description must not be empty");
		const global = (kind === "sym" ? ctx.libs.sym : ctx.libs.fp).find(e => e.name === library && e.source !== "project");
		if (global) throw new Error(`"${library}" is ${ORIGIN[global.source]} ${t.what} library; only project libraries can be described`);
		projectLib(ctx, kind, library, false);
		const entry = children(libTable(ctx.project, kind), "lib").find(l => text(child(l, "name")) === library);
		if (!entry) throw new Error(`"${library}" is not in the project's ${t.file}`);
		const descr = libDescription(ctx.project, description);
		const old = text(child(entry, "descr")) ?? "";
		if (old === descr) throw new Error(`${t.what} library ${library} already has the description "${descr}"`);
		setChild(entry, L("descr", str(descr)));
		saveTable(ctx.project, kind);
		ctx.notes.push(`${t.what} library ${library}: description "${old}" -> "${descr}" (${t.file})`);
	},
};

function symLibTree(project: Project, file: string): List {
	const st = stateOf(project);
	let tree = st.symLibs.get(file);
	if (!tree) {
		tree = fs.existsSync(file)
			? parse(fs.readFileSync(file, "utf8"))
			: L("kicad_symbol_lib", L("version", 20251024), L("generator", str("kicad_symbol_editor")), L("generator_version", str("10.0")));
		st.symLibs.set(file, tree);
	}
	return tree;
}

/** Write a symbol library with its symbols in KiCad's save order: by inheritance depth, then name. */
function saveSymLib(project: Project, file: string, tree: List): void {
	const syms = children(tree, "symbol");
	const byName = new Map(syms.map(s => [text(s) ?? "", s]));
	const depth = (s: List): number => {
		const parent = byName.get(text(child(s, "extends")) ?? "");
		return parent && parent !== s ? 1 + depth(parent) : 0;
	};
	const sorted = syms
		.map(s => ({ s, d: depth(s), n: text(s) ?? "" }))
		.sort((a, b) => a.d - b.d || (a.n < b.n ? -1 : a.n > b.n ? 1 : 0))
		.map(x => x.s);
	const at = tree.indexOf(syms[0] ?? []);
	if (at >= 0) tree.splice(at, syms.length, ...sorted);
	project.setFile(file, `${print(tree)}\n`);
}

function findSymbol(tree: List, name: string, library: string): List {
	const s = children(tree, "symbol").find(x => text(x) === name);
	if (!s) {
		const names = children(tree, "symbol").map(x => text(x) ?? "");
		throw new Error(`no symbol "${name}" in ${library}; symbols: ${names.join(", ") || "none"}`);
	}
	return s;
}

function footprintTree(project: Project, file: string): List | undefined {
	const st = stateOf(project);
	if (st.footprints.has(file)) return st.footprints.get(file) ?? undefined;
	return fs.existsSync(file) ? parse(fs.readFileSync(file, "utf8")) : undefined;
}

function saveFootprint(project: Project, file: string, tree: List): void {
	stateOf(project).footprints.set(file, tree);
	project.setFile(file, `${print(tree)}\n`);
}

/** Whether "Lib:Name" resolves to a footprint (stock, project, or added in this transaction). */
function footprintExists(ctx: OpContext, id: string): boolean {
	const [nick, name] = splitId(id);
	const dir = stateOf(ctx.project).added.get(`fp:${nick}`) ?? ctx.libs.fp.find(e => e.name === nick)?.uri;
	return !!dir && !!footprintTree(ctx.project, path.join(dir, `${name}.kicad_mod`));
}

function checkFootprintRef(ctx: OpContext, id: string): void {
	if (id && !footprintExists(ctx, id)) throw new Error(`footprint "${id}" not found (expected "Library:Name" of an existing footprint)`);
}

/** References of placed symbols matching `pred`, across all sheet instances. */
function usedBy(project: Project, pred: (sym: List, libId: string) => boolean): string[] {
	const refs = new Set<string>();
	for (const inst of project.hierarchy()) {
		for (const sym of inst.sch.symbols()) if (pred(sym, inst.sch.libId(sym))) refs.add(inst.sch.refOf(sym, inst.path));
	}
	return [...refs].sort();
}

function strField(o: Record<string, unknown>, k: string, where: string): string | undefined {
	const v = o[k];
	if (v === undefined || v === null) return undefined;
	if (typeof v === "number") return String(v);
	if (typeof v !== "string") throw new Error(`${where}: "${k}" must be a string, got ${JSON.stringify(v)}`);
	return v;
}

function oneOf(v: string | undefined, values: readonly string[], what: string): string | undefined {
	if (v !== undefined && !values.includes(v)) throw new Error(`${what} "${v}" is not one of ${values.join(", ")}`);
	return v;
}

// ---- symbols -------------------------------------------------------------------------------------

const SIDES = ["left", "right", "top", "bottom"] as const;

function csvCell(v: string): string {
	return /[",\n\r]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v;
}

interface PinRow {
	number: string;
	name: string;
	type: string;
	side: string;
	unit: string;
}

function pinRows(raw: Record<string, unknown>[]): PinRow[] {
	const seen = new Set<string>();
	const rows = raw.map((p, i) => {
		const where = `pins[${i}]`;
		for (const k of Object.keys(p)) if (!["number", "name", "type", "side", "unit"].includes(k)) throw new Error(`${where}: unknown key "${k}"`);
		const type = oneOf(strField(p, "type", where), [...PIN_TYPES, "space"], `${where} type`);
		const side = oneOf(strField(p, "side", where), SIDES, `${where} side`);
		if (!type || !side) throw new Error(`${where}: "type" and "side" are required`);
		const number = strField(p, "number", where) ?? "";
		const name = strField(p, "name", where) ?? "";
		if (type !== "space") {
			if (!number || !name) throw new Error(`${where}: "number" and "name" are required (use "~" for an unnamed pin)`);
			if (seen.has(number)) throw new Error(`${where}: pin number ${number} is used twice`);
			seen.add(number);
		}
		return { number, name, type, side, unit: strField(p, "unit", where) ?? "" };
	});
	const withUnit = rows.filter(r => r.unit).length;
	if (withUnit && withUnit !== rows.length) throw new Error("pins: give every pin a unit, or none (single-unit symbol)");
	return rows;
}

const createSymbol: OpDef = {
	op: "lib_create_symbol",
	group: "library",
	doc: "Generate a KLC-style symbol with kicad-library-utils' CSV symbol generator (pins grouped on the sides you give, body sized to the pin names) and add it to a project symbol library (created and registered in sym-lib-table when missing). Put positive supplies on top, grounds at the bottom, inputs left, outputs right (KLC S4.2).",
	fields: {
		library: { t: "string", req: true, doc: 'project symbol library nickname, e.g. "Interface_Ethernet_ICPlus"; created in libDir when missing' },
		library_description: LIBRARY_DESCRIPTION,
		name: { t: "string", req: true, doc: "symbol name (the part number, KLC S2.1); must not exist in the library" },
		reference: { t: "string", req: true, doc: 'reference prefix, e.g. "U"' },
		pins: {
			t: "objects",
			req: true,
			doc: `[{number, name, type, side, unit?}]: type one of ${PIN_TYPES.join(", ")} (or "space" for a gap, without number/name); side left|right|top|bottom; name may list alternate functions after ";" ("GPIO5;SDA"), "~{RST}" overbars; no_connect pins are hidden inside the body; unit = unit name, for every pin or none (units are ordered by name)`,
		},
		value: { t: "string", doc: "Value field (default: the symbol name)" },
		footprint: { t: "string", doc: 'default footprint "Library:Name" (must exist)' },
		datasheet: { t: "string", doc: "datasheet URL" },
		description: { t: "string", doc: "Description field" },
		keywords: { t: "string", doc: "space-separated search keywords" },
		fp_filters: { t: "strings", doc: 'footprint filters, e.g. ["SOIC*3.9x4.9mm*P1.27mm*"]; KLC (S5.1/S5.2) needs one matching the default footprint' },
	},
	async apply(ctx, a) {
		const library = a.library as string;
		const name = a.name as string;
		const pins = pinRows(a.pins as Record<string, unknown>[]);
		if (typeof a.footprint === "string") checkFootprintRef(ctx, a.footprint);
		const file = projectLib(ctx, "sym", library, true, a.library_description);
		const tree = symLibTree(ctx.project, file);
		checkNewSymbol(tree, name, library);
		const meta: [string, unknown][] = [
			["name", name],
			["reference", a.reference],
			["footprint", a.footprint],
			["footprint_filter", (a.fp_filters as string[] | undefined)?.join(" ")],
			["datasheet", a.datasheet],
			["description", a.description],
			["keywords", a.keywords],
		];
		const unit = pins.some(p => p.unit);
		const csv = [
			...meta.filter(([, v]) => typeof v === "string" && v).map(([k, v]) => `${k},${csvCell(v as string)}`),
			",",
			unit ? "Pin,Name,Type,Side,Unit" : "Pin,Name,Type,Side",
			...pins.map(p => [p.number, p.name, p.type, p.side, ...(unit ? [p.unit] : [])].map(csvCell).join(",")),
		].join("\n");

		const tools = await cli.libraryTools(ctx.project.repoRoot);
		const dir = cli.tmpDir("symgen");
		let generated: List;
		try {
			fs.writeFileSync(path.join(dir, "part.csv"), `${csv}\n`);
			const out = path.join(dir, `${library}.kicad_sym`);
			const r = await cli.run(["python3", path.join(tools, "library_utils", "symbol-generators", "from_csv_generator.py"), "part.csv", "-o", out], {
				cwd: dir,
				repoRoot: ctx.project.repoRoot,
			});
			if (r.code !== 0 || !fs.existsSync(out)) throw new Error(`symbol generator failed: ${(r.stderr || r.stdout).trim().slice(-2000)}`);
			for (const w of r.stderr.split("\n").filter(l => l.startsWith("WARNING: "))) ctx.notes.push(`${name}: ${w.slice(9)}`);
			const s = child(parse(fs.readFileSync(out, "utf8")), "symbol");
			if (!s) throw new Error(`symbol generator wrote no symbol: ${r.stdout.trim()}`);
			generated = s;
		} finally {
			fs.rmSync(dir, { recursive: true, force: true });
		}
		if (typeof a.value === "string") prop(generated, "Value")![2] = str(a.value);
		// an empty filter is a KLC S5.2 violation; no filter is not
		if (propValue(generated, "ki_fp_filters") === "") generated.splice(generated.indexOf(prop(generated, "ki_fp_filters")!), 1);
		tree.push(generated);
		saveSymLib(ctx.project, file, tree);
		ctx.touchPart("symbol", `${library}:${name}`);
		const units = new Set(pins.map(p => p.unit)).size;
		ctx.notes.push(`added symbol ${library}:${name} (${pins.filter(p => p.type !== "space").length} pins${units > 1 ? `, ${units} units` : ""})`);
	},
};

function checkNewSymbol(tree: List, name: string, library: string): void {
	if (children(tree, "symbol").some(s => text(s) === name)) {
		throw new Error(`symbol "${name}" already exists in ${library}; change it with lib_edit_symbol or remove it first`);
	}
}

const SHAPES_DOC =
	'[shape]: {type, unit?, body_style?, …} in mm, sheet orientation (x right, y down) relative to the symbol origin; type polyline {points: [[x,y],…]} | rectangle {start, end} | circle {center, radius} | arc {start, mid, end} | bezier {points: 4 points} | text {text, at, size? (1.27), rot? 0|90, justify? left|center|right}; non-text shapes take width? (stroke mm, 0 = KiCad default; KLC body outline 0.254) and fill? none|outline|background (KLC body: background); unit 0 = every unit (default), body_style 1 (default), 2 = De Morgan, 0 = both. lib_symbol with graphics: true returns this format';

/** Pin stacks, for the pins docs of lib_draw_symbol and lib_edit_symbol. */
const PIN_STACK_DOC =
	'several pads on one pin (e.g. GND on 1 and 8) are one pin with a KiCad 10 stacked number, "[1,8]" or "[1-3,5]"; hidden duplicate pins at one point (legacy stacks, as in stock Power_Management:TPS2116DRL; lib_symbol lists them in legacy_stacks) fail KLC S4.3';

/** A property as KiCad 10 writes it: visible ones (Reference, Value) left-justified at `at` (sheet orientation), the rest hidden at the origin. */
function propertyNode(key: string, value: string, at?: Pt): List {
	return L(
		"property",
		str(key),
		str(value),
		L("at", at?.[0] ?? 0, at ? 0 - at[1] : 0, 0),
		L("show_name", false),
		L("do_not_autoplace", false),
		...(at ? [] : [L("hide", true)]),
		L("effects", L("font", L("size", 1.27, 1.27)), ...(at ? [L("justify", "left")] : [])),
	);
}

/** Reference 1.27 mm above and Value 1.27 mm below a drawing extent, both at its left edge. */
function symbolFieldPlaces([x0, y0, , y1]: Box): { Reference: Pt; Value: Pt } {
	return { Reference: [x0, y0 - 1.27], Value: [x0, y1 + 1.27] };
}

const drawSymbol: OpDef = {
	op: "lib_draw_symbol",
	group: "library",
	doc: "Draw a symbol from explicit pins and shapes (transformer coils as arcs, capacitor plates, LED triangles, text, a connector outline …) and add it to a project symbol library (created and registered when missing). Reference goes above and Value below the drawing, left-aligned; the other fields are hidden. Start from a similar stock symbol: kicad_query lib_symbol {lib_id, graphics: true} returns its pins and shapes in this format, and kicad_render {symbol} shows it. KLC: pin ends on the 2.54 mm grid, pin length 2.54 (default), body rectangle width 0.254 with fill background.",
	fields: {
		library: { t: "string", req: true, doc: "project symbol library nickname (created when missing)" },
		library_description: LIBRARY_DESCRIPTION,
		name: { t: "string", req: true, doc: "symbol name (the part number, KLC S2.1); must not exist in the library" },
		reference: { t: "string", req: true, doc: 'reference prefix, e.g. "J"' },
		value: { t: "string", doc: "Value field (default: the symbol name)" },
		footprint: { t: "string", doc: 'default footprint "Library:Name" (must exist)' },
		datasheet: { t: "string", doc: "datasheet URL" },
		description: { t: "string", doc: "Description field" },
		keywords: { t: "string", doc: "space-separated search keywords" },
		fp_filters: { t: "strings", doc: 'footprint filters, e.g. ["RJ45*HanRun*HR871150C*"]; KLC (S5.1/S5.2) needs one matching the default footprint' },
		pins: {
			t: "objects",
			req: true,
			doc: `[{number, name, type, at, dir, length?, shape?, hidden?, unit?, body_style?}]: at = connection end [x, y] mm (sheet orientation), dir L|R|U|D = side the wire leaves, name "" for none ("~{RST}" overbars), type ${PIN_TYPES.join("|")}, length default 2.54, shape ${PIN_SHAPES.join("|")} (default line), unit default 1 (0 = every unit), body_style default 1; ${PIN_STACK_DOC}`,
		},
		shapes: { t: "objects", req: true, doc: SHAPES_DOC },
		pin_name_offset: { t: "number", doc: "pin name offset inside the body in mm (default 0.508; 0 puts names outside, above the pin)" },
		hide_pin_names: { t: "boolean", doc: "hide all pin names (default false)" },
		hide_pin_numbers: { t: "boolean", doc: "hide all pin numbers (default false)" },
	},
	apply(ctx, a) {
		const library = a.library as string;
		const name = a.name as string;
		const pins = pinsIn(a.pins as Record<string, unknown>[]);
		const shapes = shapesIn(a.shapes as Record<string, unknown>[]);
		if (!pins.length && !shapes.length) throw new Error("give pins and/or shapes");
		const offset = typeof a.pin_name_offset === "number" ? a.pin_name_offset : PIN_NAME_OFFSET;
		if (offset < 0) throw new Error("pin_name_offset must not be negative");
		if (typeof a.footprint === "string") checkFootprintRef(ctx, a.footprint);
		const file = projectLib(ctx, "sym", library, true, a.library_description);
		const tree = symLibTree(ctx.project, file);
		checkNewSymbol(tree, name, library);
		const items: SymItem[] = [
			...shapes.map(s => ({ unit: s.unit, body: s.body_style, node: shapeNode(s) })),
			...pins.map(p => ({ unit: p.unit, body: p.body_style, node: pinNode(p) })),
		];
		const places = symbolFieldPlaces(drawExtent(shapes, pins));
		const filters = (a.fp_filters as string[] | undefined) ?? [];
		const sym = L(
			"symbol",
			str(name),
			...(items.some(it => it.body === 2) ? [L("body_styles", "demorgan")] : []),
			...(a.hide_pin_numbers === true ? [L("pin_numbers", L("hide", true))] : []),
			...(offset !== PIN_NAME_OFFSET || a.hide_pin_names === true
				? [L("pin_names", ...(offset !== PIN_NAME_OFFSET ? [L("offset", offset)] : []), ...(a.hide_pin_names === true ? [L("hide", true)] : []))]
				: []),
			L("exclude_from_sim", false),
			L("in_bom", true),
			L("on_board", true),
			L("in_pos_files", true),
			L("duplicate_pin_numbers_are_jumpers", false),
			propertyNode("Reference", a.reference as string, places.Reference),
			propertyNode("Value", typeof a.value === "string" ? a.value : name, places.Value),
			propertyNode("Footprint", typeof a.footprint === "string" ? a.footprint : ""),
			propertyNode("Datasheet", typeof a.datasheet === "string" ? a.datasheet : ""),
			propertyNode("Description", typeof a.description === "string" ? a.description : ""),
			...(typeof a.keywords === "string" && a.keywords ? [propertyNode("ki_keywords", a.keywords)] : []),
			...(filters.length ? [propertyNode("ki_fp_filters", filters.join(" "))] : []),
			...subSymbols(name, items, new Map()),
			L("embedded_fonts", false),
		);
		tree.push(sym);
		saveSymLib(ctx.project, file, tree);
		ctx.touchPart("symbol", `${library}:${name}`);
		const units = new Set(items.map(it => it.unit).filter(u => u > 0)).size;
		ctx.notes.push(`added symbol ${library}:${name} (${pins.length} pins, ${shapes.length} shapes${units > 1 ? `, ${units} units` : ""})`);
	},
};

/** A library tree to copy from: a project library as edited in this transaction, else from disk. */
function sourceSymLib(ctx: OpContext, nick: string): List {
	const st = stateOf(ctx.project);
	const file = st.added.get(`sym:${nick}`) ?? path.resolve(ctx.libs.symLib(nick).uri);
	return st.symLibs.get(file) ?? ctx.libs.symLibTree(nick);
}

const copySymbol: OpDef = {
	op: "lib_copy_symbol",
	group: "library",
	doc: "Copy a stock or project symbol into a project symbol library under a new name (created and registered when missing), as a standalone symbol: a derived (extends) symbol is flattened, unit sub-symbols are renamed, Value is set to the new name. Then adapt it with lib_edit_symbol (pins, shapes, properties).",
	fields: {
		from: { t: "string", req: true, doc: 'source symbol "Library:Name", e.g. "Connector:RJ45_Abracon_ARJP11A-MASA-B-A-EMU2"' },
		library: { t: "string", req: true, doc: "project symbol library nickname (created when missing)" },
		library_description: LIBRARY_DESCRIPTION,
		name: { t: "string", req: true, doc: "new symbol name (the part number, KLC S2.1); must not exist in the library" },
		footprint: { t: "string", doc: 'default footprint "Library:Name" replacing the source\'s (must exist)' },
	},
	apply(ctx, a) {
		const from = a.from as string;
		const library = a.library as string;
		const name = a.name as string;
		const [nick, srcName] = splitId(from);
		const sym = flattenSymbol(sourceSymLib(ctx, nick), srcName, nick);
		if (typeof a.footprint === "string") checkFootprintRef(ctx, a.footprint);
		const file = projectLib(ctx, "sym", library, true, a.library_description);
		const tree = symLibTree(ctx.project, file);
		checkNewSymbol(tree, name, library);
		sym[1] = str(name);
		for (const sub of children(sym, "symbol")) sub[1] = str(name + (/_\d+_\d+$/.exec(text(sub) ?? "")?.[0] ?? ""));
		prop(sym, "Value")![2] = str(name);
		if (typeof a.footprint === "string") prop(sym, "Footprint")![2] = str(a.footprint);
		tree.push(sym);
		saveSymLib(ctx.project, file, tree);
		ctx.touchPart("symbol", `${library}:${name}`);
		const source = ctx.libs.sym.find(e => e.name === nick)?.source;
		ctx.notes.push(
			`copied ${from} to ${library}:${name}${typeof a.footprint === "string" ? ` (footprint ${a.footprint})` : ""}; update Description, Datasheet, keywords and footprint filters for the new part` +
				(source === "stock" ? `; derived from the KiCad symbol library, licensed CC-BY-SA 4.0 (with the KiCad libraries exception): keep that license and attribution if the library is shared` : "") +
				(source === "user" ? `; ${nick} is the user's own global library, neither in the repo nor KLC-reviewed: check the copy against the datasheet and KLC` : ""),
		);
	},
};

const MANDATORY = ["Reference", "Value", "Footprint", "Datasheet", "Description"];

const editSymbol: OpDef = {
	op: "lib_edit_symbol",
	group: "library",
	doc: "Change a project library symbol: set or remove properties; replace all graphics (shapes); add, remove, move, resize, restyle, hide, rename or retype pins (by number, every pin with that number in every unit). When shapes are replaced or pins are added, removed, moved, turned or resized, Reference and Value are re-placed above and below the new drawing as lib_draw_symbol places them (fields: \"keep\" leaves them). Placed symbols keep their cached copy until updated from the library.",
	fields: {
		library: { t: "string", req: true, doc: "project symbol library nickname" },
		name: { t: "string", req: true, doc: "symbol name" },
		properties: {
			t: "object",
			doc: '{Name: value | null}: set a property (new ones are added hidden) or remove it (null; not for Reference/Value/Footprint/Datasheet/Description). Keywords are "ki_keywords", footprint filters "ki_fp_filters"',
		},
		pins: {
			t: "objects",
			doc: `[{number, name?, type?, at?, dir?, length?, shape?, hidden?, unit?, remove?: true}]: only the given keys change (at, dir and shapes as in lib_draw_symbol); a number that doesn't exist yet with at and dir adds a pin (name default "", type passive, length 2.54, unit 1); remove: true deletes it; ${PIN_STACK_DOC}: to convert one, remove the hidden duplicates and renumber the visible pin (remove it and add the stacked number at its place)`,
		},
		shapes: { t: "objects", doc: `replaces all graphics of the symbol (every unit and body style; pins stay): ${SHAPES_DOC}` },
		fields: {
			t: "string",
			values: ["auto", "keep"],
			doc: 'Reference and Value placement: "auto" puts Reference 1.27 mm above and Value 1.27 mm below the drawing, left-aligned at its left edge (as lib_draw_symbol); "keep" leaves them. Default: auto when shapes are replaced or pins are added, removed, moved, turned or resized, else keep',
		},
	},
	apply(ctx, a) {
		const library = a.library as string;
		const name = a.name as string;
		const file = projectLib(ctx, "sym", library, false);
		const tree = symLibTree(ctx.project, file);
		const sym = findSymbol(tree, name, library);
		const changes: string[] = [];
		for (const [k, v] of Object.entries((a.properties as Record<string, unknown> | undefined) ?? {})) {
			const p = prop(sym, k);
			if (v === null) {
				if (MANDATORY.includes(k)) throw new Error(`property ${k} is mandatory; set it to "" instead of removing it`);
				if (!p) throw new Error(`symbol ${name} has no property "${k}"`);
				sym.splice(sym.indexOf(p), 1);
				changes.push(`removed ${k}`);
				continue;
			}
			if (typeof v !== "string") throw new Error(`properties.${k} must be a string or null, got ${JSON.stringify(v)}`);
			if (k === "Footprint") checkFootprintRef(ctx, v);
			if (p) p[2] = str(v);
			else {
				const props = children(sym, "property");
				const at = props.length ? sym.indexOf(props[props.length - 1]!) + 1 : 2;
				sym.splice(at, 0, L("property", str(k), str(v), L("at", 0, 0, 0), L("hide", true), L("effects", L("font", L("size", 1.27, 1.27)))));
			}
			changes.push(`${k} = "${v}"`);
		}
		const pinEdits = pinEditsIn((a.pins as Record<string, unknown>[] | undefined) ?? []);
		const shapes = a.shapes === undefined ? undefined : shapesIn(a.shapes as Record<string, unknown>[]);
		if ((pinEdits.length || shapes) && child(sym, "extends")) {
			throw new Error(`${name} extends ${text(child(sym, "extends"))}: its pins and graphics belong to that symbol, edit it instead`);
		}
		let moved = false;
		if (pinEdits.length || shapes) {
			const { items, unitNames } = symbolItems(sym);
			let list = shapes ? items.filter(it => head(it.node) === "pin") : items;
			if (shapes) {
				list.push(...shapes.map(s => ({ unit: s.unit, body: s.body_style, node: shapeNode(s) })));
				changes.push(`graphics replaced (${shapes.length} shapes)`);
			}
			pinEdits.forEach((e, i) => {
				const where = `pins[${i}]`;
				const hits = list.filter(it => head(it.node) === "pin" && text(child(it.node, "number")) === e.number);
				if (e.remove) {
					if (!hits.length) throw new Error(`${where}: ${name} has no pin ${e.number} to remove`);
					list = list.filter(it => !hits.includes(it));
					moved = true;
					changes.push(`removed pin ${e.number}`);
					return;
				}
				const keys = (["name", "type", "at", "dir", "length", "shape", "hidden", "unit"] as const).filter(k => e[k] !== undefined);
				if (!keys.length) throw new Error(`${where}: nothing to change for pin ${e.number}`);
				if (!hits.length) {
					if (!e.at || !e.dir) throw new Error(`${where}: ${name} has no pin ${e.number}; to add it give "at" and "dir"`);
					const unit = e.unit ?? 1;
					const p = { number: e.number, name: e.name ?? "", type: e.type ?? "passive", at: e.at, dir: e.dir, length: e.length ?? 2.54, shape: e.shape ?? "line", hidden: e.hidden ?? false, unit, body_style: 1 };
					list.push({ unit, body: 1, node: pinNode(p) });
					moved = true;
					changes.push(`added pin ${e.number}`);
					return;
				}
				if (e.at && new Set(hits.map(h => text(child(h.node, "at"), 1) + "," + text(child(h.node, "at"), 2))).size > 1) {
					throw new Error(`${where}: pin ${e.number} sits at ${hits.length} places; "at" would stack them (remove and re-add instead)`);
				}
				for (const h of hits) {
					editPinNode(h.node, e);
					if (e.unit !== undefined) h.unit = e.unit;
				}
				if (e.at || e.dir || e.length !== undefined) moved = true;
				changes.push(`pin ${e.number} ${keys.join("/")}`);
			});
			if (list.some(it => it.body === 2) && !child(sym, "body_styles")) {
				const power = child(sym, "power");
				sym.splice(power ? sym.indexOf(power) + 1 : 2, 0, L("body_styles", "demorgan"));
			}
			setSubSymbols(sym, subSymbols(name, list, unitNames));
		}
		if ((a.fields ?? (shapes || moved ? "auto" : "keep")) === "auto") {
			if (child(sym, "extends")) throw new Error(`${name} extends ${text(child(sym, "extends"))}: its drawing belongs to that symbol; use fields: "auto" there`);
			const items = symbolItems(sym).items;
			if (!items.length) throw new Error(`${name} has no drawing to place Reference and Value around`);
			const places = symbolFieldPlaces(itemsExtent(items));
			for (const key of ["Reference", "Value"] as const) {
				const p = prop(sym, key);
				if (!p) continue;
				const [x, y] = places[key];
				setChild(p, L("at", x, 0 - y, 0));
				let fx = child(p, "effects");
				if (!fx) {
					fx = L("effects", L("font", L("size", 1.27, 1.27)));
					p.push(fx);
				}
				setChild(fx, L("justify", "left"));
			}
			changes.push(`Reference and Value re-placed above and below the drawing`);
		}
		if (!changes.length) throw new Error("nothing to change: give properties, pins, shapes and/or fields");
		saveSymLib(ctx.project, file, tree);
		ctx.touchPart("symbol", `${library}:${name}`);
		ctx.notes.push(`${library}:${name}: ${changes.join(", ")}`);
	},
};

const removeSymbol: OpDef = {
	op: "lib_remove_symbol",
	group: "library",
	doc: "Remove a symbol from a project symbol library; refused while placed in the project or extended by another symbol.",
	fields: {
		library: { t: "string", req: true, doc: "project symbol library nickname" },
		name: { t: "string", req: true, doc: "symbol name" },
	},
	apply(ctx, a) {
		const library = a.library as string;
		const name = a.name as string;
		const file = projectLib(ctx, "sym", library, false);
		const tree = symLibTree(ctx.project, file);
		const sym = findSymbol(tree, name, library);
		const ext = children(tree, "symbol").filter(s => text(child(s, "extends")) === name);
		if (ext.length) throw new Error(`${name} is extended by ${ext.map(s => text(s)).join(", ")}; remove those first`);
		const refs = usedBy(ctx.project, (_, id) => id === `${library}:${name}`);
		if (refs.length) throw new Error(`${library}:${name} is placed as ${refs.join(", ")}; replace or remove those first`);
		tree.splice(tree.indexOf(sym), 1);
		saveSymLib(ctx.project, file, tree);
		ctx.notes.push(`removed symbol ${library}:${name}`);
	},
};

// ---- footprints ----------------------------------------------------------------------------------

/** 3D model path of a project footprint, by the project convention. */
function projectModel(project: Project, prettyDir: string, nick: string, name: string): string {
	const rel = path.relative(project.dir, path.join(path.dirname(prettyDir), `${nick}.3dshapes`, `${name}.step`));
	return `\${KIPRJMOD}/${rel.split(path.sep).join("/")}`;
}

function modelNode(file: string): List {
	return L("model", str(file), L("offset", L("xyz", 0, 0, 0)), L("scale", L("xyz", 1, 1, 1)), L("rotate", L("xyz", 0, 0, 0)));
}

/** Placement fields of the footprint's 3D model → the model's s-expression key. */
const MODEL_PLACEMENT: Record<string, string> = { model_offset: "offset", model_rotation: "rotate", model_scale: "scale" };

const MODEL_PLACEMENT_FIELDS: Fields = {
	model_offset: { t: "any", doc: "[x, y, z] mm: 3D model offset (z up from the board surface), as in KiCad's 3D model properties" },
	model_rotation: { t: "any", doc: "[x, y, z] degrees about each axis, as in KiCad's 3D model properties" },
	model_scale: { t: "any", doc: "[x, y, z] scale factors (KiCad default [1, 1, 1])" },
};

/** Set model_offset / model_rotation / model_scale on the footprint's only 3D model; returns what changed. */
function placeModel(fp: List, a: Record<string, unknown>): string[] {
	const given = Object.entries(MODEL_PLACEMENT).filter(([k]) => a[k] !== undefined);
	if (!given.length) return [];
	const models = children(fp, "model");
	if (models.length !== 1) {
		throw new Error(
			models.length
				? `the footprint has ${models.length} 3D models (${models.map(m => text(m)).join(", ")}); model_offset/model_rotation/model_scale need exactly one`
				: "the footprint has no 3D model to place (lib_create_footprint: model: true; lib_edit_footprint: model)",
		);
	}
	return given.map(([k, key]) => {
		const v = a[k];
		if (!Array.isArray(v) || v.length !== 3 || !v.every((x): x is number => typeof x === "number" && Number.isFinite(x))) {
			throw new Error(`${k} must be [x, y, z], got ${JSON.stringify(v)}`);
		}
		if (key === "scale" && v.includes(0)) throw new Error(`model_scale must not be 0 on any axis, got ${JSON.stringify(v)}`);
		setChild(models[0]!, L(key, L("xyz", ...(key === "offset" ? v.map(mm) : v))));
		return `${k} [${v.join(", ")}]`;
	});
}

const FP_NAME = /^[A-Za-z0-9][A-Za-z0-9_.+,-]*$/;

function footprintFile(ctx: OpContext, nick: string, name: string, create: boolean, description?: unknown): { dir: string; file: string } {
	if (!FP_NAME.test(name)) throw new Error(`footprint name "${name}" may use letters, digits and _ . + - , only (KLC F2.1)`);
	const dir = projectLib(ctx, "fp", nick, create, description);
	return { dir, file: path.join(dir, `${name}.kicad_mod`) };
}

/**
 * Official generator families whose parts come from YAML specs (data/<generator>/*.yaml in
 * kicad-library-tools), so one custom spec makes one part.
 */
const GENERATORS: Record<string, string> = {
	"package/no_lead": "QFN, DFN, SON/WSON/VSON, LGA",
	"package/gullwing": "SOIC, SOP, SSOP, TSSOP, MSOP, QFP, SOT",
	"package/grid_array": "BGA, CSP, LGA",
	"package/PLCC": "PLCC",
	"connector/pin_header_socket": "pin headers and sockets",
	"connector/generator": "generic SMD/THT board connectors",
};

/**
 * Runs generate.py on a data directory of our own: the generators read their specs from a fixed
 * `data` path next to their sources, so point the spec loader at ours (it binds the parsed CLI
 * arguments at import time, so keep its copy in step).
 */
const GENERATE_PY = `import sys
from pathlib import Path
import generators.tools.cli_args as cli_args
import generators.tools.spec.spec_generator as spec_generator

spec_generator._DATA_PATH = Path(sys.argv[1])
_init = cli_args.init
def _init_both(args):
    _init(args)
    spec_generator.CLI_ARGS = args
cli_args.init = _init_both

from generators.generate import run
sys.argv = ["generate.py"] + sys.argv[2:]
run()
`;

function listMods(dir: string): string[] {
	return fs.readdirSync(dir, { recursive: true, encoding: "utf8" }).filter(f => f.endsWith(".kicad_mod")).map(f => path.join(dir, f));
}

const generateFootprint: OpDef = {
	op: "lib_generate_footprint",
	group: "library",
	doc: `Generate a footprint with an official kicad-library-tools generator from one part spec and add it to a project footprint library (variants the generator also makes, e.g. _ThermalVias, are not added). Families: ${Object.entries(
		GENERATORS,
	)
		.map(([g, d]) => `${g} (${d})`)
		.join(
			"; ",
		)}. The spec is one entry of the family's YAML files ($KICAD_LIBRARY_TOOLS/data/<generator>/*.yaml; copy a similar part, keep its keys, change the dimensions), as JSON, e.g. package/no_lead: {"device_type": "WSON", "manufacturer": "Texas", "part_number": "DSG0008A", "size_source": "https://…", "body_size_x": "1.9 .. 2.0 .. 2.1", "body_size_y": "1.9 .. 2.0 .. 2.1", "overall_height": 0.8, "num_pins_x": 0, "num_pins_y": 4, "pitch": 0.5, "lead_width": "0.2 .. 0.25 .. 0.3", "lead_len": "0.2 .. 0.3 .. 0.4", "EP_size_x": "0.8 .. 0.9 .. 1.0", "EP_size_y": "1.5 .. 1.6 .. 1.7", "EP_num_paste_pads": [1, 2]}. "inherit": "<entry>" builds on any entry of those files (and that file's defaults), as the connector families need. The 3D model path is set to the project's <Library>.3dshapes/<Name>.step.`,
	fields: {
		library: { t: "string", req: true, doc: "project footprint library nickname (created when missing)" },
		library_description: LIBRARY_DESCRIPTION,
		generator: { t: "string", req: true, values: Object.keys(GENERATORS), doc: "generator family" },
		spec: { t: "object", req: true, doc: "one part spec in the family's YAML format (as JSON); size_source must be the datasheet URL" },
		name: { t: "string", doc: "footprint name instead of the generated one (e.g. with the vendor prefix of a vendor land pattern)" },
	},
	async apply(ctx, a) {
		const library = a.library as string;
		const generator = a.generator as string;
		const spec = a.spec as Record<string, unknown>;
		const id = typeof a.name === "string" ? a.name : "toolkit_part";
		const tools = await cli.libraryTools(ctx.project.repoRoot);
		const tmp = cli.tmpDir("fpgen");
		let mods: List[];
		try {
			const data = path.join(tmp, "data");
			fs.mkdirSync(path.join(data, generator), { recursive: true });
			// JSON is YAML; an inheriting entry goes into the stock file that defines its parent
			let yaml = "";
			const parent = spec.inherit;
			if (typeof parent === "string") {
				const stock = path.join(tools, "data", generator);
				const entry = (k: string) => new RegExp(`^["']?${k.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}["']?:`, "m");
				const src = fs
					.readdirSync(stock)
					.filter(f => f.endsWith(".yaml"))
					.map(f => fs.readFileSync(path.join(stock, f), "utf8"))
					.find(t => entry(parent).test(t));
				if (!src) throw new Error(`spec.inherit: no entry "${parent}" in ${stock}/*.yaml`);
				if (entry(id).test(src)) throw new Error(`name "${id}" is already an entry of the stock file; choose another name`);
				yaml = `${src}\n`;
			}
			fs.writeFileSync(path.join(data, generator, "part.yaml"), `${yaml}${JSON.stringify(id)}: ${JSON.stringify(spec)}\n`);
			fs.writeFileSync(path.join(tmp, "generate.py"), GENERATE_PY);
			const out = path.join(tmp, "out");
			const r = await cli.run(["python3", "generate.py", data, "-j", "1", "-g", generator, "-p", id, "-f", out], {
				cwd: tmp,
				repoRoot: ctx.project.repoRoot,
				env: { PYTHONPATH: `${path.join(tools, "src")}:${tools}` },
			});
			const files = fs.existsSync(out) ? listMods(out) : [];
			if (r.code !== 0 || !files.length) {
				throw new Error(`generator ${generator} made no footprint (exit ${r.code}): ${(r.stderr.trim() || r.stdout.trim()).slice(-2000)}`);
			}
			mods = files.map(f => parse(fs.readFileSync(f, "utf8")));
		} finally {
			fs.rmSync(tmp, { recursive: true, force: true });
		}
		const names = mods.map(m => text(m) ?? "");
		const base = names.reduce((x, y) => (y.length < x.length ? y : x));
		// one part and its variants (e.g. _ThermalVias) share the base name; anything else is a series
		if (!names.every(n => n.startsWith(base))) {
			throw new Error(`the spec makes ${names.length} different footprints (${names.slice(0, 4).join(", ")}, …); restrict it to one part (e.g. a single pin count)`);
		}
		const mod = mods[names.indexOf(base)]!;
		const name = typeof a.name === "string" ? a.name : base;
		const { dir, file } = footprintFile(ctx, library, name, true, a.library_description);
		if (footprintTree(ctx.project, file)) throw new Error(`footprint ${library}:${name} already exists; remove it first`);
		mod[1] = str(name);
		prop(mod, "Value")![2] = str(name);
		const model = projectModel(ctx.project, dir, library, name);
		for (const m of children(mod, "model")) mod.splice(mod.indexOf(m), 1);
		mod.push(modelNode(model));
		saveFootprint(ctx.project, file, mod);
		ctx.touchPart("footprint", `${library}:${name}`);
		const pads = children(mod, "pad").filter(p => text(p) !== "").length;
		const skipped = names.filter(n => n !== base);
		ctx.notes.push(
			`added footprint ${library}:${name} (${generator}, ${pads} pads); 3D model path ${model} (build the STEP there, or point it at a stock model with lib_edit_footprint)` +
				(skipped.length ? `; generator variants not added: ${skipped.join(", ")}` : ""),
		);
	},
};

type Rect = [number, number, number, number];

function grow(r: Rect, d: number): Rect {
	return [r[0] - d, r[1] - d, r[2] + d, r[3] + d];
}

function hits(a: Rect, b: Rect): boolean {
	return a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
}

const PAD_TYPES = ["smd", "thru_hole", "np_thru_hole", "connect"] as const;
const PAD_SHAPES = ["rect", "roundrect", "circle", "oval"] as const;
const PAD_LAYERS: Record<string, string[]> = {
	smd: ["F.Cu", "F.Mask", "F.Paste"],
	connect: ["F.Cu", "F.Mask"],
	thru_hole: ["*.Cu", "*.Mask"],
	np_thru_hole: ["*.Cu", "*.Mask"],
};

const SILK_W = 0.12;
const FAB_W = 0.1;
const CRTYD_W = 0.05;
/** Copper to silkscreen clearance (KLC F5.1). */
const SILK_PAD = 0.2;
const SILK_MIN_LEN = 0.1;

interface PadIn {
	number: string;
	type: string;
	shape: string;
	at: [number, number];
	/** Degrees (pads read from a footprint file). */
	rot?: number;
	size: [number, number];
	drill?: number | [number, number];
	layers: string[];
}

/** A pad given to lib_create_footprint / add_pads, with the node it is written as. */
interface NewPad extends PadIn {
	node: List;
}

/** Pad overrides the pad inputs take as lib_footprint reports them (KiCad's tokens), with their limits. */
const PAD_OVERRIDES: Record<string, { min?: number; max?: number; integer?: boolean }> = {
	solder_mask_margin: {},
	solder_paste_margin: {},
	solder_paste_margin_ratio: {},
	clearance: { min: 0 },
	zone_connect: { min: 0, max: 3, integer: true },
	thermal_bridge_width: { min: 0 },
	thermal_bridge_angle: {},
	thermal_gap: { min: 0 },
};
const CHAMFER_CORNERS = ["top_left", "top_right", "bottom_left", "bottom_right"];
/** Optional pad keys of the pad inputs (lib_footprint's pad details). */
const PAD_SETTING_KEYS = ["roundrect_rratio", "corner_radius", "chamfer_ratio", "chamfer", ...Object.keys(PAD_OVERRIDES)];
/** KiCad's order of those pad children (PCB_IO_KICAD_SEXPR's pad writer), for inserting a missing one. */
const PAD_ORDER = [
	"roundrect_rratio",
	"chamfer_ratio",
	"chamfer",
	"solder_mask_margin",
	"solder_paste_margin",
	"solder_paste_margin_ratio",
	"clearance",
	"zone_connect",
	"thermal_bridge_width",
	"thermal_bridge_angle",
	"thermal_gap",
	"uuid",
];

const PAD_SETTINGS_DOC =
	'roundrect pads: roundrect_rratio (default 0.25) or corner_radius in mm (rratio = radius / smaller pad side; both may be given when they agree, as lib_footprint reports them), chamfer_ratio with chamfer (corners: top_left|top_right|bottom_left|bottom_right); any pad: overrides solder_mask_margin, solder_paste_margin (mm), solder_paste_margin_ratio, clearance (mm), zone_connect (0 none, 1 thermal, 2 solid, 3 thermal for through-hole only), thermal_bridge_width, thermal_gap (mm), thermal_bridge_angle (degrees); unset ones follow the footprint and board settings';

/** Set a pad child: replace it, else insert it where KiCad writes it. */
function setPadChild(p: List, node: List): void {
	const key = head(node)!;
	const i = p.findIndex(c => isList(c) && head(c) === key);
	if (i >= 0) {
		p[i] = node;
		return;
	}
	const rank = PAD_ORDER.indexOf(key);
	const j = p.findIndex(c => isList(c) && PAD_ORDER.indexOf(head(c) ?? "") > rank);
	if (j >= 0) p.splice(j, 0, node);
	else p.push(node);
}

/** Apply the optional pad settings of a pads entry (PAD_SETTING_KEYS; null removes one) to a pad node of its final shape and size. */
function padSettings(p: List, e: Record<string, unknown>, where: string): void {
	const rr = e.roundrect_rratio;
	const cr = e.corner_radius;
	if (text(p, 3) !== "roundrect") {
		const given = ["roundrect_rratio", "corner_radius", "chamfer_ratio", "chamfer"].filter(k => e[k] !== undefined && e[k] !== null);
		if (given.length) throw new Error(`${where}: ${given.join(", ")} ${given.length > 1 ? "are" : "is"} for roundrect pads`);
		for (const k of ["roundrect_rratio", "chamfer_ratio", "chamfer"]) removeChild(p, k);
	} else {
		if (rr !== undefined && (typeof rr !== "number" || rr < 0 || rr > 0.5)) throw new Error(`${where}: roundrect_rratio is a number from 0 to 0.5`);
		if (cr !== undefined && (typeof cr !== "number" || cr < 0)) throw new Error(`${where}: corner_radius is a radius in mm`);
		const size = child(p, "size");
		const side = Math.min(num(size, 1), num(size, 2));
		if (typeof cr === "number") {
			if (cr > side / 2 + 1e-9) throw new Error(`${where}: corner_radius ${cr} mm is more than half the smaller pad side (${side} mm)`);
			if (typeof rr === "number" && Math.abs(rr * side - cr) > 5e-4) {
				throw new Error(`${where}: roundrect_rratio ${rr} makes a ${mm(rr * side).v} mm corner radius on this pad, not corner_radius ${cr}; give one of them`);
			}
		}
		const ratio = typeof rr === "number" ? rr : typeof cr === "number" ? cr / side : undefined;
		if (ratio !== undefined) setPadChild(p, L("roundrect_rratio", mm(ratio)));
		else if (!child(p, "roundrect_rratio")) setPadChild(p, L("roundrect_rratio", 0.25));
		if (e.chamfer === null) {
			removeChild(p, "chamfer_ratio");
			removeChild(p, "chamfer");
		} else if (e.chamfer !== undefined || e.chamfer_ratio !== undefined) {
			const ch = e.chamfer_ratio;
			if (ch !== undefined && (typeof ch !== "number" || ch < 0 || ch > 0.5)) throw new Error(`${where}: chamfer_ratio is a number from 0 to 0.5`);
			if (typeof ch === "number") setPadChild(p, L("chamfer_ratio", mm(ch)));
			const corners = e.chamfer;
			if (corners !== undefined) {
				if (!Array.isArray(corners) || !corners.length || !corners.every((c): c is string => typeof c === "string" && CHAMFER_CORNERS.includes(c))) {
					throw new Error(`${where}: chamfer is a list of corners from ${CHAMFER_CORNERS.join(", ")} (null removes the chamfer)`);
				}
				setPadChild(p, L("chamfer", ...corners.map(c => new Sym(c))));
			}
			if (!child(p, "chamfer_ratio") || !child(p, "chamfer")) throw new Error(`${where}: a chamfer needs both chamfer_ratio and chamfer (the corners)`);
		}
	}
	for (const [k, lim] of Object.entries(PAD_OVERRIDES)) {
		const v = e[k];
		if (v === undefined) continue;
		if (v === null) {
			removeChild(p, k);
			continue;
		}
		const [lo, hi] = [lim.min ?? -Infinity, lim.max ?? Infinity];
		if (typeof v !== "number" || !Number.isFinite(v) || v < lo || v > hi || (lim.integer && !Number.isInteger(v))) {
			throw new Error(`${where}: "${k}" must be ${lim.integer ? "an integer" : "a number"}${lo > -Infinity ? ` from ${lo}` : ""}${hi < Infinity ? ` to ${hi}` : ""} (null removes it), got ${JSON.stringify(v)}`);
		}
		setPadChild(p, L(k, mm(v)));
	}
}

function isPair(v: unknown): v is [number, number] {
	return Array.isArray(v) && v.length === 2 && v.every(x => typeof x === "number" && Number.isFinite(x));
}

function padsIn(raw: Record<string, unknown>[], field = "pads"): NewPad[] {
	return raw.map((p, i) => {
		const where = `${field}[${i}]`;
		for (const k of Object.keys(p)) {
			if (!["number", "type", "shape", "at", "size", "drill", "layers", ...PAD_SETTING_KEYS].includes(k)) throw new Error(`${where}: unknown key "${k}"`);
		}
		const type = oneOf(strField(p, "type", where), PAD_TYPES, `${where} type`);
		const shape = oneOf(strField(p, "shape", where), PAD_SHAPES, `${where} shape`);
		if (!type || !shape) throw new Error(`${where}: "type" and "shape" are required`);
		if (!isPair(p.at) || !isPair(p.size)) throw new Error(`${where}: "at" and "size" must be [x, y] in mm`);
		const drill = p.drill;
		const holed = type === "thru_hole" || type === "np_thru_hole";
		if (holed !== (drill !== undefined)) throw new Error(`${where}: ${holed ? `a ${type} pad needs "drill"` : `a ${type} pad has no drill`}`);
		if (drill !== undefined && !(typeof drill === "number" || isPair(drill))) throw new Error(`${where}: "drill" must be a diameter or [w, h] (oval)`);
		const layers = p.layers === undefined ? PAD_LAYERS[type]! : p.layers;
		if (!Array.isArray(layers) || !layers.every(l => typeof l === "string")) throw new Error(`${where}: "layers" must be a list of layer names`);
		const pad: PadIn = { number: strField(p, "number", where) ?? "", type, shape, at: p.at, size: p.size, drill, layers: layers as string[] };
		const node = padNode(pad);
		padSettings(node, p, where);
		return { ...pad, node };
	});
}

/** Bounding box of a pad, its rotation included. */
function padRect(p: PadIn): Rect {
	const r = ((p.rot ?? 0) * Math.PI) / 180;
	const c = Math.abs(Math.cos(r));
	const s = Math.abs(Math.sin(r));
	const hw = (c * p.size[0] + s * p.size[1]) / 2;
	const hh = (s * p.size[0] + c * p.size[1]) / 2;
	return [p.at[0] - hw, p.at[1] - hh, p.at[0] + hw, p.at[1] + hh];
}

/** A pad's number, type, shape, position, size, drill and layers; padSettings adds the rest. */
function padNode(p: PadIn): List {
	const parts: Item[] = [str(p.number), new Sym(p.type), new Sym(p.shape), L("at", mm(p.at[0]), mm(p.at[1]), ...(p.rot ? [p.rot] : [])), xy("size", p.size)];
	if (p.drill !== undefined) parts.push(typeof p.drill === "number" ? L("drill", mm(p.drill)) : L("drill", "oval", mm(p.drill[0]), mm(p.drill[1])));
	parts.push(L("layers", ...p.layers.map(l => str(l))));
	return L("pad", ...parts);
}

/** A pad of a footprint file, as the outline drawing sees it. */
function padOf(p: List): PadIn {
	const at = child(p, "at");
	const size = child(p, "size");
	return {
		number: text(p, 1) ?? "",
		type: text(p, 2) ?? "",
		shape: text(p, 3) ?? "",
		at: [num(at, 1), num(at, 2)],
		rot: at && at.length > 3 ? num(at, 3) : 0,
		size: [num(size, 1), num(size, 2)],
		layers: (child(p, "layers") ?? []).slice(1).flatMap(l => (isList(l) ? [] : [l.v])),
	};
}

function textEffects(size: number, thickness: number): List {
	return L("effects", L("font", L("size", size, size), L("thickness", thickness)));
}

interface Body {
	width: number;
	height: number;
	center: [number, number];
}

const BODY: Fields = {
	width: { t: "number", req: true, doc: "body size along x in mm" },
	height: { t: "number", req: true, doc: "body size along y in mm" },
	center: { t: "point", doc: "body centre (default [0, 0])" },
};

function bodyIn(raw: unknown, where: string): Body {
	if (typeof raw !== "object" || raw === null || Array.isArray(raw)) throw new Error(`${where}: expected {width, height, center?}`);
	const b = validate(where, { ...raw }, BODY);
	const [width, height] = [b.width as number, b.height as number];
	if (width <= 0 || height <= 0) throw new Error(`${where}: width and height must be positive (mm)`);
	return { width, height, center: (b.center as [number, number] | undefined) ?? [0, 0] };
}

/** Parts of the segment a→b (axis-parallel) outside every keep-out rectangle. */
function clipSegment(a: [number, number], b: [number, number], keepouts: Rect[]): [[number, number], [number, number]][] {
	const horiz = a[1] === b[1];
	const ax = horiz ? 0 : 1;
	const c = horiz ? a[1] : a[0];
	const lo = Math.min(a[ax], b[ax]);
	const hi = Math.max(a[ax], b[ax]);
	const cut = keepouts
		.filter(k => (horiz ? k[1] < c && c < k[3] : k[0] < c && c < k[2]))
		.map(k => (horiz ? [k[0], k[2]] : [k[1], k[3]]) as [number, number])
		.sort((x, y) => x[0] - y[0]);
	const out: [[number, number], [number, number]][] = [];
	let from = lo;
	const emit = (s: number, e: number) => {
		if (e - s >= SILK_MIN_LEN) out.push(horiz ? [[s, c], [e, c]] : [[c, s], [c, e]]);
	};
	for (const [s, e] of cut) {
		if (e <= from) continue;
		if (s >= hi) break;
		emit(from, Math.min(s, hi));
		from = Math.max(from, e);
	}
	if (from < hi) emit(from, hi);
	return out;
}

/** Round outward to the courtyard grid (0.01 mm). */
function crtydRound(v: number, up: boolean): number {
	return (up ? Math.ceil(v * 100 - 1e-6) : Math.floor(v * 100 + 1e-6)) / 100;
}

/**
 * KLC-style drawings around the pads: F.Fab body with the corner nearest pad 1 chamfered and
 * ${REFERENCE}, F.SilkS outline clear of copper with a pin-1 triangle, F.CrtYd rectangle around pads
 * and body (returned as `courtyard`, for placing Reference and Value).
 */
function outlineItems(pads: PadIn[], o: Body, margin: number): { items: List[]; courtyard: Rect } {
	const { width: w, height: h, center: c } = o;
	const body: Rect = [c[0] - w / 2, c[1] - h / 2, c[0] + w / 2, c[1] + h / 2];
	const copper = pads.filter(p => p.layers.some(l => l.endsWith(".Cu")) || p.type === "np_thru_hole");
	const keepouts = copper.map(p => grow(padRect(p), SILK_PAD + SILK_W / 2));
	const pin1 = pads.find(p => p.number === "1");
	const corners: [number, number][] = [
		[body[0], body[1]],
		[body[2], body[1]],
		[body[2], body[3]],
		[body[0], body[3]],
	];
	const items: List[] = [];

	// F.Fab: body outline, corner nearest pad 1 chamfered
	const ch = Math.min(1, 0.25 * Math.min(w, h));
	const near = pin1 ? corners.map((k, i) => [Math.hypot(k[0] - pin1.at[0], k[1] - pin1.at[1]), i] as const).sort((x, y) => x[0] - y[0])[0]![1] : -1;
	const fab: [number, number][] = [];
	corners.forEach((k, i) => {
		if (i !== near) {
			fab.push(k);
			return;
		}
		for (const nb of [corners[(i + 3) % 4]!, corners[(i + 1) % 4]!]) {
			const d = Math.hypot(nb[0] - k[0], nb[1] - k[1]);
			fab.push([k[0] + ((nb[0] - k[0]) * ch) / d, k[1] + ((nb[1] - k[1]) * ch) / d]);
		}
	});
	items.push(encodeShape({ type: "poly", points: fab, width: FAB_W }, "F.Fab"));

	// F.SilkS: outline just outside the body, cut clear of the pads
	const off = (FAB_W + SILK_W) / 2;
	const s: Rect = grow(body, off);
	const sc: [number, number][] = [
		[s[0], s[1]],
		[s[2], s[1]],
		[s[2], s[3]],
		[s[0], s[3]],
	];
	for (let i = 0; i < 4; i++) {
		for (const [a, b] of clipSegment(sc[i]!, sc[(i + 1) % 4]!, keepouts)) items.push(encodeShape({ type: "line", start: a, end: b, width: SILK_W }, "F.SilkS"));
	}

	// F.SilkS pin-1 mark: a filled triangle outside the outline, pointing at pad 1 across the nearest body edge
	if (pin1) {
		const [px, py] = pin1.at;
		const edges = [
			{ d: Math.abs(px - body[0]), n: [-1, 0] },
			{ d: Math.abs(body[2] - px), n: [1, 0] },
			{ d: Math.abs(py - body[1]), n: [0, -1] },
			{ d: Math.abs(body[3] - py), n: [0, 1] },
		].sort((x, y) => x.d - y.d);
		const [nx, ny] = edges[0]!.n as [number, number];
		const along = nx ? Math.min(Math.max(py, s[1]), s[3]) : Math.min(Math.max(px, s[0]), s[2]);
		const len = 0.33;
		const half = 0.24;
		for (let shift = 0; shift < 20; shift += 0.05) {
			const base = nx ? (nx < 0 ? s[0] : s[2]) + nx * shift : (ny < 0 ? s[1] : s[3]) + ny * shift;
			const tri: [number, number][] = nx
				? [
						[base, along],
						[base + nx * len, along - half],
						[base + nx * len, along + half],
					]
				: [
						[along, base],
						[along - half, base + ny * len],
						[along + half, base + ny * len],
					];
			const box: Rect = grow(
				[Math.min(...tri.map(t => t[0])), Math.min(...tri.map(t => t[1])), Math.max(...tri.map(t => t[0])), Math.max(...tri.map(t => t[1]))],
				SILK_W / 2,
			);
			if (keepouts.some(k => hits(k, box))) continue;
			items.push(encodeShape({ type: "poly", points: tri, width: SILK_W, fill: true }, "F.SilkS"));
			break;
		}
	}

	// F.CrtYd: rectangle around pads and body plus the margin, on the 0.01 mm grid
	const all = [body, ...pads.map(padRect)];
	const cy: Rect = [
		crtydRound(Math.min(...all.map(r => r[0])) - margin, false),
		crtydRound(Math.min(...all.map(r => r[1])) - margin, false),
		crtydRound(Math.max(...all.map(r => r[2])) + margin, true),
		crtydRound(Math.max(...all.map(r => r[3])) + margin, true),
	];
	const cc: [number, number][] = [
		[cy[0], cy[1]],
		[cy[2], cy[1]],
		[cy[2], cy[3]],
		[cy[0], cy[3]],
	];
	for (let i = 0; i < 4; i++) items.push(encodeShape({ type: "line", start: cc[i]!, end: cc[(i + 1) % 4]!, width: CRTYD_W }, "F.CrtYd"));

	const refSize = Math.min(1, Math.max(0.5, Math.round((Math.min(w, h) / 4) * 100) / 100));
	items.push(encodeShape({ type: "text", text: "${REFERENCE}", at: c, size: refSize, thickness: Math.round(refSize * 15) / 100 }, "F.Fab"));
	return { items, courtyard: cy };
}

/** Reference centred above the courtyard, Value centred below it. */
function fieldPlaces(cy: Rect): { reference: [number, number]; value: [number, number] } {
	const cx = (cy[0] + cy[2]) / 2;
	return { reference: [cx, cy[1] - 0.7], value: [cx, cy[3] + 0.7] };
}

/** KLC-style footprint: pads plus the outline drawings of `outlineItems`. */
function drawFootprint(o: { name: string; description: string; tags: string; attr: string; pads: NewPad[]; body: Body; margin: number; model?: string }): List {
	const { items, courtyard } = outlineItems(o.pads, o.body, o.margin);
	const at = fieldPlaces(courtyard);
	return L(
		"footprint",
		str(o.name),
		L("version", 20260206),
		L("generator", str("pcbnew")),
		L("generator_version", str("10.0")),
		L("layer", str("F.Cu")),
		L("descr", str(o.description)),
		L("tags", str(o.tags)),
		L("property", str("Reference"), str("REF**"), L("at", ...at.reference, 0), L("layer", str("F.SilkS")), textEffects(1, 0.15)),
		L("property", str("Value"), str(o.name), L("at", ...at.value, 0), L("layer", str("F.Fab")), textEffects(1, 0.15)),
		L("attr", new Sym(o.attr)),
		L("duplicate_pad_numbers_are_jumpers", false),
		...items,
		...o.pads.map(p => p.node),
		L("embedded_fonts", false),
		...(o.model ? [modelNode(o.model)] : []),
	);
}

const createFootprint: OpDef = {
	op: "lib_create_footprint",
	group: "library",
	doc: "Draw a KLC-style footprint for a part no generator family fits (modules, odd connectors) and add it to a project footprint library: your pads, F.Fab body outline with pin-1 chamfer and ${REFERENCE}, F.SilkS outline 0.12 mm clear of copper by 0.2 mm with a pin-1 triangle, F.CrtYd rectangle around pads and body, then your own `graphics`. Origin: body centre for SMD, pad 1 for through-hole (KLC F7.2).",
	fields: {
		library: { t: "string", req: true, doc: "project footprint library nickname (created when missing)" },
		library_description: LIBRARY_DESCRIPTION,
		name: { t: "string", req: true, doc: 'footprint name per KLC F2, e.g. "Waveshare_ESP32-P4-Core"' },
		description: { t: "string", req: true, doc: "description ending with the datasheet URL (KLC F9.1)" },
		tags: { t: "string", req: true, doc: "space-separated keywords" },
		attr: { t: "string", req: true, values: ["smd", "through_hole"], doc: "placement type" },
		pads: {
			t: "objects",
			req: true,
			doc: `[{number, type, shape, at, size, drill?, layers?, …settings}]: type ${PAD_TYPES.join("|")}, shape ${PAD_SHAPES.join("|")}, at/size [x, y] mm (y down), drill diameter or [w, h] slot (holes only), layers default by type (smd: F.Cu F.Mask F.Paste; thru_hole: *.Cu *.Mask); optional settings, as lib_footprint reports them: ${PAD_SETTINGS_DOC}`,
		},
		body: { t: "object", req: true, doc: "{width, height, center?: [x, y]} body outline in mm (center default [0, 0])" },
		courtyard_margin: { t: "number", doc: "courtyard clearance around pads and body (default 0.25 mm; KLC: 0.5 connectors, 1 BGA)" },
		model: { t: "boolean", doc: "reference the 3D model <Library>.3dshapes/<Name>.step next to the library (default false)" },
		...MODEL_PLACEMENT_FIELDS,
		graphics: { t: "object", doc: `${GRAPHICS_DOC}; replaces the generated drawings of those layers` },
	},
	apply(ctx, a) {
		const library = a.library as string;
		const name = a.name as string;
		const body = bodyIn(a.body, "body");
		const pads = padsIn(a.pads as Record<string, unknown>[]);
		if (!pads.length) throw new Error("pads: give at least one pad");
		const graphics = a.graphics ? graphicsIn(a.graphics as Record<string, unknown>) : undefined;
		if (!/https?:\/\//.test(a.description as string)) throw new Error("description must include the datasheet URL (KLC F9.1)");
		const { dir, file } = footprintFile(ctx, library, name, true, a.library_description);
		if (footprintTree(ctx.project, file)) throw new Error(`footprint ${library}:${name} already exists; remove it first`);
		const model = a.model === true ? projectModel(ctx.project, dir, library, name) : undefined;
		const tree = drawFootprint({
			name,
			description: a.description as string,
			tags: a.tags as string,
			attr: a.attr as string,
			pads,
			body,
			margin: typeof a.courtyard_margin === "number" ? a.courtyard_margin : 0.25,
			model,
		});
		if (graphics) applyGraphics(tree, graphics);
		const placed = placeModel(tree, a);
		saveFootprint(ctx.project, file, tree);
		ctx.touchPart("footprint", `${library}:${name}`);
		ctx.notes.push(`added footprint ${library}:${name} (${pads.length} pads)${model ? `; 3D model path ${model}` : ""}${placed.length ? `, ${placed.join(", ")}` : ""}`);
	},
};

const copyFootprint: OpDef = {
	op: "lib_copy_footprint",
	group: "library",
	doc: "Copy a stock or project footprint into a project footprint library (created and registered when missing) under a new name (footprint name and Value), to derive a part from a similar one: change its pads and drawings with lib_edit_footprint, then its description (datasheet URL) and 3D model. Everything else is copied as is.",
	fields: {
		from: { t: "string", req: true, doc: '"Library:Name" of the source, e.g. "Connector_RJ:RJ45_Abracon_ARJP11A-MA_Horizontal"' },
		library: { t: "string", req: true, doc: "project footprint library nickname (created when missing)" },
		library_description: LIBRARY_DESCRIPTION,
		name: { t: "string", req: true, doc: "new footprint name per KLC F2; must not exist in the library" },
	},
	apply(ctx, a) {
		const from = a.from as string;
		const library = a.library as string;
		const name = a.name as string;
		const [nick, srcName] = splitId(from);
		const srcDir = stateOf(ctx.project).added.get(`fp:${nick}`) ?? ctx.libs.fp.find(e => e.name === nick)?.uri;
		const srcFile = srcDir ? path.join(srcDir, `${srcName}.kicad_mod`) : "";
		const src = srcFile ? footprintTree(ctx.project, srcFile) : undefined;
		if (!src) throw new Error(`footprint "${from}" not found (expected "Library:Name" of an existing footprint)`);
		const { file } = footprintFile(ctx, library, name, true, a.library_description);
		if (footprintTree(ctx.project, file)) throw new Error(`footprint ${library}:${name} already exists; remove it first`);
		const fp = clone(src);
		fp[1] = str(name);
		const value = prop(fp, "Value");
		if (value) value[2] = str(name);
		saveFootprint(ctx.project, file, fp);
		ctx.touchPart("footprint", `${library}:${name}`);
		const source = ctx.libs.fp.find(e => e.name === nick)?.source;
		const model = children(fp, "model").map(m => text(m) ?? "");
		ctx.notes.push(
			`copied footprint ${from} to ${library}:${name}` +
				(source === "stock" ? "; it derives from the KiCad footprint library, CC-BY-SA 4.0 material" : "") +
				(source === "user" ? `; ${nick} is the user's own global library, neither in the repo nor KLC-reviewed: check the copy against the datasheet and KLC` : "") +
				`; still ${from}'s: description "${text(child(fp, "descr")) ?? ""}", Datasheet "${propValue(fp, "Datasheet") ?? ""}", 3D model ${model.join(", ") || "none"}; update them for the new part (lib_edit_footprint description/model)`,
		);
	},
};

const PAD_EDIT_KEYS = ["pad", "number", "type", "shape", "at", "size", "drill", "layers", ...PAD_SETTING_KEYS, "remove"];

/** The pad a selector names: its number, or {number, near} for the nearest of several pads sharing a number. */
function selectPad(pads: List[], sel: unknown, where: string): List {
	let number = sel;
	let near: [number, number] | undefined;
	if (typeof sel === "object" && sel !== null && !Array.isArray(sel)) {
		for (const k of Object.keys(sel)) if (k !== "number" && k !== "near") throw new Error(`${where}.pad: unknown key "${k}" (expected {number, near: [x, y]})`);
		number = "number" in sel ? sel.number : undefined;
		const n = "near" in sel ? sel.near : undefined;
		if (!isPair(n)) throw new Error(`${where}.pad.near must be [x, y] in mm`);
		near = n;
	}
	if (typeof number === "number") number = String(number);
	if (typeof number !== "string") throw new Error(`${where}: "pad" must be a pad number ("6"; "" for unnumbered pads) or {number, near: [x, y]}`);
	const hits = pads.filter(p => (text(p, 1) ?? "") === number);
	if (!hits.length) throw new Error(`${where}: no pad "${number}"; pads: ${[...new Set(pads.map(p => `"${text(p, 1) ?? ""}"`))].join(", ")}`);
	const places = hits.map(p => inline(child(p, "at") ?? [])).join(", ");
	if (!near) {
		if (hits.length > 1) throw new Error(`${where}: ${hits.length} pads are numbered "${number}" (${places}); select one with {number: "${number}", near: [x, y]}`);
		return hits[0]!;
	}
	const [x, y] = near;
	const by = hits
		.map(p => ({ p, d: Math.hypot(num(child(p, "at"), 1) - x, num(child(p, "at"), 2) - y) }))
		.sort((m, n) => m.d - n.d);
	if (by.length > 1 && by[1]!.d - by[0]!.d < 1e-6) throw new Error(`${where}: pads "${number}" ${places} are equally near [${x}, ${y}]`);
	return by[0]!.p;
}

/** Apply one `pads` entry (everything but the selector and `remove`) to a pad node. */
function editPad(p: List, e: Record<string, unknown>, where: string): void {
	const number = strField(e, "number", where);
	const type = oneOf(strField(e, "type", where), PAD_TYPES, `${where} type`);
	const shape = oneOf(strField(e, "shape", where), PAD_SHAPES, `${where} shape`);
	if (number !== undefined) p[1] = str(number);
	if (type) p[2] = new Sym(type);
	if (shape) p[3] = new Sym(shape);
	if (e.at !== undefined) {
		const at = e.at;
		if (!Array.isArray(at) || at.length < 2 || at.length > 3 || !at.every(v => typeof v === "number" && Number.isFinite(v))) {
			throw new Error(`${where}: "at" must be [x, y] (rotation kept) or [x, y, degrees] in mm`);
		}
		const old = child(p, "at");
		const rot = at.length === 3 ? at[2] : old && old.length > 3 ? num(old, 3) : 0;
		setChild(p, L("at", mm(at[0]), mm(at[1]), ...(rot ? [rot] : [])));
	}
	if (e.size !== undefined) {
		if (!isPair(e.size)) throw new Error(`${where}: "size" must be [w, h] in mm`);
		setChild(p, xy("size", e.size));
	}
	const t = text(p, 2) ?? "";
	const holed = t === "thru_hole" || t === "np_thru_hole";
	const drill = e.drill;
	if (drill !== undefined) {
		if (!holed) throw new Error(`${where}: a ${t} pad has no drill`);
		if (!(typeof drill === "number" || isPair(drill))) throw new Error(`${where}: "drill" must be a diameter or [w, h] (oval)`);
		// a drill offset stays
		const offset = child(child(p, "drill") ?? [], "offset");
		setChild(p, L("drill", ...(typeof drill === "number" ? [mm(drill)] : ["oval", mm(drill[0]), mm(drill[1])]), ...(offset ? [offset] : [])));
	} else if (!holed) removeChild(p, "drill");
	else if (!child(p, "drill")) throw new Error(`${where}: a ${t} pad needs "drill"`);
	const layers = e.layers ?? (type ? PAD_LAYERS[type] : undefined);
	if (layers !== undefined) {
		if (!Array.isArray(layers) || !layers.every((l): l is string => typeof l === "string")) throw new Error(`${where}: "layers" must be a list of layer names`);
		setChild(p, L("layers", ...layers.map(l => str(l))));
	}
	padSettings(p, e, where);
}

const editFootprint: OpDef = {
	op: "lib_edit_footprint",
	group: "library",
	doc: "Change a project library footprint, in this order: description, tags, 3D model path and placement (model_offset/rotation/scale); `pads` (change or remove pads), `add_pads`, `outline` (redraw F.Fab, F.SilkS and F.CrtYd around the pads as lib_create_footprint does), `graphics` (replace the drawings of given layers). Everything not named stays. Boards keep their copy until updated from the library.",
	fields: {
		library: { t: "string", req: true, doc: "project footprint library nickname" },
		name: { t: "string", req: true, doc: "footprint name" },
		description: { t: "string", doc: "new description (keep the datasheet URL)" },
		tags: { t: "string", doc: "new space-separated keywords" },
		model: {
			t: "string",
			doc: 'model path, e.g. "${KIPRJMOD}/../lib/<Library>.3dshapes/<Name>.step" or a stock "${KICAD10_3DMODEL_DIR}/…" (replacing the one model keeps its placement); null removes every model',
		},
		...MODEL_PLACEMENT_FIELDS,
		pads: {
			t: "objects",
			doc: `[{pad, number?, type?, shape?, at?, size?, drill?, layers?, …settings, remove?: true}]: pad selects one pad by number ("6"; "" for unnumbered NPTH) or, when several share it, {number, near: [x, y]} (the nearest); an ambiguous selector is an error. Selectors refer to the pads before this edit. The other keys as in lib_create_footprint's pads, settings null removes one (chamfer: null removes the chamfer); at [x, y] keeps the pad rotation, [x, y, degrees] sets it; a new size keeps roundrect_rratio (the corner radius scales) unless corner_radius is given; a new type without layers gets that type's default layers; remove: true deletes the pad`,
		},
		add_pads: { t: "objects", doc: "new pads, in lib_create_footprint's pads format" },
		outline: {
			t: "object",
			doc: "{body: {width, height, center?: [x, y]}, courtyard_margin? (default 0.25 mm; KLC: 0.5 connectors)}: replace every drawing on F.Fab, F.SilkS and F.CrtYd with lib_create_footprint's outline around the current pads, and put Reference above and Value below the courtyard",
		},
		graphics: { t: "object", doc: `${GRAPHICS_DOC}; applied after outline` },
	},
	apply(ctx, a) {
		const library = a.library as string;
		const name = a.name as string;
		const { file } = footprintFile(ctx, library, name, false);
		const tree = footprintTree(ctx.project, file);
		if (!tree) throw new Error(`no footprint ${library}:${name}`);
		const changes: string[] = [];
		if (typeof a.description === "string") {
			setChild(tree, L("descr", str(a.description)));
			changes.push("description");
		}
		if (typeof a.tags === "string") {
			setChild(tree, L("tags", str(a.tags)));
			changes.push("tags");
		}
		if ("model" in a) {
			const old = children(tree, "model");
			if (typeof a.model === "string" && old.length === 1) old[0]![1] = str(a.model);
			else {
				for (const m of old) tree.splice(tree.indexOf(m), 1);
				if (typeof a.model === "string") tree.push(modelNode(a.model));
			}
			changes.push(typeof a.model === "string" ? `model ${a.model}` : "model removed");
		}
		changes.push(...placeModel(tree, a));

		const edits = (a.pads as Record<string, unknown>[] | undefined) ?? [];
		const pads = children(tree, "pad");
		const picked = edits.map((e, i) => {
			for (const k of Object.keys(e)) if (!PAD_EDIT_KEYS.includes(k)) throw new Error(`pads[${i}]: unknown key "${k}"`);
			return selectPad(pads, e.pad, `pads[${i}]`);
		});
		edits.forEach((e, i) => {
			const where = `pads[${i}]`;
			const p = picked[i]!;
			const j = picked.indexOf(p);
			if (j < i) throw new Error(`${where} selects the same pad as pads[${j}]; give each pad one entry`);
			const label = `pad "${text(p, 1) ?? ""}" ${inline(child(p, "at") ?? [])}`;
			const keys = Object.keys(e).filter(k => k !== "pad");
			if (e.remove !== undefined) {
				if (e.remove !== true || keys.length > 1) throw new Error(`${where}: remove: true deletes the pad and takes no other change`);
				tree.splice(tree.indexOf(p), 1);
				changes.push(`removed ${label}`);
				return;
			}
			if (!keys.length) throw new Error(`${where}: nothing to change for ${label}`);
			editPad(p, e, where);
			changes.push(`${label}: ${keys.join(", ")}`);
		});

		if (a.add_pads) {
			const add = padsIn(a.add_pads as Record<string, unknown>[], "add_pads");
			const last = children(tree, "pad").at(-1);
			const end = tree.findIndex(c => isList(c) && ["embedded_fonts", "model"].includes(text(c, 0) ?? ""));
			tree.splice(last ? tree.indexOf(last) + 1 : end < 0 ? tree.length : end, 0, ...add.map(p => p.node));
			changes.push(`added pads ${add.map(p => `"${p.number}"`).join(", ")}`);
		}

		if (a.outline) {
			const o = validate("outline", { ...(a.outline as Record<string, unknown>) }, { body: { t: "object", req: true, doc: "" }, courtyard_margin: { t: "number", doc: "" } });
			const margin = (o.courtyard_margin as number | undefined) ?? 0.25;
			if (margin < 0) throw new Error("outline.courtyard_margin must not be negative");
			const { items, courtyard } = outlineItems(children(tree, "pad").map(padOf), bodyIn(o.body, "outline.body"), margin);
			replaceDrawings(tree, ["F.Fab", "F.SilkS", "F.CrtYd"], items);
			const at = fieldPlaces(courtyard);
			for (const [field, pos] of [
				["Reference", at.reference],
				["Value", at.value],
			] as const) {
				const f = prop(tree, field);
				if (f) setChild(f, L("at", mm(pos[0]), mm(pos[1]), 0));
			}
			changes.push("outline redrawn (F.Fab, F.SilkS, F.CrtYd)");
		}

		if (a.graphics) {
			const g = graphicsIn(a.graphics as Record<string, unknown>);
			applyGraphics(tree, g);
			changes.push(`drawings replaced on ${Object.keys(g).join(", ") || "no layer"}`);
		}

		if (!changes.length) throw new Error("nothing to change: give description, tags, model, pads, add_pads, outline and/or graphics");
		saveFootprint(ctx.project, file, tree);
		ctx.touchPart("footprint", `${library}:${name}`);
		ctx.notes.push(`${library}:${name}: ${changes.join("; ")}`);
	},
};

const removeFootprint: OpDef = {
	op: "lib_remove_footprint",
	group: "library",
	doc: "Delete a footprint from a project footprint library; refused while a placed symbol uses it.",
	fields: {
		library: { t: "string", req: true, doc: "project footprint library nickname" },
		name: { t: "string", req: true, doc: "footprint name" },
	},
	apply(ctx, a) {
		const library = a.library as string;
		const name = a.name as string;
		const { file } = footprintFile(ctx, library, name, false);
		if (!footprintTree(ctx.project, file)) throw new Error(`no footprint ${library}:${name}`);
		const refs = usedBy(ctx.project, sym => propValue(sym, "Footprint") === `${library}:${name}`);
		if (refs.length) throw new Error(`${library}:${name} is the footprint of ${refs.join(", ")}; change those first`);
		ctx.project.deleteFile(file);
		stateOf(ctx.project).footprints.set(file, null);
		ctx.notes.push(`removed footprint ${library}:${name}`);
	},
};

export const ops: OpDef[] = [
	createSymbol,
	drawSymbol,
	copySymbol,
	editSymbol,
	removeSymbol,
	generateFootprint,
	createFootprint,
	copyFootprint,
	editFootprint,
	removeFootprint,
	setLibrary,
];
