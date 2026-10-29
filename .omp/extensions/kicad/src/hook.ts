/**
 * "KiCad files are toolkit-only": decides whether a tool call bypasses the toolkit.
 *
 * Blocked: editing or writing KiCad files (schematics, project, rules, libraries, lib tables,
 * board) with any tool, shell commands that run KiCad programs or write KiCad files, and eval
 * cells that touch KiCad files or programs. Reading (read, grep, glob, find, git) stays allowed.
 * The toolkit's own test fixtures are exempt.
 */

const KICAD_FILE = /[\w./~@+-]*?(\.kicad_(?:sch|pro|dru|sym|mod|pcb|prl|wks)\b|(?:^|[\\/\s"'=])(?:sym|fp)-lib-table\b)/g;
const KICAD_PROG = /(?:^|[\s;&|(`$/])(?:kicad-cli|kicad|eeschema|pcbnew|kicad-cmd|_kipython)(?=$|[\s;&|)`'"])/;
const FIXTURES = ".omp/extensions/kicad/test/fixtures/";
const SHELL_WRITE =
	/(?:^|[^-<2&])>{1,2}(?!&)|\bsed\b[^|;&]*\s-[a-zA-Z]*i|\bperl\b[^|;&]*\s-[a-zA-Z]*i|\btee\b|\b(?:cp|mv|rm|dd|truncate|install|ln|rsync|patch|unzip|tar)\b|\b(?:python3?|node|bun|deno|ruby|lua|php)\b|\bawk\b[^|;&]*inplace/;
const READ_ONLY: Record<string, true> = {
	read: true,
	grep: true,
	glob: true,
	find: true,
	lsp: true,
	kicad_query: true,
	kicad_edit: true,
	kicad_check: true,
	kicad_render: true,
	kicad_datasheet: true,
};

export const BLOCK_REASON =
	"KiCad files are toolkit-only in this repo (AGENTS.md): don't edit them, write them or run KiCad programs " +
	"(kicad-cli, kicad, eeschema, pcbnew) directly. Use kicad_query, kicad_edit, kicad_check and kicad_render. " +
	"If they can't do what you need, stop: tell the user what the toolkit is missing and use the ask tool to ask " +
	"whether it should be added to the toolkit.";

function kicadPaths(s: string): string[] {
	return [...s.matchAll(KICAD_FILE)].map(m => m[0]);
}

function allFixtures(paths: string[]): boolean {
	return paths.length > 0 && paths.every(p => p.includes(FIXTURES));
}

function stringsAt(input: Record<string, unknown>, keys: string[]): string[] {
	const out: string[] = [];
	for (const k of keys) {
		const v = input[k];
		if (typeof v === "string") out.push(v);
		else if (Array.isArray(v)) out.push(...v.filter((x): x is string => typeof x === "string"));
	}
	return out;
}

/** Target files of a hashline edit body: `[path#TAG]` header lines. */
function editHeaders(body: string): string[] {
	return [...body.matchAll(/^\[([^\]#\n]+)#[0-9A-Fa-f]{4}\]\s*$/gm)].map(m => m[1]!.trim());
}

/** The reason to block this call, or undefined to let it run. */
export function blockReason(toolName: string, input: Record<string, unknown>): string | undefined {
	if (READ_ONLY[toolName]) return undefined;

	if (toolName === "bash") {
		const cmd = typeof input.command === "string" ? input.command : "";
		const paths = kicadPaths(cmd);
		if (KICAD_PROG.test(cmd) && !allFixtures(paths)) return BLOCK_REASON;
		if (paths.length && !allFixtures(paths) && SHELL_WRITE.test(cmd) && !/^\s*git\s/.test(cmd)) return BLOCK_REASON;
		return undefined;
	}

	if (toolName === "eval") {
		const code = typeof input.code === "string" ? input.code : "";
		const paths = kicadPaths(code);
		if ((paths.length && !allFixtures(paths)) || KICAD_PROG.test(code) || /\bpcbnew\b|\bkipy\b/.test(code)) return BLOCK_REASON;
		return undefined;
	}

	// write, edit, ast_edit, apply_patch, notebook and any other file-writing tool
	const targets = stringsAt(input, ["path", "paths", "file", "files", "target"]);
	if (typeof input.input === "string") targets.push(...editHeaders(input.input));
	if (typeof input.patch === "string") targets.push(...[...input.patch.matchAll(/^\+\+\+ (?:b\/)?(\S+)/gm)].map(m => m[1]!));
	const hits = targets.flatMap(kicadPaths);
	if (hits.length && !allFixtures(hits)) return BLOCK_REASON;
	return undefined;
}
