/**
 * Toolkit settings: `<repo>/.omp/kicad.json`.
 *
 * {
 *   "defaults": { ...ProjectConfig },
 *   "projects": { "hardware/espion-cam": { ...ProjectConfig overrides } }
 * }
 */
import * as fs from "node:fs";
import * as path from "node:path";

export interface StyleConfig {
	/** Straight wire every wired pin needs before any junction, bend or other connection (mm). */
	pinStub: number;
	/** Checks run by the `style` check; any of: pin_stub, power, underline, overlap, off_grid, frame, wiring. */
	rules: string[];
}

export interface FieldsConfig {
	/** Fields every BOM part must carry with a non-empty value. */
	required: string[];
	/** Reference prefixes exempt from the required-field check (e.g. "H" for mounting holes). */
	exemptPrefixes: string[];
}

/** A JLCPCB placement correction for a footprint, applied to its CPL row. */
export interface PlacementCorrection {
	/** Degrees, counter-clockwise seen from the part's top, added to the CPL rotation (default 0). */
	rot?: number;
	/**
	 * [x, y] mm in the footprint's own frame (x right, y down, as the footprint editor shows it):
	 * where JLCPCB's part centre sits relative to the footprint origin (default [0, 0]).
	 */
	offset?: [number, number];
}

export interface AssemblyConfig {
	/**
	 * JLCPCB placement corrections by footprint "Lib:Name": `*`/`?` patterns, case-insensitive,
	 * first match wins (a project's entries before the defaults'). An entry with rot 0 and no
	 * offset records that the footprint was checked in JLCPCB's placement preview and needs none.
	 */
	corrections: Record<string, PlacementCorrection>;
	/**
	 * Parts assembled by hand, by reference, with the reason: {"U6": "hand-soldered module"}. They
	 * are left out of JLCPCB's BOM and CPL, listed in `<board>-hand-assembly.csv` and highlighted in
	 * the assembly drawings (a project's entries over the defaults').
	 */
	hand: Record<string, string>;
}

/**
 * Board layout constraints, checked by kicad_check `layout` (and the two distances and the impedance
 * classes by `decoupling_placement`, `via_return` and `return_path`). Pads are "REF.PAD" ("U6.75"),
 * footprints "REF", nets full names ("/Camera/CSI_D0_P") or `*`/`?` patterns on the full name or
 * the leaf, case-insensitive. Every constraint carries `why` (the reason: a doc or datasheet
 * reference), which its findings quote.
 */
export interface LayoutConfig {
	/** decoupling_placement: a decoupling capacitor's pad-to-power-pin distance above this is a warning (mm, default 3). */
	decouplingMaxDistance: number;
	/** via_return: a signal via of an impedance class needs a ground-net via within this distance (mm, centre to centre, default 1). */
	viaReturnMaxDistance: number;
	/** Net classes return_path and via_return look at (default: every class but Default whose diff-pair width or gap differs from Default's). */
	impedanceClasses: string[];
	/** Part or pad within `max` mm of another (edge to edge of copper; a footprint is its pads). */
	near: { a: string; b: string; max: number; why: string }[];
	/** Routed lengths of these nets within maxSpread of each other (a pair with both nets in the group: their mean); optional maxLength each. */
	lengthGroups: { name: string; nets: string[]; maxSpread: number; maxLength?: number; why: string }[];
	/** Copper path lengths from one pad to each of several pads (star-point branches): each <= maxLength, max - min <= maxDiff. */
	paths: { name: string; from: string; to: string[]; maxLength?: number; maxDiff?: number; why: string }[];
	/** Nets that must have the same via count. */
	viaParity: { name: string; nets: string[]; why: string }[];
	/** Copper area of a net on these layers (zone fills + tracks + pads, mm²) and its via count, optionally only within `within` mm of footprint `near`'s pads. */
	copperArea: { name: string; net: string; layers: string[]; minArea?: number; minVias?: number; near?: string; within?: number; why: string }[];
	/**
	 * Cone keep-outs: a part h mm tall must sit >= r0 + slope*h from center (its courtyard outline's
	 * nearest point). Heights by ref ("J1") or footprint "Lib:Name" pattern; a part closer than
	 * r0 + slope*maxHeight (default: the tallest height given) with no height known is a warning.
	 * Only parts on `side` (default top) count.
	 */
	heightZones: {
		name: string;
		center: [number, number];
		r0: number;
		slope: number;
		heights: Record<string, number>;
		maxHeight?: number;
		side?: "top" | "bottom";
		why: string;
	}[];
}

export interface ProjectConfig {
	/** Connection grid (mm): symbol pins, wire ends and labels must sit on it. */
	grid: number;
	style: StyleConfig;
	fields: FieldsConfig;
	/**
	 * Numbering blocks per reference prefix and sheet name for annotation, e.g.
	 * {"#PWR": {"Power": 300}}: new power symbols on Power get #PWR301, #PWR302, …. A sheet
	 * missing from a prefix's blocks (e.g. a new sheet) gets the next free block past the
	 * highest one. Prefixes without blocks take the next number after the highest in the project.
	 */
	refBase: Record<string, Record<string, number>>;
	/** lib_id per power net name for add_power, e.g. {"+3V3": "power:+3V3"}; default `power:<net>`. */
	powerSymbols: Record<string, string>;
	/** Project library directory (repo-relative) where new libraries are created. */
	libDir: string;
	/**
	 * Appended to the description of every project library the toolkit creates or describes
	 * (lib-table `descr`), e.g. "(espion project library)"; "" for none.
	 */
	libraryDescriptionSuffix: string;
	/** KLC rule ids expected on every project part (reported as info, not failures), e.g. "F9.3". */
	klcExpected: string[];
	/**
	 * KLC rules waived for one part, with the reason: {"Lib:Name": {"S4.5": "no pin 6 (datasheet p. 2)"}}.
	 * Use this, not klcExpected, for a rule that only this part breaks on purpose.
	 */
	klcWaivers: Record<string, Record<string, string>>;
	/** Pin/pad pairs exempt from the footprint pin-pad match, as "REF" or "REF.pin". */
	padMatchExempt: string[];
	/** References exempt from one required field, e.g. {"LCSC": ["U1"]} for a part bought elsewhere. */
	fieldExempt: Record<string, string[]>;
	/** Ground nets for the decoupling and pulls checks: `*`/`?` patterns on the net's leaf name, case-insensitive. */
	groundNets: string[];
	/** Power pins that need no decoupling capacitor, with the reason: {"U3": "…"} or {"U3.7": "…"}. */
	decouplingExempt: Record<string, string>;
	/** Nets that need a pull-up resistor to a rail: `*`/`?` patterns on the net's leaf name, case-insensitive. */
	pullUpNets: string[];
	/** Nets exempt from the pulls check, by net name or leaf name, with the reason. */
	pullsExempt: Record<string, string>;
	/** Fabrication outputs (kicad_export assembly). */
	assembly: AssemblyConfig;
	/** Board layout constraints (kicad_check layout and the board review checks). */
	layout: LayoutConfig;
}

export interface Config {
	repoRoot: string;
	file: string;
	defaults: ProjectConfig;
	projects: Record<string, Partial<ProjectConfig>>;
}

export const DEFAULTS: ProjectConfig = {
	grid: 1.27,
	style: { pinStub: 1.27, rules: ["pin_stub", "power", "underline", "overlap", "off_grid", "frame", "wiring"] },
	fields: { required: [], exemptPrefixes: ["#"] },
	refBase: {},
	powerSymbols: {},
	libDir: "lib",
	libraryDescriptionSuffix: "",
	klcExpected: [],
	klcWaivers: {},
	padMatchExempt: [],
	fieldExempt: {},
	groundNets: ["GND*", "*GND", "VSS*"],
	decouplingExempt: {},
	pullUpNets: ["*SDA*", "*SCL*", "*MDIO*"],
	pullsExempt: {},
	assembly: { corrections: {}, hand: {} },
	layout: {
		decouplingMaxDistance: 3,
		viaReturnMaxDistance: 1,
		impedanceClasses: [],
		near: [],
		lengthGroups: [],
		paths: [],
		viaParity: [],
		copperArea: [],
		heightZones: [],
	},
};

/** Walk up from `start` to the directory holding `.omp` or `.git`. */
export function findRepoRoot(start: string): string {
	let d = path.resolve(start);
	for (;;) {
		if (fs.existsSync(path.join(d, ".omp")) || fs.existsSync(path.join(d, ".git"))) return d;
		const up = path.dirname(d);
		if (up === d) return path.resolve(start);
		d = up;
	}
}

export function loadConfig(repoRoot: string): Config {
	const file = path.join(repoRoot, ".omp", "kicad.json");
	let raw: { defaults?: Partial<ProjectConfig>; projects?: Record<string, Partial<ProjectConfig>> } = {};
	if (fs.existsSync(file)) {
		try {
			raw = JSON.parse(fs.readFileSync(file, "utf8"));
		} catch (e) {
			throw new Error(`${file}: ${(e as Error).message}`);
		}
	}
	return { repoRoot, file, defaults: merge(DEFAULTS, checked(raw.defaults ?? {}, `${file}: defaults`)), projects: raw.projects ?? {} };
}

function merge(a: ProjectConfig, b: Partial<ProjectConfig>): ProjectConfig {
	return {
		...a,
		...b,
		style: { ...a.style, ...b.style },
		fields: { ...a.fields, ...b.fields },
		refBase: { ...a.refBase, ...b.refBase },
		powerSymbols: { ...a.powerSymbols, ...b.powerSymbols },
		fieldExempt: { ...a.fieldExempt, ...b.fieldExempt },
		klcWaivers: { ...a.klcWaivers, ...b.klcWaivers },
		decouplingExempt: { ...a.decouplingExempt, ...b.decouplingExempt },
		pullsExempt: { ...a.pullsExempt, ...b.pullsExempt },
		assembly: {
			...a.assembly,
			...b.assembly,
			// the project's patterns first: first match wins
			corrections: { ...b.assembly?.corrections, ...a.assembly.corrections, ...b.assembly?.corrections },
			hand: { ...a.assembly.hand, ...b.assembly?.hand },
		},
		layout: { ...a.layout, ...b.layout },
	};
}

export function projectConfig(cfg: Config, projectDir: string): ProjectConfig {
	const rel = path.relative(cfg.repoRoot, projectDir);
	return merge(cfg.defaults, checked(cfg.projects[rel] ?? {}, `${cfg.file}: projects["${rel}"]`));
}

/** A config part with its layout section and hand-assembly list validated (throws naming the bad entry). */
function checked(c: Partial<ProjectConfig>, where: string): Partial<ProjectConfig> {
	if (c.layout !== undefined) validateLayout(c.layout, `${where}.layout`);
	if (c.assembly?.hand !== undefined) validateHand(c.assembly.hand, `${where}.assembly.hand`);
	return c;
}

/** Throws on a malformed `assembly.hand`, naming the entry: an object of reference → non-empty reason. */
export function validateHand(raw: unknown, where: string): void {
	if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error(`${where}: give an object {"<reference>": "<reason>"}, got ${JSON.stringify(raw)}`);
	for (const [ref, reason] of Object.entries(raw)) {
		const at = `${where}[${JSON.stringify(ref)}]`;
		if (!/^[^\s.*?]+$/.test(ref)) throw new Error(`${at}: give a footprint reference ("U6"), not a pattern, pad or blank`);
		if (typeof reason !== "string" || !reason.trim()) throw new Error(`${at}: give the reason as a non-empty string, got ${JSON.stringify(reason)}`);
	}
}

type FieldKind = "string" | "strings" | "number" | "count" | "point" | "heights" | "side" | "pad" | "pads";

/** Fields of each layout list's entries: kind and whether required. */
const LAYOUT_LISTS: Record<string, Record<string, [FieldKind, boolean]>> = {
	near: { a: ["string", true], b: ["string", true], max: ["number", true], why: ["string", true] },
	lengthGroups: { name: ["string", true], nets: ["strings", true], maxSpread: ["number", true], maxLength: ["number", false], why: ["string", true] },
	paths: { name: ["string", true], from: ["pad", true], to: ["pads", true], maxLength: ["number", false], maxDiff: ["number", false], why: ["string", true] },
	viaParity: { name: ["string", true], nets: ["strings", true], why: ["string", true] },
	copperArea: {
		name: ["string", true],
		net: ["string", true],
		layers: ["strings", true],
		minArea: ["number", false],
		minVias: ["count", false],
		near: ["string", false],
		within: ["number", false],
		why: ["string", true],
	},
	heightZones: {
		name: ["string", true],
		center: ["point", true],
		r0: ["number", true],
		slope: ["number", true],
		heights: ["heights", true],
		maxHeight: ["number", false],
		side: ["side", false],
		why: ["string", true],
	},
};

const PAD_ID = /^[^.\s]+\.[^\s]+$/;

function fieldProblem(kind: FieldKind, v: unknown): string | undefined {
	const mm = (x: unknown) => typeof x === "number" && Number.isFinite(x) && x >= 0;
	switch (kind) {
		case "string":
			return typeof v === "string" && v.trim() ? undefined : "give a non-empty string";
		case "strings":
			return Array.isArray(v) && v.length && v.every(s => typeof s === "string" && s.trim()) ? undefined : "give a non-empty list of strings";
		case "number":
			return mm(v) ? undefined : "give a number >= 0 (mm)";
		case "count":
			return mm(v) && Number.isInteger(v) ? undefined : "give a whole number >= 0";
		case "point":
			return Array.isArray(v) && v.length === 2 && v.every(x => typeof x === "number" && Number.isFinite(x)) ? undefined : "give [x, y] in board mm";
		case "heights":
			return typeof v === "object" && v !== null && !Array.isArray(v) && Object.values(v).every(mm) ? undefined : 'give {"REF" or "Lib:Name pattern": height mm >= 0}';
		case "side":
			return v === "top" || v === "bottom" ? undefined : 'give "top" or "bottom"';
		case "pad":
			return typeof v === "string" && PAD_ID.test(v) ? undefined : 'give a pad as "REF.PAD", e.g. "U6.75"';
		case "pads":
			return Array.isArray(v) && v.length && v.every(s => typeof s === "string" && PAD_ID.test(s)) ? undefined : 'give a non-empty list of pads as "REF.PAD"';
	}
}

/** Throws on the first bad layout setting, naming the entry ("….layout.near[1] (C3 – U1): max: …"). */
export function validateLayout(raw: unknown, where: string): void {
	if (typeof raw !== "object" || raw === null || Array.isArray(raw)) throw new Error(`${where}: give an object`);
	const known = Object.keys(DEFAULTS.layout);
	for (const [k, v] of Object.entries(raw)) {
		if (!known.includes(k)) throw new Error(`${where}: unknown setting "${k}" (settings: ${known.join(", ")})`);
		if (k === "decouplingMaxDistance" || k === "viaReturnMaxDistance") {
			if (!(typeof v === "number" && v > 0)) throw new Error(`${where}.${k}: give a number > 0 (mm), got ${JSON.stringify(v)}`);
			continue;
		}
		if (k === "impedanceClasses") {
			if (!Array.isArray(v) || !v.every(s => typeof s === "string" && s.trim())) throw new Error(`${where}.${k}: give a list of net class names`);
			continue;
		}
		if (!Array.isArray(v)) throw new Error(`${where}.${k}: give a list of entries`);
		const fields = LAYOUT_LISTS[k]!;
		v.forEach((e: unknown, i) => {
			const o = typeof e === "object" && e !== null && !Array.isArray(e) ? (e as Record<string, unknown>) : undefined;
			const label = typeof o?.name === "string" ? ` (${o.name})` : typeof o?.a === "string" ? ` (${o.a} – ${String(o.b)})` : "";
			const fail = (msg: string) => new Error(`${where}.${k}[${i}]${label}: ${msg}`);
			if (!o) throw fail("give an object");
			for (const f of Object.keys(o)) if (!(f in fields)) throw fail(`unknown field "${f}" (fields: ${Object.keys(fields).join(", ")})`);
			for (const [f, [kind, req]] of Object.entries(fields)) {
				if (o[f] === undefined) {
					if (req) throw fail(`${f} is required`);
					continue;
				}
				const p = fieldProblem(kind, o[f]);
				if (p) throw fail(`${f}: ${p}, got ${JSON.stringify(o[f])}`);
			}
			if (k === "paths" && o.maxLength === undefined && o.maxDiff === undefined) throw fail("give maxLength, maxDiff or both");
			if (k === "copperArea" && o.minArea === undefined && o.minVias === undefined) throw fail("give minArea, minVias or both");
			if (k === "copperArea" && (o.near === undefined) !== (o.within === undefined)) throw fail("near and within go together");
			if (k === "heightZones" && !Object.keys(o.heights as object).length && o.maxHeight === undefined) throw fail("give heights or maxHeight");
		});
	}
}

