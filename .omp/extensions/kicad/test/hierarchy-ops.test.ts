import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import * as cli from "../src/cli";
import { ops } from "../src/ops/hierarchy";
import { Project } from "../src/project";
import { at, newUuid } from "../src/sch";
import { child, children, clone, L, type List, parse, setChild, str, text } from "../src/sexpr";
import { validate } from "../src/spec";
import type { OpContext } from "../src/types";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

type Step = [string, Record<string, unknown>];

async function edit(p: Project, steps: Step[]): Promise<string[]> {
	const ctx: OpContext = { project: p, libs: await p.libs(), notes: [], touch() {}, touchPart() {} };
	for (const [op, args] of steps) {
		const def = ops.find(o => o.op === op);
		if (!def) throw new Error(`no op ${op}`);
		await def.apply(ctx, validate(op, args, def.fields));
	}
	return ctx.notes;
}

/** Write the pending files into the staged project and let KiCad re-save every sheet written. */
async function commit(p: Project, root: string): Promise<void> {
	const pending = p.pendingWrites();
	for (const [f, content] of pending) {
		fs.mkdirSync(path.dirname(f), { recursive: true });
		fs.writeFileSync(f, content);
	}
	for (const f of p.deleted) fs.rmSync(f);
	for (const f of pending.keys()) if (f.endsWith(".kicad_sch")) await cli.upgrade("sch", f, { cwd: p.dir, repoRoot: root });
}

/** kicad-cli netlist, plus KiCad's own sheet list (name path → page number) from its design section. */
async function netlist(p: Project, root: string) {
	const dir = cli.tmpDir("hier-net");
	const out = path.join(dir, "out.net");
	const r = await cli.run(["kicad-cli", "sch", "export", "netlist", "--format", "kicadsexpr", "-o", out, p.rootFile], {
		cwd: p.dir,
		repoRoot: root,
	});
	expect(r.code).toBe(0);
	const raw = fs.readFileSync(out, "utf8");
	fs.rmSync(dir, { recursive: true, force: true });
	const pages = new Map(
		children(child(parse(raw), "design") ?? [], "sheet").map(s => [text(child(s, "name")) ?? "", text(child(s, "number")) ?? ""]),
	);
	return { nl: cli.parseNetlist(raw), pages };
}

function connectivity(nl: cli.Netlist): string[] {
	return nl.nets.map(n => `${n.name}: ${n.nodes.map(x => `${x.ref}.${x.pin}`).sort().join(" ")}`).sort();
}

/** ERC errors by type and sheet: new ones are what an edit must not add. */
async function ercErrors(p: Project, root: string): Promise<string[]> {
	return (await cli.erc(p.rootFile, { cwd: p.dir, repoRoot: root }))
		.filter(v => v.severity === "error" && !v.excluded)
		.map(v => `${v.type} ${v.sheet}`)
		.sort();
}

function wire(a: [number, number], b: [number, number]): List {
	return L("wire", L("pts", L("xy", a[0], a[1]), L("xy", b[0], b[1])), L("stroke", L("width", 0), L("type", "default")), L("uuid", str(newUuid())));
}

function hierLabel(name: string, shape: string, x: number, y: number): List {
	return L(
		"hierarchical_label",
		str(name),
		L("shape", shape),
		L("at", x, y, 180),
		L("effects", L("font", L("size", 1.27, 1.27)), L("justify", "right")),
		L("uuid", str(newUuid())),
	);
}

/**
 * Fill the new sheet: hierarchical label VIN → R901 pin 1, R901 pin 2 → local label SENSE. R901 is a
 * copy of a Device:R from the Power sheet, annotated for the new sheet's instance path.
 */
function fillProbe(p: Project): void {
	const probe = p.sheet("Probe");
	const power = p.sheet("Power").sch;
	const sch = probe.sch;
	sch.libSymbols().push(clone(power.libSymbol("Device:R")!));
	const r = clone(power.symbols().find(s => power.libId(s) === "Device:R")!);
	const from = at(r).p;
	const to: [number, number] = [63.5, 58.42];
	setChild(r, L("at", to[0], to[1], 0));
	setChild(r, L("uuid", str(newUuid())));
	for (const pin of children(r, "pin")) setChild(pin, L("uuid", str(newUuid())));
	for (const f of children(r, "property")) {
		const fp = at(f);
		setChild(f, L("at", fp.p[0] - from[0] + to[0], fp.p[1] - from[1] + to[1], fp.rot));
	}
	r.splice(r.indexOf(child(r, "instances")!), 1);
	sch.setRef(r, probe.path, p.name, "R901");
	sch.insert(r);
	sch.insert(hierLabel("VIN", "passive", 50.8, 50.8));
	sch.insert(wire([50.8, 50.8], [63.5, 50.8]));
	sch.insert(wire([63.5, 50.8], [63.5, 54.61]));
	sch.insert(wire([63.5, 62.23], [63.5, 66.04]));
	sch.insert(wire([63.5, 66.04], [76.2, 66.04]));
	sch.insert(L("label", str("SENSE"), L("at", 76.2, 66.04, 0), L("effects", L("font", L("size", 1.27, 1.27)), L("justify", "left", "bottom")), L("uuid", str(newUuid()))));
	p.touch(sch);
}

const PROBE: Step = ["create_sheet", { parent: "root", name: "Probe", file: "probe.kicad_sch", at: [88.9, 121.92], size: [25.4, 12.7] }];

describe.skipIf(!HAS_KICAD)("hierarchy ops through KiCad", () => {
	let root = "";
	let ercBefore: string[] = [];
	let probeUuid = "";
	beforeAll(async () => {
		root = stageFixture();
		ercBefore = await ercErrors(Project.open(PROJECT, root), root);
	});
	afterAll(() => cleanup(root));

	test("create_sheet + add_sheet_pin join a new sheet's net to a root net", async () => {
		const p = Project.open(PROJECT, root);
		const before = await netlist(p, root);
		const pwrSrc = before.nl.nets.find(n => n.name.endsWith("/PWR_SRC"))!;
		expect(pwrSrc.nodes.length).toBeGreaterThan(1);

		await edit(p, [PROBE]);
		fillProbe(p);
		await edit(p, [["add_sheet_pin", { sheet: "Probe", name: "VIN", side: "top", offset: 12.7 }]]);
		// root: from the pin up to the PWR_SRC wire between the Power and MCU blocks
		p.root().insert(wire([101.6, 121.92], [101.6, 101.6]));
		p.root().insert(L("junction", L("at", 101.6, 101.6), L("diameter", 0), L("color", 0, 0, 0, 0), L("uuid", str(newUuid()))));
		p.touch(p.root());
		await commit(p, root);

		const q = Project.open(PROJECT, root);
		const inst = q.sheet("Probe");
		probeUuid = inst.block!.uuid;
		expect(inst.path).toBe(`/${q.root().uuid}/${probeUuid}`);
		expect(inst.block!.pins.map(x => [x.name, x.shape, x.at, x.rot])).toEqual([["VIN", "passive", [101.6, 121.92], 90]]);
		expect(q.proJson.data.sheets).toContainEqual([probeUuid, "Probe"]);

		const after = await netlist(q, root);
		expect(after.pages.get("/Probe/")).toBe("9");
		expect(after.nl.components.get("R901")?.sheetName).toBe("/Probe/");
		const joined = after.nl.nets.find(n => n.nodes.some(x => x.ref === "R901" && x.pin === "1"))!;
		for (const node of pwrSrc.nodes) expect(joined.nodes).toContainEqual(node);
		expect(after.nl.nets.some(n => n.name === "/Probe/SENSE")).toBe(true);

		expect(await ercErrors(q, root)).toEqual(ercBefore);
		const svgDir = await cli.svg(q.rootFile, { cwd: q.dir, repoRoot: root });
		expect(fs.readdirSync(svgDir).some(f => f.endsWith("-Probe.svg"))).toBe(true);
		fs.rmSync(svgDir, { recursive: true, force: true });
	});

	test("set_page sets paper and title block of the new sheet", async () => {
		const p = Project.open(PROJECT, root);
		await edit(p, [["set_page", { sheet: "Probe", paper: "A3", title: "Probe", rev: "B", comments: ["one", "two"] }]]);
		await commit(p, root);
		const sch = Project.open(PROJECT, root).sheet("Probe").sch;
		expect(sch.paper()).toEqual([420, 297]);
		const tb = child(sch.tree, "title_block")!;
		expect([text(child(tb, "title")), text(child(tb, "rev"))]).toEqual(["Probe", "B"]);
		expect(children(tb, "comment").map(c => [text(c), text(c, 2)])).toEqual([
			["1", "one"],
			["2", "two"],
		]);
	});

	test("move_sheet drags the wires on its pins and keeps connectivity", async () => {
		const p = Project.open(PROJECT, root);
		const before = connectivity((await netlist(p, root)).nl);
		await edit(p, [["move_sheet", { sheet: "Probe", at: [88.9, 124.46], size: [27.94, 15.24] }]]);
		await commit(p, root);
		const q = Project.open(PROJECT, root);
		const block = q.sheet("Probe").block!;
		expect([block.at, block.size]).toEqual([
			[88.9, 124.46],
			[27.94, 15.24],
		]);
		expect(block.pins[0]!.at).toEqual([101.6, 124.46]);
		expect(connectivity((await netlist(q, root)).nl)).toEqual(before);
	});

	test("rename_sheet renames the hierarchical net names", async () => {
		const p = Project.open(PROJECT, root);
		const before = connectivity((await netlist(p, root)).nl);
		await edit(p, [["rename_sheet", { sheet: "Probe", name: "Sensor" }]]);
		await commit(p, root);
		const q = Project.open(PROJECT, root);
		const after = await netlist(q, root);
		expect(connectivity(after.nl)).toEqual(before.map(l => l.replaceAll("/Probe/", "/Sensor/")).sort());
		expect(after.nl.nets.some(n => n.name === "/Sensor/SENSE")).toBe(true);
		expect(after.nl.components.get("R901")?.sheetName).toBe("/Sensor/");
		expect(q.proJson.data.sheets).toContainEqual([probeUuid, "Sensor"]);
	});

	test("remove_sheet with delete_file removes the block, the .kicad_pro entry and the file", async () => {
		const p = Project.open(PROJECT, root);
		const file = p.sheet("Sensor").file;
		// the sheet still has R901
		await expect(edit(p, [["remove_sheet", { sheet: "Sensor", delete_file: true }]])).rejects.toThrow();
		await edit(p, [["remove_sheet", { sheet: "Sensor", delete_file: true, force: true }]]);
		await commit(p, root);
		const q = Project.open(PROJECT, root);
		expect(fs.existsSync(file)).toBe(false);
		expect(q.hierarchy().map(i => i.name)).not.toContain("Sensor");
		expect(JSON.stringify(q.proJson.data.sheets)).not.toContain(probeUuid);
		const after = await netlist(q, root);
		expect(after.nl.components.has("R901")).toBe(false);
	});
});

describe("hierarchy ops in memory", () => {
	let root = "";
	beforeAll(() => {
		root = stageFixture();
	});
	afterAll(() => cleanup(root));

	test("add_sheet_pin needs a hierarchical label of that name in the sheet", async () => {
		const p = Project.open(PROJECT, root);
		await edit(p, [PROBE]);
		await expect(edit(p, [["add_sheet_pin", { sheet: "Probe", name: "NOPE", side: "left", offset: 2.54 }]])).rejects.toThrow();
		expect(p.sheet("Probe").block!.pins).toEqual([]);
	});

	test("a new sheet is part of the hierarchy in the same transaction and refuses duplicates", async () => {
		const p = Project.open(PROJECT, root);
		await edit(p, [PROBE]);
		const inst = p.sheet("Probe");
		expect(inst.parent?.namePath).toBe("/");
		expect(inst.sch.symbols()).toEqual([]);
		expect(child(inst.sch.tree, "sheet_instances")).toBeUndefined();
		const page = children(child(inst.block!.node, "instances")!, "project").flatMap(x => children(x, "path"));
		expect(page.map(x => [text(x), text(child(x, "page"))])).toEqual([[inst.parent!.path, "9"]]);
		// same name (any case), same file, overlapping another block
		const again = (args: Record<string, unknown>) => edit(p, [["create_sheet", { ...PROBE[1], ...args }]]);
		await expect(again({ file: "other.kicad_sch", name: "probe" })).rejects.toThrow();
		await expect(again({ name: "Other", at: [38.1, 124.46] })).rejects.toThrow();
		await expect(again({ name: "Other", file: "other.kicad_sch", at: [50.8, 91.44] })).rejects.toThrow();
	});

	test("sync_sheet_pins adds missing pins by direction and reports orphans", async () => {
		const p = Project.open(PROJECT, root);
		await edit(p, [PROBE]);
		const sch = p.sheet("Probe").sch;
		sch.insert(hierLabel("EN", "input", 50.8, 40.64));
		sch.insert(hierLabel("OUT", "output", 50.8, 45.72));
		sch.insert(hierLabel("IO", "bidirectional", 50.8, 50.8));
		await edit(p, [["add_sheet_pin", { sheet: "Probe", name: "EN", side: "left", offset: 2.54 }]]);
		sch.remove(sch.labels().find(l => l.name === "EN")!.node);
		const notes = await edit(p, [["sync_sheet_pins", { sheet: "Probe" }]]);
		// the block is at (88.9, 121.92), 25.4 wide
		expect(p.sheet("Probe").block!.pins.map(q => [q.name, q.shape, q.at, q.rot])).toEqual([
			["EN", "input", [88.9, 124.46], 180],
			["OUT", "output", [114.3, 124.46], 0],
			["IO", "bidirectional", [88.9, 127], 180],
		]);
		expect(notes.some(n => n.includes("EN"))).toBe(true);
	});

	test("move_sheet refuses a size the pins no longer fit", async () => {
		const p = Project.open(PROJECT, root);
		await edit(p, [PROBE]);
		p.sheet("Probe").sch.insert(hierLabel("A", "passive", 50.8, 50.8));
		await edit(p, [["add_sheet_pin", { sheet: "Probe", name: "A", side: "left", offset: 10.16 }]]);
		await expect(edit(p, [["move_sheet", { sheet: "Probe", at: [88.9, 121.92], size: [25.4, 10.16] }]])).rejects.toThrow();
	});
});
