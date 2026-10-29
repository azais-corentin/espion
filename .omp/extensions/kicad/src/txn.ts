/**
 * kicad_edit transaction.
 *
 * Ops change the project in memory. The new files are then written into a temporary copy of the
 * project (with its in-repo libraries), re-saved there by KiCad (`kicad-cli … upgrade --force`)
 * and checked against the copy's state from before the write:
 *
 * - the connectivity change must equal the declared `expect` exactly,
 * - no new ERC violation,
 * - no new drawing-style finding on the touched sheets,
 * - a changed .kicad_dru must parse (sentinel rule), changed library parts no new KLC violation.
 *
 * Only when every gate passes are KiCad's re-saved bytes copied over the real files, each
 * replaced atomically; if a copy fails midway the files already replaced are restored. The real
 * files never hold unverified content.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "./analysis";
import { ercFindings } from "./checks/connectivity";
import { druProblems } from "./checks/rules";
import { klcFindings } from "./checks/klc";
import { styleFindings } from "./checks/style";
import * as cli from "./cli";
import type { Box } from "./geom";
import type { Libraries } from "./libs";
import { type Expect, checkExpect, diffNetlists, summarize } from "./netdiff";
import { Project, type SheetInst } from "./project";
import { OPS } from "./registry";
import { type LabelledImage, type Rasterize, renderPart, renderRegions } from "./render";
import { SpecError, validate } from "./spec";
import type { Finding, OpContext, PartKind } from "./types";

export interface EditRequest {
	project?: string;
	ops: Record<string, unknown>[];
	expect?: Expect;
	dry_run?: boolean;
	render?: boolean;
}

export interface EditResult {
	applied: boolean;
	report: string;
	images: LabelledImage[];
}

/** KiCad lock files (`~name.ext.lck`) in the project or its in-repo library directories. */
export function findLocks(dirs: string[]): string[] {
	const out: string[] = [];
	for (const d of dirs) {
		if (!fs.existsSync(d) || !fs.statSync(d).isDirectory()) continue;
		for (const f of fs.readdirSync(d)) if (f.startsWith("~") && f.endsWith(".lck")) out.push(path.join(d, f));
	}
	return out;
}

/** In-repo library locations of the project's own lib tables (files or .pretty dirs). */
function inRepoLibPaths(project: Project, libs: Libraries): string[] {
	const tables = [path.join(project.dir, "sym-lib-table"), path.join(project.dir, "fp-lib-table")];
	return [...libs.sym, ...libs.fp]
		.filter(e => tables.includes(e.table))
		.map(e => e.uri)
		.filter(u => !path.relative(project.repoRoot, u).startsWith("..") && fs.existsSync(u));
}

const SKIP = /(^~.*\.lck$)|(-backups$)|(^\.history$)|(\.kicad_prl$)|(^fp-info-cache$)/;

/** Copy the project and its in-repo libraries into a temp root, keeping repo-relative paths. */
function stage(project: Project, extra: string[]): string {
	const root = cli.tmpDir("txn");
	const copy = (src: string) => {
		const dst = path.join(root, path.relative(project.repoRoot, src));
		fs.mkdirSync(path.dirname(dst), { recursive: true });
		fs.cpSync(src, dst, { recursive: true, filter: s => !SKIP.test(path.basename(s)) });
	};
	copy(project.dir);
	for (const p of extra) copy(p);
	if (fs.existsSync(project.config.file)) copy(project.config.file);
	return root;
}

function count(fs_: Finding[]): Map<string, { n: number; f: Finding }> {
	const m = new Map<string, { n: number; f: Finding }>();
	for (const f of fs_) {
		const e = m.get(f.key);
		if (e) e.n++;
		else m.set(f.key, { n: 1, f });
	}
	return m;
}

/** Findings present after and not before (by key, counting repeats). */
export function newFindings(before: Finding[], after: Finding[]): Finding[] {
	const b = count(before);
	const out: Finding[] = [];
	for (const [k, { n, f }] of count(after)) if (n > (b.get(k)?.n ?? 0)) out.push(f);
	return out;
}

function fmtFinding(f: Finding): string {
	return `  - [${f.check}] ${f.sheet ? `${f.sheet} ` : ""}${f.message}${f.at ? ` @ (${f.at[0]}, ${f.at[1]})` : ""}`;
}

export async function runEdit(req: EditRequest, repoRoot: string, rasterize: Rasterize | undefined, signal?: AbortSignal): Promise<EditResult> {
	const project = Project.open(req.project, repoRoot);
	const libs = await project.libs();
	const libPaths = inRepoLibPaths(project, libs);
	const lockDirs = [project.dir, ...new Set(libPaths.map(p => path.dirname(p)))];
	const locks = findLocks(lockDirs);
	if (locks.length) {
		throw new Error(
			`KiCad has the project open (${locks.map(l => project.rel(l)).join(", ")}). ` +
				"Close it in KiCad first: KiCad would overwrite toolkit edits on its next save.",
		);
	}
	if (!Array.isArray(req.ops) || req.ops.length === 0) throw new Error("ops: give at least one op");

	// 1. apply the ops in memory
	const touched = new Map<string, { sheet: SheetInst; boxes: Box[] }>();
	const touchedParts = new Map<string, { kind: PartKind; id: string }>();
	const ctx: OpContext = {
		project,
		libs,
		notes: [],
		touch(sheet, box) {
			const e = touched.get(sheet.namePath) ?? { sheet, boxes: [] };
			if (box) e.boxes.push(box);
			touched.set(sheet.namePath, e);
		},
		touchPart(kind, id) {
			touchedParts.set(`${kind}:${id}`, { kind, id });
		},
	};
	for (const [i, raw] of req.ops.entries()) {
		const name = typeof raw?.op === "string" ? raw.op : "";
		const def = OPS.get(name);
		const where = `ops[${i}] (${name || "?"})`;
		if (!def) throw new SpecError(`${where}: unknown op; ops: ${[...OPS.keys()].join(", ")}`);
		try {
			await def.apply(ctx, validate(where, raw, def.fields, ["op"]));
		} catch (e) {
			const msg = (e as Error).message;
			throw new Error(msg.startsWith(where) ? msg : `${where}: ${msg}`);
		}
	}
	const pending = project.pendingWrites();
	if (pending.size === 0 && project.deleted.size === 0) return { applied: false, report: "No file changes.", images: [] };

	// 2. stage a copy; everything from here on works in it and the copy always goes away
	const tempRoot = stage(project, libPaths);
	try {
		return await verifyAndWrite({ req, project, tempRoot, pending, touched, touchedParts, notes: ctx.notes, lockDirs, rasterize, signal });
	} finally {
		fs.rmSync(tempRoot, { recursive: true, force: true });
	}
}

interface Staged {
	req: EditRequest;
	project: Project;
	tempRoot: string;
	pending: Map<string, string>;
	touched: Map<string, { sheet: SheetInst; boxes: Box[] }>;
	/** Library parts created or changed by the ops, keyed "kind:Lib:Name". */
	touchedParts: Map<string, { kind: PartKind; id: string }>;
	notes: string[];
	lockDirs: string[];
	rasterize: Rasterize | undefined;
	signal?: AbortSignal;
}

/** Take the copy's "before" state, write and re-save the new files there, gate, then commit to the real files. */
async function verifyAndWrite(s: Staged): Promise<EditResult> {
	const { req, project, tempRoot, pending, touched, signal } = s;
	const repoRoot = project.repoRoot;
	const toTemp = (f: string) => path.join(tempRoot, path.relative(repoRoot, f));
	const before = Project.open(path.relative(repoRoot, project.dir), tempRoot);
	const beforeA = new Analysis(before, signal);
	const touchedSheets = (p: Project) => p.hierarchy().filter(sh => touched.has(sh.namePath));
	const libFiles = [...pending.keys()].filter(f => /\.(kicad_sym|kicad_mod)$/.test(f));
	const [netBefore, ercBefore, styleBefore, klcBefore] = await Promise.all([
		beforeA.netlist(),
		beforeA.erc(),
		styleFindings(beforeA, touchedSheets(before).filter(sh => fs.existsSync(sh.file))),
		libFiles.length ? klcFindings(before, libFiles.map(toTemp).filter(f => fs.existsSync(f)), signal) : Promise.resolve([]),
	]).finally(() => beforeA.dispose());

	// 3. write the new files into the copy and let KiCad re-save them
	const ro = { cwd: path.dirname(toTemp(project.rootFile)), repoRoot, signal };
	for (const [f, content] of pending) {
		const t = toTemp(f);
		fs.mkdirSync(path.dirname(t), { recursive: true });
		fs.writeFileSync(t, content);
	}
	for (const f of project.deleted) fs.rmSync(toTemp(f), { force: true });
	// kicad-cli re-saves footprints per .pretty directory only; only pending files are copied back
	const prettyDirs = new Set<string>();
	for (const f of pending.keys()) {
		const t = toTemp(f);
		if (t.endsWith(".kicad_sch")) await cli.upgrade("sch", t, ro);
		else if (t.endsWith(".kicad_sym")) await cli.upgrade("sym", t, ro);
		else if (t.endsWith(".kicad_mod")) prettyDirs.add(path.dirname(t));
	}
	for (const d of prettyDirs) await cli.upgrade("fp", d, ro);

	// 4. the "after" state and the gates
	const afterP = Project.open(path.relative(repoRoot, project.dir), tempRoot);
	const afterA = new Analysis(afterP, signal);
	try {
		const [netAfter, ercAfter, styleAfter, klcAfter, druErrs] = await Promise.all([
			afterA.netlist(),
			afterA.erc(),
			styleFindings(afterA, touchedSheets(afterP)),
			libFiles.length ? klcFindings(afterP, libFiles.map(toTemp), signal) : Promise.resolve([]),
			pending.has(project.druFile) ? druProblems(afterP, signal) : Promise.resolve([]),
		]);
		const diff = diffNetlists(netBefore, netAfter);
		const problems: string[] = [];
		for (const p of checkExpect(diff, req.expect ?? {})) problems.push(`  - [connectivity] ${p}`);
		const ercNew = newFindings(ercFindings(ercBefore), ercFindings(ercAfter));
		const styleNew = newFindings(styleBefore, styleAfter);
		// expected and waived KLC rules come back as info (klcFindings)
		const klcNew = newFindings(klcBefore, klcAfter).filter(f => f.severity !== "info");
		problems.push(...[...ercNew, ...styleNew, ...klcNew].map(fmtFinding));
		problems.push(...druErrs.map(e => `  - [rules] ${e}`));

		const lines: string[] = [];
		if (s.notes.length) lines.push("Done:", ...s.notes.map(n => `  - ${n}`));
		const sum = summarize(diff);
		lines.push("Connectivity and part changes:", ...(sum.length ? sum.map(x => `  - ${x}`) : ["  - none"]));
		const ercResolved = newFindings(ercFindings(ercAfter), ercFindings(ercBefore)).length;
		lines.push(`ERC: ${ercAfter.filter(v => !v.excluded).length} violations after (${ercNew.length} new, ${ercResolved} resolved).`);
		lines.push(
			`Files: ${[
				...[...pending.keys()].map(f => `${project.rel(f)}${project.created.has(f) ? " (new)" : ""}`),
				...[...project.deleted].map(f => `${project.rel(f)} (deleted)`),
			].join(", ")}`,
		);

		// touched sheet regions, then every touched library part (all units), from the verified copy
		const images: LabelledImage[] = [];
		if (req.render && s.rasterize) {
			const sheets = [...touched.values()].filter(t => fs.existsSync(toTemp(t.sheet.file)));
			images.push(...(await renderRegions(afterA, sheets.map(t => ({ sheet: t.sheet.name, boxes: t.boxes })), s.rasterize)));
			const libs = await afterP.libs();
			for (const { kind, id } of s.touchedParts.values()) {
				const img = await renderPart(libs, { kind, id }, s.rasterize, ro);
				images.push({ label: `${kind === "symbol" ? "Symbol" : "Footprint"} ${id}`, png: img.png });
			}
		}

		if (problems.length) {
			return {
				applied: false,
				report: [
					"NOT APPLIED: the result failed these checks (files unchanged):",
					...problems,
					...(klcNew.length
						? [
								"  KLC: if the part breaks a rule on purpose (it really differs, e.g. it has no pin 6), waive that rule for this part only: " +
									'.omp/kicad.json projects.<project>.klcWaivers {"Lib:Name": {"<rule>": "<reason with datasheet page>"}}, then run the edit again. Never add a part-specific rule to klcExpected.',
							]
						: []),
					"",
					...lines,
				].join("\n"),
				images,
			};
		}
		if (req.dry_run) return { applied: false, report: ["Dry run: all checks pass; nothing written.", ...lines].join("\n"), images };

		// 5. copy KiCad's re-saved bytes over the real files
		const late = findLocks(s.lockDirs);
		if (late.length) throw new Error(`KiCad opened the project during the edit (${late.join(", ")}); nothing written.`);
		const written: { file: string; old?: Buffer }[] = [];
		try {
			for (const f of pending.keys()) {
				const bytes = fs.readFileSync(toTemp(f));
				const old = fs.existsSync(f) ? fs.readFileSync(f) : undefined;
				fs.mkdirSync(path.dirname(f), { recursive: true });
				const tmp = `${f}.kicad-toolkit.tmp`;
				fs.writeFileSync(tmp, bytes);
				fs.renameSync(tmp, f);
				written.push({ file: f, old });
			}
			for (const f of project.deleted) {
				const old = fs.readFileSync(f);
				fs.rmSync(f);
				written.push({ file: f, old });
			}
		} catch (e) {
			for (const w of written.reverse()) {
				if (w.old) fs.writeFileSync(w.file, w.old);
				else fs.rmSync(w.file, { force: true });
			}
			throw new Error(`writing the files failed, restored the originals: ${(e as Error).message}`);
		}
		return { applied: true, report: ["Applied.", ...lines].join("\n"), images };
	} finally {
		afterA.dispose();
	}
}
