import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import { checks, klcFindings } from "../src/checks/klc";
import * as cli from "../src/cli";
import { describeLibSymbol } from "../src/libs";
import { ops } from "../src/ops/library";
import { Project } from "../src/project";
import { queries } from "../src/queries/library";
import { child, children, num, parse, text } from "../src/sexpr";
import { validate } from "../src/spec";
import type { OpContext } from "../src/types";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

/** Apply library ops in memory, then write the files into the fixture and let KiCad re-save them (throws if KiCad rejects one). */
async function commit(root: string, list: Record<string, unknown>[]): Promise<string[]> {
	const project = Project.open(PROJECT, root);
	const ctx: OpContext = { project, libs: await project.libs(), notes: [], touch() {}, touchPart() {} };
	for (const o of list) {
		const def = ops.find(d => d.op === o.op)!;
		await def.apply(ctx, validate(String(o.op), o, def.fields, ["op"]));
	}
	const writes = project.pendingWrites();
	for (const [f, content] of writes) {
		fs.mkdirSync(path.dirname(f), { recursive: true });
		fs.writeFileSync(f, content);
	}
	for (const f of project.deleted) fs.rmSync(f);
	const ro = { cwd: project.dir, repoRoot: root };
	for (const f of writes.keys()) if (f.endsWith(".kicad_sym")) await cli.upgrade("sym", f, ro);
	// kicad-cli upgrades footprints per library directory only
	for (const dir of new Set([...writes.keys()].filter(f => f.endsWith(".kicad_mod")).map(f => path.dirname(f)))) await cli.upgrade("fp", dir, ro);
	return ctx.notes;
}

async function query(root: string, what: string, args: Record<string, unknown>): Promise<Record<string, unknown>> {
	const project = Project.open(PROJECT, root);
	const def = queries.find(q => q.what === what)!;
	const out = await def.run({ project, analysis: new Analysis(project), libs: () => project.libs() }, validate(what, args, def.fields));
	return out as Record<string, unknown>;
}

function libFiles(dir: string): string[] {
	return fs.readdirSync(dir, { recursive: true, encoding: "utf8" }).filter(f => /\.(kicad_sym|kicad_mod)$/.test(f)).map(f => path.join(dir, f));
}

const IC = {
	op: "lib_create_symbol",
	library: "Interface_Test",
	name: "TST1234",
	reference: "U",
	footprint: "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm",
	fp_filters: ["SOIC*3.9x4.9mm*P1.27mm*"],
	datasheet: "https://example.com/tst1234.pdf",
	description: "Test I2C interface IC, SOIC-8",
	keywords: "test interface i2c",
	pins: [
		{ number: 8, name: "VCC", type: "power_in", side: "top" },
		{ number: 1, name: "SDA", type: "bidirectional", side: "left" },
		{ number: 2, name: "SCL", type: "input", side: "left" },
		{ number: 3, name: "EN", type: "input", side: "left" },
		{ number: 5, name: "OUT", type: "output", side: "right" },
		{ number: 6, name: "~{INT}", type: "open_collector", side: "right" },
		{ number: 7, name: "FB", type: "input", side: "right" },
		{ number: 4, name: "GND", type: "power_in", side: "bottom" },
	],
};

const WSON = {
	op: "lib_generate_footprint",
	library: "Package_SON_Test",
	generator: "package/no_lead",
	spec: {
		device_type: "WSON",
		manufacturer: "Texas",
		part_number: "DSG0008A",
		size_source: "https://www.ti.com/lit/ds/symlink/mspm0c1104.pdf",
		body_size_x: "1.9 .. 2.0 .. 2.1",
		body_size_y: "1.9 .. 2.0 .. 2.1",
		overall_height: 0.8,
		num_pins_x: 0,
		num_pins_y: 4,
		pitch: 0.5,
		lead_width: "0.2 .. 0.25 .. 0.3",
		lead_len: "0.2 .. 0.3 .. 0.4",
		EP_size_x: "0.8 .. 0.9 .. 1.0",
		EP_size_y: "1.5 .. 1.6 .. 1.7",
		EP_num_paste_pads: [1, 2],
	},
};

const MODULE = {
	op: "lib_create_footprint",
	library: "Module_Test",
	name: "Test_Module_12x10mm",
	description: "Test 4-pad SMD module, 12x10mm (https://example.com/module.pdf)",
	tags: "module test",
	attr: "smd",
	body: { width: 12, height: 10 },
	model: true,
	pads: [
		{ number: 1, type: "smd", shape: "roundrect", at: [-5.5, -3.5], size: [2, 1.5] },
		{ number: 2, type: "smd", shape: "roundrect", at: [-5.5, 3.5], size: [2, 1.5] },
		{ number: 3, type: "smd", shape: "roundrect", at: [5.5, 3.5], size: [2, 1.5] },
		{ number: 4, type: "smd", shape: "roundrect", at: [5.5, -3.5], size: [2, 1.5] },
	],
};

describe("library ops", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	test.skipIf(!HAS_KICAD)("lib_create_symbol: new library registered, symbol loads, KLC clean, KiCad accepts it", async () => {
		await commit(root, [IC]);
		const project = Project.open(PROJECT, root);
		const libs = await project.libs();
		const file = path.join(root, "hardware/lib/Interface_Test.kicad_sym");
		expect(path.resolve(libs.symLib("Interface_Test").uri)).toBe(file);
		expect(fs.readFileSync(path.join(project.dir, "sym-lib-table"), "utf8")).toContain('(uri "${KIPRJMOD}/../lib/Interface_Test.kicad_sym")');
		const d = describeLibSymbol(libs.flatSymbol("Interface_Test:TST1234"));
		expect(d.pins.length).toBe(8);
		expect(d.pins.find(p => p.number === "6")).toMatchObject({ name: "~{INT}", type: "open_collector" });
		expect(d.properties.Footprint).toBe("Package_SO:SOIC-8_3.9x4.9mm_P1.27mm");
		expect(await klcFindings(project, [file])).toEqual([]);
		await expect(commit(root, [IC])).rejects.toThrow(/already exists/);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("lib_edit_symbol and lib_remove_symbol", async () => {
		await commit(root, [
			{ op: "lib_edit_symbol", library: "Interface_Test", name: "TST1234", properties: { Description: "Edited", MPN: "TST1234-X" }, pins: [{ number: "7", name: "ADJ", type: "passive" }] },
		]);
		const libs = await Project.open(PROJECT, root).libs();
		const d = describeLibSymbol(libs.flatSymbol("Interface_Test:TST1234"));
		expect(d.properties).toMatchObject({ Description: "Edited", MPN: "TST1234-X" });
		expect(d.pins.find(p => p.number === "7")).toMatchObject({ name: "ADJ", type: "passive" });

		await expect(commit(root, [{ op: "lib_remove_symbol", library: "MCU_Module_Waveshare", name: "ESP32-P4-Core" }])).rejects.toThrow(/U1/);
		await commit(root, [{ op: "lib_remove_symbol", library: "Interface_Test", name: "TST1234" }]);
		expect((await Project.open(PROJECT, root).libs()).symbolNames("Interface_Test")).toEqual([]);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("lib_generate_footprint: official no_lead generator part passes KLC except expected rules", async () => {
		const notes = await commit(root, [WSON]);
		expect(notes.join("\n")).toContain("Package_SON_Test:Texas_DSG0008A_WSON-8");
		const project = Project.open(PROJECT, root);
		const libs = await project.libs();
		const [name] = libs.footprintNames("Package_SON_Test");
		const pads = libs.footprintPads(`Package_SON_Test:${name}`).filter(p => p.num !== "");
		expect(pads.map(p => p.num).sort((a, b) => Number(a) - Number(b))).toEqual(["1", "2", "3", "4", "5", "6", "7", "8", "9"]);
		const findings = await klcFindings(project, libFiles(libs.fpLib("Package_SON_Test").uri));
		expect(findings.filter(f => f.severity !== "info")).toEqual([]);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("lib_generate_footprint: a spec can inherit a stock entry; a whole series is refused", async () => {
		const notes = await commit(root, [
			{
				op: "lib_generate_footprint",
				library: "Connector_Test",
				generator: "connector/generator",
				name: "JST_SHD_BM20B-SRDS-G-TF_Test",
				spec: { inherit: "defaults-top-entry", description: "JST SHD test, {num_pins:d} circuits", positions: [10] },
			},
		]);
		expect(notes.join("\n")).toContain("Connector_Test:JST_SHD_BM20B-SRDS-G-TF_Test");
		const libs = await Project.open(PROJECT, root).libs();
		expect(libs.footprintPads("Connector_Test:JST_SHD_BM20B-SRDS-G-TF_Test").filter(p => /^\d+$/.test(p.num)).length).toBe(20);
		await expect(
			commit(root, [{ op: "lib_generate_footprint", library: "Connector_Test", generator: "connector/pin_header_socket", spec: { inherit: "THT-1row_Straight_Right" } }]),
		).rejects.toThrow(/restrict it to one part/);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("lib_create_footprint: 4-pad SMD module passes KLC except expected rules", async () => {
		await commit(root, [MODULE]);
		const project = Project.open(PROJECT, root);
		const libs = await project.libs();
		const file = libs.footprintFile("Module_Test:Test_Module_12x10mm");
		const fp = parse(fs.readFileSync(file, "utf8"));
		expect(text(child(fp, "model"))).toBe("${KIPRJMOD}/../lib/Module_Test.3dshapes/Test_Module_12x10mm.step");
		const crtyd = children(fp, "fp_line").filter(l => text(child(l, "layer")) === "F.CrtYd");
		expect(crtyd.length).toBe(4);
		const xs = crtyd.flatMap(l => [num(child(l, "start"), 1), num(child(l, "end"), 1)]);
		expect(Math.min(...xs)).toBeCloseTo(-6.75, 6);
		const findings = await klcFindings(project, [file]);
		expect(findings.filter(f => f.severity !== "info")).toEqual([]);
		expect(findings.map(f => f.key)).toEqual(["klc|F9.3|Module_Test:Test_Module_12x10mm"]);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("lib_edit_footprint and lib_remove_footprint", async () => {
		await commit(root, [{ op: "lib_edit_footprint", library: "Module_Test", name: "Test_Module_12x10mm", tags: "module edited", model: null }]);
		const libs = await Project.open(PROJECT, root).libs();
		const fp = parse(fs.readFileSync(libs.footprintFile("Module_Test:Test_Module_12x10mm"), "utf8"));
		expect(text(child(fp, "tags"))).toBe("module edited");
		expect(child(fp, "model")).toBeUndefined();

		await expect(commit(root, [{ op: "lib_remove_footprint", library: "Module_Waveshare", name: "Waveshare_ESP32-P4-Core" }])).rejects.toThrow(/U1/);
		await commit(root, [{ op: "lib_remove_footprint", library: "Module_Test", name: "Test_Module_12x10mm" }]);
		expect((await Project.open(PROJECT, root).libs()).footprintNames("Module_Test")).toEqual([]);
	}, 60_000);

	test("refuses stock library names and non-project libraries", async () => {
		await expect(commit(root, [{ ...IC, library: "Device" }])).rejects.toThrow(/stock/);
		await expect(commit(root, [{ op: "lib_remove_footprint", library: "Package_SO", name: "SOIC-8_3.9x4.9mm_P1.27mm" }])).rejects.toThrow(/not a footprint library of the project/);
	});
});

describe("klc", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	test.skipIf(!HAS_KICAD)("the project library reproduces the expected checker messages of hardware/lib/README.md", async () => {
		const project = Project.open(PROJECT, root);
		const findings = await klcFindings(project, libFiles(path.join(root, "hardware/lib")));
		const by = (key: string) => findings.find(f => f.key === key);
		expect(by("klc|F9.3|Module_Waveshare:Waveshare_ESP32-P4-Core")?.severity).toBe("info");
		expect(by("klc|F9.3|Capacitor_SMD_JIERR:CP_Elec_8x10.5")?.message).toContain("STEP");
		expect(by("klc|F6.3|Package_SON_TI:Texas_DRV0006A_WSON-6-1EP_2x2mm_P0.65mm_EP1x1.6mm")?.message).toContain("Pad '7' missing layer 'Paste'");
		// S4.5 is waived for this one part in .omp/kicad.json: info with the reason; the same rule
		// on any other part stays an error
		const s45 = by("klc|S4.5|Connector_HanRun:HR871150C");
		expect(s45?.severity).toBe("info");
		expect(s45?.message).toContain("Pin 6");
		expect(s45?.message).toContain("[waived: the part has no pin 6");
		project.settings.klcWaivers = { "Connector_HanRun:Other": { "S4.5": "elsewhere" } };
		const scoped = await klcFindings(project, [path.join(root, "hardware/lib/Connector_HanRun.kicad_sym")]);
		expect(scoped.find(f => f.key === "klc|S4.5|Connector_HanRun:HR871150C")?.severity).toBe("error");
		// stock footprints resolve through --footprints: no false S5.1
		expect(findings.filter(f => f.key.startsWith("klc|S5.1|"))).toEqual([]);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("check: only parts changed since the baseline (untracked libraries too), or all of them, naming the parts checked", async () => {
		const git = (...args: string[]) => Bun.spawnSync(["git", "-C", root, ...args], { stdout: "pipe", stderr: "pipe" });
		git("init", "-q");
		git("add", "-A");
		expect(git("-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-qm", "base").exitCode).toBe(0);
		const project = Project.open(PROJECT, root);
		const run = (args: Record<string, unknown>) => checks[0]!.run({ project: Project.open(PROJECT, root), analysis: new Analysis(project), baseline: "HEAD", args });
		const checked = (f: { key: string; message: string }[]) => f.find(x => x.key === "klc|checked")?.message;

		const none = await run({});
		expect(none.map(f => [f.severity, f.key])).toEqual([["info", "klc|nothing"]]);
		expect(none[0]!.message).toContain("nothing checked");

		await commit(root, [
			{ op: "lib_edit_footprint", library: "Module_Waveshare", name: "Waveshare_ESP32-P4-Core", tags: "Waveshare ESP32-P4 module edited" },
			{ op: "lib_edit_symbol", library: "Connector_HanRun", name: "HR871150C", properties: { Description: "edited" } },
		]);
		const changed = await run({});
		expect(checked(changed)).toBe("checked 2 parts changed since HEAD: Connector_HanRun:HR871150C, Module_Waveshare:Waveshare_ESP32-P4-Core");
		const violations = changed.filter(f => f.key !== "klc|checked");
		expect(new Set(violations.map(f => f.key.split("|").slice(2).join("|")))).toEqual(
			new Set(["Module_Waveshare:Waveshare_ESP32-P4-Core", "Connector_HanRun:HR871150C"]),
		);

		// a new, untracked library registered in the project's table is checked as changed
		const lib = path.join(root, "hardware/lib");
		fs.copyFileSync(path.join(lib, "Connector_HanRun.kicad_sym"), path.join(lib, "Connector_Test.kicad_sym"));
		const table = path.join(root, PROJECT, "sym-lib-table");
		fs.writeFileSync(
			table,
			fs.readFileSync(table, "utf8").replace("(version 7)", '(version 7)\n\t(lib (name "Connector_Test") (type "KiCad") (uri "${KIPRJMOD}/../lib/Connector_Test.kicad_sym") (options "") (descr ""))'),
		);
		expect(checked(await run({}))).toContain("Connector_Test:HR871150C");

		const all = await run({ all: true });
		expect(checked(all)).toMatch(/^checked \d+ parts \(all project library parts\): /);
		expect(all.length).toBeGreaterThan(changed.length);
	}, 120_000);
});

describe("library queries", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	test.skipIf(!HAS_KICAD)("lib_symbol: the U1 module's pins", async () => {
		const r = await query(root, "lib_symbol", { lib_id: "MCU_Module_Waveshare:ESP32-P4-Core" });
		const pins = r.pins as { number: string; name: string; type: string }[];
		expect(pins.length).toBeGreaterThan(70);
		expect(pins.some(p => p.name === "GPIO35/BOOT")).toBe(true);
	});

	test.skipIf(!HAS_KICAD)("lib_footprint: a stock footprint's pads", async () => {
		const r = await query(root, "lib_footprint", { fp_id: "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm" });
		const pads = r.pads as { number: string; at: number[]; size: number[] }[];
		expect(pads.map(p => p.number)).toEqual(["1", "2", "3", "4", "5", "6", "7", "8"]);
		expect(pads[0]!.at[0]).toBeLessThan(0);
		expect(r.attributes).toContain("smd");
		expect((r.models as unknown[]).length).toBe(1);
	});

	test.skipIf(!HAS_KICAD)("lib_search: by name and description, project and stock", async () => {
		const fp = await query(root, "lib_search", { query: "SOIC-8_3.9x4.9mm_P1.27mm", kind: "footprint" });
		expect((fp.parts as { id: string }[])[0]!.id).toBe("Package_SO:SOIC-8_3.9x4.9mm_P1.27mm");
		const sym = await query(root, "lib_search", { query: "esp32-p4", kind: "symbol" });
		expect((sym.parts as { id: string }[]).map(p => p.id)).toContain("MCU_Module_Waveshare:ESP32-P4-Core");
		const phy = await query(root, "lib_search", { query: "ethernet phy", library: "Interface_Ethernet*" });
		expect((phy.parts as { id: string }[]).map(p => p.id)).toContain("Interface_Ethernet_ICPlus:IP101GRI");
	}, 60_000);
});
