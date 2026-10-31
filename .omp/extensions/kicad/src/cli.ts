/**
 * External programs: kicad-cli and the Python tools from kicad-library-tools.
 *
 * All of them come from the repo's dev shell. When the omp process was not started inside it
 * (no kicad-cli on PATH), the dev shell environment is loaded once with `nix develop` and reused.
 */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { child, children, isList, type List, parse, text } from "./sexpr";

let shellEnv: Promise<Record<string, string>> | undefined;

async function loadEnv(repoRoot: string): Promise<Record<string, string>> {
	const base = { ...process.env } as Record<string, string>;
	if (Bun.which("kicad-cli", { PATH: base.PATH ?? "" }) && base.KICAD_LIBRARY_TOOLS) return base;
	const proc = Bun.spawn(["nix", "develop", repoRoot, "--command", "env", "-0"], {
		cwd: repoRoot,
		stdout: "pipe",
		stderr: "pipe",
	});
	const [out, err, code] = await Promise.all([
		new Response(proc.stdout).text(),
		new Response(proc.stderr).text(),
		proc.exited,
	]);
	if (code !== 0) throw new Error(`could not load the dev shell (nix develop): ${err.trim().slice(-2000)}`);
	const env: Record<string, string> = {};
	for (const kv of out.split("\0")) {
		const i = kv.indexOf("=");
		if (i > 0) env[kv.slice(0, i)] = kv.slice(i + 1);
	}
	return env;
}

/** The dev-shell environment (cached for the process). */
export function devEnv(repoRoot: string): Promise<Record<string, string>> {
	shellEnv ??= loadEnv(repoRoot).catch(e => {
		shellEnv = undefined;
		throw e;
	});
	return shellEnv;
}

export interface RunResult {
	code: number;
	stdout: string;
	stderr: string;
}

export interface RunOpts {
	cwd: string;
	repoRoot: string;
	signal?: AbortSignal;
	env?: Record<string, string>;
}

export async function run(cmd: string[], opts: RunOpts): Promise<RunResult> {
	const env = { ...(await devEnv(opts.repoRoot)), ...opts.env };
	const proc = Bun.spawn(cmd, { cwd: opts.cwd, env, stdout: "pipe", stderr: "pipe", signal: opts.signal });
	const [stdout, stderr, code] = await Promise.all([
		new Response(proc.stdout).text(),
		new Response(proc.stderr).text(),
		proc.exited,
	]);
	return { code, stdout, stderr };
}

async function must(cmd: string[], opts: RunOpts): Promise<RunResult> {
	const r = await run(cmd, opts);
	if (r.code !== 0) {
		throw new Error(`${cmd.slice(0, 3).join(" ")} failed (exit ${r.code}): ${(r.stderr || r.stdout).trim().slice(-2000)}`);
	}
	return r;
}

export function tmpDir(prefix: string): string {
	return fs.mkdtempSync(path.join(os.tmpdir(), `kicad-${prefix}-`));
}

// ---- kicad-cli ------------------------------------------------------------------------------

export interface NetNode {
	ref: string;
	pin: string;
	pinfunction?: string;
	pintype?: string;
}

export interface Net {
	code: number;
	name: string;
	netclass?: string;
	nodes: NetNode[];
}

export interface Component {
	ref: string;
	value: string;
	footprint: string;
	datasheet: string;
	description: string;
	lib: string;
	part: string;
	sheetName: string;
	sheetFile: string;
	/** Sheet instance path of uuids as the netlist writes it: "/" for the root, "/<sheet uuid>/" below. */
	sheetPath: string;
	/** The symbol's uuids, one per unit, first unit first (netlist `tstamps`). */
	uuids: string[];
	/** Footprint filters (`ki_fp_filters`), space separated. */
	fpFilters: string;
	fields: Record<string, string>;
	dnp: boolean;
	excludeFromBom: boolean;
	excludeFromBoard: boolean;
}

export interface Netlist {
	nets: Net[];
	components: Map<string, Component>;
}

/** Text of the `(key "value")` child of l, or "". */
function txt(l: List | undefined, key: string): string {
	return (l && text(child(l, key))) ?? "";
}

export function parseNetlist(source: string): Netlist {
	const t = parse(source);
	const components = new Map<string, Component>();
	for (const c of children(child(t, "components") ?? [], "comp")) {
		const fields: Record<string, string> = {};
		for (const f of children(child(c, "fields") ?? [], "field")) fields[txt(f, "name")] = text(f, 2) ?? "";
		const props: Record<string, string> = {};
		for (const p of children(c, "property")) props[txt(p, "name")] = txt(p, "value");
		const libsource = child(c, "libsource");
		const ref = txt(c, "ref");
		components.set(ref, {
			ref,
			value: txt(c, "value"),
			footprint: txt(c, "footprint"),
			datasheet: txt(c, "datasheet"),
			description: txt(c, "description"),
			lib: txt(libsource, "lib"),
			part: txt(libsource, "part"),
			sheetName: txt(child(c, "sheetpath"), "names"),
			sheetFile: props.Sheetfile ?? "",
			sheetPath: txt(child(c, "sheetpath"), "tstamps"),
			uuids: (child(c, "tstamps") ?? [])
				.slice(1)
				.flatMap(a => (isList(a) ? [] : a.v.split(/\s+/)))
				.filter(Boolean),
			fpFilters: props.ki_fp_filters ?? "",
			fields,
			dnp: "dnp" in props,
			excludeFromBom: "exclude_from_bom" in props,
			excludeFromBoard: "exclude_from_board" in props,
		});
	}
	const nets: Net[] = [];
	for (const n of children(child(t, "nets") ?? [], "net")) {
		nets.push({
			code: Number(txt(n, "code")),
			name: txt(n, "name"),
			netclass: txt(n, "class") || undefined,
			nodes: children(n, "node").map(x => ({
				ref: txt(x, "ref"),
				pin: txt(x, "pin"),
				pinfunction: txt(x, "pinfunction") || undefined,
				pintype: txt(x, "pintype") || undefined,
			})),
		});
	}
	return { nets, components };
}

/** Run a kicad-cli export into a temp file, read it, and remove the temp dir. */
async function exportText(file: string, cmd: (out: string) => string[], o: RunOpts, allowExit = false): Promise<{ text: string; r: RunResult }> {
	const dir = tmpDir("out");
	try {
		const out = path.join(dir, file);
		const r = allowExit ? await run(cmd(out), o) : await must(cmd(out), o);
		if (!fs.existsSync(out)) throw new Error(`${cmd(out).slice(0, 3).join(" ")} wrote no output: ${(r.stderr || r.stdout).trim()}`);
		return { text: fs.readFileSync(out, "utf8"), r };
	} finally {
		fs.rmSync(dir, { recursive: true, force: true });
	}
}

export async function netlist(rootSch: string, o: RunOpts): Promise<Netlist> {
	const { text } = await exportText("out.net", out => ["kicad-cli", "sch", "export", "netlist", "--format", "kicadsexpr", "-o", out, rootSch], o);
	return parseNetlist(text);
}

export interface ErcItem {
	description: string;
	uuid?: string;
	pos?: { x: number; y: number };
}

export interface ErcViolation {
	type: string;
	severity: string;
	description: string;
	sheet: string;
	items: ErcItem[];
	excluded?: boolean;
	/** Comment the user gave when excluding the violation in KiCad. */
	comment?: string;
}

export async function erc(rootSch: string, o: RunOpts): Promise<ErcViolation[]> {
	// ERC exits non-zero when it finds violations; the report is what matters
	const { text } = await exportText("erc.json", out => ["kicad-cli", "sch", "erc", "--format", "json", "--severity-all", "-o", out, rootSch], o, true);
	// kicad-cli's JSON report (erc.v1 schema, coordinate_units "mm") is trusted as KiCad's own output.
	const j = JSON.parse(text) as { sheets: { path: string; violations: Omit<ErcViolation, "sheet">[] }[] };
	return j.sheets.flatMap(s => s.violations.map(v => ({ ...v, sheet: s.path })));
}

/** Export every sheet to SVG in a temp dir; files are named `<project>-<Sheet name>.svg` (root: `<project>.svg`). */
export async function svg(rootSch: string, o: RunOpts): Promise<string> {
	const dir = tmpDir("svg");
	await must(["kicad-cli", "sch", "export", "svg", "-o", dir, rootSch], o);
	return dir;
}

/**
 * Plot one symbol of a library file into `outDir`: one SVG per unit and body style
 * (`<name>_unit<N>[_demorgan | _<body style name>].svg`), each page fitted to that unit.
 */
export async function symSvg(lib: string, name: string, theme: string, outDir: string, o: RunOpts): Promise<void> {
	await must(["kicad-cli", "sym", "export", "svg", "--theme", theme, "--symbol", name, "-o", outDir, lib], o);
}

/** Plot one footprint of a .pretty directory into `outDir/<name>.svg`, the page fitted to the footprint. */
export async function fpSvg(pretty: string, name: string, layers: string[], theme: string, outDir: string, o: RunOpts): Promise<string> {
	// unlike sym export, fp export doesn't create its output directory
	fs.mkdirSync(outDir, { recursive: true });
	const r = await must(
		["kicad-cli", "fp", "export", "svg", "--theme", theme, "--footprint", name, "--layers", layers.join(","), "--sketch-pads-on-fab-layers", "-o", outDir, pretty],
		o,
	);
	// a failed plot still exits 0
	const out = path.join(outDir, `${name}.svg`);
	if (!fs.existsSync(out)) throw new Error(`kicad-cli fp export svg wrote no output: ${(r.stderr || r.stdout).trim().slice(-2000)}`);
	return out;
}

export interface Render3dOpts {
	/** Side of the square image, px. */
	size: number;
	side: string;
	/** Board rotation [x, y, z] in degrees. */
	rotate?: readonly number[];
	zoom: number;
	perspective: boolean;
}

/**
 * Window side for which KiCad's raytracer draws at least `size` px. It draws the centre of the
 * window in 28 px steps (render_3d_raytrace_base.cpp initializeBlockPositions): the largest
 * multiple of 28 below window − 36, plus 32, rounded down to 8.
 */
function raytraceWindow(size: number): number {
	let k = 0;
	while (((28 * k + 32) & ~7) < size) k++;
	return 28 * k + 37;
}

/**
 * Raytrace a board with KiCad's 3D viewer (high quality, opaque background), then crop the
 * centre to exactly `size` × `size`; returns the PNG bytes.
 */
export async function pcbRender(board: string, v: Render3dOpts, o: RunOpts): Promise<Uint8Array> {
	const dir = path.dirname(board);
	const raw = path.join(dir, "raytraced.png");
	const out = path.join(dir, "render.png");
	const win = String(raytraceWindow(v.size));
	const r = await must(
		[
			"kicad-cli", "pcb", "render", "--quality", "high", "--background", "opaque",
			"--width", win, "--height", win, "--side", v.side, "--zoom", String(v.zoom),
			...(v.rotate ? ["--rotate", v.rotate.join(",")] : []),
			...(v.perspective ? ["--perspective"] : []),
			"-o", raw, board,
		],
		o,
	);
	if (!fs.existsSync(raw)) throw new Error(`kicad-cli pcb render wrote no image: ${(r.stderr || r.stdout).trim().slice(-2000)}`);
	await must(["magick", raw, "-gravity", "center", "-crop", `${v.size}x${v.size}+0+0`, "+repage", out], o);
	return fs.readFileSync(out);
}

/** BOM as CSV text (KiCad's own export, optionally through a BOM preset). */
export async function bom(rootSch: string, preset: string | undefined, o: RunOpts): Promise<string> {
	const { text } = await exportText("bom.csv", out => ["kicad-cli", "sch", "export", "bom", "-o", out, ...(preset ? ["--preset", preset] : []), rootSch], o);
	return text;
}

export async function upgrade(kind: "sch" | "sym" | "fp" | "pcb", file: string, o: RunOpts): Promise<void> {
	await must(["kicad-cli", kind, "upgrade", "--force", file], o);
}

/** One item of a DRC violation: KiCad's description ("Pad 2 [GND] of R1 on F.Cu"), position (mm) and uuid. */
export interface DrcItem {
	description: string;
	pos?: { x: number; y: number };
	uuid?: string;
}

export interface DrcViolation {
	type: string;
	severity: string;
	description: string;
	items: DrcItem[];
	excluded?: boolean;
	/** Comment the user gave when excluding the violation in KiCad. */
	comment?: string;
}

export interface DrcReport {
	violations: DrcViolation[];
	unconnected: DrcViolation[];
	/** Schematic parity issues; empty unless asked for. */
	parity: DrcViolation[];
	raw: string;
}

/**
 * KiCad's DRC of a board (JSON report, drc.v1 schema, mm), with the schematic parity test when
 * `parity`; with `refill` the zones are refilled first, in memory (the file is left as it is).
 */
export async function pcbDrc(board: string, o: RunOpts, parity = false, refill = false): Promise<DrcReport> {
	return drcRun(board, o, [...(parity ? ["--schematic-parity"] : []), ...(refill ? ["--refill-zones"] : [])]);
}

/**
 * Refill every zone and save the board in place (`pcb drc --refill-zones --save-board`), as
 * pcbnew's "Fill All Zones" then Save; returns the DRC report of the refilled board.
 */
export async function pcbRefillAndSave(board: string, o: RunOpts, parity = false): Promise<DrcReport> {
	return drcRun(board, o, [...(parity ? ["--schematic-parity"] : []), "--refill-zones", "--save-board"]);
}

/**
 * Every DRC run reports every violation of every track (`--all-track-errors`): KiCad otherwise
 * stops at a track's first one, so a new violation on a track that already had one would go
 * unseen by the edit gate.
 */
async function drcRun(board: string, o: RunOpts, flags: string[]): Promise<DrcReport> {
	const { text, r } = await exportText(
		"drc.json",
		out => ["kicad-cli", "pcb", "drc", "--format", "json", "--severity-all", "--all-track-errors", ...flags, "-o", out, board],
		o,
		true,
	);
	// kicad-cli's JSON report is trusted as KiCad's own output
	const j = JSON.parse(text) as { violations?: DrcViolation[]; unconnected_items?: DrcViolation[]; schematic_parity?: DrcViolation[] };
	return { violations: j.violations ?? [], unconnected: j.unconnected_items ?? [], parity: j.schematic_parity ?? [], raw: r.stdout + r.stderr };
}

/** KiCad's board statistics (`pcb export stats`, JSON, mm), as KiCad writes them. */
export async function pcbStats(board: string, o: RunOpts): Promise<Record<string, unknown>> {
	const { text } = await exportText("stats.json", out => ["kicad-cli", "pcb", "export", "stats", "--format", "json", "--units", "mm", "-o", out, board], o);
	// kicad-cli's JSON report is trusted as KiCad's own output
	return JSON.parse(text) as Record<string, unknown>;
}

/**
 * Plot board layers into one SVG (`--mode-single`), without the drawing sheet. The SVG's user
 * units are board millimetres: a board point (x, y) is drawn at SVG (x, y). `flags` are further
 * kicad-cli plot options (e.g. `--black-and-white`).
 */
export async function pcbSvg(board: string, layers: string[], theme: string, out: string, o: RunOpts, flags: string[] = []): Promise<string> {
	const r = await must(
		[
			"kicad-cli", "pcb", "export", "svg", "--mode-single", "--theme", theme, "--layers", layers.join(","), "--exclude-drawing-sheet", "--page-size-mode", "1",
			"--sketch-pads-on-fab-layers", ...flags, "-o", out, board,
		],
		o,
	);
	if (!fs.existsSync(out)) throw new Error(`kicad-cli pcb export svg wrote no output: ${(r.stderr || r.stdout).trim().slice(-2000)}`);
	return out;
}

/** PDF resolution of svgToPdf (dots per inch). */
export const PDF_DPI = 300;

/**
 * An SVG as a PDF page of the same size, rasterised at PDF_DPI by the dev shell's ImageMagick
 * (librsvg). False, and no file, when the dev shell has no `magick`.
 */
export async function svgToPdf(svg: string, pdf: string, o: RunOpts): Promise<boolean> {
	if (!Bun.which("magick", { PATH: (await devEnv(o.repoRoot)).PATH ?? "" })) return false;
	const r = await must(["magick", "-density", String(PDF_DPI), "-units", "PixelsPerInch", svg, pdf], o);
	if (!fs.existsSync(pdf)) throw new Error(`magick wrote no PDF: ${(r.stderr || r.stdout).trim().slice(-2000)}`);
	return true;
}

/** Root of kicad-library-tools (dev shell env KICAD_LIBRARY_TOOLS). */
export async function libraryTools(repoRoot: string): Promise<string> {
	const env = await devEnv(repoRoot);
	const root = env.KICAD_LIBRARY_TOOLS;
	if (!root || !fs.existsSync(root)) {
		throw new Error("KICAD_LIBRARY_TOOLS is not set: enter the dev shell (flake input kicad-library-tools)");
	}
	return root;
}

// ---- fabrication outputs --------------------------------------------------------------------

/** Files directly in `dir`, sorted. */
function filesIn(dir: string): string[] {
	return fs
		.readdirSync(dir, { withFileTypes: true })
		.filter(e => e.isFile())
		.map(e => path.join(dir, e.name))
		.sort();
}

/**
 * Gerbers of `layers` into `outDir` with kicad-cli's file names: Protel extensions (kicad-cli's
 * default), solder mask subtracted from silkscreen, zones checked and refilled first (in memory),
 * reference and value texts plotted, absolute origin. Returns the files of `outDir`.
 */
export async function pcbGerbers(board: string, layers: string[], outDir: string, o: RunOpts): Promise<string[]> {
	fs.mkdirSync(outDir, { recursive: true });
	await must(["kicad-cli", "pcb", "export", "gerbers", "--layers", layers.join(","), "--subtract-soldermask", "--check-zones", "-o", `${outDir}${path.sep}`, board], o);
	return filesIn(outDir);
}

/**
 * Excellon drill files into `outDir`: absolute origin, mm, decimal zeros, oval holes in the
 * alternate drill mode, plated and non-plated holes in separate files, plus a PDF drill map.
 * Returns the files of `outDir`.
 */
export async function pcbDrill(board: string, outDir: string, o: RunOpts): Promise<string[]> {
	fs.mkdirSync(outDir, { recursive: true });
	await must(
		[
			"kicad-cli", "pcb", "export", "drill", "--format", "excellon", "--drill-origin", "absolute", "--excellon-units", "mm",
			"--excellon-zeros-format", "decimal", "--excellon-oval-format", "alternate", "--excellon-separate-th",
			"--generate-map", "--map-format", "pdf", "-o", `${outDir}${path.sep}`, board,
		],
		o,
	);
	return filesIn(outDir);
}

/**
 * KiCad's footprint position file of both sides as CSV text, mm, page origin: columns Ref, Val,
 * Package, PosX, PosY, Rot, Side; PosY points up (the board's y negated), Rot is the footprint's
 * rotation as pcbnew shows it, Side "top" or "bottom". Footprints excluded from position files are left out.
 */
export async function pcbPos(board: string, o: RunOpts): Promise<string> {
	const { text } = await exportText("pos.csv", out => ["kicad-cli", "pcb", "export", "pos", "--side", "both", "--format", "csv", "--units", "mm", "-o", out, board], o);
	return text;
}

/** The board as a STEP file with its footprints' 3D models (no model substitution), overwriting `out`. */
export async function pcbStep(board: string, out: string, o: RunOpts): Promise<RunResult> {
	const r = await must(["kicad-cli", "pcb", "export", "step", "--force", "-o", out, board], o);
	if (!fs.existsSync(out)) throw new Error(`kicad-cli pcb export step wrote no output: ${(r.stderr || r.stdout).trim().slice(-2000)}`);
	return r;
}
