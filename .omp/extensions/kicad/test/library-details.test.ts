/**
 * Library descriptions in the lib tables, Reference/Value placement after a drawing change,
 * where library parts come from (project, stock, user), legacy pin stacks and footprint pad
 * details (corner radius, mask and paste margins).
 */
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import { describeFootprint, type FpPadInfo } from "../src/libs";
import { ops } from "../src/ops/library";
import { Project } from "../src/project";
import { queries } from "../src/queries/library";
import { prop } from "../src/sch";
import { child, children, type List, num, parse, text } from "../src/sexpr";
import { validate } from "../src/spec";
import type { OpContext } from "../src/types";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

/** Apply library ops in memory; returns the pending writes by repo-relative path, and the notes. */
async function stage(root: string, list: Record<string, unknown>[]): Promise<{ files: Map<string, string>; notes: string[] }> {
	const project = Project.open(PROJECT, root);
	const ctx: OpContext = { project, libs: await project.libs(), notes: [], touch() {}, touchPart() {} };
	for (const o of list) {
		const def = ops.find(d => d.op === o.op)!;
		await def.apply(ctx, validate(String(o.op), o, def.fields, ["op"]));
	}
	const files = new Map<string, string>();
	for (const [f, content] of project.pendingWrites()) files.set(path.relative(root, f), content);
	return { files, notes: ctx.notes };
}

async function query(root: string, what: string, args: Record<string, unknown>): Promise<Record<string, unknown>> {
	const project = Project.open(PROJECT, root);
	const def = queries.find(q => q.what === what)!;
	return (await def.run({ project, analysis: new Analysis(project), libs: () => project.libs() }, validate(what, args, def.fields))) as Record<string, unknown>;
}

const SYM_TABLE = "hardware/espion-cam/sym-lib-table";

/** The descr of a library in a lib table's text. */
function descrOf(table: string, nick: string): string | undefined {
	const lib = children(parse(table), "lib").find(l => text(child(l, "name")) === nick);
	return lib ? text(child(lib, "descr")) : undefined;
}

const BOX = {
	op: "lib_draw_symbol",
	library: "Desc_Test",
	name: "BOX1",
	reference: "U",
	pins: [{ number: "1", name: "A", type: "passive", at: [-7.62, 0], dir: "L" }],
	shapes: [{ type: "rectangle", start: [-5.08, -5.08], end: [5.08, 5.08], width: 0.254, fill: "background" }],
};

/** Reference and Value positions of a symbol in a .kicad_sym text, in sheet orientation (y down). */
function fieldsAt(lib: string, name: string): Record<"Reference" | "Value", [number, number]> {
	const sym = children(parse(lib), "symbol").find(s => text(s) === name)!;
	const at = (key: string): [number, number] => {
		const a = child(prop(sym, key) as List, "at");
		return [num(a, 1), 0 - num(a, 2)];
	};
	return { Reference: at("Reference"), Value: at("Value") };
}

describe("library descriptions", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	test("a new library needs library_description; its table entry gets the project suffix, one line per library", async () => {
		await expect(stage(root, [BOX])).rejects.toThrow(/symbol library "Desc_Test" doesn't exist yet: give library_description.*"<library_description> \(espion project library\)"/);
		const { files, notes } = await stage(root, [{ ...BOX, library_description: "Test descriptions" }]);
		const table = files.get(SYM_TABLE)!;
		expect(descrOf(table, "Desc_Test")).toBe("Test descriptions (espion project library)");
		expect(notes.join("\n")).toContain('description "Test descriptions (espion project library)"');
		const lines = table.trimEnd().split("\n");
		expect(lines[0]).toBe("(sym_lib_table");
		expect(lines.at(-1)).toBe(")");
		expect(lines.slice(1, -1).every(l => /^\t\((version|lib) /.test(l))).toBe(true);
		expect(lines.filter(l => l.startsWith("\t(lib ")).length).toBe(children(parse(table), "lib").length);
		// the suffix is not doubled when the description already ends with it
		const again = await stage(root, [{ ...BOX, library_description: "Test descriptions (espion project library)" }]);
		expect(descrOf(again.files.get(SYM_TABLE)!, "Desc_Test")).toBe("Test descriptions (espion project library)");
	});

	test("lib_set_library describes an existing project library; stock and non-project libraries are refused", async () => {
		const { files, notes } = await stage(root, [{ op: "lib_set_library", kind: "symbol", library: "Connector_HanRun", description: "HanRun RJ45 MagJacks" }]);
		const table = files.get(SYM_TABLE)!;
		expect(descrOf(table, "Connector_HanRun")).toBe("HanRun RJ45 MagJacks (espion project library)");
		expect(descrOf(table, "MCU_Module_Waveshare")).toBe(descrOf(fs.readFileSync(path.join(root, SYM_TABLE), "utf8"), "MCU_Module_Waveshare"));
		expect(notes.join("\n")).toContain('-> "HanRun RJ45 MagJacks (espion project library)"');
		await expect(stage(root, [{ op: "lib_set_library", kind: "symbol", library: "Device", description: "x" }])).rejects.toThrow(/"Device" is a stock KiCad symbol library/);
		await expect(stage(root, [{ op: "lib_set_library", kind: "footprint", library: "Connector_HanRun", description: "x" }])).rejects.toThrow(/not a footprint library of the project/);
		// a library created earlier in the same transaction can be described too
		const both = await stage(root, [
			{ ...BOX, library_description: "First" },
			{ op: "lib_set_library", kind: "symbol", library: "Desc_Test", description: "Second" },
		]);
		expect(descrOf(both.files.get(SYM_TABLE)!, "Desc_Test")).toBe("Second (espion project library)");
	});
});

describe("lib_edit_symbol field placement", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	const lib = "hardware/lib/Desc_Test.kicad_sym";
	const bigger = [{ type: "rectangle", start: [-10.16, -7.62], end: [10.16, 7.62], width: 0.254, fill: "background" }];

	test("new shapes or moved pins re-place Reference above and Value below the drawing, as lib_draw_symbol does", async () => {
		const drawn = await stage(root, [{ ...BOX, library_description: "Test" }]);
		// pin end at x -7.62, body from -5.08 to 5.08: Reference 1.27 above, Value 1.27 below, at the left edge
		expect(fieldsAt(drawn.files.get(lib)!, "BOX1")).toEqual({ Reference: [-7.62, -6.35], Value: [-7.62, 6.35] });

		const edit = { op: "lib_edit_symbol", library: "Desc_Test", name: "BOX1" };
		const reshaped = await stage(root, [{ ...BOX, library_description: "Test" }, { ...edit, shapes: bigger }]);
		expect(fieldsAt(reshaped.files.get(lib)!, "BOX1")).toEqual({ Reference: [-10.16, -8.89], Value: [-10.16, 8.89] });
		expect(reshaped.notes.join("\n")).toContain("Reference and Value re-placed");

		const moved = await stage(root, [{ ...BOX, library_description: "Test" }, { ...edit, pins: [{ number: "1", at: [-7.62, 10.16] }] }]);
		expect(fieldsAt(moved.files.get(lib)!, "BOX1")).toEqual({ Reference: [-7.62, -6.35], Value: [-7.62, 11.43] });

		const kept = await stage(root, [{ ...BOX, library_description: "Test" }, { ...edit, shapes: bigger, fields: "keep" }]);
		expect(fieldsAt(kept.files.get(lib)!, "BOX1")).toEqual({ Reference: [-7.62, -6.35], Value: [-7.62, 6.35] });

		// a property or pin name change leaves them, unless fields: "auto" asks
		const renamed = await stage(root, [{ ...BOX, library_description: "Test" }, { ...edit, pins: [{ number: "1", name: "B" }], properties: { MPN: "X" } }]);
		expect(renamed.notes.join("\n")).not.toContain("re-placed");
		expect(fieldsAt(renamed.files.get(lib)!, "BOX1")).toEqual({ Reference: [-7.62, -6.35], Value: [-7.62, 6.35] });
	});
});

describe("library part details", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	test.skipIf(!HAS_KICAD)("lib_symbol, lib_footprint and lib_search say where a part comes from", async () => {
		expect((await query(root, "lib_symbol", { lib_id: "MCU_Module_Waveshare:ESP32-P4-Core" })).source).toBe("project");
		expect((await query(root, "lib_symbol", { lib_id: "Device:R" })).source).toBe("stock");
		expect((await query(root, "lib_footprint", { fp_id: "Module_Waveshare:Waveshare_ESP32-P4-Core" })).source).toBe("project");
		expect((await query(root, "lib_footprint", { fp_id: "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm" })).source).toBe("stock");
		const hits = (await query(root, "lib_search", { query: "esp32-p4" })).parts as { id: string; source: string }[];
		expect(hits.find(h => h.id === "MCU_Module_Waveshare:ESP32-P4-Core")?.source).toBe("project");
		const stock = (await query(root, "lib_search", { query: "SOIC-8_3.9x4.9mm_P1.27mm", kind: "footprint" })).parts as { id: string; source: string }[];
		expect(stock[0]).toMatchObject({ id: "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm", source: "stock" });
	}, 60_000);

	test.skipIf(!HAS_KICAD)("any other global library is the user's", async () => {
		const cfg = fs.mkdtempSync(path.join(root, "xdg-"));
		fs.mkdirSync(path.join(cfg, "kicad", "10.0"), { recursive: true });
		const user = path.join(cfg, "User_Test.kicad_sym");
		fs.copyFileSync(path.join(root, "hardware/lib/Connector_HanRun.kicad_sym"), user);
		fs.writeFileSync(
			path.join(cfg, "kicad", "10.0", "sym-lib-table"),
			`(sym_lib_table\n\t(version 7)\n\t(lib (name "Device") (type "KiCad") (uri "\${KICAD10_SYMBOL_DIR}/Device.kicad_sym") (options "") (descr ""))\n\t(lib (name "User_Test") (type "KiCad") (uri "${user}") (options "") (descr ""))\n)\n`,
		);
		const saved = process.env.XDG_CONFIG_HOME;
		process.env.XDG_CONFIG_HOME = cfg;
		try {
			const libs = await Project.open(PROJECT, root).libs();
			expect(libs.symLib("User_Test").source).toBe("user");
			expect(libs.symLib("Device").source).toBe("stock");
			expect(libs.symLib("Connector_HanRun").source).toBe("project");
			const hits = (await query(root, "lib_search", { query: "HR871150C", kind: "symbol", library: "User_Test" })).parts as { source: string }[];
			expect(hits.map(h => h.source)).toEqual(["user"]);
			const { notes } = await stage(root, [{ op: "lib_copy_symbol", from: "User_Test:HR871150C", library: "Copy_Test", library_description: "Test", name: "HR871150C_Copy" }]);
			expect(notes.join("\n")).toContain("User_Test is the user's own global library, neither in the repo nor KLC-reviewed");
		} finally {
			if (saved === undefined) delete process.env.XDG_CONFIG_HOME;
			else process.env.XDG_CONFIG_HOME = saved;
		}
	}, 60_000);

	test.skipIf(!HAS_KICAD)("lib_symbol flags legacy pin stacks (stock TPS2116DRL: pin 7 hidden under pin 2)", async () => {
		const r = await query(root, "lib_symbol", { lib_id: "Power_Management:TPS2116DRL" });
		expect(r.legacy_stacks).toEqual([{ unit: 1, body_style: 1, at: [10.16, -5.08], visible: "2", hidden: ["7"], native: "[2,7]" }]);
		expect(r.note).toContain("KLC S4.3");
		const g = await query(root, "lib_symbol", { lib_id: "Power_Management:TPS2116DRL", graphics: true });
		expect(g.legacy_stacks).toEqual(r.legacy_stacks);
		expect((await query(root, "lib_symbol", { lib_id: "Device:R" })).legacy_stacks).toBeUndefined();
	}, 60_000);

	test.skipIf(!HAS_KICAD)("lib_footprint reports a roundrect pad's corner radius", async () => {
		const r = await query(root, "lib_footprint", { fp_id: "Package_DFN_QFN:Texas_VQFN-HR-12_2x2.5mm_P0.5mm" });
		const pads = r.pads as FpPadInfo[];
		const p1 = pads.find(p => p.number === "1")!;
		expect(p1.shape).toBe("roundrect");
		expect(p1.roundrect_rratio).toBe(0.25);
		expect(p1.corner_radius).toBeCloseTo(0.25 * Math.min(...p1.size!), 6);
		expect(p1.solder_mask_margin).toBeUndefined();
		expect(r.overrides).toBeUndefined();
	}, 60_000);

	test("pads take corner_radius, chamfers and mask/paste overrides as lib_footprint reports them", async () => {
		const base = {
			op: "lib_create_footprint",
			library: "Pad_Test",
			library_description: "Test pads",
			name: "Pads",
			description: "Pad settings test (https://example.com/pads.pdf)",
			tags: "test",
			attr: "smd",
			body: { width: 2, height: 2 },
		};
		const pad = { number: "1", type: "smd", shape: "roundrect", at: [0, 0], size: [0.25, 0.6] };
		const { files } = await stage(root, [
			{
				...base,
				pads: [
					{ ...pad, corner_radius: 0.05, solder_mask_margin: 0.05, solder_paste_margin: -0.02, clearance: 0.1, zone_connect: 2 },
					{ ...pad, number: "2", at: [0.5, 0], chamfer_ratio: 0.2, chamfer: ["top_left"] },
				],
			},
		]);
		const fp = describeFootprint(parse(files.get("hardware/lib/Pad_Test.pretty/Pads.kicad_mod")!));
		expect(fp.pads[0]).toMatchObject({ roundrect_rratio: 0.2, corner_radius: 0.05, solder_mask_margin: 0.05, solder_paste_margin: -0.02, clearance: 0.1, zone_connect: 2 });
		expect(fp.pads[1]).toMatchObject({ roundrect_rratio: 0.25, chamfer_ratio: 0.2, chamfer: ["top_left"] });
		await expect(stage(root, [{ ...base, pads: [{ ...pad, corner_radius: 0.05, roundrect_rratio: 0.25 }] }])).rejects.toThrow(/roundrect_rratio 0.25 makes a 0.0625 mm corner radius/);
		await expect(stage(root, [{ ...base, pads: [{ ...pad, shape: "rect", corner_radius: 0.05 }] }])).rejects.toThrow(/corner_radius is for roundrect pads/);

		const edited = await stage(root, [
			{ ...base, pads: [{ ...pad, solder_mask_margin: 0.05 }] },
			{ op: "lib_edit_footprint", library: "Pad_Test", name: "Pads", pads: [{ pad: "1", size: [0.3, 0.6], corner_radius: 0.06, solder_mask_margin: null }] },
		]);
		const e = describeFootprint(parse(edited.files.get("hardware/lib/Pad_Test.pretty/Pads.kicad_mod")!)).pads[0]!;
		expect(e.corner_radius).toBeCloseTo(0.06, 6);
		expect(e.solder_mask_margin).toBeUndefined();
	});
});
