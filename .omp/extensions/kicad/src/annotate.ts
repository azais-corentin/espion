/** Reference numbering shared by the edit ops. */
import type { Project, SheetInst } from "./project";

/** "R12" → "R", "#PWR301" → "#PWR", "U?" → "U". */
export function refPrefix(ref: string): string {
	return ref.replace(/[0-9?]+$/, "");
}

/**
 * Numbering block of `sheetName` for `prefix`, or undefined when the prefix has no blocks. A
 * sheet without a configured block (e.g. one created in this edit) gets the next free one past
 * the highest configured block, spaced like the configured ones (100 when only one is set);
 * several such sheets take consecutive blocks in hierarchy order.
 */
function blockBase(project: Project, prefix: string, sheetName: string): number | undefined {
	const blocks = project.settings.refBase[prefix];
	if (!blocks) return undefined;
	const configured = blocks[sheetName];
	if (configured !== undefined) return configured;
	const bases = Object.values(blocks).sort((a, b) => a - b);
	if (!bases.length) return undefined;
	let step = Infinity;
	for (let i = 1; i < bases.length; i++) if (bases[i]! > bases[i - 1]!) step = Math.min(step, bases[i]! - bases[i - 1]!);
	if (step === Infinity) step = 100;
	const unconfigured = [...new Set(project.hierarchy().map(s => s.name))].filter(n => blocks[n] === undefined);
	return bases.at(-1)! + step * (unconfigured.indexOf(sheetName) + 1);
}

/**
 * Next free reference for `prefix` on a sheet instance: the sheet's numbering block (block 300 →
 * 301, 302, …) when the project configures blocks for this prefix (see blockBase), else one past
 * the highest number of that prefix in the whole project. `taken` holds references already
 * handed out but not yet in the project.
 */
export function nextRef(project: Project, prefix: string, sheetInst: SheetInst, taken: Set<string> = new Set()): string {
	const used = project.allRefs();
	for (const t of taken) used.add(t);
	let n: number;
	const base = blockBase(project, prefix, sheetInst.name);
	if (base !== undefined) n = base + 1;
	else {
		let max = 0;
		for (const r of used) {
			if (!r.startsWith(prefix)) continue;
			const tail = r.slice(prefix.length);
			if (/^\d+$/.test(tail)) max = Math.max(max, Number(tail));
		}
		n = max + 1;
	}
	while (used.has(`${prefix}${n}`)) n++;
	return `${prefix}${n}`;
}
