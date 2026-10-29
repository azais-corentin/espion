import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { nextRef } from "../src/annotate";
import * as cli from "../src/cli";
import { samePt } from "../src/geom";
import { Project } from "../src/project";
import { OPS } from "../src/registry";
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
});
