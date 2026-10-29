/**
 * Contracts between the tools and the op / query / check modules.
 *
 * Each module exports an array of definitions; `registry.ts` collects them. Definitions validate
 * their own arguments through `fields` (see spec.ts) before `apply` / `run` sees them.
 */
import type { Analysis } from "./analysis";
import type { Box, Pt } from "./geom";
import type { Libraries } from "./libs";
import type { Project, SheetInst } from "./project";
import type { Fields } from "./spec";

// ---- kicad_edit ops ----------------------------------------------------------------------------

export interface OpContext {
	project: Project;
	libs: Libraries;
	/** Lines reported back to the agent after the transaction (what was done, chosen references). */
	notes: string[];
	/** Record that a sheet changed, optionally the region (for renders and style checks). */
	touch(sheet: SheetInst, box?: Box): void;
	/** Record that a project library part was created or changed ("Lib:Name"), for renders. */
	touchPart(kind: PartKind, id: string): void;
}

export type PartKind = "symbol" | "footprint";

export interface OpDef {
	op: string;
	/** Group in the tool description: "schematic", "hierarchy", "project", "rules", "library". */
	group: string;
	doc: string;
	fields: Fields;
	apply(ctx: OpContext, args: Record<string, unknown>): void | Promise<void>;
}

// ---- kicad_query -------------------------------------------------------------------------------

export interface QueryContext {
	project: Project;
	analysis: Analysis;
	libs(): Promise<Libraries>;
}

export interface QueryDef {
	what: string;
	doc: string;
	fields: Fields;
	run(ctx: QueryContext, args: Record<string, unknown>): Promise<unknown>;
}

// ---- kicad_check -------------------------------------------------------------------------------

export type Severity = "error" | "warning" | "info";

export interface Finding {
	check: string;
	severity: Severity;
	message: string;
	/**
	 * Stable identity without coordinates (rule + subject), used to tell new findings from old ones
	 * when an edit is gated or a baseline is compared.
	 */
	key: string;
	sheet?: string;
	at?: Pt;
	refs?: string[];
	net?: string;
}

export interface CheckContext {
	project: Project;
	analysis: Analysis;
	/** Sheets to look at (undefined = all). */
	sheets?: SheetInst[];
	/** Git ref to compare against, for checks that diff (connectivity, klc). */
	baseline: string;
	args: Record<string, unknown>;
	signal?: AbortSignal;
}

export interface CheckDef {
	name: string;
	doc: string;
	/** Run when kicad_check gets no explicit list. */
	byDefault: boolean;
	run(ctx: CheckContext): Promise<Finding[]>;
}
