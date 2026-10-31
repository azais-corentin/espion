import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Board, toBoard } from "../src/board";
import { Copper, shape, shapeDistance } from "../src/copper";
import type { Pt } from "../src/geom";
import { offsetPolyline, pairPaths } from "../src/ops/copper";
import { offsetPolygon } from "../src/ops/zones";
import { Project } from "../src/project";
import { OPS } from "../src/registry";
import type { Rasterize } from "../src/render";
import { child, children, L, type List, num, parse, print, str, text } from "../src/sexpr";
import { validate } from "../src/spec";
import { runChecks, runQuery } from "../src/tools";
import { type EditRequest, runEdit } from "../src/txn";
import type { OpContext } from "../src/types";
import { BOARD_FIXTURE, BOARD_PROJECT, cleanup, HAS_KICAD, stageBoardFixture } from "./helpers";

const near = (a: readonly number[], b: readonly number[], eps = 1e-9) => a.length === b.length && a.every((v, i) => Math.abs(v - b[i]!) <= eps);

/** Distance from p to segment ab. */
function segDist(p: Pt, a: Pt, b: Pt): number {
	const [dx, dy] = [b[0] - a[0], b[1] - a[1]];
	const t = Math.max(0, Math.min(1, ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy)));
	return Math.hypot(p[0] - a[0] - t * dx, p[1] - a[1] - t * dy);
}

describe("pair and outline geometry", () => {
	test("a pair along an L: (width + gap) apart on every leg, mitred at the turn, P on the side of its pad; pads swapped at the end can't be reached", () => {
		const centre: Pt[] = [
			[0, 0],
			[10, 0],
			[10, 10],
		];
		const pitch = 0.2 + 0.25;
		const left = offsetPolyline(centre, pitch / 2);
		// left of travel on screen: up while going right, +x while going down
		expect(left.every((p, i) => near(p, ([[0, -0.225], [10.225, -0.225], [10.225, 10]] as Pt[])[i]!))).toBe(true);
		const { p, n, side } = pairPaths(centre, pitch, [
			[0, 1],
			[0, -1],
		]);
		expect(side).toBe(-1);
		// each leg of one track runs exactly the pitch from the matching leg of the other (the line through it)
		const lineDist = (q: Pt, a: Pt, b: Pt) => Math.abs((b[0] - a[0]) * (q[1] - a[1]) - (b[1] - a[1]) * (q[0] - a[0])) / Math.hypot(b[0] - a[0], b[1] - a[1]);
		for (let i = 0; i < 2; i++) for (const q of [p[i]!, p[i + 1]!]) expect(lineDist(q, n[i]!, n[i + 1]!)).toBeCloseTo(pitch, 12);
		// P (pad below the start) runs on the right of travel: below the first leg, left of the second
		expect(near(p[0]!, [0, 0.225])).toBe(true);
		expect(near(p[2]!, [9.775, 10])).toBe(true);
		const ok = pairPaths(
			centre,
			pitch,
			[
				[0, 1],
				[0, -1],
			],
			[
				[9, 11],
				[11, 11],
			],
		);
		expect(ok.side).toBe(-1);
		expect(() =>
			pairPaths(
				centre,
				pitch,
				[
					[0, 1],
					[0, -1],
				],
				[
					[11, 11],
					[9, 11],
				],
			),
		).toThrow("the tracks would have to cross: cross them at vias");
		expect(() =>
			offsetPolyline(
				[
					[0, 0],
					[10, 0],
					[0, 1],
				],
				0.2,
			),
		).toThrow("the turn is sharper than 120°");
	});

	test("polygon offsets: a square and an L inset, a square grown, a margin that folds the outline refused; either winding", () => {
		const square: Pt[] = [
			[0, 0],
			[10, 0],
			[10, 10],
			[0, 10],
		];
		expect(offsetPolygon(square, 1)).toEqual([
			[1, 1],
			[9, 1],
			[9, 9],
			[1, 9],
		]);
		expect(offsetPolygon([...square].reverse(), 1)).toEqual([
			[1, 9],
			[9, 9],
			[9, 1],
			[1, 1],
		]);
		expect(offsetPolygon(square, -1)).toEqual([
			[-1, -1],
			[11, -1],
			[11, 11],
			[-1, 11],
		]);
		const l: Pt[] = [
			[0, 0],
			[10, 0],
			[10, 4],
			[4, 4],
			[4, 10],
			[0, 10],
		];
		expect(offsetPolygon(l, 1).map(p => p.map(v => Math.round(v * 1e9) / 1e9))).toEqual([
			[1, 1],
			[9, 1],
			[9, 3],
			[3, 3],
			[3, 9],
			[1, 9],
		]);
		expect(() => offsetPolygon(square, 6)).toThrow("a margin of 6 mm folds the outline over itself");
	});
});

/** The copper fixture as the board fixture project's board (see copper.test.ts). */
const COPPER_BOARD = path.join(BOARD_FIXTURE, "copper.kicad_pcb");

describe.skipIf(!HAS_KICAD)("board write ops on the copper fixture", () => {
	let root = "";
	let dir = "";
	let boardFile = "";
	const edit = (ops: Record<string, unknown>[], more: Partial<EditRequest> = {}) => runEdit({ project: BOARD_PROJECT, ops, ...more }, root, undefined, undefined, () => []);
	const query = async (what: string, args: Record<string, unknown> = {}) => (await runQuery(root, { project: BOARD_PROJECT, what, args })) as Record<string, any>;
	/** Apply ops in memory (no transaction), on a fresh project; returns the context. */
	const inMemory = async (steps: [string, Record<string, unknown>][], project = Project.open(BOARD_PROJECT, root)) => {
		const ctx: OpContext = { project, libs: await project.libs(), notes: [], touch() {}, touchPart() {}, touchBoard() {} };
		for (const [op, args] of steps) await OPS.get(op)!.apply(ctx, validate(op, args, OPS.get(op)!.fields));
		return ctx;
	};

	beforeEach(() => {
		root = stageBoardFixture();
		dir = path.join(root, BOARD_PROJECT);
		boardFile = path.join(dir, "board.kicad_pcb");
		fs.copyFileSync(COPPER_BOARD, boardFile);
	});
	afterEach(() => cleanup(root));

	test("add_track pad to pad closes /VIN's gap: net and Default width found, unconnected 1 → 0, the committed board carries KiCad's zone fills; the render shows the touched copper", async () => {
		const svgs: string[] = [];
		const stub: Rasterize = async svg => {
			svgs.push(new TextDecoder().decode(svg));
			return new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
		};
		const r = await runEdit({ project: BOARD_PROJECT, ops: [{ op: "add_track", layer: "F.Cu", from: "R1.1", to: "C1.1" }], render: true }, root, stub, undefined, () => []);
		if (!r.applied) throw new Error(r.report);
		// the track's box grown by half its width and 1 mm, plus the render's 3 mm margin
		expect(r.images.map(i => i.label)).toEqual(["Board, region [5.08, 5.90, 13.32, 24.10] mm (outline, F.Cu, courtyards; /VIN highlighted)"]);
		// the new track in the highlight colour over KiCad's plot
		expect(svgs.at(-1)).toContain('<line x1="9.1750" y1="10.0000" x2="9.2250" y2="20.0000" stroke="#FFE14D" stroke-width="0.2000"');
		expect(r.report).toContain("track /VIN on F.Cu, 0.2 mm: 1 segment, 10.0001 mm, start (9.175, 10) on pad R1.1, end (9.225, 20) on pad C1.1");
		expect(r.report).toContain("Unconnected items: 1 before → 0 after (not gated).");
		const c = Copper.of(Board.load(boardFile));
		expect(c.connectivity("/VIN").unrouted).toEqual([]);
		expect(c.tracks.filter(t => t.net === "/VIN" && t.width === 0.2).map(t => [t.start, t.end])).toEqual([
			[
				[9.175, 10],
				[9.225, 20],
			],
		]);
		// the gate filled GND_B and saved its fills with the board
		expect(c.zones[0]!.fills.length).toBeGreaterThan(0);
		expect(c.connectivity("GND").clusters).toHaveLength(1);
	}, 120_000);

	test("add_track refuses an end on another net's copper, pads of two nets and a net that differs from the pad's; nothing written", async () => {
		const bytes = fs.readFileSync(boardFile);
		await expect(edit([{ op: "add_track", layer: "F.Cu", from: "R1.1", to: [25.7875, 11] }])).rejects.toThrow(/end \(25\.7875, 11\) lands on track c0ffee00 of LED_A on F\.Cu, not on \/VIN/);
		await expect(edit([{ op: "add_track", layer: "F.Cu", from: "R1.1", to: "D1.2" }])).rejects.toThrow("R1.1 is on /VIN but D1.2 is on LED_A");
		await expect(edit([{ op: "add_track", layer: "F.Cu", from: "R1.1", points: [[5, 5]], net: "GND" }])).rejects.toThrow("net GND differs from R1.1's net /VIN");
		await expect(edit([{ op: "add_track", layer: "B.Cu", from: "R1.1", points: [[5, 5]] }])).rejects.toThrow("R1.1 is on F.Cu, not B.Cu: route it on its own layer or through a via");
		await expect(edit([{ op: "add_track", layer: "F.Cu", points: [[2, 2], [3, 2]], net: "GND" }])).rejects.toThrow("neither end lands on GND copper on F.Cu");
		expect(fs.readFileSync(boardFile).equals(bytes)).toBe(true);
	}, 60_000);

	test("a free end is routing in progress: reported with its position, not gated", async () => {
		const r = await edit([{ op: "add_track", layer: "F.Cu", from: "R1.1", points: [[5, 10]] }]);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toContain("end (5, 10) free (a free end shows as track_dangling until it is routed on)");
		const routing = r.report.split("Routing in progress, new unconnected items and dangling tracks or vias (not gated):\n")[1]!;
		expect(routing).toMatch(/^ {2}- \[drc\] track_dangling: [^\n]* @ \(/);
	}, 120_000);

	test("add_via: net from the copper under it, size and drill from its net class; no copper or two nets refused", async () => {
		const cls = await edit([
			{ op: "set_netclass", name: "PWR", via_diameter: 0.8, via_drill: 0.4 },
			{ op: "set_netclass_patterns", netclass: "PWR", patterns: ["/VIN"] },
		]);
		if (!cls.applied) throw new Error(cls.report);
		const ctx = await inMemory([["add_via", { at: [9.175, 12] }]]);
		expect(ctx.notes[0]).toMatch(/^via \/VIN at \(9\.175, 12\): 0\.8\/0\.4 mm, through F\.Cu–B\.Cu \(class PWR\); uuid /);
		const via = children(ctx.project.board().tree, "via").at(-1)!;
		expect([num(child(via, "size")), num(child(via, "drill")), text(child(via, "net"))]).toEqual([0.8, 0.4, "/VIN"]);
		// on GND_B's area only: no net's copper until KiCad fills it
		await expect(inMemory([["add_via", { at: [5, 5] }]])).rejects.toThrow("no net's copper at (5, 5) on F.Cu, B.Cu: give net");
		await expect(inMemory([["add_via", { at: [9.175, 12], net: "GND" }]])).rejects.toThrow("the copper at (9.175, 12) is on /VIN, not GND");
		await expect(inMemory([["add_via", { at: [5, 5], net: "GND", type: "blind" }]])).rejects.toThrow("a blind via needs layers");
		const r = await edit([{ op: "add_via", at: [9.175, 12] }], { dry_run: true });
		expect(r.report).toStartWith("Dry run: all checks pass; nothing written.");
	}, 120_000);

	test("stitch_vias: a grid skips other nets' copper, pads and the outline edge; rows along a net's tracks", async () => {
		const ctx = await inMemory([["stitch_vias", { net: "GND", region: { rect: { x: 6, y: 14, width: 8, height: 18 } }, pitch: 1.5 }]]);
		expect(ctx.notes[0]).toMatch(/^stitch_vias GND: \d+ placed, \d+ skipped \(\d+ copper, \d+ pads, \d+ outline(, \d+ holes)?\); 0\.6\/0\.3 mm, pitch 1\.5 mm$/);
		const c = Copper.of(ctx.project.board());
		const fresh = c.vias.filter(v => !v.uuid.startsWith("c0ffee00"));
		expect(fresh.length).toBeGreaterThan(10);
		const vinCopper = [...c.connectivity("/VIN").tracks, ...c.pads.filter(p => p.ref === "C1")];
		for (const v of fresh) {
			expect(v.net).toBe("GND");
			// inside the 40 × 30 outline with the 0.5 mm edge clearance
			expect(v.at[0] - 0.3 >= 0.5 && v.at[1] - 0.3 >= 0.5 && v.at[1] + 0.3 <= 29.5).toBe(true);
			const gap = Math.min(...vinCopper.flatMap(it => it.shapes.map(s => shapeDistance(shape([v.at], 0.3), s)!.d)));
			expect(gap).toBeGreaterThanOrEqual(0.2 - 1e-9);
		}
		const rows = await inMemory([["stitch_vias", { net: "GND", along: { net: "LED_A", offset: 1 }, pitch: 2 }]]);
		const placed = Copper.of(rows.project.board()).vias.filter(v => !v.uuid.startsWith("c0ffee00"));
		expect(placed.length).toBeGreaterThan(4);
		// a row on each side of the B.Cu track at y = 13 (x 20 … 24)
		expect(placed.some(v => near(v.at, [22, 12]))).toBe(true);
		expect(placed.some(v => near(v.at, [22, 14]))).toBe(true);
		await expect(inMemory([["stitch_vias", { net: "GND", region: { rect: { x: 30, y: 18, width: 8, height: 10 } }, pitch: 2 }]])).rejects.toThrow(/no via position fits \(\d+ tried; skipped: \d+ rule area\)/);
	}, 60_000);

	test("stitch_vias honours the project's custom rules through KiCad's DRC: a .kicad_dru clearance drops the vias too near /VIN, the rest pass the gate", async () => {
		// only between /VIN and vias, so the fixture's own copper keeps its old violations
		fs.writeFileSync(path.join(dir, "board.kicad_dru"), '(version 1)\n(rule "VIN spacing"\n\t(constraint clearance (min 1.5mm))\n\t(condition "A.NetName == \'/VIN\' && B.Type == \'Via\'"))\n');
		const ops = [{ op: "stitch_vias", net: "GND", region: { rect: { x: 6, y: 12, width: 8, height: 6 } }, pitch: 1 }];
		const r = await edit(ops);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toMatch(/stitch_vias GND: \d+ placed, \d+ skipped \([^)]*\), \d+ of them by KiCad's DRC \(\d+ clearance \(VIN spacing\)\)/);
		const c = Copper.of(Board.load(boardFile));
		const fresh = c.vias.filter(v => !v.uuid.startsWith("c0ffee00"));
		expect(fresh.length).toBeGreaterThan(5);
		const vin = c.connectivity("/VIN").tracks.flatMap(t => t.shapes);
		for (const v of fresh) expect(Math.min(...vin.map(s => shapeDistance(v.shapes[0]!, s)!.d))).toBeGreaterThanOrEqual(1.5 - 1e-6);
	}, 180_000);

	test("zones and rule areas of a footprint are refused by the zone ops, by name or by a point", async () => {
		const project = Project.open(BOARD_PROJECT, root);
		const keep = L(
			"zone",
			L("layers", str("F.Cu")),
			L("uuid", str("0f0f0f0f-0000-4000-8000-000000000001")),
			L("name", str("FP_KEEP")),
			L("hatch", "edge", 0.5),
			L("connect_pads", L("clearance", 0)),
			L("min_thickness", 0.25),
			L("keepout", L("tracks", "not_allowed"), L("vias", "not_allowed"), L("pads", "allowed"), L("copperpour", "allowed"), L("footprints", "allowed")),
			L("polygon", L("pts", L("xy", 9, 11), L("xy", 11, 11), L("xy", 11, 13), L("xy", 9, 13))),
		);
		project.board().footprint("R1").push(keep);
		await expect(inMemory([["remove_rule_area", { name: "FP_KEEP" }]], project)).rejects.toThrow('the rule area "FP_KEEP" 0f0f0f0f belongs to footprint R1');
		await expect(inMemory([["set_rule_area", { at: [10, 12], forbid: ["vias"] }]], project)).rejects.toThrow("belongs to footprint R1");
		expect(children(project.board().footprint("R1"), "zone")).toHaveLength(1);
	});

	test("a via on no copper of its net is via_dangling: reported, not gated", async () => {
		const r = await edit([{ op: "add_via", at: [5, 5], net: "LED_A" }]);
		if (!r.applied) throw new Error(r.report);
		const routing = r.report.split("Routing in progress, new unconnected items and dangling tracks or vias (not gated):\n")[1]!;
		expect(routing).toMatch(/^ {2}- \[drc\] via_dangling: [^\n]* @ \(5, 5\)$/m);
	}, 120_000);

	test("blind and micro vias on a 4-layer board: written as KiCad keeps them, the gate passes", async () => {
		const four = parse(fs.readFileSync(path.join(BOARD_FIXTURE, "four-layer.kicad_pcb"), "utf8"));
		const b = parse(fs.readFileSync(boardFile, "utf8"));
		b[b.indexOf(child(b, "layers")!)] = child(four, "layers")!;
		const setup = child(b, "setup")!;
		setup[setup.indexOf(child(setup, "stackup")!)] = child(child(four, "setup")!, "stackup")!;
		fs.writeFileSync(boardFile, `${print(b)}\n`);
		await expect(inMemory([["add_via", { at: [5, 5], net: "GND", type: "blind", layers: ["In1.Cu", "In2.Cu"] }]])).rejects.toThrow("In1.Cu–In2.Cu would be buried");
		await expect(inMemory([["add_via", { at: [5, 5], net: "GND", type: "micro", layers: ["F.Cu", "In2.Cu"] }]])).rejects.toThrow("a micro via joins two adjacent copper layers");
		const r = await edit([
			{ op: "add_via", at: [5, 5], net: "GND", type: "blind", layers: ["In2.Cu", "F.Cu"] },
			{ op: "add_via", at: [5, 8], net: "GND", type: "micro", layers: ["B.Cu", "In2.Cu"] },
		]);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toContain("via GND at (5, 5): 0.6/0.3 mm, blind F.Cu–In2.Cu (class Default)");
		expect(r.report).toContain("via GND at (5, 8): 0.3/0.1 mm, micro In2.Cu–B.Cu (class Default)");
		const vias = children(parse(fs.readFileSync(boardFile, "utf8")), "via").filter(v => !(text(child(v, "uuid")) ?? "").startsWith("c0ffee00"));
		expect(vias.map(v => [text(v, 1), text(child(v, "layers"), 1), text(child(v, "layers"), 2)])).toEqual([
			["blind", "F.Cu", "In2.Cu"],
			["micro", "In2.Cu", "B.Cu"],
		]);
	}, 120_000);

	test("remove_copper: half a tuning pattern refused, all of it removes the pattern too; a path between pads; no match is an error", async () => {
		const tuned = (p: Project) => {
			p.board().add(
				L(
					"generated",
					L("uuid", str("9e9e9e9e-0000-4000-8000-000000000001")),
					L("type", "tuning_pattern"),
					L("name", str("Tuning Pattern")),
					L("layer", str("F.Cu")),
					L("last_netname", str("LED_A")),
					L("members", str("c0ffee00-0000-4000-8000-000000000001"), str("c0ffee00-0000-4000-8000-000000000003")),
				),
			);
			return p;
		};
		await expect(inMemory([["remove_copper", { at: [12, 10] }]], tuned(Project.open(BOARD_PROJECT, root)))).rejects.toThrow(
			"these items include 1 of the 2 tracks of tuning pattern Tuning Pattern (9e9e9e9e, LED_A): select all of them (e.g. net and layer) or none",
		);
		const all = await inMemory([["remove_copper", { net: "LED_A", layer: "F.Cu", kind: "track" }]], tuned(Project.open(BOARD_PROJECT, root)));
		expect(all.notes[0]).toBe("removed 5 tracks and 0 vias (LED_A), with tuning pattern Tuning Pattern");
		expect(children(all.project.board().tree, "generated")).toEqual([]);
		const path_ = await inMemory([["remove_copper", { from: "R1.2", to: "D1.2" }]]);
		expect(path_.notes[0]).toBe("removed 6 tracks and 2 vias (LED_A)");
		expect(Copper.of(path_.project.board()).connectivity("LED_A").tracks).toEqual([]);
		const one = await inMemory([["remove_copper", { at: [20, 13], kind: "via" }]]);
		expect(one.notes[0]).toBe("removed 0 tracks and 1 via (LED_A)");
		await expect(inMemory([["remove_copper", { at: [35, 5] }]])).rejects.toThrow("nothing matches");
		await expect(inMemory([["remove_copper", { layer: "F.Cu" }]])).rejects.toThrow("give at, box, net and/or from+to");
	}, 60_000);

	test("set_track_width re-widths the selected tracks only", async () => {
		const ctx = await inMemory([["set_track_width", { width: 0.3, net: "LED_A", layer: "B.Cu" }]]);
		expect(ctx.notes[0]).toBe("track width 0.3 mm on 1 track (was 0.25 mm)");
		const c = Copper.of(ctx.project.board());
		expect(c.tracks.filter(t => t.width === 0.3).map(t => t.layer)).toEqual(["B.Cu"]);
		await expect(inMemory([["set_track_width", { width: 0.3, net: "LED_A", box: [0, 25, 5, 29] }]])).rejects.toThrow("no track matches");
	}, 60_000);

	test('add_zone "board" with a margin: inset outline, KiCad fills it in the transaction and the committed file keeps the fills; set_zone and remove_zone', async () => {
		const r = await edit([{ op: "add_zone", net: "GND", layers: ["F.Cu"], outline: "board", margin: 1, name: "GND_F" }]);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toMatch(/zone "GND_F" GND on F\.Cu: 4-point outline \(board outline, 1 mm in\), priority 0; clearance 0\.5, min thickness 0\.25, pads thermal \(gap 0\.5, spokes 0\.5\), islands always, solid fill; uuid /);
		const z = await query("zones");
		const gndF = z.zones.find((x: { name?: string }) => x.name === "GND_F");
		expect(gndF.outline).toEqual([
			[1, 1],
			[39, 1],
			[39, 29],
			[1, 29],
		]);
		expect(gndF.filled).toBe(true);
		expect(gndF.filled_area["F.Cu"]).toBeGreaterThan(500);
		const tree = parse(fs.readFileSync(boardFile, "utf8"));
		expect(children(tree, "zone").flatMap(x => children(x, "filled_polygon")).map(f => text(child(f, "layer")))).toContain("F.Cu");

		const s = await edit([{ op: "set_zone", name: "GND_F", clearance: 0.3, connect_pads: "solid", priority: 2 }]);
		if (!s.applied) throw new Error(s.report);
		expect((await query("zones")).zones.find((x: { name?: string }) => x.name === "GND_F")).toMatchObject({ priority: 2, filled: true, settings: { clearance: 0.3, connect_pads: "solid" } });
		await expect(edit([{ op: "set_zone", at: [20, 20] }])).rejects.toThrow('several zones match ("GND_F"');
		await expect(edit([{ op: "add_zone", net: "GND", layers: ["F.Cu"], outline: "board", name: "GND_F" }])).rejects.toThrow('a zone named "GND_F" exists already');
		const gone = await edit([{ op: "remove_zone", name: "GND_F" }]);
		if (!gone.applied) throw new Error(gone.report);
		expect((await query("zones")).zones.map((x: { name?: string }) => x.name)).toEqual(["GND_B"]);
	}, 180_000);

	test("a rule area that forbids tracks: the board's DRC refuses a track through it; set_rule_area and remove_rule_area", async () => {
		const a = await edit([{ op: "add_rule_area", name: "KEEP", layers: ["F.Cu"], outline: { rect: { x: 15, y: 20, width: 5, height: 5 } }, forbid: ["tracks", "vias"] }]);
		if (!a.applied) throw new Error(a.report);
		expect((await query("zones")).rule_areas.find((x: { name: string }) => x.name === "KEEP")).toMatchObject({ layers: ["F.Cu"], keepout: ["tracks", "vias"], outline: [[15, 20], [20, 20], [20, 25], [15, 25]] });
		const bytes = fs.readFileSync(boardFile);
		const t = await edit([{ op: "add_track", layer: "F.Cu", from: "C1.2", points: [[12, 22.5], [23, 22.5]] }]);
		expect(t.applied).toBe(false);
		expect(t.report).toStartWith("NOT APPLIED: the result failed these checks (files unchanged):\n  - [drc] items_not_allowed:");
		expect(fs.readFileSync(boardFile).equals(bytes)).toBe(true);
		const s = await edit([{ op: "set_rule_area", name: "KEEP", forbid: ["vias"], rename: "NO_VIAS" }]);
		if (!s.applied) throw new Error(s.report);
		expect((await query("zones")).rule_areas.map((x: { name: string; keepout: string[] }) => [x.name, x.keepout])).toContainEqual(["NO_VIAS", ["vias"]]);
		const r = await edit([{ op: "remove_rule_area", name: "NO_VIAS" }]);
		if (!r.applied) throw new Error(r.report);
		expect((await query("zones")).rule_areas.map((x: { name: string }) => x.name)).toEqual(["NO_COPPER"]);
	}, 180_000);

	test("board texts and drawings round-trip through the board_graphics query; remove_board_item by text and by a point on a stroke", async () => {
		const r = await edit([
			{ op: "add_board_text", text: "REV A", at: [5, 26], layer: "F.SilkS", justify: ["left"] },
			{ op: "add_board_text", text: "BACK", at: [30, 5], layer: "B.SilkS", size: 1.2, rot: 90 },
			{ op: "add_board_graphic", layer: "F.SilkS", shape: { type: "rect", start: [2, 2], end: [5, 4], width: 0.15 } },
			{ op: "add_board_graphic", layer: "Dwgs.User", shape: { type: "circle", center: [20, 5], radius: 2 } },
		]);
		if (!r.applied) throw new Error(r.report);
		const g = await query("board_graphics");
		const texts = g.texts.map(({ id, ...t }: Record<string, unknown>) => t);
		expect(texts).toContainEqual({ type: "text", text: "REV A", at: [5, 26], size: [1, 1], thickness: 0.1, rot: 0, justify: ["left"], layer: "F.SilkS", knockout: false, mirrored: false });
		expect(texts).toContainEqual({ type: "text", text: "BACK", at: [30, 5], size: [1.2, 1.2], thickness: 0.1, rot: 90, justify: [], layer: "B.SilkS", knockout: false, mirrored: true });
		const drawings = g.drawings.map(({ id, ...d }: Record<string, unknown>) => Object.fromEntries(Object.entries(d).filter(([, v]) => v !== undefined)));
		expect(drawings).toContainEqual({ type: "rect", start: [2, 2], end: [5, 4], width: 0.15, fill: false, layer: "F.SilkS" });
		expect(drawings).toContainEqual({ type: "circle", center: [20, 5], end: [22, 5], radius: 2, width: 0.1, fill: false, layer: "Dwgs.User" });
		await expect(edit([{ op: "add_board_graphic", layer: "Edge.Cuts", shape: { type: "line", start: [0, 0], end: [1, 1] } }])).rejects.toThrow("the outline belongs to set_board_edges");
		const rm = await edit([
			{ op: "remove_board_item", text: "REV A" },
			{ op: "remove_board_item", layer: "F.SilkS", at: [3.5, 4] },
		]);
		if (!rm.applied) throw new Error(rm.report);
		const after = await query("board_graphics", { layer: "F.SilkS" });
		expect(after.texts.map((t: { text: string }) => t.text)).toEqual(["ESPION"]);
		expect(after.drawings.map((d: { type: string }) => d.type)).toEqual(["line"]);
	}, 180_000);

	test("add_board_text defaults are raised to the board's min_text_height and min_text_thickness on silkscreen, not on fab layers", async () => {
		const { rules } = await query("board");
		const strict = await edit([{ op: "set_board_rules", rules: { min_text_height: 1.2, min_text_thickness: 0.15 } }]);
		if (!strict.applied) throw new Error(strict.report);
		try {
			const r = await edit([
				{ op: "add_board_text", text: "SILK", at: [5, 22], layer: "F.SilkS" },
				{ op: "add_board_text", text: "FAB", at: [5, 18], layer: "F.Fab" },
			]);
			if (!r.applied) throw new Error(r.report);
			const texts = (await query("board_graphics")).texts;
			expect(texts.find((t: { text: string }) => t.text === "SILK")).toMatchObject({ size: [1.2, 1.2], thickness: 0.15 });
			expect(texts.find((t: { text: string }) => t.text === "FAB")).toMatchObject({ size: [1, 1] });
			const rm = await edit([
				{ op: "remove_board_item", text: "SILK" },
				{ op: "remove_board_item", text: "FAB" },
			]);
			if (!rm.applied) throw new Error(rm.report);
		} finally {
			const back = await edit([{ op: "set_board_rules", rules: { min_text_height: rules.min_text_height, min_text_thickness: rules.min_text_thickness } }]);
			if (!back.applied) throw new Error(back.report);
		}
	}, 180_000);

	test("place_footprint side flips as pcbnew's default left/right flip does; flipped twice the pads are back where they were", async () => {
		const pads = (p: Project) =>
			Copper.of(p.board())
				.pads.filter(x => x.ref === "R1")
				.map(x => [x.number, x.at.map(v => Math.round(v * 1e6) / 1e6), x.copper]);
		const turned = await inMemory([["place_footprint", { ref: "R1", at: [10, 10], rot: 30 }]]);
		const before = pads(turned.project);
		const flip = await inMemory([["place_footprint", { ref: "R1", at: [10, 10], side: "bottom" }]], turned.project);
		const flipped = pads(turned.project);
		// y mirrored in the footprint's frame (pad 1 at (-0.825, 0) stays), the footprint turned to 180° − 30°
		expect(flipped.map(f => f[2])).toEqual([["B.Cu"], ["B.Cu"]]);
		expect(near(flipped[0]![1] as number[], toBoard({ at: [10, 10], rot: 150 }, [-0.825, 0]), 1e-6)).toBe(true);
		expect(flip.notes).toEqual(["R1 at (10, 10), 150°, bottom (flipped)"]);
		await inMemory([["place_footprint", { ref: "R1", at: [10, 10], side: "top" }]], turned.project);
		expect(pads(turned.project)).toEqual(before);
		// KiCad agrees: the flipped board passes the gate; the tracks left at the old pads are routing in progress
		const r = await edit([{ op: "place_footprint", ref: "R1", at: [10, 10], side: "bottom" }]);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toContain("Routing in progress");
		expect((await query("footprint", { ref: "R1" })).side).toBe("bottom");
	}, 120_000);

	test("a flip turns texts as KiCad does (a → −a for the left/right flip), mirrors only those on F.*/B.* layers, keeps their justification", async () => {
		const project = Project.open(BOARD_PROJECT, root);
		const fp = project.board().footprint("R1");
		const ref = children(fp, "property").find(p => text(p, 1) === "Reference")!;
		ref[ref.indexOf(child(ref, "at")!)] = L("at", 0, -1.43, 30);
		child(ref, "effects")!.push(L("justify", "left"));
		fp.splice(fp.indexOf(ref) + 1, 0, L("fp_text", "user", str("note"), L("at", 1, 2, 0), L("layer", str("Cmts.User")), L("uuid", str("0e0e0e0e-0000-4000-8000-000000000001")), L("effects", L("font", L("size", 1, 1)))));
		await inMemory([["place_footprint", { ref: "R1", at: [10, 10], side: "bottom" }]], project);
		const after = (h: string, pred: (l: List) => boolean) => children(fp, h).find(pred)!;
		const atoms = (l: List | undefined) => (l ?? []).slice(1).map((_, i) => text(l, i + 1));
		const r1 = after("property", p => text(p, 1) === "Reference");
		expect([text(r1, 2), ...atoms(child(r1, "at")), text(child(r1, "layer"))]).toEqual(["R1", "0", "1.43", "330", "B.SilkS"]);
		expect(atoms(child(child(r1, "effects")!, "justify"))).toEqual(["left", "mirror"]);
		const note = after("fp_text", t => text(t, 2) === "note");
		expect(atoms(child(note, "at"))).toEqual(["1", "-2", "0"]);
		expect(child(child(note, "effects")!, "justify")).toBeUndefined();
		// KiCad loads and keeps the flipped texts
		fs.writeFileSync(boardFile, project.board().serialize());
		const r = await edit([{ op: "place_footprint", ref: "R1", at: [10, 10], side: "top" }]);
		if (!r.applied) throw new Error(r.report);
		const back = children(parse(fs.readFileSync(boardFile, "utf8")), "footprint").find(f => children(f, "property").some(p => text(p, 2) === "R1"))!;
		const r1Back = children(back, "property").find(p => text(p, 1) === "Reference")!;
		expect([...atoms(child(r1Back, "at")), text(child(r1Back, "layer"))]).toEqual(["0", "-1.43", "30", "F.SilkS"]);
	}, 120_000);

	test("board-only footprints: a fiducial added and removed; a footprint linked to a symbol refused", async () => {
		const a = await edit([{ op: "add_board_footprint", fp_id: "Fiducial:Fiducial_1mm_Mask2mm", ref: "FID1", at: [35, 5], side: "bottom" }]);
		if (!a.applied) throw new Error(a.report);
		expect(a.report).toContain("Schematic parity: 0 issues after (0 new, 0 resolved).");
		const fid = await query("footprint", { ref: "FID1" });
		expect([fid.side, fid.at]).toEqual(["bottom", [35, 5]]);
		await expect(edit([{ op: "add_board_footprint", fp_id: "Fiducial:Fiducial_1mm_Mask2mm", ref: "R1", at: [35, 5] }])).rejects.toThrow("R1 is on the board already");
		await expect(edit([{ op: "remove_board_footprint", ref: "R1" }])).rejects.toThrow("R1 is linked to a symbol");
		const r = await edit([{ op: "remove_board_footprint", ref: "FID1" }]);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toContain("removed FID1 (Fiducial:Fiducial_1mm_Mask2mm)");
	}, 120_000);

	test("placement: a footprint's own keep-out (the tooling hole's ring) doesn't flag it; another footprint's courtyard on that keep-out does", async () => {
		const ctx = await inMemory([
			["add_board_footprint", { fp_id: "MountingHole:ToolingHole_1.152mm", ref: "TOOL1", at: [35, 5] }],
			["add_board_footprint", { fp_id: "Fiducial:Fiducial_1mm_Mask2mm", ref: "FID1", at: [36.5, 5] }],
		]);
		expect(Copper.of(ctx.project.board()).ruleAreas.filter(a => a.footprint === "TOOL1" && a.keepout.footprints)).toHaveLength(1);
		fs.writeFileSync(boardFile, ctx.project.board().serialize());
		const r = await runChecks(root, { project: BOARD_PROJECT, checks: ["placement"] });
		const errors = r.findings.filter(f => f.check === "placement" && f.severity === "error").map(f => f.message);
		expect(errors).toHaveLength(1);
		expect(errors[0]).toStartWith("FID1: its courtyard overlaps rule area ");
	}, 120_000);

	test("set_board_setup: title block (the rev kicad_export names its folder after), mask and tenting in the board, teardrops in the .kicad_pro", async () => {
		const r = await edit([
			{
				op: "set_board_setup",
				title: "Fixture",
				rev: "B",
				comments: ["first"],
				pad_to_mask_clearance: 0.05,
				tent_vias: false,
				grid_origin: [10, 10],
				teardrops: { td_onvia: false, round_shape: { td_maxlen: 1.5 } },
			},
		]);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toContain('board setup: title block: title "Fixture", rev "B", comments; grid_origin (10, 10); pad_to_mask_clearance 0.05 mm; vias not tented; teardrops: round_shape.td_maxlen 1 → 1.5, td_onvia true → false');
		const tree = parse(fs.readFileSync(boardFile, "utf8"));
		const tb = child(tree, "title_block")!;
		expect([text(child(tb, "title")), text(child(tb, "rev")), text(child(tb, "comment"), 2)]).toEqual(["Fixture", "B", "first"]);
		const setup = child(tree, "setup")!;
		expect(num(child(setup, "pad_to_mask_clearance"))).toBe(0.05);
		expect(text(child(child(setup, "tenting")!, "front"))).toBe("no");
		const pro = JSON.parse(fs.readFileSync(path.join(dir, "board.kicad_pro"), "utf8"));
		expect(pro.board.design_settings.teardrop_options[0].td_onvia).toBe(false);
		expect(pro.board.design_settings.teardrop_parameters.find((p: { td_target_name: string }) => p.td_target_name === "td_round_shape").td_maxlen).toBe(1.5);
		await expect(edit([{ op: "set_board_setup", teardrops: { td_bogus: true } }])).rejects.toThrow('teardrops: unknown key "td_bogus"');
	}, 120_000);

	test("add_diff_pair on a P/N pair: the two tracks (width + gap) apart, P beside its pad, fan-in from the pads", async () => {
		// give R1.1/C1.1 and R1.2/C1.2 the nets of a pair, in memory
		const project = Project.open(BOARD_PROJECT, root);
		const board = project.board();
		const setNet = (ref: string, pad: string, net: string) => {
			const fp = board.footprint(ref);
			const p = children(fp, "pad").find(x => text(x, 1) === pad)!;
			p[p.indexOf(child(p, "net")!)] = L("net", str(net));
		};
		setNet("R1", "1", "USB_D+");
		setNet("R1", "2", "USB_D-");
		const ctx = await inMemory([["add_diff_pair", { p: "USB_D+", n: "USB_D-", layer: "F.Cu", from: ["R1.1", "R1.2"], points: [[10, 12], [10, 25]] }]], project);
		expect(ctx.notes[0]).toMatch(/^diff pair USB_D\+ \/ USB_D- on F\.Cu: width 0\.2 mm, gap 0\.25 mm \(0\.45 mm centre to centre\), USB_D\+ on the right of the centreline;/);
		const c = Copper.of(board);
		const legs = (net: string) => c.tracks.filter(t => t.net === net && t.width === 0.2).map(t => [t.start, t.end] as [Pt, Pt]);
		const [p, n] = [legs("USB_D+"), legs("USB_D-")];
		// fan-in from the pad, then the offset leg
		expect(p.map(s => s.map(q => q.map(v => Math.round(v * 1e6) / 1e6)))).toEqual([
			[
				[9.175, 10],
				[9.775, 12],
			],
			[
				[9.775, 12],
				[9.775, 25],
			],
		]);
		expect(segDist(p[1]![0], n[1]![0], n[1]![1])).toBeCloseTo(0.45, 9);
		await expect(inMemory([["add_diff_pair", { p: "USB_D-", n: "USB_D+", layer: "F.Cu", points: [[10, 12], [10, 25]] }]], project)).rejects.toThrow("USB_D- is the negative net of the pair: swap p and n");
		await expect(inMemory([["add_diff_pair", { p: "/VIN", n: "GND", layer: "F.Cu", points: [[10, 12], [10, 25]] }]], project)).rejects.toThrow("/VIN and GND are not KiCad diff-pair partners");
	}, 60_000);
});
