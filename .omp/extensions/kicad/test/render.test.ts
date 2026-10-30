import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import type { Box, Pt } from "../src/geom";
import { describeLibSymbol, type Libraries } from "../src/libs";
import { Project } from "../src/project";
import { cropSvg, type Rasterize, renderPart, renderSheet } from "../src/render";
import { at, prop } from "../src/sch";
import { runRender } from "../src/tools";
import { runEdit } from "../src/txn";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

/** The root <svg> tag's viewBox and size attributes. */
function svgFrame(svg: string): { viewBox: number[]; width: string; height: string } {
	const tag = /<svg\b[^>]*>/.exec(svg)?.[0] ?? "";
	return {
		viewBox: (/\sviewBox="([^"]*)"/.exec(tag)?.[1] ?? "").split(/\s+/).map(Number),
		width: /\swidth="([^"]*)"/.exec(tag)?.[1] ?? "",
		height: /\sheight="([^"]*)"/.exec(tag)?.[1] ?? "",
	};
}

const calls: { svg: string; w: number; h: number; scale: number }[] = [];
/** @oh-my-pi/pi-natives doesn't load under bun test: record what would be rasterised. */
const stub: Rasterize = async (svg, w, h, scale) => {
	calls.push({ svg: new TextDecoder().decode(svg), w, h, scale });
	return new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
};
/** Longest side of the image rasterizeSvg draws: intrinsic px (96 dpi) × scale. */
const longestPx = (c: { svg: string; scale: number }) => {
	const { width, height } = svgFrame(c.svg);
	return Math.max(parseFloat(width), parseFloat(height)) * (96 / 25.4) * c.scale;
};

/** The unit cells of a part image: placement in the image (mm), viewBox into KiCad's plot, plot content. */
function cells(svg: string): { x: number; y: number; w: number; h: number; viewBox: number[]; body: string }[] {
	return [...svg.matchAll(/<svg x="([^"]*)" y="([^"]*)" width="([^"]*)" height="([^"]*)" viewBox="([^"]*)">([\s\S]*?)<\/svg>/g)].map(m => ({
		x: Number(m[1]),
		y: Number(m[2]),
		w: Number(m[3]),
		h: Number(m[4]),
		viewBox: m[5]!.split(" ").map(Number),
		body: m[6]!,
	}));
}

/** Where part coordinates land in a cell's plot: viewBox origin minus the frame's origin. */
const cellOffset = (cell: { viewBox: number[] }, frame: Box): Pt => [cell.viewBox[0]! - frame[0], cell.viewBox[1]! - frame[1]];

const near = (a: Pt, b: Pt, tol = 0.002) => Math.abs(a[0] - b[0]) <= tol && Math.abs(a[1] - b[1]) <= tol;

/** Path vertices of a KiCad plot ("x,y" in graphics, "Lx y" in pins). */
const vertices = (svg: string): Pt[] => [...svg.matchAll(/(-?\d+\.\d+)[, ](-?\d+\.\d+)/g)].map(m => [Number(m[1]), Number(m[2])]);

const circles = (svg: string): { c: Pt; r: number }[] =>
	[...svg.matchAll(/<circle cx="([^"]+)" cy="([^"]+)" r="([^"]+)"/g)].map(m => ({ c: [Number(m[1]), Number(m[2])], r: Number(m[3]) }));

/** Every round pad is plotted as a copper circle of its size at its position + offset. */
function roundPadsAt(svg: string, pads: { shape: string; at: Pt; size: Pt }[], offset: Pt): boolean {
	const cs = circles(svg);
	return pads
		.filter(p => p.shape === "circle")
		.every(p => cs.some(c => Math.abs(c.r - p.size[0] / 2) < 0.002 && near(c.c, [p.at[0] + offset[0], p.at[1] + offset[1]])));
}

/** Offset under which a plot shows every round pad where it belongs, if one exists. */
function padOffset(svg: string, pads: { shape: string; at: Pt; size: Pt }[]): Pt | undefined {
	const first = pads.find(p => p.shape === "circle")!;
	return circles(svg)
		.map(c => [c.c[0] - first.at[0], c.c[1] - first.at[1]] as Pt)
		.find(off => roundPadsAt(svg, pads, off));
}

describe("cropSvg", () => {
	test("rewrites only the root viewBox and size", () => {
		const src =
			'<?xml version="1.0"?>\n<svg xmlns="http://www.w3.org/2000/svg" version="1.1"\n  width="297.0022mm" height="210.0072mm" viewBox="0.0000 0.0000 297.0022 210.0072">\n<svg width="5mm" height="5mm" viewBox="0 0 5 5"/><path d="M1 2 L3 4"/></svg>';
		const out = cropSvg(src, [10, 20, 50.8, 80]);
		expect(svgFrame(out)).toEqual({ viewBox: [10, 20, 40.8, 60], width: "40.8000mm", height: "60.0000mm" });
		expect(out).toContain('<svg width="5mm" height="5mm" viewBox="0 0 5 5"/>');
		expect(out).toContain('<path d="M1 2 L3 4"/>');
		expect(out.replace(/<svg\b[^>]*>/, "")).toBe(src.replace(/<svg\b[^>]*>/, ""));
	});
});

describe.skipIf(!HAS_KICAD)("renderSheet", () => {
	let root = "";
	let analysis: Analysis;

	beforeAll(() => {
		root = stageFixture();
		analysis = new Analysis(Project.open(PROJECT, root));
	});

	afterAll(() => {
		analysis.dispose();
		cleanup(root);
	});

	test("refs: the crop frames the part's body, pins and fields plus the margin", async () => {
		const img = await renderSheet(analysis, { sheet: "MCU", refs: ["U1"], margin: 5, size: 800 }, stub);
		const call = calls.at(-1)!;
		expect([call.w, call.h]).toEqual([800, 800]);
		// a small crop is drawn at the requested size, not at the SVG's intrinsic ~3.8 px/mm
		expect(longestPx(call)).toBeCloseTo(800, 3);
		expect(img.png).toEqual(new Uint8Array([0x89, 0x50, 0x4e, 0x47]));
		const { viewBox, width, height } = svgFrame(call.svg);
		const [x0, y0, w, h] = viewBox as [number, number, number, number];
		expect([x0, y0, x0 + w, y0 + h].map(v => Math.round(v * 1000) / 1000)).toEqual(img.box.map(v => Math.round(v * 1000) / 1000));
		expect(width).toBe(`${w.toFixed(4)}mm`);
		expect(height).toBe(`${h.toFixed(4)}mm`);

		const inst = analysis.project.sheet("MCU");
		const u1 = inst.sch.symbols().find(s => inst.sch.refOf(s, inst.path) === "U1")!;
		const body = inst.sch.bodyBox(u1);
		const pts = [
			[body[0], body[1]],
			[body[2], body[3]],
			...["Reference", "Value"].map(f => at(prop(u1, f)!).p),
		];
		const xs = pts.map(p => p[0]!);
		const ys = pts.map(p => p[1]!);
		// everything of U1 inside, and exactly the margin around its extremes
		expect(img.box[0]).toBeCloseTo(Math.min(...xs) - 5, 3);
		expect(img.box[1]).toBeCloseTo(Math.min(...ys) - 5, 3);
		expect(img.box[2]).toBeCloseTo(Math.max(...xs) + 5, 3);
		expect(img.box[3]).toBeCloseTo(Math.max(...ys) + 5, 3);
		expect(w).toBeLessThan(297);
	});

	test("box: margin added and clamped to the page", async () => {
		const box: Box = [100, 100, 150, 130];
		const img = await renderSheet(analysis, { sheet: "Power", box, margin: 5 }, stub);
		expect(img.box).toEqual([95, 95, 155, 135]);
		expect(svgFrame(calls.at(-1)!.svg)).toEqual({ viewBox: [95, 95, 60, 40], width: "60.0000mm", height: "40.0000mm" });
		expect([calls.at(-1)!.w, calls.at(-1)!.h]).toEqual([1600, 1600]);
		expect(longestPx(calls.at(-1)!)).toBeCloseTo(1600, 3);

		const edge = await renderSheet(analysis, { sheet: "Power", box: [0, 0, 20, 20] }, stub);
		expect(edge.box).toEqual([0, 0, 28, 28]);
	});

	test("whole sheet without refs or box; unknown refs are named in the error", async () => {
		const img = await renderSheet(analysis, { sheet: "root" }, stub);
		expect(img.box).toEqual([0, 0, 297, 210]);
		expect(svgFrame(calls.at(-1)!.svg).viewBox).toEqual([0, 0, 297, 210]);
		const before = calls.length;
		await expect(renderSheet(analysis, { sheet: "MCU", refs: ["U1", "R999"] }, stub)).rejects.toThrow(/R999/);
		expect(calls.length).toBe(before);
	});
});

describe("kicad_render arguments", () => {
	test("exactly one of sheet, symbol, footprint, board; part options only with their part", async () => {
		const before = calls.length;
		await expect(runRender("/nonexistent", { sheet: "root", symbol: "Device:R" }, stub)).rejects.toThrow(
			"give exactly one of sheet, symbol, footprint, board (got sheet and symbol)",
		);
		await expect(runRender("/nonexistent", { symbol: "Device:R", footprint: "Resistor_SMD:R_0603_1608Metric" }, stub)).rejects.toThrow(
			"(got symbol and footprint)",
		);
		await expect(runRender("/nonexistent", { board: true, sheet: "root" }, stub)).rejects.toThrow("(got sheet and board)");
		await expect(runRender("/nonexistent", {}, stub)).rejects.toThrow(/^give exactly one of sheet, symbol, footprint, board$/);
		await expect(runRender("/nonexistent", { board: false }, stub)).rejects.toThrow("board: give true");
		await expect(runRender("/nonexistent", { footprint: "Connector_RJ:X", unit: 2 }, stub)).rejects.toThrow("unit applies to a symbol only");
		await expect(runRender("/nonexistent", { symbol: "Device:R", layers: ["F.Cu"] }, stub)).rejects.toThrow("layers applies to a footprint or the board only");
		expect(calls.length).toBe(before);
	});
});

describe.skipIf(!HAS_KICAD)("renderPart", () => {
	const RJ45_SYM = "Connector:RJ45_Abracon_ARJP11A-MASA-B-A-EMU2";
	const RJ45_FP = "Connector_RJ:RJ45_Abracon_ARJP11A-MA_Horizontal";
	let root = "";
	let project: Project;
	let libs: Libraries;
	let o: { cwd: string; repoRoot: string };

	beforeAll(async () => {
		root = stageFixture();
		project = Project.open(PROJECT, root);
		libs = await project.libs();
		o = { cwd: project.dir, repoRoot: root };
	});

	afterAll(() => cleanup(root));

	test("stock symbol: framed on KiCad's extent of the unit plus margin, pins where the symbol puts them", async () => {
		const img = await renderPart(libs, { kind: "symbol", id: RJ45_SYM, size: 900 }, stub, o);
		const call = calls.at(-1)!;
		expect([call.w, call.h]).toEqual([900, 900]);
		expect(longestPx(call)).toBeCloseTo(900, 3);
		expect(img.frames.map(f => f.unit)).toEqual([1]);
		const frame = img.frames[0]!.box;
		const [cell, ...rest] = cells(call.svg);
		expect(rest).toHaveLength(0);
		// the image is exactly the frame
		expect(svgFrame(call.svg).viewBox).toEqual([0, 0, cell!.w, cell!.h]);
		expect(cell!.w).toBeCloseTo(frame[2] - frame[0], 3);
		expect(cell!.h).toBeCloseTo(frame[3] - frame[1], 3);
		expect(cell!.viewBox.slice(2)).toEqual([cell!.w, cell!.h]);

		// every pin's connection end (sheet orientation) is a vertex of the plot at that position
		const off = cellOffset(cell!, frame);
		const pins = describeLibSymbol(libs.flatSymbol(RJ45_SYM)).pins;
		expect(pins).toHaveLength(15);
		const vs = vertices(cell!.body);
		// (pin 8 is a hidden NC pin, not drawn)
		for (const p of pins.filter(p => !p.hidden)) {
			expect([p.number, vs.some(v => near(v, [p.at[0] + off[0], p.at[1] + off[1]]))]).toEqual([p.number, true]);
		}

		// KiCad's extent: pin ends x -20.32 … 22.86, y -12.7 … 12.7, fields above at y -12.7; text sticks out a little
		const [x0, y0, x1, y1] = [frame[0] + 2, frame[1] + 2, frame[2] - 2, frame[3] - 2];
		for (const p of pins) expect(p.at[0] >= x0 && p.at[0] <= x1 && p.at[1] >= y0 && p.at[1] <= y1).toBe(true);
		expect(x0).toBeGreaterThan(-26);
		expect(x0).toBeLessThan(-20.32);
		expect(x1).toBeGreaterThan(22.86);
		expect(x1).toBeLessThan(24.5);
		expect(y0).toBeGreaterThan(-14.5);
		expect(y0).toBeLessThan(-12.7);
		expect(y1).toBeGreaterThan(12.7);
		expect(y1).toBeLessThan(14.5);
		// the probe that located the origin is gone
		expect(circles(call.svg).filter(c => Math.abs(c.r - 0.0371) < 0.001)).toHaveLength(0);
	});

	test("stock footprint: framed on KiCad's extent plus margin, pads in place, default layers", async () => {
		const img = await renderPart(libs, { kind: "footprint", id: RJ45_FP, size: 1200 }, stub, o);
		const call = calls.at(-1)!;
		expect([call.w, call.h]).toEqual([1200, 1200]);
		expect(longestPx(call)).toBeCloseTo(1200, 3);
		const frame = img.frames[0]!.box;
		expect(img.frames[0]!.unit).toBeUndefined();
		const [cell] = cells(call.svg);
		expect(cell!.w).toBeCloseTo(frame[2] - frame[0], 3);
		const off = cellOffset(cell!, frame);
		const pads = libs.footprintPads(RJ45_FP);
		expect(roundPadsAt(cell!.body, pads, off)).toBe(true);

		// KiCad's extent: pads, courtyard (x -4.04 … 15.46, y -2.3 …) and the texts: REF** at y -3.1,
		// the Value (the widest item) centred at (5.71, 21.2)
		const [x0, y0, x1, y1] = [frame[0] + 2, frame[1] + 2, frame[2] - 2, frame[3] - 2];
		for (const p of pads) expect(p.at[0] - p.size[0] / 2 >= x0 && p.at[0] + p.size[0] / 2 <= x1 && p.at[1] >= y0 && p.at[1] <= y1).toBe(true);
		expect(x0).toBeLessThan(-4.04);
		expect((x0 + x1) / 2).toBeCloseTo(5.71, 2);
		expect(y0).toBeLessThan(-3.1 - 0.5);
		expect(y0).toBeGreaterThan(-3.1 - 1.2);
		expect(y1).toBeGreaterThan(21.2 + 0.5);
		expect(y1).toBeLessThan(21.2 + 1.2);

		// copper, silkscreen, courtyard and fab colours of KiCad's built-in theme, with pad numbers
		for (const colour of ["#C83434", "#4D7FC4", "#F2EDA1", "#FF26E2", "#AFAFAF"]) expect(cell!.body).toContain(colour);
		expect(cell!.body).toContain("<desc>14</desc>");
		// F.Cu alone: the probe goes on a copper layer then, and the pads still land in place
		const fcuImg = await renderPart(libs, { kind: "footprint", id: RJ45_FP, layers: ["F.Cu"] }, stub, o);
		const fcu = cells(calls.at(-1)!.svg)[0]!;
		expect(fcu.body).toContain("#C83434");
		for (const colour of ["#4D7FC4", "#F2EDA1", "#FF26E2"]) expect(fcu.body).not.toContain(colour);
		expect(roundPadsAt(fcu.body, pads, cellOffset(fcu, fcuImg.frames[0]!.box))).toBe(true);
	});

	test("box: crops in part coordinates, at the requested size", async () => {
		const full = await renderPart(libs, { kind: "footprint", id: RJ45_FP }, stub, o);
		const fullOff = cellOffset(cells(calls.at(-1)!.svg)[0]!, full.frames[0]!.box);
		// pads 1 … 10 only
		const box: Box = [-1.5, -1.5, 12.93, 4.04];
		const img = await renderPart(libs, { kind: "footprint", id: RJ45_FP, box, margin: 0, size: 1000 }, stub, o);
		const call = calls.at(-1)!;
		expect(img.frames[0]!.box).toEqual(box);
		expect(svgFrame(call.svg)).toEqual({ viewBox: [0, 0, 14.43, 5.54], width: "14.4300mm", height: "5.5400mm" });
		const [cell] = cells(call.svg);
		expect(cell!.viewBox[0]).toBeCloseTo(box[0] + fullOff[0], 4);
		expect(cell!.viewBox[1]).toBeCloseTo(box[1] + fullOff[1], 4);
		expect(cell!.viewBox.slice(2)).toEqual([14.43, 5.54]);
		expect(longestPx(call)).toBeCloseTo(1000, 3);

		const sym = await renderPart(libs, { kind: "symbol", id: RJ45_SYM, box: [-20.32, -10.16, 0, 10.16], margin: 1 }, stub, o);
		expect(sym.frames[0]!.box).toEqual([-21.32, -11.16, 1, 11.16]);
		expect(svgFrame(calls.at(-1)!.svg).viewBox).toEqual([0, 0, 22.32, 22.32]);
		expect(longestPx(calls.at(-1)!)).toBeCloseTo(1600, 3);
	});

	test("units: every unit in a grid in reading order, or one; body styles", async () => {
		const all = await renderPart(libs, { kind: "symbol", id: "74xx:74LS00" }, stub, o);
		expect(all.frames.map(f => f.unit)).toEqual([1, 2, 3, 4, 5]);
		const cs = cells(calls.at(-1)!.svg);
		expect(cs).toHaveLength(5);
		// 3 columns: gates 1-3, then gate 4 and the power unit
		const grid: Pt[] = [
			[0, 0],
			[cs[0]!.w, 0],
			[cs[0]!.w + cs[1]!.w, 0],
			[0, cs[0]!.h],
			[cs[3]!.w, cs[0]!.h],
		];
		for (const [i, c] of cs.entries()) expect(near([c.x, c.y], grid[i]!, 0.001)).toBe(true);
		const { viewBox } = svgFrame(calls.at(-1)!.svg);
		expect(viewBox[2]).toBeCloseTo(cs[0]!.w + cs[1]!.w + cs[2]!.w, 3);
		expect(viewBox[3]).toBeCloseTo(cs[0]!.h + Math.max(cs[3]!.h, cs[4]!.h), 3);
		// the power unit is taller than a gate and holds VCC and GND
		expect(cs[4]!.h).toBeGreaterThan(cs[0]!.h);
		const pins = describeLibSymbol(libs.flatSymbol("74xx:74LS00")).pins.filter(p => p.unit === 5);
		const off = cellOffset(cs[4]!, all.frames[4]!.box);
		for (const p of pins) expect(vertices(cs[4]!.body).some(v => near(v, [p.at[0] + off[0], p.at[1] + off[1]]))).toBe(true);

		const one = await renderPart(libs, { kind: "symbol", id: "74xx:74LS00", unit: 2, body_style: 2 }, stub, o);
		expect(one.frames.map(f => f.unit)).toEqual([2]);
		expect(cells(calls.at(-1)!.svg)).toHaveLength(1);
		await expect(renderPart(libs, { kind: "symbol", id: "74xx:74LS00", unit: 6 }, stub, o)).rejects.toThrow("has units 1..5");
		await expect(renderPart(libs, { kind: "symbol", id: "74xx:74LS00", body_style: 3 }, stub, o)).rejects.toThrow("has body styles 1..2");
		await expect(renderPart(libs, { kind: "symbol", id: RJ45_SYM, body_style: 2 }, stub, o)).rejects.toThrow("has one body style");
		// a derived symbol keeps its parent's De Morgan body style
		await renderPart(libs, { kind: "symbol", id: "74xx:74HC00", unit: 1, body_style: 2 }, stub, o);
	});

	test("project parts resolve through ${KIPRJMOD}", async () => {
		const id = "Connector_RJ_HanRun:RJ45_HanRun_HR871150C_Horizontal";
		const img = await renderPart(libs, { kind: "footprint", id }, stub, o);
		const pads = libs.footprintPads(id);
		expect(pads.find(p => p.num === "8")?.at).toEqual([8.89, 2.54]);
		expect(roundPadsAt(cells(calls.at(-1)!.svg)[0]!.body, pads, cellOffset(cells(calls.at(-1)!.svg)[0]!, img.frames[0]!.box))).toBe(true);
		const sym = await renderPart(libs, { kind: "symbol", id: "Connector_HanRun:HR871150C" }, stub, o);
		expect(sym.frames).toHaveLength(1);
	});

	test("kicad_edit render: every touched part, from the verified copy, captioned with its id", async () => {
		const r = await runEdit(
			{
				ops: [
					{ op: "lib_copy_symbol", from: "74xx:74LS00", library: "Connector_HanRun", name: "SN74LS00_TEST" },
					{ op: "lib_copy_footprint", from: RJ45_FP, library: "Connector_RJ_HanRun", name: "RJ45_Test" },
				],
				dry_run: true,
				render: true,
			},
			root,
			stub,
		);
		expect(r.images.map(i => i.label)).toEqual(["Symbol Connector_HanRun:SN74LS00_TEST", "Footprint Connector_RJ_HanRun:RJ45_Test"]);
		const [symCall, fpCall] = calls.slice(-2);
		// the copies exist only in the transaction's copy of the project, never in the fixture
		expect(libs.symbolNames("Connector_HanRun")).not.toContain("SN74LS00_TEST");
		expect(cells(symCall!.svg)).toHaveLength(5);
		expect(padOffset(cells(fpCall!.svg)[0]!.body, libs.footprintPads(RJ45_FP))).toBeDefined();
	});
});

describe("kicad_render 3d arguments", () => {
	test("3d is for footprints and the board; 3d and 2d options don't mix", async () => {
		const before = calls.length;
		const r = (req: Parameters<typeof runRender>[1]) => runRender("/nonexistent", req, stub);
		await expect(r({ symbol: "Device:R", view: "3d" })).rejects.toThrow("view 3d applies to a footprint or the board only");
		await expect(r({ sheet: "root", view: "3d" })).rejects.toThrow("view 3d applies to a footprint or the board only");
		await expect(r({ footprint: "A:B", view: "2.5d" })).rejects.toThrow('view must be "2d" or "3d"');
		await expect(r({ footprint: "A:B", side: "top", zoom: 2 })).rejects.toThrow('side, zoom: for view "3d" only');
		await expect(r({ board: true, side: "bottom", zoom: 2 })).rejects.toThrow('zoom: for view "3d" only');
		await expect(r({ footprint: "A:B", view: "3d", box: [0, 0, 1, 1], layers: ["F.Cu"] })).rejects.toThrow('layers, box: for view "2d" only');
		expect(calls.length).toBe(before);
	});
});

describe.skipIf(!HAS_KICAD)("renderFootprint3d", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	/** Width and height from a PNG's IHDR. */
	const pngSize = (png: Uint8Array) => {
		const b = Buffer.from(png);
		expect(b.subarray(0, 8)).toEqual(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]));
		return [b.readUInt32BE(16), b.readUInt32BE(20)];
	};

	test("stock footprint with its stock model: rendered by KiCad, the model found", async () => {
		const before = calls.length;
		const img = await runRender(
			root,
			{ project: PROJECT, footprint: "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm", view: "3d", rotate: [-45, 0, 45], size: 240 },
			stub,
		);
		// KiCad's own PNG, no SVG rasterising
		expect(calls.length).toBe(before);
		expect(pngSize(img.png)).toEqual([240, 240]);
		const lines = img.text.split("\n");
		// courtyard ±3.7 × ±2.7 plus 1 mm
		expect(lines[0]).toBe(
			"Footprint Package_SO:SOIC-8_3.9x4.9mm_P1.27mm in 3D, from top, board rotated [-45, 0, 45]°, zoom 0.7, on a 1.6 mm scratch board [-4.70, -3.70, 4.70, 3.70] mm (courtyard + 1 mm, footprint coordinates).",
		);
		expect(lines[1]).toBe("3D models:");
		expect(lines[2]).toMatch(
			/^- \$\{KICAD10_3DMODEL_DIR\}\/Package_SO\.3dshapes\/SOIC-8_3\.9x4\.9mm_P1\.27mm\.stpZ → \/\S+\/Package_SO\.3dshapes\/SOIC-8_3\.9x4\.9mm_P1\.27mm\.stpZ: found$/,
		);
		expect(lines).toHaveLength(3);
	});

	test("a missing project model is reported; ${KIPRJMOD} is the real project directory", async () => {
		const req = { project: PROJECT, footprint: "Connector_RJ_HanRun:RJ45_HanRun_HR871150C_Horizontal", view: "3d", size: 160 };
		const rel = "hardware/lib/Connector_RJ_HanRun.3dshapes/RJ45_HanRun_HR871150C_Horizontal.step";
		const model = path.join(root, rel);
		const aside = `${model}.aside`;
		fs.renameSync(model, aside);
		const missing = await runRender(root, req, stub);
		expect(pngSize(missing.png)).toEqual([160, 160]);
		expect(missing.text).toContain("[-5.03, -3.01, 16.46, 21.19] mm");
		expect(missing.text).toContain(
			`- \${KIPRJMOD}/../lib/Connector_RJ_HanRun.3dshapes/RJ45_HanRun_HR871150C_Horizontal.step → ${model}: MISSING (no such file; nothing of it is drawn)`,
		);
		expect(missing.text).toContain("WARNING: 1 of 1 model(s) missing");

		fs.renameSync(aside, model);
		const found = await runRender(root, req, stub);
		expect(found.text).toContain(`→ ${model}: found`);
		expect(found.text).not.toContain("WARNING");
		// the jack's body now covers the pads seen from the top
		expect(Buffer.from(found.png).equals(Buffer.from(missing.png))).toBe(false);
	});
});
