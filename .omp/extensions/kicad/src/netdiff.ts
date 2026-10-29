/**
 * Connectivity difference between two netlists, and the check of that difference against the
 * change an edit declared (`expect`).
 *
 * A net is its set of pins ("REF.pin"). Single-pin nets KiCad names `unconnected-(…)` count as no
 * net. Nets with the same pins and a different name are renames; KiCad's automatic names
 * (`Net-(…)`, `unconnected-(…)`) follow references, so a rename between two automatic names is
 * not a change.
 */
import type { Netlist } from "./cli";

export interface NetState {
	name: string;
	pins: string[];
}

export interface ConnDiff {
	addedParts: string[];
	removedParts: string[];
	renamed: { from: string; to: string; pins: string[] }[];
	/** Nets whose pin set is new (created, grown, shrunk, merged or split), as they are after. */
	changed: NetState[];
	/** Nets as they were before, for every pin set that no longer exists. */
	gone: NetState[];
	/** Pins that were on a net before and are on none after (parts still present). */
	disconnected: string[];
	/** Value, footprint, field and DNP changes on parts present before and after. */
	fieldChanges: { ref: string; field: string; from: string; to: string }[];
}

/**
 * One net whose pins change. Either `pins` (the exact final pin list) or, for a net that exists
 * before the edit, `added` / `removed` relative to its pins before; `name` is required for the
 * delta form.
 */
export interface ExpectNet {
	name?: string;
	pins?: string[];
	added?: string[];
	removed?: string[];
}

export interface Expect {
	added_parts?: string[];
	removed_parts?: string[];
	renamed_nets?: { from: string; to: string }[];
	/** Every net whose pins change, as it must be after the edit. */
	nets?: ExpectNet[];
	/** Pins of existing parts that must end up on no net. */
	unconnected?: string[];
}

const AUTO = /^(Net-\(|unconnected-\()/;

function nets(nl: Netlist): Map<string, NetState> {
	const out = new Map<string, NetState>();
	for (const n of nl.nets) {
		if (n.name.startsWith("unconnected-(")) continue;
		const pins = n.nodes.map(x => `${x.ref}.${x.pin}`).sort();
		out.set(pins.join(" "), { name: n.name, pins });
	}
	return out;
}

const FIELDS = ["value", "footprint", "datasheet"] as const;

export function diffNetlists(before: Netlist, after: Netlist): ConnDiff {
	const a = nets(before);
	const b = nets(after);
	const addedParts = [...after.components.keys()].filter(r => !before.components.has(r)).sort();
	const removedParts = [...before.components.keys()].filter(r => !after.components.has(r)).sort();
	const renamed: ConnDiff["renamed"] = [];
	const changed: NetState[] = [];
	for (const [k, n] of b) {
		const old = a.get(k);
		if (!old) changed.push(n);
		else if (old.name !== n.name && !(AUTO.test(old.name) && AUTO.test(n.name))) {
			renamed.push({ from: old.name, to: n.name, pins: n.pins });
		}
	}
	const gone = [...a].filter(([k]) => !b.has(k)).map(([, n]) => n);
	const onNet = new Set([...b.values()].flatMap(n => n.pins));
	const removed = new Set(removedParts);
	const disconnected = [...new Set(gone.flatMap(n => n.pins))]
		.filter(p => !onNet.has(p) && !removed.has(p.slice(0, p.lastIndexOf("."))))
		.sort();
	const fieldChanges: ConnDiff["fieldChanges"] = [];
	for (const [ref, c] of after.components) {
		const o = before.components.get(ref);
		if (!o) continue;
		for (const f of FIELDS) if (o[f] !== c[f]) fieldChanges.push({ ref, field: f, from: o[f], to: c[f] });
		const keys = new Set([...Object.keys(o.fields), ...Object.keys(c.fields)]);
		for (const f of keys) {
			if (["Footprint", "Datasheet", "Description"].includes(f)) continue;
			if ((o.fields[f] ?? "") !== (c.fields[f] ?? "")) fieldChanges.push({ ref, field: f, from: o.fields[f] ?? "", to: c.fields[f] ?? "" });
		}
		if (o.dnp !== c.dnp) fieldChanges.push({ ref, field: "dnp", from: String(o.dnp), to: String(c.dnp) });
	}
	return {
		addedParts,
		removedParts,
		renamed,
		changed: changed.sort((x, y) => x.name.localeCompare(y.name)),
		gone,
		disconnected,
		fieldChanges,
	};
}

/** Net names match when equal, or when the expected name is the leaf of a hierarchical name. */
function nameMatches(actual: string, wanted: string): boolean {
	return actual === wanted || actual.endsWith(`/${wanted.replace(/^\//, "")}`);
}

const sameSet = (x: string[], y: string[]) => x.length === y.length && [...x].sort().join(" ") === [...y].sort().join(" ");

/** Problems where the diff and the declared expectation disagree ([] = they match). */
export function checkExpect(d: ConnDiff, e: Expect): string[] {
	const problems: string[] = [];
	const cmpList = (what: string, actual: string[], wanted: string[] = []) => {
		const extra = actual.filter(x => !wanted.includes(x));
		const missing = wanted.filter(x => !actual.includes(x));
		if (extra.length) problems.push(`${what} not declared in expect: ${extra.join(", ")}`);
		if (missing.length) problems.push(`${what} declared in expect but did not happen: ${missing.join(", ")}`);
	};
	cmpList("added parts", d.addedParts, e.added_parts);
	cmpList("removed parts", d.removedParts, e.removed_parts);
	cmpList("unconnected pins", d.disconnected, e.unconnected);

	const wantRen = e.renamed_nets ?? [];
	for (const r of d.renamed) {
		if (!wantRen.some(w => nameMatches(r.from, w.from) && nameMatches(r.to, w.to))) {
			problems.push(`net renamed but not declared: ${r.from} -> ${r.to} (${r.pins.length} pins)`);
		}
	}
	for (const w of wantRen) {
		if (!d.renamed.some(r => nameMatches(r.from, w.from) && nameMatches(r.to, w.to))) {
			// a declared rename may also have changed pins; then it shows up as a changed net
			const ch = d.changed.find(c => nameMatches(c.name, w.to));
			if (!ch) problems.push(`rename ${w.from} -> ${w.to} declared but no net is named ${w.to} with the old pins`);
		}
	}

	const wantNets = e.nets ?? [];
	const before = (name: string) => d.gone.find(g => nameMatches(g.name, name));
	/** Final pin list an entry stands for, or a reason it can't be resolved. */
	const target = (w: ExpectNet): string[] | string => {
		if (w.pins) return w.pins;
		if (!w.name) return "an expect.nets entry without `pins` needs `name`";
		const b = before(w.name);
		const start = b?.pins ?? [];
		const missing = (w.removed ?? []).filter(p => !start.includes(p));
		if (missing.length) return `net ${w.name}: removed pins ${missing.join(", ")} were not on it before`;
		return [...start.filter(p => !(w.removed ?? []).includes(p)), ...(w.added ?? [])];
	};
	const describeW = (w: ExpectNet) =>
		w.pins ? `${w.name ?? ""} [${w.pins.join(", ")}]` : `${w.name} +[${(w.added ?? []).join(", ")}] -[${(w.removed ?? []).join(", ")}]`;
	const targets = wantNets.map(target);
	for (const t of targets) if (typeof t === "string") problems.push(t);
	const used = new Set<number>();
	for (const c of d.changed) {
		const i = targets.findIndex((t, j) => !used.has(j) && typeof t !== "string" && sameSet(t, c.pins) && (!wantNets[j]!.name || nameMatches(c.name, wantNets[j]!.name!)));
		if (i >= 0) {
			used.add(i);
			continue;
		}
		const near = wantNets.find(w => (w.name && nameMatches(c.name, w.name)) || [...(w.pins ?? []), ...(w.added ?? [])].some(p => c.pins.includes(p)));
		problems.push(`net ${describeChange(d, c)} but expect.nets has no matching entry` + (near ? ` (closest declared: ${describeW(near)})` : ""));
	}
	wantNets.forEach((w, j) => {
		if (!used.has(j) && typeof targets[j] !== "string") {
			const actual = d.changed.find(c => w.name && nameMatches(c.name, w.name));
			problems.push(`expected net ${describeW(w)} not found among changed nets` + (actual ? `; actual: ${describeChange(d, actual)}` : ""));
		}
	});
	return problems;
}

/** "GND: added [R66.2], removed [] (125 pins now)" for a net that existed by that name before, else "X: [all pins]". */
function describeChange(d: ConnDiff, c: NetState): string {
	const b = d.gone.find(g => g.name === c.name);
	if (!b) return `${c.name}: [${c.pins.join(", ")}]`;
	const added = c.pins.filter(p => !b.pins.includes(p));
	const removed = b.pins.filter(p => !c.pins.includes(p));
	return `${c.name}: added [${added.join(", ")}], removed [${removed.join(", ")}] (${c.pins.length} pins now)`;
}

export function summarize(d: ConnDiff): string[] {
	const out: string[] = [];
	if (d.addedParts.length) out.push(`added parts: ${d.addedParts.join(", ")}`);
	if (d.removedParts.length) out.push(`removed parts: ${d.removedParts.join(", ")}`);
	for (const r of d.renamed) out.push(`renamed net: ${r.from} -> ${r.to}`);
	for (const c of d.changed) out.push(`net ${describeChange(d, c)}`);
	if (d.disconnected.length) out.push(`now unconnected: ${d.disconnected.join(", ")}`);
	for (const f of d.fieldChanges) out.push(`${f.ref} ${f.field}: "${f.from}" -> "${f.to}"`);
	return out;
}
