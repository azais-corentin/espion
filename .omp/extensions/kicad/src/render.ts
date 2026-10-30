/**
 * Sheet, board and library part images: kicad-cli's SVG export (text is stroked, so no fonts are
 * needed), framed by rewriting viewBoxes (SVG units are millimetres), then rasterised to PNG by
 * the host (omp's rasterizeSvg). Footprints and boards in 3D: KiCad's raytracer.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import type { Analysis } from "./analysis";
import { Board, courtyardBox } from "./board";
import * as cli from "./cli";
import { Copper, type Shape } from "./copper";
import { boardOutline, isEdgeItem } from "./edges";
import { type Box, bbox, type Pt } from "./geom";
import { describeFootprint, type FpModelInfo, kicadVars, type Libraries, splitId } from "./libs";
import { at, prop } from "./sch";
import { child, children, clone, flag, head, isList, L, type List, num, parse, print, Str, str, Sym, text } from "./sexpr";
import type { PartKind } from "./types";

/** omp's rasterizeSvg: draws at `scale` × the SVG's intrinsic size, then shrinks to fit the limits. */
export type Rasterize = (svg: Uint8Array, maxWidthPx: number, maxHeightPx: number, scale: number) => Promise<Uint8Array>;

/** SVG intrinsic pixels per millimetre (CSS 96 dpi). */
const PX_PER_MM = 96 / 25.4;

export interface RenderRequest {
	sheet: string;
	/** Region in sheet mm; default: around `refs`, or the whole sheet. */
	box?: Box;
	refs?: string[];
	/** Extra space around refs / boxes (mm). */
	margin?: number;
	/** Longest image side in pixels. */
	size?: number;
}

export interface RenderedImage {
	sheet: string;
	box: Box;
	png: Uint8Array;
}

/** An image with the caption the tools put above it ("Sheet Power", "Footprint Lib:Name"). */
export interface LabelledImage {
	label: string;
	png: Uint8Array;
}

export function cropSvg(svg: string, box: Box): string {
	const [x0, y0, x1, y1] = box;
	const w = x1 - x0;
	const h = y1 - y0;
	return svg.replace(/<svg\b[^>]*>/, tag =>
		tag
			.replace(/\swidth="[^"]*"/, ` width="${w.toFixed(4)}mm"`)
			.replace(/\sheight="[^"]*"/, ` height="${h.toFixed(4)}mm"`)
			.replace(/\sviewBox="[^"]*"/, ` viewBox="${x0.toFixed(4)} ${y0.toFixed(4)} ${w.toFixed(4)} ${h.toFixed(4)}"`),
	);
}

/** Box around the bodies, pins and visible fields of these references on one sheet. */
function refsBox(a: Analysis, sheetName: string, refs: string[]): Box {
	const inst = a.project.sheet(sheetName);
	const pts: [number, number][] = [];
	const missing: string[] = [];
	for (const ref of refs) {
		const syms = inst.sch.symbols().filter(s => inst.sch.refOf(s, inst.path) === ref);
		if (!syms.length) missing.push(ref);
		for (const s of syms) {
			const b = inst.sch.bodyBox(s);
			pts.push([b[0], b[1]], [b[2], b[3]]);
			for (const f of ["Reference", "Value"]) {
				const p = prop(s, f);
				if (p) pts.push([...at(p).p]);
			}
		}
	}
	if (missing.length) throw new Error(`not on sheet ${inst.name}: ${missing.join(", ")}`);
	return bbox(pts);
}

export async function renderSheet(a: Analysis, req: RenderRequest, rasterize: Rasterize): Promise<RenderedImage> {
	const inst = a.project.sheet(req.sheet);
	const [pw, ph] = inst.sch.paper();
	let box: Box = req.box ?? (req.refs?.length ? refsBox(a, inst.name, req.refs) : [0, 0, pw, ph]);
	if (req.box || req.refs?.length) {
		const m = req.margin ?? 8;
		box = [Math.max(0, box[0] - m), Math.max(0, box[1] - m), Math.min(pw, box[2] + m), Math.min(ph, box[3] + m)];
	}
	if (box[2] - box[0] < 1 || box[3] - box[1] < 1) throw new Error(`empty region ${JSON.stringify(box)}`);
	const svg = cropSvg(fs.readFileSync(await a.svgFile(inst.name), "utf8"), box);
	return { sheet: inst.name, box, png: await toPng(svg, box[2] - box[0], box[3] - box[1], rasterize, req.size) };
}

/** Rasterise an SVG of w × h mm so that its longest side is `size` px. */
function toPng(svg: string, w: number, h: number, rasterize: Rasterize, size = 1600): Promise<Uint8Array> {
	// The rasterizer never upscales past `scale`, so this makes the longest side exactly `size`.
	const scale = size / (Math.max(w, h) * PX_PER_MM);
	return rasterize(new TextEncoder().encode(svg), size, size, scale);
}

/** One image per touched sheet, framing the touched regions (whole sheet when none recorded). */
export async function renderRegions(
	a: Analysis,
	regions: { sheet: string; boxes: Box[] }[],
	rasterize: Rasterize,
): Promise<LabelledImage[]> {
	const out: LabelledImage[] = [];
	for (const r of regions) {
		const box = r.boxes.length ? bbox(r.boxes.flatMap(b => [[b[0], b[1]] as const, [b[2], b[3]] as const])) : undefined;
		const img = await renderSheet(a, { sheet: r.sheet, box, margin: 10 }, rasterize);
		out.push({ label: `Sheet ${img.sheet}`, png: img.png });
	}
	return out;
}

// ---- library parts -----------------------------------------------------------------------------

/**
 * Parts are plotted with KiCad's built-in theme, whatever the user's editor themes are, on that
 * theme's background: KiCad's part SVGs have none, and the board colours need a dark one.
 */
const PART_THEME = "_builtin_default";
const BACKGROUND: Record<PartKind, string> = { symbol: "#F5F4EF", footprint: "#001023" };

/** Back copper under front copper, silkscreen, courtyard, then fabrication with numbered pad outlines on top. */
export const DEFAULT_FP_LAYERS = ["B.Cu", "F.Cu", "F.SilkS", "F.CrtYd", "F.Fab"];

/** Radius (mm) of the probe circle that tells where the part coordinates land in KiCad's plot. */
const PROBE_R = 0.0371;

export interface PartRenderRequest {
	kind: PartKind;
	/** "Lib:Name", stock or project. */
	id: string;
	/** Symbols: one unit (default every unit, laid out in reading order). */
	unit?: number;
	/** Symbols: 1, or 2 for De Morgan (default 1). */
	body_style?: number;
	/** Footprints: layers to plot, bottom first (default DEFAULT_FP_LAYERS). */
	layers?: string[];
	/** Region in part coordinates (mm; symbols y down); default the drawn extent of each unit. */
	box?: Box;
	/** Extra space around the box or drawn extent (mm). */
	margin?: number;
	/** Longest image side in pixels. */
	size?: number;
}

export interface RenderedPart {
	kind: PartKind;
	id: string;
	/** Region drawn per unit (footprints: one, without unit), in part coordinates. */
	frames: { unit?: number; box: Box }[];
	png: Uint8Array;
}

/** One plot of KiCad's: its SVG, where part coordinates land in it, and KiCad's extent of the part. */
interface Plot {
	unit?: number;
	svg: string;
	/** SVG position = part position + offset. */
	offset: Pt;
	/** Bounding box KiCad fitted the page to, in part coordinates. */
	extent: Box;
}

/** Points that drawn items pass through (direct `at`/`start`/`end`/`mid`/`center` and `pts`). */
function itemPoints(items: List[]): Pt[] {
	const out: Pt[] = [];
	for (const item of items) {
		for (const c of item) {
			if (!isList(c)) continue;
			const h = head(c);
			if (h === "at" || h === "start" || h === "end" || h === "mid" || h === "center") out.push([num(c, 1), num(c, 2)]);
			else if (h === "pts") for (const xy of children(c, "xy")) out.push([num(xy, 1), num(xy, 2)]);
		}
	}
	return out;
}

/**
 * A probe position inside KiCad's bounding box of the part (the centre of points the part's
 * items pass through), so the probe doesn't move the plot's page. Rounded to what KiCad keeps.
 */
function probeAt(pts: Pt[]): Pt {
	if (!pts.length) return [0, 0];
	const [x0, y0, x1, y1] = bbox(pts);
	return [Math.round((x0 + x1) * 50) / 100, Math.round((y0 + y1) * 50) / 100];
}

/** Size (mm) of a KiCad SVG page: its root viewBox, which always starts at 0 0. */
function pageSize(svg: string): Pt {
	const vb = /<svg\b[^>]*\sviewBox="([^"]*)"/.exec(svg)?.[1]?.trim().split(/\s+/).map(Number) ?? [];
	if (vb.length !== 4 || vb.some(Number.isNaN)) throw new Error("KiCad's plot has no viewBox");
	return [vb[2]!, vb[3]!];
}

/** Remove the probe circle from a plot; returns the SVG without it and the probe's SVG position. */
function takeProbe(svg: string): { svg: string; at: Pt } {
	const hits = [...svg.matchAll(/<circle cx="(-?[\d.]+)" cy="(-?[\d.]+)" r="([\d.]+)" \/>/g)].filter(m => Math.abs(Number(m[3]) - PROBE_R) < 2e-4);
	if (hits.length !== 1) throw new Error(`can't place the part in KiCad's plot: ${hits.length} probe marks`);
	const m = hits[0]!;
	return { svg: svg.slice(0, m.index) + svg.slice(m.index + m[0].length), at: [Number(m[1]), Number(m[2])] };
}

/** Unit and body style of a sub-symbol name ("Name_2_1"). */
function unitOf(sub: List): [number, number] {
	const m = /_(\d+)_(\d+)$/.exec(text(sub) ?? "");
	if (!m) throw new Error(`unexpected sub-symbol name "${text(sub)}"`);
	return [Number(m[1]), Number(m[2])];
}

/**
 * Plot the requested units of a symbol. The symbol is flattened into a scratch library as
 * "part" with a probe circle in each unit; KiCad fits each unit's page to 1.2 × the unit's
 * bounding box, centred, so the box is the middle 10/12 of the page.
 */
async function plotSymbol(libs: Libraries, req: PartRenderRequest, dir: string, o: cli.RunOpts): Promise<Plot[]> {
	const [nick] = splitId(req.id);
	const version = child(libs.symLibTree(nick), "version");
	if (!version) throw new Error(`symbol library ${nick} has no format version`);
	const sym = libs.flatSymbol(req.id);
	sym[1] = str("part");
	const subs = children(sym, "symbol");
	for (const s of subs) s[1] = str(`part_${unitOf(s).join("_")}`);
	const units = Math.max(1, ...subs.map(s => unitOf(s)[0]));

	// body styles and the file name suffix KiCad gives each (eeschema_jobs_handler doSymExportSvg)
	const bs = child(sym, "body_styles");
	const demorgan = bs?.[1] instanceof Sym && bs[1].v === "demorgan";
	const names = bs && !demorgan ? bs.slice(1).filter((a): a is Str => a instanceof Str).map(a => a.v) : [];
	const bodies = demorgan ? 2 : Math.max(1, names.length);
	const body = req.body_style ?? 1;
	if (!Number.isInteger(body) || body < 1 || body > bodies) throw new Error(`body_style ${body}: ${req.id} has ${bodies === 1 ? "one body style" : `body styles 1..${bodies}`}`);
	const suffix = demorgan ? (body === 2 ? "_demorgan" : "") : names.length ? `_${names[body - 1]!.toLowerCase()}` : "";
	if (req.unit !== undefined && (!Number.isInteger(req.unit) || req.unit < 1 || req.unit > units)) {
		throw new Error(`unit ${req.unit}: ${req.id} has ${units === 1 ? "one unit" : `units 1..${units}`}`);
	}
	const want = req.unit !== undefined ? [req.unit] : Array.from({ length: units }, (_, i) => i + 1);

	const probes = new Map<number, Pt>();
	for (const u of want) {
		// items KiCad draws for unit u (common items: unit 0; every item when there is one unit)
		const drawn = subs.filter(s => {
			const [su, sb] = unitOf(s);
			return (su === 0 || units === 1 || su === u) && (sb === 0 || sb === body);
		});
		// library files are y up; part coordinates here are sheet orientation (y down)
		const p = probeAt(itemPoints(drawn.flatMap(s => s.filter(isList))).map(([x, y]) => [x, -y] as const));
		probes.set(u, p);
		const name = `part_${u}_${body}`;
		let sub = subs.find(s => text(s) === name);
		if (!sub) {
			sub = L("symbol", str(name));
			sym.push(sub);
		}
		sub.push(
			L("circle", L("center", p[0], -p[1]), L("radius", PROBE_R), L("stroke", L("width", 0.01), L("type", "default")), L("fill", L("type", "none"))),
		);
	}
	const lib = path.join(dir, "part.kicad_sym");
	fs.writeFileSync(lib, `${print(L("kicad_symbol_lib", clone(version), L("generator", str("kicad_toolkit")), sym))}\n`);
	const out = path.join(dir, "out");
	await cli.symSvg(lib, "part", PART_THEME, out, o);

	return want.map(u => {
		const file = path.join(out, `part_unit${u}${suffix}.svg`);
		if (!fs.existsSync(file)) throw new Error(`kicad-cli sym export svg wrote no plot of unit ${u} of ${req.id}`);
		const probe = takeProbe(fs.readFileSync(file, "utf8"));
		const p = probes.get(u)!;
		const offset: Pt = [probe.at[0] - p[0], probe.at[1] - p[1]];
		const [w, h] = pageSize(probe.svg);
		const extent: Box = [w / 12 - offset[0], h / 12 - offset[1], (w * 11) / 12 - offset[0], (h * 11) / 12 - offset[1]];
		return { unit: u, svg: probe.svg, offset, extent };
	});
}

/**
 * Plot a footprint from a scratch .pretty copy holding a probe circle. KiCad places it in an
 * empty board at the origin and fits the page exactly to the board's bounding box.
 */
async function plotFootprint(libs: Libraries, req: PartRenderRequest, dir: string, o: cli.RunOpts): Promise<Plot> {
	const layers = req.layers?.length ? req.layers : DEFAULT_FP_LAYERS;
	const fp = parse(fs.readFileSync(libs.footprintFile(req.id), "utf8"));
	fp[1] = str("part");
	const items = fp.filter(isList).filter(c => head(c) === "pad" || (head(c)?.startsWith("fp_") && head(c) !== "fp_text"));
	const p = probeAt(itemPoints(items));
	// on a plotted drawing layer: that's where KiCad draws a circle as a plain <circle>
	const layer = layers.find(l => /\.(Fab|SilkS|CrtYd)$|^User\./.test(l)) ?? layers[0]!;
	fp.push(L("fp_circle", L("center", p[0], p[1]), L("end", p[0] + PROBE_R, p[1]), L("stroke", L("width", 0.01), L("type", "solid")), L("fill", "no"), L("layer", str(layer))));
	const pretty = path.join(dir, "part.pretty");
	fs.mkdirSync(pretty);
	fs.writeFileSync(path.join(pretty, "part.kicad_mod"), `${print(fp)}\n`);
	const file = await cli.fpSvg(pretty, "part", layers, PART_THEME, path.join(dir, "out"), o);
	const probe = takeProbe(fs.readFileSync(file, "utf8"));
	const offset: Pt = [probe.at[0] - p[0], probe.at[1] - p[1]];
	const [w, h] = pageSize(probe.svg);
	return { svg: probe.svg, offset, extent: [-offset[0], -offset[1], w - offset[0], h - offset[1]] };
}

const f4 = (v: number) => v.toFixed(4);

/** The children of a plot's root <svg>. */
function svgBody(svg: string): string {
	const open = /<svg\b[^>]*>/.exec(svg);
	const end = svg.lastIndexOf("</svg>");
	if (!open || end < 0) throw new Error("KiCad's plot is not an SVG document");
	return svg.slice(open.index + open[0].length, end);
}

/** Lay framed plots out in a grid, in reading order, on one background; size in mm. */
function composeSvg(cells: { plot: Plot; frame: Box }[], background: string): { svg: string; w: number; h: number } {
	const cols = Math.ceil(Math.sqrt(cells.length));
	const parts: string[] = [];
	const borders: string[] = [];
	let w = 0;
	let y = 0;
	for (let r = 0; r * cols < cells.length; r++) {
		let x = 0;
		let rowH = 0;
		for (const { plot, frame } of cells.slice(r * cols, (r + 1) * cols)) {
			const [fw, fh] = [frame[2] - frame[0], frame[3] - frame[1]];
			const vb = [frame[0] + plot.offset[0], frame[1] + plot.offset[1], fw, fh].map(f4).join(" ");
			parts.push(`<svg x="${f4(x)}" y="${f4(y)}" width="${f4(fw)}" height="${f4(fh)}" viewBox="${vb}">${svgBody(plot.svg)}</svg>`);
			borders.push(`<rect x="${f4(x)}" y="${f4(y)}" width="${f4(fw)}" height="${f4(fh)}"/>`);
			x += fw;
			rowH = Math.max(rowH, fh);
		}
		w = Math.max(w, x);
		y += rowH;
	}
	const h = y;
	// the size as written, so the scale makes the longest side exactly the requested pixels
	const [ws, hs] = [f4(w), f4(h)];
	// unit cells get a thin grey outline so units read apart
	const outline =
		cells.length > 1 ? `<g fill="none" stroke="#909090" stroke-width="${f4(Math.max(w, h) / 600)}">${borders.join("")}</g>` : "";
	const svg = [
		`<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" version="1.1" width="${ws}mm" height="${hs}mm" viewBox="0 0 ${ws} ${hs}">`,
		`<rect width="${ws}" height="${hs}" fill="${background}"/>`,
		...parts,
		outline,
		"</svg>",
	].join("\n");
	return { svg, w: Number(ws), h: Number(hs) };
}

/**
 * Image of a library part, framed per unit on `box` or on KiCad's own extent of the part, plus
 * `margin`. Symbol units are laid out in a grid in reading order.
 */
export async function renderPart(libs: Libraries, req: PartRenderRequest, rasterize: Rasterize, o: cli.RunOpts): Promise<RenderedPart> {
	const dir = cli.tmpDir("part");
	try {
		const plots = req.kind === "symbol" ? await plotSymbol(libs, req, dir, o) : [await plotFootprint(libs, req, dir, o)];
		const m = req.margin ?? 2;
		const cells = plots.map(plot => {
			const b = req.box ?? plot.extent;
			const frame: Box = [b[0] - m, b[1] - m, b[2] + m, b[3] + m];
			if (!(frame[2] > frame[0] && frame[3] > frame[1])) throw new Error(`empty region ${JSON.stringify(frame)}`);
			return { plot, frame };
		});
		const { svg, w, h } = composeSvg(cells, BACKGROUND[req.kind]);
		return {
			kind: req.kind,
			id: req.id,
			frames: cells.map(c => ({ unit: c.plot.unit, box: c.frame })),
			png: await toPng(svg, w, h, rasterize, req.size),
		};
	} finally {
		fs.rmSync(dir, { recursive: true, force: true });
	}
}

// ---- footprints in 3D --------------------------------------------------------------------------

export const SIDES = ["top", "bottom", "left", "right", "front", "back"] as const;

export interface Footprint3dRequest {
	/** "Lib:Name", stock or project. */
	id: string;
	side?: (typeof SIDES)[number];
	/** Board rotation [x, y, z] in degrees, e.g. [-45, 0, 45] for isometric. */
	rotate?: readonly number[];
	/** Default: the whole board with a border, 0.85, or 0.7 when rotated. */
	zoom?: number;
	perspective?: boolean;
	/** Image side in pixels (square). */
	size?: number;
}

/** A footprint's 3D model, and where its file is for this project. */
export interface ModelInfo extends FpModelInfo {
	/** Variables expanded, absolute. */
	resolved: string;
	exists: boolean;
	hidden: boolean;
}

export interface Rendered3d {
	models: ModelInfo[];
	/** The scratch board's Edge.Cuts rectangle, footprint coordinates (mm). */
	board: Box;
	/** Camera zoom used. */
	zoom: number;
	png: Uint8Array;
}

/** Space between the courtyard (or pads and drawings) and the scratch board's edge, mm. */
const BOARD_MARGIN = 1;

/** Extent of a footprint: its courtyard, or its pads and drawings when it has none. */
function footprintExtent(fp: List): Box {
	const graphics = fp.filter(isList).filter(c => head(c)?.startsWith("fp_") && head(c) !== "fp_text");
	const onCourtyard = graphics.filter(g => /CrtYd$/.test(text(child(g, "layer")) ?? ""));
	const items = onCourtyard.length ? onCourtyard : graphics;
	const pts = [...itemPoints(items)];
	// a circle reaches its radius around the centre
	for (const c of items.filter(g => head(g) === "fp_circle")) {
		const [cx, cy] = [num(child(c, "center"), 1), num(child(c, "center"), 2)];
		const r = Math.hypot(num(child(c, "end"), 1) - cx, num(child(c, "end"), 2) - cy);
		pts.push([cx - r, cy - r], [cx + r, cy + r]);
	}
	if (!onCourtyard.length) {
		for (const p of children(fp, "pad")) {
			const [x, y] = [num(child(p, "at"), 1), num(child(p, "at"), 2)];
			const r = Math.hypot(num(child(p, "size"), 1), num(child(p, "size"), 2)) / 2;
			pts.push([x - r, y - r], [x + r, y + r]);
		}
	}
	if (!pts.length) throw new Error("the footprint has no courtyard, pads or drawings to put a board around");
	return bbox(pts);
}

/** At zoom 1 KiCad's camera spans about the board's longest side; these leave a border. */
const ZOOM_FLAT = 0.85;
const ZOOM_ROTATED = 0.7;

/** The raytracer settings of a 3D request, checked: camera side, zoom (default by rotation) and image size. */
function camera(req: Omit<Footprint3dRequest, "id">): cli.Render3dOpts {
	if (req.rotate && (req.rotate.length !== 3 || req.rotate.some(v => !Number.isFinite(v)))) throw new Error("rotate must be [x, y, z] in degrees");
	const zoom = req.zoom ?? (req.rotate ? ZOOM_ROTATED : ZOOM_FLAT);
	if (!(zoom > 0)) throw new Error("zoom must be > 0");
	const size = req.size ?? 1600;
	if (!Number.isInteger(size) || size < 32 || size > 4000) throw new Error("size must be a whole number of pixels, 32..4000");
	return { size, side: req.side ?? "top", rotate: req.rotate, zoom, perspective: req.perspective ?? false };
}

/**
 * A footprint in 3D: placed at the origin of a scratch 1.6 mm board whose outline is the
 * courtyard plus a margin, with model paths made absolute (`${KIPRJMOD}` is the real project
 * directory, other variables come from the dev shell's KiCad), rendered by `kicad-cli pcb render`.
 */
export async function renderFootprint3d(libs: Libraries, req: Footprint3dRequest, o: cli.RunOpts): Promise<Rendered3d> {
	const cam = camera(req);

	const fp = parse(fs.readFileSync(libs.footprintFile(req.id), "utf8"));
	const vars: Record<string, string> = { ...(await kicadVars(o.repoRoot)), KIPRJMOD: libs.projectDir };
	const nodes = children(fp, "model");
	const models = describeFootprint(fp).models.map((info, i) => {
		const expanded = info.path.replace(/\$\{([A-Za-z0-9_]+)\}/g, (v, k: string) => vars[k] ?? v);
		// KiCad resolves a relative path against the project directory first
		const resolved = expanded.includes("${") ? expanded : path.resolve(libs.projectDir, expanded);
		nodes[i]![1] = str(resolved);
		return { ...info, resolved, exists: !resolved.includes("${") && fs.existsSync(resolved), hidden: flag(nodes[i]!, "hide") === true };
	});
	const [x0, y0, x1, y1] = footprintExtent(fp);
	const r2 = (v: number) => Math.round(v * 100) / 100;
	const board: Box = [r2(x0 - BOARD_MARGIN), r2(y0 - BOARD_MARGIN), r2(x1 + BOARD_MARGIN), r2(y1 + BOARD_MARGIN)];

	// the board takes the footprint's file format version; the footprint keeps the rest
	const version = child(fp, "version");
	const placed = fp.filter(c => !(isList(c) && ["version", "generator", "generator_version"].includes(head(c) ?? "")));
	const pcb = L(
		"kicad_pcb",
		...(version ? [version] : []),
		L("generator", str("kicad_toolkit")),
		L("general", L("thickness", 1.6)),
		L("gr_rect", L("start", board[0], board[1]), L("end", board[2], board[3]), L("stroke", L("width", 0.05), L("type", "solid")), L("fill", "no"), L("layer", str("Edge.Cuts"))),
		placed,
	);
	const dir = cli.tmpDir("3d");
	try {
		const file = path.join(dir, "part.kicad_pcb");
		fs.writeFileSync(file, `${print(pcb)}\n`);
		const png = await cli.pcbRender(file, cam, { ...o, cwd: dir });
		return { models, board, zoom: cam.zoom, png };
	} finally {
		fs.rmSync(dir, { recursive: true, force: true });
	}
}

// ---- boards -------------------------------------------------------------------------------------

/** Layers a board image draws by default, bottom first, for the side looked at. */
export const DEFAULT_BOARD_LAYERS = {
	top: ["Edge.Cuts", "F.Cu", "F.Courtyard", "F.Fab", "F.Silkscreen"],
	bottom: ["Edge.Cuts", "B.Cu", "B.Courtyard", "B.Fab", "B.Silkscreen"],
} as const;

/** What a board edit's image shows: the outline, copper and courtyards of both sides. */
export const EDIT_BOARD_LAYERS = ["Edge.Cuts", "B.Cu", "F.Cu", "B.Courtyard", "F.Courtyard"];

/** `layers: ["copper"]` stands for the outline and every copper layer, back to front. */
export const COPPER_PRESET = "copper";

/** Highlight colour of the selected nets' copper and of unrouted connections, over KiCad's plot. */
const HIGHLIGHT = "#FFE14D";
const RATSNEST = "#FFFFFF";

export interface BoardRenderRequest {
	/** Layers to plot, bottom first (default DEFAULT_BOARD_LAYERS of `side`); "copper" expands to Edge.Cuts and every copper layer, back to front. */
	layers?: string[];
	side?: "top" | "bottom";
	/** Region in board mm; default the outline's bbox, or every footprint's courtyard when there is no outline. */
	box?: Box;
	/** Extra space around the box (mm, default 3). */
	margin?: number;
	size?: number;
	/** Nets whose pads, tracks, arcs, vias and zone fills are drawn in the highlight colour over the plot. */
	nets?: string[];
	/** Draw the unrouted connections (of `nets`, or of every net) as thin lines. */
	ratsnest?: boolean;
}

export interface RenderedBoard {
	box: Box;
	layers: string[];
	png: Uint8Array;
	/** The highlighted nets (as the board names them) and how many copper items were drawn. */
	highlighted?: { nets: string[]; items: number };
	/** Unrouted connections drawn. */
	ratsnest?: number;
}

/** One copper shape as SVG in board mm: a disc, a stadium, or a (rounded) polygon. */
function shapeSvg(s: Shape, colour: string): string {
	const pts = s.pts;
	if (pts.length === 1) return `<circle cx="${f4(pts[0]![0])}" cy="${f4(pts[0]![1])}" r="${f4(s.r)}" fill="${colour}"/>`;
	if (pts.length === 2) {
		return `<line x1="${f4(pts[0]![0])}" y1="${f4(pts[0]![1])}" x2="${f4(pts[1]![0])}" y2="${f4(pts[1]![1])}" stroke="${colour}" stroke-width="${f4(2 * s.r)}" stroke-linecap="round"/>`;
	}
	const stroke = s.r > 0 ? ` stroke="${colour}" stroke-width="${f4(2 * s.r)}" stroke-linejoin="round"` : "";
	return `<polygon points="${pts.map(p => `${f4(p[0])},${f4(p[1])}`).join(" ")}" fill="${colour}"${stroke}/>`;
}

/**
 * The selected nets' copper in the highlight colour, then the unrouted connections as thin lines
 * between the pads' copper centres. Only copper on a plotted layer is drawn: fills, tracks and
 * drawings on one, pads and vias that reach one.
 */
function copperOverlay(c: Copper, nets: string[], plotted: string[], ratsnest: boolean, lineWidth: number): { svg: string; items: number; rats: number } {
	const parts: string[] = [];
	let items = 0;
	const shown = (layers: string[]) => layers.some(l => plotted.includes(l));
	for (const net of nets) {
		const k = c.connectivity(net);
		// fills under tracks, drawings, vias and pads
		const drawn = [
			...k.fills.filter(f => shown([f.layer])).map(f => [f.shape]),
			...k.tracks.filter(t => shown([t.layer])).map(t => t.shapes),
			...k.drawings.filter(d => shown([d.layer])).map(d => d.shapes),
			...k.vias.filter(v => shown(v.span)).map(v => v.shapes),
			...k.pads.filter(p => shown(p.copper)).map(p => p.shapes),
		];
		for (const shapes of drawn) for (const s of shapes) parts.push(shapeSvg(s, HIGHLIGHT));
		items += drawn.length;
	}
	let rats = 0;
	if (ratsnest) {
		for (const net of nets.length ? nets : c.nets()) {
			for (const r of c.connectivity(net).unrouted) {
				const [a, b] = [r.from.centre, r.to.centre];
				parts.push(`<line x1="${f4(a[0])}" y1="${f4(a[1])}" x2="${f4(b[0])}" y2="${f4(b[1])}" stroke="${RATSNEST}" stroke-width="${f4(lineWidth)}"/>`);
				rats++;
			}
		}
	}
	return { svg: `<g opacity="0.75">${parts.join("")}</g>`, items, rats };
}

/** The board region a default image frames: the outline, or the footprints when it has none. */
function boardExtent(tree: List): Box | undefined {
	const outline = boardOutline(tree.filter((c): c is List => isList(c) && isEdgeItem(c)));
	if (outline.bbox) return outline.bbox;
	const boxes = children(tree, "footprint").flatMap(fp => courtyardBox(fp)?.box ?? []);
	return boxes.length ? bbox(boxes.flatMap((b): Pt[] => [[b[0], b[1]], [b[2], b[3]]])) : undefined;
}

/**
 * A 2D image of a board's layers: kicad-cli's SVG plot at the page size (SVG units are board mm,
 * so a board point is drawn at its own coordinates), framed on `box` + margin, on the board colours' dark background.
 * KiCad's plot carries no nets, so the highlighted nets and the ratsnest are drawn over it from the copper model.
 */
export async function renderBoard(boardFile: string, req: BoardRenderRequest, rasterize: Rasterize, o: cli.RunOpts): Promise<RenderedBoard> {
	const board = Board.load(boardFile);
	const layers = (req.layers?.length ? req.layers : [...DEFAULT_BOARD_LAYERS[req.side ?? "top"]]).flatMap(l =>
		l === COPPER_PRESET ? ["Edge.Cuts", ...board.copperLayers().reverse()] : [l],
	);
	const copper = req.nets?.length || req.ratsnest ? Copper.of(board) : undefined;
	const nets = copper ? (req.nets ?? []).map(n => copper.net(n)) : [];
	const extent = req.box ?? boardExtent(board.tree);
	if (!extent) throw new Error("the board has no outline and no footprints: give a box");
	const m = req.margin ?? 3;
	const box: Box = [extent[0] - m, extent[1] - m, extent[2] + m, extent[3] + m];
	if (!(box[2] - box[0] > 0 && box[3] - box[1] > 0)) throw new Error(`empty region ${JSON.stringify(box)}`);
	const dir = cli.tmpDir("board");
	try {
		const file = await cli.pcbSvg(boardFile, layers, PART_THEME, path.join(dir, "board.svg"), o);
		const [w, h] = [box[2] - box[0], box[3] - box[1]];
		const overlay = copper ? copperOverlay(copper, nets, layers, req.ratsnest === true, Math.max(w, h) / 800) : undefined;
		const svg = [
			`<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" version="1.1" width="${f4(w)}mm" height="${f4(h)}mm" viewBox="${[box[0], box[1], w, h].map(f4).join(" ")}">`,
			`<rect x="${f4(box[0])}" y="${f4(box[1])}" width="${f4(w)}" height="${f4(h)}" fill="${BACKGROUND.footprint}"/>`,
			svgBody(fs.readFileSync(file, "utf8")),
			overlay?.svg ?? "",
			"</svg>",
		].join("\n");
		return {
			box,
			layers,
			png: await toPng(svg, w, h, rasterize, req.size),
			...(nets.length ? { highlighted: { nets, items: overlay!.items } } : {}),
			...(req.ratsnest ? { ratsnest: overlay!.rats } : {}),
		};
	} finally {
		fs.rmSync(dir, { recursive: true, force: true });
	}
}

/**
 * A board in 3D with its footprints' models: a temp copy whose `${KIPRJMOD}` model paths point
 * at the project directory, raytraced by `kicad-cli pcb render`.
 */
export async function renderBoard3d(boardFile: string, projectDir: string, req: Omit<Footprint3dRequest, "id">, o: cli.RunOpts): Promise<{ zoom: number; png: Uint8Array }> {
	const cam = camera(req);
	const tree = parse(fs.readFileSync(boardFile, "utf8"));
	for (const fp of children(tree, "footprint")) {
		for (const m of children(fp, "model")) m[1] = str((text(m) ?? "").replace(/\$\{KIPRJMOD\}/g, projectDir));
	}
	const dir = cli.tmpDir("board3d");
	try {
		const file = path.join(dir, "board.kicad_pcb");
		fs.writeFileSync(file, `${print(tree)}\n`);
		const png = await cli.pcbRender(file, cam, { ...o, cwd: dir });
		return { zoom: cam.zoom, png };
	} finally {
		fs.rmSync(dir, { recursive: true, force: true });
	}
}
