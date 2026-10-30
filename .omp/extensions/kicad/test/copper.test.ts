import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { Board } from "../src/board";
import { pcbRefillAndSave, upgrade } from "../src/cli";
import { Copper, dpPartnerName, layerDistance, shape, shapeDistance, shapeHas } from "../src/copper";
import type { Pt } from "../src/geom";
import type { Rasterize } from "../src/render";
import { child, children, L, num, parseMany } from "../src/sexpr";
import { runChecks, runQuery, runRender } from "../src/tools";
import { BOARD_FIXTURE, BOARD_PROJECT, cleanup, HAS_KICAD, REPO, stageBoardFixture } from "./helpers";

/**
 * The copper fixture: the board fixture's R1 (10, 10), D1 (25, 10) and C1 (10, 20) on a 40 × 30 mm
 * outline, 1.51 mm dielectric and 35 µm copper, with
 * - LED_A routed R1.2 → arc (centre (14, 13), r 3) → via (20, 13) → B.Cu → via (24, 13) → D1.2;
 * - /VIN as two tracks with a gap between (9.175, 14) and (9.2, 15);
 * - GND: D1.1 and C1.2 each down a via to the B.Cu zone GND_B (not filled in the file);
 * - a rule area NO_COPPER (30..38, 18..28), the text ESPION and a silkscreen line.
 * KiCad re-saved (kicad-cli pcb upgrade).
 */
const COPPER_BOARD = path.join(BOARD_FIXTURE, "copper.kicad_pcb");

const ARC = (3 * Math.PI) / 2;
/** LED_A pad to pad along its tracks: 3.175 + arc + 3 + 4 (B.Cu) + 1.7875 + 3. */
const LED_A_TRACKS = 3.175 + ARC + 3 + 4 + 1.7875 + 3;
/** Between the middles of F.Cu and B.Cu: half of each 35 µm copper plus the 1.51 mm dielectric. */
const VIA_HEIGHT = 0.035 + 1.51;

const LAYER_IDS: Record<string, number> = { "F.Cu": 0, "In1.Cu": 4, "In2.Cu": 6, "B.Cu": 2 };

/** A board with these items and copper layers (front to back), no stack-up section. */
function board(items: string, copper = ["F.Cu", "B.Cu"]): Board {
	const file = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "kicad-copper-")), "b.kicad_pcb");
	const layers = copper.map(l => `(${LAYER_IDS[l]} "${l}" signal)`).join(" ");
	fs.writeFileSync(file, `(kicad_pcb (version 20260206) (general (thickness 1.6)) (layers ${layers} (25 "Edge.Cuts" user)) ${items})`);
	return Board.load(file);
}

const seg = (a: Pt, b: Pt, net: string, layer = "F.Cu", w = 0.2) => `(segment (start ${a[0]} ${a[1]}) (end ${b[0]} ${b[1]}) (width ${w}) (layer "${layer}") (net "${net}") (uuid "${net}-${a}-${b}"))`;

describe("copper model", () => {
	const c = Copper.of(Board.load(COPPER_BOARD));

	test("reads tracks, the arc, vias, the zone, the rule area, drawings and texts as KiCad 10 stores them", () => {
		expect(c.tracks.map(t => t.kind).sort()).toEqual(["arc", ...Array(9).fill("track")]);
		expect(c.vias.map(v => [v.net, v.at, v.size, v.drill, v.type, v.span])).toContainEqual(["LED_A", [20, 13], 0.6, 0.3, "through", ["F.Cu", "B.Cu"]]);
		const arc = c.tracks.find(t => t.kind === "arc")!;
		// KiCad keeps the mid point to the nm: the centre and radius come out within a µm
		expect(arc.center![0]).toBeCloseTo(14, 5);
		expect(arc.center![1]).toBeCloseTo(13, 5);
		expect(arc.radius).toBeCloseTo(3, 5);
		expect(arc.length).toBeCloseTo(ARC, 5);
		const [z] = c.zones;
		expect([z!.name, z!.net, z!.layers, z!.priority, z!.filled, z!.fills.length]).toEqual(["GND_B", "GND", ["B.Cu"], 1, false, 0]);
		expect(z!.settings).toMatchObject({ clearance: 0.3, min_thickness: 0.25, connect_pads: "thermal", thermal_gap: 0.5, thermal_spoke_width: 0.5, island_removal: "always", fill_mode: "solid" });
		expect(c.ruleAreas.map(a => [a.name, a.layers, a.keepout])).toEqual([
			["NO_COPPER", ["F.Cu", "B.Cu"], { tracks: true, vias: true, pads: false, copperpour: true, footprints: true }],
		]);
		expect(c.texts.map(t => [t.text, t.at, t.size, t.layer])).toEqual([["ESPION", [20, 26], [1.5, 1.5], "F.SilkS"]]);
		expect(c.drawings.map(d => [d.type, d.layer]).sort()).toEqual([["line", "F.SilkS"], ["rect", "Edge.Cuts"]]);
	});

	test("a gap splits /VIN into two clusters joined by one unrouted pad pair; closing it joins them", () => {
		const vin = c.connectivity("/VIN");
		expect(vin.clusters.map(k => k.pads.map(p => p.id))).toEqual([["C1.1"], ["R1.1"]]);
		expect(vin.unrouted.map(r => [r.from.id, r.to.id, r.from.at, r.to.at, r.distance])).toEqual([["C1.1", "R1.1", [9.225, 20], [9.175, 10], Math.hypot(0.05, 10)]]);
		// GND's vias end on an unfilled zone: two clusters until KiCad fills it
		expect(c.connectivity("GND").clusters).toHaveLength(2);
		expect(c.connectivity("LED_A").clusters).toHaveLength(1);
		const b = Board.load(COPPER_BOARD);
		const loose = children(b.tree, "segment").find(s => num(child(s, "start"), 1) === 9.2)!;
		expect(loose).toBeDefined();
		// move the loose end onto the middle of the other track: a T-junction joins too
		loose[loose.indexOf(child(loose, "start")!)] = L("start", 9.175, 12);
		const joined = Copper.of(b).connectivity("/VIN");
		expect(joined.clusters).toHaveLength(1);
		expect(joined.unrouted).toEqual([]);
	});

	test("lengths: tracks and the arc per layer; via heights only when asked; pad to pad paths along the copper", () => {
		const off = c.netLength("LED_A", { useHeight: false });
		expect(off.track).toBeCloseTo(LED_A_TRACKS, 5);
		expect(off.perLayer["B.Cu"]).toBeCloseTo(4, 9);
		expect([off.vias, off.viaHeight]).toEqual([2, 0]);
		const on = c.netLength("LED_A", { useHeight: true });
		expect(on.viaHeight).toBeCloseTo(2 * VIA_HEIGHT, 9);
		expect(on.total).toBeCloseTo(LED_A_TRACKS + 2 * VIA_HEIGHT, 5);
		const p = c.path("R1.2", "D1.2", { useHeight: true })!;
		expect(p.length).toBeCloseTo(LED_A_TRACKS + 2 * VIA_HEIGHT, 5);
		expect(p.vias).toBe(2);
		expect(c.path("D1.2", "R1.2", { useHeight: false })!.length).toBeCloseTo(LED_A_TRACKS, 5);
		expect(c.path("R1.1", "C1.1", { useHeight: true })).toBeUndefined();
		expect(() => c.path("R1.1", "D1.2", { useHeight: true })).toThrow("R1.1 is on /VIN but D1.2 is on LED_A");
		expect(() => c.path("R1.7", "D1.2", { useHeight: true })).toThrow("R1 has no pad 7 (pads: 1, 2)");
		expect(() => c.path("R9.1", "D1.2", { useHeight: true })).toThrow("no footprint R9 on the board");
	});

	test("the shortest path takes the short branch of a loop and enters a track at a T-junction", () => {
		// A (0,0) → (10,0); a branch from the middle (5,0) down to (5,4); a detour (0,0) → (0,8) → (10,8) → (10,0)
		const b = board(
			[seg([0, 0], [10, 0], "N"), seg([5, 0], [5, 4], "N"), seg([0, 0], [0, 8], "N"), seg([0, 8], [10, 8], "N"), seg([10, 8], [10, 0], "N")].join(" ") +
				` (footprint "X" (layer "F.Cu") (at 0 0) (property "Reference" "P1") (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu") (net "N")) (pad "2" smd rect (at 10 0) (size 1 1) (layers "F.Cu") (net "N")) (pad "3" smd rect (at 5 4) (size 1 1) (layers "F.Cu") (net "N")))`,
		);
		const k = Copper.of(b);
		expect(k.path("P1.1", "P1.2", { useHeight: false })!.length).toBeCloseTo(10, 9);
		expect(k.path("P1.1", "P1.3", { useHeight: false })!.length).toBeCloseTo(9, 9);
		expect(k.path("P1.2", "P1.3", { useHeight: false })!.length).toBeCloseTo(9, 9);
	});

	test("diff pairs by KiCad's naming, and the P/N skew", () => {
		expect(dpPartnerName("USB_D+")).toEqual({ partner: "USB_D-", positive: true });
		expect(dpPartnerName("/CSI/LVDS_N0")).toEqual({ partner: "/CSI/LVDS_P0", positive: false });
		expect(dpPartnerName("ETH_TXP_1")).toEqual({ partner: "ETH_TXN_1", positive: true });
		expect(dpPartnerName("GND")).toBeUndefined();
		const k = Copper.of(board([seg([0, 0], [10, 0], "USB_P"), seg([0, 1], [9.5, 1], "USB_N"), seg([0, 3], [5, 3], "VCAP")].join(" ")));
		expect(k.dpPartner("USB_P")).toEqual({ partner: "USB_N", positive: true });
		// VCAP ends in P, but there is no VCAN
		expect(k.dpPartner("VCAP")).toBeUndefined();
		expect(k.netLength("USB_P", { useHeight: true }).total - k.netLength("USB_N", { useHeight: true }).total).toBeCloseTo(0.5, 9);
	});

	test("pad shapes follow the footprint's rotation and the pad's own; roundrect corners and ovals are round", () => {
		// a 2 × 1 rect pad turned 90° with its footprint: 1 mm wide in x, 2 mm tall in y
		const k = Copper.of(
			board(
				`(footprint "X" (layer "F.Cu") (at 10 10 90) (property "Reference" "U1") (pad "1" smd rect (at 3 0 90) (size 2 1) (layers "F.Cu") (net "A")) (pad "2" smd roundrect (at 0 0 90) (size 2 2) (layers "F.Cu") (roundrect_rratio 0.25) (net "B")) (pad "3" thru_hole oval (at -3 0 90) (size 1 3) (drill 0.6) (layers "*.Cu") (net "C")))`,
			),
		);
		const [p1, p2, p3] = k.pads;
		// footprint at (10, 10) turned 90° ccw: local (3, 0) lands at (10, 7)
		expect(p1!.at).toEqual([10, 7]);
		expect(p1!.shapes.some(s => shapeHas(s, [10, 7.95]))).toBe(true);
		expect(p1!.shapes.some(s => shapeHas(s, [10.45, 7]))).toBe(true);
		expect(p1!.shapes.some(s => shapeHas(s, [10.6, 7]))).toBe(false);
		// roundrect 2 × 2, radius 0.5: the corner (10.95, 10.95) is cut off, (10.6, 10.6) is in
		expect(p2!.shapes.some(s => shapeHas(s, [10.95, 10.95]))).toBe(false);
		expect(p2!.shapes.some(s => shapeHas(s, [10.6, 10.6]))).toBe(true);
		// oval 1 × 3 turned: 3 mm along x at (10, 13); a through-hole pad is on every copper layer
		expect(p3!.copper).toEqual(["F.Cu", "B.Cu"]);
		expect(p3!.shapes.some(s => shapeHas(s, [11.45, 13]))).toBe(true);
		expect(p3!.shapes.some(s => shapeHas(s, [10, 13.6]))).toBe(false);
	});

	test("shape distances: edge to edge with the closest points, 0 when touching", () => {
		const disc = shape([[0, 0]], 1);
		const track = shape([[3, -5], [3, 5]], 0.5);
		const d = shapeDistance(disc, track)!;
		expect(d.d).toBeCloseTo(1.5, 9);
		expect(d.pa).toEqual([1, 0]);
		expect(d.pb).toEqual([2.5, 0]);
		const square = shape([[-2, -2], [2, -2], [2, 2], [-2, 2]]);
		expect(shapeDistance(square, disc)!.d).toBe(0);
		expect(shapeDistance(square, shape([[5, 0]]))!.d).toBeCloseTo(3, 9);
	});

	test("stack-up heights: from the board's stack-up, or KiCad's default one", () => {
		const su = Board.load(COPPER_BOARD).stackup();
		expect(layerDistance(su, ["F.Cu", "B.Cu"], "F.Cu", "B.Cu")).toBeCloseTo(VIA_HEIGHT, 9);
		// no stack-up: 1.6 mm board, 35 µm copper, 10 µm masks
		const none = board("").stackup();
		expect(layerDistance(none, ["F.Cu", "B.Cu"], "B.Cu", "F.Cu")).toBeCloseTo(1.6 - 0.07 - 0.02 + 0.035, 9);
	});

	test("a 4-layer via counts the height between the outermost layers joined to it, not its whole span; a stub counts nothing", () => {
		const four = ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"];
		const via = (at: Pt, net: string) => `(via (at ${at[0]} ${at[1]}) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net "${net}") (uuid "v-${net}-${at}"))`;
		const k = Copper.of(
			board(
				[
					// F.Cu → via → In1.Cu: a through via used between the top two layers
					seg([0, 0], [5, 0], "S"),
					via([5, 0], "S"),
					seg([5, 0], [10, 0], "S", "In1.Cu"),
					// a stub: one track on F.Cu ends in it
					seg([0, 5], [5, 5], "T"),
					via([5, 5], "T"),
				].join(" "),
				four,
			),
		);
		const su = k.board.stackup();
		const top2 = layerDistance(su, four, "F.Cu", "In1.Cu");
		expect(top2).toBeLessThan(layerDistance(su, four, "F.Cu", "B.Cu") / 2);
		const s = k.netLength("S", { useHeight: true });
		expect([s.track, s.vias]).toEqual([10, 1]);
		expect(s.viaHeight).toBeCloseTo(top2, 9);
		expect(k.netLength("T", { useHeight: true }).viaHeight).toBe(0);
	});

	test("lengths as KiCad's length rules: a track end inside a via counts from its centre, a bend inside the end pad is cut short, pad-to-die lengths add", () => {
		const k = Copper.of(
			board(
				[
					`(via (at 0 0) (size 1) (drill 0.4) (layers "F.Cu" "B.Cu") (net "V") (uuid "via-v"))`,
					// ends 0.3 off the via's centre, inside its copper: counts as 0.5 (centre to edge) + 4.5
					seg([0.3, 0], [5, 0], "V"),
					// from the pad's position (10, 10) it bends inside the 2 × 2 pad and leaves at x = 11
					seg([10, 10], [10.5, 10.5], "P"),
					seg([10.5, 10.5], [14, 10.5], "P"),
					`(footprint "X" (layer "F.Cu") (at 10 10) (property "Reference" "U1") (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu") (net "P") (die_length 1.25)) (pad "2" smd rect (at 4 0.5) (size 0.4 0.4) (layers "F.Cu") (net "P")))`,
				].join(" "),
			),
		);
		expect(k.netLength("V", { useHeight: false }).track).toBeCloseTo(5, 9);
		const p = k.netLength("P", { useHeight: false });
		// the exit point is found to within a µm
		expect(p.track).toBeCloseTo(Math.hypot(1, 0.5) + 3, 5);
		expect(p.padToDie).toBe(1.25);
		expect(p.total).toBeCloseTo(Math.hypot(1, 0.5) + 3 + 1.25, 5);
		const path = k.path("U1.1", "U1.2", { useHeight: false })!;
		expect(path.padToDie).toBe(1.25);
		expect(path.length).toBeCloseTo(Math.hypot(1, 0.5) + 3 + 1.25, 5);
	});

	test("copper joins where it overlaps, as KiCad's connectivity: a track short of the pad centre, crossing tracks, a track over a pad, a copper drawing with the net; jumper pads", () => {
		const fp = (ref: string, at: Pt, pads: string, extra = "") => `(footprint "X" (layer "F.Cu") (at ${at[0]} ${at[1]}) (property "Reference" "${ref}") ${extra} ${pads})`;
		const pad = (n: string, at: Pt, net: string, size = 1) => `(pad "${n}" smd rect (at ${at[0]} ${at[1]}) (size ${size} ${size}) (layers "F.Cu") (net "${net}"))`;
		const k = Copper.of(
			board(
				[
					// A: ends 0.4 short of the pad edge, but 1 mm wide: overlaps the pad
					fp("A1", [0, 0], pad("1", [0, 0], "A") + pad("2", [10, 0], "A")),
					seg([0.9, 0], [9.5, 0], "A", "F.Cu", 1),
					// B: two tracks crossing, each from its own pad
					fp("B1", [0, 10], pad("1", [0, 0], "B") + pad("2", [5, -5], "B")),
					seg([0, 10], [10, 10], "B"),
					seg([5, 5], [5, 15], "B"),
					// C: a track passing over pad 2 on its way to pad 3
					fp("C1", [0, 20], pad("1", [0, 0], "C") + pad("2", [5, 0], "C") + pad("3", [10, 0], "C")),
					seg([0, 20], [10, 20], "C"),
					// D: a copper line with the net between two pads
					fp("D1", [0, 30], pad("1", [0, 0], "D") + pad("2", [10, 0], "D")),
					`(gr_line (start 0 30) (end 10 30) (stroke (width 0.3) (type solid)) (layer "F.Cu") (net "D") (uuid "gl-d"))`,
					// E: a 0.1 mm track ending 0.2 short of the pad: no join
					fp("E1", [0, 40], pad("1", [0, 0], "E") + pad("2", [10, 0], "E")),
					seg([0.7, 40], [10, 40], "E", "F.Cu", 0.1),
					// J: two pads numbered 1, joined as jumpers by the footprint
					fp("J1", [0, 50], pad("1", [0, 0], "J") + pad("1", [10, 0], "J"), "(duplicate_pad_numbers_are_jumpers yes)"),
				].join(" "),
			),
		);
		const clusters = (n: string) => k.connectivity(n).clusters.filter(c => c.pads.length).length;
		expect(["A", "B", "C", "D", "E", "J"].map(clusters)).toEqual([1, 1, 1, 1, 2, 1]);
		expect(k.connectivity("D").clusters[0]!.drawings.map(d => d.type)).toEqual(["line"]);
	});

	test("zones and keepouts inside footprints are read, in board coordinates, with their footprint", () => {
		const k = Copper.of(
			board(
				`(footprint "X" (layer "F.Cu") (at 20 20) (property "Reference" "U9") (zone (net "") (layers "F.Cu") (uuid "ant") (name "ANT") (keepout (tracks not_allowed) (vias not_allowed) (pads allowed) (copperpour not_allowed) (footprints not_allowed)) (polygon (pts (xy 20 20) (xy 25 20) (xy 25 25) (xy 20 25)))))`,
			),
		);
		expect(k.ruleAreas.map(a => [a.name, a.footprint, a.outline[2], a.keepout.footprints])).toEqual([["ANT", "U9", [25, 25], true]]);
	});

	test("a 40 000-point fill joins 1500 pads fast: inside, touching and clear of it", () => {
		// a disc of radius 50 around (50, 50) as one fill region; 1 × 1 mm pads every 2.5 mm over its box
		const n = 40_000;
		const ring = Array.from({ length: n }, (_, i) => `(xy ${(50 + 50 * Math.cos((2 * Math.PI * i) / n)).toFixed(6)} ${(50 + 50 * Math.sin((2 * Math.PI * i) / n)).toFixed(6)})`).join(" ");
		const centres: Pt[] = [];
		for (let i = 0; i < 39 && centres.length < 1500; i++) for (let j = 0; j < 39 && centres.length < 1500; j++) centres.push([1.3 + i * 2.5, 1.7 + j * 2.5]);
		const pads = centres.map((p, i) => `(pad "${i + 1}" smd rect (at ${p[0]} ${p[1]}) (size 1 1) (layers "F.Cu") (net "GND"))`).join(" ");
		const k = Copper.of(
			board(
				`(footprint "X" (layer "F.Cu") (at 0 0) (property "Reference" "U1") ${pads}) (zone (net "GND") (layer "F.Cu") (uuid "z") (fill yes) (polygon (pts (xy 0 0) (xy 100 0) (xy 100 100) (xy 0 100))) (filled_polygon (layer "F.Cu") (pts ${ring})))`,
			),
		);
		// how far each pad's square is from the disc's centre
		const gap = centres.map(([x, y]) => Math.hypot(Math.max(Math.abs(x - 50) - 0.5, 0), Math.max(Math.abs(y - 50) - 0.5, 0)) - 50);
		const clear = gap.filter(g => g > 0.01).length;
		const unsure = gap.filter(g => Math.abs(g) <= 0.01).length;
		const t = performance.now();
		const conn = k.connectivity("GND");
		const ms = performance.now() - t;
		expect(ms).toBeLessThan(1500);
		const alone = conn.clusters.filter(c => c.pads.length === 1 && !c.fills.length).length;
		expect(alone).toBeGreaterThanOrEqual(clear);
		expect(alone).toBeLessThanOrEqual(clear + unsure);
		expect(conn.clusters.find(c => c.fills.length)!.pads.length).toBe(1500 - alone);
	});
});

describe.skipIf(!HAS_KICAD)("copper queries and render on the copper fixture", () => {
	let root = "";
	let dir = "";
	let unconnectedByKiCad = -1;
	const q = (what: string, args: Record<string, unknown> = {}) => runQuery(root, { project: BOARD_PROJECT, what, args }) as Promise<Record<string, any>>;

	beforeAll(async () => {
		root = stageBoardFixture();
		dir = path.join(root, BOARD_PROJECT);
		fs.copyFileSync(COPPER_BOARD, path.join(dir, "board.kicad_pcb"));
	});
	afterAll(() => cleanup(root));

	test("the drc check refills zones in memory: GND joins through the zone, the file keeps no fills", async () => {
		const r = await runChecks(root, { project: BOARD_PROJECT, checks: ["drc"] });
		expect(r.report).not.toContain("FAILED TO RUN");
		// without the refill GND would be unconnected too
		expect(r.findings.find(f => f.key === "drc|unconnected")?.message).toStartWith("1 unconnected items");
		expect(fs.readFileSync(path.join(dir, "board.kicad_pcb"), "utf8")).not.toContain("filled_polygon");
		const before = await q("board");
		expect(before.copper).toMatchObject({ zones: 1, zones_filled: "none", unrouted: 2 });
		const report = await pcbRefillAndSave(path.join(dir, "board.kicad_pcb"), { cwd: dir, repoRoot: REPO });
		unconnectedByKiCad = report.unconnected.length;
	}, 120_000);

	test("after the refill: the zone is filled, KiCad's unconnected count matches the model's ratsnest", async () => {
		expect(unconnectedByKiCad).toBe(1);
		const z = await q("zones");
		const [gnd] = z.zones;
		expect(gnd).toMatchObject({ name: "GND_B", net: "GND", filled: true });
		expect(gnd.filled_area["B.Cu"]).toBeGreaterThan(500);
		expect(gnd.filled_area["B.Cu"]).toBeLessThan(39 * 29);
		expect(z.rule_areas).toEqual([{ id: expect.any(String), name: "NO_COPPER", layers: ["F.Cu", "B.Cu"], keepout: ["tracks", "vias", "copperpour", "footprints"], outline: [[30, 18], [38, 18], [38, 28], [30, 28]] }]);
		const b = await q("board");
		expect(b.copper).toEqual({ tracks: 9, arcs: 1, vias: 4, zones: 1, rule_areas: 1, drawings: 2, texts: 1, tuning_patterns: 0, zones_filled: "all", unrouted: unconnectedByKiCad });
		const gndRouting = await q("routing", { net: "GND" });
		expect(gndRouting.nets[0].clusters).toEqual([{ pads: ["D1.1", "C1.2"], tracks: 2, vias: 2, zones: ["GND_B"] }]);
		const vin = await q("routing", { net: "/VIN" });
		expect(vin.nets[0].unrouted).toEqual([{ from: "C1.1", to: "R1.1", from_at: [9.225, 20], to_at: [9.175, 10], distance: 10.0001 }]);
		const all = await q("routing");
		expect(all.unrouted).toBe(1);
		expect(all.nets).toContainEqual(["LED_A", 2, 22.7649, 2, 1, 0]);
	}, 120_000);

	test("lengths and paths; via heights only with use_height_for_length_calcs", async () => {
		const l = await q("lengths", { nets: ["LED_A"], paths: [{ from: "R1.2", to: "D1.2" }, { from: "R1.1", to: "C1.1" }, { from: "D1.1", to: "C1.2" }] });
		expect(l.via_heights_counted).toBe(true);
		expect(l.nets[0].length).toBeCloseTo(LED_A_TRACKS + 2 * VIA_HEIGHT, 3);
		expect(l.paths[0].length).toBeCloseTo(LED_A_TRACKS + 2 * VIA_HEIGHT, 3);
		expect(l.paths[1]).toEqual({ from: "R1.1", to: "C1.1", unrouted: true });
		expect(l.paths[2].through_zones).toEqual(["GND_B"]);
		const pro = path.join(dir, "board.kicad_pro");
		const json = JSON.parse(fs.readFileSync(pro, "utf8"));
		json.board.design_settings.rules.use_height_for_length_calcs = false;
		fs.writeFileSync(pro, JSON.stringify(json, null, 2));
		try {
			const flat = await q("lengths", { paths: [{ from: "R1.2", to: "D1.2" }] });
			expect(flat.via_heights_counted).toBe(false);
			expect(flat.paths[0].length).toBeCloseTo(LED_A_TRACKS, 3);
			expect(flat.paths[0].via_height).toBeUndefined();
		} finally {
			json.board.design_settings.rules.use_height_for_length_calcs = true;
			fs.writeFileSync(pro, JSON.stringify(json, null, 2));
		}
		await expect(q("lengths", { paths: [{ from: "R1.2" }] })).rejects.toThrow('paths[0] must be {from: "REF.PIN", to: "REF.PIN"}');
	});

	test("copper {box, kind, net, layer} keeps what touches the region", async () => {
		const r = await q("copper", { box: [19, 12, 21, 14] });
		expect(r.tracks.map((t: { start: Pt; end: Pt; layer: string }) => [t.layer, t.start, t.end])).toEqual([
			["F.Cu", [17, 13], [20, 13]],
			["B.Cu", [20, 13], [24, 13]],
		]);
		expect(r.vias.map((v: { at: Pt }) => v.at)).toEqual([[20, 13]]);
		expect(r.zones.map((z: { name: string }) => z.name)).toEqual(["GND_B"]);
		const arcs = await q("copper", { kind: "arc" });
		expect(arcs.tracks).toEqual([{ id: "c0ffee00", kind: "arc", net: "LED_A", layer: "F.Cu", width: 0.25, start: [14, 10], end: [17, 13], mid: [16.1213, 10.8787], length: 4.7124 }]);
		expect((await q("copper", { layer: "B.Cu", kind: "track" })).tracks.map((t: { net: string }) => t.net)).toEqual(["LED_A"]);
		expect((await q("copper", { net: "GND", kind: "via" })).vias).toHaveLength(2);
		await expect(q("copper", { net: "VIN" })).rejects.toThrow('no net "VIN" on the board; nets like it: /VIN');
		await expect(q("copper", { layer: "In1.Cu" })).rejects.toThrow('layer "In1.Cu" is not a copper layer of the board (F.Cu, B.Cu)');
	});

	test("distance: copper edge to edge and centre to centre", async () => {
		const d = await q("distance", { a: "R1.2", b: "D1.2" });
		// pads on one line: centres 14.9625 apart, minus half of R1's 0.8 and D1's 0.875 pad widths
		expect(d.centre).toBe(14.9625);
		expect(d.edge).toBeCloseTo(14.125, 4);
		// facing edges: the closest points are on them, level with each other
		const [pa, pb] = d.closest.points as [Pt, Pt];
		expect([pa[0], pb[0], pa[1] === pb[1], Math.abs(pa[1] - 10) <= 0.475]).toEqual([11.225, 25.35, true, true]);
		const fp = await q("distance", { a: "R1", b: "C1" });
		expect(fp.centre).toBe(10);
		expect(fp.courtyard_gap).toBeGreaterThan(0);
		expect(fp.courtyard_gap).toBeLessThan(fp.edge);
		const net = await q("distance", { a: [22, 16], b: { net: "LED_A" }, layer: "B.Cu" });
		// the B.Cu track at y = 13, 0.25 wide; the vias at its ends are further
		expect(net.edge).toBeCloseTo(3 - 0.125, 4);
		expect(net.closest.b).toMatch(/^track /);
		// below a via the via's copper (r 0.3) is nearer than the track's
		const via = await q("distance", { a: [20, 16], b: { net: "LED_A" }, layer: "B.Cu" });
		expect([via.edge, via.closest.b.split(" ")[0]]).toEqual([2.7, "via"]);
		expect((await q("distance", { a: "R1.2", b: { net: "LED_A" } })).overlap).toBe(true);
	});

	test("board_free_space avoids courtyards, the outline (edge clearance), keepouts for footprints, and other results", async () => {
		const fps = (await q("footprints")).footprints as { ref: string; courtyard: [number, number, number, number] }[];
		const clearOf = (b: number[]) => fps.every(f => b[2]! <= f.courtyard[0] || f.courtyard[2] <= b[0]! || b[3]! <= f.courtyard[1] || f.courtyard[3] <= b[1]!);
		const r = await q("board_free_space", { near: [10, 10], size: [3, 2], limit: 4 });
		expect(r.edge_clearance).toBe(0.5);
		expect(r.boxes).toHaveLength(4);
		for (const { box } of r.boxes) expect(clearOf(box)).toBe(true);
		const corner = await q("board_free_space", { near: [-5, -5], size: [3, 2], limit: 1 });
		expect(corner.boxes[0].box).toEqual([0.5, 0.5, 3.5, 2.5]);
		const keep = await q("board_free_space", { near: [34, 23], size: [2, 2], limit: 1 });
		const [x0, y0, x1, y1] = keep.boxes[0].box as number[];
		expect(x1! <= 30 || x0! >= 38 || y1! <= 18 || y0! >= 28).toBe(true);
		await expect(q("board_free_space", { near: [10, 10], size: [50, 50] })).rejects.toThrow("no free 50 × 50 mm area on the board");
	});

	test("board_graphics and stats", async () => {
		const g = await q("board_graphics", { layer: "F.SilkS" });
		expect(g.drawings).toEqual([{ id: "c0ffee00", type: "line", start: [2, 28], end: [12, 28], width: 0.15, fill: false, layer: "F.SilkS" }]);
		expect(g.texts).toEqual([
			{ id: "c0ffee00", type: "text", text: "ESPION", at: [20, 26], size: [1.5, 1.5], thickness: 0.2, rot: 0, justify: [], layer: "F.SilkS", knockout: false, mirrored: false },
		]);
		expect((await q("board_graphics", { kind: "rect" })).drawings.map((d: { layer: string }) => d.layer)).toEqual(["Edge.Cuts"]);
		const s = await q("stats");
		expect(s.vias).toMatchObject({ through: 4 });
		expect(s.pads).toMatchObject({ smd: 6 });
		expect(s.board.width).toBe(40);
		expect(s.board.min_track_width).toBe(0.25);
	}, 60_000);

	test("render: nets highlighted over KiCad's plot, the ratsnest, the copper layer preset; unknown nets refused", async () => {
		const svgs: string[] = [];
		const stub: Rasterize = async svg => {
			svgs.push(new TextDecoder().decode(svg));
			return new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
		};
		const img = await runRender(root, { project: BOARD_PROJECT, board: true, layers: ["copper"], nets: ["LED_A"], ratsnest: true }, stub);
		expect(img.png.length).toBeGreaterThan(0);
		expect(img.text).toBe(
			"Board board/board.kicad_pcb, layers Edge.Cuts, B.Cu, F.Cu, region [-3.00, -3.00, 43.00, 33.00] mm (board coordinates, seen from the top); nets LED_A highlighted in yellow (10 pads, tracks, vias and zone fills); 0 unrouted connections drawn as thin white lines (of these nets)",
		);
		const svg = svgs.at(-1)!;
		const overlay = svg.slice(svg.lastIndexOf('<g opacity="0.75">'));
		// the via at (20, 13), the B.Cu track, no ratsnest line for LED_A (routed)
		expect(overlay).toContain('<circle cx="20.0000" cy="13.0000" r="0.3000" fill="#FFE14D"/>');
		expect(overlay).toContain('<line x1="20.0000" y1="13.0000" x2="24.0000" y2="13.0000" stroke="#FFE14D" stroke-width="0.2500"');
		expect(overlay).not.toContain("#FFFFFF");
		await runRender(root, { project: BOARD_PROJECT, board: true, ratsnest: true }, stub);
		expect(svgs.at(-1)).toContain('<line x1="9.2250" y1="20.0000" x2="9.1750" y2="10.0000" stroke="#FFFFFF"');
		await expect(runRender(root, { project: BOARD_PROJECT, board: true, nets: ["LED"] }, stub)).rejects.toThrow('no net "LED" on the board; nets like it: LED_A');
		await expect(runRender(root, { project: BOARD_PROJECT, sheet: "root", nets: ["LED_A"] }, stub)).rejects.toThrow("nets and ratsnest apply to the board only");
	}, 120_000);
});

describe.skipIf(!HAS_KICAD)("connectivity against KiCad's DRC: pad shapes, overlaps, crossings, copper drawings, thermal spokes", () => {
	let root = "";
	let model: Copper;
	let kicadUnrouted: string[] = [];
	let n = 0;
	const u = () => `"0dd5a9e0-0000-4000-8000-${String(++n).padStart(12, "0")}"`;
	const fp = (ref: string, at: number[], side: "F" | "B", pads: string) =>
		`(footprint "T:${ref}" (layer "${side}.Cu") (uuid ${u()}) (at ${at.join(" ")}) (property "Reference" "${ref}" (at 0 0 0) (layer "${side}.SilkS") (hide yes) (uuid ${u()}) (effects (font (size 1 1) (thickness 0.15)))) (attr smd) ${pads})`;
	const smd = (num: string, at: Pt, size: number, net: string, side = "F") => `(pad "${num}" smd rect (at ${at[0]} ${at[1]}) (size ${size} ${size}) (layers "${side}.Cu" "${side}.Mask") (net "${net}") (uuid ${u()}))`;
	const track = (a: Pt, b: Pt, net: string, w: number, layer = "F.Cu") => `(segment (start ${a[0]} ${a[1]}) (end ${b[0]} ${b[1]}) (width ${w}) (layer "${layer}") (net "${net}") (uuid ${u()}))`;
	/** Pads under test: footprint placement, side, the pad (`NET` stands for its net). */
	const SHAPED: { name: string; at: number[]; side: "F" | "B"; pad: string }[] = [
		// a back-side footprint turned 30°: the pad stored flipped, its angle absolute
		{ name: "BK", at: [0, 0, 30], side: "B", pad: `(pad "1" smd rect (at 1 0.5 30) (size 2 1) (layers "B.Cu" "B.Mask") (net "NET") (uuid UID))` },
		// a through-hole pad whose copper sits 1 mm off its hole, turned with the footprint
		{ name: "OF", at: [0, 0, 90], side: "F", pad: `(pad "1" thru_hole rect (at 0 0 90) (size 1.5 1.5) (drill 0.6 (offset 1 0)) (layers "*.Cu" "*.Mask") (net "NET") (uuid UID))` },
		// a trapezoid 1.4 mm tall growing to 2 mm at its left end
		{ name: "TZ", at: [0, 0, 0], side: "F", pad: `(pad "1" smd trapezoid (at 0 0) (size 2 1.4) (rect_delta 0.6 0) (layers "F.Cu" "F.Mask") (net "NET") (uuid UID))` },
		// a custom pad: a round anchor with a 2.5 mm triangular finger, turned 45°
		{
			name: "CU",
			at: [0, 0, 45],
			side: "F",
			pad: `(pad "1" smd custom (at 0 0 45) (size 1 1) (layers "F.Cu" "F.Mask") (net "NET") (options (clearance outline) (anchor circle)) (primitives (gr_poly (pts (xy 0 -0.4) (xy 2.5 0) (xy 0 0.4)) (width 0) (fill yes))) (uuid UID))`,
		},
		// a rounded rectangle turned 20°: its corners are round
		{ name: "RR", at: [0, 0, 20], side: "F", pad: `(pad "1" smd roundrect (at 0 0 20) (size 2 1) (layers "F.Cu" "F.Mask") (roundrect_rratio 0.25) (net "NET") (uuid UID))` },
	];
	const slot = (i: number): Pt => [8 + (i % 6) * 15, 8 + Math.floor(i / 6) * 14];
	/** Nets that must stay unrouted: every _OUT probe, the thin track short of its pad, the cut corner of the chamfered pad. */
	const EXPECTED = ["BK_OUT", "CH_OUT", "CU_OUT", "OF_OUT", "OV_THIN", "RR_OUT", "TZ_OUT"];

	beforeAll(async () => {
		root = stageBoardFixture();
		const dir = path.join(root, BOARD_PROJECT);
		const file = path.join(dir, "board.kicad_pcb");
		const b = Board.load(file);
		const items: string[] = [`(gr_rect (start 0 0) (end 100 60) (stroke (width 0.05) (type solid)) (fill no) (layer "Edge.Cuts") (uuid ${u()}))`];
		let s = 0;
		const placeShaped: { net: string; ref: string; side: "F" | "B"; out: boolean }[] = [];
		for (const c of SHAPED) {
			for (const out of [false, true]) {
				const net = `${c.name}_${out ? "OUT" : "IN"}`;
				const [x, y] = slot(s++);
				items.push(fp(`${c.name}${out ? "O" : "I"}`, [x, y, c.at[2]!], c.side, c.pad.replace("NET", net).replace("UID", u())));
				placeShaped.push({ net, ref: `${c.name}${out ? "O" : "I"}`, side: c.side, out });
			}
		}
		// a 2 × 2 pad with its top-left corner cut by 0.5 mm
		const chamfer = (net: string) =>
			`(pad "1" smd roundrect (at 0 0) (size 2 2) (layers "F.Cu" "F.Mask") (roundrect_rratio 0) (chamfer_ratio 0.25) (chamfer top_left) (net "${net}") (uuid ${u()}))`;
		const [chI, chO] = [slot(s++), slot(s++)];
		items.push(fp("CHI", [...chI, 0], "F", chamfer("CH_IN")), fp("CHO", [...chO, 0], "F", chamfer("CH_OUT")));
		const ov = slot(s++);
		const thin = slot(s++);
		const cr = slot(s++);
		const op = slot(s++);
		const dr = slot(s++);
		const two = (ref: string, o: Pt, net: string, second: Pt = [10, 0]) => fp(ref, [...o, 0], "F", smd("1", [0, 0], 1, net) + smd("2", second, 1, net));
		items.push(
			// 1 mm wide, ending 0.4 short of pad 1's edge: its copper overlaps the pad
			two("OV", ov, "OV_WIDE"),
			track([ov[0] + 0.9, ov[1]], [ov[0] + 10, ov[1]], "OV_WIDE", 1),
			// 0.1 mm wide, ending 0.2 short: no overlap
			two("OT", thin, "OV_THIN"),
			track([thin[0] + 0.7, thin[1]], [thin[0] + 10, thin[1]], "OV_THIN", 0.1),
			// two tracks crossing, neither ending on the other
			two("CR", cr, "CROSS", [5, -5]),
			track([cr[0], cr[1]], [cr[0] + 10, cr[1]], "CROSS", 0.2),
			track([cr[0] + 5, cr[1] - 5], [cr[0] + 5, cr[1] + 5], "CROSS", 0.2),
			// a track over pad 2 on its way to pad 3
			fp("OP", [...op, 0], "F", smd("1", [0, 0], 1, "OVER") + smd("2", [5, 0], 1, "OVER") + smd("3", [10, 0], 1, "OVER")),
			track([op[0], op[1]], [op[0] + 3, op[1]], "OVER", 0.2),
			track([op[0] + 5.2, op[1]], [op[0] + 10, op[1]], "OVER", 0.2),
			track([op[0] + 3, op[1]], [op[0] + 4.8, op[1]], "OVER", 0.2),
			// a copper line carrying the net
			two("DR", dr, "DRAWN"),
			`(gr_line (start ${dr[0]} ${dr[1]}) (end ${dr[0] + 10} ${dr[1]}) (stroke (width 0.3) (type solid)) (layer "F.Cu") (net "DRAWN") (uuid ${u()}))`,
			// two pads in a zone, joined only through its thermal spokes once KiCad fills it
			fp("TH", [68, 51, 0], "F", smd("1", [0, 0], 1, "THERMAL") + smd("2", [17, 0], 1, "THERMAL")),
			`(zone (net "THERMAL") (layer "F.Cu") (uuid ${u()}) (name "TH") (hatch edge 0.5) (connect_pads (clearance 0.3)) (min_thickness 0.25) (fill (thermal_gap 0.5) (thermal_bridge_width 0.5)) (polygon (pts (xy 62 46) (xy 95 46) (xy 95 56) (xy 62 56))))`,
		);
		b.add(...parseMany(items.join("\n")));
		// probes: a thin track from just inside (or just outside) the pad's farthest copper, straight out to a small pad
		const pre = Copper.of(b);
		const probes: string[] = [];
		const probe = (ref: string, net: string, at: Pt, dir: Pt, side: "F" | "B") => {
			const far: Pt = [at[0] + dir[0] * 3, at[1] + dir[1] * 3];
			probes.push(track(at, far, net, 0.1, `${side}.Cu`), fp(`Q${ref}`, [...far, 0], side, smd("1", [0, 0], 0.6, net, side)));
		};
		for (const p of placeShaped) {
			const pad = pre.pad(`${p.ref}.1`)[0]!;
			// the copper point farthest from the pad's position, over every shape (a vertex grown by its radius)
			let best: { p: Pt; d: number } = { p: pad.at, d: -1 };
			for (const sh of pad.shapes) {
				for (const v of sh.pts) {
					const d = Math.hypot(v[0] - pad.at[0], v[1] - pad.at[1]);
					const e: Pt = d > 0 ? [v[0] + ((v[0] - pad.at[0]) / d) * sh.r, v[1] + ((v[1] - pad.at[1]) / d) * sh.r] : v;
					const de = Math.hypot(e[0] - pad.at[0], e[1] - pad.at[1]);
					if (de > best.d) best = { p: e, d: de };
				}
			}
			const dir: Pt = [(best.p[0] - pad.at[0]) / best.d, (best.p[1] - pad.at[1]) / best.d];
			const k = p.out ? 0.15 : -0.1;
			probe(p.ref, p.net, [best.p[0] + dir[0] * k, best.p[1] + dir[1] * k], dir, p.side);
		}
		// the chamfered corner: 0.1 mm in from the uncut corner is outside the cut; the opposite corner is copper
		const diag = Math.SQRT1_2;
		probe("CHO", "CH_OUT", [chO[0] - 1 + 0.1 * diag, chO[1] - 1 + 0.1 * diag], [-diag, -diag], "F");
		probe("CHI", "CH_IN", [chI[0] + 1 - 0.1 * diag, chI[1] + 1 - 0.1 * diag], [diag, diag], "F");
		b.add(...parseMany(probes.join("\n")));
		fs.writeFileSync(file, b.serialize());
		const o = { cwd: dir, repoRoot: REPO };
		await upgrade("pcb", file, o);
		const report = await pcbRefillAndSave(file, o);
		kicadUnrouted = [...new Set(report.unconnected.flatMap(v => v.items.flatMap(i => [...i.description.matchAll(/\[([^\]]+)\]/g)].map(m => m[1]!))))].sort();
		model = Copper.of(Board.load(file));
	}, 120_000);
	afterAll(() => cleanup(root));

	test("the nets left unrouted are KiCad's, and they are the ones meant to be", () => {
		expect(kicadUnrouted).toEqual(EXPECTED);
		expect(model.nets().filter(net => model.connectivity(net).unrouted.length > 0)).toEqual(EXPECTED);
	});

	test("the copper line and the thermal spokes join, as KiCad says", () => {
		expect(model.connectivity("DRAWN").clusters.map(c => [c.pads.length, c.drawings.length])).toEqual([[2, 1]]);
		const th = model.connectivity("THERMAL");
		expect(th.fills.length).toBeGreaterThan(0);
		// each pad sits in a 0.5 mm thermal gap: copper only crosses it on the spokes
		expect(th.pads.map(p => th.fills.some(f => shapeHas(f.shape, [p.at[0] + 0.75, p.at[1] + 0.75])))).toEqual([false, false]);
		expect(th.clusters.map(c => c.pads.length)).toEqual([2]);
	});
});

describe("copper queries on espion-cam (183 footprints, no tracks)", () => {
	const q = async (what: string, args: Record<string, unknown> = {}) => {
		const t = performance.now();
		const out = (await runQuery(REPO, { project: "hardware/espion-cam", what, args })) as Record<string, any>;
		return { out, ms: performance.now() - t };
	};

	test("routing, copper, lengths and board answer fast", async () => {
		const routing = await q("routing");
		expect(routing.ms).toBeLessThan(2000);
		expect(routing.out.unrouted).toBeGreaterThan(100);
		const gnd = await q("routing", { net: "GND" });
		expect(gnd.ms).toBeLessThan(2000);
		expect(gnd.out.nets[0].unrouted.length).toBe(gnd.out.nets[0].clusters.length - 1);
		const copper = await q("copper");
		expect(copper.ms).toBeLessThan(2000);
		expect(copper.out).toEqual({ total: 0, tracks: [], vias: [], zones: [] });
		const board = await q("board");
		expect(board.out.copper.unrouted).toBe(routing.out.unrouted);
		const free = await q("board_free_space", { near: [45, 43], size: [4, 4], side: "bottom" });
		expect(free.ms).toBeLessThan(2000);
	});
});

