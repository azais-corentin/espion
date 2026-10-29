/** Sheet geometry: millimetres, x right, y down. Library geometry has y up. */

export type Pt = readonly [number, number];
export type Box = readonly [number, number, number, number]; // x0, y0, x1, y1
export type Dir = "L" | "R" | "U" | "D";
export type Mirror = "x" | "y" | undefined;

export const DIR_VEC: Record<Dir, Pt> = { L: [-1, 0], R: [1, 0], U: [0, -1], D: [0, 1] };

/** Round to 1 µm, so coordinates computed different ways compare equal. */
export function r3(v: number): number {
	return Math.round(v * 1000) / 1000;
}

export function pt(x: number, y: number): Pt {
	return [r3(x), r3(y)];
}

export function key(p: Pt): string {
	return `${r3(p[0])},${r3(p[1])}`;
}

export function samePt(a: Pt, b: Pt): boolean {
	return r3(a[0]) === r3(b[0]) && r3(a[1]) === r3(b[1]);
}

/**
 * Library point (y up) to sheet point (y down) for a symbol at (X, Y) with rotation and mirror,
 * matching KiCad 10 (checked against kicad-cli netlists for every rotation/mirror).
 */
export function xform(lx: number, ly: number, X: number, Y: number, rot: number, mirror: Mirror): Pt {
	let sx = lx;
	let sy = -ly;
	for (let k = 0; k < (((Math.round(rot / 90) % 4) + 4) % 4); k++) [sx, sy] = [sy, -sx];
	if (mirror === "x") sy = -sy;
	else if (mirror === "y") sx = -sx;
	return pt(X + sx, Y + sy);
}

/** Side a wire leaves a pin whose library angle (pointing from the pin end toward the body) is `angle`. */
export function pinDir(angle: number, rot: number, mirror: Mirror): Dir {
	const a = (angle * Math.PI) / 180;
	const [dx, dy] = xform(Math.cos(a), Math.sin(a), 0, 0, rot, mirror);
	if (Math.abs(dx) > Math.abs(dy)) return dx > 0 ? "L" : "R";
	return dy > 0 ? "U" : "D";
}

export function bbox(ps: readonly Pt[]): Box {
	let x0 = Infinity;
	let y0 = Infinity;
	let x1 = -Infinity;
	let y1 = -Infinity;
	for (const [x, y] of ps) {
		x0 = Math.min(x0, x);
		y0 = Math.min(y0, y);
		x1 = Math.max(x1, x);
		y1 = Math.max(y1, y);
	}
	return [x0, y0, x1, y1];
}

export function overlap(a: Box, b: Box, margin = 0): boolean {
	return a[0] - margin < b[2] && b[0] - margin < a[2] && a[1] - margin < b[3] && b[1] - margin < a[3];
}

export interface Seg {
	a: Pt;
	b: Pt;
}

/** p lies strictly inside the axis-aligned segment (not on an end). */
export function onInterior(p: Pt, s: Seg): boolean {
	if (samePt(p, s.a) || samePt(p, s.b)) return false;
	const [x, y] = [r3(p[0]), r3(p[1])];
	if (r3(s.a[0]) === r3(s.b[0]) && r3(s.a[0]) === x) return Math.min(s.a[1], s.b[1]) < y && y < Math.max(s.a[1], s.b[1]);
	if (r3(s.a[1]) === r3(s.b[1]) && r3(s.a[1]) === y) return Math.min(s.a[0], s.b[0]) < x && x < Math.max(s.a[0], s.b[0]);
	return false;
}

export function touches(p: Pt, s: Seg): boolean {
	return samePt(p, s.a) || samePt(p, s.b) || onInterior(p, s);
}

export function isOrthogonal(s: Seg): boolean {
	return r3(s.a[0]) === r3(s.b[0]) || r3(s.a[1]) === r3(s.b[1]);
}

export function onGrid(v: number, grid: number): boolean {
	return Math.abs(v / grid - Math.round(v / grid)) < 1e-3;
}
