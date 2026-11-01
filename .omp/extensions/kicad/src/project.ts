/**
 * A KiCad project: the .kicad_pro, its sheet hierarchy, its board and the files an edit touches.
 *
 * Files load lazily and stay in memory; edits mark them dirty, and `pendingWrites()` returns the
 * new bytes for the transaction to write.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { Board } from "./board";
import { tmpDir } from "./cli";
import { type Config, loadConfig, type ProjectConfig, projectConfig } from "./config";
import { Libraries } from "./libs";
import { type ProJson, parsePro, stringifyPro } from "./projson";
import { type SheetBlock, Sch } from "./sch";
import type { List } from "./sexpr";

export interface SheetInst {
	/** Instance path of uuids: "/<root uuid>" for the root, "/<root uuid>/<sheet uuid>" below. */
	path: string;
	/** Hierarchical name path as in netlists: "/" for the root, "/Power/" below. */
	namePath: string;
	/** Sheet name ("root" for the root sheet). */
	name: string;
	file: string;
	sch: Sch;
	parent?: SheetInst;
	block?: SheetBlock;
}

export interface FoundSymbol {
	sheet: SheetInst;
	sym: List;
}

export class Project {
	readonly files = new Map<string, Sch>();
	/** Files whose in-memory content changed: absolute path -> how to produce the bytes. */
	private dirtyFiles = new Map<string, () => string>();
	readonly created = new Set<string>();
	/** Files to delete when the transaction commits. */
	readonly deleted = new Set<string>();
	readonly proJson: ProJson;
	private libsP?: Promise<Libraries>;
	private boardV?: Board;

	private constructor(
		readonly dir: string,
		readonly name: string,
		readonly repoRoot: string,
		readonly config: Config,
		readonly settings: ProjectConfig,
	) {
		this.proJson = parsePro(fs.readFileSync(this.proFile, "utf8"));
	}

	/**
	 * Open a project from a .kicad_pro path or a directory holding one. With no selector, the only
	 * project listed in .omp/kicad.json is used.
	 */
	static open(selector: string | undefined, repoRoot: string): Project {
		const config = loadConfig(repoRoot);
		let target = selector;
		if (!target) {
			const listed = Object.keys(config.projects);
			if (listed.length !== 1) {
				throw new Error(
					listed.length
						? `several projects in ${config.file}; pass project: one of ${listed.join(", ")}`
						: `no project given and none listed in ${config.file}`,
				);
			}
			target = listed[0]!;
		}
		let p = path.resolve(repoRoot, target);
		if (fs.existsSync(p) && fs.statSync(p).isDirectory()) {
			const pros = fs.readdirSync(p).filter(f => f.endsWith(".kicad_pro"));
			if (pros.length !== 1) throw new Error(`expected one .kicad_pro in ${p}, found ${pros.length}`);
			p = path.join(p, pros[0]!);
		}
		if (!p.endsWith(".kicad_pro") || !fs.existsSync(p)) throw new Error(`no KiCad project at ${p}`);
		const dir = path.dirname(p);
		return new Project(dir, path.basename(p, ".kicad_pro"), repoRoot, config, projectConfig(config, dir));
	}

	get proFile(): string {
		return path.join(this.dir, `${this.name}.kicad_pro`);
	}

	get rootFile(): string {
		return path.join(this.dir, `${this.name}.kicad_sch`);
	}

	get druFile(): string {
		return path.join(this.dir, `${this.name}.kicad_dru`);
	}

	get boardFile(): string {
		return path.join(this.dir, `${this.name}.kicad_pcb`);
	}

	/** The board, loaded once; throws when the project has none. */
	board(): Board {
		if (!this.boardV) {
			if (!fs.existsSync(this.boardFile)) throw new Error(`${this.rel(this.boardFile)} does not exist: create the board in KiCad (File > New Board in the project) first`);
			this.boardV = Board.load(this.boardFile);
		}
		return this.boardV;
	}

	/** Is this file part of the pending writes? */
	isDirty(file: string): boolean {
		return this.dirtyFiles.has(file);
	}

	rel(file: string): string {
		return path.relative(this.repoRoot, file);
	}

	libs(): Promise<Libraries> {
		this.libsP ??= Libraries.load(this.dir, this.repoRoot);
		return this.libsP;
	}

	sch(file: string): Sch {
		const abs = path.resolve(this.dir, file);
		let s = this.files.get(abs);
		if (!s) {
			if (!fs.existsSync(abs)) throw new Error(`sheet file ${this.rel(abs)} does not exist`);
			s = Sch.load(abs);
			this.files.set(abs, s);
		}
		return s;
	}

	addSch(s: Sch): void {
		if (fs.existsSync(s.file) || this.files.has(s.file)) throw new Error(`${this.rel(s.file)} already exists`);
		this.files.set(s.file, s);
		this.created.add(s.file);
		this.touch(s);
	}

	root(): Sch {
		return this.sch(this.rootFile);
	}

	/** All sheet instances, depth first, in file order. */
	hierarchy(): SheetInst[] {
		const root = this.root();
		const out: SheetInst[] = [];
		const walk = (inst: SheetInst, depth: number) => {
			if (depth > 32) throw new Error(`sheet recursion at ${inst.namePath}`);
			out.push(inst);
			for (const b of inst.sch.sheets()) {
				const file = path.resolve(path.dirname(inst.file), b.file);
				walk(
					{
						path: `${inst.path}/${b.uuid}`,
						namePath: `${inst.namePath}${b.name}/`,
						name: b.name,
						file,
						sch: this.sch(file),
						parent: inst,
						block: b,
					},
					depth + 1,
				);
			}
		};
		walk({ path: `/${root.uuid}`, namePath: "/", name: "root", file: this.rootFile, sch: root }, 0);
		return out;
	}

	/** Find a sheet by name, file name (with or without .kicad_sch), name path or uuid path. */
	sheet(selector: string): SheetInst {
		const all = this.hierarchy();
		const s = selector.trim();
		const norm = (x: string) => x.toLowerCase();
		const hits = all.filter(
			i =>
				norm(i.name) === norm(s) ||
				i.namePath === s ||
				i.namePath === `/${s.replace(/^\/|\/$/g, "")}/` ||
				i.path === s ||
				path.basename(i.file) === s ||
				path.basename(i.file, ".kicad_sch") === s ||
				(s === "/" && i.namePath === "/"),
		);
		if (hits.length === 0) {
			throw new Error(`no sheet "${selector}"; sheets: ${all.map(i => `${i.name} (${path.basename(i.file)})`).join(", ")}`);
		}
		if (hits.length > 1) throw new Error(`"${selector}" matches several sheet instances: ${hits.map(h => h.namePath).join(", ")}`);
		return hits[0]!;
	}

	/** A symbol by reference. Multi-unit parts share a reference: this is the first unit (see findUnits). */
	findSymbol(ref: string): FoundSymbol {
		const hit = this.findUnits(ref)[0];
		if (!hit) throw new Error(`no symbol with reference ${ref}`);
		return hit;
	}

	findUnits(ref: string): FoundSymbol[] {
		const hits: FoundSymbol[] = [];
		for (const sheet of this.hierarchy()) {
			for (const sym of sheet.sch.symbols()) if (sheet.sch.refOf(sym, sheet.path) === ref) hits.push({ sheet, sym });
		}
		return hits;
	}

	/** Every reference in use, all sheet instances. */
	allRefs(): Set<string> {
		const out = new Set<string>();
		for (const sheet of this.hierarchy()) for (const sym of sheet.sch.symbols()) out.add(sheet.sch.refOf(sym, sheet.path));
		return out;
	}

	// ---- dirty tracking ----------------------------------------------------------------------

	touch(s: Sch): void {
		this.dirtyFiles.set(s.file, () => s.serialize());
	}

	touchPro(): void {
		this.dirtyFiles.set(this.proFile, () => stringifyPro(this.proJson));
	}

	touchBoard(): void {
		const b = this.board();
		this.dirtyFiles.set(this.boardFile, () => b.serialize());
	}

	/** Mark an arbitrary file (rules, lib table, library) to be written with this content. */
	setFile(file: string, content: string): void {
		if (!fs.existsSync(file)) this.created.add(file);
		this.deleted.delete(file);
		this.dirtyFiles.set(file, () => content);
	}

	/** Delete a file (an unused sheet, a library part) when the transaction commits. */
	deleteFile(file: string): void {
		this.dirtyFiles.delete(file);
		this.files.delete(file);
		if (this.created.delete(file)) return;
		if (!fs.existsSync(file)) throw new Error(`${this.rel(file)} does not exist`);
		this.deleted.add(file);
	}

	/** A file's content as this transaction would leave it: pending bytes, else disk; undefined when absent or deleted. */
	fileText(file: string): string | undefined {
		const make = this.dirtyFiles.get(file);
		if (make) return make();
		if (this.deleted.has(file) || !fs.existsSync(file)) return undefined;
		return fs.readFileSync(file, "utf8");
	}

	pendingWrites(): Map<string, string> {
		const out = new Map<string, string>();
		for (const [f, make] of this.dirtyFiles) out.set(f, make());
		return out;
	}
}

/** In-repo library locations of the project's own lib tables (files or .pretty dirs). */
export function inRepoLibPaths(project: Project, libs: Libraries): string[] {
	const tables = [path.join(project.dir, "sym-lib-table"), path.join(project.dir, "fp-lib-table")];
	return [...libs.sym, ...libs.fp]
		.filter(e => tables.includes(e.table))
		.map(e => e.uri)
		.filter(u => !path.relative(project.repoRoot, u).startsWith("..") && fs.existsSync(u));
}

const SKIP = /(^~.*\.lck$)|(-backups$)|(^\.history$)|(\.kicad_prl$)|(^fp-info-cache$)/;

/** Copy the project, `extra` (its in-repo libraries) and the toolkit config into a temp root, keeping repo-relative paths. */
export function stageCopy(project: Project, extra: string[]): string {
	const root = tmpDir("txn");
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
