/**
 * Small argument specs for edit ops, queries and checks: one definition validates the arguments
 * and documents them in the tool description, so the two can't drift apart.
 */
import type { Pt } from "./geom";

export type FieldType =
	| "string"
	| "number"
	| "integer"
	| "boolean"
	| "point"
	| "points"
	| "strings"
	| "object"
	| "objects"
	| "any";

export interface Field {
	t: FieldType;
	doc: string;
	req?: boolean;
	/** Allowed values for strings (or for each string of "strings"). */
	values?: readonly string[];
}

export type Fields = Record<string, Field>;

export class SpecError extends Error {}

function typeName(f: Field): string {
	const base = {
		string: "string",
		number: "number",
		integer: "integer",
		boolean: "boolean",
		point: "[x,y] mm",
		points: "[[x,y],…] mm",
		strings: "string[]",
		object: "object",
		objects: "object[]",
		any: "any",
	}[f.t];
	return f.values ? `${f.values.map(v => JSON.stringify(v)).join("|")}${f.t === "strings" ? "[]" : ""}` : base;
}

/** One-line doc: `name {a: type, b?: type} — doc`, with field docs indented below. */
export function describe(name: string, doc: string, fields: Fields): string {
	const sig = Object.entries(fields)
		.map(([k, f]) => `${k}${f.req ? "" : "?"}: ${typeName(f)}`)
		.join(", ");
	const lines = [`- ${name} {${sig}}: ${doc}`];
	for (const [k, f] of Object.entries(fields)) lines.push(`    ${k}: ${f.doc}`);
	return lines.join("\n");
}

function isPt(v: unknown): v is Pt {
	return Array.isArray(v) && v.length === 2 && v.every(x => typeof x === "number" && Number.isFinite(x));
}

/** Validate `args` against `fields`; unknown keys are rejected so typos don't pass silently. */
export function validate(where: string, args: Record<string, unknown>, fields: Fields, ignore: readonly string[] = []): Record<string, unknown> {
	for (const k of Object.keys(args)) {
		if (!(k in fields) && !ignore.includes(k)) {
			throw new SpecError(`${where}: unknown argument "${k}" (allowed: ${Object.keys(fields).join(", ") || "none"})`);
		}
	}
	for (const [k, f] of Object.entries(fields)) {
		const v = args[k];
		if (v === undefined || v === null) {
			if (f.req) throw new SpecError(`${where}: missing required argument "${k}" (${f.doc})`);
			continue;
		}
		const bad = (want: string) => new SpecError(`${where}: "${k}" must be ${want}, got ${JSON.stringify(v)}`);
		switch (f.t) {
			case "string":
				if (typeof v !== "string") throw bad("a string");
				if (f.values && !f.values.includes(v)) throw bad(`one of ${f.values.join(", ")}`);
				break;
			case "number":
				if (typeof v !== "number" || !Number.isFinite(v)) throw bad("a number");
				break;
			case "integer":
				if (typeof v !== "number" || !Number.isInteger(v)) throw bad("an integer");
				break;
			case "boolean":
				if (typeof v !== "boolean") throw bad("true or false");
				break;
			case "point":
				if (!isPt(v)) throw bad("[x, y] in mm");
				break;
			case "points":
				if (!Array.isArray(v) || v.length < 2 || !v.every(isPt)) throw bad("a list of at least two [x, y] points");
				break;
			case "strings":
				if (!Array.isArray(v) || !v.every(x => typeof x === "string")) throw bad("a list of strings");
				if (f.values) for (const x of v) if (!f.values.includes(x as string)) throw bad(`values from ${f.values.join(", ")}`);
				break;
			case "object":
				if (typeof v !== "object" || Array.isArray(v)) throw bad("an object");
				break;
			case "objects":
				if (!Array.isArray(v) || !v.every(x => x && typeof x === "object" && !Array.isArray(x))) throw bad("a list of objects");
				break;
			case "any":
				break;
		}
	}
	return args;
}
