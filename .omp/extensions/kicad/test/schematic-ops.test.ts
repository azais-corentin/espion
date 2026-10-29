import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import { nextRef } from "../src/annotate";
import * as cli from "../src/cli";
import { type Pt, samePt } from "../src/geom";
import { Project } from "../src/project";
import { queries } from "../src/queries/project";
import { OPS } from "../src/registry";
import { propValue } from "../src/sch";
import { child, children, print, text } from "../src/sexpr";
import { validate } from "../src/spec";
import type { OpContext } from "../src/types";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

async function context(root: string): Promise<OpContext> {
	const project = Project.open(PROJECT, root);
	return { project, libs: await project.libs(), notes: [], touch() {}, touchPart() {} };
}

async function apply(ctx: OpContext, op: string, args: Record<string, unknown>): Promise<void> {
	const def = OPS.get(op);
	if (!def) throw new Error(`no op ${op}`);
	await def.apply(ctx, validate(op, args, def.fields));
}

/** Write the pending files into the staged project and let KiCad re-save them. */
async function commit(ctx: OpContext, root: string): Promise<void> {
	for (const [f, content] of ctx.project.pendingWrites()) {
		fs.writeFileSync(f, content);
		if (f.endsWith(".kicad_sch")) await cli.upgrade("sch", f, { cwd: path.dirname(f), repoRoot: root });
	}
}

async function netlist(root: string): Promise<cli.Netlist> {
	const p = Project.open(PROJECT, root);
	return cli.netlist(p.rootFile, { cwd: p.dir, repoRoot: root });
}

/** net name → sorted "REF.pin" list. */
function nets(n: cli.Netlist): Map<string, string[]> {
	return new Map(n.nets.map(x => [x.name, x.nodes.map(d => `${d.ref}.${d.pin}`).sort()]));
}

function netOf(n: cli.Netlist, node: string): string | undefined {
	return n.nets.find(x => x.nodes.some(d => `${d.ref}.${d.pin}` === node))?.name;
}

async function ercKeys(root: string): Promise<string[]> {
	const p = Project.open(PROJECT, root);
	const v = await cli.erc(p.rootFile, { cwd: p.dir, repoRoot: root });
	return v
		.filter(x => !x.excluded)
		.map(x => `${x.type}|${x.items.map(i => i.description).sort().join("|")}`)
		.sort();
}

describe.skipIf(!HAS_KICAD)("schematic ops on the fixture (kicad-cli)", () => {
	let root = "";
	let ercBefore: string[] = [];
	beforeAll(async () => {
		root = stageFixture();
		ercBefore = await ercKeys(root);
	}, 120_000);
	afterAll(() => cleanup(root));

	test("adding resistors, wires and labels connects the intended pins", async () => {
		const ctx = await context(root);
		await apply(ctx, "add_symbol", { sheet: "Power", lib_id: "Device:R", at: [63.5, 177.8], value: "10k", footprint: "Resistor_SMD:R_0402_1005Metric" });
		await apply(ctx, "add_symbol", { sheet: "Power", lib_id: "Device:R", at: [88.9, 177.8], value: "1k" });
		// R66.1 (top) and R67.2 (bottom) join through two TEST_NET labels
		await apply(ctx, "add_wire", { sheet: "Power", points: [[63.5, 173.99], [63.5, 171.45], [76.2, 171.45]] });
		await apply(ctx, "add_label", { sheet: "Power", name: "TEST_NET", at: [76.2, 171.45], rot: 180 });
		await apply(ctx, "add_wire", { sheet: "Power", points: [[88.9, 181.61], [88.9, 184.15], [101.6, 184.15]] });
		await apply(ctx, "add_label", { sheet: "Power", name: "TEST_NET", at: [101.6, 184.15], rot: 180 });
		await apply(ctx, "add_wire", { sheet: "Power", points: [[63.5, 181.61], [63.5, 182.88]] });
		await apply(ctx, "add_wire", { sheet: "Power", points: [[88.9, 173.99], [88.9, 172.72]] });
		await commit(ctx, root);
		const n = nets(await netlist(root));
		expect(n.get("/Power/TEST_NET")).toEqual(["R66.1", "R67.2"]);
	});

	test("add_power puts the pin exactly on the point and joins the power net", async () => {
		const ctx = await context(root);
		await apply(ctx, "add_power", { sheet: "Power", net: "GND", at: [63.5, 182.88] });
		await apply(ctx, "add_power", { sheet: "Power", net: "+3V3", at: [88.9, 172.72] });
		const power = ctx.project.sheet("Power");
		for (const [ref, p] of [["#PWR325", [63.5, 182.88]], ["#PWR326", [88.9, 172.72]]] as const) {
			const sym = power.sch.symbols().find(s => power.sch.refOf(s, power.path) === ref);
			expect(sym).toBeDefined();
			expect(samePt(power.sch.pins(sym!)[0]!.at, p)).toBe(true);
		}
		await commit(ctx, root);
		const nl = await netlist(root);
		expect(netOf(nl, "R66.2")).toBe("GND");
		expect(netOf(nl, "R67.1")).toBe("+3V3");
	});

	test("moving a symbol with drag keeps its connectivity", async () => {
		const before = nets(await netlist(root));
		const ctx = await context(root);
		await apply(ctx, "move_symbol", { ref: "R66", at: [63.5, 176.53] });
		await commit(ctx, root);
		expect(nets(await netlist(root))).toEqual(before);
		// a sideways move would turn the vertical wires on its pins diagonal
		const side = await context(root);
		await expect(apply(side, "move_symbol", { ref: "R66", at: [66.04, 176.53] })).rejects.toThrow();
	});

	test("rename_net renames a local net and keeps its pins; power nets are refused", async () => {
		const ctx = await context(root);
		await apply(ctx, "rename_net", { from: "TEST_NET", to: "TEST_RENAMED", sheet: "Power" });
		await commit(ctx, root);
		const n = nets(await netlist(root));
		expect(n.has("/Power/TEST_NET")).toBe(false);
		expect(n.get("/Power/TEST_RENAMED")).toEqual(["R66.1", "R67.2"]);
		await expect(apply(await context(root), "rename_net", { from: "GND", to: "GND2" })).rejects.toThrow();
	});

	test("ERC has no new violations after the edits", async () => {
		expect(await ercKeys(root)).toEqual(ercBefore);
	}, 120_000);

	test("set_fields and set_attrs show up in the netlist", async () => {
		const ctx = await context(root);
		await apply(ctx, "set_fields", { ref: "R66", fields: { MPN: "RC0402", Foo: "bar", Value: "4k7" }, show: ["MPN"] });
		await apply(ctx, "set_attrs", { ref: "R66", dnp: true, in_bom: false });
		await apply(ctx, "set_fields", { ref: "R67", fields: { Foo: "baz" } });
		await apply(ctx, "set_fields", { ref: "R67", fields: { Foo: null } });
		await commit(ctx, root);
		const c = (await netlist(root)).components;
		const r66 = c.get("R66")!;
		expect(r66.value).toBe("4k7");
		expect(r66.fields.MPN).toBe("RC0402");
		expect(r66.fields.Foo).toBe("bar");
		expect(r66.dnp).toBe(true);
		expect(r66.excludeFromBom).toBe(true);
		const r67 = c.get("R67")!;
		expect("Foo" in r67.fields).toBe(false);
		expect(r67.dnp).toBe(false);
	});
});

describe("schematic ops in memory", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	test("annotation follows refBase for #PWR and the project-wide maximum for R", async () => {
		const ctx = await context(root);
		const p = ctx.project;
		expect(nextRef(p, "#PWR", p.sheet("Power"))).toBe("#PWR325");
		expect(nextRef(p, "#PWR", p.sheet("root"))).toBe("#PWR105");
		expect(nextRef(p, "R", p.sheet("MCU"))).toBe("R66");
		expect(nextRef(p, "R", p.sheet("MCU"), new Set(["R66"]))).toBe("R67");

		await apply(ctx, "add_power", { sheet: "Power", net: "GND", at: [63.5, 182.88] });
		await apply(ctx, "add_symbol", { sheet: "MCU", lib_id: "Device:R", at: [63.5, 177.8], ref: "R?" });
		await apply(ctx, "add_symbol", { sheet: "Power", lib_id: "Device:R", at: [88.9, 177.8], ref: "R?" });
		await apply(ctx, "annotate", {});
		const refs = p.allRefs();
		expect(refs.has("#PWR325")).toBe(true);
		expect(refs.has("R66")).toBe(true);
		expect(refs.has("R67")).toBe(true);
		expect([...refs].some(r => r.endsWith("?"))).toBe(false);
		await apply(ctx, "add_symbol", { sheet: "Power", lib_id: "Device:R", at: [101.6, 177.8] });
		expect(p.allRefs().has("R68")).toBe(true);

		// a sheet created in this edit takes the next #PWR block after the highest configured one (IMU 800)
		await apply(ctx, "create_sheet", { parent: "root", name: "Probe", file: "probe.kicad_sch", at: [88.9, 121.92], size: [25.4, 12.7] });
		await apply(ctx, "add_power", { sheet: "Probe", net: "GND", at: [101.6, 101.6] });
		expect(p.allRefs().has("#PWR901")).toBe(true);
	});

	test("diagonal wires and off-grid points are rejected", async () => {
		const ctx = await context(root);
		await expect(apply(ctx, "add_wire", { sheet: "Power", points: [[50.8, 177.8], [53.34, 180.34]] })).rejects.toThrow(/diagonal/);
		await expect(apply(ctx, "add_wire", { sheet: "Power", points: [[50.8, 177.8], [50.8, 180]] })).rejects.toThrow(/grid/);
		await expect(apply(ctx, "add_symbol", { sheet: "Power", lib_id: "Device:R", at: [50, 177.8] })).rejects.toThrow(/grid/);
		expect(ctx.project.pendingWrites().size).toBe(0);
	});

	test("a wire ending on another wire's interior gets a junction; removing it drops the junction", async () => {
		const ctx = await context(root);
		const sch = ctx.project.sheet("Power").sch;
		const at = [55.88, 190.5] as const;
		const before = sch.points("junction").length;
		await apply(ctx, "add_wire", { sheet: "Power", points: [[50.8, 190.5], [60.96, 190.5]] });
		await apply(ctx, "add_wire", { sheet: "Power", points: [[55.88, 186.69], at] });
		expect(sch.points("junction").some(j => samePt(j.at, at))).toBe(true);
		await apply(ctx, "remove_wire", { sheet: "Power", from: [55.88, 186.69], to: at });
		expect(sch.points("junction").length).toBe(before);
		await expect(apply(ctx, "remove_wire", { sheet: "Power", at: [50.8, 186.69] })).rejects.toThrow();
	});

	test("removing a wire through a T drops the junction on its middle; redrawing the top part leaves no junction at the corner", async () => {
		// the session's Power sheet: a vertical (170.18, 55.88)-(170.18, 71.12) with a T at (170.18, 63.5) feeding U4.3
		const ctx = await context(root);
		const sch = ctx.project.sheet("Power").sch;
		const t: Pt = [170.18, 63.5];
		const hasJunction = () => sch.points("junction").some(j => samePt(j.at, t));
		expect(hasJunction()).toBe(true);
		await apply(ctx, "remove_wire", { sheet: "Power", from: [170.18, 55.88], to: [170.18, 71.12] });
		expect(hasJunction()).toBe(false);
		expect(ctx.notes.at(-1)).toContain("junction(s) no longer needed removed at (170.18, 63.5)");
		expect(ctx.notes.at(-1)).toContain("left unconnected: ");
		await apply(ctx, "add_wire", { sheet: "Power", points: [[170.18, 55.88], t] });
		expect(hasJunction()).toBe(false);
		expect(sch.junctionDegree(t)).toBe(2);
	});

	test("remove_wire from/to inside one segment removes just that stretch and keeps the rest", async () => {
		const ctx = await context(root);
		const sch = ctx.project.sheet("Power").sch;
		const vertical = () => sch.wires().filter(w => w.a[0] === 170.18 && w.b[0] === 170.18 && Math.min(w.a[1], w.b[1]) >= 55.88 && Math.max(w.a[1], w.b[1]) <= 71.12);
		const [orig] = vertical();
		expect(vertical()).toHaveLength(1);
		const stroke = print(child(orig!.node, "stroke")!);
		const uuid = text(child(orig!.node, "uuid"));

		// a stretch in the middle leaves two wires, with fresh uuids and the same stroke
		await apply(ctx, "remove_wire", { sheet: "Power", from: [170.18, 60.96], to: [170.18, 58.42] });
		const two = vertical();
		expect(two.map(w => [w.a, w.b])).toEqual([
			[[170.18, 55.88], [170.18, 58.42]],
			[[170.18, 60.96], [170.18, 71.12]],
		]);
		const uuids = two.map(w => text(child(w.node, "uuid")));
		expect(new Set([uuid, ...uuids]).size).toBe(3);
		for (const w of two) expect(print(child(w.node, "stroke")!)).toBe(stroke);
		expect(ctx.notes.at(-1)).toContain("keeping (170.18, 55.88)-(170.18, 58.42) and (170.18, 60.96)-(170.18, 71.12)");
		// the T still has its third connection: the wire passes through
		expect(sch.points("junction").some(j => samePt(j.at, [170.18, 63.5]))).toBe(true);

		// a stretch at the end of a segment: the T becomes a corner and its junction goes
		await apply(ctx, "remove_wire", { sheet: "Power", from: [170.18, 71.12], to: [170.18, 63.5] });
		expect(vertical().map(w => [w.a, w.b])).toEqual([
			[[170.18, 55.88], [170.18, 58.42]],
			[[170.18, 60.96], [170.18, 63.5]],
		]);
		expect(sch.points("junction").some(j => samePt(j.at, [170.18, 63.5]))).toBe(false);
		expect(ctx.notes.at(-1)).toContain("left unconnected: wire end (170.18, 71.12)");

		// points on two different segments are refused, naming the segments there
		await expect(apply(ctx, "remove_wire", { sheet: "Power", from: [170.18, 55.88], to: [170.18, 63.5] })).rejects.toThrow(
			/don't lie on one wire segment.*wires at from: \(170\.18, 55\.88\)-\(170\.18, 58\.42\)/,
		);
	});

	test("change_symbol swaps the part in place, keeps the sourcing fields and reports the wiring it no longer meets", async () => {
		const ctx = await context(root);
		const sch = ctx.project.sheet("Power").sch;
		const before = ctx.project.findSymbol("U4").sym;
		const at = sch.pose(before);
		await apply(ctx, "change_symbol", { ref: "U4", lib_id: "Power_Management:TPS2412D", fields: { MPN: "TPS2412DR" } });
		const u4 = ctx.project.findSymbol("U4").sym;
		expect(sch.libId(u4)).toBe("Power_Management:TPS2412D");
		expect(sch.pose(u4)).toEqual(at);
		expect(propValue(u4, "Value")).toBe("TPS2412D");
		expect(propValue(u4, "Manufacturer")).toBe("TI");
		expect(propValue(u4, "LCSC")).toBe("C3235557");
		expect(propValue(u4, "MPN")).toBe("TPS2412DR");
		expect(children(u4, "pin").map(p => text(p)).sort()).toEqual(["1", "2", "3", "4", "5", "6", "7", "8"]);
		expect(sch.libSymbol("Power_Management:TPS2116DRL")).toBeUndefined();
		expect(sch.libSymbol("Power_Management:TPS2412D")).toBeDefined();
		const note = ctx.notes.at(-1)!;
		expect(note).toContain("changed U4: Power_Management:TPS2116DRL → Power_Management:TPS2412D");
		expect(note).toContain('MPN "TPS2116DRLR" → "TPS2412DR"');
		expect(note).toContain('kept Footprint="Package_TO_SOT_SMD:SOT-583-8", Manufacturer="TI", LCSC="C3235557"');
		expect(note).toContain("wire end (177.8, 63.5) (was U4.3)");
		expect(note).toContain("pins with nothing on them: ");
		expect(note).toContain("U4.5 GATE (187.96, 58.42)");
		expect(note).toContain("pins: 1 VDD (195.58, 66.04) R");
		await expect(apply(ctx, "change_symbol", { ref: "U4", unit: 2 })).rejects.toThrow(/units 1\.\.1/);
		await expect(apply(ctx, "change_symbol", { ref: "U4", fields: { Value: null } })).rejects.toThrow(/mandatory/);
	});

	test("change_symbol without lib_id refreshes a placed symbol from its edited library", async () => {
		const own = stageFixture();
		try {
			// move DP1435-5V's VOUT pin in the project library, as lib_edit_symbol writes it
			const lib = await context(own);
			await apply(lib, "lib_edit_symbol", { library: "Converter_DCDC_SDAPO", name: "DP1435-5V", pins: [{ number: "6", at: [10.16, 0], dir: "R" }] });
			for (const [f, content] of lib.project.pendingWrites()) fs.writeFileSync(f, content);

			const ctx = await context(own);
			const sch = ctx.project.sheet("Power").sch;
			const pin6 = () => sch.pins(ctx.project.findSymbol("U3").sym).find(p => p.num === "6")!.at;
			expect(pin6()).toEqual([71.12, 55.88]);
			const fields = Object.fromEntries(children(ctx.project.findSymbol("U3").sym, "property").map(p => [text(p), text(p, 2)]));
			await apply(ctx, "change_symbol", { ref: "U3" });
			expect(pin6()).toEqual([71.12, 58.42]);
			expect(Object.fromEntries(children(ctx.project.findSymbol("U3").sym, "property").map(p => [text(p), text(p, 2)]))).toEqual(fields);
			const note = ctx.notes.at(-1)!;
			expect(note).toContain("refreshed U3 from the library (Converter_DCDC_SDAPO:DP1435-5V)");
			expect(note).toContain("wire end (71.12, 55.88) (was U3.6)");
			expect(note).toContain("U3.6 VOUT (71.12, 58.42)");
		} finally {
			cleanup(own);
		}
	});

	test("next_ref previews exactly what add_symbol then assigns", async () => {
		const ctx = await context(root);
		const q = queries.find(d => d.what === "next_ref")!;
		const analysis = new Analysis(ctx.project);
		const run = (args: Record<string, unknown>) => q.run({ project: ctx.project, analysis, libs: () => ctx.project.libs() }, validate("next_ref", args, q.fields));
		try {
			const r = (await run({ prefix: "R", sheet: "Power", count: 2 })) as { refs: string[] };
			const pwr = (await run({ prefix: "#PWR", sheet: "Power" })) as { refs: string[] };
			await apply(ctx, "add_symbol", { sheet: "Power", lib_id: "Device:R", at: [63.5, 177.8] });
			await apply(ctx, "add_symbol", { sheet: "Power", lib_id: "Device:R", at: [88.9, 177.8] });
			await apply(ctx, "add_power", { sheet: "Power", net: "GND", at: [63.5, 182.88] });
			const added = ctx.notes.map(n => /^added (\S+)/.exec(n)?.[1]).filter(Boolean);
			expect(added).toEqual([...r.refs, ...pwr.refs]);
			expect(r.refs).toEqual(["R66", "R67"]);
			expect(pwr.refs).toEqual(["#PWR325"]);
		} finally {
			analysis.dispose();
		}
	});
});
