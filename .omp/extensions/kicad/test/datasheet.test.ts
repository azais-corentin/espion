import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import type { Server } from "bun";
import * as cli from "../src/cli";
import { datasheetCacheDir, readDatasheet } from "../src/datasheet";
import { Project } from "../src/project";
import { cleanup, HAS_KICAD, PROJECT, stageFixture } from "./helpers";

/** Width and height from a PNG's IHDR chunk. */
function pngSize(png: Uint8Array): [number, number] {
	const v = new DataView(png.buffer, png.byteOffset, png.byteLength);
	return [v.getUint32(16), v.getUint32(20)];
}

describe.skipIf(!HAS_KICAD)("kicad_datasheet", () => {
	let root = "";
	let pdf = "";
	let pages = 0;
	const hits: Record<string, number> = {};
	let server: Server<undefined>;
	const savedCache = process.env.XDG_CACHE_HOME;

	beforeAll(async () => {
		root = stageFixture();
		process.env.XDG_CACHE_HOME = path.join(root, "cache");
		const project = Project.open(PROJECT, root);
		pdf = path.join(root, "sheets.pdf");
		const r = await cli.run(["kicad-cli", "sch", "export", "pdf", "-o", pdf, project.rootFile], { cwd: project.dir, repoRoot: root });
		expect(r.code).toBe(0);
		pages = project.hierarchy().length;
		const bytes = fs.readFileSync(pdf);
		server = Bun.serve({
			port: 0,
			fetch(req) {
				const p = new URL(req.url).pathname;
				hits[p] = (hits[p] ?? 0) + 1;
				if (p === "/ds.pdf") return new Response(bytes, { headers: { "content-type": "application/pdf" } });
				if (p === "/moved") return Response.redirect("/ds.pdf", 302);
				return new Response("<!DOCTYPE html><html>login</html>", { headers: { "content-type": "text/html" } });
			},
		});
	}, 120_000);

	afterAll(() => {
		server?.stop(true);
		if (savedCache === undefined) delete process.env.XDG_CACHE_HOME;
		else process.env.XDG_CACHE_HOME = savedCache;
		cleanup(root);
	});

	test("index: local path, page count, first lines of each page; the download is cached and reused", async () => {
		const url = `${server.url}moved`;
		const r = await readDatasheet(root, { url });
		const cached = path.join(datasheetCacheDir(), fs.readdirSync(datasheetCacheDir())[0]!);
		expect(cached.startsWith(path.join(root, "cache", "omp-kicad", "datasheets"))).toBe(true);
		expect(r.text).toContain(`Local PDF: ${cached} (${pages} pages)`);
		expect(r.text.match(/^p\d+: /gm)?.length).toBe(pages);
		expect(r.png).toBeUndefined();
		expect(hits["/ds.pdf"]).toBe(1);
		await readDatasheet(root, { url, page: 1, size: 300 });
		expect(hits["/ds.pdf"]).toBe(1);
	});

	test("page: text and an image of the page or a box, at most `size` px", async () => {
		const full = await readDatasheet(root, { url: pdf, page: 1, size: 500 });
		expect(full.text).toContain(`Local PDF: ${pdf}`);
		expect(full.text).toContain("Text of page 1:");
		const [w, h] = pngSize(full.png!);
		expect(Math.max(w, h)).toBe(500);
		expect(w).toBeGreaterThan(h); // A4/A3 landscape sheet

		// a quarter of the page at 50 dpi is smaller than size: rendered at 50 dpi
		const crop = await readDatasheet(root, { url: pdf, page: 1, box: [0.5, 0.5, 1, 1], dpi: 50 });
		const [cw, ch] = pngSize(crop.png!);
		const at50 = /image (\d+)x(\d+) px of box \[0\.5, 0\.5, 1, 1\] at 50 dpi/.exec(crop.text);
		expect(at50).not.toBeNull();
		expect([cw, ch]).toEqual([Number(at50![1]), Number(at50![2])]);
		const [fw, fh] = pngSize((await readDatasheet(root, { url: pdf, page: 1, dpi: 50 })).png!);
		expect(Math.abs(cw - fw / 2)).toBeLessThanOrEqual(1);
		expect(Math.abs(ch - fh / 2)).toBeLessThanOrEqual(1);
	});

	test("pages: the text of a page list, no image", async () => {
		const r = await readDatasheet(root, { url: pdf, pages: "3, 1-2,2" });
		expect(r.png).toBeUndefined();
		expect(r.text.match(/^--- page \d+ ---$/gm)).toEqual(["--- page 1 ---", "--- page 2 ---", "--- page 3 ---"]);
		expect(r.text).toContain("File: power.kicad_sch");
		expect(r.text).not.toContain("Sheet: /IMU/");
	});

	test("find: matching lines with page and line numbers and context, on all pages or the given ones", async () => {
		const all = await readDatasheet(root, { url: pdf, find: "^d\\s+file: [\\w-]+\\.KICAD_SCH" });
		expect(all.png).toBeUndefined();
		expect(all.text).toContain(`${pages} matching lines:`);
		const hits = all.text.split("\n").filter(l => /^p\d+:\d+: /.test(l));
		expect(hits.map(l => Number(/^p(\d+):/.exec(l)![1]))).toEqual(Array.from({ length: pages }, (_, i) => i + 1));
		expect(all.text).toMatch(/^p\d+:\d+- Sheet: \/Power\/$/m);
		expect(all.text).toContain("\n--\n");
		const some = await readDatasheet(root, { url: pdf, find: "file: power\\.kicad_sch", pages: "3-4" });
		expect(some.text).toContain("on pages 3-4");
		expect(some.text).toContain("1 matching line:");
		expect(some.text).toMatch(/^p3:\d+: D {2}File: power\.kicad_sch {2}D$/m);
	});

	test("pages, find and page don't mix; bad page lists and regexes are refused", async () => {
		await expect(readDatasheet(root, { url: pdf, page: 1, pages: "1-2" })).rejects.toThrow(/pages and find give text only/);
		await expect(readDatasheet(root, { url: pdf, page: 1, find: "x" })).rejects.toThrow(/pages and find give text only/);
		await expect(readDatasheet(root, { url: pdf, pages: "2-x" })).rejects.toThrow(/pages must be like/);
		await expect(readDatasheet(root, { url: pdf, pages: `1-${pages + 1}` })).rejects.toThrow(`outside 1..${pages}`);
		await expect(readDatasheet(root, { url: pdf, pages: "3-2" })).rejects.toThrow(/reversed/);
		await expect(readDatasheet(root, { url: pdf, find: "(" })).rejects.toThrow(/find is not a valid regex/);
	});

	test("refuses non-PDF data, bad pages and boxes", async () => {
		await expect(readDatasheet(root, { url: `${server.url}login` })).rejects.toThrow(/is not a PDF \(text\/html/);
		expect(fs.readdirSync(datasheetCacheDir()).length).toBe(1);
		await expect(readDatasheet(root, { url: pdf, page: pages + 1 })).rejects.toThrow(`page must be 1..${pages}`);
		await expect(readDatasheet(root, { url: pdf, page: 1, box: [0.5, 0, 0.4, 1] })).rejects.toThrow(/box must be/);
		await expect(readDatasheet(root, { url: pdf, box: [0, 0, 1, 1] })).rejects.toThrow(/give page/);
		await expect(readDatasheet(root, { url: path.join(root, PROJECT, "espion-cam.kicad_pro") })).rejects.toThrow(/is not a PDF/);
	});
});
