/**
 * "KiCad files are toolkit-only": decides whether a tool call bypasses the toolkit.
 *
 * Blocked: editing or writing KiCad files (schematics, project, rules, libraries, lib tables,
 * board) with any file tool, and shell commands that run a KiCad program or write a KiCad file.
 * Shell lines are split into commands at separators outside quotes; a KiCad program counts only
 * at command position (start of a command, behind env assignments and wrappers such as
 * sudo/env/exec/xargs/timeout), as the command after `-c`/`--command`/`-exec` (a quoted payload
 * there, as of `sh -c '…'`, is read as a shell line), as the argument of `eval`, or inside a
 * command substitution. So paths like `.omp/extensions/kicad`, words in quoted arguments of
 * ordinary commands (`pgrep -f 'kicad|eeschema'`, `grep -E "kicad|eeschema"`, `git commit -m
 * "…"`) and process names given to `pkill`/`killall` (stopping KiCad is not running it) are not
 * mistaken for a run. Reading (read, grep, glob, find, git) stays allowed. Eval cells are not
 * inspected: AGENTS.md and the skills keep agents from touching KiCad files there. The toolkit's
 * own test fixtures are exempt.
 */

const KICAD_FILE = /[\w./~@+-]*?(\.kicad_(?:sch|pro|dru|sym|mod|pcb|prl|wks)\b|(?:^|[\\/\s"'=])(?:sym|fp)-lib-table\b)/g;
const KICAD_PROGS: Record<string, true> = { "kicad-cli": true, kicad: true, eeschema: true, pcbnew: true, "kicad-cmd": true, _kipython: true };
/** Options whose argument is a command: `sh -c`, `bash -lc`, `nix develop -c`, `nix-shell --command/--run`, `find -exec`. */
const COMMAND_OPTION = /^(?:-[A-Za-z]*c|--command|--run|-exec(?:dir)?|-ok(?:dir)?)$/;
/** Commands whose `-c`-like options are not commands (`grep -c`, `wc -c`, `git -c key=value`). */
const PLAIN_TOOLS: Record<string, true> = {
	grep: true,
	egrep: true,
	fgrep: true,
	rg: true,
	pgrep: true,
	pkill: true,
	ps: true,
	wc: true,
	head: true,
	tail: true,
	cut: true,
	sort: true,
	uniq: true,
	ls: true,
	cat: true,
	echo: true,
	printf: true,
	git: true,
	diff: true,
	jq: true,
};
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

/** A shell word with its quotes removed; `quoted` when any part of it was quoted or escaped. */
interface Word {
	text: string;
	quoted: boolean;
}

/** Index of the next `close` after i (or the end of the text). */
function closeAt(src: string, i: number, close: string): number {
	const j = src.indexOf(close, i + 1);
	return j < 0 ? src.length : j;
}

/** Index of the `)` that closes a `$(` whose body starts at i, skipping quoted text. */
function closeParen(src: string, i: number): number {
	let depth = 1;
	for (; i < src.length; i++) {
		const c = src[i];
		if (c === "\\") i++;
		else if (c === "'") i = closeAt(src, i, "'");
		else if (c === '"') i = doubleQuoted(src, i).end;
		else if (c === "(") depth++;
		else if (c === ")" && --depth === 0) return i;
	}
	return src.length;
}

/** The double-quoted string opening at i: its closing index, text, and the bodies of its command substitutions. */
function doubleQuoted(src: string, i: number): { end: number; text: string; subs: string[] } {
	let text = "";
	const subs: string[] = [];
	for (let j = i + 1; j < src.length; j++) {
		const c = src[j]!;
		if (c === '"') return { end: j, text, subs };
		if (c === "\\") {
			const n = src[j + 1] ?? "";
			text += '$`"\\\n'.includes(n) ? n : `\\${n}`;
			j++;
		} else if (c === "$" && src[j + 1] === "(") {
			const e = closeParen(src, j + 2);
			subs.push(src.slice(j + 2, e));
			j = e;
		} else if (c === "`") {
			const e = closeAt(src, j, "`");
			subs.push(src.slice(j + 1, e));
			j = e;
		} else text += c;
	}
	return { end: src.length, text, subs };
}

/**
 * The simple commands of a shell line as words with quotes removed, split at separators outside
 * quotes (newline ; & | ( ) and { } groups); the bodies of command substitutions, `$(…)` and
 * backticks (also inside double quotes), come out as commands of their own. Comments are dropped.
 */
function commands(src: string): Word[][] {
	const out: Word[][] = [];
	let cmd: Word[] = [];
	let word: Word | undefined;
	const add = (text: string, quoted: boolean) => {
		word ??= { text: "", quoted: false };
		word.text += text;
		word.quoted ||= quoted;
	};
	const endWord = () => {
		if (word) cmd.push(word);
		word = undefined;
	};
	const endCommand = () => {
		endWord();
		if (cmd.length) out.push(cmd);
		cmd = [];
	};
	for (let i = 0; i < src.length; i++) {
		const c = src[i]!;
		if (c === "\\") {
			if (src[i + 1] !== "\n") add(src[i + 1] ?? "", true);
			i++;
		} else if (c === "'") {
			const e = closeAt(src, i, "'");
			add(src.slice(i + 1, e), true);
			i = e;
		} else if (c === '"') {
			const d = doubleQuoted(src, i);
			add(d.text, true);
			for (const s of d.subs) out.push(...commands(s));
			i = d.end;
		} else if (c === "$" && src[i + 1] === "(") {
			const e = closeParen(src, i + 2);
			out.push(...commands(src.slice(i + 2, e)));
			add("$(…)", false);
			i = e;
		} else if (c === "$" && src[i + 1] === "{") {
			const e = closeAt(src, i, "}");
			add(src.slice(i, e + 1), false);
			i = e;
		} else if (c === "`") {
			const e = closeAt(src, i, "`");
			out.push(...commands(src.slice(i + 1, e)));
			add("`…`", false);
			i = e;
		} else if (c === "#" && !word) {
			i = closeAt(src, i, "\n") - 1;
		} else if ("\n;&|()".includes(c) || ((c === "{" || c === "}") && !word && /^[\s;]?$/.test(src[i + 1] ?? ""))) {
			endCommand();
		} else if (/\s/.test(c)) {
			endWord();
		} else add(c, false);
	}
	endCommand();
	return out;
}

/** Does this simple command run a KiCad program, itself (by name or path) or through a command option or `eval`? */
function runsKicad(words: Word[]): boolean {
	let i = 0;
	let wrapped = false;
	// env assignments, wrappers, and the wrappers' options and numbers (timeout 30, nice -n 5)
	while (i < words.length) {
		const w = words[i]!.text;
		if (ASSIGNMENT.test(w) || Object.hasOwn(WRAPPERS, w)) wrapped = Object.hasOwn(WRAPPERS, w) || wrapped;
		else if (!(wrapped && (w.startsWith("-") || /^\d+[smhd]?$/.test(w)))) break;
		i++;
	}
	const cmd = words[i]?.text;
	if (!cmd) return false;
	const name = cmd.slice(cmd.lastIndexOf("/") + 1);
	if (Object.hasOwn(KICAD_PROGS, name)) return true;
	const args = words.slice(i + 1);
	if (name === "eval") return runsKicadProgram(args.map(w => w.text).join(" "));
	if (Object.hasOwn(PLAIN_TOOLS, name)) return false;
	// the command option's argument: a shell line when quoted (sh -c '…'), else the command of the rest
	return args.some(
		(w, j) => !w.quoted && COMMAND_OPTION.test(w.text) && j + 1 < args.length && (runsKicadProgram(args[j + 1]!.text) || runsKicad(args.slice(j + 1))),
	);
}

/** Does a command of this shell line run a KiCad program (by name or path, e.g. /nix/store/…/bin/kicad-cli)? */
function runsKicadProgram(line: string): boolean {
	return commands(line).some(runsKicad);
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
