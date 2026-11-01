import { afterEach, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { Analysis } from "../src/analysis";
import { erc } from "../src/cli";
import { excludes, exclusionList, exclusions, hiddenBy, parseKey, selectExclusions, unexcluded } from "../src/exclusions";
import { ops } from "../src/ops/project";
import { Project } from "../src/project";
import { parsePro, stringifyPro } from "../src/projson";
import { queries } from "../src/queries/project";
import { validate } from "../src/spec";
import { runEdit } from "../src/txn";
import type { OpContext } from "../src/types";
import { BOARD_PROJECT, cleanup, HAS_KICAD, PROJECT, stageBoardFixture, stageFixture } from "./helpers";

const NIL = "00000000-0000-0000-0000-000000000000";
/** The fixture's ERC exclusions (U12 SA0 and SDx tied to GND): pin_to_pin with #FLG09's pin, main item on the IMU sheet, aux item on the Power sheet. */
const SA0 =
	"pin_to_pin|1371600|939800|0158166b-1052-43f6-921e-5cf218d0b459|c082198d-9317-456f-a5e6-dc5097bb410f|/6c2ac6d3-fc29-423f-b8b3-ded6190f8eb6/b46c0f76-16c4-4d72-b9e8-a38b09270ab0|/6c2ac6d3-fc29-423f-b8b3-ded6190f8eb6/b46c0f76-16c4-4d72-b9e8-a38b09270ab0|/6c2ac6d3-fc29-423f-b8b3-ded6190f8eb6/852bc00d-c86c-4810-b88b-d834d499a4b0";
/** The copper board fixture's dangling track end (still dangling after the zone refill), as KiCad keys its DRC exclusion. */
const TRACK = `track_dangling|9175000|14000000|c0ffee00-0000-4000-8000-000000000009|${NIL}`;
/**
 * espion-cam with both ERC exclusions stale but their items still there: U12 is the MSA3S02 by
 * then, the keys' pin uuids name its NC pin 1 and SDA pin 2, nothing ties them to the PWR_FLAG.
 */
const STALE_COMMIT = "08f6d3b";

const noKicad = () => [];

/** Apply ops to a freshly opened project and write the pending files; returns the op notes. */
async function apply(root: string, project: string, list: Record<string, unknown>[]): Promise<string[]> {
	const p = Project.open(project, root);
	const ctx: OpContext = { project: p, libs: await p.libs(), notes: [], touch() {}, touchPart() {}, touchBoard() {} };
	for (const { op, ...args } of list) {
		const def = ops.find(o => o.op === op)!;
		await def.apply(ctx, validate(def.op, args, def.fields));
	}
	for (const [file, content] of p.pendingWrites()) fs.writeFileSync(file, content);
	return ctx.notes;
}

// biome-ignore lint/suspicious/noExplicitAny: query results are plain JSON, inspected by shape
async function query(root: string, project: string, args: Record<string, unknown> = {}): Promise<any> {
	const p = Project.open(project, root);
	const analysis = new Analysis(p);
	try {
		const def = queries.find(d => d.what === "exclusions")!;
		return await def.run({ project: p, analysis, libs: () => p.libs() }, validate("exclusions", args, def.fields));
	} finally {
		analysis.dispose();
	}
}

describe("exclusion keys", () => {
	test("ERC keys: rule, position in sheet mm, item uuids and sheet paths (main item's first)", () => {
		expect(parseKey("erc", SA0)).toEqual({
			rule: "pin_to_pin",
			at: [137.16, 93.98],
			items: ["0158166b-1052-43f6-921e-5cf218d0b459", "c082198d-9317-456f-a5e6-dc5097bb410f"],
			sheetPaths: ["/6c2ac6d3-fc29-423f-b8b3-ded6190f8eb6/b46c0f76-16c4-4d72-b9e8-a38b09270ab0", "/6c2ac6d3-fc29-423f-b8b3-ded6190f8eb6/852bc00d-c86c-4810-b88b-d834d499a4b0"],
		});
		// a field's text instead of the second item
		expect(parseKey("erc", `unresolved_variable|10000|20000|0158166b-1052-43f6-921e-5cf218d0b459|\${MISSING}||/a|`)).toMatchObject({ items: ["0158166b-1052-43f6-921e-5cf218d0b459"], text: "${MISSING}", at: [1, 2] });
	});

	test("DRC keys: position in board mm and KiCad's per-rule layouts", () => {
		expect(parseKey("drc", TRACK)).toEqual({ rule: "track_dangling", at: [9.175, 14], items: ["c0ffee00-0000-4000-8000-000000000009"] });
		expect(parseKey("drc", `via_dangling|24212500|7000000|c0ffee00-0000-4000-8000-000000000012|${NIL}`)).toMatchObject({ at: [24.2125, 7] });
		const a = "c0ffee00-0000-4000-8000-00000000000a";
		const b = "c0ffee00-0000-4000-8000-00000000000b";
		expect(parseKey("drc", `copper_sliver|1000000|0|${a}|F.Cu`)).toEqual({ rule: "copper_sliver", at: [1, 0], items: [a], layer: "F.Cu" });
		expect(parseKey("drc", `unconnected_items|0|0|B.Cu|2|${a}|${b}`)).toEqual({ rule: "unconnected_items", at: [0, 0], items: [a, b], layer: "B.Cu" });
		expect(parseKey("drc", `unconnected_items|0|0|${a}|${b}`)).toEqual({ rule: "unconnected_items", at: [0, 0], items: [a, b] });
		expect(parseKey("drc", `starved_thermal|0|0|${a}|${b}|In1.Cu`)).toEqual({ rule: "starved_thermal", at: [0, 0], items: [a, b], layer: "In1.Cu" });
	});

	test("a violation matches by rule and every item uuid of the key, else by position", () => {
		const e = { ...parseKey("erc", SA0) };
		const items = [
			{ description: "U12 pin 1", uuid: "0158166b-1052-43f6-921e-5cf218d0b459", pos: { x: 137.16, y: 93.98 } },
			{ description: "#FLG09 pin 1", uuid: "c082198d-9317-456f-a5e6-dc5097bb410f" },
		];
		expect(excludes(e, { type: "pin_to_pin", items })).toBe(true);
		expect(excludes(e, { type: "pin_not_driven", items })).toBe(false);
		expect(excludes(e, { type: "pin_to_pin", items: items.slice(0, 1) })).toBe(false);
		expect(excludes({ ...e, items: [] }, { type: "pin_to_pin", items: items.slice(0, 1) })).toBe(true);
	});

	test("selection: exactly one of key or match; one hit unless all", () => {
		const data = { erc: { erc_exclusions: [[SA0, "U12 SA0 tied"], [SA0.replace("939800", "965200"), "U12 SDx tied"], "pin_to_pin|0|0|||||"] } };
		expect(selectExclusions(data, "erc", { match: "sdx" }).map(e => e.index)).toEqual([1]);
		expect(selectExclusions(data, "erc", { key: SA0 }).map(e => e.comment)).toEqual(["U12 SA0 tied"]);
		expect(selectExclusions(data, "erc", { key: "pin_to_pin|0|0|||||" }).map(e => [e.index, e.comment])).toEqual([[2, ""]]);
		expect(() => selectExclusions(data, "erc", { match: "U12" })).toThrow("2 ERC exclusions match");
		expect(selectExclusions(data, "erc", { match: "U12", all: true })).toHaveLength(2);
		expect(() => selectExclusions(data, "erc", { match: "nothing" })).toThrow('no ERC exclusion matches "nothing"');
		expect(() => selectExclusions(data, "erc", { key: SA0, match: "U12" })).toThrow("exactly one of key");
		expect(() => selectExclusions(data, "erc", {})).toThrow("exactly one of key");
	});

	test("an exclusion hides a violation only when KiCad detects more of it than the report shows; removing it shows only that", () => {
		const e = exclusions({ erc: { erc_exclusions: [[SA0, "U12 SA0 tied"]] } }, "erc")[0]!;
		const items = [
			{ description: "U12 pin 1", uuid: "0158166b-1052-43f6-921e-5cf218d0b459", pos: { x: 137.16, y: 93.98 } },
			{ description: "#FLG09 pin 1", uuid: "c082198d-9317-456f-a5e6-dc5097bb410f" },
		];
		const real = { type: "pin_to_pin", items };
		const excluded = { ...real, excluded: true };
		const other = { type: "pin_not_driven", items: items.slice(1) };
		// live: KiCad detects it and the report has it excluded
		expect(hiddenBy(e, [excluded, other], [real, other])).toBe(excluded);
		// stale, items still there: the report has KiCad's re-created marker, the run without exclusions finds nothing
		expect(hiddenBy(e, [excluded, other], [other])).toBeUndefined();
		// stale, the violation moved: found and shown, the re-created marker beside it
		expect(hiddenBy(e, [real, excluded], [real])).toBeUndefined();

		// removing it: the after report (without it) tells what it hid
		const live = unexcluded([excluded, other], [e], [real, other]);
		expect(live.shown).toEqual([{ ...real, excluded: false }]);
		expect(live.report).toEqual([{ ...real, excluded: false }, other]);
		const stale = unexcluded([excluded, other], [e], [other]);
		expect(stale).toEqual({ report: [excluded, other], shown: [] });
		expect(unexcluded([real, excluded], [e], [real]).shown).toEqual([]);
	});
});

describe.skipIf(!HAS_KICAD)("ERC exclusions", () => {
	let root = "";
	afterEach(() => cleanup(root));
	const pro = () => path.join(root, PROJECT, "espion-cam.kicad_pro");

	test("set_exclusion_comment changes only that comment; remove_exclusion deletes one, or every match with all", async () => {
		root = stageFixture();
		const pro0 = fs.readFileSync(pro(), "utf8");
		await apply(root, PROJECT, [{ op: "set_exclusion_comment", kind: "erc", match: "U12 SDx", comment: "SDx to GND: sensor hub unused." }]);
		const a = pro0.split("\n");
		const b = fs.readFileSync(pro(), "utf8").split("\n");
		expect(b).toHaveLength(a.length);
		expect(a.flatMap((l, i) => (l === b[i] ? [] : [b[i]!.trim()]))).toEqual(['"SDx to GND: sensor hub unused."']);

		await expect(apply(root, PROJECT, [{ op: "remove_exclusion", kind: "erc", match: "to gnd" }])).rejects.toThrow("2 ERC exclusions match");
		const notes = await apply(root, PROJECT, [{ op: "remove_exclusion", kind: "erc", key: SA0 }]);
		expect(notes).toEqual([expect.stringMatching(/^ERC exclusion removed: pin_to_pin @ \(137\.16, 93\.98\) "U12 SA0 tied to GND/)]);
		expect(JSON.parse(fs.readFileSync(pro(), "utf8")).erc.erc_exclusions).toEqual([[expect.stringContaining("|965200|"), "SDx to GND: sensor hub unused."]]);
		await apply(root, PROJECT, [{ op: "remove_exclusion", kind: "erc", match: "pin_to_pin", all: true }]);
		expect(fs.readFileSync(pro(), "utf8")).toContain('"erc_exclusions": [],');
		// the fixture's .kicad_pro has no board settings yet
		await expect(apply(root, PROJECT, [{ op: "remove_exclusion", kind: "drc", match: "x" }])).rejects.toThrow("has no board.design_settings.drc_exclusions");
	});

	test("the exclusions query lists both with their items and the violation each still hides", async () => {
		root = stageFixture();
		const { exclusions } = await query(root, PROJECT);
		expect(exclusions).toHaveLength(2);
		expect(exclusions[0]).toMatchObject({
			kind: "erc",
			rule: "pin_to_pin",
			at: [137.16, 93.98],
			sheet: "/IMU/",
			comment: expect.stringMatching(/^U12 SA0 tied to GND/),
			items: ["U12 pin 1", "#FLG09 pin 1"],
			hides: expect.stringContaining("Symbol U12 Pin 1 [SDO/SA0"),
			key: SA0,
		});
		expect(exclusions[1]).toMatchObject({ items: ["U12 pin 2", "#FLG09 pin 1"], hides: expect.stringContaining("U12 Pin 2") });
		expect(exclusions[1].stale).toBeUndefined();
		expect((await query(root, PROJECT, { kind: "drc" })).exclusions).toEqual([]);
	}, 60_000);

	test("stale exclusions whose items still exist: KiCad's report keeps them excluded, the query names them stale and removing them shows nothing again", async () => {
		root = stageFixture(STALE_COMMIT);
		const dir = path.join(root, PROJECT);
		// KiCad re-creates an excluded marker from each key whose items resolve, though ERC finds no conflict there
		const v = await erc(path.join(dir, "espion-cam.kicad_sch"), { cwd: dir, repoRoot: root });
		expect(v.filter(x => x.type === "pin_to_pin" && x.excluded).map(x => x.items[0]!.description)).toEqual([
			"Symbol U12 Hidden pin 1 [NC, Unconnected, Line]",
			"Symbol U12 Pin 2 [SDA, Bidirectional, Line]",
		]);
		const { exclusions } = await query(root, PROJECT, { kind: "erc" });
		expect(exclusions).toHaveLength(2);
		expect(exclusions[0]).toMatchObject({ items: ["U12 pin 1", "#FLG05 pin 1"], stale: true });
		expect(exclusions[1]).toMatchObject({ items: ["U12 pin 2", "#FLG05 pin 1"], stale: true });
		for (const e of exclusions) expect(e.hides).toBeUndefined();

		const r = await runEdit({ ops: [{ op: "remove_exclusion", kind: "erc", match: "pin_to_pin", all: true }] }, root, undefined, undefined, noKicad);
		expect(r.applied).toBe(true);
		expect(r.report).toContain("(0 new, 0 resolved)");
		expect(r.report).not.toContain("shown again");
	}, 120_000);

	test("removing an exclusion that still hides a violation applies and reports the violation, not gated", async () => {
		root = stageFixture();
		const dir = path.join(root, PROJECT);
		const r = await runEdit({ ops: [{ op: "remove_exclusion", kind: "erc", match: "U12 SA0" }] }, root, undefined, undefined, noKicad);
		expect(r.applied).toBe(true);
		expect(r.report).toContain("ERC: ");
		expect(r.report).toContain("(0 new, 0 resolved)");
		expect(r.report).toContain("ERC violations shown again by the removed exclusions (not gated");
		expect(r.report).toContain("Symbol U12 Pin 1 [SDO/SA0");
		const v = await erc(path.join(dir, "espion-cam.kicad_sch"), { cwd: dir, repoRoot: root });
		const pin = (n: number) => v.find(x => x.type === "pin_to_pin" && x.items.some(i => i.description.startsWith(`Symbol U12 Pin ${n} `)));
		expect(pin(1)?.excluded).toBeFalsy();
		expect(pin(2)?.excluded).toBe(true);
	}, 120_000);
});

describe.skipIf(!HAS_KICAD)("DRC exclusions", () => {
	let root = "";
	afterEach(() => cleanup(root));
	const proFile = () => path.join(root, BOARD_PROJECT, "board.kicad_pro");

	/** The board fixture with the copper board and its dangling track excluded (written as KiCad writes the .kicad_pro). */
	function stage(entry: unknown = [TRACK, "probe track, routing comes later"]): void {
		root = stageBoardFixture();
		const dir = path.join(root, BOARD_PROJECT);
		fs.copyFileSync(path.join(dir, "copper.kicad_pcb"), path.join(dir, "board.kicad_pcb"));
		const pro = parsePro(fs.readFileSync(proFile(), "utf8"));
		exclusionList(pro.data, "drc")!.push(entry);
		fs.writeFileSync(proFile(), stringifyPro(pro));
	}

	test("the exclusions query checks DRC exclusions against KiCad's DRC and names ERC ones whose items are gone stale", async () => {
		stage();
		const { exclusions } = await query(root, BOARD_PROJECT);
		const drc = exclusions.filter((e: { kind: string }) => e.kind === "drc");
		expect(drc).toEqual([
			{
				kind: "drc",
				rule: "track_dangling",
				at: [9.175, 14],
				comment: "probe track, routing comes later",
				items: ["segment"],
				hides: expect.stringContaining("Track [/VIN] on F.Cu"),
				key: TRACK,
			},
		]);
		// the fixture's .kicad_pro carries espion-cam's two ERC exclusions: nothing of theirs is in this project
		const ercRows = exclusions.filter((e: { kind: string }) => e.kind === "erc");
		expect(ercRows).toHaveLength(2);
		for (const e of ercRows) {
			expect(e.stale).toBe(true);
			expect(e.sheet).toEndWith(" (gone)");
			expect(e.items.every((i: string) => i.endsWith(" (gone)"))).toBe(true);
		}
	}, 60_000);

	test("set_exclusion_comment turns a bare key into a [key, comment] pair; remove_exclusion of a DRC exclusion reports the violation shown again", async () => {
		stage(TRACK);
		await apply(root, BOARD_PROJECT, [{ op: "set_exclusion_comment", kind: "drc", key: TRACK, comment: "routing comes later" }]);
		expect(JSON.parse(fs.readFileSync(proFile(), "utf8")).board.design_settings.drc_exclusions).toEqual([[TRACK, "routing comes later"]]);
		const r = await runEdit({ project: BOARD_PROJECT, ops: [{ op: "remove_exclusion", kind: "drc", match: "c0ffee00-0000-4000-8000-000000000009" }] }, root, undefined, undefined, noKicad);
		expect(r.applied).toBe(true);
		expect(r.report).toContain("DRC violations shown again by the removed exclusions (not gated");
		expect(r.report).toContain("[drc] track_dangling: Track has unconnected end [Track [/VIN] on F.Cu");
		expect(r.report).toMatch(/DRC: \d+ violations after \(0 new, 0 resolved\)/);
		expect(JSON.parse(fs.readFileSync(proFile(), "utf8")).board.design_settings.drc_exclusions).toEqual([]);
	}, 120_000);
});
