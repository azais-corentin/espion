/**
 * Hierarchy ops: sheets (blocks on the parent, their files, the .kicad_pro sheet list), sheet pins
 * and page settings. Blocks and pins are written the way KiCad 10 writes them (see the root sheet
 * of espion-cam).
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { type Box, key, onGrid, onInterior, overlap, type Pt, pt, samePt } from "../geom";
import { dragWireEnds, gridPt, sheetArg } from "../edit-util";
import type { Project, SheetInst } from "../project";
import { newUuid, PAPER, prop, Sch, type SheetBlock, type SheetPin } from "../sch";
import { child, children, head, isList, L, type List, num, removeChild, setChild, str, text } from "../sexpr";
import type { OpContext, OpDef } from "../types";

type Side = "left" | "right" | "top" | "bottom";

const SIDES = ["left", "right", "top", "bottom"] as const;
const SHAPES = ["input", "output", "bidirectional", "tri_state", "passive"] as const;

/** Pin angle and text justification per block side, as KiCad writes sheet pins. */
const SIDE_ROT: Record<Side, number> = { left: 180, right: 0, top: 90, bottom: 270 };
const SIDE_JUSTIFY: Record<Side, string> = { left: "left", right: "right", top: "right", bottom: "left" };
const ROT_SIDE: Record<number, Side> = { 180: "left", 0: "right", 90: "top", 270: "bottom" };

/** Autoplaced Sheetname baseline above the block and Sheetfile top below it (KiCad 10, 1.27 mm text). */
const NAME_DY = 0.7116;
const FILE_DY = 0.5846;
/** Border of the default worksheet frame. */
const FRAME = 10;

interface BlockRef {
	inst: SheetInst;
	parent: SheetInst;
	block: SheetBlock;
}

function blockArg(ctx: OpContext, v: unknown): BlockRef {
	const inst = sheetArg(ctx, v);
	if (!inst.parent || !inst.block) throw new Error("the root sheet has no sheet block; give a sub-sheet");
	const node = inst.block.node;
	const block = inst.parent.sch.sheets().find(b => b.node === node);
	if (!block) throw new Error(`the block of sheet ${inst.name} is gone from ${path.basename(inst.parent.file)}`);
	return { inst, parent: inst.parent, block };
}

function sideOf(p: SheetPin): Side {
	const s = ROT_SIDE[((p.rot % 360) + 360) % 360];
	if (!s) throw new Error(`sheet pin ${p.name} has an unsupported angle ${p.rot}`);
	return s;
}

function sideLength(size: Pt, side: Side): number {
	return side === "left" || side === "right" ? size[1] : size[0];
}

function pinOffset(origin: Pt, p: SheetPin, side: Side): number {
	return side === "left" || side === "right" ? p.at[1] - origin[1] : p.at[0] - origin[0];
}

function pinPoint(origin: Pt, size: Pt, side: Side, offset: number): Pt {
	const [x, y] = origin;
	const [w, h] = size;
	const SPOT: Record<Side, Pt> = {
		left: [x, y + offset],
		right: [x + w, y + offset],
		top: [x + offset, y],
		bottom: [x + offset, y + h],
	};
	return pt(...SPOT[side]);
}

function blockBox(origin: Pt, size: Pt): Box {
	return [origin[0], origin[1], origin[0] + size[0], origin[1] + size[1]];
}

function sizeArg(ctx: OpContext, v: unknown): Pt {
	const s = gridPt(ctx, v, "size");
	if (s[0] <= 0 || s[1] <= 0) throw new Error(`size (${s.join(", ")}) must be positive`);
	return s;
}

/** A sheet name that is valid and not used by another sheet (names select sheets, so they stay unique). */
function sheetName(project: Project, v: unknown, self?: string): string {
	const name = String(v).trim();
	if (!name || name.includes("/")) throw new Error(`invalid sheet name "${name}": give a non-empty name without "/"`);
	const clash = project.hierarchy().find(i => i.block && i.block.uuid !== self && i.name.toLowerCase() === name.toLowerCase());
	if (clash) throw new Error(`a sheet named "${clash.name}" already exists (${clash.namePath}); choose another name`);
	return name;
}

/** The block must sit inside the frame (with its name and file fields) and clear of other blocks and symbols. */
function checkPlace(parent: SheetInst, box: Box, self?: List): void {
	const [pw, ph] = parent.sch.paper();
	const [x0, y0, x1, y1] = box;
	if (x0 < FRAME || y0 - 2.54 < FRAME || x1 > pw - FRAME || y1 + 2.54 > ph - FRAME) {
		throw new Error(
			`the block (${x0}, ${y0})-(${x1}, ${y1}) with its name and file fields leaves the ${pw} x ${ph} mm frame of ${parent.name} ` +
				`(keep it within (${FRAME}, ${FRAME + 2.54})-(${pw - FRAME}, ${ph - FRAME - 2.54}))`,
		);
	}
	for (const b of parent.sch.sheets()) {
		if (b.node !== self && overlap(box, blockBox(b.at, b.size), 2.54)) {
			throw new Error(`the block would overlap (or come within 2.54 mm of) sheet ${b.name} at (${b.at.join(", ")}) size (${b.size.join(", ")})`);
		}
	}
	for (const s of parent.sch.symbols()) {
		if (overlap(box, parent.sch.bodyBox(s), 1.27)) throw new Error(`the block would overlap symbol ${parent.sch.refOf(s, parent.path)} on ${parent.name}`);
	}
}

function sheetField(name: string, value: string, p: Pt, vjustify: "top" | "bottom"): List {
	return L(
		"property",
		str(name),
		str(value),
		L("at", p[0], p[1], 0),
		L("show_name", false),
		L("do_not_autoplace", false),
		L("effects", L("font", L("size", 1.27, 1.27)), L("justify", "left", vjustify)),
	);
}

/** Every page number used by a sheet block instance or the root, across the loaded hierarchy. */
function nextPage(project: Project): number {
	let max = 0;
	const seen = new Set<Sch>();
	for (const inst of project.hierarchy()) {
		if (seen.has(inst.sch)) continue;
		seen.add(inst.sch);
		const paths = [
			...children(child(inst.sch.tree, "sheet_instances") ?? [], "path"),
			...inst.sch
				.items("sheet")
				.flatMap(s => children(child(s, "instances") ?? [], "project"))
				.flatMap(p => children(p, "path")),
		];
		for (const p of paths) {
			const n = Number.parseInt(text(child(p, "page")) ?? "", 10);
			if (n > max) max = n;
		}
	}
	return max + 1;
}

/** The .kicad_pro `sheets` list: `[uuid, name]` entries. */
function proSheets(project: Project): unknown[] {
	const list = project.proJson.data.sheets;
	if (Array.isArray(list)) return list;
	const fresh: unknown[] = [];
	project.proJson.data.sheets = fresh;
	return fresh;
}

const isEntry = (e: unknown, uuid: string): e is unknown[] => Array.isArray(e) && e[0] === uuid;

function pinNode(name: string, shape: string, p: Pt, side: Side): List {
	return L(
		"pin",
		str(name),
		shape,
		L("at", p[0], p[1], SIDE_ROT[side]),
		L("uuid", str(newUuid())),
		L("effects", L("font", L("size", 1.27, 1.27)), L("justify", SIDE_JUSTIFY[side])),
	);
}

/** Add a pin to the block (before its instances, where KiCad writes pins). */
function insertPin(block: SheetBlock, pin: List): void {
	const i = block.node.findIndex(x => isList(x) && head(x) === "instances");
	block.node.splice(i >= 0 ? i : block.node.length, 0, pin);
}

/** Wire ends left without a partner where pins were removed. */
function danglingNote(sch: Sch, pins: SheetPin[], what: string): string | undefined {
	const loose = pins.filter(p => sch.wires().some(w => samePt(w.a, p.at) || samePt(w.b, p.at)));
	if (!loose.length) return undefined;
	return `${what}: wires now end unconnected at ${loose.map(p => `(${p.at.join(", ")}) [${p.name}]`).join(", ")}; remove or reconnect them`;
}

const createSheet: OpDef = {
	op: "create_sheet",
	group: "hierarchy",
	doc:
		"Create a sub-sheet: a new empty .kicad_sch next to the parent's file and its block on the parent (no pins yet; " +
		"add hierarchical labels in the new sheet, then add_sheet_pin or sync_sheet_pins). Later ops in the same call can use the new sheet by name.",
	fields: {
		parent: { t: "string", req: true, doc: "sheet that gets the block (\"root\" for the top sheet)" },
		name: { t: "string", req: true, doc: "sheet name, unique in the project, no \"/\"" },
		file: { t: "string", req: true, doc: "new file name next to the parent's file, e.g. \"usb.kicad_sch\"" },
		at: { t: "point", req: true, doc: "top-left corner of the block on the parent (grid)" },
		size: { t: "point", req: true, doc: "[width, height] of the block in mm (grid multiples)" },
		paper: { t: "string", values: Object.keys(PAPER), doc: "paper size of the new sheet (default A4, landscape)" },
		title: { t: "string", doc: "title block title of the new sheet" },
	},
	apply(ctx, a) {
		const { project } = ctx;
		const parent = sheetArg(ctx, a.parent);
		const name = sheetName(project, a.name);
		const raw = String(a.file).trim();
		const file = raw.endsWith(".kicad_sch") ? raw : `${raw}.kicad_sch`;
		if (!raw || file.includes("/") || file.includes("\\")) {
			throw new Error(`file "${raw}" must be a plain file name (it goes next to ${path.basename(parent.file)})`);
		}
		const abs = path.join(path.dirname(parent.file), file);
		if (fs.existsSync(abs) || project.files.has(abs)) throw new Error(`${project.rel(abs)} already exists; choose another file name`);
		const origin = gridPt(ctx, a.at, "at");
		const size = sizeArg(ctx, a.size);
		const box = blockBox(origin, size);
		checkPlace(parent, box);

		const sheet = Sch.empty(abs, typeof a.paper === "string" ? a.paper : "A4", typeof a.title === "string" ? a.title : undefined);
		// KiCad writes sheet_instances only into the root file
		removeChild(sheet.tree, "sheet_instances");
		const uuid = newUuid();
		let page = nextPage(project);
		const parentInsts = project.hierarchy().filter(i => i.file === parent.file);
		const pages: string[] = [];
		const paths = parentInsts.map(pi => {
			pages.push(String(page));
			return L("path", str(pi.path), L("page", str(String(page++))));
		});
		parent.sch.insert(
			L(
				"sheet",
				L("at", origin[0], origin[1]),
				L("size", size[0], size[1]),
				L("exclude_from_sim", false),
				L("in_bom", true),
				L("on_board", true),
				L("dnp", false),
				L("fields_autoplaced", true),
				L("stroke", L("width", 0.1524), L("type", "solid")),
				L("fill", L("color", 0, 0, 0, 0)),
				L("uuid", str(uuid)),
				sheetField("Sheetname", name, [origin[0], origin[1] - NAME_DY], "bottom"),
				sheetField("Sheetfile", file, [origin[0], origin[1] + size[1] + FILE_DY], "top"),
				L("instances", L("project", str(project.name), ...paths)),
			),
		);
		project.touch(parent.sch);
		project.addSch(sheet);
		proSheets(project).push([uuid, name]);
		project.touchPro();

		ctx.touch(parent, box);
		for (const inst of project.hierarchy()) if (inst.block?.uuid === uuid) ctx.touch(inst);
		ctx.notes.push(
			`created sheet ${name} (${project.rel(abs)}, page ${pages.join(", ")}) with its block at (${origin.join(", ")}) size (${size.join(", ")}) on ${parent.name}`,
		);
	},
};

const addSheetPin: OpDef = {
	op: "add_sheet_pin",
	group: "hierarchy",
	doc: "Add a pin to a sheet block for an existing hierarchical label of that sheet.",
	fields: {
		sheet: { t: "string", req: true, doc: "the sub-sheet whose block gets the pin" },
		name: { t: "string", req: true, doc: "hierarchical label name in that sheet" },
		shape: { t: "string", values: SHAPES, doc: "electrical shape (default: the label's shape)" },
		side: { t: "string", req: true, values: SIDES, doc: "block side" },
		offset: { t: "number", req: true, doc: "mm (grid) from the top corner on left/right sides, from the left corner on top/bottom" },
	},
	apply(ctx, a) {
		const { inst, parent, block } = blockArg(ctx, a.sheet);
		const name = String(a.name);
		const labels = inst.sch.labels().filter(l => l.kind === "hierarchical_label");
		const label = labels.find(l => l.name === name);
		if (!label) {
			throw new Error(
				`sheet ${inst.name} has no hierarchical label "${name}" ` +
					`(has: ${[...new Set(labels.map(l => l.name))].join(", ") || "none"}); add it first with add_label kind hierarchical on sheet ${inst.name}`,
			);
		}
		if (block.pins.some(p => p.name === name)) throw new Error(`the block of ${inst.name} already has a pin "${name}"`);
		const side = String(a.side) as Side;
		const offset = Number(a.offset);
		const g = ctx.project.settings.grid;
		const len = sideLength(block.size, side);
		if (!onGrid(offset, g) || offset <= 0 || offset >= len) {
			throw new Error(`offset ${offset} must be a multiple of ${g} mm strictly between 0 and ${len} (the ${side} side's length)`);
		}
		const p = pinPoint(block.at, block.size, side, offset);
		const taken = block.pins.find(q => samePt(q.at, p));
		if (taken) throw new Error(`pin ${taken.name} already sits at (${p.join(", ")}) on the ${side} side; choose another offset`);
		insertPin(block, pinNode(name, typeof a.shape === "string" ? a.shape : (label.shape ?? "passive"), p, side));
		ctx.project.touch(parent.sch);
		ctx.touch(parent, [p[0] - 2.54, p[1] - 2.54, p[0] + 2.54, p[1] + 2.54]);
		ctx.notes.push(`sheet pin ${inst.name}/${name} at (${p.join(", ")}) on ${parent.name}`);
	},
};

const removeSheetPin: OpDef = {
	op: "remove_sheet_pin",
	group: "hierarchy",
	doc: "Remove a pin from a sheet block (wires to it stay and are reported).",
	fields: {
		sheet: { t: "string", req: true, doc: "the sub-sheet whose block has the pin" },
		name: { t: "string", req: true, doc: "pin name" },
	},
	apply(ctx, a) {
		const { inst, parent, block } = blockArg(ctx, a.sheet);
		const pin = block.pins.find(p => p.name === String(a.name));
		if (!pin) throw new Error(`the block of ${inst.name} has no pin "${a.name}" (pins: ${block.pins.map(p => p.name).join(", ") || "none"})`);
		block.node.splice(block.node.indexOf(pin.node), 1);
		ctx.project.touch(parent.sch);
		ctx.touch(parent, [pin.at[0] - 2.54, pin.at[1] - 2.54, pin.at[0] + 2.54, pin.at[1] + 2.54]);
		ctx.notes.push(`removed sheet pin ${inst.name}/${pin.name}`);
		const dangling = danglingNote(parent.sch, [pin], `on ${parent.name}`);
		if (dangling) ctx.notes.push(dangling);
	},
};

const syncSheetPins: OpDef = {
	op: "sync_sheet_pins",
	group: "hierarchy",
	doc:
		"Add a block pin for every hierarchical label of the sheet that has none (outputs and tri-states on the right, others on the left, " +
		"on free positions 2 grid steps apart); pins whose label is gone are reported, not removed.",
	fields: {
		sheet: { t: "string", req: true, doc: "the sub-sheet whose block is synced" },
	},
	apply(ctx, a) {
		const { inst, parent, block } = blockArg(ctx, a.sheet);
		const labels = inst.sch
			.labels()
			.filter(l => l.kind === "hierarchical_label")
			.sort((l, m) => l.at[1] - m.at[1] || l.at[0] - m.at[0]);
		const names = new Set(labels.map(l => l.name));
		const have = new Set(block.pins.map(p => p.name));
		const step = 2 * ctx.project.settings.grid;
		const taken: Record<Side, number[]> = { left: [], right: [], top: [], bottom: [] };
		for (const p of block.pins) {
			const side = sideOf(p);
			taken[side].push(pinOffset(block.at, p, side));
		}
		const added: string[] = [];
		for (const l of labels) {
			if (have.has(l.name)) continue;
			have.add(l.name);
			const shape = l.shape ?? "passive";
			const side: Side = shape === "output" || shape === "tri_state" ? "right" : "left";
			const len = sideLength(block.size, side);
			let offset = 0;
			for (let o = step; o <= len - step + 1e-6; o += step) {
				if (taken[side].every(t => Math.abs(t - o) >= step - 1e-6)) {
					offset = Math.round(o * 1000) / 1000;
					break;
				}
			}
			if (!offset) {
				throw new Error(
					`no free position left on the ${side} side of ${inst.name}'s block (height ${len} mm) for pin ${l.name}; ` +
						"make the block taller with move_sheet size, then sync again",
				);
			}
			taken[side].push(offset);
			const p = pinPoint(block.at, block.size, side, offset);
			insertPin(block, pinNode(l.name, shape, p, side));
			added.push(`${l.name} (${side}, ${p.join(", ")})`);
		}
		if (added.length) {
			ctx.project.touch(parent.sch);
			ctx.touch(parent, blockBox(block.at, block.size));
			ctx.notes.push(`added sheet pins on ${inst.name}: ${added.join(", ")}`);
		} else {
			ctx.notes.push(`every hierarchical label of ${inst.name} already has a sheet pin`);
		}
		const orphans = block.pins.filter(p => !names.has(p.name));
		if (orphans.length) {
			ctx.notes.push(
				`sheet pins of ${inst.name} without a hierarchical label (left in place): ${orphans.map(p => p.name).join(", ")}; ` +
					"remove them with remove_sheet_pin or add the labels",
			);
		}
	},
};

const moveSheet: OpDef = {
	op: "move_sheet",
	group: "hierarchy",
	doc: "Move and/or resize a sheet block; pins keep their side and offset, wire ends on them are dragged along.",
	fields: {
		sheet: { t: "string", req: true, doc: "the sub-sheet whose block moves" },
		at: { t: "point", req: true, doc: "new top-left corner (grid)" },
		size: { t: "point", doc: "new [width, height] in mm (grid multiples; default: unchanged)" },
	},
	apply(ctx, a) {
		const { inst, parent, block } = blockArg(ctx, a.sheet);
		const origin = gridPt(ctx, a.at, "at");
		const size = a.size === undefined ? block.size : sizeArg(ctx, a.size);
		const moves = new Map<string, Pt>();
		const placed: { pin: SheetPin; to: Pt; side: Side }[] = [];
		const misfit: string[] = [];
		const need: [number, number] = [0, 0];
		for (const pin of block.pins) {
			const side = sideOf(pin);
			const off = pinOffset(block.at, pin, side);
			const vertical = side === "left" || side === "right";
			const n = vertical ? 1 : 0;
			need[n] = Math.max(need[n], off + ctx.project.settings.grid);
			if (off >= sideLength(size, side)) misfit.push(`${pin.name} (${side}, offset ${off})`);
			const to = pinPoint(origin, size, side, off);
			placed.push({ pin, to, side });
			if (!samePt(to, pin.at)) moves.set(key(pin.at), to);
		}
		if (misfit.length) {
			throw new Error(
				`pins ${misfit.join(", ")} no longer fit on a block of size (${size.join(", ")}); ` +
					`give at least (${Math.max(need[0], size[0])}, ${Math.max(need[1], size[1])}) or remove the pins first`,
			);
		}
		const box = blockBox(origin, size);
		checkPlace(parent, box, block.node);

		// things other than wire ends on a pin don't follow it: say so
		const points = parent.sch.connectionPoints();
		const wires = parent.sch.wires();
		const stay: string[] = [];
		for (const { pin } of placed) {
			if (!moves.has(key(pin.at))) continue;
			const here = (points.get(key(pin.at)) ?? []).filter(w => !w.startsWith("sheet pin "));
			if (wires.some(w => onInterior(pin.at, w))) here.push("a wire passing through");
			if (here.length) stay.push(`${pin.name}: ${here.join(", ")}`);
		}
		const dragged = dragWireEnds(parent.sch, moves);

		const [x0, y0] = block.at;
		const bottom = y0 + block.size[1];
		for (const f of children(block.node, "property")) {
			// raw numbers: KiCad keeps 4 decimals for field positions
			const fa = child(f, "at");
			const [fx, fy] = [num(fa, 1), num(fa, 2)];
			// fields below the block keep their place relative to the bottom edge, the others to the top
			const dy = fy >= bottom ? origin[1] + size[1] - bottom : origin[1] - y0;
			setChild(f, L("at", fx + origin[0] - x0, fy + dy, (fa?.length ?? 0) > 3 ? num(fa, 3) : 0));
		}
		for (const { pin, to, side } of placed) setChild(pin.node, L("at", to[0], to[1], SIDE_ROT[side]));
		setChild(block.node, L("at", origin[0], origin[1]));
		setChild(block.node, L("size", size[0], size[1]));

		ctx.project.touch(parent.sch);
		ctx.touch(parent, blockBox(block.at, block.size));
		ctx.touch(parent, box);
		ctx.notes.push(`moved the block of ${inst.name} to (${origin.join(", ")}) size (${size.join(", ")}); ${dragged} wire end(s) dragged`);
		if (stay.length) ctx.notes.push(`left behind at the old pin positions (reconnect them): ${stay.join("; ")}`);
	},
};

const renameSheet: OpDef = {
	op: "rename_sheet",
	group: "hierarchy",
	doc: "Rename a sub-sheet (block name and .kicad_pro sheet list); its hierarchical net names change with it.",
	fields: {
		sheet: { t: "string", req: true, doc: "the sub-sheet to rename" },
		name: { t: "string", req: true, doc: "new unique name, no \"/\"" },
	},
	apply(ctx, a) {
		const { project } = ctx;
		const { inst, parent, block } = blockArg(ctx, a.sheet);
		const name = sheetName(project, a.name, block.uuid);
		const field = prop(block.node, "Sheetname") ?? prop(block.node, "Sheet name");
		if (!field) throw new Error(`the block of ${inst.name} has no Sheetname field`);
		field[2] = str(name);
		const list = proSheets(project);
		const entry = list.find(e => isEntry(e, block.uuid));
		if (isEntry(entry, block.uuid)) entry[1] = name;
		else list.push([block.uuid, name]);
		project.touch(parent.sch);
		project.touchPro();
		ctx.touch(parent, blockBox(block.at, block.size));
		ctx.notes.push(`renamed sheet ${inst.name} to ${name}: its nets are now ${parent.namePath}${name}/…`);
	},
};

const removeSheet: OpDef = {
	op: "remove_sheet",
	group: "hierarchy",
	doc:
		"Remove a sub-sheet's block (and its .kicad_pro entry); with delete_file the file goes too when no other block uses it. " +
		"Refused while the sheet holds sub-sheets, or symbols unless delete_file and force are both true.",
	fields: {
		sheet: { t: "string", req: true, doc: "the sub-sheet to remove" },
		delete_file: { t: "boolean", doc: "also delete the sheet's file when no other block uses it (default false)" },
		force: { t: "boolean", doc: "with delete_file: delete even though the sheet still has symbols" },
	},
	apply(ctx, a) {
		const { project } = ctx;
		const { inst, parent, block } = blockArg(ctx, a.sheet);
		const subs = inst.sch.sheets();
		if (subs.length) throw new Error(`sheet ${inst.name} holds sub-sheets (${subs.map(s => s.name).join(", ")}); remove those first`);
		const others = project.hierarchy().filter(i => i.file === inst.file && i.path !== inst.path);
		const symbols = inst.sch.symbols().length;
		if (symbols && !others.length && !(a.delete_file === true && a.force === true)) {
			throw new Error(
				`sheet ${inst.name} (${project.rel(inst.file)}) still has ${symbols} symbol(s); remove them first, ` +
					"or pass delete_file: true and force: true to drop them with the file",
			);
		}
		parent.sch.remove(block.node);
		const list = proSheets(project);
		const i = list.findIndex(e => isEntry(e, block.uuid));
		if (i >= 0) {
			list.splice(i, 1);
			project.touchPro();
		}
		project.touch(parent.sch);
		ctx.touch(parent, blockBox(block.at, block.size));
		ctx.notes.push(`removed the block of sheet ${inst.name} from ${parent.name}`);
		const dangling = danglingNote(parent.sch, block.pins, `on ${parent.name}`);
		if (dangling) ctx.notes.push(dangling);
		if (a.delete_file !== true) {
			if (!others.length) ctx.notes.push(`${project.rel(inst.file)} is kept but no longer used`);
		} else if (others.length) {
			ctx.notes.push(`${project.rel(inst.file)} is kept: still used by ${others.map(o => o.namePath).join(", ")}`);
		} else {
			project.deleteFile(inst.file);
			ctx.notes.push(`deleted ${project.rel(inst.file)}`);
		}
	},
};

const TITLE_KEYS = ["title", "date", "rev", "company"] as const;

const setPage: OpDef = {
	op: "set_page",
	group: "hierarchy",
	doc: "Set a sheet file's paper size and title block fields; an empty string clears a field, comments replace all comment lines.",
	fields: {
		sheet: { t: "string", req: true, doc: "sheet (\"root\" for the top sheet); a file used by several blocks changes for all" },
		paper: { t: "string", values: Object.keys(PAPER), doc: "paper size (landscape)" },
		title: { t: "string", doc: "title" },
		date: { t: "string", doc: "date, e.g. \"2026-10-29\"" },
		rev: { t: "string", doc: "revision" },
		company: { t: "string", doc: "company" },
		comments: { t: "strings", doc: "comment lines 1..9 (empty strings leave a line blank)" },
	},
	apply(ctx, a) {
		const inst = sheetArg(ctx, a.sheet);
		const tree = inst.sch.tree;
		const given = ["paper", ...TITLE_KEYS, "comments"].filter(k => a[k] !== undefined);
		if (!given.length) throw new Error("give at least one of paper, title, date, rev, company, comments");
		if (typeof a.paper === "string") setChild(tree, L("paper", str(a.paper)));

		const old = child(tree, "title_block");
		const values = new Map<string, string>();
		for (const k of TITLE_KEYS) {
			const v = a[k] ?? text(child(old ?? [], k));
			if (typeof v === "string" && v !== "") values.set(k, v);
		}
		let comments = children(old ?? [], "comment").map(c => [Number(text(c)), text(c, 2) ?? ""] as const);
		if (Array.isArray(a.comments)) {
			if (a.comments.length > 9) throw new Error(`KiCad has 9 comment lines; got ${a.comments.length}`);
			comments = a.comments.map((c, i) => [i + 1, String(c)] as const);
		}
		comments = comments.filter(([, c]) => c !== "");
		const items: List[] = [...values].map(([k, v]) => L(k, str(v)));
		for (const [n, c] of comments) items.push(L("comment", n, str(c)));
		if (old) tree.splice(tree.indexOf(old), 1);
		if (items.length) {
			const i = tree.findIndex(x => isList(x) && head(x) === "paper");
			tree.splice(i + 1, 0, L("title_block", ...items));
		}
		ctx.project.touch(inst.sch);
		ctx.touch(inst);
		const shared = ctx.project.hierarchy().filter(i => i.file === inst.file).length;
		ctx.notes.push(
			`page settings of ${path.basename(inst.file)}: ${given.join(", ")} set${shared > 1 ? ` (shared by ${shared} sheet instances)` : ""}`,
		);
	},
};

export const ops: OpDef[] = [createSheet, addSheetPin, removeSheetPin, syncSheetPins, moveSheet, renameSheet, removeSheet, setPage];
