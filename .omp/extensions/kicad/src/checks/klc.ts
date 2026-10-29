/**
 * KiCad Library Convention checks: kicad-library-utils' `check_symbol.py` / `check_footprint.py`
 * on project library parts, one finding per violated rule per part.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import * as cli from "../cli";
import type { Project } from "../project";
import { children, parse, print, text } from "../sexpr";
import type { CheckDef, Finding } from "../types";

interface Violation {
	component: string;
	rule: string;
	title: string;
	url: string;
	details: string[];
}

function xmlText(s: string): string {
	return s
		.replace(/&lt;/g, "<")
		.replace(/&gt;/g, ">")
		.replace(/&quot;/g, '"')
		.replace(/&apos;/g, "'")
		.replace(/&#(\d+);/g, (_, d: string) => String.fromCodePoint(Number(d)))
		.replace(/&amp;/g, "&");
}

/**
 * Violations from a klc-check JUnit report. Test cases are named "<component> - Errors" /
 * "<component> - Warnings"; each failure is "<rule>: <title>", then the rule URL, then details.
 */
function parseJunit(xml: string, component: (name: string) => string): Violation[] {
	const byKey = new Map<string, Violation>();
	for (const tc of xml.matchAll(/<testcase\b([^>]*?)(?:\/>|>([\s\S]*?)<\/testcase>)/g)) {
		const name = xmlText(/\bname="([^"]*)"/.exec(tc[1]!)?.[1] ?? "");
		const m = /^(.*) - (Errors|Warnings|Info)$/.exec(name);
		if (!m || m[2] === "Info" || !tc[2]) continue;
		const comp = component(m[1]!);
		for (const f of tc[2].matchAll(/<failure message="([^"]*)"[^>]*>([\s\S]*?)<\/failure>/g)) {
			const msg = xmlText(f[1]!);
			const i = msg.indexOf(": ");
			const rule = i > 0 ? msg.slice(0, i) : msg;
			const lines = xmlText(f[2]!)
				.split("\n")
				.map(l => l.trim())
				.filter(Boolean);
			const key = `${rule}|${comp}`;
			const v = byKey.get(key) ?? { component: comp, rule, title: i > 0 ? msg.slice(i + 2) : "", url: "", details: [] };
			v.url ||= lines.find(l => /^https?:\/\//.test(l)) ?? "";
			v.details.push(...lines.slice(1).filter(l => l !== v.url));
			byKey.set(key, v);
		}
	}
	return [...byKey.values()];
}

async function runChecker(project: Project, script: string, args: string[], signal?: AbortSignal): Promise<string> {
	const tools = await cli.libraryTools(project.repoRoot);
	const dir = cli.tmpDir("klc");
	const out = path.join(dir, "junit.xml");
	try {
		const r = await cli.run(["python3", path.join(tools, "library_utils", "klc-check", script), "--nocolor", "--junit", out, ...args], {
			cwd: dir,
			repoRoot: project.repoRoot,
			signal,
		});
		if (!fs.existsSync(out)) throw new Error(`${script} failed (exit ${r.code}): ${(r.stderr || r.stdout).trim().slice(-2000)}`);
		return fs.readFileSync(out, "utf8");
	} finally {
		fs.rmSync(dir, { recursive: true, force: true });
	}
}

/**
 * A directory with one `<nickname>.pretty` link per footprint library of the project (its own
 * and the stock ones), so check_symbol can resolve every default footprint (rule S5.1).
 */
async function footprintDir(project: Project): Promise<string> {
	const libs = await project.libs();
	const dir = cli.tmpDir("klc-fp");
	for (const e of libs.fp) {
		const link = path.join(dir, `${e.name}.pretty`);
		if (fs.existsSync(e.uri) && !fs.existsSync(link)) fs.symlinkSync(e.uri, link);
	}
	return dir;
}

const MIL = 0.0254;
const mmText = (v: number) => String(Number((v * MIL).toFixed(4)) || 0);

/**
 * check_symbol reports positions in mils in KiCad's library orientation (y up): add them in mm in
 * the toolkit's orientation (x right, y down), and for S3.1 the shift that centres the symbol.
 */
export function symbolDetailMm(rule: string, line: string): string {
	const out = line.replace(/@ \((-?\d+(?:\.\d+)?),\s*(-?\d+(?:\.\d+)?)\)/g, (m, x: string, y: string) => `${m} mil = (${mmText(Number(x))}, ${mmText(-Number(y))}) mm`);
	const c = rule === "S3.1" ? /Center calculated @ \((-?\d+(?:\.\d+)?),\s*(-?\d+(?:\.\d+)?)\)/.exec(line) : null;
	if (!c) return out;
	const [x, y] = [Number(c[1]), -Number(c[2])];
	return `${out} (the centre of the body rectangle, or of the pins without one, is at (${mmText(x)}, ${mmText(y)}) mm, x right, y down; moving every pin and shape by (${mmText(-x)}, ${mmText(-y)}) mm centres it)`;
}

/**
 * KLC findings for these .kicad_sym / .kicad_mod files: one per violated rule per part, keyed
 * `klc|<rule>|<Library:Part>`. Rules in the project's `klcExpected`, or waived for that part in
 * `klcWaivers`, are info (with the waiver's reason); others are errors. Symbol positions are given
 * in mils (as the checker prints them) and in mm.
 */
export async function klcFindings(project: Project, files: string[], signal?: AbortSignal): Promise<Finding[]> {
	const syms = files.filter(f => f.endsWith(".kicad_sym"));
	const byDir = new Map<string, string[]>();
	for (const f of files.filter(x => x.endsWith(".kicad_mod"))) byDir.set(path.dirname(f), [...(byDir.get(path.dirname(f)) ?? []), f]);

	const runs: Promise<Violation[]>[] = [];
	if (syms.length) {
		runs.push(
			(async () => {
				const fpDir = await footprintDir(project);
				try {
					const vs = parseJunit(await runChecker(project, "check_symbol.py", ["--footprints", fpDir, ...syms], signal), n => n);
					return vs.map(v => ({ ...v, details: v.details.map(l => symbolDetailMm(v.rule, l)) }));
				} finally {
					fs.rmSync(fpDir, { recursive: true, force: true });
				}
			})(),
		);
	}
	// footprint reports name parts without their library: one run per library directory
	for (const [dir, mods] of byDir) {
		const nick = path.basename(dir, ".pretty");
		runs.push(runChecker(project, "check_footprint.py", mods, signal).then(xml => parseJunit(xml, n => `${nick}:${n}`)));
	}
	const { klcExpected, klcWaivers } = project.settings;
	return (await Promise.all(runs)).flat().map(v => {
		const waiver = klcWaivers[v.component]?.[v.rule];
		const accepted = waiver !== undefined || klcExpected.includes(v.rule);
		return {
			check: "klc",
			severity: accepted ? "info" : "error",
			message: `${v.component} violates KLC ${v.rule} (${v.title})${v.details.length ? `: ${v.details.join("; ")}` : ""}${v.url ? ` ${v.url}` : ""}${waiver !== undefined ? ` [waived: ${waiver}]` : ""}`,
			key: `klc|${v.rule}|${v.component}`,
		};
	});
}

/** The project's own (in-repo) library files: .kicad_sym files and the .kicad_mod files of its .pretty dirs. */
async function projectLibFiles(project: Project): Promise<{ sym: Map<string, string>; mods: Map<string, string> }> {
	const libs = await project.libs();
	const own = (table: string) => (e: { table: string; uri: string }) =>
		e.table === path.join(project.dir, table) && !path.relative(project.repoRoot, e.uri).startsWith("..") && fs.existsSync(e.uri);
	const sym = new Map<string, string>();
	for (const e of libs.sym.filter(own("sym-lib-table"))) sym.set(path.resolve(e.uri), e.name);
	const mods = new Map<string, string>();
	for (const e of libs.fp.filter(own("fp-lib-table"))) {
		for (const f of fs.readdirSync(e.uri).filter(x => x.endsWith(".kicad_mod"))) mods.set(path.join(e.uri, f), `${e.name}:${f.slice(0, -".kicad_mod".length)}`);
	}
	return { sym, mods };
}

async function git(project: Project, args: string[], signal?: AbortSignal): Promise<string> {
	const r = await cli.run(["git", ...args], { cwd: project.repoRoot, repoRoot: project.repoRoot, signal });
	if (r.code !== 0) throw new Error(`git ${args[0]} failed: ${r.stderr.trim()}`);
	return r.stdout;
}

/** Top-level symbols of a library text by name, printed canonically (formatting-independent). */
function symbolTexts(src: string): Map<string, string> {
	return new Map(children(parse(src), "symbol").map(s => [text(s) ?? "", print(s)]));
}

/** Parts ("Lib:Name") of the project libraries whose content differs from `baseline`. */
async function changedParts(project: Project, files: { sym: Map<string, string>; mods: Map<string, string> }, baseline: string, signal?: AbortSignal): Promise<Set<string>> {
	const all = [...files.sym.keys(), ...files.mods.keys()];
	const dirs = [...new Set([...files.sym.keys(), ...[...files.mods.keys()].map(f => path.dirname(f))])].map(p => path.relative(project.repoRoot, p));
	if (!dirs.length) return new Set();
	const [diff, untracked] = await Promise.all([
		git(project, ["diff", "--name-only", "--relative", baseline, "--", ...dirs], signal),
		git(project, ["ls-files", "--others", "--exclude-standard", "--", ...dirs], signal),
	]);
	const changed = new Set(
		[...diff.split("\n"), ...untracked.split("\n")].filter(Boolean).map(f => path.resolve(project.repoRoot, f)),
	);
	const out = new Set<string>();
	for (const f of all.filter(x => changed.has(x))) {
		const part = files.mods.get(f);
		if (part) {
			out.add(part);
			continue;
		}
		const nick = files.sym.get(f)!;
		const now = symbolTexts(fs.readFileSync(f, "utf8"));
		const rel = path.relative(project.repoRoot, f).split(path.sep).join("/");
		const old = await cli.run(["git", "show", `${baseline}:./${rel}`], { cwd: project.repoRoot, repoRoot: project.repoRoot, signal });
		const before = old.code === 0 ? symbolTexts(old.stdout) : new Map<string, string>();
		for (const [name, t] of now) if (before.get(name) !== t) out.add(`${nick}:${name}`);
	}
	return out;
}

/** The info finding naming the parts a run checked. */
function checkedLine(parts: string[], what: string): Finding {
	const sorted = [...parts].sort();
	return {
		check: "klc",
		severity: "info",
		message: `checked ${sorted.length} part${sorted.length === 1 ? "" : "s"} ${what}: ${sorted.join(", ")}`,
		key: "klc|checked",
	};
}

const klc: CheckDef = {
	name: "klc",
	doc:
		"KiCad Library Convention (kicad-library-utils check_symbol / check_footprint) on the project's own library parts changed since the baseline (git; new untracked libraries count as changed), or on all of them with args {all: true}. Rules in klcExpected (all parts) or klcWaivers (one part, with a reason) in .omp/kicad.json are info, others errors. " +
		'Always one info line naming the parts checked ("checked 1 part changed since HEAD: Lib:Name"), or saying no part changed and nothing was checked. Symbol positions are in mils as the checker prints them, then in mm (x right, y down); S3.1 also gives the shift that centres the symbol.',
	byDefault: false,
	async run(ctx) {
		const files = await projectLibFiles(ctx.project);
		const all = [...files.sym.keys(), ...files.mods.keys()];
		if (ctx.args.all === true) {
			const names = [
				...[...files.sym].flatMap(([f, nick]) => [...symbolTexts(fs.readFileSync(f, "utf8")).keys()].map(n => `${nick}:${n}`)),
				...files.mods.values(),
			];
			return [checkedLine(names, "(all project library parts)"), ...(await klcFindings(ctx.project, all, ctx.signal))];
		}
		const parts = await changedParts(ctx.project, files, ctx.baseline, ctx.signal);
		if (!parts.size) {
			return [
				{
					check: "klc",
					severity: "info",
					message: `no project library part changed since ${ctx.baseline} (tracked or untracked): nothing checked; args {all: true} checks every part`,
					key: "klc|nothing",
				},
			];
		}
		const touched = all.filter(f => {
			const part = files.mods.get(f);
			return part ? parts.has(part) : [...parts].some(p => p.startsWith(`${files.sym.get(f)}:`));
		});
		const findings = await klcFindings(ctx.project, touched, ctx.signal);
		return [checkedLine([...parts], `changed since ${ctx.baseline}`), ...findings.filter(f => parts.has(f.key.split("|").slice(2).join("|")))];
	},
};

export const checks: CheckDef[] = [klc];
