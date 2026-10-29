/**
 * KiCad's ERC (`erc`) and the connectivity change against a git baseline (`connectivity`).
 *
 * The baseline netlist comes from KiCad itself: the project and its in-repo libraries are
 * exported at the baseline ref with `git archive` into a temp dir and netlisted there, so the
 * comparison is KiCad's view of both states.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { type ErcViolation, type Netlist, netlist, run, tmpDir } from "../cli";
import { diffNetlists } from "../netdiff";
import type { Project } from "../project";
import type { CheckDef, Finding } from "../types";
import { sheetOfPath } from "./parts";

/** Findings for KiCad's ERC violations that are not excluded in KiCad (keys without coordinates). */
export function ercFindings(v: ErcViolation[]): Finding[] {
	return v
		.filter(x => !x.excluded)
		.map(x => ({
			check: "erc",
			severity: x.severity === "error" ? "error" : "warning",
			message: `${x.type}: ${x.description} [${x.items.map(i => i.description).join("; ")}]`,
			key: `erc|${x.type}|${x.items.map(i => i.description).sort().join("|")}`,
			sheet: x.sheet,
			at: x.items[0]?.pos ? [x.items[0].pos.x, x.items[0].pos.y] : undefined,
		}));
}

const erc: CheckDef = {
	name: "erc",
	doc: "KiCad's ERC, one finding per violation with KiCad's severity and sheet path; violations excluded in KiCad are listed as info with their exclusion comment.",
	byDefault: true,
	async run(ctx) {
		const violations = await ctx.analysis.erc();
		const excluded = violations.filter(v => v.excluded);
		const findings = [
			...ercFindings(violations),
			...ercFindings(excluded.map(v => ({ ...v, excluded: false }))).map((f, i): Finding => {
				const v = excluded[i]!;
				const comment = v.comment ? ` (${v.comment})` : "";
				return { ...f, severity: "info", message: `excluded in KiCad${comment}: ${f.message}` };
			}),
		];
		if (!ctx.sheets) return findings;
		const paths = new Set(ctx.sheets.map(s => s.namePath));
		return findings.filter(f => f.sheet !== undefined && paths.has(f.sheet));
	},
};

/** In-repo library locations (files or .pretty dirs) of the project's own lib tables, relative to `top`. */
async function libPaths(project: Project, top: string): Promise<string[]> {
	const libs = await project.libs();
	const tables = [path.join(project.dir, "sym-lib-table"), path.join(project.dir, "fp-lib-table")];
	return [...libs.sym, ...libs.fp]
		.filter(e => tables.includes(e.table))
		.map(e => path.relative(top, e.uri))
		.filter(r => r && !r.startsWith("..") && !path.isAbsolute(r));
}

/** KiCad's netlist of the project as it was at `ref`. */
async function baselineNetlist(project: Project, ref: string, signal?: AbortSignal): Promise<Netlist> {
	const opts = { cwd: project.dir, repoRoot: project.repoRoot, signal };
	const topR = await run(["git", "rev-parse", "--show-toplevel"], opts);
	if (topR.code !== 0) throw new Error(`a baseline needs git: ${project.dir} is not in a git repository`);
	const top = topR.stdout.trim();
	const at = { ...opts, cwd: top };
	const sha = await run(["git", "rev-parse", "--verify", "--quiet", `${ref}^{commit}`], at);
	if (sha.code !== 0) throw new Error(`baseline "${ref}" is not a commit, branch or tag in ${top}`);
	const commit = sha.stdout.trim();
	const projRel = path.relative(top, project.dir) || ".";
	const wanted = [projRel, ...(await libPaths(project, top))];
	const listed = await run(["git", "ls-tree", "--name-only", commit, "--", ...wanted], at);
	const present = new Set(listed.stdout.split("\n").filter(Boolean));
	if (projRel !== "." && !present.has(projRel)) throw new Error(`${projRel} does not exist at ${ref}`);
	const dir = tmpDir("base");
	try {
		const tar = path.join(dir, "base.tar");
		const arch = await run(["git", "archive", "--format=tar", "-o", tar, commit, "--", ...wanted.filter(w => w === "." || present.has(w))], at);
		if (arch.code !== 0) throw new Error(`git archive ${ref} failed: ${arch.stderr.trim()}`);
		const x = await run(["tar", "-x", "-f", tar, "-C", dir], at);
		if (x.code !== 0) throw new Error(`unpacking ${ref} failed: ${x.stderr.trim()}`);
		const cfgRel = path.relative(top, project.config.file);
		const cfgOut = path.join(dir, cfgRel);
		fs.mkdirSync(path.dirname(cfgOut), { recursive: true });
		const cfg = await run(["git", "show", `${commit}:${cfgRel}`], at);
		if (cfg.code === 0) fs.writeFileSync(cfgOut, cfg.stdout);
		else if (fs.existsSync(project.config.file)) fs.copyFileSync(project.config.file, cfgOut);
		const root = path.join(dir, projRel, `${project.name}.kicad_sch`);
		if (!fs.existsSync(root)) throw new Error(`${project.rel(project.rootFile)} does not exist at ${ref}`);
		return await netlist(root, { cwd: path.dirname(root), repoRoot: project.repoRoot, signal });
	} finally {
		fs.rmSync(dir, { recursive: true, force: true });
	}
}

const connectivity: CheckDef = {
	name: "connectivity",
	doc: "Netlist now vs the baseline git ref (default HEAD): added/removed parts, renamed and changed nets, disconnected pins, value/footprint/field/DNP changes (info); one info finding when nothing changed.",
	byDefault: true,
	async run(ctx) {
		const ref = ctx.baseline;
		const [before, after] = await Promise.all([baselineNetlist(ctx.project, ref, ctx.signal), ctx.analysis.netlist()]);
		const d = diffNetlists(before, after);
		const out: Finding[] = [];
		const add = (f: Omit<Finding, "check" | "severity">) => out.push({ check: "connectivity", severity: "info", ...f });
		for (const r of d.addedParts) {
			const c = after.components.get(r)!;
			add({ message: `part ${r} (${c.value}) added since ${ref}`, key: `connectivity|added|${r}`, refs: [r], sheet: sheetOfPath(c.sheetName) });
		}
		for (const r of d.removedParts) {
			const c = before.components.get(r)!;
			add({ message: `part ${r} (${c.value}) removed since ${ref}`, key: `connectivity|removed|${r}`, refs: [r], sheet: sheetOfPath(c.sheetName) });
		}
		for (const r of d.renamed) {
			add({ message: `net ${r.from} renamed to ${r.to} (${r.pins.length} pins)`, key: `connectivity|renamed|${r.from}|${r.to}`, net: r.to });
		}
		for (const c of d.changed) {
			const was = d.gone.filter(g => g.pins.some(p => c.pins.includes(p)));
			add({
				message:
					`net ${c.name} is now [${c.pins.join(", ")}]` +
					(was.length ? `; at ${ref}: ${was.map(g => `${g.name} [${g.pins.join(", ")}]`).join("; ")}` : " (new net)"),
				key: `connectivity|net|${c.name}`,
				net: c.name,
			});
		}
		for (const p of d.disconnected) {
			const was = d.gone.find(g => g.pins.includes(p));
			add({
				message: `pin ${p} is no longer connected (was on ${was?.name ?? "a net"} at ${ref})`,
				key: `connectivity|disconnected|${p}`,
				refs: [p.slice(0, p.lastIndexOf("."))],
			});
		}
		for (const f of d.fieldChanges) {
			add({ message: `${f.ref} ${f.field}: "${f.from}" -> "${f.to}"`, key: `connectivity|field|${f.ref}|${f.field}`, refs: [f.ref] });
		}
		if (out.length === 0) add({ message: `connectivity unchanged since ${ref}`, key: "connectivity|unchanged" });
		return out;
	},
};

export const checks: CheckDef[] = [erc, connectivity];
