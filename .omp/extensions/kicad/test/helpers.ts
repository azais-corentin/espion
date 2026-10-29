/**
 * Test fixtures: the espion-cam project at a pinned commit, unpacked from git into a temp
 * directory (with the toolkit config), so tests run against a real KiCad 10 project without
 * touching the working tree.
 */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

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
