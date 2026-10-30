/**
 * KiCad's DRC with schematic parity (`drc`): one finding per violation and parity issue, keyed
 * without coordinates or net names, so an edit's before/after can tell new ones from old ones.
 */
import { type DrcReport, type DrcViolation, pcbDrc } from "../cli";
import { isEdgeItem } from "../edges";
import { child, children, head, isList, type List, text } from "../sexpr";
import type { CheckDef, Finding, Severity } from "../types";

/**
 * The footprint a violation item belongs to: "Pad 2 [GND] of R1 on F.Cu", "Reference field of R36",
 * "Footprint U1". Board items (Edge.Cuts, tracks, vias, zones, board text) have none.
 */
function ownerOf(item: string): string | undefined {
	return /\bof ([^\s[\]]+)(?= on |$)/.exec(item)?.[1] ?? /^[Ff]ootprint ([A-Za-z#_][\w#+.-]*)(?=\s|$)/.exec(item)?.[1];
}

/** References a violation names: its items' owners, and the symbol of a "Missing footprint H1 (M3)" parity issue. */
function refsOf(v: DrcViolation): string[] {
	const out = new Set<string>();
	for (const i of v.items) {
		const o = ownerOf(i.description);
		if (o) out.add(o);
	}
	const missing = /^Missing footprint (\S+)/.exec(v.description)?.[1];
	if (missing) out.add(missing);
	return [...out].sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
}

/** The one footprint every item of a violation belongs to; undefined when an item is a board item or owners differ. */
function confinedTo(v: DrcViolation): string | undefined {
	const owners = new Set(v.items.map(i => ownerOf(i.description)));
	const [only] = owners;
	return v.items.length > 0 && owners.size === 1 && only !== undefined ? only : undefined;
}

/**
 * An item or description as a key part: the bracketed net name dropped ("Pad 1 [GND] of R1" →
 * "Pad 1 of R1") and footprint references renamed, so renamed nets and references keep old violations old.
 */
function keyText(s: string, rename: ReadonlyMap<string, string>): string {
	return s
		.replace(/ \[.*?\](?= of | on |,|$)/, "")
		.replace(/(\bof |\b[Ff]ootprint )([^\s[\]()]+)/g, (m, lead: string, ref: string) => (rename.has(ref) ? lead + rename.get(ref)! : m));
}

function finding(v: DrcViolation, kind: "drc" | "parity", severity: Severity, rename: ReadonlyMap<string, string> = new Map()): Finding {
	const items = v.items.map(i => i.description);
	const pos = v.items.find(i => i.pos)?.pos;
	const refs = refsOf(v);
	const confined = confinedTo(v);
	return {
		check: "drc",
		severity,
		message: `${kind === "parity" ? "schematic parity, " : ""}${v.type}: ${v.description}${items.length ? ` [${items.join("; ")}]` : ""}`,
		key: `${kind}|${v.type}|${kind === "parity" ? `${keyText(v.description, rename)}|` : ""}${items
			.map(i => keyText(i, rename))
			.sort()
			.join("|")}`,
		...(pos ? { at: [pos.x, pos.y] as const } : {}),
		...(refs.length ? { refs } : {}),
		...(confined ? { confinedTo: confined } : {}),
	};
}

const sev = (s: string): Severity => (s === "error" ? "error" : "warning");

/**
 * Findings for the DRC violations and parity issues that are not excluded in KiCad. `rename` maps
 * references in this report to the ones to key them by (a before-edit report keyed with the after-edit names).
 */
export function drcFindings(r: DrcReport, rename?: ReadonlyMap<string, string>): { violations: Finding[]; parity: Finding[] } {
	return {
		violations: r.violations.filter(v => !v.excluded).map(v => finding(v, "drc", sev(v.severity), rename)),
		parity: r.parity.filter(v => !v.excluded).map(v => finding(v, "parity", sev(v.severity), rename)),
	};
}

/** References that changed between two versions of a board, matched by footprint uuid: before ref → after ref. */
export function refRenames(before: List, after: List): Map<string, string> {
	const refsByUuid = (tree: List) => {
		const m = new Map<string, string>();
		for (const fp of children(tree, "footprint")) {
			const uuid = text(child(fp, "uuid"));
			const ref = children(fp, "property").find(p => text(p, 1) === "Reference");
			if (uuid && ref) m.set(uuid, text(ref, 2) ?? "");
		}
		return m;
	};
	const now = refsByUuid(after);
	const out = new Map<string, string>();
	for (const [uuid, ref] of refsByUuid(before)) {
		const to = now.get(uuid);
		if (to !== undefined && to !== ref) out.set(ref, to);
	}
	return out;
}

/** Is there anything to check: footprints or an outline? */
export function boardIsEmpty(tree: List): boolean {
	return !tree.some(c => isList(c) && (head(c) === "footprint" || isEdgeItem(c)));
}

const drc: CheckDef = {
	name: "drc",
	doc:
		"KiCad's DRC with schematic parity on the board: one finding per violation and parity issue (KiCad's severity, rule type, message, position in board mm, references); violations excluded in KiCad are info with their exclusion comment; unconnected items (unrouted connections) are one info finding with their count unless args {unconnected: true} lists each. " +
		"Zones are refilled first, in memory (the file keeps its fills). An empty board (no footprints, no outline) gives one info finding and no DRC run.",
	byDefault: true,
	async run(ctx) {
		const board = ctx.project.board();
		if (boardIsEmpty(board.tree)) {
			return [{ check: "drc", severity: "info", message: "the board is empty (no footprints, no outline): DRC not run", key: "drc|empty" }];
		}
		// zones (the board's and its footprints') refilled in memory first, as pcbnew's DRC does, so fills match the current copper
		const zones = children(board.tree, "zone").length > 0 || board.footprints().some(fp => children(fp, "zone").length > 0);
		const r = await pcbDrc(ctx.project.boardFile, { cwd: ctx.project.dir, repoRoot: ctx.project.repoRoot, signal: ctx.signal }, true, zones);
		const { violations, parity } = drcFindings(r);
		const excluded = [...r.violations, ...r.parity]
			.filter(v => v.excluded)
			.map((v): Finding => {
				const f = finding(v, r.parity.includes(v) ? "parity" : "drc", "info");
				return { ...f, message: `excluded in KiCad${v.comment?.trim() ? ` (${v.comment.trim()})` : " without a comment"}: ${f.message}` };
			});
		const unconnected: Finding[] =
			ctx.args.unconnected === true
				? r.unconnected.map(v => ({ ...finding(v, "drc", sev(v.severity)), key: `unconnected|${v.items.map(i => i.description).sort().join("|")}` }))
				: r.unconnected.length
					? [{ check: "drc", severity: "info", message: `${r.unconnected.length} unconnected items (connections not routed yet); args {unconnected: true} lists them`, key: "drc|unconnected" }]
					: [];
		return [...violations, ...parity, ...excluded, ...unconnected];
	},
};

export const checks: CheckDef[] = [drc];
