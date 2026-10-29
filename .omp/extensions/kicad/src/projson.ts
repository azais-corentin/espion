/**
 * .kicad_pro JSON that round-trips byte for byte: KiCad writes double-typed settings with a
 * decimal point ("1.0"), JSON.stringify would drop it. Keys whose numbers carry a decimal point
 * or exponent anywhere in the original file are written as doubles again.
 */

export interface ProJson {
	data: Record<string, unknown>;
	floatKeys: Set<string>;
}

export function parsePro(source: string): ProJson {
	const floatKeys = new Set<string>();
	const floatArrays = new WeakSet<unknown[]>();
	// The reviver runs bottom-up: an array element is seen before the array's own key.
	const data = JSON.parse(source, function (this: unknown, k: string, v: unknown, ctx?: { source?: string }) {
		if (typeof v === "number" && ctx?.source && /[.eE]/.test(ctx.source)) {
			if (Array.isArray(this)) floatArrays.add(this);
			else floatKeys.add(k);
		} else if (Array.isArray(v) && floatArrays.has(v)) {
			floatKeys.add(k);
		}
		return v;
	}) as Record<string, unknown>;
	return { data, floatKeys };
}

export function stringifyPro(pro: ProJson): string {
	const out = (v: unknown, key: string, ind: string): string => {
		if (typeof v === "number") {
			return pro.floatKeys.has(key) && Number.isInteger(v) ? `${v}.0` : JSON.stringify(v);
		}
		if (Array.isArray(v)) {
			if (v.length === 0) return "[]";
			const i2 = `${ind}  `;
			return `[\n${v.map(x => i2 + out(x, key, i2)).join(",\n")}\n${ind}]`;
		}
		if (v && typeof v === "object") {
			const entries = Object.entries(v).filter(([, x]) => x !== undefined);
			if (entries.length === 0) return "{}";
			const i2 = `${ind}  `;
			return `{\n${entries.map(([k, x]) => `${i2}${JSON.stringify(k)}: ${out(x, k, i2)}`).join(",\n")}\n${ind}}`;
		}
		return JSON.stringify(v);
	};
	return `${out(pro.data, "", "")}\n`;
}
