import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import { klcFindings } from "../src/checks/klc";
import * as cli from "../src/cli";
import { ops } from "../src/ops/library";
import { Project } from "../src/project";
import { queries } from "../src/queries/library";
import { inline, parse } from "../src/sexpr";
import { validate } from "../src/spec";
import type { OpContext } from "../src/types";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

const RJ45 = "Connector:RJ45_Abracon_ARJP11A-MASA-B-A-EMU2";

/** Apply library ops and write the files into the fixture as the ops wrote them (no KiCad re-save). */
async function apply(root: string, list: Record<string, unknown>[]): Promise<{ notes: string[]; parts: string[]; files: string[] }> {
	const project = Project.open(PROJECT, root);
	const parts: string[] = [];
	const ctx: OpContext = { project, libs: await project.libs(), notes: [], touch() {}, touchPart: (kind, id) => parts.push(`${kind}:${id}`) };
	for (const o of list) {
		const def = ops.find(d => d.op === o.op)!;
		await def.apply(ctx, validate(String(o.op), o, def.fields, ["op"]));
	}
	const writes = project.pendingWrites();
	for (const [f, content] of writes) {
		fs.mkdirSync(path.dirname(f), { recursive: true });
		fs.writeFileSync(f, content);
	}
	return { notes: ctx.notes, parts, files: [...writes.keys()].filter(f => f.endsWith(".kicad_sym")) };
}

/** Let KiCad re-save a symbol library; returns whether the s-expression changed. */
async function resaveChanges(root: string, file: string): Promise<{ before: string; after: string }> {
	const before = inline(parse(fs.readFileSync(file, "utf8")));
	await cli.upgrade("sym", file, { cwd: path.dirname(file), repoRoot: root });
	return { before, after: inline(parse(fs.readFileSync(file, "utf8"))) };
}

async function query(root: string, args: Record<string, unknown>, what = "lib_symbol"): Promise<Record<string, unknown>> {
	const project = Project.open(PROJECT, root);
	const def = queries.find(q => q.what === what)!;
	return (await def.run({ project, analysis: new Analysis(project), libs: () => project.libs() }, validate(what, args, def.fields))) as Record<string, unknown>;
}

type PinOut = { number: string; at: number[]; dir: string; unit: number; name: string };

describe("library symbols", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	test.skipIf(!HAS_KICAD)("lib_symbol graphics of a stock symbol redraws to the same symbol, and KiCad keeps the file as written", async () => {
		const stock = await query(root, { lib_id: RJ45, graphics: true });
		const plain = await query(root, { lib_id: RJ45 });
		expect(Object.keys((plain.pins as object[])[0]!)).toEqual(["number", "name", "type", "unit", "hidden", "at", "dir"]);
		expect(plain.shapes).toBeUndefined();
		const shapes = stock.shapes as Record<string, unknown>[];
		expect(shapes.filter(s => s.type === "arc").length).toBe(24);
		expect(shapes.find(s => s.type === "text" && s.text === "XMIT")).toEqual({ type: "text", text: "XMIT", at: [-10.16, -3.81], size: 1.016, justify: "left", body_style: 0 });
		expect((stock.pins as PinOut[]).find(p => p.number === "7")).toMatchObject({ at: [10.16, 12.7], dir: "D", length: 2.54, unit: 1 });

		const { files, parts } = await apply(root, [
			{
				op: "lib_draw_symbol",
				library: "Connector_Test",
				name: "RJ45_Redrawn",
				reference: "J",
				pins: stock.pins,
				shapes: stock.shapes,
				pin_name_offset: stock.pin_name_offset,
				hide_pin_names: stock.hide_pin_names,
				hide_pin_numbers: stock.hide_pin_numbers,
			},
		]);
		expect(parts).toEqual(["symbol:Connector_Test:RJ45_Redrawn"]);
		const drawn = await query(root, { lib_id: "Connector_Test:RJ45_Redrawn", graphics: true });
		for (const k of ["pins", "shapes", "pin_name_offset", "hide_pin_names", "hide_pin_numbers", "units"]) expect(drawn[k]).toEqual(stock[k]);
		const { before, after } = await resaveChanges(root, files[0]!);
		expect(after).toBe(before);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("shapes and pins in any order and arcs of either winding are written in KiCad's canonical form", async () => {
		const shapes = [
			{ type: "text", text: "B", at: [0, 0] },
			{ type: "polyline", points: [[5, 0], [6, 1]] },
			{ type: "arc", start: [1, 0], mid: [0, -1], end: [-1, 0] },
			{ type: "arc", start: [11, 0], mid: [10, 1], end: [9, 0] },
			{ type: "circle", center: [0, 0], radius: 1 },
			{ type: "bezier", points: [[0, 0], [1, -1], [2, -1], [3, 0]] },
			{ type: "polyline", points: [[0, 0], [1, 1]], width: 0.254 },
			{ type: "rectangle", start: [-2, -2], end: [2, 2], width: 0.254, fill: "background" },
			{ type: "text", text: "A", at: [0, 0], rot: 90, size: 1 },
			{ type: "circle", center: [-5, 0], radius: 1, unit: 2 },
		];
		const pins = ["10", "2", "1", "A1"].map((number, i) => ({ number, name: "", type: "passive", at: [-7.62, i * 2.54], dir: "L", unit: i % 2 ? 2 : 1 }));
		const { files } = await apply(root, [{ op: "lib_draw_symbol", library: "Connector_Test", name: "Order", reference: "U", pins, shapes }]);
		const { before, after } = await resaveChanges(root, files[0]!);
		expect(after).toBe(before);
		const q = await query(root, { lib_id: "Connector_Test:Order", graphics: true });
		expect(q.units).toBe(2);
		// the clockwise-on-sheet arc came back with its ends swapped, the other unchanged
		const arcs = (q.shapes as Record<string, unknown>[]).filter(s => s.type === "arc");
		expect(arcs).toContainEqual({ type: "arc", start: [-1, 0], mid: [0, -1], end: [1, 0] });
		expect(arcs).toContainEqual({ type: "arc", start: [11, 0], mid: [10, 1], end: [9, 0] });
	}, 60_000);

	test.skipIf(!HAS_KICAD)("lib_copy_symbol then lib_edit_symbol: remove, move and add pins, replace shapes", async () => {
		const stock = await query(root, { lib_id: RJ45, graphics: true });
		const { notes, files } = await apply(root, [
			{ op: "lib_copy_symbol", from: RJ45, library: "Connector_Copy", name: "HR871150C_Test", footprint: "Connector_RJ:RJ45_Abracon_ARJP11A-MA_Horizontal" },
		]);
		expect(notes.join("\n")).toContain("CC-BY-SA 4.0");
		const src = fs.readFileSync(files[0]!, "utf8");
		expect(src).toContain('(symbol "HR871150C_Test_1_1"');
		expect(src).not.toContain("ARJP11A-MASA-B-A-EMU2_");
		const copied = await query(root, { lib_id: "Connector_Copy:HR871150C_Test", graphics: true });
		expect((copied.properties as Record<string, string>).Value).toBe("HR871150C_Test");
		expect(copied.pins).toEqual(stock.pins);
		expect(copied.shapes).toEqual(stock.shapes);

		const box = { type: "rectangle", start: [-17.78, -11.43], end: [20.32, 10.16], width: 0.254, fill: "background" };
		await apply(root, [
			{
				op: "lib_edit_symbol",
				library: "Connector_Copy",
				name: "HR871150C_Test",
				shapes: [box],
				pins: [
					{ number: "8", remove: true },
					{ number: "6", at: [22.86, -2.54], dir: "R" },
					{ number: "15", name: "SH", type: "passive", at: [22.86, 7.62], dir: "R" },
				],
			},
		]);
		const edited = await query(root, { lib_id: "Connector_Copy:HR871150C_Test", graphics: true });
		expect(edited.shapes).toEqual([{ type: "rectangle", start: [-17.78, -11.43], end: [20.32, 10.16], width: 0.254, fill: "background" }]);
		const want = (stock.pins as PinOut[])
			.filter(p => p.number !== "8")
			.map(p => (p.number === "6" ? { ...p, at: [22.86, -2.54], dir: "R" } : p))
			.concat([{ number: "15", name: "SH", type: "passive", at: [22.86, 7.62], dir: "R", length: 2.54, hidden: false, unit: 1 } as PinOut]);
		const byNum = (a: PinOut, b: PinOut) => a.number.localeCompare(b.number);
		expect((edited.pins as PinOut[]).sort(byNum)).toEqual(want.sort(byNum));
		const { before, after } = await resaveChanges(root, files[0]!);
		expect(after).toBe(before);

		await expect(apply(root, [{ op: "lib_edit_symbol", library: "Connector_Copy", name: "HR871150C_Test", pins: [{ number: "99", name: "X" }] }])).rejects.toThrow(
			/no pin 99; to add it give "at" and "dir"/,
		);
		await expect(apply(root, [{ op: "lib_edit_symbol", library: "Connector_Copy", name: "HR871150C_Test", shapes: [{ type: "text", text: "x", at: [0, 0], fill: "outline" }] }])).rejects.toThrow(
			/unknown key "fill"/,
		);
		await expect(apply(root, [{ op: "lib_edit_symbol", library: "Connector_Copy", name: "HR871150C_Test", pins: [{ number: "1", dir: "left" }] }])).rejects.toThrow(/L, R, U, D/);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("lib_copy_symbol flattens a derived De Morgan symbol", async () => {
		const { files } = await apply(root, [{ op: "lib_copy_symbol", from: "74xx:74HC00", library: "Logic_Test", name: "HC00_Copy" }]);
		const q = await query(root, { lib_id: "Logic_Test:HC00_Copy", graphics: true });
		expect(q.units).toBe(5);
		expect((q.shapes as { body_style?: number }[]).some(s => s.body_style === 2)).toBe(true);
		expect(fs.readFileSync(files[0]!, "utf8")).toContain("(body_styles demorgan)");
		const { before, after } = await resaveChanges(root, files[0]!);
		expect(after).toBe(before);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("a drawn symbol passes KLC", async () => {
		const pins = [
			{ number: "1", name: "IN", type: "input", at: [-7.62, -2.54], dir: "L" },
			{ number: "2", name: "GND", type: "power_in", at: [0, 7.62], dir: "D" },
			{ number: "3", name: "OUT", type: "output", at: [7.62, -2.54], dir: "R" },
			{ number: "4", name: "VCC", type: "power_in", at: [0, -7.62], dir: "U" },
		];
		const { files, notes } = await apply(root, [
			{
				op: "lib_draw_symbol",
				library: "Interface_Draw_Test",
				name: "TST42",
				reference: "U",
				footprint: "Package_TO_SOT_SMD:SOT-23-5",
				fp_filters: ["SOT?23*"],
				datasheet: "https://example.com/tst42.pdf",
				description: "Test buffer, SOT-23",
				keywords: "test buffer",
				pins,
				shapes: [
					{ type: "rectangle", start: [-5.08, -5.08], end: [5.08, 5.08], width: 0.254, fill: "background" },
					{ type: "polyline", points: [[-1.27, -1.27], [1.27, 0], [-1.27, 1.27], [-1.27, -1.27]], width: 0.254 },
				],
			},
		]);
		expect(notes.join("\n")).toContain("Interface_Draw_Test:TST42");
		await cli.upgrade("sym", files[0]!, { cwd: path.dirname(files[0]!), repoRoot: root });
		const findings = await klcFindings(Project.open(PROJECT, root), files);
		expect(findings.filter(f => f.severity !== "info")).toEqual([]);
	}, 60_000);

	test.skipIf(!HAS_KICAD)("lib_fp_filter: KiCad's filter matching, stacked pin numbers and the default footprint", async () => {
		const pins = [
			{ number: "[1-3]", name: "S", type: "passive", at: [-5.08, 0], dir: "L" },
			{ number: "4", name: "G", type: "passive", at: [5.08, 0], dir: "R" },
		];
		const draw = (name: string, fp_filters: string[], footprint?: string) => ({
			op: "lib_draw_symbol",
			library: "Filter_Test",
			name,
			reference: "Q",
			fp_filters,
			footprint,
			pins,
			shapes: [{ type: "rectangle", start: [-2.54, -2.54], end: [2.54, 2.54] }],
		});
		await apply(root, [draw("WithLib", ["package_to_sot_smd:sot-143*"], "Package_TO_SOT_SMD:SOT-23"), draw("NoLib", ["SOT?143*", "TSOT?23*"])]);

		const withLib = await query(root, { lib_id: "Filter_Test:WithLib" }, "lib_fp_filter");
		expect(withLib.pin_count).toBe(4);
		const fps = withLib.footprints as { id: string; pads: number }[];
		expect(fps.length).toBeGreaterThan(0);
		expect(fps.every(f => f.id.startsWith("Package_TO_SOT_SMD:SOT-143"))).toBe(true);
		expect(fps[0]!.pads).toBe(4);
		expect(withLib.default_footprint).toEqual({ footprint: "Package_TO_SOT_SMD:SOT-23", exists: true, accepted: false, pads: 3, pad_count_matches: false });

		const noLib = await query(root, { lib_id: "Filter_Test:NoLib", limit: 3 }, "lib_fp_filter");
		expect(noLib.default_footprint).toBeNull();
		const all = noLib.footprints as { id: string; pads: number }[];
		expect(all.length).toBe(3);
		expect(noLib.shown).toBe(3);
		expect(noLib.total as number).toBeGreaterThan(noLib.with_pin_count as number);
		// pin-count matches first, then the rest (TSOT-23 variants have 3, 5, 6 or 8 pads)
		expect(all[0]!.pads).toBe(4);
	}, 60_000);
});
