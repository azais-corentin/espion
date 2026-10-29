/**
 * KiCad s-expressions with explicitly typed atoms.
 *
 * A bare atom (keyword or number) is a `Sym`; a quoted atom is a `Str`. Quoting is decided by
 * the atom's class, never by the content or by object identity, so a value can't silently lose
 * or gain quotes on a round trip.
 */

export class Sym {
	constructor(readonly v: string) {}
}

export class Str {
	constructor(public v: string) {}
}

export type Atom = Sym | Str;
export type List = Node[];
export type Node = Atom | List;

const WS = " \t\r\n";

export function parse(text: string): List {
	const stack: List[] = [[]];
	let i = 0;
	const n = text.length;
	while (i < n) {
		const c = text[i]!;
		if (c === "(") {
			stack.push([]);
			i++;
		} else if (c === ")") {
			const done = stack.pop();
			if (!done || stack.length === 0) throw new Error(`unbalanced ')' at offset ${i}`);
			stack[stack.length - 1]!.push(done);
			i++;
		} else if (WS.includes(c)) {
			i++;
		} else if (c === '"') {
			let j = i + 1;
			let out = "";
			while (j < n && text[j] !== '"') {
				if (text[j] === "\\" && j + 1 < n) {
					const e = text[j + 1]!;
					out += e === "n" ? "\n" : e === "r" ? "\r" : e === "t" ? "\t" : e;
					j += 2;
				} else {
					out += text[j];
					j++;
				}
			}
			if (j >= n) throw new Error(`unterminated string at offset ${i}`);
			stack[stack.length - 1]!.push(new Str(out));
			i = j + 1;
		} else {
			let j = i;
			while (j < n && !WS.includes(text[j]!) && text[j] !== "(" && text[j] !== ")") j++;
			stack[stack.length - 1]!.push(new Sym(text.slice(i, j)));
			i = j;
		}
	}
	if (stack.length !== 1) throw new Error("unbalanced '(': missing ')' at end of input");
	const top = stack[0]!;
	const lists = top.filter(isList);
	if (lists.length !== 1) throw new Error(`expected one top-level list, found ${lists.length}`);
	return lists[0]!;
}

/** Parse a file that holds several top-level lists (e.g. a .kicad_dru rules file). */
export function parseMany(text: string): List[] {
	return parse(`(__root__ ${text})`).slice(1).filter(isList);
}

function quote(s: string): string {
	return `"${s.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n").replace(/\r/g, "\\r")}"`;
}

function atomText(a: Atom): string {
	return a instanceof Str ? quote(a.v) : a.v;
}

/**
 * KiCad-like layout: a list of atoms only goes on one line, a list with child lists puts each
 * child on its own tab-indented line. KiCad re-saves (kicad-cli upgrade) canonicalise the rest.
 */
export function print(node: Node, depth = 0): string {
	if (!isList(node)) return atomText(node);
	const ind = "\t".repeat(depth);
	const atoms: string[] = [];
	let k = 0;
	while (k < node.length && !isList(node[k]!)) atoms.push(atomText(node[k] as Atom)), k++;
	if (k === node.length) return `${ind}(${atoms.join(" ")})`;
	const lines = [`${ind}(${atoms.join(" ")}`];
	for (; k < node.length; k++) {
		const c = node[k]!;
		lines.push(isList(c) ? print(c, depth + 1) : `${ind}\t${atomText(c)}`);
	}
	lines.push(`${ind})`);
	return lines.join("\n");
}

/** One-line rendering, for messages and tests. */
export function inline(node: Node): string {
	return isList(node) ? `(${node.map(inline).join(" ")})` : atomText(node);
}

export function isList(n: Node | undefined): n is List {
	return Array.isArray(n);
}

export function head(l: List): string | undefined {
	const h = l[0];
	return h instanceof Sym ? h.v : undefined;
}

export function child(l: List, key: string): List | undefined {
	for (const c of l) if (isList(c) && head(c) === key) return c;
	return undefined;
}

export function children(l: List, key: string): List[] {
	return l.filter((c): c is List => isList(c) && head(c) === key);
}

/** Text of the atom at index i (keyword or string alike), or undefined. */
export function text(l: List | undefined, i = 1): string | undefined {
	const a = l?.[i];
	return a instanceof Sym || a instanceof Str ? a.v : undefined;
}

export function num(l: List | undefined, i = 1): number {
	const t = text(l, i);
	const v = t === undefined ? Number.NaN : Number(t);
	if (Number.isNaN(v)) throw new Error(`expected a number at ${l ? inline(l) : "<missing>"}[${i}]`);
	return v;
}

export function flag(l: List, key: string): boolean | undefined {
	const c = child(l, key);
	if (!c) return undefined;
	const t = text(c);
	return t === undefined ? true : t === "yes";
}

/** KiCad number formatting: up to 4 decimals, no trailing zeros, no -0. */
export function fmt(v: number): string {
	const s = v.toFixed(4).replace(/\.?0+$/, "");
	return s === "-0" || s === "" ? "0" : s;
}

const KEYWORD = /^[A-Za-z_][A-Za-z0-9_.+-]*$/;

export type Item = Node | number | boolean | string;

/**
 * Build a list. JS numbers become number atoms, booleans become yes/no, bare JS strings must be
 * keywords (a string value must be passed as `str(...)`), so quoting is always explicit.
 */
export function L(key: string, ...items: Item[]): List {
	return [sym(key), ...items.map(toNode)];
}

function toNode(x: Item): Node {
	if (typeof x === "number") return new Sym(fmt(x));
	if (typeof x === "boolean") return new Sym(x ? "yes" : "no");
	if (typeof x === "string") return sym(x);
	return x;
}

export function sym(v: string): Sym {
	if (!KEYWORD.test(v)) throw new Error(`not a keyword: ${JSON.stringify(v)} (use str() for string values)`);
	return new Sym(v);
}

export function str(v: string): Str {
	return new Str(v);
}

/** Replace (or append) the child list with this key. */
export function setChild(l: List, node: List): void {
	const key = head(node)!;
	const i = l.findIndex(c => isList(c) && head(c) === key);
	if (i >= 0) l[i] = node;
	else l.push(node);
}

export function removeChild(l: List, key: string): void {
	const i = l.findIndex(c => isList(c) && head(c) === key);
	if (i >= 0) l.splice(i, 1);
}

export function clone<T extends Node>(n: T): T {
	if (!isList(n)) return (n instanceof Str ? new Str(n.v) : n) as T;
	return n.map(clone) as T;
}
