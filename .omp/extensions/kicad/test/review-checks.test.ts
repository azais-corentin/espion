/**
 * decoupling, pulls, bom_lines and the erc exclusion rule on small hand-built netlists and ERC
 * reports (the checks only read KiCad's netlist / ERC report and the project settings).
 */
import { describe, expect, test } from "bun:test";
import type { Analysis } from "../src/analysis";
import type { Component, ErcViolation, Net, Netlist } from "../src/cli";
import { parseValue } from "../src/checks/parts";
import { DEFAULTS, type ProjectConfig } from "../src/config";
import type { Project, SheetInst } from "../src/project";
import { CHECKS } from "../src/registry";
import type { Finding } from "../src/types";

function part(ref: string, value: string, sheet = "/MCU/", extra: Partial<Component> = {}): Component {
	return {
		ref,
		value,
		footprint: "",
		datasheet: "",
		description: "",
		lib: "",
		part: "",
		sheetName: sheet,
		sheetFile: "",
		fields: {},
		dnp: false,
		excludeFromBom: false,
		excludeFromBoard: false,
		...extra,
	};
}

/** A net from "REF.pin:type" nodes; the type defaults to passive. */
function net(name: string, ...nodes: string[]): Net {
	return {
		code: 0,
		name,
		nodes: nodes.map(n => {
			const [pinRef, pintype = "passive"] = n.split(":");
			const [ref, pin] = pinRef!.split(".");
			return { ref: ref!, pin: pin!, pintype, pinfunction: pintype === "power_in" ? "VDD" : undefined };
		}),
	};
}

interface Run {
	parts: Component[];
	nets?: Net[];
	settings?: Partial<ProjectConfig>;
	sheets?: string[];
	erc?: ErcViolation[];
}

function run(check: string, { parts, nets = [], settings = {}, sheets, erc = [] }: Run): Promise<Finding[]> {
	const netlist: Netlist = { nets, components: new Map(parts.map(c => [c.ref, c])) };
	return CHECKS.get(check)!.run({
		project: { settings: { ...DEFAULTS, ...settings } } as unknown as Project,
		analysis: { netlist: async () => netlist, erc: async () => erc } as unknown as Analysis,
		sheets: sheets?.map(namePath => ({ namePath }) as SheetInst),
		baseline: "HEAD",
		args: {},
	});
}

const keys = (f: Finding[]) => f.map(x => `${x.severity} ${x.key}`).sort();

describe("decoupling", () => {
	const ic = part("U1", "MCU");

	test("a capacitor from the pin's net to ground on the same sheet satisfies it", async () => {
		const f = await run("decoupling", {
			parts: [ic, part("C1", "100n"), part("#PWR01", "+3V3")],
			nets: [net("+3V3", "U1.1:power_in", "C1.1", "#PWR01.1:power_in"), net("GND", "U1.2:power_in", "C1.2")],
		});
		expect(f).toEqual([]);
	});

	test("no capacitor: a warning per part and net, naming the power pins", async () => {
		const f = await run("decoupling", {
			parts: [ic],
			nets: [net("+3V3", "U1.1:power_in", "U1.5:power_in"), net("GND", "U1.2:power_in")],
		});
		expect(keys(f)).toEqual(["warning decoupling|U1|+3V3"]);
		expect(f[0]!.message).toContain("U1.1 (VDD), U1.5 (VDD)");
	});

	test("the capacitor must sit on the part's sheet", async () => {
		const f = await run("decoupling", {
			parts: [ic, part("C1", "100n", "/Power/")],
			nets: [net("+3V3", "U1.1:power_in", "C1.1"), net("GND", "C1.2")],
		});
		expect(keys(f)).toEqual(["warning decoupling|U1|+3V3"]);
	});

	test("ground nets: default patterns on the leaf name, and the groundNets setting", async () => {
		for (const gnd of ["GND", "/Power/AGND", "GNDA", "VSS"]) {
			const f = await run("decoupling", {
				parts: [ic, part("C1", "100n")],
				nets: [net("+3V3", "U1.1:power_in", "C1.1"), net(gnd, "C1.2", "U1.2:power_in")],
			});
			expect(f).toEqual([]);
		}
		const nets = [net("+3V3", "U1.1:power_in", "C1.1"), net("0V", "C1.2")];
		expect(keys(await run("decoupling", { parts: [ic, part("C1", "100n")], nets }))).toEqual(["warning decoupling|U1|+3V3"]);
		expect(await run("decoupling", { parts: [ic, part("C1", "100n")], nets, settings: { groundNets: ["0V"] } })).toEqual([]);
	});

	test("a capacitor between two rails does not count", async () => {
		const f = await run("decoupling", {
			parts: [ic, part("C1", "100n")],
			nets: [net("+3V3", "U1.1:power_in", "C1.1"), net("+5V", "C1.2")],
		});
		expect(keys(f)).toEqual(["warning decoupling|U1|+3V3"]);
	});

	test("DNP capacitors don't count; DNP and off-board parts and power symbols need none", async () => {
		const f = await run("decoupling", {
			parts: [ic, part("C1", "100n", "/MCU/", { dnp: true }), part("U2", "x", "/MCU/", { dnp: true }), part("U3", "x", "/MCU/", { excludeFromBoard: true }), part("#PWR01", "+3V3")],
			nets: [net("+3V3", "U1.1:power_in", "U2.1:power_in", "U3.1:power_in", "#PWR01.1:power_in", "C1.1"), net("GND", "C1.2")],
		});
		expect(keys(f)).toEqual(["warning decoupling|U1|+3V3"]);
	});

	test("decouplingExempt (REF or REF.pin) turns the warning into info with the reason", async () => {
		const nets = [net("+3V3", "U1.1:power_in", "U1.5:power_in")];
		const byRef = await run("decoupling", { parts: [ic], nets, settings: { decouplingExempt: { U1: "module has its own" } } });
		expect(byRef.map(x => x.severity)).toEqual(["info"]);
		expect(byRef[0]!.message).toContain("module has its own");
		const byPin = await run("decoupling", { parts: [ic], nets, settings: { decouplingExempt: { "U1.5": "sense input" } } });
		expect(byPin.map(x => [x.severity, x.message.includes("sense input")]).sort()).toEqual([
			["info", true],
			["warning", false],
		]);
		expect(byPin.find(x => x.severity === "warning")!.message).not.toContain("U1.5");
	});

	test("only parts on the checked sheets", async () => {
		const f = await run("decoupling", { parts: [ic], nets: [net("+3V3", "U1.1:power_in")], sheets: ["/Power/"] });
		expect(f).toEqual([]);
	});
});

describe("pulls", () => {
	// U9 powers +3V3 and +1V8, so both are rails
	const supply = part("U9", "LDO");
	const rails = [net("+3V3", "U9.1:power_out", "R1.1", "R2.1"), net("+1V8", "U9.2:power_out", "R3.1"), net("GND", "U9.3:power_in", "R4.2")];
	const rs = ["R1", "R2", "R3", "R4"].map(r => part(r, "10k"));

	test("open collector: a resistor to a rail satisfies it, none is a warning", async () => {
		const parts = [supply, ...rs, part("U2", "sensor")];
		expect(await run("pulls", { parts, nets: [...rails, net("/MCU/INT", "U2.4:open_collector", "R1.2")] })).toEqual([]);
		const f = await run("pulls", { parts, nets: [...rails, net("/MCU/INT", "U2.4:open_collector")] });
		expect(keys(f)).toEqual(["warning pulls|pullup|/MCU/INT"]);
		expect(f[0]!.message).toContain("U2.4");
	});

	test("a resistor to a net without power pins is not a pull-up", async () => {
		const f = await run("pulls", {
			parts: [part("U2", "sensor"), part("R1", "10k"), part("U3", "x")],
			nets: [net("/MCU/INT", "U2.4:open_collector", "R1.2"), net("/MCU/OTHER", "R1.1", "U3.1:input")],
		});
		expect(keys(f)).toEqual(["warning pulls|pullup|/MCU/INT"]);
	});

	test("open emitter needs a resistor to ground", async () => {
		const parts = [supply, ...rs, part("U2", "sensor")];
		expect(await run("pulls", { parts, nets: [...rails, net("/MCU/OUT", "U2.4:open_emitter", "R4.1")] })).toEqual([]);
		const f = await run("pulls", { parts, nets: [...rails, net("/MCU/OUT", "U2.4:open_emitter", "R1.2")] });
		expect(keys(f)).toEqual(["warning pulls|pulldown|/MCU/OUT"]);
	});

	test("pullUpNets: I²C and MDIO nets need a pull-up; a DNP resistor doesn't count", async () => {
		const parts = [supply, ...rs, part("U2", "sensor"), part("R5", "10k", "/MCU/", { dnp: true })];
		const pulled = [net("/MCU/I2C_SDA", "U2.1:bidirectional", "R1.2"), net("/MCU/I2C_SCL", "U2.2:input", "R2.2")];
		expect(await run("pulls", { parts, nets: [...rails, ...pulled] })).toEqual([]);
		const bare = [net("+3V3", "U9.1:power_out", "R5.1"), net("/MCU/I2C_SDA", "U2.1:bidirectional", "R5.2"), net("/Ethernet/MDIO", "U2.3:bidirectional"), net("/MCU/GPIO1", "U2.5:bidirectional")];
		expect(keys(await run("pulls", { parts, nets: bare }))).toEqual([
			"warning pulls|pullup|/Ethernet/MDIO",
			"warning pulls|pullup|/MCU/I2C_SDA",
		]);
	});

	test("an I²C pair pulled up to different rails is a warning", async () => {
		const parts = [supply, ...rs, part("U2", "sensor")];
		const same = await run("pulls", {
			parts,
			nets: [...rails, net("/MCU/I2C_SDA", "U2.1:bidirectional", "R1.2"), net("/MCU/I2C_SCL", "U2.2:input", "R2.2")],
		});
		expect(same).toEqual([]);
		const split = await run("pulls", {
			parts,
			nets: [...rails, net("/MCU/I2C_SDA", "U2.1:bidirectional", "R1.2"), net("/MCU/I2C_SCL", "U2.2:input", "R3.2")],
		});
		expect(keys(split)).toEqual(["warning pulls|i2c_rails|/MCU/I2C_SDA|/MCU/I2C_SCL"]);
		expect(split[0]!.message).toContain("+1V8 (R3)");
	});

	test("pullsExempt (net name or leaf) turns the warnings into info with the reason", async () => {
		const parts = [supply, ...rs, part("U2", "sensor")];
		const nets = [...rails, net("/MCU/I2C_SDA", "U2.1:bidirectional"), net("/MCU/INT", "U2.4:open_collector")];
		const f = await run("pulls", { parts, nets, settings: { pullsExempt: { I2C_SDA: "pulled up on the module", "/MCU/INT": "internal pull-up" } } });
		expect(keys(f)).toEqual(["info pulls|pullup|/MCU/I2C_SDA", "info pulls|pullup|/MCU/INT"]);
		expect(f.find(x => x.net === "/MCU/I2C_SDA")!.message).toContain("pulled up on the module");
		expect(f.find(x => x.net === "/MCU/INT")!.message).toContain("internal pull-up");
	});
});

describe("erc exclusions", () => {
	const v = (type: string, extra: Partial<ErcViolation>): ErcViolation => ({
		type,
		severity: "error",
		description: "d",
		sheet: "/",
		items: [{ description: type }],
		...extra,
	});

	test("excluded with a comment is info, without one a warning; others keep KiCad's severity", async () => {
		const f = await run("erc", {
			parts: [],
			erc: [v("a", { excluded: true, comment: "on purpose" }), v("b", { excluded: true }), v("c", { excluded: true, comment: "  " }), v("d", {})],
		});
		expect(f.map(x => [x.key, x.severity]).sort()).toEqual([
			["erc|a|a", "info"],
			["erc|b|b", "warning"],
			["erc|c|c", "warning"],
			["erc|d|d", "error"],
		]);
		expect(f.find(x => x.key === "erc|a|a")!.message).toContain("on purpose");
		expect(f.find(x => x.key === "erc|b|b")!.message).toContain("without a reason");
	});
});

describe("bom_lines", () => {
	const bom = (ref: string, value: string, mpn: string, footprint = "R_0402", extra: Partial<Component> = {}) =>
		part(ref, value, "/MCU/", { footprint, fields: { MPN: mpn }, ...extra });

	test("value normalization", () => {
		for (const v of ["4k7", "4.7k", "4700", "4.7kΩ", "4K7 1%", "4.7 kohm"]) expect(parseValue(v)).toBe(4700);
		for (const v of ["100n", "100nF", "0.1u", "0.1µF 50V X7R", "100 nF"]) expect(parseValue(v)).toBe(1e-7);
		expect(parseValue("1R5")).toBe(1.5);
		expect(parseValue("10uH")).toBe(1e-5);
		expect(parseValue("1M")).toBe(1e6);
		expect(parseValue("TS-1187A")).toBeUndefined();
	});

	test("one MPN with different values is an error; equivalent notations are the same value", async () => {
		expect(await run("bom_lines", { parts: [bom("R1", "4k7", "X"), bom("R2", "4.7k", "X"), bom("C1", "100n", "Y"), bom("C2", "0.1uF 16V", "Y")] })).toEqual([]);
		const f = await run("bom_lines", { parts: [bom("R1", "4k7", "X"), bom("R2", "22k", "X")] });
		expect(keys(f)).toEqual(["error bom_lines|values|X"]);
		expect(f[0]!.refs).toEqual(["R1", "R2"]);
	});

	test("one MPN with different values on other parts (Value names the function), or unparsed R/C/L values, is info", async () => {
		const f = await run("bom_lines", { parts: [bom("SW1", "RESET", "TS-1187A", "SW_SPST"), bom("SW2", "BOOT", "TS-1187A", "SW_SPST")] });
		expect(keys(f)).toEqual(["info bom_lines|values|TS-1187A"]);
		expect(f[0]!.message).toContain("RESET (SW1); BOOT (SW2)");
		expect(keys(await run("bom_lines", { parts: [bom("R1", "4k7", "X"), bom("R2", "jumper", "X")] }))).toEqual(["info bom_lines|values|X"]);
	});

	test("same value and footprint with different MPNs is a warning; other footprints are separate lines", async () => {
		expect(await run("bom_lines", { parts: [bom("R1", "4k7", "X"), bom("R2", "4.7k", "Y", "R_0603")] })).toEqual([]);
		const f = await run("bom_lines", { parts: [bom("R1", "4k7", "X"), bom("R2", "4.7k", "Y"), bom("R3", "4700", "Y")] });
		expect(keys(f)).toEqual(["warning bom_lines|mpns|R|R_0402|4700"]);
		expect(f[0]!.message).toContain("X (R1); Y (R2, R3)");
	});

	test("LCSC stands in for an empty MPN; parts with neither, DNP and BOM-excluded parts are skipped", async () => {
		const f = await run("bom_lines", {
			parts: [
				bom("R1", "4k7", "", "R_0402", { fields: { MPN: "", LCSC: "C25900" } }),
				bom("R2", "4k7", "X"),
				bom("R3", "4k7", "", "R_0402", { fields: {} }),
				bom("R4", "4k7", "Z", "R_0402", { dnp: true }),
				bom("R5", "4k7", "W", "R_0402", { excludeFromBom: true }),
			],
		});
		expect(keys(f)).toEqual(["warning bom_lines|mpns|R|R_0402|4700"]);
		expect(f[0]!.message).toContain("LCSC C25900 (R1); X (R2)");
	});

	test("merge candidates: adjacent R/C/L values on one footprint within a ratio of 2.2 are info", async () => {
		const f = await run("bom_lines", {
			parts: [bom("R1", "1k", "A"), bom("R2", "2.2k", "B"), bom("R3", "4.9k", "C"), bom("R4", "2k", "D", "R_0603"), bom("U1", "1k", "E")],
		});
		expect(keys(f)).toEqual(["info bom_lines|merge|R|R_0402|1000|2200"]);
		expect(f[0]!.refs).toEqual(["R1", "R2"]);
	});
});
