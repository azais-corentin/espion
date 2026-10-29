import { afterEach, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import * as cli from "../src/cli";
import type { Expect } from "../src/netdiff";
import { runEdit } from "../src/txn";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

/** A new sheet "Test" with R66/R67 on net A and both on GND through upright power symbols. */
const OPS = [
	{ op: "create_sheet", parent: "root", name: "Test", file: "test.kicad_sch", at: [137.16, 144.78], size: [25.4, 10.16] },
	{ op: "add_symbol", sheet: "Test", lib_id: "Device:R", at: [101.6, 101.6], value: "10k" },
	{ op: "add_symbol", sheet: "Test", lib_id: "Device:R", at: [111.76, 101.6], value: "10k" },
	{ op: "add_wire", sheet: "Test", points: [[101.6, 97.79], [101.6, 95.25], [111.76, 95.25], [111.76, 97.79]] },
	{ op: "add_label", sheet: "Test", name: "A", at: [104.14, 95.25] },
	{ op: "add_wire", sheet: "Test", points: [[101.6, 105.41], [101.6, 106.68]] },
	{ op: "add_wire", sheet: "Test", points: [[111.76, 105.41], [111.76, 106.68]] },
	{ op: "add_power", sheet: "Test", net: "GND", at: [101.6, 106.68] },
	{ op: "add_power", sheet: "Test", net: "GND", at: [111.76, 106.68] },
];
const EXPECT: Expect = {
	added_parts: ["R66", "R67"],
	nets: [
		{ name: "A", pins: ["R66.1", "R67.1"] },
		{ name: "GND", added: ["R66.2", "R67.2"] },
	],
};

function snapshot(dir: string): Map<string, string> {
	return new Map(fs.readdirSync(dir).map(f => [f, fs.readFileSync(path.join(dir, f), "utf8")]));
}

let root = "";
afterEach(() => cleanup(root));

describe.skipIf(!HAS_KICAD)("kicad_edit transaction", () => {
	test("applies a verified edit; the written files are KiCad's canonical re-save", async () => {
		root = stageFixture();
		const dir = path.join(root, PROJECT);
		const r = await runEdit({ ops: OPS, expect: EXPECT }, root, undefined);
		expect(r.report).toStartWith("Applied.");
		expect(r.applied).toBe(true);
		const nl = await cli.netlist(path.join(dir, "espion-cam.kicad_sch"), { cwd: dir, repoRoot: root });
		const a = nl.nets.find(n => n.name === "/Test/A");
		expect(a?.nodes.map(x => `${x.ref}.${x.pin}`).sort()).toEqual(["R66.1", "R67.1"]);
		// re-saving again changes nothing: the toolkit wrote KiCad's own output
		const before = snapshot(dir);
		for (const f of ["espion-cam.kicad_sch", "test.kicad_sch"]) await cli.upgrade("sch", path.join(dir, f), { cwd: dir, repoRoot: root });
		expect(snapshot(dir)).toEqual(before);
		expect(JSON.parse(before.get("espion-cam.kicad_pro")!).sheets.some((s: [string, string]) => s[1] === "Test")).toBe(true);
	});

	test("a result that differs from expect is not written", async () => {
		root = stageFixture();
		const dir = path.join(root, PROJECT);
		const before = snapshot(dir);
		const r = await runEdit({ ops: OPS, expect: { ...EXPECT, nets: [EXPECT.nets![0]!] } }, root, undefined);
		expect(r.applied).toBe(false);
		expect(r.report).toContain("NOT APPLIED");
		expect(r.report).toContain("[connectivity] net GND");
		expect(snapshot(dir)).toEqual(before);
	});

	test("a new drawing-rule violation is not written", async () => {
		root = stageFixture();
		const dir = path.join(root, PROJECT);
		const before = snapshot(dir);
		// the label's text runs past the end of a 1.27 mm wire (R3)
		const ops = [...OPS.slice(0, 4), { op: "add_label", sheet: "Test", name: "A_LONG_NAME", at: [110.49, 95.25] }, ...OPS.slice(5)];
		const r = await runEdit(
			{ ops, expect: { ...EXPECT, nets: [{ name: "A_LONG_NAME", pins: ["R66.1", "R67.1"] }, EXPECT.nets![1]!] } },
			root,
			undefined,
		);
		expect(r.applied).toBe(false);
		expect(r.report).toContain("[style]");
		expect(r.report).toContain("A_LONG_NAME");
		expect(snapshot(dir)).toEqual(before);
	});

	test("a dry run checks everything and writes nothing", async () => {
		root = stageFixture();
		const dir = path.join(root, PROJECT);
		const before = snapshot(dir);
		const r = await runEdit({ ops: OPS, expect: EXPECT, dry_run: true }, root, undefined);
		expect(r.report).toStartWith("Dry run: all checks pass");
		expect(snapshot(dir)).toEqual(before);
	});

	test("refuses while KiCad has the project open", async () => {
		root = stageFixture();
		fs.writeFileSync(path.join(root, PROJECT, "~espion-cam.kicad_pro.lck"), "");
		await expect(runEdit({ ops: OPS, expect: EXPECT }, root, undefined)).rejects.toThrow(/KiCad has the project open/);
	});
});
