/**
 * The board review checks (track_widths, return_path, via_return, decoupling_placement, placement,
 * layout) on a routed 4-layer fixture, settings.layout validation, and a run on espion-cam.
 *
 * The fixture (test/fixtures/layout; net classes from the test's .kicad_pro: DP for D_*, gap 0.2;
 * Power for VDD, track width 0.4), 60 × 40 mm, F.Cu / In1.Cu / In2.Cu / B.Cu:
 * - U1 (20, 20), locked: VDD pins 1 (17, 18) and 7 (23, 17), VIO pin 3 (17, 22), D_P/D_N pins 4/5, CLK 6;
 * - C1 (14.4, 18) VDD/GND 2.7 mm from U1.1 with a GND via by its ground pad; C2 (26, 14) 2.704 mm
 *   from U1.7, no ground via; C3 (10, 30) VIO, 10 mm from U1.3; C4, C5, R1 around (8, 8);
 * - VDD routed C1.1 → U1.1 → U1.7 → C2.1: its first piece 0.3 mm wide, the last 0.3995;
 * - D_P/D_N coupled at gap 0.2 (x 25..35) and 0.3 (x 36..45) on F.Cu, then down vias at (46, 19) and
 *   (46, 21) to B.Cu and J1 (through-hole); a GND via at (46, 18.1) 0.9 mm from D_P's;
 * - CLK (class Default) U1.6 → J1.4 straight along y 23;
 * - zone GND_IN1 on In1.Cu over the whole board (saved unfilled), rule area SLOT keeping its pour out
 *   of x 39.5..41.5, y 10..30; rule area NO_PARTS (52..58, 28..38) keeping footprints out;
 * - board-only parts: X1 across the right edge, X2 off the board, X3 in NO_PARTS, X4 on the bottom.
 * The schematic gives the netlist (pin types, sheets, classes); KiCad re-saves both files in setup.
 */
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { Board, courtyardEdges } from "../src/board";
import { courtyardDistance } from "../src/checks/copper";
import { classRules } from "../src/checks/rules";
import { upgrade } from "../src/cli";
import { DEFAULTS, loadConfig, projectConfig, validateLayout } from "../src/config";
import { Copper } from "../src/copper";
import { runChecks } from "../src/tools";
import type { Finding } from "../src/types";
import { child, children, num, text } from "../src/sexpr";
import { BOARD_FIXTURE, BOARD_PROJECT, cleanup, HAS_KICAD, REPO, stageBoardFixture } from "./helpers";

const FIXTURE = path.join(import.meta.dir, "fixtures", "layout");
const NEW_CHECKS = ["track_widths", "return_path", "via_return", "decoupling_placement", "placement", "layout"];

const keys = (fs: Finding[], check: string, severity?: string) => fs.filter(f => f.check === check && (!severity || f.severity === severity)).map(f => f.key);

describe("settings.layout validation", () => {
	const near = { a: "C1", b: "U1", max: 3, why: "DS p.4" };

	test("bad entries are named with their list, index and name", () => {
		expect(() => validateLayout({ near: [near, { ...near, max: "3" }] }, "layout")).toThrow('layout.near[1] (C1 – U1): max: give a number >= 0 (mm), got "3"');
		expect(() => validateLayout({ near: [{ a: "C1", b: "U1", max: 3 }] }, "layout")).toThrow("layout.near[0] (C1 – U1): why is required");
		expect(() => validateLayout({ lengthGroups: [{ name: "CSI", nets: ["CSI_*"], maxSpred: 1, maxSpread: 1, why: "x" }] }, "layout")).toThrow(
			'layout.lengthGroups[0] (CSI): unknown field "maxSpred" (fields: name, nets, maxSpread, maxLength, why)',
		);
		expect(() => validateLayout({ paths: [{ name: "star", from: "U1", to: ["C1.1"], maxDiff: 1, why: "x" }] }, "layout")).toThrow(
			'layout.paths[0] (star): from: give a pad as "REF.PAD", e.g. "U6.75", got "U1"',
		);
		expect(() => validateLayout({ paths: [{ name: "star", from: "U1.1", to: ["C1.1"], why: "x" }] }, "layout")).toThrow("layout.paths[0] (star): give maxLength, maxDiff or both");
		expect(() => validateLayout({ copperArea: [{ name: "pad", net: "GND", layers: ["F.Cu"], minArea: 5, near: "U1", why: "x" }] }, "layout")).toThrow(
			"layout.copperArea[0] (pad): near and within go together",
		);
		expect(() => validateLayout({ copperArea: [{ name: "pad", net: "GND", layers: ["F.Cu"], minVias: 1.5, why: "x" }] }, "layout")).toThrow("minVias: give a whole number >= 0, got 1.5");
		expect(() => validateLayout({ heightZones: [{ name: "lens", center: [1], r0: 1, slope: 1, heights: {}, why: "x" }] }, "layout")).toThrow("layout.heightZones[0] (lens): center: give [x, y] in board mm, got [1]");
		expect(() => validateLayout({ heightZones: [{ name: "lens", center: [1, 2], r0: 1, slope: 1, heights: {}, why: "x" }] }, "layout")).toThrow("give heights or maxHeight");
		expect(() => validateLayout({ viaReturnMaxDistance: 0 }, "layout")).toThrow("layout.viaReturnMaxDistance: give a number > 0 (mm), got 0");
		expect(() => validateLayout({ nearby: [] }, "layout")).toThrow('layout: unknown setting "nearby"');
		expect(() => validateLayout({ near: [near], decouplingMaxDistance: 2, heightZones: [{ name: "lens", center: [1, 2], r0: 1, slope: 0.5, heights: { "Lib:*": 2 }, side: "bottom", why: "x" }] }, "layout")).not.toThrow();
	});

	test("defaults, merged per setting; a bad project entry fails loading with the file and project named", () => {
		const root = fs.mkdtempSync(path.join(os.tmpdir(), "kicad-layout-config-"));
		try {
			fs.mkdirSync(path.join(root, ".omp"));
			const file = path.join(root, ".omp", "kicad.json");
			fs.writeFileSync(file, JSON.stringify({ defaults: { layout: { decouplingMaxDistance: 2 } }, projects: { p: { layout: { near: [near] } } } }));
			const l = projectConfig(loadConfig(root), path.join(root, "p")).layout;
			expect(l).toEqual({ ...DEFAULTS.layout, decouplingMaxDistance: 2, near: [near] });
			fs.writeFileSync(file, JSON.stringify({ projects: { p: { layout: { near: [{ ...near, b: "" }] } } } }));
			expect(() => projectConfig(loadConfig(root), path.join(root, "p"))).toThrow(`${file}: projects["p"].layout.near[0] (C1 – ): b: give a non-empty string, got ""`);
		} finally {
			cleanup(root);
		}
	});
});

describe("net class settings", () => {
	test("a setting the class leaves out comes from Default, then KiCad's default", () => {
		const ns = { classes: [{ name: "Default", clearance: 0.15 }, { name: "HS", diff_pair_gap: 0.1 }], netclass_patterns: [] };
		expect(classRules(ns, "HS")).toEqual({
			name: "HS",
			clearance: 0.15,
			track_width: 0.2,
			via_diameter: 0.6,
			via_drill: 0.3,
			microvia_diameter: 0.3,
			microvia_drill: 0.1,
			diff_pair_width: 0.2,
			diff_pair_gap: 0.1,
		});
		// several classes (KiCad's netlist joins them with commas): the first the .kicad_pro has; none: Default
		expect([classRules(ns, "Other,HS").name, classRules(ns, "Other").name]).toEqual(["HS", "Default"]);
	});
});

describe("stale impedanceClasses on a board without copper", () => {
	test("return_path and via_return report the stale class before finding nothing to check", async () => {
		const root = stageBoardFixture();
		try {
			fs.mkdirSync(path.join(root, ".omp"));
			fs.writeFileSync(path.join(root, ".omp", "kicad.json"), JSON.stringify({ projects: { [BOARD_PROJECT]: { layout: { impedanceClasses: ["HS"] } } } }));
			const r = await runChecks(root, { project: BOARD_PROJECT, checks: ["return_path", "via_return"] });
			expect(r.findings.map(f => `${f.severity} ${f.key}`)).toEqual(["error return_path|stale|HS", "info return_path|none", "error via_return|stale|HS", "info via_return|none"]);
		} finally {
			cleanup(root);
		}
	});
});

describe.skipIf(!HAS_KICAD)("board review checks on the routed fixture", () => {
	let root = "";
	let dir = "";
	let all: Finding[] = [];
	const setLayout = (layout: Record<string, unknown>) => fs.writeFileSync(path.join(root, ".omp", "kicad.json"), JSON.stringify({ projects: { layout: { layout } } }));
	const run = async (checks: string[]) => {
		const r = await runChecks(root, { project: "layout", checks });
		expect(r.report).not.toContain("FAILED TO RUN");
		return r.findings;
	};

	beforeAll(async () => {
		root = fs.mkdtempSync(path.join(os.tmpdir(), "kicad-layout-test-"));
		dir = path.join(root, "layout");
		fs.cpSync(FIXTURE, dir, { recursive: true });
		const pro = JSON.parse(fs.readFileSync(path.join(BOARD_FIXTURE, "board.kicad_pro"), "utf8"));
		pro.meta.filename = "layout.kicad_pro";
		const def = pro.net_settings.classes[0];
		// KiCad writes only the keys a class sets: DP leaves its track and diff-pair widths to Default (0.2), Power everything but its track width
		pro.net_settings.classes = [def, { name: "DP", priority: 0, diff_pair_gap: 0.2 }, { name: "Power", priority: 1, track_width: 0.4 }];
		pro.net_settings.netclass_patterns = [
			{ netclass: "DP", pattern: "D_*" },
			{ netclass: "Power", pattern: "VDD" },
		];
		fs.writeFileSync(path.join(dir, "layout.kicad_pro"), JSON.stringify(pro, null, 2));
		const o = { cwd: dir, repoRoot: REPO };
		await upgrade("sch", path.join(dir, "layout.kicad_sch"), o);
		await upgrade("pcb", path.join(dir, "layout.kicad_pcb"), o);
		fs.mkdirSync(path.join(root, ".omp"));
		setLayout({});
		all = await run(NEW_CHECKS);
	}, 120_000);
	afterAll(() => cleanup(root));

	test("track_widths: the 0.3 mm VDD piece, not the 0.3995 one (within 1 µm); the pair's 0.3 mm gap stretch, not the 0.2 mm one", () => {
		expect(keys(all, "track_widths")).toEqual(["track_widths|width|VDD|F.Cu|0.3", "track_widths|gap|D_P|F.Cu"]);
		const gap = all.find(f => f.key === "track_widths|gap|D_P|F.Cu")!;
		expect(gap.message).toBe("diff pair D_P / D_N (class DP, gap 0.2 mm) on F.Cu: coupled at 0.3 mm over 9 mm (1 stretch, longest 9 mm at (40.5, 19.75))");
		expect(all.find(f => f.key.startsWith("track_widths|width"))!.at).toEqual([13.6, 17.25]);
	});

	test("return_path: the pair over the plane's slot, from the refilled zone (the file stays unfilled); B.Cu has no plane next to it", () => {
		const rp = all.filter(f => f.check === "return_path");
		expect(rp.map(f => [f.severity, f.message])).toEqual([
			["warning", "D_P (class DP) on F.Cu: 2 mm of track not over a ground zone fill on In1.Cu, from (39.5, 19.75) to (41.5, 19.75)"],
			["warning", "class DP has tracks on B.Cu but no ground zone on In2.Cu (the adjacent copper): no return plane under them"],
			["warning", "D_N (class DP) on F.Cu: 2 mm of track not over a ground zone fill on In1.Cu, from (39.5, 20.25) to (41.5, 20.25)"],
		]);
		// CLK (class Default) crosses the slot too: not an impedance class
		expect(rp.some(f => f.net === "CLK")).toBe(false);
		expect(fs.readFileSync(path.join(dir, "layout.kicad_pcb"), "utf8")).not.toContain("filled_polygon");
		expect(Copper.of(Board.load(path.join(dir, "layout.kicad_pcb"))).zones[0]!.fills).toEqual([]);
	});

	test("via_return: D_N's via has no GND via within 1 mm, D_P's has one at 0.9 mm; within 3 mm both pass", async () => {
		expect(all.filter(f => f.check === "via_return").map(f => f.message)).toEqual(["D_N (class DP): 1 via changing layers with no ground via or through-hole ground pad within 1 mm: (46, 21)"]);
		setLayout({ viaReturnMaxDistance: 3 });
		expect(await run(["via_return"])).toEqual([]);
		setLayout({ impedanceClasses: ["Power"] });
		expect(keys(await run(["via_return"]), "via_return")).toEqual(["via_return|none"]);
	}, 60_000);

	test("decoupling_placement: the nearest capacitor per pin decides; ground pads without a via; the threshold", async () => {
		expect(keys(all, "decoupling_placement")).toEqual(["decoupling_placement|U1.3|VIO", "decoupling_placement|ground|C2", "decoupling_placement|ground|C3"]);
		expect(all.find(f => f.key === "decoupling_placement|U1.3|VIO")!.message).toBe(
			"U1.3 (VIO_3) on VIO: the nearest decoupling capacitor, C3, is 10.148 mm away (pad edge to pad edge; max 3 mm; capacitors on this net and sheet: C3)",
		);
		// U1.1 is 2.7 mm from C1 and U1.7 2.704 mm from C2: both pass at 3 mm (C2, C4, C5 far from U1.1 don't matter); at 2.702 only U1.7 fails
		setLayout({ decouplingMaxDistance: 2.702 });
		const tight = await run(["decoupling_placement"]);
		expect(keys(tight, "decoupling_placement", "warning").filter(k => !k.includes("|ground|"))).toEqual(["decoupling_placement|U1.7|VDD", "decoupling_placement|U1.3|VIO"]);
		expect(tight.find(f => f.key === "decoupling_placement|U1.7|VDD")!.message).toStartWith("U1.7 (VDD_7) on VDD: the nearest decoupling capacitor, C2, is 2.704 mm away");
	}, 60_000);

	test("placement: X1 across the edge, X3 in the footprint keepout (X6, turned 45°, only by its courtyard's bounding box), X2 not placed yet, counts", () => {
		expect(all.filter(f => f.check === "placement").map(f => [f.severity, f.message])).toEqual([
			["warning", "X1: pad 2 outside the board outline or crossing its edge"],
			["error", "X3: its courtyard overlaps rule area NO_PARTS, which keeps footprints out on F.Cu"],
			["info", "13 of 14 footprints placed on the board: 12 top, 1 bottom; 1 locked"],
			["info", "1 footprints not placed yet (origin off the board): X2"],
		]);
		expect(keys(all, "layout")).toEqual(["layout|none"]);
	});

	test("layout: near, lengthGroups (spread boundary), paths, viaParity, copperArea, heightZones; each quoting its why", async () => {
		const c = Copper.of(Board.load(path.join(dir, "layout.kicad_pcb")));
		const len = (n: string) => c.netLength(n, { useHeight: true }).total;
		const spread = Math.abs((len("D_P") + len("D_N")) / 2 - len("CLK"));
		const branch = (to: string) => c.path("U1.1", to, { useHeight: true })!.length;
		const diff = Math.abs(branch("C2.1") - branch("C1.1"));
		expect(diff).toBeGreaterThan(5);
		setLayout({
			near: [
				{ a: "C1", b: "U1.1", max: 3, why: "close" },
				{ a: "C3", b: "U1", max: 5, why: "far" },
			],
			lengthGroups: [
				{ name: "lanes", nets: ["D_*", "clk"], maxSpread: spread, why: "skew" },
				{ name: "tight", nets: ["D_*", "CLK"], maxSpread: spread - 0.001, maxLength: 10, why: "skew" },
				{ name: "open", nets: ["VIO", "CLK"], maxSpread: 1, why: "skew" },
			],
			paths: [{ name: "vdd", from: "U1.1", to: ["C1.1", "C2.1"], maxLength: 15, maxDiff: 5, why: "star" }],
			viaParity: [
				{ name: "pair", nets: ["D_P", "D_N"], why: "symmetry" },
				{ name: "clk", nets: ["D_P", "CLK"], why: "symmetry" },
				{ name: "open", nets: ["VIO", "CLK"], why: "symmetry" },
			],
			copperArea: [
				{ name: "plane", net: "GND", layers: ["In1.Cu"], minArea: 2000, why: "heat" },
				{ name: "under", net: "GND", layers: ["In1.Cu"], minArea: 100, near: "U1", within: 2, why: "heat" },
				{ name: "more", net: "GND", layers: ["In1.Cu"], minArea: 120, minVias: 2, near: "U1", within: 2, why: "heat" },
			],
			heightZones: [
				{ name: "lens", center: [8, 8], r0: 2, slope: 1, heights: { "Fixture:C": 1, J1: 5 }, why: "field of view" },
				// X5 at (30, 5) turned 45°: its courtyard's short side faces (33, 8) 3.443 mm away; its bounding box's corner is 1.943 mm away
				{ name: "turned", center: [33, 8], r0: 3, slope: 0, heights: { X5: 1 }, why: "outline" },
				{ name: "turned closer", center: [33, 8], r0: 3.5, slope: 0, heights: { X5: 1 }, why: "outline" },
			],
		});
		const f = await run(["layout"]);
		const by = Object.fromEntries(f.map(x => [x.key, x]));
		expect(f.map(x => `${x.severity} ${x.key}`)).toEqual([
			"warning layout|near|C3 – U1",
			"warning layout|lengthGroups|tight|maxLength|D_P / D_N",
			"warning layout|lengthGroups|tight|maxLength|CLK",
			"warning layout|lengthGroups|tight|spread",
			"info layout|lengthGroups|open|unrouted",
			"warning layout|paths|vdd|diff",
			"warning layout|viaParity|clk",
			"info layout|viaParity|open|unrouted",
			"warning layout|copperArea|more|area",
			"warning layout|copperArea|more|vias",
			"warning layout|heightZones|lens|C5",
			"warning layout|heightZones|lens|R1|unknown",
			"warning layout|heightZones|turned closer|X5",
		]);
		expect(by["layout|near|C3 – U1"]!.message).toMatch(/^near C3 – U1: 9\.\d+ mm apart \(copper edge to edge, max 5 mm\) \(why: far\)$/);
		expect(by["layout|lengthGroups|open|unrouted"]!.message).toBe("lengthGroups open: not routed yet: VIO (why: skew)");
		expect(by["layout|viaParity|clk"]!.message).toBe("viaParity clk: via counts differ: D_P 1, CLK 0 (why: symmetry)");
		// the fill under U1 (pads 16.7..23.3 grown by 2: 10.6 mm square) is whole, one GND via in it
		expect(by["layout|copperArea|more|area"]!.message).toMatch(/^copperArea more: GND on In1\.Cu within 2 mm of U1: 112\.\d+ mm² of copper \(min 120 mm²\) \(why: heat\)$/);
		expect(by["layout|copperArea|more|vias"]!.message).toBe("copperArea more: GND on In1.Cu within 2 mm of U1: 1 via (min 2) (why: heat)");
		// C5's courtyard 2.99 mm from the centre (needs 2 + 1 × 1), C4's 3.01 (passes); R1 has no height, 3.5 mm < 2 + 1 × 5
		expect(by["layout|heightZones|lens|C5"]!.message).toBe('heightZones lens: C5 (1 mm tall, by "Fixture:C") is 2.99 mm from (8, 8) (courtyard), needs 3 mm (why: field of view)');
		expect(by["layout|heightZones|lens|R1|unknown"]!.message).toStartWith("heightZones lens: R1 (Fixture:R) is 3.5 mm from (8, 8) (courtyard) and its height is unknown");
		expect(by["layout|heightZones|turned closer|X5"]!.message).toBe('heightZones turned closer: X5 (1 mm tall, by "X5") is 3.443 mm from (33, 8) (courtyard), needs 3.5 mm (why: outline)');
	}, 60_000);

	test("stale config: refs, pads, nets, layers and classes the board doesn't have are errors naming the entry", async () => {
		setLayout({
			impedanceClasses: ["HS"],
			near: [{ a: "U9.1", b: "U1", max: 1, why: "w" }],
			lengthGroups: [{ name: "g", nets: ["D_*", "NOPE"], maxSpread: 1, why: "w" }],
			paths: [{ name: "p", from: "U1.1", to: ["U1.99", "J1.3"], maxDiff: 1, why: "w" }],
			copperArea: [{ name: "a", net: "GND", layers: ["In3.Cu"], minArea: 1, why: "w" }],
			heightZones: [{ name: "h", center: [0, 0], r0: 1, slope: 1, heights: { Q7: 2, "Lib:None*": 1 }, why: "w" }],
		});
		const f = await run(["layout", "return_path", "via_return"]);
		expect(f.filter(x => x.severity === "error").map(x => x.message)).toEqual([
			"near U9.1 – U1: no footprint U9 on the board (stale config) (why: w)",
			'lengthGroups g: no net "NOPE" on the board (stale config) (why: w)',
			"paths p: U1 has no pad 99 (pads: 1, 2, 3, 4, 5, 6, 7) (stale config) (why: w)",
			'copperArea a: "In3.Cu" is not a copper layer of the board (F.Cu, In1.Cu, In2.Cu, B.Cu) (why: w)',
			'heightZones h: heights names "Q7", which matches no footprint on the board (stale config) (why: w)',
			'heightZones h: heights names "Lib:None*", which matches no footprint on the board (stale config) (why: w)',
			'settings.layout.impedanceClasses names net class "HS", which the .kicad_pro doesn\'t have (classes: Default, DP, Power)',
			'settings.layout.impedanceClasses names net class "HS", which the .kicad_pro doesn\'t have (classes: Default, DP, Power)',
		]);
		// a pad on another net is a config error too
		setLayout({ paths: [{ name: "p", from: "U1.1", to: ["J1.3"], maxDiff: 1, why: "w" }] });
		expect((await run(["layout"])).map(x => x.message)).toEqual(["paths p: U1.1 is on VDD but J1.3 is on GND (stale config) (why: w)"]);
	}, 60_000);

	test("board and schematic out of step, returns through a through-hole ground pad, footprints without copper pads", async () => {
		// a copy of the board with U1 pad 7 (VDD) gone, C3's VIO pad on no net, C5 gone, X5's pads gone and the GND via at (46, 18.1) gone
		const drift = path.join(root, "drift");
		fs.cpSync(dir, drift, { recursive: true });
		const file = path.join(drift, "layout.kicad_pcb");
		const b = Board.load(file);
		const pad = (ref: string, n: string) => children(b.footprint(ref), "pad").find(p => text(p, 1) === n)!;
		const u1 = b.footprint("U1");
		u1.splice(u1.indexOf(pad("U1", "7")), 1);
		const c3 = pad("C3", "1");
		c3.splice(c3.indexOf(child(c3, "net")!), 1);
		b.remove(b.footprint("C5"));
		const x5 = b.footprint("X5");
		for (const p of children(x5, "pad")) x5.splice(x5.indexOf(p), 1);
		b.remove(children(b.tree, "via").find(v => num(child(v, "at"), 1) === 46 && num(child(v, "at"), 2) === 18.1)!);
		fs.writeFileSync(file, b.serialize());
		fs.writeFileSync(
			path.join(root, ".omp", "kicad.json"),
			JSON.stringify({
				projects: {
					drift: {
						layout: {
							viaReturnMaxDistance: 3,
							near: [{ a: "X5", b: "U1", max: 1, why: "w" }],
							copperArea: [{ name: "a", net: "GND", layers: ["In1.Cu"], minArea: 1, near: "X5", within: 1, why: "w" }],
						},
					},
				},
			}),
		);
		const r = await runChecks(root, { project: "drift", checks: ["decoupling_placement", "via_return", "layout"] });
		expect(r.report).not.toContain("FAILED TO RUN");
		expect(keys(r.findings, "decoupling_placement").sort()).toEqual(["decoupling_placement|drift|C5", "decoupling_placement|drift|U1.3|VIO", "decoupling_placement|drift|U1.7"]);
		expect(r.findings.find(f => f.key === "decoupling_placement|drift|U1.3|VIO")!.message).toBe(
			"U1.3 on VIO: none of its capacitors on the board (C3) has a pad on VIO (board and schematic out of step: update the board from the schematic)",
		);
		// D_P's via (46, 19) has J1.3, a through-hole GND pad, 2.83 mm away; D_N's (46, 21) is 4.47 mm from it
		expect(r.findings.filter(f => f.check === "via_return").map(f => f.message)).toEqual(["D_N (class DP): 1 via changing layers with no ground via or through-hole ground pad within 3 mm: (46, 21)"]);
		expect(r.findings.filter(f => f.check === "layout").map(f => `${f.severity} ${f.message}`)).toEqual([
			"error near X5 – U1: X5 has no copper pads to measure from (why: w)",
			"error copperArea a: X5 has no copper pads to measure from (why: w)",
		]);
	}, 60_000);
});

describe("board review checks on espion-cam (183 footprints, 21 placed, no tracks)", () => {
	test("quick and quiet: info findings only", async () => {
		const t = performance.now();
		const r = await runChecks(REPO, { project: "hardware/espion-cam", checks: NEW_CHECKS });
		const ms = performance.now() - t;
		expect(r.report).not.toContain("FAILED TO RUN");
		expect(ms).toBeLessThan(5000);
		expect(r.findings.filter(f => f.severity !== "info")).toEqual([]);
		expect(new Set(r.findings.map(f => f.check))).toEqual(new Set(NEW_CHECKS));
	}, 60_000);

	test("ring LEDs turned ±30° clear the lens cone by their courtyard outline, which their bounding box would not", () => {
		const board = Board.load(path.join(REPO, "hardware/espion-cam/espion-cam.kicad_pcb"));
		// the lens cone's centre in .omp/kicad.json; D4 (2.3 mm tall) needs 7 + 0.83 × 2.3 = 8.909 mm, its bounding box reaches 7.511
		expect(courtyardDistance(courtyardEdges(board.footprint("D4")), [165, 159])).toBeCloseTo(9.32, 2);
	});
});
