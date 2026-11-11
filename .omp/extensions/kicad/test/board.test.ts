import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Board, flipToBack, fpRef, placement, r6, setFieldPose, setPlacement, toBoard } from "../src/board";
import { drcFindings } from "../src/checks/board";
import { type DrcReport, netlist, pcbDrc, run } from "../src/cli";
import { Copper } from "../src/copper";
import { boardOutline, checkCutouts, decodeEdges, type EdgeShape, encodeEdge, shapeIn } from "../src/edges";
import type { Pt } from "../src/geom";
import { Project } from "../src/project";
import { OPS } from "../src/registry";
import type { Rasterize } from "../src/render";
import { children, L, type List, parse, print, str, text } from "../src/sexpr";
import { validate } from "../src/spec";
import { runChecks, runQuery, runRender } from "../src/tools";
import { type EditRequest, runEdit } from "../src/txn";
import type { OpContext } from "../src/types";
import { BOARD_FIXTURE, BOARD_PROJECT, cleanup, HAS_KICAD, stageBoardFixture } from "./helpers";

/** A board outline 60 × 40 mm with a 10 × 5 mm notch in the top edge: R2 outer corners, R0.5 mill radius in the notch's concave corners. In normal form. */
const NOTCHED: EdgeShape = {
	points: [[0, 0, 2], [25, 0], [25, 5, 0.5], [35, 5, 0.5], [35, 0], [60, 0, 2], [60, 40, 2], [0, 40, 2]],
};
/** Cutouts in read-back order (top to bottom, then left to right). */
const CUTOUTS: EdgeShape[] = [{ rect: { x: 45, y: 15, width: 6, height: 4 } }, { circle: { center: [10, 20], radius: 1.6 } }, { points: [[20, 25], [30, 25], [25, 32]] }];

describe("board outline shapes", () => {
	test("validation: fillets that don't fit, self-intersection, too few vertices, cutouts outside or overlapping", () => {
		const outline = (points: number[][]) => shapeIn({ points }, "outline", false);
		expect(() => outline([[0, 0], [10, 0, 6], [10, 10, 6], [0, 10]])).toThrow(
			"outline.points: the fillets at vertices 1 and 2 need 12 mm of edge 1–2, which is 10 mm long: make the radii smaller",
		);
		expect(() => outline([[0, 0], [10, 10], [10, 0], [0, 10]])).toThrow("outline.points: not a simple polygon, edge 0–1 crosses or touches edge 2–3");
		expect(() => outline([[0, 0], [10, 0]])).toThrow("a polygon needs at least 3 vertices, got 2");
		expect(() => outline([[0, 0], [5, 0, 1], [10, 0], [10, 10]])).toThrow("vertex 1 is on a straight line");
		expect(() => shapeIn({ circle: { center: [0, 0], radius: 5 } }, "outline", false)).toThrow("give exactly one of rect, points");
		expect(() => shapeIn({ rect: { x: 0, y: 0, width: 10, height: 4, corner_radius: 2.5 } }, "outline", false)).toThrow("at most half the shorter side (2 mm)");
		const square: EdgeShape = { rect: { x: 0, y: 0, width: 10, height: 10 } };
		expect(() => checkCutouts(square, [{ circle: { center: [9.5, 5], radius: 1 } }])).toThrow("cutouts[0] is not strictly inside the outline");
		expect(() => checkCutouts(square, [{ circle: { center: [3, 5], radius: 1 } }, { circle: { center: [4.5, 5], radius: 1 } }])).toThrow("cutouts[0] and cutouts[1] overlap or touch");
		expect(() => checkCutouts(square, [{ circle: { center: [3, 5], radius: 1 } }, { circle: { center: [7, 5], radius: 1 } }])).not.toThrow();
	});

	test("drawn as lines and tangent arcs and read back as the same shapes, in normal form", () => {
		let n = 0;
		const uuid = () => `00000000-0000-0000-0000-${String(++n).padStart(12, "0")}`;
		const items = [NOTCHED, ...CUTOUTS].flatMap(s => encodeEdge(s, 0.05, uuid));
		// 8 edges and 6 fillets; a rect and a circle as one item each; the triangle's 3 lines
		expect(items.map(i => text(i, 0))).toEqual([
			...["gr_arc", "gr_line", "gr_line", "gr_arc", "gr_line", "gr_arc", "gr_line", "gr_line", "gr_arc", "gr_line", "gr_arc", "gr_line", "gr_arc", "gr_line"],
			"gr_rect",
			"gr_circle",
			"gr_line",
			"gr_line",
			"gr_line",
		]);
		// any order and direction of the items reads back the same
		const shuffled = [...items].reverse();
		expect(decodeEdges(shuffled).sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b)))).toEqual(
			[NOTCHED, ...CUTOUTS].sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b))),
		);
		// the same polygon given counter-clockwise from another vertex is stored in normal form
		const ccw: EdgeShape = { points: [[35, 5, 0.5], [25, 5, 0.5], [25, 0], [0, 0, 2], [0, 40, 2], [60, 40, 2], [60, 0, 2], [35, 0]] };
		expect(decodeEdges(encodeEdge(ccw, 0.05, uuid))).toEqual([NOTCHED]);
		// a rounded rect is a rect again
		expect(decodeEdges(encodeEdge({ rect: { x: 1, y: 2, width: 30, height: 20, corner_radius: 1.5 } }, 0.05, uuid))).toEqual([
			{ rect: { x: 1, y: 2, width: 30, height: 20, corner_radius: 1.5 } },
		]);
	});

	test("a chain whose arc is no fillet of its lines reads back raw: a square's corner cut by a concave arc not tangent to the edges", () => {
		const at = (k: string, p: Pt) => L(k, p[0], p[1]);
		const edge = (kind: string, ...pts: [string, Pt][]) => L(kind, ...pts.map(([k, p]) => at(k, p)), L("layer", str("Edge.Cuts")));
		const s45 = 3 * Math.SQRT1_2;
		const r4 = (v: number) => Math.round(v * 1e4) / 1e4;
		// the quarter circle around the corner (10, 0) meets both edges at right angles
		const items = [
			edge("gr_line", ["start", [0, 0]], ["end", [7, 0]]),
			edge("gr_arc", ["start", [7, 0]], ["mid", [10 - s45, s45]], ["end", [10, 3]]),
			edge("gr_line", ["start", [10, 3]], ["end", [10, 10]]),
			edge("gr_line", ["start", [10, 10]], ["end", [0, 10]]),
			edge("gr_line", ["start", [0, 10]], ["end", [0, 0]]),
		];
		const [shape] = decodeEdges(items);
		expect(shape).toMatchObject({ closed: true, items: [{ type: "line" }, { type: "arc", points: [[7, 0], [r4(10 - s45), r4(s45)], [10, 3]] }, { type: "line" }, { type: "line" }, { type: "line" }] });
		expect(boardOutline(items).area!).toBeCloseTo(100 - (Math.PI * 9) / 4, 1);
	});
});

describe("drc findings", () => {
	const report: DrcReport = {
		violations: [
			{
				type: "clearance",
				severity: "error",
				description: "Clearance violation (netclass 'Default' clearance 0,2000 mm; actual 0,1500 mm)",
				items: [
					{ description: "Pad 1 [GND] of R1 on F.Cu", pos: { x: 10, y: 20 }, uuid: "a" },
					{ description: "Pad 2 [/MCU/IMU_INT1] of R2 on F.Cu", pos: { x: 11, y: 20 }, uuid: "b" },
				],
			},
			{
				type: "drill_out_of_range",
				severity: "error",
				description: "Hole size out of range (board setup constraints min hole 0,3000 mm; actual 0,2000 mm)",
				items: [{ description: "PTH pad 33 [GND] of U1", pos: { x: 3, y: 4 } }],
			},
			{ type: "silk_overlap", severity: "warning", description: "Silkscreen clearance", items: [{ description: "Reference field of R36" }, { description: "Segment of D2 on F.Silkscreen" }] },
			{ type: "silk_over_copper", severity: "warning", description: "Silkscreen clipped by solder mask", items: [{ description: "Reference field of R3" }], excluded: true, comment: "ok" },
			{
				type: "copper_edge_clearance",
				severity: "error",
				description: "Board edge clearance violation (board setup constraints edge clearance 0,5000 mm; actual 0,1000 mm)",
				items: [{ description: "Pad 2 [GND] of R1 on F.Cu" }, { description: "Segment on Edge.Cuts" }],
			},
			{ type: "missing_courtyard", severity: "warning", description: "Footprint has no courtyard defined", items: [{ description: "Footprint U3" }] },
		],
		unconnected: [],
		parity: [
			{ type: "missing_footprint", severity: "warning", description: "Missing footprint H1 (M3)", items: [] },
			{ type: "extra_footprint", severity: "warning", description: "Footprint not found in schematic", items: [{ description: "Footprint J9" }] },
		],
		raw: "",
	};

	test("one finding per violation: KiCad's severity, rule type, message, position, references; keys without coordinates or nets", () => {
		const { violations, parity } = drcFindings(report);
		expect(violations).toEqual([
			{
				check: "drc",
				severity: "error",
				message:
					"clearance: Clearance violation (netclass 'Default' clearance 0,2000 mm; actual 0,1500 mm) [Pad 1 [GND] of R1 on F.Cu; Pad 2 [/MCU/IMU_INT1] of R2 on F.Cu]",
				key: "drc|clearance|Pad 1 of R1 on F.Cu|Pad 2 of R2 on F.Cu",
				at: [10, 20],
				refs: ["R1", "R2"],
			},
			{
				check: "drc",
				severity: "error",
				message: "drill_out_of_range: Hole size out of range (board setup constraints min hole 0,3000 mm; actual 0,2000 mm) [PTH pad 33 [GND] of U1]",
				key: "drc|drill_out_of_range|PTH pad 33 of U1",
				at: [3, 4],
				refs: ["U1"],
				confinedTo: "U1",
			},
			{
				check: "drc",
				severity: "warning",
				message: "silk_overlap: Silkscreen clearance [Reference field of R36; Segment of D2 on F.Silkscreen]",
				key: "drc|silk_overlap|Reference field of R36|Segment of D2 on F.Silkscreen",
				refs: ["D2", "R36"],
			},
			// a board item takes part: confined to no footprint
			{
				check: "drc",
				severity: "error",
				message:
					"copper_edge_clearance: Board edge clearance violation (board setup constraints edge clearance 0,5000 mm; actual 0,1000 mm) [Pad 2 [GND] of R1 on F.Cu; Segment on Edge.Cuts]",
				key: "drc|copper_edge_clearance|Pad 2 of R1 on F.Cu|Segment on Edge.Cuts",
				refs: ["R1"],
			},
			// "Footprint has …" names no footprint: the reference comes from the item
			{
				check: "drc",
				severity: "warning",
				message: "missing_courtyard: Footprint has no courtyard defined [Footprint U3]",
				key: "drc|missing_courtyard|Footprint U3",
				refs: ["U3"],
				confinedTo: "U3",
			},
		]);
		expect(parity).toEqual([
			{
				check: "drc",
				severity: "warning",
				message: "schematic parity, missing_footprint: Missing footprint H1 (M3)",
				key: "parity|missing_footprint|Missing footprint H1 (M3)|",
				refs: ["H1"],
			},
			{
				check: "drc",
				severity: "warning",
				message: "schematic parity, extra_footprint: Footprint not found in schematic [Footprint J9]",
				key: "parity|extra_footprint|Footprint not found in schematic|Footprint J9",
				refs: ["J9"],
				confinedTo: "J9",
			},
		]);
		// the same violation elsewhere is the same finding
		const moved = { ...report, violations: [{ ...report.violations[0]!, items: report.violations[0]!.items.map(i => ({ ...i, pos: { x: 99, y: 99 } })) }] };
		expect(drcFindings(moved).violations[0]!.key).toBe(violations[0]!.key);
		// renamed nets and references: a before report keyed with the after names matches the after report
		const after: DrcReport = {
			...report,
			violations: [{ ...report.violations[0]!, items: [{ description: "Pad 1 [GND2] of R7 on F.Cu" }, { description: "Pad 2 [/MCU/INT1] of R1 on F.Cu" }] }],
			parity: [{ type: "missing_footprint", severity: "warning", description: "Missing footprint H2 (M3)", items: [] }],
		};
		const renamed = drcFindings({ ...report, violations: [report.violations[0]!] }, new Map([["R1", "R7"], ["R2", "R1"], ["H1", "H2"]]));
		expect(renamed.violations.map(f => f.key)).toEqual(drcFindings(after).violations.map(f => f.key));
		expect(renamed.parity[0]!.key).toBe(drcFindings(after).parity[0]!.key);
	});
});

describe.skipIf(!HAS_KICAD)("board ops, queries, check and render on the fixture project", () => {
	let root: string;
	let dir: string;
	let boardFile: string;
	const o = () => ({ cwd: dir, repoRoot: root });
	const edit = (ops: Record<string, unknown>[], more: Partial<EditRequest> = {}) => runEdit({ project: BOARD_PROJECT, ops, ...more }, root, undefined, undefined, () => []);
	const query = async <T = Record<string, unknown>>(what: string, args?: Record<string, unknown>) => (await runQuery(root, { project: BOARD_PROJECT, what, args })) as T;
	const fp = async (ref: string) => query<Record<string, any>>("footprint", { ref });
	const imported = async () => {
		const r = await edit([{ op: "update_from_schematic" }]);
		expect(r.applied).toBe(true);
		return r;
	};

	beforeEach(() => {
		root = stageBoardFixture();
		dir = path.join(root, BOARD_PROJECT);
		boardFile = path.join(dir, "board.kicad_pcb");
	});
	afterEach(() => cleanup(root));

	test("set_stackup: KiCad keeps the stack-up and the board thickness is its total; dielectric count and finish checked", async () => {
		const r = await edit([{ op: "set_stackup", dielectrics: [{ thickness: 1.51, epsilon_r: 4.4 }], copper: [0.035, 0.035], finish: "ENIG", mask_thickness: 0.01 }]);
		expect(r.applied).toBe(true);
		expect(r.report).toContain("stack-up: 2 copper layers, dielectrics 1.51 core, finish ENIG; board thickness 1.6 mm (was 1.6)");
		const b = await query("board");
		expect(b.stackup).toEqual({
			dielectrics: [{ thickness: 1.51, type: "core", material: "FR4", epsilon_r: 4.4, loss_tangent: 0.02 }],
			copper: [0.035, 0.035],
			finish: "ENIG",
			mask_thickness: [0.01, 0.01],
			thickness: 1.6,
		});
		await edit([{ op: "set_stackup", dielectrics: [{ thickness: 1.2 }], copper: [0.07, 0.07] }]);
		// KiCad re-saved it: masks + copper + dielectric
		const su = Board.load(boardFile).stackup();
		expect([su.thickness, su.total, su.finish, su.dielectrics[0]!.epsilon_r]).toEqual([1.36, 1.36, "ENIG", 4.4]);
		// the same sum on a stack-up pcbnew wrote: a 4-layer 1.6 mm board saved by pcbnew 10
		const ks = Board.load(path.join(dir, "four-layer.kicad_pcb")).stackup();
		expect([ks.thickness, ks.total, ks.dielectrics.length]).toEqual([1.6, 1.6, 3]);

		await expect(edit([{ op: "set_stackup", dielectrics: [{ thickness: 0.2 }, { thickness: 1.2 }] }])).rejects.toThrow(
			"the board has 2 copper layers (F.Cu, B.Cu), so give 1 dielectric(s), got 2",
		);
		await expect(edit([{ op: "set_stackup", dielectrics: [{ thickness: 1.5 }], finish: "Gold" }])).rejects.toThrow('"finish" must be one of None, ENIG, ENEPIG, HAL SnPb');
		await expect(edit([{ op: "set_stackup", dielectrics: [{ thickness: 1.5, colour: "green" }] }])).rejects.toThrow('dielectrics[0]: unknown argument "colour"');
	}, 120_000);

	test("set_board_rules changes only the given .kicad_pro keys; unknown keys are rejected with KiCad's list", async () => {
		const pro = path.join(dir, "board.kicad_pro");
		const before = fs.readFileSync(pro, "utf8").split("\n");
		const r = await edit([{ op: "set_board_rules", rules: { min_clearance: 0.15, min_track_width: 0.127 }, severities: { silk_overlap: "ignore" } }]);
		expect(r.applied).toBe(true);
		expect(r.report).toContain("board rules: min_clearance 0 → 0.15, min_track_width 0.2 → 0.127, silk_overlap: warning → ignore");
		const after = fs.readFileSync(pro, "utf8").split("\n");
		expect(after).toHaveLength(before.length);
		expect(before.flatMap((l, i) => (l === after[i] ? [] : [[l.trim(), after[i]!.trim()]]))).toEqual([
			['"silk_overlap": "warning",', '"silk_overlap": "ignore",'],
			['"min_clearance": 0.0,', '"min_clearance": 0.15,'],
			['"min_track_width": 0.2,', '"min_track_width": 0.127,'],
		]);
		const b = await query<{ rules: Record<string, number>; severities: Record<string, string[]> }>("board");
		expect([b.rules.min_clearance, b.rules.min_track_width]).toEqual([0.15, 0.127]);
		expect(b.severities.ignore).toContain("silk_overlap");

		await expect(edit([{ op: "set_board_rules", rules: { min_clearence: 0.1 } }])).rejects.toThrow(
			'rules: unknown key "min_clearence"; KiCad\'s keys: max_error, min_clearance, min_connection, min_copper_edge_clearance',
		);
		await expect(edit([{ op: "set_board_rules", rules: { use_height_for_length_calcs: 1 } }])).rejects.toThrow('unknown key "use_height_for_length_calcs"');
		await expect(edit([{ op: "set_board_rules", severities: { silk_overlaps: "ignore" } }])).rejects.toThrow('severities: unknown DRC rule "silk_overlaps"');
		await expect(edit([{ op: "set_board_rules", severities: { silk_overlap: "off" } }])).rejects.toThrow("severities.silk_overlap: one of error, warning, ignore");
	}, 120_000);

	test("set_board_edges: KiCad's re-saved outline and cutouts read back exactly; a second call replaces them", async () => {
		const r = await edit([{ op: "set_board_edges", outline: NOTCHED, cutouts: CUTOUTS }]);
		expect(r.applied).toBe(true);
		expect(r.report).toContain("DRC: 0 violations after (0 new, 1 resolved).");
		const b = await query("board");
		expect(b.outline).toEqual(NOTCHED);
		expect(b.cutouts).toEqual(CUTOUTS);
		expect(b.outline_bbox).toEqual([0, 0, 60, 40]);
		// outline minus the notch, the corners' fillets and the cutouts (rect 24, circle 8.04, triangle 35)
		expect(b.area as number).toBeCloseTo(2400 - 50 - 4 * (4 - Math.PI) + 2 * (0.25 - Math.PI / 16) - 24 - Math.PI * 1.6 * 1.6 - 35, 0);

		const again = await edit([{ op: "set_board_edges", outline: { rect: { x: 0, y: 0, width: 50, height: 30 } } }]);
		expect(again.report).toContain("outline: rect 50 × 30 mm at (0, 0) (replaced 19 Edge.Cuts items)");
		const b2 = await query("board");
		expect([b2.outline, b2.cutouts]).toEqual([{ rect: { x: 0, y: 0, width: 50, height: 30 } }, []]);

		await expect(edit([{ op: "set_board_edges", outline: { points: [[0, 0], [10, 10], [10, 0], [0, 10]] } }])).rejects.toThrow("not a simple polygon");
		await expect(edit([{ op: "set_board_edges", outline: NOTCHED, cutouts: [{ circle: { center: [30, 3], radius: 1 } }] }])).rejects.toThrow(
			"cutouts[0] is not strictly inside the outline",
		);
		await expect(edit([{ op: "set_board_edges", outline: { points: [[0, 0], [10, 0, 6], [10, 10, 6], [0, 10]] } }])).rejects.toThrow("need 12 mm of edge 1–2");
	}, 120_000);

	test("update_from_schematic: every symbol's footprint with its link, fields, attributes, nets and models; KiCad finds no parity issue; a second run changes nothing", async () => {
		const r = await imported();
		expect(r.report).toContain("update_from_schematic: 3 added, 0 updated, 0 replaced, 0 removed");
		expect(r.report).toContain("Schematic parity: 0 issues after (0 new, 3 resolved).");
		const drc = await pcbDrc(boardFile, o(), true);
		expect(drc.parity).toEqual([]);
		expect(drc.violations.map(v => v.type)).toEqual(["invalid_outline"]);
		expect(drc.unconnected).toHaveLength(3);

		const nl = await netlist(path.join(dir, "board.kicad_sch"), o());
		const d1 = nl.components.get("D1")!;
		const led = await fp("D1");
		expect(led).toMatchObject({
			ref: "D1",
			fp_id: "Fixture:LED_Fixture_0603",
			value: "red",
			sheet: "/Led/",
			sheetfile: "led.kicad_sch",
			path: `${d1.sheetPath}${d1.uuids[0]}`,
			side: "top",
			inside: "no",
			properties: { Reference: "D1", Value: "red", Datasheet: d1.datasheet, Description: d1.description, Manufacturer: "Everlight", MPN: "19-217/R6C-AL1M2VY/3T", LCSC: "C72044", ki_fp_filters: d1.fpFilters },
			attributes: ["smd"],
		});
		expect(led.path).toMatch(/^\/[0-9a-f-]{36}\/[0-9a-f-]{36}$/);
		// pin data as KiCad's netlist gives it
		const node = (pin: string) => nl.nets.flatMap(n => n.nodes.filter(x => x.ref === "D1" && x.pin === pin).map(x => [pin, n.name, x.pinfunction, x.pintype]))[0];
		expect(node("1")).toEqual(["1", "GND", "K_1", "passive"]);
		expect(led.pads.map((p: Record<string, unknown>) => [p.number, p.net, p.pinfunction, p.pintype])).toEqual([node("1"), node("2")]);
		expect(led.models.map((m: { path: string }) => m.path)).toEqual(["${KICAD10_3DMODEL_DIR}/LED_SMD.3dshapes/LED_0603_1608Metric.stpZ"]);
		expect((await fp("C1")).attributes).toEqual(["smd", "dnp"]);
		const r1 = await fp("R1");
		expect(r1.path).toBe(`/${nl.components.get("R1")!.uuids[0]}`);
		expect(r1.sheet).toBe("/");
		// placed right of x = 0 (no outline yet), courtyards apart
		const rows = (await query<{ footprints: { ref: string; courtyard: number[] }[] }>("footprints")).footprints;
		expect(rows.map(f => f.ref)).toEqual(["C1", "D1", "R1"]);
		for (const a of rows) expect(a.courtyard[0]!).toBeGreaterThan(0);
		for (const a of rows) for (const b of rows) if (a !== b) expect(a.courtyard[2]! <= b.courtyard[0]! || b.courtyard[2]! <= a.courtyard[0]! || a.courtyard[3]! <= b.courtyard[1]! || b.courtyard[3]! <= a.courtyard[1]!).toBe(true);
		expect(drc.violations.some(v => v.type === "courtyards_overlap")).toBe(false);

		const again = await edit([{ op: "update_from_schematic" }]);
		expect(again.report).toStartWith("No file changes.");
	}, 120_000);

	test("update_from_schematic refreshes changed fields, replaces a footprint in place (on the back too) and removes extras only when asked", async () => {
		await imported();
		// R1 to the back, as pcbnew's flip would leave it
		const board = Board.load(boardFile);
		flipToBack(board.footprint("R1"));
		fs.writeFileSync(boardFile, board.serialize());
		const at = placement(board.footprint("R1")).at;

		expect((await edit([{ op: "set_fields", ref: "R1", fields: { MPN: "0603WAF1001T5E-X", Footprint: "Resistor_SMD:R_0805_2012Metric" } }])).applied).toBe(true);
		expect((await edit([{ op: "set_fields", ref: "C1", fields: { LCSC: "C1525" } }])).applied).toBe(true);
		const r = await edit([{ op: "update_from_schematic" }]);
		expect(r.applied).toBe(true);
		expect(r.report).toContain("update_from_schematic: 0 added, 1 updated, 1 replaced, 0 removed");
		expect(r.report).toContain("replaced (Footprint field changed): R1 (Resistor_SMD:R_0603_1608Metric → Resistor_SMD:R_0805_2012Metric)");
		expect(r.report).toMatch(/Schematic parity: 0 issues after \(0 new, [1-9]\d* resolved\)\./);
		const r1 = await fp("R1");
		expect([r1.fp_id, r1.at, r1.side, r1.properties.MPN]).toEqual(["Resistor_SMD:R_0805_2012Metric", at, "bottom", "0603WAF1001T5E-X"]);
		expect(new Set(r1.pads.flatMap((p: { layers: string[] }) => p.layers))).toEqual(new Set(["B.Cu", "B.Mask", "B.Paste"]));
		expect((await fp("C1")).properties.LCSC).toBe("C1525");
		expect((await pcbDrc(boardFile, o(), true)).parity).toEqual([]);

		// C1 taken off the board in the schematic: its footprint has no symbol any more
		const off = await edit([{ op: "set_attrs", ref: "C1", on_board: false }], {
			expect: { removed_parts: ["C1"], nets: [{ name: "/VIN", removed: ["C1.1"] }, { name: "GND", removed: ["C1.2"] }] },
		});
		if (!off.applied) throw new Error(off.report);
		const kept = await edit([{ op: "update_from_schematic" }], { dry_run: true });
		expect(kept.report).toContain("kept (no symbol; remove_extra removes them): C1");
		const removed = await edit([{ op: "update_from_schematic", remove_extra: true }]);
		expect(removed.report).toContain("update_from_schematic: 0 added, 0 updated, 0 replaced, 1 removed");
		expect((await query<{ footprints: { ref: string }[] }>("footprints")).footprints.map(f => f.ref)).toEqual(["D1", "R1"]);
		expect((await pcbDrc(boardFile, o(), true)).parity).toEqual([]);
	}, 180_000);

	test("update_footprint_from_library takes a library change onto the placed footprint: placement, side, lock, uuid, field texts and pad nets stay", async () => {
		await imported();
		const placed = await edit([
			{ op: "place_footprint", ref: "D1", at: [10, 10], rot: 90, side: "bottom", locked: true },
			{ op: "set_footprint_field", ref: "D1", field: "Value", offset: [0, 2], hidden: false },
		]);
		if (!placed.applied) throw new Error(placed.report);
		const before = await fp("D1");
		const uuid = text(children(Board.load(boardFile).footprint("D1"), "uuid")[0]!, 1);
		// pad 1 grows; a second pad 2 (as shield tabs share a number) joins pad 2 inside its copper
		const lib = await edit([
			{
				op: "lib_edit_footprint",
				library: "Fixture",
				name: "LED_Fixture_0603",
				pads: [{ pad: "1", size: [1.0, 0.95] }],
				add_pads: [{ number: "2", type: "smd", shape: "roundrect", at: [0.7875, 0.3], size: [0.3, 0.3] }],
			},
		]);
		if (!lib.applied) throw new Error(lib.report);
		// update_from_schematic leaves it: the Footprint field did not change
		expect((await edit([{ op: "update_from_schematic" }], { dry_run: true })).report).toContain("update_from_schematic: 0 added, 0 updated, 0 replaced, 0 removed");

		const r = await edit([{ op: "update_footprint_from_library", ref: "D1" }]);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toContain("D1 updated from library Fixture:LED_Fixture_0603");
		const after = await fp("D1");
		expect([after.at, after.rot, after.side, after.locked]).toEqual([before.at, before.rot, "bottom", true]);
		const netOf = new Map(before.pads.map((p: { number: string; net?: string }) => [p.number, p.net]));
		const pads = after.pads.map((p: { number: string; net?: string; size: number[] }) => [p.number, p.net, p.size]).sort((a: string[], b: string[]) => JSON.stringify(a).localeCompare(JSON.stringify(b)));
		expect(pads).toEqual([
			["1", netOf.get("1"), [1.0, 0.95]],
			["2", netOf.get("2"), [0.3, 0.3]],
			["2", netOf.get("2"), [0.875, 0.95]],
		]);
		expect(new Set(after.pads.flatMap((p: { layers: string[] }) => p.layers))).toEqual(new Set(["B.Cu", "B.Mask", "B.Paste"]));
		expect(after.fields).toEqual(before.fields);
		expect(text(children(Board.load(boardFile).footprint("D1"), "uuid")[0]!, 1)).toBe(uuid);
		expect((await pcbDrc(boardFile, o(), true)).parity).toEqual([]);
	}, 180_000);

	test("place_footprint moves and turns a footprint as KiCad sees it: pads where IPC-D-356 puts them, texts turned with it; locked ones stay", async () => {
		await imported();
		const before = (await fp("R1")).pads.map((p: { at: Pt }) => p.at);
		const r = await edit([{ op: "place_footprint", ref: "R1", at: [50, 40], rot: 30 }]);
		expect(r.applied).toBe(true);
		const r1 = await fp("R1");
		expect([r1.at, r1.rot]).toEqual([[50, 40], 30]);
		// R_0603 pads at (∓0.825, 0) in the footprint
		const want = [toBoard({ at: [50, 40], rot: 30 }, [-0.825, 0]), toBoard({ at: [50, 40], rot: 30 }, [0.825, 0])];
		expect(r1.pads.map((p: { at: Pt }) => p.at)).toEqual(want);
		expect(want).not.toEqual(before);
		expect(r1.pads.map((p: { rot?: number }) => p.rot)).toEqual([30, 30]);
		// KiCad's own pad positions: IPC-D-356 test records (inch / 10000, y up)
		const out = path.join(root, "out.d356");
		expect((await run(["kicad-cli", "pcb", "export", "ipcd356", "-o", out, boardFile], o())).code).toBe(0);
		const kicad = fs
			.readFileSync(out, "utf8")
			.split("\n")
			.filter(l => /^327/.test(l) && l.slice(20, 26).trim() === "R1")
			.map(l => {
				const m = /X([+-]\d+)Y([+-]\d+)/.exec(l)!;
				return [l.slice(27, 32).replace(/^-/, "").trim(), (Number(m[1]) / 1e4) * 25.4, (-Number(m[2]) / 1e4) * 25.4] as const;
			});
		expect(kicad.map(k => k[0])).toEqual(["1", "2"]);
		kicad.forEach(([, x, y], i) => {
			expect(Math.abs(x - want[i]![0])).toBeLessThan(0.003);
			expect(Math.abs(y - want[i]![1])).toBeLessThan(0.003);
		});
		// the reference text kept its pose relative to the footprint: library angle 0, now 30
		const ref = children(parse(fs.readFileSync(boardFile, "utf8")), "footprint")
			.find(f => children(f, "property").some(p => text(p, 1) === "Reference" && text(p, 2) === "R1"))!;
		const refAt = children(ref, "property").find(p => text(p, 1) === "Reference")!.find(c => Array.isArray(c) && text(c, 0) === "at") as unknown[];
		expect(text(refAt as never, 3)).toBe("30");

		expect((await edit([{ op: "place_footprint", ref: "R1", at: [50, 40], locked: true }])).applied).toBe(true);
		await expect(edit([{ op: "place_footprint", ref: "R1", at: [55, 40] }])).rejects.toThrow("R1 is locked: give locked: false to unlock and move it");
		expect((await edit([{ op: "place_footprint", ref: "R1", at: [55, 40], locked: false }])).applied).toBe(true);
		expect((await fp("R1")).locked).toBe(false);
		await expect(edit([{ op: "place_footprint", ref: "R9", at: [0, 0] }])).rejects.toThrow("no footprint R9 on the board (3 footprints: C1, D1, R1)");
	}, 120_000);

	test("a board edit that adds a DRC violation is refused: an outline edge through a placed pad", async () => {
		await imported();
		await edit([{ op: "place_footprint", ref: "R1", at: [60, 40] }]);
		const bytes = fs.readFileSync(boardFile);
		// R1's pad 2 spans x 60.425 … 61.225
		const r = await edit([{ op: "set_board_edges", outline: { rect: { x: 60.8, y: 20, width: 30, height: 30 } } }]);
		expect(r.applied).toBe(false);
		expect(r.report).toStartWith("NOT APPLIED: the result failed these checks (files unchanged):\n  - [drc] copper_edge_clearance:");
		expect(fs.readFileSync(boardFile).equals(bytes)).toBe(true);
		// the same outline clear of R1 goes through
		expect((await edit([{ op: "set_board_edges", outline: { rect: { x: 40, y: 20, width: 30, height: 30 } } }])).applied).toBe(true);
	}, 120_000);

	test("a violation with a board item is gated also for a footprint the edit placed from the libraries: update_from_schematic + place_footprint across the outline", async () => {
		expect((await edit([{ op: "set_board_edges", outline: { rect: { x: 0, y: 0, width: 30, height: 30 } } }])).applied).toBe(true);
		const bytes = fs.readFileSync(boardFile);
		// R1's pads at x 29.175 and 30.825: the outline's right edge runs between them
		const r = await edit([{ op: "update_from_schematic" }, { op: "place_footprint", ref: "R1", at: [30, 15] }]);
		expect(r.applied).toBe(false);
		expect(r.report).toStartWith("NOT APPLIED: the result failed these checks (files unchanged):\n  - [drc] copper_edge_clearance:");
		expect(r.report).toMatch(/\[drc\] copper_edge_clearance: [^\n]*of R1 on F\.Cu/);
		expect(r.report).not.toContain("inside footprints this edit placed");
		expect(fs.readFileSync(boardFile).equals(bytes)).toBe(true);
	}, 120_000);

	test("DRC gate and check: a placed footprint's own violations reported, tighter rules reported, renamed references and nets keep old violations old, KiCad exclusions as info", async () => {
		const lines = (report: string, heading: string) => (report.split(`${heading}\n`)[1] ?? "").split("\n").filter(l => l.startsWith("  - [drc] "));
		// a board minimum the library footprints break between their own pads
		expect((await edit([{ op: "set_board_rules", rules: { min_clearance: 0.9 } }])).applied).toBe(true);
		const r = await imported();
		const own = lines(r.report, "New DRC violations inside footprints this edit placed from the libraries (the library footprint against the board's rules; not gated):");
		expect(own.map(l => /^ {2}- \[drc\] clearance: Clearance violation \(board minimum clearance 0,9000 mm; [^)]*\) \[Pad \d \[[^\]]+\] of (\w+) on F\.Cu; Pad \d \[[^\]]+\] of \1 on F\.Cu\]/.exec(l)?.[1]).sort()).toEqual(["C1", "D1", "R1"]);

		// a tighter rule shows the board's problems: applied, listed, not gated
		const t = await edit([{ op: "set_board_rules", rules: { min_text_height: 1.5 } }]);
		expect(t.applied).toBe(true);
		expect(t.report).toContain("DRC: 7 violations after (3 new, 0 resolved).");
		const tight = lines(t.report, "New DRC violations under the changed board rules (not gated):");
		expect(tight.map(l => /\[Reference field of (\w+)\]/.exec(l)?.[1]).sort()).toEqual(["C1", "D1", "R1"]);

		// R1 → R5 and LED_A → LED_B in the schematic, then on the board: R1's violations are R5's now, not new ones
		const ops = [
			{ op: "rename_ref", ref: "R1", to: "R5" },
			{ op: "rename_net", from: "LED_A", to: "LED_B" },
		];
		const dry = await edit(ops, { dry_run: true });
		const expectNow = JSON.parse(dry.report.slice(dry.report.indexOf("{", dry.report.indexOf("Actual connectivity change as `expect`"))));
		expect((await edit(ops, { expect: expectNow })).applied).toBe(true);
		const up = await edit([{ op: "update_from_schematic" }]);
		expect(up.report).toStartWith("Applied.");
		expect(up.report).toContain("update_from_schematic: 0 added, 2 updated, 0 replaced, 0 removed");
		expect(up.report).toContain("DRC: 7 violations after (0 new, 0 resolved).");
		const drc = await pcbDrc(boardFile, o(), true);
		expect(drc.violations.flatMap(v => v.items.map(i => i.description))).toContain("Pad 2 [LED_B] of R5 on F.Cu");

		// KiCad's exclusion of R5's reference height (rule|x nm|y nm|item uuid|second item uuid, with a comment)
		const small = drc.violations.find(v => v.type === "text_height" && v.items[0]!.description === "Reference field of R5")!.items[0]!;
		const pro = path.join(dir, "board.kicad_pro");
		const settings = JSON.parse(fs.readFileSync(pro, "utf8"));
		settings.board.design_settings.drc_exclusions = [
			[`text_height|${Math.round(small.pos!.x * 1e6)}|${Math.round(small.pos!.y * 1e6)}|${small.uuid}|00000000-0000-0000-0000-000000000000`, "hand-soldered prototype"],
		];
		fs.writeFileSync(pro, JSON.stringify(settings, null, 2));
		const c = await runChecks(root, { project: BOARD_PROJECT, checks: ["drc"] });
		const excluded = c.findings.filter(f => f.message.startsWith("excluded in KiCad"));
		expect(excluded.map(f => [f.severity, f.message, f.refs])).toEqual([
			[
				"info",
				"excluded in KiCad (hand-soldered prototype): text_height: Text height out of range (board setup constraints silk text height min height 1,5000 mm; actual 1,0000 mm) [Reference field of R5]",
				["R5"],
			],
		]);
		expect(c.findings.filter(f => f.message.startsWith("text_height:")).map(f => f.refs![0]).sort()).toEqual(["C1", "D1"]);
	}, 240_000);

	test("set_footprint_field moves, turns, hides and re-layers field texts as KiCad re-saves them; a reference moved onto the pads is refused by DRC", async () => {
		await imported();
		const at = (await fp("R1")).at as Pt;
		const ops = [
			{ op: "set_footprint_field", ref: "R1", field: "Reference", offset: [0, -2], rot: -90 },
			{ op: "set_footprint_field", ref: "R1", field: "Value", at: [at[0], at[1] + 2], layer: "B.Fab", hidden: true },
			{ op: "set_footprint_field", ref: "R1", field: "MPN", offset: [1, 1], hidden: false },
		];
		// the same changes in memory on the file as KiCad last saved it
		const mine = Board.load(boardFile);
		const r1 = mine.footprint("R1");
		const prop = (fp: List, name: string) => children(fp, "property").find(p => text(p, 1) === name)!;
		setFieldPose(r1, prop(r1, "Reference"), { at: [at[0], at[1] - 2], rot: -90 });
		setFieldPose(r1, prop(r1, "Value"), { at: [at[0], at[1] + 2], layer: "B.Fab", hidden: true });
		setFieldPose(r1, prop(r1, "MPN"), { at: [at[0] + 1, at[1] + 1], hidden: false });
		const r = await edit(ops);
		expect(r.applied).toBe(true);
		expect(r.report).toContain(`R1 Reference at (${r6(at[0])}, ${r6(at[1] - 2)}), 270°, F.SilkS, shown`);
		expect(r.report).toContain("R1 Value at");
		expect(r.report).toContain("0°, B.Fab (mirrored), hidden");
		const saved = Board.load(boardFile).footprint("R1");
		for (const name of ["Reference", "Value", "MPN"]) expect(print(prop(saved, name))).toBe(print(prop(r1, name)));
		const fields = (await fp("R1")).fields as Record<string, unknown>[];
		const field = (name: string) => fields.find(f => f.name === name);
		expect(field("Reference")).toEqual({ name: "Reference", value: "R1", at: [at[0], r6(at[1] - 2)], rot: 270, layer: "F.SilkS", hidden: false, mirrored: false });
		expect(field("Value")).toEqual({ name: "Value", value: "1k", at: [at[0], r6(at[1] + 2)], rot: 0, layer: "B.Fab", hidden: true, mirrored: true });
		expect(field("MPN")).toMatchObject({ at: [r6(at[0] + 1), r6(at[1] + 1)], layer: "F.Fab", hidden: false });
		// back to the front: not mirrored any more
		expect((await edit([{ op: "set_footprint_field", ref: "R1", field: "Value", layer: "F.Fab", hidden: false }])).applied).toBe(true);
		expect((await fp("R1")).fields.find((f: { name: string }) => f.name === "Value")).toMatchObject({ layer: "F.Fab", hidden: false, mirrored: false });

		// the reference on its own pads: silkscreen over copper and over R1's silk lines
		const bytes = fs.readFileSync(boardFile);
		const onPads = await edit([{ op: "set_footprint_field", ref: "R1", field: "Reference", offset: [0, 0], rot: 0 }]);
		expect(onPads.applied).toBe(false);
		const silk = onPads.report.split("\n").filter(l => l.startsWith("  - [drc] "));
		// KiCad lists a violation's two items in either order
		const described = (l: string) => {
			const [, type, items] = /\[drc\] (\w+): [^[]*\[(.*)\]/.exec(l)!;
			return `${type} ${items!.split("; ").sort().join("; ")}`;
		};
		expect(silk.map(described).sort()).toEqual([
			"silk_over_copper Pad 1 [/VIN] of R1 on F.Cu; Reference field of R1",
			"silk_over_copper Pad 2 [LED_A] of R1 on F.Cu; Reference field of R1",
			"silk_overlap Reference field of R1; Segment of R1 on F.Silkscreen",
		]);
		expect(fs.readFileSync(boardFile).equals(bytes)).toBe(true);
		// hidden there, it draws nothing: no finding
		const hidden = await edit([{ op: "set_footprint_field", ref: "R1", field: "Reference", offset: [0, 0], rot: 0, hidden: true }]);
		expect(hidden.report).toContain("DRC: 1 violations after (0 new, 0 resolved).");

		await expect(edit([{ op: "set_footprint_field", ref: "R1", field: "Reference", at: [0, 0], offset: [0, 0] }])).rejects.toThrow("give at or offset, not both");
		await expect(edit([{ op: "set_footprint_field", ref: "R1", field: "Reference" }])).rejects.toThrow("give at, offset, rot, hidden and/or layer");
		await expect(edit([{ op: "set_footprint_field", ref: "R1", field: "Rating", hidden: false }])).rejects.toThrow('R1 has no field "Rating" (fields: Reference, Value, Datasheet, Description, Manufacturer, MPN, LCSC)');
		await expect(edit([{ op: "set_footprint_field", ref: "R1", field: "Value", layer: "F.Silk" }])).rejects.toThrow('layer: the board has no layer "F.Silk" (layers: F.Cu');
	}, 180_000);

	test("footprints query: a courtyard on the board with a cutout inside its box is partly inside", async () => {
		await imported();
		const board = Board.load(boardFile);
		let n = 0;
		const uuid = () => `00000000-0000-0000-0000-${String(++n).padStart(12, "0")}`;
		// R1's courtyard spans ±1.48 × ±0.73 mm: a small hole at (1.2, 0.5) from its origin, clear of its pads' edges' sample points
		board.add(...[{ rect: { x: 0, y: 0, width: 40, height: 30 } }, { circle: { center: [21.2, 15.5] as Pt, radius: 0.1 } }].flatMap(s => encodeEdge(s, 0.05, uuid)));
		setPlacement(board.footprint("R1"), [20, 15], 0);
		setPlacement(board.footprint("D1"), [10, 10], 0);
		setPlacement(board.footprint("C1"), [60, 10], 0);
		fs.writeFileSync(boardFile, board.serialize());
		const rows = (await query<{ footprints: { ref: string; inside: string }[] }>("footprints")).footprints;
		expect(rows.map(f => [f.ref, f.inside])).toEqual([
			["C1", "no"],
			["D1", "yes"],
			["R1", "partly"],
		]);
	}, 120_000);

	test("drc check: an empty board says so; then one finding per issue and the unconnected items as one count", async () => {
		const empty = await runChecks(root, { project: BOARD_PROJECT, checks: ["drc"] });
		expect(empty.report).toBe("## drc: 0 errors, 0 warnings, 1 info\n- info: the board is empty (no footprints, no outline): DRC not run");
		await imported();
		const r = await runChecks(root, { project: BOARD_PROJECT, checks: ["drc"] });
		expect(r.findings.map(f => [f.severity, f.message.split(":")[0]])).toEqual([
			["error", "invalid_outline"],
			["info", "3 unconnected items (connections not routed yet); args {unconnected"],
		]);
		const each = await runChecks(root, { project: BOARD_PROJECT, checks: ["drc"], args: { unconnected: true } });
		const un = each.findings.filter(f => f.key.startsWith("unconnected|"));
		expect(un).toHaveLength(3);
		expect(un.flatMap(f => f.refs ?? []).sort()).toEqual(["C1", "C1", "D1", "D1", "R1", "R1"]);
	}, 120_000);

	test("kicad_render board: 2D framed on the outline in board mm, back layers from below, 3D raytraced; kicad_edit renders the touched board", async () => {
		const svgs: string[] = [];
		const stub: Rasterize = async svg => {
			svgs.push(new TextDecoder().decode(svg));
			return new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
		};
		await expect(runRender(root, { project: BOARD_PROJECT, board: true }, stub)).rejects.toThrow("the board has no outline and no footprints: give a box");
		const e = await runEdit({ project: BOARD_PROJECT, ops: [{ op: "set_board_edges", outline: { rect: { x: 10, y: 10, width: 50, height: 30 } } }], render: true }, root, stub, undefined, () => []);
		expect(e.images.map(i => i.label)).toEqual(["Board, region [7.00, 7.00, 63.00, 43.00] mm (outline, copper, courtyards)"]);
		const top = await runRender(root, { project: BOARD_PROJECT, board: true, size: 800 }, stub);
		expect(top.text).toBe("Board board/board.kicad_pcb, layers Edge.Cuts, F.Cu, F.Courtyard, F.Fab, F.Silkscreen, region [7.00, 7.00, 63.00, 43.00] mm (board coordinates, seen from the top)");
		// the frame is the region; KiCad's outline path sits at its board coordinates
		expect(/viewBox="([^"]*)"/.exec(svgs.at(-1)!)?.[1]).toBe("7.0000 7.0000 56.0000 36.0000");
		expect(svgs.at(-1)).toMatch(/d="M 10\.0000,10\.0000\s+60\.0000,10\.0000\s+60\.0000,40\.0000\s+10\.0000,40\.0000\s+Z/);
		const bottom = await runRender(root, { project: BOARD_PROJECT, board: true, side: "bottom", box: [0, 0, 20, 20], margin: 0 }, stub);
		expect(bottom.text).toContain("layers Edge.Cuts, B.Cu, B.Courtyard, B.Fab, B.Silkscreen, region [0.00, 0.00, 20.00, 20.00] mm");
		const before = svgs.length;
		const r3d = await runRender(root, { project: BOARD_PROJECT, board: true, view: "3d", size: 120 }, stub);
		expect(svgs.length).toBe(before);
		expect(r3d.text).toBe("Board board/board.kicad_pcb in 3D, from top, zoom 0.85, with its footprints' 3D models.");
		const png = Buffer.from(r3d.png);
		expect([png.readUInt32BE(16), png.readUInt32BE(20)]).toEqual([120, 120]);
	}, 120_000);

	test("a lock file of the board is honoured like the others", async () => {
		fs.writeFileSync(path.join(dir, "~board.kicad_pcb.lck"), JSON.stringify({ hostname: "tan-gnu", username: "tan" }));
		await expect(edit([{ op: "set_board_rules", rules: { min_clearance: 0.1 } }])).rejects.toThrow("KiCad has the project open (board/~board.kicad_pcb.lck: locked by tan@tan-gnu)");
	});
});

describe.skipIf(!HAS_KICAD)("update_from_schematic on the copper fixture: copper follows its pads' nets", () => {
	let root: string;
	let dir: string;
	let boardFile: string;
	const o = () => ({ cwd: dir, repoRoot: root });
	const edit = (ops: Record<string, unknown>[], more: Partial<EditRequest> = {}) => runEdit({ project: BOARD_PROJECT, ops, ...more }, root, undefined, undefined, () => []);
	/** A schematic edit applied with the connectivity change its dry run reports. */
	const schematic = async (ops: Record<string, unknown>[]) => {
		const dry = await edit(ops, { dry_run: true });
		const r = await edit(ops, { expect: JSON.parse(dry.report.slice(dry.report.indexOf("{", dry.report.indexOf("Actual connectivity change as `expect`")))) });
		if (!r.applied) throw new Error(r.report);
	};
	/** Tracks (arcs included), vias and zones on a net. */
	const copperOn = (net: string, board = Board.load(boardFile)) => {
		const c = Copper.of(board);
		return [c.tracks.filter(t => t.net === net).length, c.vias.filter(v => v.net === net).length, c.zones.filter(z => z.net === net).length];
	};
	/** update_from_schematic on the board in memory, without the transaction (so without KiCad's re-save). */
	const inMemory = async (args: Record<string, unknown> = {}) => {
		const project = Project.open(BOARD_PROJECT, root);
		const ctx: OpContext = { project, libs: await project.libs(), notes: [], touch() {}, touchPart() {}, touchBoard() {} };
		const def = OPS.get("update_from_schematic")!;
		await def.apply(ctx, validate("update_from_schematic", args, def.fields));
		return ctx;
	};

	beforeEach(() => {
		root = stageBoardFixture();
		dir = path.join(root, BOARD_PROJECT);
		boardFile = path.join(dir, "board.kicad_pcb");
		fs.copyFileSync(path.join(BOARD_FIXTURE, "copper.kicad_pcb"), boardFile);
	});
	afterEach(() => cleanup(root));

	test("a renamed net: its tracks, arc, vias and zone end on the new name, in the dry run too; KiCad finds no parity issue", async () => {
		const zone = await edit([{ op: "add_zone", net: "LED_A", layers: ["F.Cu"], outline: { rect: { x: 19, y: 12, width: 6, height: 2 } }, name: "LED_A_F" }]);
		if (!zone.applied) throw new Error(zone.report);
		expect(copperOn("LED_A")).toEqual([6, 2, 1]);
		await schematic([{ op: "rename_net", from: "LED_A", to: "LED_B" }]);

		const dry = await edit([{ op: "update_from_schematic" }], { dry_run: true });
		expect(dry.report).toContain("copper of LED_A now on LED_B: 6 tracks, 2 vias, 1 zones");
		expect(copperOn("LED_A")).toEqual([6, 2, 1]);
		const r = await edit([{ op: "update_from_schematic" }]);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toContain("copper of LED_A now on LED_B: 6 tracks, 2 vias, 1 zones");
		expect(copperOn("LED_A")).toEqual([0, 0, 0]);
		expect(copperOn("LED_B")).toEqual([6, 2, 1]);
		expect((await pcbDrc(boardFile, o(), true)).parity).toEqual([]);
		expect((await edit([{ op: "update_from_schematic" }])).report).toStartWith("No file changes.");
	}, 180_000);

	test("two routed nets merged by deleting the series part between them: all copper on the surviving name, the part's footprint removed", async () => {
		const vin = copperOn("/VIN");
		const led = copperOn("LED_A");
		expect(vin[0]).toBeGreaterThan(0);
		// R1 out, its two ends wired together and on to C1.1; the global label LED_A names the net
		await schematic([
			{ op: "remove_symbol", ref: "R1" },
			{ op: "remove_label", sheet: "root", name: "VIN", at: [101.6, 97.79] },
			{ op: "remove_label", sheet: "root", name: "VIN", at: [121.92, 97.79] },
			{ op: "add_wire", sheet: "root", points: [[101.6, 105.41], [101.6, 95.25], [121.92, 95.25], [121.92, 97.79]] },
		]);
		// the op itself, before KiCad's re-save (which gives a pad's net to tracks touching only that pad)
		const ctx = await inMemory({ remove_extra: true });
		const board = ctx.project.board();
		expect(ctx.notes).toContain(`copper of /VIN now on LED_A: ${vin[0]} tracks, ${vin[1]} vias, ${vin[2]} zones`);
		expect([copperOn("/VIN", board), copperOn("LED_A", board)]).toEqual([[0, 0, 0], led.map((n, i) => n + vin[i]!)]);
		const r = await edit([{ op: "update_from_schematic", remove_extra: true }]);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toContain("removed: R1");
		expect(r.report).toContain("copper of /VIN now on LED_A");
		expect(copperOn("/VIN")).toEqual([0, 0, 0]);
		expect(copperOn("LED_A")).toEqual(led.map((n, i) => n + vin[i]!));
		expect(Board.load(boardFile).footprints().map(f => fpRef(f)).sort()).toEqual(["C1", "D1"]);
	}, 180_000);

	test("a chain of renames: /VIN's pads take the name LED_A while LED_A's take LED_B; each net's copper moves once, to its own pads' new name", async () => {
		const vin = copperOn("/VIN");
		const led = copperOn("LED_A");
		await schematic([
			{ op: "rename_net", from: "LED_A", to: "LED_B" },
			{ op: "remove_label", sheet: "root", name: "VIN", at: [101.6, 97.79] },
			{ op: "remove_label", sheet: "root", name: "VIN", at: [121.92, 97.79] },
			{ op: "add_label", sheet: "root", name: "LED_A", kind: "global", at: [101.6, 97.79] },
			{ op: "add_label", sheet: "root", name: "LED_A", kind: "global", at: [121.92, 97.79] },
		]);
		const ctx = await inMemory();
		const board = ctx.project.board();
		expect(ctx.notes.filter(n => n.startsWith("copper of "))).toEqual([
			`copper of /VIN now on LED_A: ${vin[0]} tracks, ${vin[1]} vias, ${vin[2]} zones`,
			`copper of LED_A now on LED_B: ${led[0]} tracks, ${led[1]} vias, ${led[2]} zones`,
		]);
		expect([copperOn("/VIN", board), copperOn("LED_A", board), copperOn("LED_B", board)]).toEqual([[0, 0, 0], vin, led]);
		const r = await edit([{ op: "update_from_schematic" }]);
		if (!r.applied) throw new Error(r.report);
		expect([copperOn("/VIN"), copperOn("LED_A"), copperOn("LED_B")]).toEqual([[0, 0, 0], vin, led]);
		expect((await pcbDrc(boardFile, o(), true)).parity).toEqual([]);
	}, 180_000);

	test("a split net: one pad moved to another net while another stays; no copper moves, the note reports it", async () => {
		const gnd = copperOn("GND");
		const led = copperOn("LED_A");
		// D1's cathode from GND to LED_A
		await schematic([
			{ op: "remove_symbol", ref: "#PWR2" },
			{ op: "add_label", sheet: "Led", name: "LED_A", kind: "global", at: [97.79, 114.3], rot: 180 },
		]);
		const ctx = await inMemory();
		expect(ctx.notes.filter(n => n.startsWith("copper of ")).map(n => n.split(" (")[0])).toEqual(["copper of GND not moved"]);
		expect([copperOn("GND", ctx.project.board()), copperOn("LED_A", ctx.project.board())]).toEqual([gnd, led]);
		const r = await edit([{ op: "update_from_schematic" }]);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toContain("copper of GND not moved");
		// KiCad's re-save gives copper that touches only D1.1 (its stub and via) the pad's net; the zone stays on GND
		expect(copperOn("GND")[2]).toBe(gnd[2]!);
	}, 180_000);

	test("a net whose parts were all removed: its copper stays, reported as orphaned", async () => {
		const led = copperOn("LED_A");
		await schematic([
			{ op: "set_attrs", ref: "R1", on_board: false },
			{ op: "set_attrs", ref: "D1", on_board: false },
		]);
		const r = await edit([{ op: "update_from_schematic", remove_extra: true }]);
		if (!r.applied) throw new Error(r.report);
		expect(r.report).toContain("copper of LED_A orphaned");
		expect(copperOn("LED_A")).toEqual(led);
	}, 180_000);
});
