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
 * - a changed .kicad_dru must parse (sentinel rule), changed library parts no new KLC violation,
 * - a changed board (re-saved with `kicad-cli pcb upgrade --force`; when it has zones, KiCad then
 *   fills them and saves the board, so the committed file carries its fills): no new KiCad DRC
 *   violation and no new schematic parity issue (`pcb drc --schematic-parity`, zones refilled).
 *   Routing in progress (unconnected items, dangling tracks and vias), violations inside
 *   footprints the ops placed from the libraries and those a board-rules-only change brings up
 *   are reported, not gated.
 * - a violation that an ERC or DRC exclusion removed by the edit still hid is an old one shown
 *   again: reported, not gated, and not counted as new or resolved.
 *
 * Only when every gate passes are KiCad's re-saved bytes copied over the real files, each
 * replaced atomically; if a copy fails midway the files already replaced are restored. The real
 * files never hold unverified content. Dry runs and refused edits end with the actual
 * connectivity change as a paste-ready `expect`.
 */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { Analysis } from "./analysis";
import { Board } from "./board";
import { drcFindings, refRenames } from "./checks/board";
import { ercFindings } from "./checks/connectivity";
import { druProblems } from "./checks/rules";
import { klcFindings } from "./checks/klc";
import { styleFindings } from "./checks/style";
import * as cli from "./cli";
import { Copper } from "./copper";
import { type Exclusion, removedExclusions, unexcluded } from "./exclusions";
import { compactJson } from "./format";
import { type Box, bbox, type Pt } from "./geom";
import { type Expect, checkExpect, diffNetlists, expectFromDiff, summarize } from "./netdiff";
import { inRepoLibPaths, Project, type SheetInst, stageCopy } from "./project";
import { OPS } from "./registry";
import { EDIT_BOARD_LAYERS, type LabelledImage, type Rasterize, renderBoard, renderPart, renderRegions } from "./render";
import { child, children, type List, parse } from "./sexpr";
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

/** A running KiCad GUI program of this user. */
export interface KicadProcess {
	pid: number;
	name: string;
}

/** This user's running KiCad GUI programs, or undefined where that can't be told (not Linux). */
export type ProcessLookup = () => KicadProcess[] | undefined;

/**
 * Programs that keep a project open: the project manager and the standalone editors, also as nix
 * wrappers (`.kicad-wrapped`). kicad-cli is left out: it runs a single command and the toolkit
 * runs it on temporary copies, so it never holds a project's lock for long.
 */
const KICAD_GUI = /^\.*(?:kicad|eeschema|pcbnew)(?:-wrapped)*$/;

/** This user's KiCad GUI processes, by executable or argv[0] name from /proc. */
export function kicadProcesses(): KicadProcess[] | undefined {
	if (process.platform !== "linux") return undefined;
	const uid = process.getuid?.();
	const out: KicadProcess[] = [];
	for (const pid of fs.readdirSync("/proc")) {
		if (!/^\d+$/.test(pid)) continue;
		try {
			if (fs.statSync(`/proc/${pid}`).uid !== uid) continue;
			const argv0 = fs.readFileSync(`/proc/${pid}/cmdline`, "utf8").split("\0")[0] ?? "";
			let exe = "";
			try {
				exe = fs.readlinkSync(`/proc/${pid}/exe`).replace(/ \(deleted\)$/, "");
			} catch {
				// kernel threads have no executable
			}
			const name = [exe, argv0].map(p => path.basename(p)).find(n => KICAD_GUI.test(n));
			if (name) out.push({ pid: Number(pid), name });
		} catch {
			// the process ended while being read
		}
	}
	return out;
}

/**
 * KiCad lock files (`~name.ext.lck`) in the project or its in-repo library directories, held or
 * stale. KiCad writes `{"hostname": …, "username": …}` into them and removes them on close; one
 * left behind by a crash or kill is stale: it names this host and user (or nothing readable) and
 * no KiCad GUI program of this user runs. Stale locks are ignored, never deleted.
 */
export function checkLocks(dirs: string[], processes: ProcessLookup): { held: { file: string; why: string }[]; stale: string[] } {
	const files: string[] = [];
	for (const d of dirs) {
		if (!fs.existsSync(d) || !fs.statSync(d).isDirectory()) continue;
		for (const f of fs.readdirSync(d)) if (f.startsWith("~") && f.endsWith(".lck")) files.push(path.join(d, f));
	}
	const held: { file: string; why: string }[] = [];
	const stale: string[] = [];
	if (files.length === 0) return { held, stale };
	const running = processes();
	const user = os.userInfo().username;
	// host names as KiCad writes them (wxGetHostName): without the domain, any case
	const here = os.hostname().split(".")[0]!.toLowerCase();
	for (const file of files) {
		let owner: { hostname?: unknown; username?: unknown } = {};
		try {
			owner = JSON.parse(fs.readFileSync(file, "utf8")) ?? {};
		} catch {
			// empty or unreadable: nobody named
		}
		const host = typeof owner.hostname === "string" ? owner.hostname : "";
		const name = typeof owner.username === "string" ? owner.username : "";
		if ((host && host.split(".")[0]!.toLowerCase() !== here) || (name && name !== user)) held.push({ file, why: `locked by ${name || "?"}@${host || "?"}` });
		else if (running === undefined) held.push({ file, why: `can't check for a running KiCad on ${process.platform}` });
		else if (running.length) held.push({ file, why: `${running.map(p => `${p.name} (pid ${p.pid})`).join(", ")} running` });
		else stale.push(file);
	}
	return { held, stale };
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

/** DRC rules that only say routing is unfinished: reported, never gated. */
const ROUTING: Record<string, true> = { track_dangling: true, via_dangling: true };

/** Unconnected items as findings, keyed by their items (pads, tracks, zones with their nets). */
function unconnectedFindings(r: cli.DrcReport | undefined): Finding[] {
	return (r?.unconnected ?? [])
		.filter(v => !v.excluded)
		.map(v => {
			const items = v.items.map(i => i.description);
			const pos = v.items.find(i => i.pos)?.pos;
			return {
				check: "drc",
				severity: "warning",
				message: `${v.type}: ${v.description}${items.length ? ` [${items.join("; ")}]` : ""}`,
				key: `unconnected|${[...items].sort().join("|")}`,
				...(pos ? { at: [pos.x, pos.y] as const } : {}),
			};
		});
}

/** A DRC report with the excluded violations a removed exclusion really hid shown as not excluded (see unexcluded), and those violations. */
function unhideDrc(r: cli.DrcReport, removed: Exclusion[], after: cli.DrcReport | undefined): { report: cli.DrcReport; shown: cli.DrcReport } {
	const v = unexcluded(r.violations, removed, after?.violations ?? []);
	const p = unexcluded(r.parity, removed, after?.parity ?? []);
	const u = unexcluded(r.unconnected, removed, after?.unconnected ?? []);
	return {
		report: { ...r, violations: v.report, parity: p.report, unconnected: u.report },
		shown: { violations: v.shown, parity: p.shown, unconnected: u.shown, raw: "" },
	};
}

/**
 * The DRC change of a board edit: new violations (gated, routing in progress, or confined to one
 * footprint the edit placed from the libraries), new parity issues, and the report lines. `rename`
 * maps the before board's references to the after board's (footprints matched by uuid), so a
 * renamed reference or net keeps an old violation old.
 */
function boardChange(before: cli.DrcReport | undefined, after: cli.DrcReport, placed: Set<string>, rename: ReadonlyMap<string, string>) {
	const b = before ? drcFindings(before, rename) : { violations: [], parity: [] };
	const a = drcFindings(after);
	const vNew = newFindings(b.violations, a.violations);
	// dangling tracks and vias are routing not finished yet, like unconnected items
	const routing = vNew.filter(f => ROUTING[f.key.split("|")[1] ?? ""]);
	// a footprint fresh from its library brings its own pads and drawings, checked against the board's rules: not this edit's doing;
	// a violation with a board item (edge, track, via, zone) or another footprint is the placement's doing
	const inside = vNew.filter(f => !routing.includes(f) && f.confinedTo !== undefined && placed.has(f.confinedTo));
	const gated = vNew.filter(f => !inside.includes(f) && !routing.includes(f));
	const parityNew = newFindings(b.parity, a.parity);
	const unconnectedNew = newFindings(unconnectedFindings(before), unconnectedFindings(after));
	const list = (l: Finding[]) => [...l.slice(0, 20).map(fmtFinding), ...(l.length > 20 ? [`  - … ${l.length - 20} more`] : [])];
	const report = (gatedRun: boolean): string[] => [
		`DRC: ${a.violations.length} violations after (${vNew.length} new, ${newFindings(a.violations, b.violations).length} resolved).`,
		`Schematic parity: ${a.parity.length} issues after (${parityNew.length} new, ${newFindings(a.parity, b.parity).length} resolved).`,
		`Unconnected items: ${before?.unconnected.length ?? 0} before → ${after.unconnected.length} after (not gated).`,
		...(unconnectedNew.length || routing.length ? ["Routing in progress, new unconnected items and dangling tracks or vias (not gated):", ...list([...unconnectedNew, ...routing])] : []),
		...(inside.length
			? ["New DRC violations inside footprints this edit placed from the libraries (the library footprint against the board's rules; not gated):", ...list(inside)]
			: []),
		...(!gatedRun && gated.length ? ["New DRC violations under the changed board rules (not gated):", ...list(gated)] : []),
	];
	return { gated, parityNew, report };
}

export async function runEdit(
	req: EditRequest,
	repoRoot: string,
	rasterize: Rasterize | undefined,
	signal?: AbortSignal,
	processes: ProcessLookup = kicadProcesses,
): Promise<EditResult> {
	const project = Project.open(req.project, repoRoot);
	const libs = await project.libs();
	const libPaths = inRepoLibPaths(project, libs);
	// a project library may sit in the project directory itself
	const lockDirs = [...new Set([project.dir, ...libPaths.map(p => path.dirname(p))])];
	const locks = checkLocks(lockDirs, processes);
	if (locks.held.length) {
		throw new Error(
			`KiCad has the project open (${locks.held.map(l => `${project.rel(l.file)}: ${l.why}`).join("; ")}). ` +
				"Close it in KiCad first: KiCad would overwrite toolkit edits on its next save.",
		);
	}
	const lockNotes = locks.stale.map(l => `Ignored stale lock ${project.rel(l)}: no KiCad process running.`);
	if (!Array.isArray(req.ops) || req.ops.length === 0) throw new Error("ops: give at least one op");

	// 1. apply the ops in memory
	const touched = new Map<string, { sheet: SheetInst; boxes: Box[] }>();
	const touchedParts = new Map<string, { kind: PartKind; id: string }>();
	const board: BoardTouch = { touched: false, boxes: [], placed: new Set(), nets: new Set(), layers: new Set() };
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
		touchBoard(box, placed, copper) {
			board.touched = true;
			if (box) board.boxes.push(box);
			for (const r of placed ?? []) board.placed.add(r);
			for (const n of copper?.nets ?? []) board.nets.add(n);
			for (const l of copper?.layers ?? []) board.layers.add(l);
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
	if (pending.size === 0 && project.deleted.size === 0) {
		return { applied: false, report: ["No file changes.", ...lockNotes, ...(ctx.notes.length ? ["Notes:", ...ctx.notes.map(n => `  - ${n}`)] : [])].join("\n"), images: [] };
	}

	// 2. stage a copy; everything from here on works in it and the copy always goes away
	const tempRoot = stageCopy(project, libPaths);
	try {
		return await verifyAndWrite({ req, project, tempRoot, pending, touched, touchedParts, board, notes: ctx.notes, lockDirs, lockNotes, processes, rasterize, signal });
	} finally {
		fs.rmSync(tempRoot, { recursive: true, force: true });
	}
}

/** The board side of an edit: whether its design changed, where, which footprints came fresh from the libraries, and which copper changed. */
interface BoardTouch {
	touched: boolean;
	boxes: Box[];
	/** References of footprints the ops placed from the libraries (added or replaced). */
	placed: Set<string>;
	/** Nets and copper layers whose copper the ops changed. */
	nets: Set<string>;
	layers: Set<string>;
}

/** Does the board have zones to fill, its own or its footprints' (rule areas have none)? */
function hasZones(tree: List): boolean {
	return [tree, ...children(tree, "footprint")].some(l => children(l, "zone").some(z => !child(z, "keepout")));
}

interface Staged {
	req: EditRequest;
	project: Project;
	tempRoot: string;
	pending: Map<string, string>;
	touched: Map<string, { sheet: SheetInst; boxes: Box[] }>;
	/** Library parts created or changed by the ops, keyed "kind:Lib:Name". */
	touchedParts: Map<string, { kind: PartKind; id: string }>;
	board: BoardTouch;
	notes: string[];
	lockDirs: string[];
	/** Stale lock files ignored, for the report. */
	lockNotes: string[];
	processes: ProcessLookup;
	rasterize: Rasterize | undefined;
	signal?: AbortSignal;
}

/** Take the copy's "before" state, write and re-save the new files there, gate, then commit to the real files. */
async function verifyAndWrite(s: Staged): Promise<EditResult> {
	const { req, project, tempRoot, pending, touched, signal } = s;
	const repoRoot = project.repoRoot;
	const toTemp = (f: string) => path.join(tempRoot, path.relative(repoRoot, f));
	const ro = { cwd: path.dirname(toTemp(project.rootFile)), repoRoot, signal };
	const before = Project.open(path.relative(repoRoot, project.dir), tempRoot);
	const beforeA = new Analysis(before, signal);
	const touchedSheets = (p: Project) => p.hierarchy().filter(sh => touched.has(sh.namePath));
	const libFiles = [...pending.keys()].filter(f => /\.(kicad_sym|kicad_mod)$/.test(f));
	// KiCad's DRC with schematic parity, when the board's design changes (its file or its rules); zones refilled first, as pcbnew's DRC does
	const boardFile = toTemp(project.boardFile);
	const boardRun = s.board.touched || pending.has(project.boardFile);
	const boardBefore = boardRun && fs.existsSync(boardFile) ? parse(fs.readFileSync(boardFile, "utf8")) : undefined;
	const [netBefore, ercReport, styleBefore, klcBefore, drcReport] = await Promise.all([
		beforeA.netlist(),
		beforeA.erc(),
		styleFindings(beforeA, touchedSheets(before).filter(sh => fs.existsSync(sh.file))),
		libFiles.length ? klcFindings(before, libFiles.map(toTemp).filter(f => fs.existsSync(f)), signal) : Promise.resolve([]),
		boardBefore ? cli.pcbDrc(boardFile, ro, true, hasZones(boardBefore)) : Promise.resolve(undefined),
	]).finally(() => beforeA.dispose());

	// 3. write the new files into the copy and let KiCad re-save them
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
		else if (t.endsWith(".kicad_pcb")) await cli.upgrade("pcb", t, ro);
		else if (t.endsWith(".kicad_mod")) prettyDirs.add(path.dirname(t));
	}
	for (const d of prettyDirs) await cli.upgrade("fp", d, ro);

	// 4. the "after" state and the gates; a changed board with zones gets KiCad's fills saved into it (the bytes committed), with that run's DRC
	const afterZones = pending.has(project.boardFile) ? hasZones(project.board().tree) : !!boardBefore && hasZones(boardBefore);
	const drcAfterRun = (): Promise<cli.DrcReport | undefined> => {
		if (!boardRun || !fs.existsSync(boardFile)) return Promise.resolve(undefined);
		if (pending.has(project.boardFile) && afterZones) return cli.pcbRefillAndSave(boardFile, ro, true);
		return cli.pcbDrc(boardFile, ro, true, afterZones);
	};
	const afterP = Project.open(path.relative(repoRoot, project.dir), tempRoot);
	const afterA = new Analysis(afterP, signal);
	try {
		const [netAfter, ercAfter, styleAfter, klcAfter, druErrs, drcAfter] = await Promise.all([
			afterA.netlist(),
			afterA.erc(),
			styleFindings(afterA, touchedSheets(afterP)),
			libFiles.length ? klcFindings(afterP, libFiles.map(toTemp), signal) : Promise.resolve([]),
			pending.has(project.druFile) ? druProblems(afterP, signal) : Promise.resolve([]),
			drcAfterRun(),
		]);
		// a violation that an exclusion the edit removes still hid is old, shown again: the before-state has it unexcluded, so it neither gates nor counts as resolved;
		// what it hid is what the after-state shows of it beyond the before-state (ERC's re-created marker of a stale exclusion hid nothing)
		const ercUnhidden = unexcluded(ercReport, removedExclusions(before.proJson.data, project.proJson.data, "erc"), ercAfter);
		const ercBefore = ercUnhidden.report;
		const drcUnhidden = drcReport && unhideDrc(drcReport, removedExclusions(before.proJson.data, project.proJson.data, "drc"), drcAfter);
		const drcBefore = drcUnhidden?.report;
		const diff = diffNetlists(netBefore, netAfter);
		const problems: string[] = [];
		for (const p of checkExpect(diff, req.expect ?? {})) problems.push(`  - [connectivity] ${p}`);
		const ercNew = newFindings(ercFindings(ercBefore), ercFindings(ercAfter));
		const styleNew = newFindings(styleBefore, styleAfter);
		// expected and waived KLC rules come back as info (klcFindings)
		const klcNew = newFindings(klcBefore, klcAfter).filter(f => f.severity !== "info");
		problems.push(...[...ercNew, ...styleNew, ...klcNew].map(fmtFinding));
		problems.push(...druErrs.map(e => `  - [rules] ${e}`));
		const rename = boardBefore && drcAfter ? refRenames(boardBefore, parse(fs.readFileSync(boardFile, "utf8"))) : new Map<string, string>();
		const drc = drcAfter ? boardChange(drcBefore, drcAfter, s.board.placed, rename) : undefined;
		// a changed board file is gated; changed board rules alone are reported (tighter rules show real problems to fix)
		if (drc && pending.has(project.boardFile)) problems.push(...[...drc.gated, ...drc.parityNew].map(fmtFinding));

		const lines: string[] = [...s.lockNotes];
		if (s.notes.length) lines.push("Done:", ...s.notes.map(n => `  - ${n}`));
		const sum = summarize(diff);
		lines.push("Connectivity and part changes:", ...(sum.length ? sum.map(x => `  - ${x}`) : ["  - none"]));
		const ercResolved = newFindings(ercFindings(ercAfter), ercFindings(ercBefore)).length;
		lines.push(`ERC: ${ercAfter.filter(v => !v.excluded).length} violations after (${ercNew.length} new, ${ercResolved} resolved).`);
		if (ercUnhidden.shown.length) {
			lines.push("ERC violations shown again by the removed exclusions (not gated; fix them or exclude them again with a reason):", ...ercFindings(ercUnhidden.shown).map(fmtFinding));
		}
		if (drc) lines.push(...drc.report(pending.has(project.boardFile)));
		const shownF = drcAfter && drcUnhidden ? drcFindings(drcUnhidden.shown) : undefined;
		const drcShown = shownF ? [...shownF.violations, ...shownF.parity, ...unconnectedFindings(drcUnhidden!.shown)] : [];
		if (drcShown.length) {
			lines.push("DRC violations shown again by the removed exclusions (not gated; fix them or exclude them again with a reason):", ...drcShown.map(fmtFinding));
		}
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
			if (s.board.touched && fs.existsSync(boardFile)) {
				const box = s.board.boxes.length ? bbox(s.board.boxes.flatMap((b): Pt[] => [[b[0], b[1]], [b[2], b[3]]])) : undefined;
				if (s.board.nets.size || s.board.layers.size) {
					// the touched copper layers, back to front, with the touched nets that still have copper highlighted
					const after = Copper.of(Board.load(boardFile));
					const copper = [...after.copperLayers].reverse().filter(l => s.board.layers.has(l));
					const nets = [...s.board.nets].filter(n => after.hasNet(n));
					const img = await renderBoard(boardFile, { box, layers: ["Edge.Cuts", ...copper, "B.Courtyard", "F.Courtyard"], nets }, s.rasterize, ro);
					images.push({
						label: `Board, region [${img.box.map(v => v.toFixed(2)).join(", ")}] mm (outline, ${copper.join(", ")}, courtyards${nets.length ? `; ${nets.join(", ")} highlighted` : ""})`,
						png: img.png,
					});
				} else {
					const img = await renderBoard(boardFile, { box, layers: EDIT_BOARD_LAYERS }, s.rasterize, ro);
					images.push({ label: `Board, region [${img.box.map(v => v.toFixed(2)).join(", ")}] mm (outline, copper, courtyards)`, png: img.png });
				}
			}
		}

		// the change as made, in `expect`'s input format, for the agent to check and pass back
		const expectBlock = [
			"",
			"Actual connectivity change as `expect` (check it against what the edit is meant to do before passing it; a pin you didn't mean to move is a bug, not something to declare):",
			compactJson(expectFromDiff(diff)),
		];
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
					...expectBlock,
				].join("\n"),
				images,
			};
		}
		if (req.dry_run) return { applied: false, report: ["Dry run: all checks pass; nothing written.", ...lines, ...expectBlock].join("\n"), images };

		// 5. copy KiCad's re-saved bytes over the real files
		const late = checkLocks(s.lockDirs, s.processes).held;
		if (late.length) {
			throw new Error(`KiCad opened the project during the edit (${late.map(l => `${project.rel(l.file)}: ${l.why}`).join("; ")}); nothing written.`);
		}
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
