/**
 * Test fixtures: the espion-cam project at a pinned commit, unpacked from git into a temp
 * directory (with the toolkit config), so tests run against a real KiCad 10 project without
 * touching the working tree.
 */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import type { Expect } from "../src/netdiff";

export const REPO = path.resolve(import.meta.dir, "../../../..");
/** "Re-save the espion-cam sheets in KiCad's canonical form": canonical sheets, IMU sheet present. */
export const FIXTURE_COMMIT = "56ec3b6";
export const PROJECT = "hardware/espion-cam";

/** A temp repo root holding hardware/espion-cam and hardware/lib (with its 3D models) at FIXTURE_COMMIT. */
export function stageFixture(commit = FIXTURE_COMMIT): string {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "kicad-test-"));
	const archive = Bun.spawnSync(["git", "-C", REPO, "archive", "--format=tar", commit, "hardware/espion-cam", "hardware/lib"], {
		stdout: "pipe",
		stderr: "pipe",
	});
	if (archive.exitCode !== 0) throw new Error(`git archive failed: ${archive.stderr.toString()}`);
	const tar = Bun.spawnSync(["tar", "-x", "-C", root], { stdin: archive.stdout, stderr: "pipe" });
	if (tar.exitCode !== 0) throw new Error(`tar failed: ${tar.stderr.toString()}`);
	fs.mkdirSync(path.join(root, ".omp"), { recursive: true });
	fs.copyFileSync(path.join(REPO, ".omp", "kicad.json"), path.join(root, ".omp", "kicad.json"));
	return root;
}

export function cleanup(root: string): void {
	fs.rmSync(root, { recursive: true, force: true });
}

/** kicad-cli integration tests need the dev shell; they are skipped outside it. */
export const HAS_KICAD = !!Bun.which("kicad-cli");

/** kicad_edit ops for a new sheet "Test" with R66/R67 on net A and both on GND through upright power symbols. */
export const TEST_SHEET_OPS = [
	{ op: "create_sheet", parent: "root", name: "Test", file: "test.kicad_sch", at: [137.16, 144.78], size: [25.4, 10.16] },
	{ op: "add_symbol", sheet: "Test", lib_id: "Device:R", at: [101.6, 101.6], value: "10k" },
	{ op: "add_symbol", sheet: "Test", lib_id: "Device:R", at: [111.76, 101.6], value: "10k" },
	{ op: "add_wire", sheet: "Test", points: [[101.6, 97.79], [101.6, 95.25], [111.76, 95.25], [111.76, 97.79]] },
	{ op: "add_label", sheet: "Test", name: "A", at: [104.14, 95.25] },
	{ op: "add_wire", sheet: "Test", points: [[101.6, 105.41], [101.6, 106.68]] },
	{ op: "add_wire", sheet: "Test", points: [[111.76, 105.41], [111.76, 106.68]] },
	{ op: "add_power", sheet: "Test", net: "GND", at: [101.6, 106.68] },
	{ op: "add_power", sheet: "Test", net: "GND", at: [111.76, 106.68] },
];

/** The connectivity change of TEST_SHEET_OPS. */
export const TEST_SHEET_EXPECT: Expect = {
	added_parts: ["R66", "R67"],
	nets: [
		{ name: "A", pins: ["R66.1", "R67.1"] },
		{ name: "GND", added: ["R66.2", "R67.2"] },
	],
};
