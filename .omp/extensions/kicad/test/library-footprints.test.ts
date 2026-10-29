import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import { klcFindings } from "../src/checks/klc";
import * as cli from "../src/cli";
import type { FpGraphics } from "../src/fpgfx";
import { describeFootprint, type FpInfo, type FpPadInfo } from "../src/libs";
import { ops } from "../src/ops/library";
import { Project } from "../src/project";
import { queries } from "../src/queries/library";
import { child, children, parse, text } from "../src/sexpr";
import { validate } from "../src/spec";
import type { OpContext } from "../src/types";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

/** Apply library ops in memory; returns the project with its pending writes and the notes. */
async function stage(root: string, list: Record<string, unknown>[]): Promise<{ project: Project; notes: string[]; touched: string[] }> {
	const project = Project.open(PROJECT, root);
	const touched: string[] = [];
	const ctx: OpContext = { project, libs: await project.libs(), notes: [], touch() {}, touchPart: (kind, id) => touched.push(`${kind}:${id}`) };
	for (const o of list) {
		const def = ops.find(d => d.op === o.op)!;
		await def.apply(ctx, validate(String(o.op), o, def.fields, ["op"]));
	}
	return { project, notes: ctx.notes, touched };
}

/** Stage the ops, write the files into the fixture and let KiCad re-save the footprint libraries, as a transaction does. */
async function commit(root: string, list: Record<string, unknown>[]) {
	const staged = await stage(root, list);
	const writes = staged.project.pendingWrites();
	for (const [f, content] of writes) {
		fs.mkdirSync(path.dirname(f), { recursive: true });
		fs.writeFileSync(f, content);
	}
	for (const dir of new Set([...writes.keys()].filter(f => f.endsWith(".kicad_mod")).map(f => path.dirname(f)))) {
		await cli.upgrade("fp", dir, { cwd: staged.project.dir, repoRoot: root });
	}
	return { ...staged, writes };
}

async function lib_footprint(root: string, fp_id: string, graphics = false) {
	const project = Project.open(PROJECT, root);
	const def = queries.find(q => q.what === "lib_footprint")!;
	const args = validate("lib_footprint", { fp_id, graphics }, def.fields);
	return (await def.run({ project, analysis: new Analysis(project), libs: () => project.libs() }, args)) as FpInfo & { file: string };
}

/** Graphics with each layer's shapes in a canonical order (KiCad sorts drawings when it saves). */
function sorted(g: FpGraphics | undefined): Record<string, string[]> {
	return Object.fromEntries(
		Object.entries(g ?? {})
			.sort(([a], [b]) => a.localeCompare(b))
			.map(([layer, shapes]) => [layer, shapes.map(s => JSON.stringify(s)).sort()]),
	);
}

function padKeys(pads: FpPadInfo[]): string[] {
	return pads.map(p => JSON.stringify([p.number, p.type, p.shape, p.at, p.size, p.drill, p.layers])).sort();
}

const STOCK_RJ45 = "Connector_RJ:RJ45_Abracon_ARJP11A-MA_Horizontal";
const COPY = { op: "lib_copy_footprint", from: STOCK_RJ45, library: "Connector_RJ_Test", name: "RJ45_HanRun_HR871150C_Horizontal" };
const ID = "Connector_RJ_Test:RJ45_HanRun_HR871150C_Horizontal";

/** The HR871150C land pattern derived from the Abracon ARJP11A, as hardware/lib's Connector_RJ_HanRun has it. */
const DERIVE = {
	op: "lib_edit_footprint",
	library: "Connector_RJ_Test",
	name: "RJ45_HanRun_HR871150C_Horizontal",
	description: "HanRun HR871150C RJ45 MagJack, shielded, THT right angle, https://datasheet.lcsc.com/datasheet/pdf/bae28869af3176e6a10bdf4859b444a5.pdf",
	pads: [
		{ pad: "6", number: "8", at: [8.89, 2.54], drill: 0.89 },
		...["1", "2", "3", "4", "5", "7", "9", "10"].map(n => ({ pad: n, drill: 0.89 })),
		{ pad: { number: "", near: [0, 8.89] }, size: [3.25, 3.25], drill: 3.25 },
		{ pad: { number: "", near: [11.43, 8.89] }, size: [3.25, 3.25], drill: 3.25 },
		{ pad: { number: "SH", near: [-2, 6] }, at: [-2.03, 5.84], drill: 1.63 },
		{ pad: { number: "SH", near: [13, 6] }, drill: 1.63 },
		{ pad: "11", at: [-0.91, 12.97], drill: 1.02 },
		{ pad: "12", at: [1.63, 12.97], drill: 1.02 },
		{ pad: "13", at: [9.8, 12.97], drill: 1.02 },
		{ pad: "14", at: [12.34, 12.97], drill: 1.02 },
	],
	outline: { body: { width: 16, height: 21.2, center: [5.715, 9.09] }, courtyard_margin: 0.5 },
};

describe("library footprints", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	test.skipIf(!HAS_KICAD)("HR871150C: copy the stock Abracon footprint, rework pads and outline; KiCad re-saves it unchanged, KLC as expected", async () => {
		const { notes, touched, writes } = await commit(root, [COPY, DERIVE]);
		expect(notes.join("\n")).toContain(`copied footprint ${STOCK_RJ45} to ${ID}; it derives from the KiCad footprint library, CC-BY-SA 4.0 material`);
		expect(touched).toEqual([`footprint:${ID}`, `footprint:${ID}`]);

		const r = await lib_footprint(root, ID, true);
		expect(r.properties.Value).toBe("RJ45_HanRun_HR871150C_Horizontal");
		const pad = (n: string) => r.pads.filter(p => p.number === n).map(p => ({ at: p.at, drill: p.drill, size: p.size }));
		expect(pad("6")).toEqual([]);
		expect(pad("8")).toEqual([{ at: [8.89, 2.54], drill: 0.89, size: [1.8, 1.8] }]);
		expect(pad("1")).toEqual([{ at: [0, 0], drill: 0.89, size: [1.8, 1.8] }]);
		expect(pad("")).toEqual([
			{ at: [0, 8.89], drill: 3.25, size: [3.25, 3.25] },
			{ at: [11.43, 8.89], drill: 3.25, size: [3.25, 3.25] },
		]);
		expect(pad("SH").map(p => [p.at, p.drill])).toEqual([
			[[-2.03, 5.84], 1.63],
			[[13.46, 5.84], 1.63],
		]);
		expect(["11", "12", "13", "14"].map(n => pad(n)[0])).toEqual(
			[-0.91, 1.63, 9.8, 12.34].map(x => ({ at: [x, 12.97], drill: 1.02, size: [2, 2] })),
		);
		expect(r.pads.find(p => p.number === "8")?.layers).toEqual(["*.Cu", "*.Mask"]);

		// the outline is lib_create_footprint's: Fab body 16 x 21.2 with a pin-1 chamfer, courtyard 0.5 mm around pads and body
		const fab = r.graphics?.["F.Fab"]?.filter(s => s.type === "poly") ?? [];
		expect(fab.length).toBe(1);
		const fx = fab[0]!.type === "poly" ? fab[0]!.points.map(p => p[0]) : [];
		expect(Math.min(...fx)).toBeCloseTo(-2.285, 6);
		expect(Math.max(...fx)).toBeCloseTo(13.715, 6);
		const crtyd = r.graphics?.["F.CrtYd"] ?? [];
		expect(crtyd.length).toBe(4);
		const cys = crtyd.flatMap(s => (s.type === "line" ? [s.start[1], s.end[1]] : []));
		expect(Math.min(...cys)).toBeCloseTo(-2.01, 6);
		expect(Math.max(...cys)).toBeCloseTo(20.19, 6);
		expect(r.graphics?.["F.SilkS"]?.length).toBeGreaterThan(4);

		// KiCad's re-save keeps every pad and drawing as written, and a second re-save changes nothing
		const file = path.join(root, r.file);
		const written = parse(writes.get(file)!);
		const saved = parse(fs.readFileSync(file, "utf8"));
		expect(padKeys(describeFootprint(saved).pads)).toEqual(padKeys(describeFootprint(written).pads));
		expect(sorted(describeFootprint(saved, true).graphics)).toEqual(sorted(describeFootprint(written, true).graphics));
		const bytes = fs.readFileSync(file, "utf8");
		await cli.upgrade("fp", path.dirname(file), { cwd: path.join(root, PROJECT), repoRoot: root });
		expect(fs.readFileSync(file, "utf8")).toBe(bytes);

		const findings = await klcFindings(Project.open(PROJECT, root), [file]);
		expect(findings.map(f => f.key)).toEqual([`klc|F9.3|${ID}`]);
	}, 120_000);

	test.skipIf(!HAS_KICAD)("pad selectors: ambiguous, unknown, equally near and repeated selectors are errors; near picks one", async () => {
		const edit = (pads: unknown[]) => stage(root, [{ ...COPY, name: "Sel" }, { op: "lib_edit_footprint", library: "Connector_RJ_Test", name: "Sel", pads }]);
		await expect(edit([{ pad: "SH", drill: 1.63 }])).rejects.toThrow(/pads\[0\]: 2 pads are numbered "SH" \(\(at -2.04 5.84\), \(at 13.46 5.84\)\); select one with \{number: "SH", near/);
		await expect(edit([{ pad: "", drill: 3 }])).rejects.toThrow(/2 pads are numbered ""/);
		await expect(edit([{ pad: "99", drill: 1 }])).rejects.toThrow(/no pad "99"; pads: "", "1", .*"SH"/);
		await expect(edit([{ pad: { number: "", near: [5.715, 8.89] }, drill: 3 }])).rejects.toThrow(/equally near/);
		await expect(edit([{ pad: { number: "SH" }, drill: 1 }])).rejects.toThrow(/near must be \[x, y\]/);
		await expect(edit([{ pad: "1", drill: 1 }, { pad: 1, size: [2, 2] }])).rejects.toThrow(/pads\[1\] selects the same pad as pads\[0\]/);
		await expect(edit([{ pad: "1", remove: true, drill: 1 }])).rejects.toThrow(/takes no other change/);
		await expect(edit([{ pad: "1", type: "smd", drill: 1 }])).rejects.toThrow(/a smd pad has no drill/);

		const { project } = await edit([
			{ pad: { number: "SH", near: [13, 6] }, at: [13.5, 6, 90], size: [3, 2] },
			{ pad: { number: "", near: [0, 9] }, remove: true },
			{ pad: "1", shape: "roundrect" },
			{ pad: "2", type: "smd" },
		]);
		const [, content] = [...project.pendingWrites()].find(([f]) => f.endsWith("Sel.kicad_mod"))!;
		const pads = describeFootprint(parse(content)).pads;
		expect(pads.filter(p => p.number === "SH").map(p => p.at)).toEqual([
			[-2.04, 5.84],
			[13.5, 6, 90],
		]);
		expect(pads.filter(p => p.number === "").map(p => p.at)).toEqual([[11.43, 8.89]]);
		const tree = parse(content);
		const node = (n: string) => children(tree, "pad").find(p => text(p, 1) === n)!;
		expect(text(child(node("1"), "roundrect_rratio"))).toBe("0.25");
		expect(pads.find(p => p.number === "2")).toMatchObject({ type: "smd", layers: ["F.Cu", "F.Mask", "F.Paste"] });
		expect(pads.find(p => p.number === "2")?.drill).toBeUndefined();
	}, 60_000);

	test.skipIf(!HAS_KICAD)("graphics round trip: a stock footprint's query output fed back reproduces its drawings", async () => {
		const STOCK = "Module:RaspberryPi_Pico_Common_THT";
		const stock = await lib_footprint(root, STOCK, true);
		const layers = Object.keys(stock.graphics ?? {});
		expect(layers).toEqual(expect.arrayContaining(["F.SilkS", "F.Fab", "F.CrtYd", "Dwgs.User", "Cmts.User"]));
		const types = new Set(Object.values(stock.graphics ?? {}).flatMap(s => s.map(x => x.type)));
		expect([...types].sort()).toEqual(["arc", "circle", "line", "poly", "rect", "text"]);

		const target = { library: "Module_Test", name: "Pico_Redrawn" };
		await commit(root, [
			{ op: "lib_copy_footprint", from: STOCK, ...target },
			{ op: "lib_edit_footprint", ...target, graphics: Object.fromEntries(layers.map(l => [l, []])) },
		]);
		expect((await lib_footprint(root, "Module_Test:Pico_Redrawn", true)).graphics).toEqual({});

		await commit(root, [{ op: "lib_edit_footprint", ...target, graphics: stock.graphics }]);
		const again = await lib_footprint(root, "Module_Test:Pico_Redrawn", true);
		expect(sorted(again.graphics)).toEqual(sorted(stock.graphics));
		expect(again.pads).toEqual(stock.pads);
	}, 120_000);

	test.skipIf(!HAS_KICAD)("lib_create_footprint graphics replace the generated drawings of their layers only; bad shapes are refused", async () => {
		const base = {
			op: "lib_create_footprint",
			library: "Module_Test",
			name: "Gfx_Test",
			description: "Graphics test (https://example.com/gfx.pdf)",
			tags: "test",
			attr: "smd",
			body: { width: 4, height: 3 },
			pads: [
				{ number: 1, type: "smd", shape: "rect", at: [-1.5, 0], size: [1, 1] },
				{ number: 2, type: "smd", shape: "rect", at: [1.5, 0], size: [1, 1] },
			],
		};
		const { project, touched } = await stage(root, [
			{ ...base, graphics: { "F.SilkS": [{ type: "circle", center: [0, -2.5], radius: 0.3, fill: true }], "B.SilkS": [{ type: "text", text: "X", at: [0, 0] }] } },
		]);
		expect(touched).toEqual(["footprint:Module_Test:Gfx_Test"]);
		const [, content] = [...project.pendingWrites()].find(([f]) => f.endsWith("Gfx_Test.kicad_mod"))!;
		const g = describeFootprint(parse(content), true).graphics!;
		expect(g["F.SilkS"]).toEqual([{ type: "circle", center: [0, -2.5], radius: 0.3, width: 0.12, fill: true }]);
		expect(g["B.SilkS"]).toEqual([{ type: "text", text: "X", at: [0, 0], size: 1, thickness: 0.15 }]);
		expect(content).toContain("(justify mirror)");
		expect(g["F.Fab"]?.length).toBe(2);
		expect(g["F.CrtYd"]?.length).toBe(4);

		await expect(stage(root, [{ ...base, graphics: { "F.Silk": [] } }])).rejects.toThrow(/"F.Silk" is not a KiCad layer name/);
		await expect(stage(root, [{ ...base, graphics: { "F.SilkS": [{ type: "line", start: [0, 0] }] } }])).rejects.toThrow(/graphics.F.SilkS\[0\]: missing required argument "end"/);
		await expect(stage(root, [{ ...base, graphics: { "F.SilkS": [{ type: "spline" }] } }])).rejects.toThrow(/type must be one of/);
	}, 60_000);
});
