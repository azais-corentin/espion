import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import { checks as connectivityChecks } from "../src/checks/connectivity";
import { checks as partChecks } from "../src/checks/parts";
import { druProblems, checks as rulesChecks } from "../src/checks/rules";
import { Project } from "../src/project";
import { runEdit } from "../src/txn";
import type { CheckDef, Finding } from "../src/types";
import { cleanup, FIXTURE_COMMIT, HAS_KICAD, PROJECT, REPO, stageFixture, TEST_SHEET_EXPECT, TEST_SHEET_OPS } from "./helpers";

const CHECKS = new Map<string, CheckDef>([...connectivityChecks, ...partChecks, ...rulesChecks].map(c => [c.name, c]));

/** The staged fixture as a git repo that sees the real repo's objects, so FIXTURE_COMMIT resolves. */
function stageGitFixture(): string {
	const root = stageFixture();
	const init = Bun.spawnSync(["git", "init", "-q", root], { stderr: "pipe" });
	if (init.exitCode !== 0) throw new Error(init.stderr.toString());
	fs.writeFileSync(path.join(root, ".git", "objects", "info", "alternates"), `${path.join(REPO, ".git", "objects")}\n`);
	return root;
}

function run(name: string, project: Project, analysis: Analysis, baseline = FIXTURE_COMMIT): Promise<Finding[]> {
	return CHECKS.get(name)!.run({ project, analysis, baseline, args: {} });
}

const bySeverity = (fs_: Finding[], s: Finding["severity"]) => fs_.filter(f => f.severity === s);

describe.skipIf(!HAS_KICAD)("project checks on the fixture", () => {
	let root: string;
	let project: Project;
	let analysis: Analysis;

	beforeAll(() => {
		root = stageGitFixture();
		project = Project.open(PROJECT, root);
		analysis = new Analysis(project);
	});
	afterAll(() => cleanup(root));

	test("erc reports the two IMU pin conflicts KiCad has excluded", async () => {
		const f = await run("erc", project, analysis);
		expect(f).toHaveLength(2);
		for (const x of f) {
			expect(x.sheet).toBe("/IMU/");
			expect(x.severity).toBe("info");
			expect(x.message).toContain("#FLG09");
		}
		expect(f.map(x => x.key).sort()).toEqual([
			"erc|pin_to_pin|Symbol #FLG09 Pin 1 [Power output, Line]|Symbol U12 Pin 1 [SDO/SA0, Bidirectional, Line]",
			"erc|pin_to_pin|Symbol #FLG09 Pin 1 [Power output, Line]|Symbol U12 Pin 2 [SDX, Bidirectional, Line]",
		]);
	});

	test("connectivity is unchanged against the fixture commit", async () => {
		const f = await run("connectivity", project, analysis);
		expect(f.map(x => x.key)).toEqual(["connectivity|unchanged"]);
	});

	test("connectivity rejects a baseline that doesn't exist", async () => {
		await expect(run("connectivity", project, analysis, "no-such-ref")).rejects.toThrow("no-such-ref");
	});

	test("footprints all resolve and match the symbols' pins", async () => {
		expect(await run("footprints", project, analysis)).toEqual([]);
	});

	test("fields: no errors (U1's LCSC is exempt), DNP parts kept in the BOM are warned", async () => {
		const f = await run("fields", project, analysis);
		expect(bySeverity(f, "error")).toEqual([]);
		expect(f.map(x => x.key).sort()).toEqual(["fields|dnp_in_bom|R36", "fields|dnp_in_bom|R37"]);
	});

	test("netclasses: only the per-class summary", async () => {
		const f = await run("netclasses", project, analysis);
		expect(f.map(x => x.key)).toEqual(["netclasses|summary"]);
		expect(f[0]!.message).toContain("MIPI_CSI 12");
	});

	test("rules: the real rules file loads in KiCad", async () => {
		expect(await druProblems(project)).toEqual([]);
		expect(await run("rules", project, analysis)).toEqual([]);
	});
});

describe.skipIf(!HAS_KICAD)("project checks on a broken fixture", () => {
	let root: string;
	let project: Project;
	let analysis: Analysis;
	let druOriginal: string;

	beforeAll(() => {
		root = stageGitFixture();
		const dir = path.join(root, PROJECT);
		const imu = path.join(dir, "imu.kicad_sch");
		fs.writeFileSync(
			imu,
			fs
				.readFileSync(imu, "utf8")
				.replaceAll('(property "MPN" "CL05B104KB54PNC"', '(property "MPN" ""')
				.replaceAll('"Package_LGA:LGA-14_3x2.5mm_P0.5mm_LayoutBorder3x4y"', '"Package_LGA:No_Such_Footprint"'),
		);
		const pro = path.join(dir, "espion-cam.kicad_pro");
		const data = JSON.parse(fs.readFileSync(pro, "utf8"));
		data.net_settings.netclass_patterns = data.net_settings.netclass_patterns.filter((p: { netclass: string }) => p.netclass !== "USB");
		data.net_settings.netclass_patterns.push({ netclass: "MDI", pattern: "/Ethernet/NO_SUCH_*" });
		fs.writeFileSync(pro, JSON.stringify(data, null, 2));
		project = Project.open(PROJECT, root);
		analysis = new Analysis(project);
		druOriginal = fs.readFileSync(project.druFile, "utf8");
	});
	afterAll(() => cleanup(root));

	test("connectivity reports the field and footprint changes", async () => {
		const keys = (await run("connectivity", project, analysis)).map(x => x.key);
		expect(keys).toContain("connectivity|field|C50|MPN");
		expect(keys).toContain("connectivity|field|C51|MPN");
		expect(keys).toContain("connectivity|field|U12|footprint");
		expect(keys).not.toContain("connectivity|unchanged");
	});

	test("footprints: an unresolvable footprint is an error", async () => {
		const f = await run("footprints", project, analysis);
		expect(f.map(x => [x.severity, x.key])).toEqual([["error", "footprints|unresolved|U12"]]);
	});

	test("fields: a missing MPN is an error", async () => {
		const errors = bySeverity(await run("fields", project, analysis), "error").map(x => x.key);
		expect(errors.sort()).toEqual(["fields|missing|C50|MPN", "fields|missing|C51|MPN"]);
	});

	test("netclasses: diff pairs left in Default, a dead pattern and an empty class", async () => {
		const f = await run("netclasses", project, analysis);
		const keys = f.map(x => `${x.severity} ${x.key}`);
		expect(keys).toContain("warning netclasses|pair_default|/MCU/USB_D_P");
		expect(keys).toContain("warning netclasses|pair_default|/MCU/USB_C_D_P");
		expect(keys).toContain("warning netclasses|pattern_unused|MDI|/Ethernet/NO_SUCH_*");
		expect(keys).toContain("info netclasses|class_empty|USB");
	});

	test("rules: a misspelt constraint makes KiCad ignore the file, and is located", async () => {
		fs.writeFileSync(project.druFile, druOriginal.replace("(constraint clearance (min 0.55mm))", "(constraint clearence (min 0.55mm))"));
		const p = await druProblems(project);
		expect(p.length).toBeGreaterThan(1);
		expect(p.slice(1).join("\n")).toContain("clearence");
		const f = await run("rules", project, analysis);
		expect(f.map(x => [x.severity, x.key])).toEqual([["error", "rules|parse"]]);
	});

	test("rules: unbalanced parentheses make KiCad ignore the file, and are located", async () => {
		fs.writeFileSync(project.druFile, druOriginal.replace("(constraint clearance (min 2mm)))", "(constraint clearance (min 2mm))"));
		const p = await druProblems(project);
		expect(p.length).toBeGreaterThan(1);
		expect(p.slice(1).join("\n")).toContain("missing \")\"");
	});

	test("rules: a condition naming an unknown net class is a warning", async () => {
		fs.writeFileSync(project.druFile, druOriginal.replace("A.NetClass == 'MDI'", "A.NetClass == 'MDI_TYPO'"));
		const f = await run("rules", project, analysis);
		expect(f.map(x => [x.severity, x.key])).toEqual([["warning", "rules|netclass|MDI spacing|MDI_TYPO"]]);
	});
});

describe.skipIf(!HAS_KICAD)("connectivity: compact report, detail and sheets", () => {
	let root: string;
	let project: Project;
	let analysis: Analysis;
	const conn = (args: Record<string, unknown>, sheets?: string[]) =>
		CHECKS.get("connectivity")!.run({ project, analysis, baseline: FIXTURE_COMMIT, args, sheets: sheets?.map(s => project.sheet(s)) });
	const keys = (f: Finding[]) => f.map(x => x.key).sort();

	beforeAll(async () => {
		root = stageGitFixture();
		const dir = path.join(root, PROJECT);
		// three pure renames: two local nets on Power, one on LED driver
		for (const [file, from, to] of [
			["power.kicad_sch", "FB_3V3", "FB_3V3_X"],
			["power.kicad_sch", "SW_3V3", "SW_3V3_X"],
			["led_driver.kicad_sch", "BST_COMP", "BST_COMP_X"],
		] as const) {
			const f = path.join(dir, file);
			fs.writeFileSync(f, fs.readFileSync(f, "utf8").replace(`(label "${from}"`, `(label "${to}"`));
		}
		// a new sheet "Test": R66/R67 on a new net A and on GND
		const r = await runEdit({ ops: TEST_SHEET_OPS, expect: TEST_SHEET_EXPECT }, root, undefined);
		expect(r.applied).toBe(true);
		project = Project.open(PROJECT, root);
		analysis = new Analysis(project);
	}, 120_000);
	afterAll(() => cleanup(root));

	test("default: renames collapse into one count line; a net that kept its name shows its pin delta", async () => {
		const f = await conn({});
		expect(keys(f)).toEqual(["connectivity|added|R66", "connectivity|added|R67", "connectivity|net|/Test/A", "connectivity|net|GND", "connectivity|renamed"]);
		expect(f.find(x => x.key === "connectivity|renamed")!.message).toStartWith("3 nets renamed");
		expect(f.find(x => x.key === "connectivity|net|GND")!.message).toMatch(/^net GND: added \[R66\.2, R67\.2\], removed \[\] \(\d+ pins now\)$/);
		expect(f.find(x => x.key === "connectivity|net|/Test/A")!.message).toBe("net /Test/A [R66.1, R67.1] (new net)");
	});

	test("detail: every rename and full pin lists", async () => {
		const f = await conn({ detail: true });
		expect(keys(f)).toContain("connectivity|renamed|/Power/FB_3V3|/Power/FB_3V3_X");
		expect(keys(f)).toContain("connectivity|renamed|/LED driver/BST_COMP|/LED driver/BST_COMP_X");
		expect(keys(f)).not.toContain("connectivity|renamed");
		expect(f.find(x => x.key === "connectivity|net|GND")!.message).toStartWith("net GND is now [");
	});

	test("sheets: only parts on them and nets with a pin on or named under them", async () => {
		expect(keys(await conn({}, ["Test"]))).toEqual(["connectivity|added|R66", "connectivity|added|R67", "connectivity|net|/Test/A", "connectivity|net|GND"]);
		const power = await conn({}, ["Power"]);
		expect(keys(power)).toEqual(["connectivity|net|GND", "connectivity|renamed"]);
		expect(power.find(x => x.key === "connectivity|renamed")!.message).toStartWith("2 nets renamed");
		expect(keys(await conn({}, ["IMU"]))).toEqual(["connectivity|net|GND"]);
	});
});
