/** Every edit op, query and check, keyed by name, plus the generated docs for the tool descriptions. */
import { checks as connectivityChecks } from "./checks/connectivity";
import { checks as klcChecks } from "./checks/klc";
import { checks as partChecks } from "./checks/parts";
import { checks as rulesChecks } from "./checks/rules";
import { checks as styleChecks } from "./checks/style";
import { ops as hierarchyOps } from "./ops/hierarchy";
import { ops as libraryOps } from "./ops/library";
import { ops as projectOps } from "./ops/project";
import { ops as schematicOps } from "./ops/schematic";
import { queries as diffQueries } from "./queries/diff";
import { queries as libraryQueries } from "./queries/library";
import { queries as projectQueries } from "./queries/project";
import { describe } from "./spec";
import type { CheckDef, OpDef, QueryDef } from "./types";

function byName<T>(items: T[], name: (t: T) => string, what: string): Map<string, T> {
	const m = new Map<string, T>();
	for (const it of items) {
		const n = name(it);
		if (m.has(n)) throw new Error(`duplicate ${what} "${n}"`);
		m.set(n, it);
	}
	return m;
}

export const OPS = byName<OpDef>([...schematicOps, ...hierarchyOps, ...projectOps, ...libraryOps], o => o.op, "op");
export const QUERIES = byName<QueryDef>([...projectQueries, ...libraryQueries, ...diffQueries], q => q.what, "query");
export const CHECKS = byName<CheckDef>(
	[...connectivityChecks, ...styleChecks, ...partChecks, ...rulesChecks, ...klcChecks],
	c => c.name,
	"check",
);

export function opsDoc(): string {
	const groups = new Map<string, string[]>();
	for (const o of OPS.values()) {
		const g = groups.get(o.group) ?? [];
		g.push(describe(o.op, o.doc, o.fields));
		groups.set(o.group, g);
	}
	return [...groups].map(([g, lines]) => `### ${g}\n${lines.join("\n")}`).join("\n\n");
}

export function queriesDoc(): string {
	return [...QUERIES.values()].map(q => describe(q.what, q.doc, q.fields)).join("\n");
}

export function checksDoc(): string {
	return [...CHECKS.values()].map(c => `- ${c.name}${c.byDefault ? "" : " (only when named)"}: ${c.doc}`).join("\n");
}
