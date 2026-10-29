/**
 * Datasheets for kicad_datasheet: a PDF from a URL (downloaded once into the user cache, outside
 * the repo) or a local path, read with poppler from the dev shell: a page index, or one page's
 * text and an image of the page or a region of it.
 */
import { createHash } from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as cli from "./cli";

export interface DatasheetRequest {
	url: string;
	page?: number;
	dpi?: number;
	box?: number[];
	size?: number;
}

export interface DatasheetResult {
	text: string;
	png?: Uint8Array;
}

const MAX_DPI = 600;
/** Lines of each page's text in the index. */
const INDEX_LINES = 3;
const INDEX_CHARS = 200;
/** Some datasheet hosts answer non-browser clients with an HTML page. */
const USER_AGENT = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";

export function datasheetCacheDir(): string {
	return path.join(process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache"), "omp-kicad", "datasheets");
}

/** PDF files start with "%PDF-" (readers accept it anywhere in the first 1024 bytes). */
function isPdf(head: Uint8Array): boolean {
	return Buffer.from(head.subarray(0, 1024)).toString("latin1").includes("%PDF-");
}

/** Local path of the PDF: the cached download of a URL, or the given file. */
async function localPdf(url: string, repoRoot: string, signal?: AbortSignal): Promise<string> {
	if (!/^https?:\/\//i.test(url)) {
		const file = path.resolve(repoRoot, url.replace(/^file:\/\//, ""));
		if (!fs.existsSync(file)) throw new Error(`no such file: ${file}`);
		const fd = fs.openSync(file, "r");
		const head = new Uint8Array(1024);
		const n = fs.readSync(fd, head, 0, head.length, 0);
		fs.closeSync(fd);
		if (!isPdf(head.subarray(0, n))) throw new Error(`${file} is not a PDF`);
		return file;
	}
	const file = path.join(datasheetCacheDir(), `${createHash("sha256").update(url).digest("hex").slice(0, 16)}.pdf`);
	if (fs.existsSync(file)) return file;
	const res = await fetch(url, { redirect: "follow", signal, headers: { "User-Agent": USER_AGENT, Accept: "application/pdf,*/*" } });
	if (!res.ok) throw new Error(`download of ${url} failed: HTTP ${res.status} ${res.statusText}`);
	const data = new Uint8Array(await res.arrayBuffer());
	if (!isPdf(data)) {
		const start = Buffer.from(data.subarray(0, 60)).toString("latin1").replace(/\s+/g, " ");
		throw new Error(
			`${url}${res.url !== url ? ` (redirected to ${res.url})` : ""} is not a PDF (${res.headers.get("content-type") ?? "no content type"}, starts with ${JSON.stringify(start)}); find the direct PDF link`,
		);
	}
	fs.mkdirSync(path.dirname(file), { recursive: true });
	const tmp = `${file}.${process.pid}.tmp`;
	fs.writeFileSync(tmp, data);
	fs.renameSync(tmp, file);
	return file;
}

async function poppler(cmd: string[], repoRoot: string, signal?: AbortSignal): Promise<string> {
	const r = await cli.run(cmd, { cwd: os.tmpdir(), repoRoot, signal });
	if (r.code !== 0) throw new Error(`${cmd[0]} failed (exit ${r.code}): ${(r.stderr || r.stdout).trim().slice(-1000)}`);
	return r.stdout;
}

function infoField(info: string, key: string): string | undefined {
	return new RegExp(`^${key}:\\s*(.*)$`, "m").exec(info)?.[1]?.trim();
}

export async function readDatasheet(repoRoot: string, req: DatasheetRequest, signal?: AbortSignal): Promise<DatasheetResult> {
	if (req.page === undefined) {
		for (const k of ["dpi", "box", "size"] as const) if (req[k] !== undefined) throw new Error(`${k} applies to a page only (give page)`);
	}
	const file = await localPdf(req.url, repoRoot, signal);
	const info = await poppler(["pdfinfo", file], repoRoot, signal);
	const pages = Number(infoField(info, "Pages") ?? "0");
	const title = infoField(info, "Title");
	const head = [`Datasheet ${req.url}`, `Local PDF: ${file} (${pages} pages)`, ...(title ? [`Title: ${title}`] : [])];

	if (req.page === undefined) {
		const all = await poppler(["pdftotext", "-layout", file, "-"], repoRoot, signal);
		const index = all
			.split("\f")
			.slice(0, pages)
			.map((t, i) => {
				const lines = t
					.split("\n")
					.map(l => l.trim().replace(/\s+/g, " "))
					.filter(Boolean)
					.slice(0, INDEX_LINES)
					.join(" | ");
				return `p${i + 1}: ${lines.length > INDEX_CHARS ? `${lines.slice(0, INDEX_CHARS)}…` : lines}`;
			});
		return { text: [...head, "", "Pages (first lines):", ...index, "", "Give page (1-based) for its text and image; box [x0, y0, x1, y1] (page fractions, y down) crops."].join("\n") };
	}

	const page = req.page;
	if (!Number.isInteger(page) || page < 1 || page > pages) throw new Error(`page must be 1..${pages}, got ${page}`);
	const dpi = req.dpi ?? 200;
	if (!(dpi > 0 && dpi <= MAX_DPI)) throw new Error(`dpi must be in (0, ${MAX_DPI}], got ${dpi}`);
	const size = req.size ?? 2000;
	if (!Number.isInteger(size) || size < 16) throw new Error(`size must be an integer of at least 16 px, got ${size}`);
	const box = req.box ?? [0, 0, 1, 1];
	if (box.length !== 4 || !box.every(v => v >= 0 && v <= 1) || box[0]! >= box[2]! || box[1]! >= box[3]!) {
		throw new Error(`box must be [x0, y0, x1, y1] page fractions with 0 ≤ x0 < x1 ≤ 1 and 0 ≤ y0 < y1 ≤ 1, got ${JSON.stringify(box)}`);
	}
	const [x0, y0, x1, y1] = box as [number, number, number, number];

	// page size as rendered (pdftoppm applies the page rotation)
	const pinfo = await poppler(["pdfinfo", "-f", String(page), "-l", String(page), file], repoRoot, signal);
	const m = new RegExp(`^Page\\s+${page} size:\\s+([\\d.]+) x ([\\d.]+)`, "m").exec(pinfo);
	if (!m) throw new Error(`pdfinfo reported no size for page ${page}`);
	const rot = Number(new RegExp(`^Page\\s+${page} rot:\\s+(\\d+)`, "m").exec(pinfo)?.[1] ?? "0");
	const [w, h] = rot % 180 ? [Number(m[2]), Number(m[1])] : [Number(m[1]), Number(m[2])];
	// render the crop at dpi, or at the lower resolution that fits it into `size`
	const longest = (Math.max((x1 - x0) * w, (y1 - y0) * h) / 72) * dpi;
	const res = longest > size ? (dpi * size) / longest : dpi;
	const px = (v: number) => (v / 72) * res;
	const W = Math.max(1, Math.min(size, Math.round(px((x1 - x0) * w))));
	const H = Math.max(1, Math.min(size, Math.round(px((y1 - y0) * h))));

	const dir = cli.tmpDir("datasheet");
	try {
		const out = path.join(dir, "page");
		const p = String(page);
		await poppler(
			["pdftoppm", "-f", p, "-l", p, "-r", res.toFixed(4), "-x", String(Math.round(px(x0 * w))), "-y", String(Math.round(px(y0 * h))), "-W", String(W), "-H", String(H), "-png", "-singlefile", file, out],
			repoRoot,
			signal,
		);
		const png = new Uint8Array(fs.readFileSync(`${out}.png`));
		const text = await poppler(["pdftotext", "-layout", "-f", p, "-l", p, file, "-"], repoRoot, signal);
		return {
			text: [
				...head,
				`Page ${page} of ${pages}: image ${W}x${H} px of box [${box.join(", ")}] at ${Math.round(res)} dpi`,
				"",
				`Text of page ${page}:`,
				text.replace(/\f/g, "").trimEnd(),
			].join("\n"),
			png,
		};
	} finally {
		fs.rmSync(dir, { recursive: true, force: true });
	}
}
