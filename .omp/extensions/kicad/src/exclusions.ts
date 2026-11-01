/**
 * ERC and DRC exclusions as KiCad 10 keeps them in the .kicad_pro: `erc.erc_exclusions` and
 * `board.design_settings.drc_exclusions`, each entry a [key, comment] pair (a bare key string in
 * older files). The key is the marker KiCad serialized when the violation was excluded
 * (SCH_MARKER / PCB_MARKER::SerializeToString), "|"-separated:
 *
 * - ERC: rule | x | y | main item uuid | aux item uuid | sheet-specific path | main item sheet path |
 *   aux item sheet path, in schematic internal units (100 nm). generic_warning, generic_error and
 *   unresolved_variable on a field or library text name the parent's uuid and the text instead.
 * - DRC: rule | x | y | main item uuid | aux item uuid in board internal units (1 nm), with
 *   per-rule variants: copper_sliver, generic_warning, generic_error (main uuid | layer),
 *   unconnected_items (layer | marker type | main | aux), starved_thermal (main | aux | layer).
 *
 * KiCad's ERC and DRC reports mark the violations an exclusion matches as excluded but don't say
 * which exclusion: a violation belongs to an exclusion with the same rule whose item uuids it
 * all carries (by position when the key names no item).
 *
 * KiCad's ERC report also holds a violation for every exclusion whose items still resolve, found
 * or not: SCHEMATIC::ResolveERCExclusions re-creates the excluded marker from the key when no
 * marker of the run serializes to it. In the report such a marker looks like a detected one (same
 * rule, message and items), so only an ERC run without the exclusions tells what KiCad detects.
 * pcbnew's DRC report has no such markers.
 */
import type { ErcViolation } from "./cli";
import type { Pt } from "./geom";

export type ExclusionKind = "erc" | "drc";

export const EXCLUSION_KINDS: readonly ExclusionKind[] = ["erc", "drc"];

const NIL_UUID = "00000000-0000-0000-0000-000000000000";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export interface Exclusion {
	kind: ExclusionKind;
	/** Index in the .kicad_pro list. */
	index: number;
	key: string;
	comment: string;
	/** KiCad's rule name (the ERC/DRC report's violation type). */
	rule: string;
	/** Marker position: sheet mm (ERC) or board mm (DRC). */
	at: Pt;
	/** Item uuids the key names (nil uuids left out). */
	items: string[];
	/** DRC: the marker's layer, for the rules whose key carries one. */
	layer?: string;
	/** ERC: the text a generic/unresolved-variable key names instead of an item. */
	text?: string;
	/** ERC: the sheet uuid paths the key names (as in SheetInst.path): the main item's first, then the sheet-specific one and the aux item's. */
	sheetPaths?: string[];
}

/** A violation of KiCad's ERC or DRC JSON report, as far as matching needs it. */
type Violation = Pick<ErcViolation, "type" | "items">;

/** The .kicad_pro section an exclusion list lives in, with the list's key and a dotted name for messages. */
function where(kind: ExclusionKind): { path: string[]; name: string } {
	return kind === "erc" ? { path: ["erc", "erc_exclusions"], name: "erc.erc_exclusions" } : { path: ["board", "design_settings", "drc_exclusions"], name: "board.design_settings.drc_exclusions" };
}

/** The raw exclusion list of the .kicad_pro data (KiCad's own entries), undefined when the file has none. */
export function exclusionList(data: Record<string, unknown>, kind: ExclusionKind): unknown[] | undefined {
	let v: unknown = data;
	for (const k of where(kind).path) v = v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>)[k] : undefined;
	return Array.isArray(v) ? v : undefined;
}

/** The exclusion list for an edit; throws when the .kicad_pro has none (not saved by KiCad 10 yet, or no board settings). */
export function editableList(data: Record<string, unknown>, kind: ExclusionKind, file: string): unknown[] {
	const list = exclusionList(data, kind);
	if (!list) throw new Error(`${file} has no ${where(kind).name}: open and save the project${kind === "drc" ? " and its board" : ""} in KiCad 10 once`);
	return list;
}

const ids = (...xs: (string | undefined)[]): string[] => xs.filter((x): x is string => !!x && x !== NIL_UUID && UUID.test(x));

/** Parse one exclusion key (see the module doc); unknown shapes keep what can be read. */
export function parseKey(kind: ExclusionKind, key: string): Pick<Exclusion, "rule" | "at" | "items" | "layer" | "text" | "sheetPaths"> {
	const p = key.split("|");
	const rule = p[0] ?? "";
	const unit = kind === "erc" ? 1e4 : 1e6;
	const at: Pt = [Number.parseInt(p[1] ?? "", 10) / unit || 0, Number.parseInt(p[2] ?? "", 10) / unit || 0];
	if (kind === "erc") {
		const sheetPaths = [...new Set([p[6], p[5], p[7]].filter((x): x is string => !!x))];
		const textKey = ["generic_warning", "generic_error", "unresolved_variable"].includes(rule) && !!p[4] && !UUID.test(p[4]);
		return textKey ? { rule, at, items: ids(p[3]), text: p[4], sheetPaths } : { rule, at, items: ids(p[3], p[4]), sheetPaths };
	}
	switch (rule) {
		case "copper_sliver":
		case "generic_warning":
		case "generic_error":
			return { rule, at, items: ids(p[3]), layer: p[4] || undefined };
		case "unconnected_items":
			// before KiCad 9.0.4: rule | x | y | main | aux
			return p.length >= 7 ? { rule, at, items: ids(p[5], p[6]), layer: p[3] || undefined } : { rule, at, items: ids(p[3], p[4]) };
		case "starved_thermal":
			return { rule, at, items: ids(p[3], p[4]), layer: p[5] || undefined };
		default:
			return { rule, at, items: ids(p[3], p[4]) };
	}
}

/** The project's exclusions of one kind, in file order. */
export function exclusions(data: Record<string, unknown>, kind: ExclusionKind): Exclusion[] {
	// KiCad writes [key, comment]; files from before comments hold the bare key
	return (exclusionList(data, kind) ?? []).map((e, index) => {
		const key = typeof e === "string" ? e : Array.isArray(e) && typeof e[0] === "string" ? e[0] : "";
		const comment = Array.isArray(e) && typeof e[1] === "string" ? e[1] : "";
		return { kind, index, key, comment, ...parseKey(kind, key) };
	});
}

/** Does this exclusion match this violation of KiCad's report (see the module doc)? */
export function excludes(e: Pick<Exclusion, "rule" | "items" | "at">, v: Violation): boolean {
	if (v.type !== e.rule) return false;
	if (e.items.length) {
		const have = new Set(v.items.map(i => i.uuid?.toLowerCase()));
		return e.items.every(u => have.has(u.toLowerCase()));
	}
	return v.items.some(i => i.pos && Math.abs(i.pos.x - e.at[0]) < 1e-4 && Math.abs(i.pos.y - e.at[1]) < 1e-4);
}

type Reported = Violation & { excluded?: boolean };

/** How many violations of the report an exclusion matches that the report shows (not excluded). */
const shownOf = (e: Exclusion, report: Reported[]): number => report.filter(v => !v.excluded && excludes(e, v)).length;

/**
 * The violation an exclusion hides, undefined when it is stale. `report` is KiCad's report as is,
 * `detected` the violations KiCad finds (for ERC a run without the exclusions; for DRC the report
 * itself): the exclusion hides one when KiCad detects more of its violations than the report
 * shows, the rest being excluded. An excluded violation alone isn't proof: ERC re-creates the
 * marker of a stale exclusion (see the module doc), and a violation that moved shows unexcluded
 * beside it.
 */
export function hiddenBy<V extends Reported>(e: Exclusion, report: V[], detected: Violation[]): V | undefined {
	if (detected.filter(v => excludes(e, v)).length <= shownOf(e, report)) return undefined;
	return report.find(v => v.excluded && excludes(e, v));
}

/** Exclusions in `before` whose key is gone from `after` (removed by the edit). */
export function removedExclusions(before: Record<string, unknown>, after: Record<string, unknown>, kind: ExclusionKind): Exclusion[] {
	const kept = new Set(exclusions(after, kind).map(e => e.key));
	return exclusions(before, kind).filter(e => !kept.has(e.key));
}

/**
 * A report with the excluded violations that a removed exclusion really hid shown as not
 * excluded: the edit's before-state for its gates, so a violation an edit makes visible again by
 * removing its exclusion is an old one, not a new one. A removed exclusion hid as many of its
 * violations as the `after` report (without it) shows beyond the before report; the excluded
 * marker ERC re-creates for a stale exclusion (see the module doc) hid nothing and stays excluded.
 */
export function unexcluded<V extends Reported>(report: V[], removed: Exclusion[], after: Reported[]): { report: V[]; shown: V[] } {
	const left = removed.map(e => shownOf(e, after) - shownOf(e, report));
	const shown: V[] = [];
	const out = report.map(v => {
		if (!v.excluded) return v;
		const i = removed.findIndex((e, k) => left[k]! > 0 && excludes(e, v));
		if (i < 0) return v;
		left[i]!--;
		const s = { ...v, excluded: false };
		shown.push(s);
		return s;
	});
	return { report: out, shown };
}

/**
 * The exclusions an op names: `key` exactly, or `match` (case-insensitive) in the key or the
 * comment. One hit unless `all`; throws listing candidates otherwise.
 */
export function selectExclusions(data: Record<string, unknown>, kind: ExclusionKind, sel: { key?: string; match?: string; all?: boolean }): Exclusion[] {
	if ((sel.key === undefined) === (sel.match === undefined)) throw new Error("give exactly one of key (the whole key) or match (text in the key or comment)");
	const list = exclusions(data, kind);
	const show = (l: Exclusion[]) => l.map(e => `${e.rule} @ (${e.at[0]}, ${e.at[1]})${e.comment ? ` "${e.comment.length > 60 ? `${e.comment.slice(0, 57)}…` : e.comment}"` : ""}`).join("; ");
	let hits: Exclusion[];
	if (sel.key !== undefined) hits = list.filter(e => e.key === sel.key);
	else {
		const m = sel.match!.toLowerCase();
		if (!m.trim()) throw new Error("match: give some text");
		hits = list.filter(e => e.key.toLowerCase().includes(m) || e.comment.toLowerCase().includes(m));
	}
	if (hits.length === 0) throw new Error(`no ${kind.toUpperCase()} exclusion ${sel.key !== undefined ? "with that key" : `matches "${sel.match}"`}; exclusions: ${show(list) || "none"} (kicad_query exclusions lists them)`);
	if (hits.length > 1 && !sel.all) throw new Error(`${hits.length} ${kind.toUpperCase()} exclusions match: ${show(hits)}; narrow match, give the key, or set all`);
	return hits;
}
