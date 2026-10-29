/**
 * "KiCad files are toolkit-only": decides whether a tool call bypasses the toolkit.
 *
 * Blocked: editing or writing KiCad files (schematics, project, rules, libraries, lib tables,
 * board) with any file tool, and shell commands that run a KiCad program or write a KiCad file.
 * A KiCad program counts only at command position (start of a command, after a separator,
 * behind env assignments and wrappers such as sudo/env/exec, or as the command of `-c`/`-exec`),
 * so paths like `.omp/extensions/kicad` or `~/.config/kicad` are not mistaken for it. Reading
 * (read, grep, glob, find, git) stays allowed. Eval cells are not inspected: AGENTS.md and the
 * skills keep agents from touching KiCad files there. The toolkit's own test fixtures are exempt.
 */

const KICAD_FILE = /[\w./~@+-]*?(\.kicad_(?:sch|pro|dru|sym|mod|pcb|prl|wks)\b|(?:^|[\\/\s"'=])(?:sym|fp)-lib-table\b)/g;
const KICAD_PROGS: Record<string, true> = { "kicad-cli": true, kicad: true, eeschema: true, pcbnew: true, "kicad-cmd": true, _kipython: true };
/** Where a new command starts: separators, subshells, groups, substitutions, and the command argument of `sh -c`, `nix develop -c`, `--command`, `find -exec`. */
const COMMAND_START = /\n|;|&&|\|\||[|&(){}`]|\s-[A-Za-z]*c(?=\s)|\s--command(?=\s)|\s-exec(?:dir)?(?=\s)/;
/** Words that run the next word as the command (their own options are skipped). */
const WRAPPERS: Record<string, true> = {
	sudo: true,
	doas: true,
	env: true,
	exec: true,
	command: true,
	builtin: true,
	nohup: true,
	time: true,
	nice: true,
	xargs: true,
	stdbuf: true,
	timeout: true,
	"!": true,
};
const ASSIGNMENT = /^[A-Za-z_]\w*=/;
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

/** Does a command of this shell line run a KiCad program (by name or path, e.g. /nix/store/…/bin/kicad-cli)? */
function runsKicadProgram(cmd: string): boolean {
	for (const segment of cmd.split(COMMAND_START)) {
		const words = segment.trim().split(/\s+/).filter(Boolean);
		let i = 0;
		let wrapped = false;
		// env assignments, wrappers, and the wrappers' options and numbers (timeout 30, nice -n 5)
		while (i < words.length) {
			const w = words[i]!;
			if (ASSIGNMENT.test(w) || Object.hasOwn(WRAPPERS, w)) wrapped = Object.hasOwn(WRAPPERS, w) || wrapped;
			else if (!(wrapped && (w.startsWith("-") || /^\d+[smhd]?$/.test(w)))) break;
			i++;
		}
		const word = words[i]?.replace(/^["'\\]+|["']+$/g, "");
		if (word && Object.hasOwn(KICAD_PROGS, word.slice(word.lastIndexOf("/") + 1))) return true;
	}
	return false;
}

/** The reason to block this call, or undefined to let it run. */
export function blockReason(toolName: string, input: Record<string, unknown>): string | undefined {
	if (Object.hasOwn(READ_ONLY, toolName) || toolName === "eval") return undefined;

	if (toolName === "bash") {
		const cmd = typeof input.command === "string" ? input.command : "";
		const paths = kicadPaths(cmd);
		if (runsKicadProgram(cmd) && !allFixtures(paths)) return BLOCK_REASON;
		if (paths.length && !allFixtures(paths) && SHELL_WRITE.test(cmd) && !/^\s*git\s/.test(cmd)) return BLOCK_REASON;
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
