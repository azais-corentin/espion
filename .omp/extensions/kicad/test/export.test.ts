import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { Board, flipToBack } from "../src/board";
import { type Component, run } from "../src/cli";
import { loadConfig, projectConfig } from "../src/config";
import { assemblyParts, bomCsv, bomRows, checkCorrections, correctionFor, cplCsv, cplRow, cplRows, parsePos, type PosRow, runExport } from "../src/export";
import { type EditRequest, runEdit } from "../src/txn";
import { BOARD_PROJECT, cleanup, HAS_KICAD, stageBoardFixture } from "./helpers";

/** A netlist component with defaults: fitted, in the BOM, on the board. */
function comp(ref: string, more: Partial<Component> = {}, fields: Record<string, string> = {}): Component {
	return {
		ref,
		value: "10k",
		footprint: "Resistor_SMD:R_0603_1608Metric",
		datasheet: "",
		description: "",
		lib: "Device",
		part: "R",
		sheetName: "/",
		sheetFile: "x.kicad_sch",
		sheetPath: "/",
		uuids: [],
		fpFilters: "",
		fields,
		dnp: false,
		excludeFromBom: false,
		excludeFromBoard: false,
		...more,
	};
}

describe("assembly: CPL", () => {
	const at = (side: "top" | "bottom", rot: number): PosRow => ({ ref: "U1", x: 10, y: -20, rot, side });

	test("rotation: top as KiCad's, bottom as 180° − KiCad's, plus the correction, in [0, 360)", () => {
		expect(cplRow(at("top", 0))).toEqual({ ref: "U1", x: 10, y: -20, rot: 0, layer: "Top" });
		expect(cplRow(at("top", -90)).rot).toBe(270);
		expect(cplRow(at("top", 90), { rot: 270 }).rot).toBe(0);
		expect(cplRow(at("top", 180), { rot: 180 }).rot).toBe(0);
		expect(cplRow(at("bottom", 0))).toEqual({ ref: "U1", x: 10, y: -20, rot: 180, layer: "Bottom" });
		expect(cplRow(at("bottom", 30)).rot).toBe(150);
		expect(cplRow(at("bottom", 30), { rot: 90 }).rot).toBe(240);
		expect(cplRow(at("bottom", -90)).rot).toBe(270);
		expect(cplRow(at("bottom", 180), { rot: -90 }).rot).toBe(270);
	});

	test("offset: footprint frame turned with the footprint, mirrored on the bottom; CPL y up", () => {
		// top, unrotated: 1 mm right, 2 mm down on the board, so y − 2 in the CPL
		expect(cplRow(at("top", 0), { offset: [1, 2] })).toMatchObject({ x: 11, y: -22 });
		// top, turned 90° counter-clockwise: the footprint's "down" points right on the board
		expect(cplRow(at("top", 90), { offset: [0, 0.5] })).toMatchObject({ x: 10.5, y: -20 });
		// bottom, unrotated: pcbnew's flip mirrors y, so "down" in the footprint is up on the board
		expect(cplRow(at("bottom", 0), { offset: [1, 2] })).toMatchObject({ x: 11, y: -18 });
		// Reference rows worked out by hand from Bouni's kicad-jlcpcb-tools, fabrication.py at commit
		// b5e193b5166d4e47fea68dbbf96d88dae33ad3ec: rotation lines 622-625 (bottom: (180 − rot) % 360),
		// correction offset lines 648-656 (offset turned by that rotation, x negated on the bottom, board
		// y down), Mid X/Y lines 955-956 (y negated). Footprint at board (10, 20), offset [0.7, −1.3],
		// correction rot 90: [side, KiCad rot, Mid X, Mid Y, rotation without and with the correction].
		const bouni: [side: "top" | "bottom", rot: number, x: number, y: number, plain: number, corrected: number][] = [
			["top", 30, 9.9562, -18.5242, 30, 120],
			["top", 90, 8.7, -19.3, 90, 180],
			["top", -45, 11.4142, -19.5757, 315, 45],
			["bottom", 30, 11.2562, -20.7758, 150, 240],
			["bottom", 90, 11.3, -19.3, 90, 180],
			["bottom", -45, 9.5757, -21.4142, 225, 315],
		];
		for (const [side, rot, x, y, plain, corrected] of bouni) {
			const row = cplRow(at(side, rot), { offset: [0.7, -1.3] });
			expect([row.x, row.y, row.rot]).toEqual([expect.closeTo(x, 4), expect.closeTo(y, 4), plain]);
			expect(cplRow(at(side, rot), { rot: 90, offset: [0.7, -1.3] }).rot).toBe(corrected);
		}
	});

	test("first matching pattern wins, case-insensitive, `*` and `?`", () => {
		const c = { "Fixture:LED_*": { rot: 180 }, "Resistor_SMD:R_0?0?_*": { rot: 0 }, "*:SOT-23*": { rot: 90 }, "*": { rot: 270 } };
		expect(correctionFor("Fixture:LED_Fixture_0603", c)?.pattern).toBe("Fixture:LED_*");
		expect(correctionFor("resistor_smd:r_0603_1608metric", c)?.pattern).toBe("Resistor_SMD:R_0?0?_*");
		expect(correctionFor("Resistor_SMD:R_0402_1005Metric", c)?.pattern).toBe("Resistor_SMD:R_0?0?_*");
		expect(correctionFor("Resistor_SMD:R_1206_3216Metric", c)?.pattern).toBe("*");
		expect(correctionFor("Package_TO_SOT_SMD:SOT-23-5", c)?.pattern).toBe("*:SOT-23*");
		expect(correctionFor("Package_TO_SOT_SMD:SOT-23-5", { "Package_TO_SOT_SMD:SOT-23": {} })).toBeUndefined();
	});

	test("rows of the assembled parts only; corrected and unconfirmed footprints; rot 0 confirms", () => {
		const pos: PosRow[] = [
			{ ref: "R10", x: 1, y: -1, rot: 0, side: "top" },
			{ ref: "R2", x: 2, y: -1, rot: 90, side: "top" },
			{ ref: "C1", x: 3, y: -1, rot: 0, side: "top" },
			{ ref: "D1", x: 4, y: -1, rot: 0, side: "bottom" },
			{ ref: "U1", x: 5, y: -1, rot: 0, side: "top" },
		];
		const ids = new Map([
			["R2", "Resistor_SMD:R_0603_1608Metric"],
			["R10", "Resistor_SMD:R_0603_1608Metric"],
			["D1", "LED_SMD:LED_0603_1608Metric"],
			["U1", "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm"],
		]);
		const r = cplRows(pos, ids, { "LED_SMD:*": { rot: 180 }, "Package_SO:*": { rot: 0, offset: [0, 0] } });
		// C1 (not assembled) has no row; natural order
		expect(r.rows.map(x => [x.ref, x.rot, x.layer])).toEqual([
			["D1", 0, "Bottom"],
			["R2", 90, "Top"],
			["R10", 0, "Top"],
			["U1", 0, "Top"],
		]);
		expect([...r.corrected]).toEqual([["D1", "LED_SMD:*"]]);
		expect([...r.unconfirmed]).toEqual([["Resistor_SMD:R_0603_1608Metric", ["R2", "R10"]]]);
		expect(cplCsv(r.rows.slice(0, 2))).toBe('"Designator","Mid X","Mid Y","Layer","Rotation"\r\n"D1","4mm","-1mm","Bottom","0"\r\n"R2","2mm","-1mm","Top","90"\r\n');
	});

	test("KiCad's position file parsed by column name; malformed corrections named", () => {
		const src = 'Ref,Val,Package,PosX,PosY,Rot,Side\n"C1","100nF","C_0603_1608Metric",12.500000,-7.250000,90.000000,top\n"U1","X, Y","SOIC-8",3.000000,-4.000000,-45.000000,bottom\n';
		expect(parsePos(src)).toEqual([
			{ ref: "C1", x: 12.5, y: -7.25, rot: 90, side: "top" },
			{ ref: "U1", x: 3, y: -4, rot: -45, side: "bottom" },
		]);
		expect(() => parsePos("Ref,Val\n")).toThrow("KiCad's position file has no PosX column");
		expect(() => checkCorrections({ "A:*": { rot: "90" } }, "k.json")).toThrow('k.json assembly.corrections["A:*"]: rot must be a number of degrees');
		expect(() => checkCorrections({ "A:*": { offset: [1] } }, "k.json")).toThrow("offset must be [x, y] in mm");
		expect(() => checkCorrections({ "A:*": { rotation: 90 } }, "k.json")).toThrow('unknown key "rotation"');
		expect(() => checkCorrections({ "A:*": 90 }, "k.json")).toThrow("must be an object");
		expect(checkCorrections({ "A:*": { rot: 90, offset: [0, 0.5] } }, "k.json")).toEqual({ "A:*": { rot: 90, offset: [0, 0.5] } });
	});

	test("config: a project's corrections come before the defaults' (first match wins) and override equal patterns", () => {
		const root = fs.mkdtempSync(path.join(os.tmpdir(), "kicad-export-cfg-"));
		try {
			fs.mkdirSync(path.join(root, ".omp"));
			fs.writeFileSync(
				path.join(root, ".omp", "kicad.json"),
				JSON.stringify({
					defaults: { assembly: { corrections: { "*": { rot: 0 }, "LED_SMD:*": { rot: 90 } } } },
					projects: { p: { assembly: { corrections: { "LED_SMD:LED_0603*": { rot: 180 }, "LED_SMD:*": { rot: 270 } } } } },
				}),
			);
			const cfg = loadConfig(root);
			const pc = projectConfig(cfg, path.join(root, "p")).assembly.corrections;
			expect(Object.entries(pc)).toEqual([
				["LED_SMD:LED_0603*", { rot: 180 }],
				["LED_SMD:*", { rot: 270 }],
				["*", { rot: 0 }],
			]);
			expect(projectConfig(cfg, path.join(root, "other")).assembly.corrections).toEqual({ "*": { rot: 0 }, "LED_SMD:*": { rot: 90 } });
		} finally {
			cleanup(root);
		}
	});
});

describe("assembly: BOM", () => {
	const board = new Map([
		["R1", "Resistor_SMD:R_0603_1608Metric"],
		["R2", "Resistor_SMD:R_0603_1608Metric"],
		["R10", "Resistor_SMD:R_0603_1608Metric"],
		["C1", "Capacitor_SMD:C_0603_1608Metric"],
		["C2", "Capacitor_SMD:C_0603_1608Metric"],
		["U6", "Module:Hand"],
		["U7", "Package_SO:SOIC-8"],
		["H1", "MountingHole:MountingHole_3.2mm_M3"],
		["LOGO1", "Logo:Espion"],
	]);
	const comps = [
		comp("R10", {}, { LCSC: "C25804" }),
		comp("R1", {}, { LCSC: "C25804" }),
		comp("R2", { value: "10k 1%" }, { LCSC: "c25804" }),
		comp("C1", { dnp: true, value: "100nF", footprint: "Capacitor_SMD:C_0603_1608Metric" }, { LCSC: "C14663" }),
		comp("C2", { excludeFromBom: true }, { LCSC: "C14663" }),
		comp("U6", { value: "ESP32-P4" }, { MPN: "X" }),
		comp("U7", { value: "TPS" }, { LCSC: " C123 " }),
		comp("J1", { excludeFromBoard: true }, { LCSC: "C999" }),
		comp("J2", {}, { LCSC: "C998" }),
		comp("#PWR01", { value: "GND" }),
		comp("H1", { value: "M3" }),
	];

	test("one row per LCSC part; DNP, BOM-excluded, no LCSC, not on the board and position-file-excluded parts left out with the reason; board-only footprints", () => {
		const a = assemblyParts(comps, board, new Set(["R1", "R2", "R10", "C1", "C2", "U6", "H1", "LOGO1"]));
		expect(a.parts.map(p => p.ref)).toEqual(["R1", "R2", "R10"]);
		expect(a.left).toEqual([
			{ ref: "C1", reason: "DNP" },
			{ ref: "C2", reason: "excluded from the BOM" },
			{ ref: "H1", reason: "no LCSC field" },
			{ ref: "J1", reason: "not on the board" },
			{ ref: "J2", reason: "not on the board" },
			{ ref: "U6", reason: "no LCSC field" },
			{ ref: "U7", reason: "excluded from position files" },
		]);
		expect(a.boardOnly).toEqual(["LOGO1"]);
		// without the position file U7 is assembled, its LCSC trimmed
		const all = assemblyParts(comps, board);
		expect(all.parts.map(p => [p.ref, p.lcsc])).toEqual([
			["R1", "C25804"],
			["R2", "c25804"],
			["R10", "C25804"],
			["U7", "C123"],
		]);
		const rows = bomRows(all.parts);
		expect(rows).toEqual([
			{ comment: "10k / 10k 1%", designators: ["R1", "R2", "R10"], footprint: "R_0603_1608Metric", lcsc: "C25804" },
			{ comment: "TPS", designators: ["U7"], footprint: "SOIC-8", lcsc: "C123" },
		]);
		expect(bomCsv(rows)).toBe(
			'"Comment","Designator","Footprint","JLCPCB Part #"\r\n"10k / 10k 1%","R1,R2,R10","R_0603_1608Metric","C25804"\r\n"TPS","U7","SOIC-8","C123"\r\n',
		);
	});
});

describe.skipIf(!HAS_KICAD)("kicad_export on the board fixture", () => {
	let root: string;
	let dir: string;
	const NOW = new Date(2026, 9, 30, 12, 0, 0);
	const edit = async (ops: Record<string, unknown>[], more: Partial<EditRequest> = {}) => {
		const r = await runEdit({ project: BOARD_PROJECT, ops, ...more }, root, undefined, undefined, () => []);
		expect(r.applied).toBe(true);
	};
	const config = (assembly: unknown, fields?: unknown) =>
		fs.writeFileSync(path.join(root, ".omp", "kicad.json"), JSON.stringify({ projects: { [BOARD_PROJECT]: { assembly, ...(fields ? { fields } : {}) } } }));

	beforeEach(() => {
		root = stageBoardFixture();
		dir = path.join(root, BOARD_PROJECT);
		fs.mkdirSync(path.join(root, ".omp"));
	});
	afterEach(() => cleanup(root));

	test("an unrouted, unplaced board: gerbers and assembly refused with what failed, an earlier run's files of them removed, step still written", async () => {
		await edit([{ op: "update_from_schematic" }]);
		// an earlier run's outputs in the same directory, and a file of the user's
		const fab = path.join(dir, "fab", "draft-2026-10-30");
		fs.mkdirSync(path.join(fab, "gerbers"), { recursive: true });
		for (const f of ["board-gerbers.zip", "gerbers/board-F_Cu.gtl", "board-bom.csv", "board-cpl.csv", "board.step", "notes.txt"]) fs.writeFileSync(path.join(fab, f), "old");
		const r = await runExport(root, { project: BOARD_PROJECT, outputs: ["step", "gerbers", "assembly"] }, undefined, NOW);
		expect(r.refused).toEqual(["gerbers", "assembly"]);
		expect(r.files).toEqual([`${BOARD_PROJECT}/fab/draft-2026-10-30/board.step`]);
		const step = fs.readFileSync(path.join(root, r.files[0]!), "utf8");
		expect(step.startsWith("ISO-10303-21;")).toBe(true);
		// the LED's stock 3D model made it in
		expect(step).toContain("LED_0603");
		expect(r.text).toContain("REFUSED gerbers and assembly: the board is not ready for fabrication:");
		expect(r.text).toContain("- KiCad DRC errors (zones refilled): 1\n    - invalid_outline:");
		expect(r.text).toContain("- unconnected items (connections not routed): 3");
		expect(r.text).toContain("- ERC errors: 1\n    - power_pin_not_driven:");
		expect(r.text).not.toContain("schematic parity");
		expect(r.text).toContain("Board: no outline, 2 copper layers (F.Cu, B.Cu); 3 footprints (3 top, 0 bottom); schematic: 3 parts, 1 DNP; to assemble: 2 parts, 2 LCSC part numbers.");
		expect(r.text).toContain("STEP: 3 3D models referenced, all found.");
		expect(fs.readdirSync(fab).sort()).toEqual(["board.step", "notes.txt"]);
		expect(r.text).toContain(
			"Removed the refused outputs' files from an earlier run: board/fab/draft-2026-10-30/board-gerbers.zip, board/fab/draft-2026-10-30/gerbers, board/fab/draft-2026-10-30/board-bom.csv, board/fab/draft-2026-10-30/board-cpl.csv",
		);

		await expect(runExport(root, { project: BOARD_PROJECT, outputs: [] })).rejects.toThrow("outputs: give one or more of gerbers, assembly, step");
		await expect(runExport(root, { project: BOARD_PROJECT, outputs: ["gerber"] })).rejects.toThrow('outputs: unknown output "gerber"');
		await expect(runExport(root, { project: BOARD_PROJECT, outputs: ["step"], rev: " " })).rejects.toThrow("rev: give a revision");
	}, 180_000);

	test("a placed, routed board with a bottom part passes the gate: zipped Gerbers and drill files, JLCPCB BOM and CPL from KiCad's position file with corrections; the fields check gates assembly only", async () => {
		// ERC: the fixture's GND has no power source
		await edit([
			{ op: "add_symbol", sheet: "root", lib_id: "power:PWR_FLAG", at: [139.7, 105.41] },
			{ op: "add_wire", sheet: "root", points: [[139.7, 105.41], [139.7, 106.68]] },
			{ op: "add_power", sheet: "root", net: "GND", at: [139.7, 106.68] },
		]);
		await edit([{ op: "update_from_schematic" }]);
		// R1 to the back, as pcbnew's flip leaves it (the toolkit has no flip op)
		const boardFile = path.join(dir, "board.kicad_pcb");
		const flipped = Board.load(boardFile);
		flipToBack(flipped.footprint("R1"));
		fs.writeFileSync(boardFile, flipped.serialize());
		await edit([
			{ op: "set_board_edges", outline: { rect: { x: 0, y: 0, width: 30, height: 20 } } },
			{ op: "place_footprint", ref: "R1", at: [8, 6], rot: 90 },
			{ op: "place_footprint", ref: "C1", at: [8, 14] },
			{ op: "place_footprint", ref: "D1", at: [20, 6] },
		]);
		// the toolkit has no routing op: tracks and vias written into the temp copy, as pcbnew saves them;
		// R1's pads (B.Cu, turned 90°): pad 1 at (8, 6.825), pad 2 at (8, 5.175)
		const seg = (a: number[], b: number[], net: string, layer = "F.Cu") =>
			`\t(segment\n\t\t(start ${a[0]} ${a[1]})\n\t\t(end ${b[0]} ${b[1]})\n\t\t(width 0.25)\n\t\t(layer "${layer}")\n\t\t(net "${net}")\n\t\t(uuid "${crypto.randomUUID()}")\n\t)\n`;
		const track = (pts: number[][], net: string, layer?: string) => pts.slice(1).map((p, i) => seg(pts[i]!, p, net, layer)).join("");
		const via = (x: number, y: number, net: string) =>
			`\t(via\n\t\t(at ${x} ${y})\n\t\t(size 0.6)\n\t\t(drill 0.3)\n\t\t(layers "F.Cu" "B.Cu")\n\t\t(net "${net}")\n\t\t(uuid "${crypto.randomUUID()}")\n\t)\n`;
		const src = fs.readFileSync(boardFile, "utf8");
		const end = src.lastIndexOf("\t(embedded_fonts");
		const copper =
			track([[8, 6.825], [8, 10]], "/VIN", "B.Cu") +
			via(8, 10, "/VIN") +
			track([[8, 10], [7.225, 14]], "/VIN") +
			track([[8, 5.175], [8, 3]], "LED_A", "B.Cu") +
			via(8, 3, "LED_A") +
			track([[8, 3], [20.7875, 3], [20.7875, 6]], "LED_A") +
			track([[8.775, 14], [19.2125, 14], [19.2125, 6]], "GND") +
			via(14, 14, "GND");
		fs.writeFileSync(boardFile, src.slice(0, end) + copper + src.slice(end));

		config({ corrections: { "Fixture:LED_*": { rot: 180, offset: [0, 0.5] } } });
		const first = await runExport(root, { project: BOARD_PROJECT, outputs: ["gerbers", "assembly"], rev: "A 1" }, undefined, NOW);
		expect(first.refused).toEqual([]);
		expect(first.text).not.toContain("REFUSED");
		const fab = path.join(dir, "fab", "A_1-2026-10-30");
		const gerbers = fs.readdirSync(path.join(fab, "gerbers")).sort();
		expect(gerbers).toEqual([
			"board-B_Cu.gbl",
			"board-B_Mask.gbs",
			"board-B_Paste.gbp",
			"board-B_Silkscreen.gbo",
			"board-Edge_Cuts.gm1",
			"board-F_Cu.gtl",
			"board-F_Mask.gts",
			"board-F_Paste.gtp",
			"board-F_Silkscreen.gto",
			"board-NPTH-drl_map.pdf",
			"board-NPTH.drl",
			"board-PTH-drl_map.pdf",
			"board-PTH.drl",
			"board-job.gbrjob",
		]);
		expect(first.files).toEqual([
			`${BOARD_PROJECT}/fab/A_1-2026-10-30/board-gerbers.zip`,
			...gerbers.map(g => `${BOARD_PROJECT}/fab/A_1-2026-10-30/gerbers/${g}`),
			`${BOARD_PROJECT}/fab/A_1-2026-10-30/board-bom.csv`,
			`${BOARD_PROJECT}/fab/A_1-2026-10-30/board-cpl.csv`,
		]);
		// Excellon in mm with the vias' 0.3 mm plated holes, (14, 14) among them
		const pth = fs.readFileSync(path.join(fab, "gerbers", "board-PTH.drl"), "utf8");
		expect(pth).toContain("METRIC");
		expect(pth).toMatch(/T1C0\.30*\n/);
		expect(pth).toContain("X14.0Y-14.0");
		// the zip holds exactly the plotted files, intact (Python's zipfile checks names, sizes and CRCs)
		const check = await run(
			[
				"python3",
				"-c",
				"import json,sys,zipfile; z=zipfile.ZipFile(sys.argv[1]); print(json.dumps([z.testzip(), sorted((i.filename, i.file_size, i.compress_type) for i in z.infolist())]))",
				path.join(fab, "board-gerbers.zip"),
			],
			{ cwd: root, repoRoot: root },
		);
		expect(check.stderr).toBe("");
		expect(JSON.parse(check.stdout)).toEqual([null, gerbers.map(g => [g, fs.statSync(path.join(fab, "gerbers", g)).size, 8])]);

		// C1 is DNP: left out of both
		expect(fs.readFileSync(path.join(fab, "board-bom.csv"), "utf8")).toBe(
			'"Comment","Designator","Footprint","JLCPCB Part #"\r\n"red","D1","LED_Fixture_0603","C72044"\r\n"1k","R1","R_0603_1608Metric","C21190"\r\n',
		);
		// D1 at (20, 6) turned 180 and its centre 0.5 mm down in the footprint (y − 0.5 in the CPL);
		// R1 on the bottom at KiCad rotation 90: 180 − 90
		expect(fs.readFileSync(path.join(fab, "board-cpl.csv"), "utf8")).toBe(
			'"Designator","Mid X","Mid Y","Layer","Rotation"\r\n"D1","20mm","-6.5mm","Top","180"\r\n"R1","8mm","-6mm","Bottom","90"\r\n',
		);
		expect(first.text).toContain("Board: 30 × 20 mm, 2 copper layers (F.Cu, B.Cu); 3 footprints (2 top, 1 bottom)");
		expect(first.text).toContain('Assembly: BOM 2 lines, 2 parts; CPL 2 rows, 1 corrected (D1 by "Fixture:LED_*").');
		expect(first.text).toContain("Left out of the BOM and CPL:\n- DNP: C1");
		expect(first.text).toContain("Not confirmed in JLCPCB's placement preview (no assembly.corrections entry in .omp/kicad.json;");
		expect(first.text).toContain("\n- Resistor_SMD:R_0603_1608Metric: R1");

		// a correction on the bottom part: Bouni's kicad-jlcpcb-tools (fabrication.py at b5e193b, lines
		// 622-625, 648-656, 955-956) gives rotation (180 − 90 + 90) = 180 and, for offset [0.5, 0.25]
		// turned by 90 with x negated on the bottom, board (8 − 0.25, 6 − 0.5): Mid (7.75, −5.5)
		config({ corrections: { "Fixture:LED_*": { rot: 180, offset: [0, 0.5] }, "Resistor_SMD:*": { rot: 90, offset: [0.5, 0.25] } } });
		const second = await runExport(root, { project: BOARD_PROJECT, outputs: ["assembly"], rev: "A 1" }, undefined, NOW);
		expect(second.refused).toEqual([]);
		expect(fs.readFileSync(path.join(fab, "board-cpl.csv"), "utf8")).toBe(
			'"Designator","Mid X","Mid Y","Layer","Rotation"\r\n"D1","20mm","-6.5mm","Top","180"\r\n"R1","7.75mm","-5.5mm","Bottom","180"\r\n',
		);
		expect(second.text).toContain("Every placed footprint is confirmed by an assembly.corrections entry.");

		// a required field the parts lack: assembly refused and its earlier files removed, gerbers written
		config({ corrections: {} }, { required: ["Tolerance"] });
		const third = await runExport(root, { project: BOARD_PROJECT, outputs: ["gerbers", "assembly"], rev: "A 1" }, undefined, NOW);
		expect(third.refused).toEqual(["assembly"]);
		expect(third.text).toContain("REFUSED assembly: the board is not ready for fabrication:\n- fields check errors: 2\n    - R1 (1k) has no Tolerance\n    - D1 (red) has no Tolerance");
		expect(third.text).toContain(
			"Removed the refused outputs' files from an earlier run: board/fab/A_1-2026-10-30/board-bom.csv, board/fab/A_1-2026-10-30/board-cpl.csv",
		);
		expect(third.text).not.toContain("REFUSED gerbers");
		expect(fs.readdirSync(fab).sort()).toEqual(["board-gerbers.zip", "gerbers"]);
		expect(fs.readdirSync(path.join(fab, "gerbers")).sort()).toEqual(gerbers);
	}, 300_000);
});
