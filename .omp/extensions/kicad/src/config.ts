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
	return { repoRoot, file, defaults: merge(DEFAULTS, raw.defaults ?? {}), projects: raw.projects ?? {} };
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
	};
}

export function projectConfig(cfg: Config, projectDir: string): ProjectConfig {
	const rel = path.relative(cfg.repoRoot, projectDir);
	return merge(cfg.defaults, cfg.projects[rel] ?? {});
}

