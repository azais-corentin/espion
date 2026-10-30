/**
 * The project's layout constraints (`layout`): settings.layout's near, lengthGroups, paths,
 * viaParity, copperArea and heightZones, checked on the board's copper (zones refilled first).
 * Every finding quotes the constraint's `why`. A ref, pad or net the board doesn't have is an error
 * (stale config); a net not routed yet or a part not placed yet is info, not a failure.
 */
import { courtyardEdges, fpId, fpRef, placement } from "../board";
import { type Copper, type Pad, type Shape, shapeArea, shapeHas, useHeightForLength } from "../copper";
import { polygonArea } from "../edges";
import { type Box, type Pt, r3 } from "../geom";
import type { CheckDef, Finding, Severity } from "../types";
import { netLeaf, wildcards } from "./circuit";
import { courtyardDistance, filledCopper, fmtPt, nearest, onBoard, outlineOf } from "./copper";

/** Nets of the board a config name or pattern matches: the full name or the leaf, case-insensitive. */
function netsMatching(c: Copper, pattern: string): string[] {
	if (c.hasNet(pattern)) return [pattern];
	const m = wildcards([pattern]);
	return c.nets().filter(n => m(n) || m(netLeaf(n)));
}

/** A polygon clipped to a box (Sutherland–Hodgman; the box is convex, so any polygon clips right for its area). */
function clip(poly: Pt[], b: Box): Pt[] {
	let pts = poly;
	const edges: [(p: Pt) => boolean, (p: Pt, q: Pt) => Pt][] = [
		[p => p[0] >= b[0], (p, q) => [b[0], p[1] + ((q[1] - p[1]) * (b[0] - p[0])) / (q[0] - p[0])]],
		[p => p[0] <= b[2], (p, q) => [b[2], p[1] + ((q[1] - p[1]) * (b[2] - p[0])) / (q[0] - p[0])]],
		[p => p[1] >= b[1], (p, q) => [p[0] + ((q[0] - p[0]) * (b[1] - p[1])) / (q[1] - p[1]), b[1]]],
		[p => p[1] <= b[3], (p, q) => [p[0] + ((q[0] - p[0]) * (b[3] - p[1])) / (q[1] - p[1]), b[3]]],
	];
	for (const [inside, cut] of edges) {
		const next: Pt[] = [];
		for (let i = 0; i < pts.length; i++) {
			const p = pts[i]!;
			const q = pts[(i + 1) % pts.length]!;
			if (inside(p)) {
				next.push(p);
				if (!inside(q)) next.push(cut(p, q));
			} else if (inside(q)) next.push(cut(p, q));
		}
		pts = next;
		if (!pts.length) break;
	}
	return pts;
}

const inBox = (p: Pt, b: Box) => p[0] >= b[0] && p[0] <= b[2] && p[1] >= b[1] && p[1] <= b[3];

const layoutCheck: CheckDef = {
	name: "layout",
	doc:
		"The board against settings.layout (each finding quotes the entry's why): near (copper of two parts or pads, a footprint being its pads, edge to edge above max: warning); lengthGroups (routed lengths with via heights when the project counts them, a diff pair whose P and N are both in the group as their mean: spread above maxSpread or a length above maxLength: warning); paths (copper path from one pad to each other pad: above maxLength, or max − min above maxDiff: warning); viaParity (nets with different via counts: warning); " +
		"copperArea (zone fills plus the tracks and pads not inside a fill of the net on that layer, on the given layers, with near/within only the copper inside the box of the footprint's pads grown by `within` (fills clipped to it, tracks and pads by their middle, vias by their centre): area below minArea or fewer vias than minVias: warning); heightZones (a placed part on the zone's side whose courtyard outline, rotation applied, comes closer to center than r0 + slope × its height: warning; one with no height known closer than r0 + slope × maxHeight: warning). " +
		"A ref, pad, net or layer the board doesn't have is an error (stale config); a net not routed yet or a part not placed yet (origin off the board) is info. Zones are refilled first on a copy of the project. No constraints: one info finding.",
	byDefault: true,
	async run(ctx) {
		const L = ctx.project.settings.layout;
		const count = L.near.length + L.lengthGroups.length + L.paths.length + L.viaParity.length + L.copperArea.length + L.heightZones.length;
		if (!count) return [{ check: "layout", severity: "info", message: "no layout constraints in settings.layout: nothing to check", key: "layout|none" }];
		const c = await filledCopper(ctx);
		const o = outlineOf(ctx.project);
		const board = ctx.project.board();
		const fps = new Map(board.footprints().map(fp => [fpRef(fp), fp]));
		const opts = { useHeight: useHeightForLength(ctx.project) };
		const out: Finding[] = [];
		const add = (severity: Severity, kind: string, name: string, key: string, message: string, why: string, extra: Partial<Finding> = {}) =>
			out.push({ check: "layout", severity, message: `${kind} ${name}: ${message} (why: ${why})`, key: `layout|${kind}|${name}${key ? `|${key}` : ""}`, ...extra });
		const routed = (net: string) => c.connectivity(net).unrouted.length === 0;
		/** Nets of an entry's names and patterns; a name that matches none is reported stale. */
		const netsOf = (kind: string, name: string, names: string[], why: string): string[] | undefined => {
			const nets: string[] = [];
			for (const n of names) {
				const hit = netsMatching(c, n);
				if (!hit.length) {
					add("error", kind, name, `stale|${n}`, `no net "${n}" on the board (stale config)`, why);
					return undefined;
				}
				for (const h of hit) if (!nets.includes(h)) nets.push(h);
			}
			return nets;
		};
		const placed = (ref: string) => onBoard(o, placement(fps.get(ref)!).at);

		// near
		for (const e of L.near) {
			const name = `${e.a} – ${e.b}`;
			const sides: { pads: Pad[]; ref: string }[] = [];
			for (const s of [e.a, e.b]) {
				const dot = s.lastIndexOf(".");
				const ref = dot > 0 && !fps.has(s) ? s.slice(0, dot) : s;
				if (!fps.has(ref)) {
					add("error", "near", name, `stale|${s}`, `no footprint ${ref} on the board (stale config)`, e.why);
					break;
				}
				let pads: Pad[];
				try {
					pads = ref === s ? c.pads.filter(p => p.ref === s && p.copper.length) : c.pad(s);
				} catch (err) {
					add("error", "near", name, `stale|${s}`, `${(err as Error).message} (stale config)`, e.why);
					break;
				}
				if (!pads.length) {
					add("error", "near", name, `nopads|${s}`, `${s} has no copper pads to measure from`, e.why);
					break;
				}
				sides.push({ pads, ref });
			}
			if (sides.length < 2) continue;
			const [a, b] = sides as [{ pads: Pad[]; ref: string }, { pads: Pad[]; ref: string }];
			const later = [a.ref, b.ref].filter(r => !placed(r));
			if (later.length) {
				add("info", "near", name, "unplaced", `${later.join(", ")} not placed on the board yet`, e.why);
				continue;
			}
			const d = nearest(
				a.pads.flatMap(p => p.shapes),
				b.pads.flatMap(p => p.shapes),
			);
			if (!d || d.d <= e.max + 1e-9) continue;
			add("warning", "near", name, "", `${r3(d.d)} mm apart (copper edge to edge, max ${e.max} mm)`, e.why, {
				at: [r3((d.pa[0] + d.pb[0]) / 2), r3((d.pa[1] + d.pb[1]) / 2)],
				refs: [...new Set([a.ref, b.ref])],
			});
		}

		// lengthGroups
		for (const g of L.lengthGroups) {
			const nets = netsOf("lengthGroups", g.name, g.nets, g.why);
			if (!nets) continue;
			const members: { label: string; nets: string[] }[] = [];
			for (const n of nets) {
				const pair = c.dpPartner(n);
				if (pair && nets.includes(pair.partner)) {
					if (pair.positive) members.push({ label: `${n} / ${pair.partner}`, nets: [n, pair.partner] });
				} else members.push({ label: n, nets: [n] });
			}
			const open = members.filter(m => !m.nets.every(routed));
			if (open.length) add("info", "lengthGroups", g.name, "unrouted", `not routed yet: ${open.map(m => m.label).join(", ")}`, g.why);
			const lengths = members
				.filter(m => !open.includes(m))
				.map(m => ({ label: m.label, length: m.nets.reduce((s, n) => s + c.netLength(n, opts).total, 0) / m.nets.length }));
			if (g.maxLength !== undefined) {
				for (const m of lengths) if (m.length > g.maxLength + 1e-6) add("warning", "lengthGroups", g.name, `maxLength|${m.label}`, `${m.label} is ${r3(m.length)} mm long (max ${g.maxLength} mm)`, g.why);
			}
			if (open.length || lengths.length < 2) continue;
			const sorted = [...lengths].sort((x, y) => x.length - y.length);
			const spread = sorted[sorted.length - 1]!.length - sorted[0]!.length;
			if (spread <= g.maxSpread + 1e-6) continue;
			add("warning", "lengthGroups", g.name, "spread", `lengths spread ${r3(spread)} mm (max ${g.maxSpread} mm): ${sorted.map(m => `${m.label} ${r3(m.length)}`).join(", ")} mm`, g.why);
		}

		// paths
		for (const p of L.paths) {
			const lengths: { to: string; length: number }[] = [];
			const open: string[] = [];
			let bad = false;
			for (const to of p.to) {
				try {
					const r = c.path(p.from, to, opts);
					if (r) lengths.push({ to, length: r.length });
					else open.push(to);
				} catch (err) {
					add("error", "paths", p.name, `stale|${to}`, `${(err as Error).message} (stale config)`, p.why);
					bad = true;
					break;
				}
			}
			if (bad) continue;
			if (open.length) add("info", "paths", p.name, "unrouted", `${p.from} to ${open.join(", ")} not routed yet`, p.why);
			if (p.maxLength !== undefined) {
				for (const l of lengths) if (l.length > p.maxLength + 1e-6) add("warning", "paths", p.name, `maxLength|${l.to}`, `${p.from} to ${l.to} is ${r3(l.length)} mm of copper (max ${p.maxLength} mm)`, p.why);
			}
			if (p.maxDiff === undefined || open.length || lengths.length < 2) continue;
			const sorted = [...lengths].sort((x, y) => x.length - y.length);
			const diff = sorted[sorted.length - 1]!.length - sorted[0]!.length;
			if (diff <= p.maxDiff + 1e-6) continue;
			add("warning", "paths", p.name, "diff", `branches from ${p.from} differ by ${r3(diff)} mm (max ${p.maxDiff} mm): ${sorted.map(l => `${l.to} ${r3(l.length)}`).join(", ")} mm`, p.why);
		}

		// viaParity
		for (const v of L.viaParity) {
			const nets = netsOf("viaParity", v.name, v.nets, v.why);
			if (!nets) continue;
			const open = nets.filter(n => !routed(n));
			if (open.length) {
				add("info", "viaParity", v.name, "unrouted", `not routed yet: ${open.join(", ")}`, v.why);
				continue;
			}
			const counts = nets.map(n => ({ net: n, vias: c.connectivity(n).vias.length }));
			if (new Set(counts.map(x => x.vias)).size <= 1) continue;
			add("warning", "viaParity", v.name, "", `via counts differ: ${counts.map(x => `${x.net} ${x.vias}`).join(", ")}`, v.why);
		}

		// copperArea
		for (const a of L.copperArea) {
			const hit = netsMatching(c, a.net);
			if (hit.length !== 1) {
				add("error", "copperArea", a.name, `stale|${a.net}`, hit.length ? `"${a.net}" matches ${hit.length} nets (${hit.slice(0, 6).join(", ")}): name one` : `no net "${a.net}" on the board (stale config)`, a.why);
				continue;
			}
			const net = hit[0]!;
			const badLayer = a.layers.find(l => !c.copperLayers.includes(l));
			if (badLayer) {
				add("error", "copperArea", a.name, `stale|${badLayer}`, `"${badLayer}" is not a copper layer of the board (${c.copperLayers.join(", ")})`, a.why);
				continue;
			}
			let region: Box | undefined;
			if (a.near !== undefined) {
				if (!fps.has(a.near)) {
					add("error", "copperArea", a.name, `stale|${a.near}`, `no footprint ${a.near} on the board (stale config)`, a.why);
					continue;
				}
				if (!placed(a.near)) {
					add("info", "copperArea", a.name, "unplaced", `${a.near} not placed on the board yet`, a.why);
					continue;
				}
				const boxes = c.pads.filter(p => p.ref === a.near && p.copper.length).flatMap(p => p.shapes.map(s => s.box));
				if (!boxes.length) {
					add("error", "copperArea", a.name, `nopads|${a.near}`, `${a.near} has no copper pads to measure from`, a.why);
					continue;
				}
				const w = a.within ?? 0;
				region = [Math.min(...boxes.map(b => b[0])) - w, Math.min(...boxes.map(b => b[1])) - w, Math.max(...boxes.map(b => b[2])) + w, Math.max(...boxes.map(b => b[3])) + w];
			}
			const fills = c.zones.filter(z => z.net === net).flatMap(z => z.fills.filter(f => a.layers.includes(f.layer)));
			const inFill = (p: Pt, layer: string) => fills.some(f => f.layer === layer && shapeHas(f.shape, p));
			const inRegion = (p: Pt) => !region || inBox(p, region);
			let area = fills.reduce((s, f) => s + (region ? polygonArea(clip(f.shape.pts, region)) : f.area), 0);
			const sum = (shapes: Shape[]) => shapes.reduce((s, x) => s + shapeArea(x), 0);
			for (const t of c.tracks) {
				if (t.net !== net || !a.layers.includes(t.layer)) continue;
				const mid: Pt = [(t.start[0] + t.end[0]) / 2, (t.start[1] + t.end[1]) / 2];
				if (inRegion(mid) && !(inFill(t.start, t.layer) && inFill(t.end, t.layer) && inFill(mid, t.layer))) area += sum(t.shapes);
			}
			for (const p of c.pads) {
				if (p.net !== net || !inRegion(p.at)) continue;
				for (const l of p.copper) if (a.layers.includes(l) && !inFill(p.at, l)) area += sum(p.shapes);
			}
			const vias = c.vias.filter(v => v.net === net && inRegion(v.at)).length;
			const where = `${net} on ${a.layers.join(", ")}${region ? ` within ${a.within} mm of ${a.near}` : ""}`;
			if (a.minArea !== undefined && area < a.minArea - 1e-6) add("warning", "copperArea", a.name, "area", `${where}: ${r3(area)} mm² of copper (min ${a.minArea} mm²)`, a.why, { net });
			if (a.minVias !== undefined && vias < a.minVias) add("warning", "copperArea", a.name, "vias", `${where}: ${vias} via${vias === 1 ? "" : "s"} (min ${a.minVias})`, a.why, { net });
		}

		// heightZones
		for (const z of L.heightZones) {
			const side = z.side ?? "top";
			const keys = Object.keys(z.heights);
			const refKeys = keys.filter(k => !k.includes(":")).map(k => ({ k, m: wildcards([k]) }));
			const libKeys = keys.filter(k => k.includes(":")).map(k => ({ k, m: wildcards([k]) }));
			const all = [...fps.values()];
			const stale = [...refKeys.filter(x => !all.some(fp => x.m(fpRef(fp)))), ...libKeys.filter(x => !all.some(fp => x.m(fpId(fp))))];
			for (const s of stale) add("error", "heightZones", z.name, `stale|${s.k}`, `heights names "${s.k}", which matches no footprint on the board (stale config)`, z.why);
			const maxH = z.maxHeight ?? Math.max(0, ...Object.values(z.heights));
			for (const [ref, fp] of fps) {
				const pl = placement(fp);
				if (pl.side !== side || !onBoard(o, pl.at)) continue;
				const court = courtyardEdges(fp);
				if (!court.length) continue;
				const d = courtyardDistance(court, z.center);
				const key = refKeys.find(x => x.m(ref)) ?? libKeys.find(x => x.m(fpId(fp)));
				const at: Pt = [r3(pl.at[0]), r3(pl.at[1])];
				if (key) {
					const h = z.heights[key.k]!;
					const need = z.r0 + z.slope * h;
					if (d < need - 1e-6) add("warning", "heightZones", z.name, ref, `${ref} (${h} mm tall, by "${key.k}") is ${r3(d)} mm from ${fmtPt(z.center)} (courtyard), needs ${r3(need)} mm`, z.why, { at, refs: [ref] });
				} else if (d < z.r0 + z.slope * maxH - 1e-6) {
					add("warning", "heightZones", z.name, `${ref}|unknown`, `${ref} (${fpId(fp)}) is ${r3(d)} mm from ${fmtPt(z.center)} (courtyard) and its height is unknown: give it in heights (a part ${maxH} mm tall needs ${r3(z.r0 + z.slope * maxH)} mm)`, z.why, { at, refs: [ref] });
				}
			}
		}
		return out;
	},
};

export const checks: CheckDef[] = [layoutCheck];
