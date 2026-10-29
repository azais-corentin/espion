import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import { styleFindings } from "../src/checks/style";
import { Project, type SheetInst } from "../src/project";
import { newUuid, Sch } from "../src/sch";
import { clone, L, type List, str } from "../src/sexpr";
import type { Finding } from "../src/types";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

/** Text widths without kicad-cli: every string falls back to CHAR_W per character. */
class NoSvg extends Analysis {
	override textWidths(): Promise<Map<string, number>> {
		return Promise.resolve(new Map());
	}
}

let root: string;
let libs: List[];

beforeAll(() => {
	root = stageFixture();
	const eth = Project.open(PROJECT, root).sheet("Ethernet").sch;
	libs = ["Device:R", "power:GND", "power:+3V3"].map(id => {
		const l = eth.libSymbol(id);
		if (!l) throw new Error(`fixture lost ${id}`);
		return clone(l);
	});
});

afterAll(() => cleanup(root));

const X = 101.6;
const Y = 101.6;

/** A synthetic sheet drawn at an offset (all coordinates relative to (X, Y) + offset). */
class Draw {
	readonly sch: Sch;
	constructor(
		readonly dx = 0,
		readonly dy = 0,
	) {
		this.sch = Sch.empty(path.join(root, "synthetic.kicad_sch"), "A4");
		this.sch.libSymbols().push(...libs.map(l => clone(l)));
	}

	private x(v: number): number {
		return X + this.dx + v;
	}

	private y(v: number): number {
		return Y + this.dy + v;
	}

	/** Device:R rot 0: pin 1 at (x, y - 3.81) leaving up, pin 2 at (x, y + 3.81) leaving down. rot 90: pin 1 left, pin 2 right. */
	sym(libId: string, ref: string, x: number, y: number, rot = 0, valueAt?: [number, number]): this {
		const field = (name: string, value: string, at: [number, number] | undefined) =>
			L(
				"property",
				str(name),
				str(value),
				L("at", at ? this.x(at[0]) : this.x(x), at ? this.y(at[1]) : this.y(y), 0),
				L("hide", !at),
				L("effects", L("font", L("size", 1.27, 1.27)), ...(at ? [L("justify", "left")] : [])),
			);
		this.sch.insert(
			L(
				"symbol",
				L("lib_id", str(libId)),
				L("at", this.x(x), this.y(y), rot),
				L("unit", 1),
				field("Reference", ref, undefined),
				field("Value", "10k", valueAt),
				L("uuid", str(newUuid())),
			),
		);
		return this;
	}

	wire(...pts: [number, number][]): this {
		for (let i = 1; i < pts.length; i++) {
			const [a, b] = [pts[i - 1]!, pts[i]!];
			this.sch.insert(
				L(
					"wire",
					L("pts", L("xy", this.x(a[0]), this.y(a[1])), L("xy", this.x(b[0]), this.y(b[1]))),
					L("stroke", L("width", 0), L("type", "default")),
					L("uuid", str(newUuid())),
				),
			);
		}
		return this;
	}

	label(name: string, x: number, y: number, rot = 0): this {
		this.sch.insert(
			L(
				"label",
				str(name),
				L("at", this.x(x), this.y(y), rot),
				L("effects", L("font", L("size", 1.27, 1.27)), L("justify", rot === 180 ? "right" : "left", "bottom")),
				L("uuid", str(newUuid())),
			),
		);
		return this;
	}

	junction(x: number, y: number): this {
		this.sch.insert(L("junction", L("at", this.x(x), this.y(y)), L("diameter", 0), L("color", 0, 0, 0, 0), L("uuid", str(newUuid()))));
		return this;
	}

	/** A sheet block whose left edge is at x with one input pin at (x, y). */
	block(name: string, pin: string, x: number, y: number): this {
		this.sch.insert(
			L(
				"sheet",
				L("at", this.x(x), this.y(y - 2.54)),
				L("size", 20.32, 5.08),
				L("uuid", str(newUuid())),
				L("property", str("Sheetname"), str(name), L("at", this.x(x), this.y(y - 3.81), 0), L("effects", L("font", L("size", 1.27, 1.27)))),
				L("property", str("Sheetfile"), str(`${name}.kicad_sch`), L("at", this.x(x), this.y(y + 3.81), 0), L("effects", L("font", L("size", 1.27, 1.27)))),
				L("pin", str(pin), "input", L("at", this.x(x), this.y(y), 180), L("uuid", str(newUuid()))),
			),
		);
		return this;
	}

	async findings(rules?: string[]): Promise<Finding[]> {
		const project = Project.open(PROJECT, root);
		if (rules) project.settings.style.rules = rules;
		const sheet: SheetInst = { path: "/synthetic", namePath: "/T/", name: "T", file: this.sch.file, sch: this.sch };
		return styleFindings(new NoSvg(project), [sheet]);
	}
}

/**
 * A drawing that follows every rule: a vertical resistor with +3V3 straight above and GND straight
 * below, and a horizontal one with a label on the wire off its left pin and GND on an L off its right pin.
 */
function correct(dx = 0, dy = 0): Draw {
	return new Draw(dx, dy)
		.sym("Device:R", "R1", 0, 0)
		.sym("power:+3V3", "#PWR1", 0, -5.08)
		.wire([0, -3.81], [0, -5.08])
		.sym("power:GND", "#PWR2", 0, 5.08)
		.wire([0, 3.81], [0, 5.08])
		.sym("Device:R", "R2", 25.4, 0, 90)
		.wire([21.59, 0], [11.43, 0])
		.label("SIG", 11.43, 0)
		.sym("power:GND", "#PWR3", 30.48, 1.27)
		.wire([29.21, 0], [30.48, 0], [30.48, 1.27]);
}

const keys = (fs: Finding[]) => fs.map(f => f.key).sort();

describe("style rules on synthetic sheets", () => {
	test("a drawing that follows the rules has no findings, wherever it sits", async () => {
		expect(await correct().findings()).toEqual([]);
		expect(await correct(25.4, -12.7).findings()).toEqual([]);
	});

	test("translating a drawing by a grid multiple keeps every key", async () => {
		const bad = (dx: number, dy: number) =>
			correct(dx, dy)
				.sym("Device:R", "R3", 50.8, 0)
				.wire([50.8, 3.81], [50.8, 5.08], [53.34, 5.08])
				.label("LONG_SIGNAL_NAME", 53.34, 5.08)
				.sym("Device:R", "R4", 53.34, 0)
				.wire([0, -5.08], [0, -10.16]);
		const a = await bad(0, 0).findings();
		const b = await bad(38.1, 25.4).findings();
		expect(a.length).toBeGreaterThan(3);
		expect(keys(b)).toEqual(keys(a));
	});

	test("pin_stub: a junction exactly at the stub length passes, one closer fails", async () => {
		const branch = (d: number) =>
			new Draw()
				.sym("Device:R", "R1", 0, 0)
				.wire([0, 3.81], [0, 3.81 + d], [0, 8.89])
				.wire([0, 3.81 + d], [5.08, 3.81 + d])
				.junction(0, 3.81 + d);
		expect(await branch(1.27).findings(["pin_stub"])).toEqual([]);
		expect(keys(await branch(0.635).findings(["pin_stub"]))).toEqual(["style|pin_stub|T|R1.2"]);
	});

	test("pin_stub: a bend inside the stub fails", async () => {
		const d = new Draw().sym("Device:R", "R1", 0, 0).wire([0, 3.81], [0, 5.08]).wire([0, 3.81], [2.54, 3.81]).label("A", 2.54, 3.81, 180);
		const fs = await d.findings();
		expect(keys(fs)).toContain("style|pin_stub|T|R1.2");
	});

	test("power: L-shape on a horizontal pin passes, a rotated symbol on a single pin fails", async () => {
		const lshape = new Draw().sym("Device:R", "R1", 0, 0, 90).sym("power:GND", "#PWR1", 5.08, 1.27).wire([3.81, 0], [5.08, 0], [5.08, 1.27]);
		expect(await lshape.findings()).toEqual([]);
		const supply = new Draw().sym("Device:R", "R1", 0, 0, 90).sym("power:+3V3", "#PWR1", -5.08, -1.27).wire([-3.81, 0], [-5.08, 0], [-5.08, -1.27]);
		expect(await supply.findings(["power"])).toEqual([]);
		const wrongWay = new Draw().sym("Device:R", "R1", 0, 0, 90).sym("power:+3V3", "#PWR1", 5.08, 1.27).wire([3.81, 0], [5.08, 0], [5.08, 1.27]);
		expect(keys(await wrongWay.findings(["power"]))).toEqual(["style|power|T|#PWR1"]);
		const rotated = new Draw().sym("Device:R", "R1", 0, 0, 90).sym("power:GND", "#PWR1", 5.08, 0, 90).wire([3.81, 0], [5.08, 0]);
		const fs = await rotated.findings();
		expect(keys(fs)).toEqual(["style|power|T|#PWR1"]);
		expect(fs[0]!.severity).toBe("error");
	});

	test("underline: a horizontal label longer than its wire fails", async () => {
		const d = new Draw().sym("Device:R", "R1", 0, 0, 90).wire([3.81, 0], [6.35, 0]).label("LONG_SIGNAL_NAME", 6.35, 0, 180);
		expect(keys(await d.findings())).toEqual(["style|underline|T|LONG_SIGNAL_NAME|1"]);
	});

	test("overlap: touching graphics are an error, a field over another body a warning", async () => {
		const bodies = await new Draw().sym("Device:R", "R1", 0, 0).sym("Device:R", "R2", 1.27, 0).findings();
		expect(bodies.map(f => [f.key, f.severity])).toEqual([["style|overlap|T|R1|R2", "error"]]);
		expect(await new Draw().sym("Device:R", "R1", 0, 0).sym("Device:R", "R2", 2.54, 0).findings()).toEqual([]);
		const fields = await new Draw().sym("Device:R", "R1", 0, 0, 0, [1.27, 0]).sym("Device:R", "R2", 5.08, 0).findings();
		expect(fields.map(f => [f.key, f.severity])).toEqual([["style|overlap|T|R1|R2", "warning"]]);
	});

	test("off_grid: a symbol whose pins miss the grid fails", async () => {
		expect(keys(await new Draw().sym("Device:R", "R1", 0.635, 0).findings())).toEqual(["style|off_grid|T|R1"]);
	});

	test("frame: a symbol outside the border fails", async () => {
		expect(keys(await new Draw().sym("Device:R", "R1", -X + 5.08, 0).findings())).toEqual(["style|frame|T|R1"]);
	});

	test("wiring: a dangling wire end warns, named after what it hangs off", async () => {
		const fs = await new Draw().sym("Device:R", "R1", 0, 0).wire([0, 3.81], [0, 6.35]).findings();
		expect(fs.map(f => [f.key, f.severity])).toEqual([["style|wiring|dangling|T|wire at R1.2", "warning"]]);
	});

	test("wiring: a T without a junction is an error", async () => {
		const d = new Draw()
			.sym("Device:R", "R1", 0, 0)
			.wire([0, 3.81], [0, 8.89])
			.label("A", 0, 8.89)
			.wire([0, 6.35], [5.08, 6.35])
			.label("A", 5.08, 6.35, 180);
		const fs = await d.findings();
		expect(fs.map(f => [f.key, f.severity])).toEqual([["style|wiring|missing_junction|T|net A", "error"]]);
	});

	test("wiring: sheet-block pins are connection points", async () => {
		const d = new Draw().sym("Device:R", "R1", 0, 0, 90).wire([3.81, 0], [12.7, 0]).label("B", 5.08, 0);
		expect(keys(await d.findings())).toEqual(["style|wiring|dangling|T|net B"]);
		expect(await d.block("Sub", "B", 12.7, 0).findings()).toEqual([]);
	});

	test("unknown rule names are rejected", async () => {
		await expect(new Draw().findings(["pin_stubs"])).rejects.toThrow(/unknown rule/);
	});
});

describe("style on the espion-cam fixture", () => {
	test.skipIf(!HAS_KICAD)(
		"is clean outside the IMU sheet",
		async () => {
			const project = Project.open(PROJECT, root);
			const analysis = new Analysis(project);
			const fs = await styleFindings(analysis, project.hierarchy()).finally(() => analysis.dispose());
			expect(fs.filter(f => f.sheet !== "IMU")).toEqual([]);
			// the IMU sheet predates the drawing rules: power symbols sit pin-to-pin, labels hang off wire ends
			const imu = fs.filter(f => f.sheet === "IMU");
			expect(new Set(imu.map(f => f.key.split("|")[1]))).toEqual(new Set(["pin_stub", "power", "underline", "overlap", "wiring"]));
			expect(keys(imu)).toContain("style|power|IMU|#PWR702");
			expect(keys(imu)).toContain("style|underline|IMU|IMU_INT1|2");
		},
		240_000,
	);
});
