import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import * as cli from "../src/cli";
import type { FpInfo } from "../src/libs";
import { ops } from "../src/ops/library";
import { Project } from "../src/project";
import { QUERIES } from "../src/registry";
import { validate } from "../src/spec";
import type { OpContext } from "../src/types";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

/** Apply library ops, write the files into the fixture and let KiCad re-save them, as a transaction does. */
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
	const ro = { cwd: project.dir, repoRoot: root };
	for (const f of writes.keys()) if (f.endsWith(".kicad_sym")) await cli.upgrade("sym", f, ro);
	for (const dir of new Set([...writes.keys()].filter(f => f.endsWith(".kicad_mod")).map(f => path.dirname(f)))) await cli.upgrade("fp", dir, ro);
	return ctx.notes;
}

async function query(root: string, what: string, args: Record<string, unknown>): Promise<Record<string, unknown>> {
	const project = Project.open(PROJECT, root);
	const def = QUERIES.get(what)!;
	return (await def.run({ project, analysis: new Analysis(project), libs: () => project.libs() }, validate(what, args, def.fields))) as Record<string, unknown>;
}

const footprint = async (root: string, fp_id: string) => (await query(root, "lib_footprint", { fp_id, graphics: true })) as unknown as FpInfo;

const MODULE = {
	op: "lib_create_footprint",
	library_description: "Test modules",
	library: "Module_Test",
	name: "Placed_Module",
	description: "Test module with a placed 3D model (https://example.com/module.pdf)",
	tags: "module test",
	attr: "smd",
	body: { width: 6, height: 4 },
	pads: [
		{ number: 1, type: "smd", shape: "rect", at: [-2.5, 0], size: [1, 1] },
		{ number: 2, type: "smd", shape: "rect", at: [2.5, 0], size: [1, 1] },
	],
	model: true,
	model_offset: [0.125, -0.0625, 1.5],
	model_rotation: [-90, 0, 180],
	model_scale: [1, 1, 2],
};

describe("3D model placement", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	test.skipIf(!HAS_KICAD)("lib_footprint reports a stock model's placement", async () => {
		const r = await footprint(root, "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm");
		expect(r.models).toEqual([{ path: expect.stringContaining("SOIC-8_3.9x4.9mm_P1.27mm"), offset: [0, 0, 0], rotation: [0, 0, 0], scale: [1, 1, 1] }]);
	});

	test.skipIf(!HAS_KICAD)("create and edit set offset, rotation and scale; KiCad keeps them; a new path keeps the placement", async () => {
		const notes = await commit(root, [MODULE]);
		expect(notes.join("\n")).toContain("model_offset [0.125, -0.0625, 1.5], model_rotation [-90, 0, 180], model_scale [1, 1, 2]");
		const id = "Module_Test:Placed_Module";
		expect((await footprint(root, id)).models).toEqual([
			{ path: "${KIPRJMOD}/../lib/Module_Test.3dshapes/Placed_Module.step", offset: [0.125, -0.0625, 1.5], rotation: [-90, 0, 180], scale: [1, 1, 2] },
		]);

		await commit(root, [{ op: "lib_edit_footprint", library: "Module_Test", name: "Placed_Module", model: "${KICAD10_3DMODEL_DIR}/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.step", model_offset: [0, 0, 0.2] }]);
		expect((await footprint(root, id)).models).toEqual([
			{ path: "${KICAD10_3DMODEL_DIR}/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.step", offset: [0, 0, 0.2], rotation: [-90, 0, 180], scale: [1, 1, 2] },
		]);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("placement needs exactly one model and [x, y, z] values", async () => {
		const edit = (args: Record<string, unknown>) => commit(root, [{ op: "lib_edit_footprint", library: "Module_Test", name: "Placed_Module", ...args }]);
		await expect(edit({ model_rotation: [0, 90] })).rejects.toThrow(/model_rotation must be \[x, y, z\]/);
		await expect(edit({ model_scale: [1, 0, 1] })).rejects.toThrow(/model_scale must not be 0/);
		await expect(commit(root, [{ ...MODULE, name: "No_Model", model: false }])).rejects.toThrow(/no 3D model to place/);

		// a second model (KiCad allows several) makes the target ambiguous
		const file = path.join(root, "hardware/lib/Module_Test.pretty/Placed_Module.kicad_mod");
		const src = fs.readFileSync(file, "utf8");
		fs.writeFileSync(file, src.replace(/\n\)\s*$/, '\n\t(model "${KIPRJMOD}/other.step")\n)\n'));
		await expect(edit({ model_offset: [0, 0, 1] })).rejects.toThrow(/has 2 3D models .*need exactly one/);
		fs.writeFileSync(file, src);
	}, 60_000);
});

const STOCK_RJ45 = "Connector_RJ:RJ45_Abracon_ARJP11A-MA_Horizontal";
const RJ45 = "Connector_RJ_Test:RJ45_Derived";

describe("lib_diff", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	test.skipIf(!HAS_KICAD)("a footprint derived through the ops differs from its source by exactly the edits", async () => {
		const line = { type: "line", start: [0, -3], end: [11.43, -3], width: 0.1 };
		await commit(root, [
			{ op: "lib_copy_footprint", from: STOCK_RJ45, library: "Connector_RJ_Test", library_description: "Test RJ45 jacks", name: "RJ45_Derived" },
			{
				op: "lib_edit_footprint",
				library: "Connector_RJ_Test",
				name: "RJ45_Derived",
				description: "Derived RJ45, https://example.com/rj45.pdf",
				model_offset: [0, 0, 0.5],
				pads: [
					{ pad: "6", number: "8", at: [8.89, 2.54] },
					{ pad: "1", drill: 0.89 },
					{ pad: { number: "SH", near: [13, 6] }, drill: 1.63 },
				],
				graphics: { "Dwgs.User": [line] },
			},
		]);
		const stock = await footprint(root, STOCK_RJ45);
		const d = await query(root, "lib_diff", { a: STOCK_RJ45, b: RJ45 });
		const pad6 = stock.pads.find(p => p.number === "6")!;
		expect(d).toEqual({
			kind: "footprint",
			a: STOCK_RJ45,
			b: RJ45,
			same: false,
			properties: { Value: ["RJ45_Abracon_ARJP11A-MA_Horizontal", "RJ45_Derived"] },
			description: [stock.description, "Derived RJ45, https://example.com/rj45.pdf"],
			models: [stock.models, [{ ...stock.models[0]!, offset: [0, 0, 0.5] }]],
			pads: {
				added: [{ ...pad6, number: "8", at: [8.89, 2.54] }],
				removed: [pad6],
				changed: [
					{ number: "1", drill: [0.9, 0.89] },
					{ number: "SH", near: [13.46, 5.84], drill: [1.7, 1.63] },
				],
			},
			graphics: { "Dwgs.User": { added: [line] } },
		});
		expect(await query(root, "lib_diff", { a: RJ45, b: RJ45 })).toEqual({ kind: "footprint", a: RJ45, b: RJ45, same: true });
	}, 60_000);

	test.skipIf(!HAS_KICAD)("a symbol derived through the ops differs from its source by exactly the edits", async () => {
		const src = await query(root, "lib_symbol", { lib_id: "Device:R", graphics: true });
		const srcPins = src.pins as Record<string, unknown>[];
		const rect = { type: "rectangle", start: [-2, -2], end: [2, 2] };
		await commit(root, [
			{ op: "lib_copy_symbol", from: "Device:R", library: "Device_Test", library_description: "Test devices", name: "R_Derived" },
			{
				op: "lib_edit_symbol",
				library: "Device_Test",
				name: "R_Derived",
				properties: { Description: "Derived resistor" },
				pins: [
					{ number: "1", name: "A" },
					{ number: "2", remove: true },
					{ number: "3", at: [2.54, 0], dir: "R", type: "passive" },
				],
				shapes: [...(src.shapes as unknown[]), rect],
			},
		]);
		const d = await query(root, "lib_diff", { a: "Device:R", b: "Device_Test:R_Derived" });
		const props = src.properties as Record<string, string>;
		const pin = (n: string) => srcPins.find(p => p.number === n)!;
		expect(d).toMatchObject({
			kind: "symbol",
			same: false,
			properties: { Value: ["R", "R_Derived"], Description: [props.Description, "Derived resistor"] },
			pins: {
				removed: [expect.objectContaining({ number: "2", at: pin("2").at })],
				added: [{ number: "3", name: "", type: "passive", at: [2.54, 0], dir: "R", length: 2.54, shape: "line", hidden: false, unit: 1, body_style: 1 }],
				changed: [{ number: "1", name: [pin("1").name, "A"] }],
			},
			shapes: { added: [rect] },
		});
		expect(Object.keys(d).sort()).toEqual(["a", "b", "kind", "pins", "properties", "same", "shapes"]);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("mixed kinds and unknown parts are errors", async () => {
		await expect(query(root, "lib_diff", { a: "Device:R", b: STOCK_RJ45 })).rejects.toThrow(/is a symbol but .* is not/);
		await expect(query(root, "lib_diff", { a: "Device:No_Such_Part", b: "Device:R" })).rejects.toThrow(/neither a symbol nor a footprint/);
	});
});
