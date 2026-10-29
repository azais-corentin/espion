import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import * as cli from "../src/cli";
import { touches } from "../src/geom";
import { blockReason } from "../src/hook";
import { checkExpect, diffNetlists } from "../src/netdiff";
import { Project } from "../src/project";
import { parsePro, stringifyPro } from "../src/projson";
import { inline, L, parse, print, Str, str, Sym } from "../src/sexpr";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

describe("sexpr", () => {
	test("quoting follows the atom type, not the content", () => {
		const t = parse('(property "yes" yes "with \\"quotes\\" and \\\\" 1.27)');
		expect(t[1]).toBeInstanceOf(Str);
		expect(t[2]).toBeInstanceOf(Sym);
		expect((t[3] as Str).v).toBe('with "quotes" and \\');
		expect(inline(t)).toBe('(property "yes" yes "with \\"quotes\\" and \\\\" 1.27)');
	});

	test("builder: numbers are formatted, strings must be explicit", () => {
		expect(inline(L("at", 1.27, -0, 90.00001, true))).toBe("(at 1.27 0 90 yes)");
		expect(inline(L("property", str("Value"), str("10k")))).toBe('(property "Value" "10k")');
		expect(() => L("property", "Value with space")).toThrow();
		expect(inline(L("pin", "passive", "line"))).toBe("(pin passive line)");
	});

	test("unbalanced input is rejected", () => {
		expect(() => parse("(a (b)")).toThrow();
		expect(() => parse("(a))")).toThrow();
		expect(() => parse('(a "open)')).toThrow();
	});
});

describe("fixture project", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));
	test.skipIf(!HAS_KICAD)("printing every sheet and re-saving with KiCad reproduces it byte for byte", async () => {
		const p = Project.open(PROJECT, root);
		const files = fs.readdirSync(p.dir).filter(f => f.endsWith(".kicad_sch"));
		expect(files.length).toBe(8);
		// KiCad needs the .kicad_pro next to the sheet, or it drops the project name from instances
		const dir = path.join(cli.tmpDir("rt"), "p");
		fs.cpSync(p.dir, dir, { recursive: true });
		for (const f of files) {
			const orig = fs.readFileSync(path.join(p.dir, f), "utf8");
			fs.writeFileSync(path.join(dir, f), `${print(parse(orig))}\n`);
			await cli.upgrade("sch", path.join(dir, f), { cwd: dir, repoRoot: root });
			expect(fs.readFileSync(path.join(dir, f), "utf8")).toBe(orig);
		}
		fs.rmSync(path.dirname(dir), { recursive: true, force: true });
	});

	test(".kicad_pro round-trips byte for byte, doubles keep their decimal point", () => {
		const file = path.join(root, PROJECT, "espion-cam.kicad_pro");
		const src = fs.readFileSync(file, "utf8");
		const pro = parsePro(src);
		expect(stringifyPro(pro)).toBe(src);
		const classes = (pro.data.net_settings as { classes: { name: string; track_width: number }[] }).classes;
		classes[0]!.track_width = 1;
		expect(stringifyPro(pro)).toContain('"track_width": 1.0');
	});

	test.skipIf(!HAS_KICAD)("every connected pin sits on a connection point of its sheet", async () => {
		const p = Project.open(PROJECT, root);
		const nl = await cli.netlist(p.rootFile, { cwd: p.dir, repoRoot: root });
		const connected = new Set(nl.nets.filter(n => n.nodes.length > 1).flatMap(n => n.nodes.map(x => `${x.ref}.${x.pin}`)));
		const poses = new Set<string>();
		let checked = 0;
		for (const s of p.hierarchy()) {
			const wires = s.sch.wires();
			const points = s.sch.connectionPoints();
			for (const sym of s.sch.symbols()) {
				const ref = s.sch.refOf(sym, s.path);
				const pose = s.sch.pose(sym);
				for (const pin of s.sch.pins(sym)) {
					if (!connected.has(`${ref}.${pin.num}`)) continue;
					checked++;
					poses.add(`${pose.rot}${pose.mirror ?? ""}`);
					const onWire = wires.some(w => touches(pin.at, w));
					const shared = (points.get(`${pin.at[0]},${pin.at[1]}`)?.length ?? 0) > 1;
					expect({ ref, pin: pin.num, ok: onWire || shared }).toEqual({ ref, pin: pin.num, ok: true });
				}
			}
		}
		expect(checked).toBeGreaterThan(400);
		// rotations and the mirror present in the fixture all went through the transform
		for (const p of ["0", "90", "180", "270", "0y"]) expect(poses.has(p)).toBe(true);
	});

	test("sheets resolve by name, file and name path", () => {
		const p = Project.open(PROJECT, root);
		expect(p.sheet("Power").file).toBe(p.sheet("power.kicad_sch").file);
		expect(p.sheet("/LED driver/").name).toBe("LED driver");
		expect(p.sheet("root").namePath).toBe("/");
		expect(() => p.sheet("Nope")).toThrow(/sheets: /);
	});
});

describe("netdiff", () => {
	const nl = (nets: [string, string[]][], refs: string[]) => ({
		nets: nets.map(([name, pins], i) => ({
			code: i + 1,
			name,
			nodes: pins.map(p => ({ ref: p.split(".")[0]!, pin: p.split(".")[1]! })),
		})),
		components: new Map(
			refs.map(r => [
				r,
				{
					ref: r,
					value: "",
					footprint: "",
					datasheet: "",
					description: "",
					lib: "",
					part: "",
					sheetName: "/",
					sheetFile: "",
					fields: {},
					dnp: false,
					excludeFromBom: false,
					excludeFromBoard: false,
				},
			]),
		),
	});

	test("an unchanged netlist needs no expect", () => {
		const a = nl([["/A", ["R1.1", "R2.1"]]], ["R1", "R2"]);
		expect(checkExpect(diffNetlists(a, a), {})).toEqual([]);
	});

	test("a renamed net must be declared; automatic names are not renames", () => {
		const a = nl([["/A", ["R1.1", "R2.1"]], ["Net-(R1-Pad2)", ["R1.2", "R3.1"]]], ["R1", "R2", "R3"]);
		const b = nl([["/B", ["R1.1", "R2.1"]], ["Net-(R3-Pad1)", ["R1.2", "R3.1"]]], ["R1", "R2", "R3"]);
		const d = diffNetlists(a, b);
		expect(checkExpect(d, {}).length).toBe(1);
		expect(checkExpect(d, { renamed_nets: [{ from: "A", to: "B" }] })).toEqual([]);
	});

	test("a new part joined to a net must be declared with the exact final pins", () => {
		const a = nl([["/A", ["R1.1", "R2.1"]]], ["R1", "R2"]);
		const b = nl([["/A", ["R1.1", "R2.1", "R9.1"]]], ["R1", "R2", "R9"]);
		const d = diffNetlists(a, b);
		expect(checkExpect(d, { added_parts: ["R9"] }).length).toBe(1);
		expect(checkExpect(d, { added_parts: ["R9"], nets: [{ name: "A", pins: ["R1.1", "R2.1"] }] }).length).toBe(2);
		expect(checkExpect(d, { added_parts: ["R9"], nets: [{ name: "A", pins: ["R9.1", "R1.1", "R2.1"] }] })).toEqual([]);
	});

	test("a big net can be declared as a delta of its pins", () => {
		const gnd = Array.from({ length: 50 }, (_, i) => `C${i + 1}.2`);
		const a = nl([["GND", gnd]], gnd.map(p => p.split(".")[0]!));
		const b = nl([["GND", [...gnd, "R9.2"]]], [...gnd.map(p => p.split(".")[0]!), "R9"]);
		const d = diffNetlists(a, b);
		expect(checkExpect(d, { added_parts: ["R9"], nets: [{ name: "GND", added: ["R9.2"] }] })).toEqual([]);
		expect(checkExpect(d, { added_parts: ["R9"], nets: [{ name: "GND", added: ["R9.1"] }] }).length).toBeGreaterThan(0);
		expect(checkExpect(d, { added_parts: ["R9"], nets: [{ added: ["R9.2"] }] })).toContain("an expect.nets entry without `pins` needs `name`");
	});

	test("pins left on no net must be declared", () => {
		const a = nl([["/A", ["R1.1", "R2.1", "R3.1"]]], ["R1", "R2", "R3"]);
		const b = nl([["/A", ["R1.1", "R2.1"]]], ["R1", "R2", "R3"]);
		const d = diffNetlists(a, b);
		expect(d.disconnected).toEqual(["R3.1"]);
		expect(checkExpect(d, { nets: [{ pins: ["R1.1", "R2.1"] }] }).length).toBe(1);
		expect(checkExpect(d, { nets: [{ pins: ["R1.1", "R2.1"] }], unconnected: ["R3.1"] })).toEqual([]);
	});
});

describe("hook", () => {
	const blocked = (tool: string, input: Record<string, unknown>) => blockReason(tool, input) !== undefined;

	test("file tools are blocked on KiCad files only", () => {
		expect(blocked("write", { path: "hardware/espion-cam/power.kicad_sch" })).toBe(true);
		expect(blocked("write", { path: "hardware/espion-cam/espion-cam.kicad_dru" })).toBe(true);
		expect(blocked("write", { path: "hardware/espion-cam/sym-lib-table" })).toBe(true);
		expect(blocked("write", { path: "hardware/lib/Foo.pretty/Bar.kicad_mod" })).toBe(true);
		expect(blocked("edit", { input: "[hardware/espion-cam/mcu.kicad_sch#1A2B]\nPUT 1.=1:\n+x" })).toBe(true);
		expect(blocked("edit", { path: "hardware/espion-cam/SCHEMATIC.md", input: "[hardware/espion-cam/SCHEMATIC.md#1A2B]\nPUT 1.=1:\n+see power.kicad_sch" })).toBe(false);
		expect(blocked("write", { path: "docs/notes.md" })).toBe(false);
	});

	test("reading stays allowed", () => {
		expect(blocked("read", { path: "hardware/espion-cam/power.kicad_sch" })).toBe(false);
		expect(blocked("grep", { pattern: "x", path: "hardware/espion-cam/power.kicad_sch" })).toBe(false);
		expect(blocked("bash", { command: "git diff hardware/espion-cam/power.kicad_sch" })).toBe(false);
		expect(blocked("bash", { command: "wc -l hardware/espion-cam/*.kicad_sch" })).toBe(false);
	});

	test("KiCad programs and shell writes to KiCad files are blocked", () => {
		expect(blocked("bash", { command: "kicad-cli sch erc hardware/espion-cam/espion-cam.kicad_sch" })).toBe(true);
		expect(blocked("bash", { command: "cd hardware/espion-cam && kicad-cli sch export netlist x" })).toBe(true);
		expect(blocked("bash", { command: "nix develop -c kicad-cli version" })).toBe(true);
		expect(blocked("bash", { command: "sed -i s/10k/4k7/ hardware/espion-cam/power.kicad_sch" })).toBe(true);
		expect(blocked("bash", { command: "echo x > hardware/espion-cam/power.kicad_sch" })).toBe(true);
		expect(blocked("bash", { command: "cp /tmp/a.kicad_sch hardware/espion-cam/" })).toBe(true);
		expect(blocked("bash", { command: "python3 fix.py hardware/espion-cam/mcu.kicad_sch" })).toBe(true);
		expect(blocked("bash", { command: "bun test test/core.test.ts" })).toBe(false);
	});

	test("eval cells touching KiCad files or programs are blocked", () => {
		expect(blocked("eval", { code: "open('hardware/espion-cam/power.kicad_sch').read()" })).toBe(true);
		expect(blocked("eval", { code: "import pcbnew" })).toBe(true);
		expect(blocked("eval", { code: "print(1 + 1)" })).toBe(false);
	});

	test("toolkit test fixtures are exempt", () => {
		expect(blocked("write", { path: ".omp/extensions/kicad/test/fixtures/a.kicad_sch" })).toBe(false);
	});
});
