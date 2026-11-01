/**
 * KiCad's own view of a project, computed once per request: netlist, ERC report (as is, and with
 * no ERC exclusions for what KiCad actually detects), SVG export and the rendered width of every
 * text string (from the SVG's textLength attributes).
 */
import * as fs from "node:fs";
import * as path from "node:path";
import * as cli from "./cli";
import { exclusionList } from "./exclusions";
import { inRepoLibPaths, type Project, stageCopy } from "./project";
import { parsePro, stringifyPro } from "./projson";

export class Analysis {
	private netP?: Promise<cli.Netlist>;
	private ercP?: Promise<cli.ErcViolation[]>;
	private detectedP?: Promise<cli.ErcViolation[]>;
	private svgP?: Promise<string>;
	private widthP?: Promise<Map<string, number>>;

	constructor(
		readonly project: Project,
		readonly signal?: AbortSignal,
	) {}

	get runOpts(): cli.RunOpts {
		return { cwd: this.project.dir, repoRoot: this.project.repoRoot, signal: this.signal };
	}

	netlist(): Promise<cli.Netlist> {
		this.netP ??= cli.netlist(this.project.rootFile, this.runOpts);
		return this.netP;
	}

	erc(): Promise<cli.ErcViolation[]> {
		this.ercP ??= cli.erc(this.project.rootFile, this.runOpts);
		return this.ercP;
	}

	/**
	 * KiCad's ERC of a temp copy of the project (with its in-repo libraries) whose .kicad_pro has no
	 * ERC exclusions: the violations KiCad detects. The report of `erc()` can't tell: KiCad re-creates
	 * an excluded marker from every exclusion key whose items still resolve, whether or not ERC still
	 * finds the violation (SCHEMATIC::ResolveERCExclusions).
	 */
	ercDetected(): Promise<cli.ErcViolation[]> {
		this.detectedP ??= (async () => {
			const root = stageCopy(this.project, inRepoLibPaths(this.project, await this.project.libs()));
			try {
				const dir = path.join(root, path.relative(this.project.repoRoot, this.project.dir));
				const proFile = path.join(dir, path.basename(this.project.proFile));
				const pro = parsePro(fs.readFileSync(proFile, "utf8"));
				const list = exclusionList(pro.data, "erc");
				if (list?.length) {
					list.length = 0;
					fs.writeFileSync(proFile, stringifyPro(pro));
				}
				return await cli.erc(path.join(dir, path.basename(this.project.rootFile)), { ...this.runOpts, cwd: dir });
			} finally {
				fs.rmSync(root, { recursive: true, force: true });
			}
		})();
		return this.detectedP;
	}

	/** Directory with one SVG per sheet: `<project>.svg` for the root, `<project>-<Sheet name>.svg` below. */
	svgDir(): Promise<string> {
		this.svgP ??= cli.svg(this.project.rootFile, this.runOpts);
		return this.svgP;
	}

	/** Remove the SVG export, once nothing reads it any more. */
	dispose(): void {
		this.svgP?.then(d => fs.rmSync(d, { recursive: true, force: true })).catch(() => undefined);
		this.svgP = undefined;
		this.widthP = undefined;
	}

	async svgFile(sheetName: string): Promise<string> {
		const dir = await this.svgDir();
		const want = sheetName === "root" ? `${this.project.name}.svg` : `${this.project.name}-${sheetName}.svg`;
		const f = path.join(dir, want);
		if (fs.existsSync(f)) return f;
		// kicad-cli replaces characters that can't go in file names; match loosely
		const norm = (s: string) => s.toLowerCase().replace(/[^a-z0-9]/g, "");
		const hit = fs.readdirSync(dir).find(x => norm(x) === norm(want));
		if (!hit) throw new Error(`no SVG for sheet "${sheetName}" in ${dir}: ${fs.readdirSync(dir).join(", ")}`);
		return path.join(dir, hit);
	}

	/** Rendered width (mm) of each text string as KiCad draws it (max over all occurrences). */
	textWidths(): Promise<Map<string, number>> {
		this.widthP ??= (async () => {
			const dir = await this.svgDir();
			const out = new Map<string, number>();
			for (const f of fs.readdirSync(dir).filter(x => x.endsWith(".svg"))) {
				const src = fs.readFileSync(path.join(dir, f), "utf8");
				for (const m of src.matchAll(/<text[^>]*?textLength="([\d.]+)"[^>]*>([^<]*)<\/text>/g)) {
					const t = decodeEntities(m[2]!);
					out.set(t, Math.max(out.get(t) ?? 0, Number(m[1])));
				}
			}
			return out;
		})();
		return this.widthP;
	}
}

function decodeEntities(s: string): string {
	return s
		.replace(/&lt;/g, "<")
		.replace(/&gt;/g, ">")
		.replace(/&quot;/g, '"')
		.replace(/&apos;/g, "'")
		.replace(/&#(\d+);/g, (_, d: string) => String.fromCodePoint(Number(d)))
		.replace(/&#x([0-9a-fA-F]+);/g, (_, h: string) => String.fromCodePoint(Number.parseInt(h, 16)))
		.replace(/&amp;/g, "&");
}

/** Approximate width for strings KiCad hasn't rendered yet: 1.27 mm text, measured average. */
export const CHAR_W = 1.35;
export const TEXT_H = 1.4;
