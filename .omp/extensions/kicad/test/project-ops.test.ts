import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import { druProblems } from "../src/checks/rules";
import { bom, netlist } from "../src/cli";
import { onInterior, type Pt } from "../src/geom";
import { druHeader, druRules, memberTaps, ops } from "../src/ops/project";
import { ops as schematicOps } from "../src/ops/schematic";
import { Project } from "../src/project";
import { queries } from "../src/queries/project";
import { text } from "../src/sexpr";
import { validate } from "../src/spec";
import { runEdit } from "../src/txn";
import type { OpContext } from "../src/types";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

/** Apply ops (project and schematic) to a freshly opened project and write the pending files; returns the op notes. */
async function apply(root: string, list: Record<string, unknown>[]): Promise<string[]> {
	const project = Project.open(PROJECT, root);
	const ctx: OpContext = { project, libs: await project.libs(), notes: [], touch() {}, touchPart() {}, touchBoard() {} };
	for (const { op, ...args } of list) {
		const def = [...ops, ...schematicOps].find(o => o.op === op);
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

	test("set_rules_header replaces, removes and inserts the header; rules and their comments keep their bytes", async () => {
		const dru0 = fs.readFileSync(druFile, "utf8");
		const rulesText = dru0.slice(dru0.indexOf("# PoE isolation"));
		const read = () => fs.readFileSync(druFile, "utf8");
		const comments = () => druRules(read()).map(r => [text(r.node, 1), r.comment]);
		const comments0 = druRules(dru0).map(r => [text(r.node, 1), r.comment]);
		expect(druHeader(dru0)).toStartWith("Spacing rules from SCHEMATIC.md");

		expect(await apply(root, [{ op: "set_rules_header", comment: "Board spacing rules.\n\nWidths: net classes." }])).toEqual(["rules header replaced"]);
		expect(read()).toBe(`(version 1)\n\n# Board spacing rules.\n#\n# Widths: net classes.\n\n${rulesText}`);
		expect(druHeader(read())).toBe("Board spacing rules.\n\nWidths: net classes.");
		expect(comments()).toEqual(comments0);

		expect(await apply(root, [{ op: "set_rules_header", comment: "" }])).toEqual(["rules header removed"]);
		expect(read()).toBe(`(version 1)\n\n${rulesText}`);
		expect(druHeader(read())).toBeUndefined();
		expect(comments()).toEqual(comments0);

		expect(await apply(root, [{ op: "set_rules_header", comment: "Spacing rules." }])).toEqual(["rules header added"]);
		expect(read()).toBe(`(version 1)\n\n# Spacing rules.\n\n${rulesText}`);
		expect(comments()).toEqual(comments0);
		expect(await druProblems(Project.open(PROJECT, root))).toEqual([]);
	});

	test("set_rules_header inserted after (version N) leaves a rule comment right below it to the rule", async () => {
		const rule = `(rule "USB spacing"\n\t(condition "A.NetClass == 'USB'")\n\t(constraint clearance (min 0.3mm)))`;
		fs.writeFileSync(druFile, `(version 1)\n# USB 2.0: 0.3 mm from the pair\n${rule}\n`);
		expect(druHeader(fs.readFileSync(druFile, "utf8"))).toBeUndefined();
		await apply(root, [{ op: "set_rules_header", comment: "Spacing rules." }]);
		const dru = fs.readFileSync(druFile, "utf8");
		expect(dru).toBe(`(version 1)\n\n# Spacing rules.\n\n# USB 2.0: 0.3 mm from the pair\n${rule}\n`);
		expect(druHeader(dru)).toBe("Spacing rules.");
		expect(druRules(dru)[0]?.comment).toBe("USB 2.0: 0.3 mm from the pair");

		expect(await apply(root, [{ op: "set_rules_header", comment: "" }])).toEqual(["rules header removed"]);
		expect(fs.readFileSync(druFile, "utf8")).toBe(`(version 1)\n\n# USB 2.0: 0.3 mm from the pair\n${rule}\n`);

		fs.rmSync(druFile);
		await apply(root, [{ op: "set_rules_header", comment: "Spacing rules." }]);
		expect(fs.readFileSync(druFile, "utf8")).toBe("(version 1)\n\n# Spacing rules.\n");
		expect(druRules(fs.readFileSync(druFile, "utf8"))).toEqual([]);

		fs.writeFileSync(druFile, "\n");
		const notes = await apply(root, [{ op: "set_rules_header", comment: "Spacing rules." }]);
		expect(notes[0]).toStartWith("filled the empty");
		expect(fs.readFileSync(druFile, "utf8")).toBe("(version 1)\n\n# Spacing rules.\n");

		fs.writeFileSync(druFile, `# USB 2.0\n${rule}\n`);
		await expect(apply(root, [{ op: "set_rules_header", comment: "Spacing rules." }])).rejects.toThrow("no (version N)");
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

describe.skipIf(!HAS_KICAD)("bus alias ops", () => {
	let root: string;
	let proFile: string;
	const aliases = (): Record<string, string[]> => JSON.parse(fs.readFileSync(proFile, "utf8")).schematic.bus_aliases;
	const FIXTURE_ALIASES = ["CAMCTL", "CSI", "IMU", "LEDCTL", "PHY", "POE"];

	beforeEach(() => {
		root = stageFixture();
		proFile = path.join(root, PROJECT, "espion-cam.kicad_pro");
	});
	afterEach(() => cleanup(root));

	// biome-ignore lint/suspicious/noExplicitAny: query results are plain JSON, inspected by shape
	async function busAliasesQuery(): Promise<any> {
		const project = Project.open(PROJECT, root);
		const analysis = new Analysis(project);
		try {
			return await queries.find(d => d.what === "bus_aliases")!.run({ project, analysis, libs: () => project.libs() }, {});
		} finally {
			analysis.dispose();
		}
	}

	test("set_bus_alias creates an alias in KiCad's name order and replaces its members; the rest of the .kicad_pro keeps its bytes", async () => {
		const pro0 = fs.readFileSync(proFile, "utf8");
		expect(await apply(root, [{ op: "set_bus_alias", name: "AUX", members: ["AUX_A", "AUX_B"] }])).toEqual(["bus alias AUX: created with AUX_A, AUX_B"]);
		const pro1 = fs.readFileSync(proFile, "utf8");
		expect(inserted(pro0, pro1)).toBe('AUX": [\n        "AUX_A",\n        "AUX_B"\n      ],\n      "');
		expect(Object.keys(aliases())).toEqual(["AUX", ...FIXTURE_ALIASES]);

		expect(await apply(root, [{ op: "set_bus_alias", name: "AUX", members: ["AUX_B", "AUX_C"] }])).toEqual(["bus alias AUX: added AUX_C; removed AUX_A"]);
		expect(aliases().AUX).toEqual(["AUX_B", "AUX_C"]);
		expect(await apply(root, [{ op: "set_bus_alias", name: "AUX", members: ["AUX_C", "AUX_B"] }])).toEqual(["bus alias AUX: members reordered"]);
		expect(aliases().AUX).toEqual(["AUX_C", "AUX_B"]);
		await expect(apply(root, [{ op: "set_bus_alias", name: "AUX", members: ["AUX_C", "AUX_C"] }])).rejects.toThrow("members: AUX_C is given twice");
		await expect(apply(root, [{ op: "set_bus_alias", name: "AUX", members: [] }])).rejects.toThrow("members: give at least one member");

		// replacing an existing alias's members touches only its list
		const withoutAux = (s: string) => s.replace(/"AUX": \[[^\]]*\],\n {6}/, "");
		await apply(root, [{ op: "set_bus_alias", name: "IMU", members: ["IMU_SDA", "IMU_SCL", "IMU_INT1", "IMU_SPARE"] }]);
		expect(inserted(withoutAux(pro1), withoutAux(fs.readFileSync(proFile, "utf8")))).toBe(',\n        "IMU_SPARE"');
		// and the alias goes again without a trace (remove_bus_alias of an unused alias)
		expect(await apply(root, [{ op: "remove_bus_alias", name: "AUX" }])).toEqual(["bus alias AUX removed (members: AUX_C, AUX_B)"]);
		await apply(root, [{ op: "set_bus_alias", name: "IMU", members: ["IMU_SDA", "IMU_SCL", "IMU_INT1"] }]);
		expect(fs.readFileSync(proFile, "utf8")).toBe(pro0);
	});

	test("set_bus_alias adding and removing an unused member passes the transaction's gates with the connectivity unchanged", async () => {
		const pro0 = fs.readFileSync(proFile, "utf8");
		const add = await runEdit({ ops: [{ op: "set_bus_alias", name: "IMU", members: ["IMU_SDA", "IMU_SCL", "IMU_INT1", "IMU_SPARE"] }] }, root, undefined);
		expect(add.report).toStartWith("Applied.");
		expect(add.report).toContain("bus alias IMU: added IMU_SPARE");
		expect(aliases().IMU).toEqual(["IMU_SDA", "IMU_SCL", "IMU_INT1", "IMU_SPARE"]);
		// no `expect`: any connectivity change would refuse the edit
		const remove = await runEdit({ ops: [{ op: "set_bus_alias", name: "IMU", members: ["IMU_SDA", "IMU_SCL", "IMU_INT1"] }] }, root, undefined);
		expect(remove.report).toStartWith("Applied.");
		expect(remove.report).toContain("bus alias IMU: removed IMU_SPARE");
		expect(remove.report).toContain("Connectivity and part changes:\n  - none");
		expect(fs.readFileSync(proFile, "utf8")).toBe(pro0);
	});

	test("set_bus_alias refuses to remove a member a local label still takes off a bus carrying the alias, naming sheet and label", async () => {
		const pro0 = fs.readFileSync(proFile, "utf8");
		const drop = { op: "set_bus_alias", name: "IMU", members: ["IMU_SDA", "IMU_SCL"] };
		await expect(apply(root, [drop])).rejects.toThrow(
			"bus alias IMU: can't remove IMU_INT1, still taken off a {IMU} bus by local label IMU_INT1 on MCU at (199.39, 152.4); local label IMU_INT1 on IMU at (124.46, 119.38): remove that label and its bus entry first",
		);
		expect(fs.readFileSync(proFile, "utf8")).toBe(pro0);
		// the labels on the bus entries' wires block it, not the IMU_INT1 label at the IMU's pin (170.18, 93.98) on a sheet using the alias
		await apply(root, [
			{ op: "remove_label", sheet: "MCU", name: "IMU_INT1" },
			{ op: "remove_label", sheet: "IMU", name: "IMU_INT1", at: [124.46, 119.38] },
		]);
		expect(await apply(root, [drop])).toEqual(["bus alias IMU: removed IMU_INT1"]);
		expect(aliases().IMU).toEqual(["IMU_SDA", "IMU_SCL"]);
	});

	test("remove_bus_alias is refused while a label or sheet pin names the alias, also in a group, and allowed when none does", async () => {
		await expect(apply(root, [{ op: "remove_bus_alias", name: "IMU" }])).rejects.toThrow(
			"bus alias IMU is still used: sheet pin {IMU} of sheet block MCU on root at (167.64, 106.68); sheet pin {IMU} of sheet block IMU on root at (215.9, 144.78); ",
		);
		await expect(apply(root, [{ op: "remove_bus_alias", name: "IMU" }])).rejects.toThrow("hierarchical label {IMU} on IMU at (119.38, 104.14)");
		await expect(apply(root, [{ op: "remove_bus_alias", name: "NOPE" }])).rejects.toThrow('no bus alias "NOPE"; aliases: CAMCTL, CSI, IMU, LEDCTL, PHY, POE');

		const pro0 = fs.readFileSync(proFile, "utf8");
		await apply(root, [
			{ op: "set_bus_alias", name: "AUX", members: ["AUX_A"] },
			{ op: "add_label", sheet: "root", name: "{IMU AUX}", at: [27.94, 27.94] },
		]);
		await expect(apply(root, [{ op: "remove_bus_alias", name: "AUX" }])).rejects.toThrow("bus alias AUX is still used: bus label {IMU AUX} on root at (27.94, 27.94); remove those first");
		await apply(root, [{ op: "remove_label", sheet: "root", name: "{IMU AUX}" }]);
		await apply(root, [{ op: "remove_bus_alias", name: "AUX" }]);
		expect(fs.readFileSync(proFile, "utf8")).toBe(pro0);
	});

	test("the bus_aliases query lists members, uses and unused members", async () => {
		const before = await busAliasesQuery();
		expect(before.aliases.map((a: { name: string }) => a.name)).toEqual(FIXTURE_ALIASES);
		expect(before.aliases.find((a: { name: string }) => a.name === "IMU")).toEqual({
			name: "IMU",
			members: ["IMU_SDA", "IMU_SCL", "IMU_INT1"],
			uses: [
				{ sheet: "root", kind: "sheet pin", text: "{IMU}", at: [167.64, 106.68], block: "MCU" },
				{ sheet: "root", kind: "sheet pin", text: "{IMU}", at: [215.9, 144.78], block: "IMU" },
				{ sheet: "MCU", kind: "bus label", text: "{IMU}", at: [114.3, 99.06] },
				{ sheet: "MCU", kind: "bus label", text: "{IMU}", at: [205.74, 157.48] },
				{ sheet: "MCU", kind: "hierarchical label", text: "{IMU}", at: [114.3, 101.6] },
				{ sheet: "IMU", kind: "hierarchical label", text: "{IMU}", at: [119.38, 104.14] },
			],
			unused_members: [],
		});
		expect(before.aliases.every((a: { unused_members: string[] }) => a.unused_members.length === 0)).toBe(true);

		await apply(root, [
			{ op: "set_bus_alias", name: "IMU", members: ["IMU_SDA", "IMU_SCL", "IMU_INT1", "IMU_SPARE"] },
			{ op: "set_bus_alias", name: "AUX", members: ["AUX_A", "AUX_B"] },
			// a label of the member's name on a sheet using the alias, but on no wire from its bus, doesn't use the member
			{ op: "add_label", sheet: "IMU", name: "IMU_SPARE", at: [27.94, 27.94] },
		]);
		const after = await busAliasesQuery();
		expect(after.aliases.find((a: { name: string }) => a.name === "IMU").unused_members).toEqual(["IMU_SPARE"]);
		expect(after.aliases.find((a: { name: string }) => a.name === "AUX")).toEqual({ name: "AUX", members: ["AUX_A", "AUX_B"], uses: [], unused_members: ["AUX_A", "AUX_B"], used_nowhere: true });

		// a branch ending on the middle of the root's {IMU} bus carries the alias: a tap on it uses the member
		await apply(root, [
			{ op: "add_bus", sheet: "root", points: [[187.96, 127], [177.8, 127]] },
			{ op: "add_bus_entry", sheet: "root", at: [180.34, 127], dir: "DL" },
			{ op: "add_wire", sheet: "root", points: [[177.8, 129.54], [170.18, 129.54]] },
			{ op: "add_label", sheet: "root", name: "IMU_SPARE", at: [175.26, 129.54], rot: 180 },
		]);
		expect(Project.open(PROJECT, root).sheet("root").sch.buses().some(s => onInterior([187.96, 127], s))).toBe(true);
		expect((await busAliasesQuery()).aliases.find((a: { name: string }) => a.name === "IMU").unused_members).toEqual([]);
	});

	test("a vector member is in use while a local label takes one of its bits off the bus", async () => {
		const imu = async () => (await busAliasesQuery()).aliases.find((a: { name: string }) => a.name === "IMU");
		// the tapped IMU_INT1 becomes a bit of IMU_INT[0..1]: the vector covers its label, so the swap is allowed
		const vector = ["IMU_SDA", "IMU_SCL", "IMU_INT[0..1]"];
		expect(await apply(root, [{ op: "set_bus_alias", name: "IMU", members: vector }])).toEqual(["bus alias IMU: added IMU_INT[0..1]; removed IMU_INT1"]);
		expect((await imu()).unused_members).toEqual([]);
		const drop = { op: "set_bus_alias", name: "IMU", members: ["IMU_SDA", "IMU_SCL"] };
		await expect(apply(root, [drop])).rejects.toThrow(
			"bus alias IMU: can't remove IMU_INT[0..1], still taken off a {IMU} bus by local label IMU_INT1 on MCU at (199.39, 152.4); local label IMU_INT1 on IMU at (124.46, 119.38): remove that label and its bus entry first",
		);
		await apply(root, [
			{ op: "remove_label", sheet: "MCU", name: "IMU_INT1" },
			{ op: "remove_label", sheet: "IMU", name: "IMU_INT1", at: [124.46, 119.38] },
		]);
		expect((await imu()).unused_members).toEqual(["IMU_INT[0..1]"]);
		expect(await apply(root, [drop])).toEqual(["bus alias IMU: removed IMU_INT[0..1]"]);

		// bits in either direction, the text after "]" kept, no leading zeros
		const taps = ["D0_N", "D1_N", "D3_N", "D4_N", "D01_N", "D1"].map(net => ({ sheet: "root", net, label: net, at: [0, 0] as Pt }));
		expect(memberTaps("D[3..1]_N", taps).map(t => t.net)).toEqual(["D1_N", "D3_N"]);
	});
});
