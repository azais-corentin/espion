import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { druProblems } from "../src/checks/rules";
import { bom, netlist } from "../src/cli";
import { druRules, ops } from "../src/ops/project";
import { Project } from "../src/project";
import { text } from "../src/sexpr";
import { validate } from "../src/spec";
import type { OpContext } from "../src/types";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

/** Apply ops to a freshly opened project and write the pending files; returns the op notes. */
async function apply(root: string, list: Record<string, unknown>[]): Promise<string[]> {
	const project = Project.open(PROJECT, root);
	const ctx: OpContext = { project, libs: await project.libs(), notes: [], touch() {}, touchPart() {} };
	for (const { op, ...args } of list) {
		const def = ops.find(o => o.op === op);
		if (!def) throw new Error(`no op ${String(op)}`);
		await def.apply(ctx, validate(def.op, args, def.fields));
	}
	for (const [file, content] of project.pendingWrites()) fs.writeFileSync(file, content);
	return ctx.notes;
}

/** The text `after` adds to `before` when it is a pure insertion (else throws). */
function inserted(before: string, after: string): string {
	let p = 0;
	while (p < before.length && before[p] === after[p]) p++;
	let s = 0;
	while (s < before.length - p && before[before.length - 1 - s] === after[after.length - 1 - s]) s++;
	if (p + s !== before.length) throw new Error(`not a pure insertion: old text changed at offset ${p}`);
	return after.slice(p, after.length - s);
}

describe.skipIf(!HAS_KICAD)("project ops", () => {
	let root: string;
	let dir: string;
	let proFile: string;
	let druFile: string;
	const opts = () => ({ cwd: dir, repoRoot: root });

	beforeEach(() => {
		root = stageFixture();
		dir = path.join(root, PROJECT);
		proFile = path.join(dir, "espion-cam.kicad_pro");
		druFile = path.join(dir, "espion-cam.kicad_dru");
	});
	afterEach(() => cleanup(root));

	test("set_netclass + set_netclass_patterns put matching nets in the new class", async () => {
		const pro0 = fs.readFileSync(proFile, "utf8");
		await apply(root, [{ op: "set_netclass", name: "IMU_I2C", track_width: 0.25, clearance: 0.15 }]);
		const pro1 = fs.readFileSync(proFile, "utf8");
		const added = inserted(pro0, pro1);
		expect(added).toContain('"name": "IMU_I2C"');
		expect(added).toContain('"track_width": 0.25');
		expect(added).toContain('"priority": 9');

		await apply(root, [{ op: "set_netclass_patterns", netclass: "IMU_I2C", patterns: ["/MCU/IMU_S*"] }]);
		expect(inserted(pro1, fs.readFileSync(proFile, "utf8"))).toContain('"pattern": "/MCU/IMU_S*"');

		const nl = await netlist(path.join(dir, "espion-cam.kicad_sch"), opts());
		const cls = new Map(nl.nets.map(n => [n.name, n.netclass]));
		expect(cls.get("/MCU/IMU_SCL")).toBe("IMU_I2C");
		expect(cls.get("/MCU/IMU_SDA")).toBe("IMU_I2C");
		expect(cls.get("/MCU/IMU_INT1")).toBe("Default");
	});

	test("set_netclass on an existing class changes only that setting", async () => {
		const pro0 = fs.readFileSync(proFile, "utf8");
		await apply(root, [{ op: "set_netclass", name: "USB", track_width: 0.25 }]);
		const a = pro0.split("\n");
		const b = fs.readFileSync(proFile, "utf8").split("\n");
		expect(b).toHaveLength(a.length);
		const diff = a.flatMap((l, i) => (l === b[i] ? [] : [[l.trim(), b[i]!.trim()]]));
		expect(diff).toEqual([['"track_width": 0.245,', '"track_width": 0.25,']]);
	});

	test("remove_netclass drops the class and its patterns", async () => {
		await apply(root, [{ op: "remove_netclass", name: "RMII_CLK" }]);
		const ns = JSON.parse(fs.readFileSync(proFile, "utf8")).net_settings;
		expect(ns.classes.map((c: { name: string }) => c.name)).not.toContain("RMII_CLK");
		expect(ns.netclass_patterns.map((p: { netclass: string }) => p.netclass)).not.toContain("RMII_CLK");
		const nl = await netlist(path.join(dir, "espion-cam.kicad_sch"), opts());
		expect(nl.nets.find(n => n.name === "/Ethernet/RMII_REF_CLK")?.netclass).toBe("Default");
		await expect(apply(root, [{ op: "remove_netclass", name: "Default" }])).rejects.toThrow();
	});

	test("set_bom_preset is used by kicad-cli, remove_bom_preset deletes it", async () => {
		const pro0 = fs.readFileSync(proFile, "utf8");
		await apply(root, [
			{
				op: "set_bom_preset",
				name: "Assembly",
				fields: [
					{ name: "Reference", label: "Designator" },
					{ name: "${QUANTITY}", label: "Qty" },
					{ name: "Value", group_by: true },
					{ name: "LCSC", label: "LCSC Part", group_by: true },
				],
				exclude_dnp: true,
			},
		]);
		expect(inserted(pro0, fs.readFileSync(proFile, "utf8"))).toContain('"name": "Assembly"');
		const csv = await bom(path.join(dir, "espion-cam.kicad_sch"), "Assembly", opts());
		const [header, ...rows] = csv.trim().split("\n");
		expect(header).toBe('"Designator","Qty","Value","LCSC Part"');
		expect(rows.some(r => r.includes('"C25744"'))).toBe(true);
		expect(rows.some(r => /"R36[",]/.test(r))).toBe(false);

		await apply(root, [{ op: "remove_bom_preset", name: "Assembly" }]);
		expect(fs.readFileSync(proFile, "utf8")).toBe(pro0);
	});

	test("set_rule replaces or appends one rule and keeps the rest byte for byte", async () => {
		const dru0 = fs.readFileSync(druFile, "utf8");
		const usb = dru0.indexOf('(rule "USB spacing"');
		const replacement = `(rule "USB spacing"\n\t(condition "A.NetClass == 'USB' && !A.isCoupledDiffPair()")\n\t(constraint clearance (min 0.35mm)))`;
		const added = `(rule "RMII spacing"\n\t(condition "A.NetClass == 'RMII'")\n\t(constraint clearance (min 0.25mm)))`;
		await apply(root, [
			{ op: "set_rule", rule: replacement },
			{ op: "set_rule", rule: added },
		]);
		const dru1 = fs.readFileSync(druFile, "utf8");
		expect(dru1).toBe(`${dru0.slice(0, usb)}${replacement}\n\n${added}\n`);
		expect(await druProblems(Project.open(PROJECT, root))).toEqual([]);
	});

	test("set_rule comment: above a new rule; replacing keeps it when omitted, replaces it when given, removes it with an empty one", async () => {
		const dru0 = fs.readFileSync(druFile, "utf8");
		const usbComment = dru0.indexOf("# USB 2.0");
		const usb = (min: string) => `(rule "USB spacing"\n\t(condition "A.NetClass == 'USB' && !A.isCoupledDiffPair()")\n\t(constraint clearance (min ${min})))`;
		const rmii = `(rule "RMII spacing"\n\t(condition "A.NetClass == 'RMII'")\n\t(constraint clearance (min 0.25mm)))`;
		const rmiiText = `# RMII: 2W from the 50 MHz reference clock\n# (LAN8720 layout guide)\n${rmii}\n`;
		const read = () => fs.readFileSync(druFile, "utf8");
		const comments = () => Object.fromEntries(druRules(read()).map(r => [text(r.node, 1), r.comment]));

		await apply(root, [{ op: "set_rule", rule: rmii, comment: "RMII: 2W from the 50 MHz reference clock\n(LAN8720 layout guide)" }]);
		expect(read()).toBe(`${dru0.trimEnd()}\n\n${rmiiText}`);

		await apply(root, [{ op: "set_rule", rule: usb("0.35mm") }]);
		expect(read()).toBe(`${dru0.slice(0, dru0.indexOf('(rule "USB spacing"'))}${usb("0.35mm")}\n\n${rmiiText}`);

		await apply(root, [{ op: "set_rule", rule: usb("0.4mm"), comment: "USB 2.0: 0.4 mm from the pair" }]);
		expect(read()).toBe(`${dru0.slice(0, usbComment)}# USB 2.0: 0.4 mm from the pair\n${usb("0.4mm")}\n\n${rmiiText}`);

		await apply(root, [{ op: "set_rule", rule: usb("0.4mm"), comment: "" }]);
		expect(read()).toBe(`${dru0.slice(0, usbComment)}${usb("0.4mm")}\n\n${rmiiText}`);

		const c = comments();
		expect(c["USB spacing"]).toBeUndefined();
		expect(c["RMII spacing"]).toBe("RMII: 2W from the 50 MHz reference clock\n(LAN8720 layout guide)");
		expect(c["MDI spacing"]).toBe("Ethernet MDI: 3x spacing to other nets (IP101G DS 6.2).");
		expect(c["PoE isolation"]?.split("\n")).toHaveLength(3);
		expect(await druProblems(Project.open(PROJECT, root))).toEqual([]);
	});

	test("remove_rule deletes the rule with its comment, other rules unchanged", async () => {
		const dru0 = fs.readFileSync(druFile, "utf8");
		const start = dru0.indexOf("# Ethernet MDI");
		const end = dru0.indexOf("# USB 2.0");
		await apply(root, [{ op: "remove_rule", name: "MDI spacing" }]);
		expect(fs.readFileSync(druFile, "utf8")).toBe(dru0.slice(0, start) + dru0.slice(end));
		expect(await druProblems(Project.open(PROJECT, root))).toEqual([]);
	});

	test("set_rule creates a missing rules file and rejects malformed rules", async () => {
		fs.rmSync(druFile);
		const rule = `(rule "USB spacing"\n\t(condition "A.NetClass == 'USB'")\n\t(constraint clearance (min 0.3mm)))`;
		await apply(root, [{ op: "set_rule", rule }]);
		expect(fs.readFileSync(druFile, "utf8")).toBe(`(version 1)\n\n${rule}\n`);
		expect(await druProblems(Project.open(PROJECT, root))).toEqual([]);
		await expect(apply(root, [{ op: "set_rule", rule: '(rule "x" (constraint clearence (min 1mm)))' }])).rejects.toThrow("clearence");
		await expect(apply(root, [{ op: "set_rule", rule: '(rule "x" (constraint clearance (min 1mm))' }])).rejects.toThrow();
	});
});
