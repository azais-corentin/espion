/** Every edit op, query and check, keyed by name, plus the generated docs for the tool descriptions. */
import { checks as boardChecks } from "./checks/board";
import { checks as circuitChecks } from "./checks/circuit";
import { checks as connectivityChecks } from "./checks/connectivity";
import { checks as copperChecks } from "./checks/copper";
import { checks as klcChecks } from "./checks/klc";
import { checks as layoutChecks } from "./checks/layout";
import { checks as partChecks } from "./checks/parts";
import { checks as rulesChecks } from "./checks/rules";
import { checks as styleChecks } from "./checks/style";
import { ops as boardOps } from "./ops/board";
import { ops as boardItemOps } from "./ops/board-items";
import { ops as copperOps } from "./ops/copper";
import { ops as hierarchyOps } from "./ops/hierarchy";
import { ops as libraryOps } from "./ops/library";
import { ops as projectOps } from "./ops/project";
import { ops as schematicOps } from "./ops/schematic";
import { ops as zoneOps } from "./ops/zones";
import { queries as boardQueries } from "./queries/board";
import { queries as copperQueries } from "./queries/copper";
import { queries as diffQueries } from "./queries/diff";
import { queries as lcscQueries } from "./queries/lcsc";
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

export const OPS = byName<OpDef>(
	[...schematicOps, ...hierarchyOps, ...projectOps, ...libraryOps, ...boardOps, ...copperOps, ...zoneOps, ...boardItemOps],
	o => o.op,
	"op",
);
export const QUERIES = byName<QueryDef>(
	[...projectQueries, ...libraryQueries, ...diffQueries, ...lcscQueries, ...boardQueries, ...copperQueries],
	q => q.what,
	"query",
);
export const CHECKS = byName<CheckDef>(
	[...connectivityChecks, ...styleChecks, ...partChecks, ...circuitChecks, ...rulesChecks, ...klcChecks, ...boardChecks, ...copperChecks, ...layoutChecks],
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
