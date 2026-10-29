/** Helpers shared by the edit ops. */
import { isOrthogonal, key, onGrid, type Pt, pt } from "./geom";
import type { FoundSymbol, SheetInst } from "./project";
import type { Sch } from "./sch";
import { child, children, L } from "./sexpr";
import type { OpContext } from "./types";

/** A point argument, checked to be on the project's connection grid. */
export function gridPt(ctx: OpContext, v: unknown, what: string): Pt {
	const [x, y] = v as [number, number];
	const g = ctx.project.settings.grid;
	if (!onGrid(x, g) || !onGrid(y, g)) {
		const snap = (n: number) => Math.round(n / g) * g;
		throw new Error(`${what} (${x}, ${y}) is off the ${g} mm grid; nearest grid point (${snap(x).toFixed(2)}, ${snap(y).toFixed(2)})`);
	}
	return pt(x, y);
}

export function sheetArg(ctx: OpContext, v: unknown): SheetInst {
	return ctx.project.sheet(String(v));
}

export function symbolArg(ctx: OpContext, ref: unknown): FoundSymbol {
	return ctx.project.findSymbol(String(ref));
}

/**
 * Move wire end points: every wire end at a key of `moves` goes to the new point (only that end;
 * the other end stays). Throws when a moved wire would become diagonal, naming it, so the caller
 * can reroute instead of silently breaking the drawing.
 */
export function dragWireEnds(sch: Sch, moves: Map<string, Pt>): number {
	let n = 0;
	for (const w of sch.wires()) {
		const na = moves.get(key(w.a));
		const nb = moves.get(key(w.b));
		if (!na && !nb) continue;
		const a = na ?? w.a;
		const b = nb ?? w.b;
		if (!isOrthogonal({ a, b })) {
			throw new Error(
				`moving would make the wire (${w.a.join(", ")})-(${w.b.join(", ")}) diagonal: (${a.join(", ")})-(${b.join(", ")}); ` +
					"move along the wire's axis, or remove and redraw the wire",
			);
		}
		const xy = children(child(w.node, "pts")!, "xy");
		xy[0]!.splice(1, 2, ...L("xy", a[0], a[1]).slice(1));
		xy[xy.length - 1]!.splice(1, 2, ...L("xy", b[0], b[1]).slice(1));
		n++;
	}
	return n;
}
