/**
 * Compact JSON for tool output: an object or array that fits in `width` columns stays on one
 * line, anything larger is expanded one level with 1-space indentation. Coordinates and short
 * records stay readable without a line per number.
 */
export function compactJson(v: unknown, width = 140): string {
	const fmt = (x: unknown, ind: string): string => {
		const flat = JSON.stringify(x);
		if (flat === undefined) return "null";
		if (x === null || typeof x !== "object" || ind.length + flat.length <= width) return flat;
		const inner = `${ind} `;
		if (Array.isArray(x)) return `[\n${x.map(e => inner + fmt(e, inner)).join(",\n")}\n${ind}]`;
		const entries = Object.entries(x).filter(([, e]) => e !== undefined);
		return `{\n${entries.map(([k, e]) => `${inner}${JSON.stringify(k)}: ${fmt(e, inner)}`).join(",\n")}\n${ind}}`;
	};
	return fmt(v, "");
}
