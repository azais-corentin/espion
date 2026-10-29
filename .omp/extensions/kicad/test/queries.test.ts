import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import { compactJson } from "../src/format";
import { type Box, overlap } from "../src/geom";
import { Project } from "../src/project";
import { queries } from "../src/queries/project";
import { validate } from "../src/spec";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

let root = "";
let project: Project;
let analysis: Analysis;

beforeAll(() => {
	root = stageFixture();
	project = Project.open(PROJECT, root);
	analysis = new Analysis(project);
});

afterAll(() => {
	analysis.dispose();
	cleanup(root);
});

// biome-ignore lint/suspicious/noExplicitAny: query results are plain JSON, inspected by shape
async function q(what: string, args: Record<string, unknown> = {}): Promise<any> {
	const def = queries.find(d => d.what === what);
	if (!def) throw new Error(`no query ${what}`);
	return def.run({ project, analysis, libs: () => project.libs() }, validate(what, args, def.fields));
}

/** Every symbol body (with pins) and wire on a sheet, as boxes. */
function drawn(sheet: string): Box[] {
	const inst = project.sheet(sheet);
	return [...inst.sch.symbols().map(s => inst.sch.bodyBox(s)), ...inst.sch.wires().map(w => [Math.min(w.a[0], w.b[0]), Math.min(w.a[1], w.b[1]), Math.max(w.a[0], w.b[0]), Math.max(w.a[1], w.b[1])] as Box)];
}

describe("project queries", () => {
	test("hierarchy lists every sheet with its block and pins", async () => {
		const { sheets } = await q("hierarchy");
		expect(sheets.map((s: { name: string }) => s.name).sort()).toEqual(
			["Camera", "Ethernet", "IMU", "LED driver", "LED ring", "MCU", "Power", "root"].sort(),
		);
		const mcu = sheets.find((s: { name: string }) => s.name === "MCU");
		expect(mcu).toMatchObject({ file: "mcu.kicad_sch", namePath: "/MCU/", parent: "root", paper: "A4" });
		expect(mcu.path).toMatch(/^\/[0-9a-f-]+\/[0-9a-f-]+$/);
		expect(mcu.pins).toContainEqual({ name: "PWR_SRC", shape: "input", side: "left", at: [127, 101.6] });
		const pages = sheets.map((s: { page: string }) => s.page);
		expect(new Set(pages).size).toBe(8);
		expect(sheets.find((s: { name: string }) => s.name === "root").page).toBe("1");
	});

	test("symbols filter by pattern and leave power symbols out by default", async () => {
		const all = await q("symbols", { limit: 1000 });
		expect(all.rows.some((r: string[]) => r[0]!.startsWith("#"))).toBe(false);
		const caps = await q("symbols", { ref: "C*", sheet: "MCU" });
		expect(caps.rows.length).toBeGreaterThan(0);
		for (const r of caps.rows) {
			expect(r[0]).toMatch(/^C\d+$/);
			expect(r[4]).toBe("MCU");
		}
		const pwr = await q("symbols", { ref: "#PWR*", value: "GND", limit: 5 });
		expect(pwr.total).toBeGreaterThan(5);
		expect(pwr.rows).toHaveLength(5);
		for (const r of pwr.rows) expect(r[1]).toBe("GND");
		const byLib = await q("symbols", { lib_id: "Device:LED" });
		expect(byLib.rows.map((r: string[]) => r[0])).toContain("D4");
	});

	test.skipIf(!HAS_KICAD)("symbol U1 shows GPIO20 on /MCU/VBUS_SENSE", async () => {
		const u1 = await q("symbol", { ref: "U1" });
		const pin = u1.pins.find((p: { num: string }) => p.num === "16");
		expect(pin).toMatchObject({ name: "GPIO20", net: "/MCU/VBUS_SENSE", dir: "L" });
		expect(u1.fields.find((f: { name: string }) => f.name === "Reference")).toMatchObject({ value: "U1", visible: true });
		expect(u1.attributes.in_bom).toBe(true);
		expect(u1.units).toHaveLength(1);
		expect(u1.description).toContain("ESP32-P4");
		await expect(q("symbol", { ref: "Q99" })).rejects.toThrow(/Q1/);
	});

	test.skipIf(!HAS_KICAD)("net VBUS_SENSE by leaf name: every pin with its position, the label carrying it", async () => {
		const nl = await analysis.netlist();
		const truth = nl.nets.find(n => n.name === "/MCU/VBUS_SENSE")!;
		const net = await q("net", { name: "VBUS_SENSE" });
		expect(net.name).toBe("/MCU/VBUS_SENSE");
		expect(net.scope).toBe("local");
		const pins = net.pins.map((p: { ref: string; pin: string }) => `${p.ref}.${p.pin}`).sort();
		expect(pins).toEqual(truth.nodes.map(n => `${n.ref}.${n.pin}`).sort());
		expect(pins).toEqual(["C49.1", "R62.2", "R63.1", "U1.16"]);
		// positions are the real pin ends on the MCU sheet
		const u1 = net.pins.find((p: { ref: string }) => p.ref === "U1");
		expect(u1).toMatchObject({ sheet: "MCU", name: "GPIO20" });
		const mcu = project.sheet("MCU");
		const sym = mcu.sch.symbols().find(s => mcu.sch.refOf(s, mcu.path) === "U1")!;
		expect(mcu.sch.pins(sym).find(p => p.num === "16")!.at).toEqual(u1.at);
		expect(net.sheets).toContainEqual({ sheet: "MCU", labels: [{ kind: "label", name: "VBUS_SENSE", at: expect.any(Array) }] });
	});

	test.skipIf(!HAS_KICAD)("net: hierarchical and global scopes, unknown names fail", async () => {
		const strobe = await q("net", { name: "/Camera/STROBE" });
		expect(strobe.scope).toBe("hierarchical");
		expect(strobe.sheets.find((s: { sheet: string }) => s.sheet === "root").sheetPins.length).toBeGreaterThanOrEqual(2);
		const gnd = await q("net", { name: "GND" });
		expect(gnd.scope).toBe("global");
		expect(gnd.pinCount).toBeGreaterThan(gnd.pins.length);
		expect(gnd.sheets.some((s: { power?: unknown[] }) => s.power?.length)).toBe(true);
		await expect(q("net", { name: "NO_SUCH_NET" })).rejects.toThrow();
	});

	test.skipIf(!HAS_KICAD)("net: an ambiguous leaf name lists the candidates", async () => {
		// a second VBUS_SENSE net: rename the IMU sheet's IMU_INT1 labels in a separate copy
		const other = stageFixture();
		try {
			const imu = path.join(other, PROJECT, "imu.kicad_sch");
			fs.writeFileSync(imu, fs.readFileSync(imu, "utf8").replaceAll('(label "IMU_INT1"', '(label "VBUS_SENSE"'));
			const p = Project.open(PROJECT, other);
			const def = queries.find(d => d.what === "net")!;
			const run = (name: string) => def.run({ project: p, analysis: new Analysis(p), libs: () => p.libs() }, { name });
			const err = await run("VBUS_SENSE").catch((e: Error) => e.message);
			expect(err).toContain("/MCU/VBUS_SENSE");
			expect(err).toContain("/IMU/VBUS_SENSE");
			expect(await run("/IMU/VBUS_SENSE")).toMatchObject({ name: "/IMU/VBUS_SENSE", scope: "local" });
		} finally {
			cleanup(other);
		}
	});

	test.skipIf(!HAS_KICAD)("nets filter by class and sheet, pins capped at 12 with the count", async () => {
		const mdi = await q("nets", { class: "MDI" });
		expect(mdi.nets.length).toBeGreaterThan(0);
		for (const n of mdi.nets) expect(n.class).toBe("MDI");
		const all = await q("nets", { limit: 1000 });
		const big = all.nets.find((n: { count: number }) => n.count > 12);
		expect(big.pins.split(" ")).toHaveLength(12);
		const vbus = await q("nets", { name: "VBUS_S*", sheet: "MCU" });
		expect(vbus.nets.map((n: { name: string }) => n.name)).toEqual(["/MCU/VBUS_SENSE"]);
	});

	test.skipIf(!HAS_KICAD)("labels carry the net they name", async () => {
		const hl = await q("labels", { sheet: "MCU", kind: "hierarchical_label", name: "PWR_SRC" });
		expect(hl.labels).toHaveLength(1);
		expect(hl.labels[0]).toMatchObject({ kind: "hierarchical_label", shape: "input", net: "/MCU/PWR_SRC" });
		const vb = await q("labels", { name: "vbus_sense" });
		expect(vb.labels.map((l: { net: string }) => l.net)).toEqual(["/MCU/VBUS_SENSE"]);
	});

	test.skipIf(!HAS_KICAD)("netclasses: MDI with its settings, patterns and member nets", async () => {
		const nl = await analysis.netlist();
		const { classes } = await q("netclasses");
		const mdi = classes.find((c: { name: string }) => c.name === "MDI");
		expect(mdi.settings.clearance).toBe(0.15);
		const members = nl.nets.filter(n => n.netclass === "MDI").map(n => n.name).sort();
		expect(members.length).toBeGreaterThan(0);
		expect(mdi.count).toBe(members.length);
		expect(mdi.nets.split(", ")).toEqual(members);
		expect(mdi.nets).toContain("/Ethernet/ETH_TD_P");
		expect(classes.find((c: { name: string }) => c.name === "POE_CABLE").patterns).toContain("/Ethernet/POE_*");
	});

	test("rules from the .kicad_dru, comments skipped", async () => {
		const { rules } = await q("rules");
		expect(rules.map((r: { name: string }) => r.name)).toEqual(["PoE isolation", "MIPI_CSI spacing", "MDI spacing", "USB spacing"]);
		expect(rules[2]).toMatchObject({ condition: "A.NetClass == 'MDI' && B.NetClass != 'MDI'", constraints: ["clearance min 0.55mm"] });
	});

	test.skipIf(!HAS_KICAD)("bom with the Order preset: grouped rows with LCSC numbers", async () => {
		const b = await q("bom", { preset: "Order" });
		expect(b.preset).toBe("Order");
		const lcsc = b.header.indexOf("LCSC");
		expect(lcsc).toBeGreaterThanOrEqual(0);
		const c17 = b.rows.find((r: string[]) => r[0]!.split(",").includes("C17"));
		expect(c17[lcsc]).toBe("C46550465");
		expect(b.rows.some((r: string[]) => /[,-]/.test(r[0]!))).toBe(true);
		const dflt = await q("bom");
		expect(dflt.preset).toBe("Order");
		await expect(q("bom", { preset: "Nope" })).rejects.toThrow(/Order/);
	});

	test("sheet summary: counts and used area inside the page", async () => {
		const s = await q("sheet", { sheet: "MCU" });
		expect(s.paper).toEqual({ name: "A4", size: [297, 210] });
		const inst = project.sheet("MCU");
		expect(s.counts.wires).toBe(inst.sch.wires().length);
		expect(s.counts.parts + s.counts.power).toBe(inst.sch.symbols().length);
		const [x0, y0, x1, y1] = s.used;
		expect(x0).toBeGreaterThan(0);
		expect(y1).toBeLessThan(210);
		expect(x1).toBeGreaterThan(x0);
		expect(y1).toBeGreaterThan(y0);
		const r = await q("sheet", { sheet: "root" });
		expect(r.blocks).toHaveLength(7);
	});

	test.skipIf(!HAS_KICAD)("free_space near an empty area returns a clear box there", async () => {
		const res = await q("free_space", { sheet: "MCU", near: [60, 180], size: [20, 10] });
		expect(res.boxes).toHaveLength(3);
		const first: Box = res.boxes[0].box;
		for (const b of drawn("MCU")) expect(overlap(first, b)).toBe(false);
		// nearest: within a grid step or two of the requested centre
		expect(Math.abs((first[0] + first[2]) / 2 - 60)).toBeLessThan(2.6);
		expect(Math.abs((first[1] + first[3]) / 2 - 180)).toBeLessThan(2.6);
		for (const v of first) expect(Math.abs(v / 1.27 - Math.round(v / 1.27))).toBeLessThan(1e-6);
		// results don't overlap each other or the frame / title block
		for (const { box } of res.boxes) {
			expect(box[0]).toBeGreaterThanOrEqual(12);
			expect(box[3]).toBeLessThanOrEqual(198);
			expect(overlap(box, [177, 166, 285, 198])).toBe(false);
		}
		expect(overlap(res.boxes[0].box, res.boxes[1].box)).toBe(false);
	});

	test.skipIf(!HAS_KICAD)("free_space in a dense area moves away from the parts", async () => {
		const near: [number, number] = [172.72, 96.52]; // U1's origin, inside its body
		const res = await q("free_space", { sheet: "MCU", near, size: [15, 10], limit: 4 });
		const inst = project.sheet("MCU");
		const bodies = inst.sch.symbols().map(s => inst.sch.bodyBox(s));
		for (const { box } of res.boxes) {
			for (const b of drawn("MCU")) expect(overlap(box, b)).toBe(false);
			expect(box[0] <= near[0] && near[0] <= box[2] && box[1] <= near[1] && near[1] <= box[3]).toBe(false);
		}
		expect(bodies.length).toBeGreaterThan(10);
		// labels' text is avoided too: no label anchor inside a box
		for (const { box } of res.boxes) {
			for (const l of inst.sch.labels()) expect(box[0] < l.at[0] && l.at[0] < box[2] && box[1] < l.at[1] && l.at[1] < box[3]).toBe(false);
		}
	});

	test.skipIf(!HAS_KICAD)("outputs stay small without a limit", async () => {
		const calls: [string, Record<string, unknown>][] = [
			["hierarchy", {}],
			["symbols", {}],
			["symbol", { ref: "U1" }],
			["nets", {}],
			["net", { name: "GND" }],
			["net", { name: "+3V3" }],
			["labels", {}],
			["netclasses", {}],
			["rules", {}],
			["bom", {}],
			["sheet", { sheet: "MCU" }],
			["free_space", { sheet: "Power", near: [150, 100], size: [20, 20] }],
		];
		for (const [what, args] of calls) {
			const lines = compactJson(await q(what, args)).split("\n").length;
			expect({ what, small: lines <= 200 }).toEqual({ what, small: true });
		}
	});
});
