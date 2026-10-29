/**
 * Circuit checks on KiCad's netlist (`decoupling`, `pulls`): every powered part has a decoupling
 * capacitor next to it, and open-collector / open-emitter outputs and bus nets (I²C, MDIO) have
 * their pull resistors. Ground nets and pull-up nets come from the project settings; exemptions
 * there carry a reason and are reported as info.
 */
import type { Component, Netlist, NetNode } from "../cli";
import type { CheckContext, CheckDef, Finding } from "../types";
import { sheetOfPath } from "./parts";

/** Case-insensitive shell patterns (`*`, `?`) on the whole string: true when any matches. */
export function wildcards(patterns: string[]): (s: string) => boolean {
	const res = patterns.map(p => new RegExp(`^${p.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".")}$`, "i"));
	return s => res.some(r => r.test(s));
}

/** Leaf name of a netlist net: "/IMU/SDA" → "SDA", "+3V3" → "+3V3". */
export const netLeaf = (name: string): string => name.slice(name.lastIndexOf("/") + 1);

/** Reference prefix: "C12" → "C", "#PWR01" → "#PWR". */
export const refPrefix = (ref: string): string => /^\D*/.exec(ref)![0];

/** Electrical type of a netlist pin without KiCad's "+no_connect" suffix. */
const pinType = (n: NetNode): string => (n.pintype ?? "").split("+")[0]!;

/** On the board and fitted. */
const fitted = (c: Component | undefined): c is Component => !!c && !c.dnp && !c.excludeFromBoard;

/** "U3.7 (VDD)" */
const pinLabel = (n: NetNode): string => `${n.ref}.${n.pin}${n.pinfunction ? ` (${n.pinfunction})` : ""}`;

/** Whether a component is on the checked sheets. */
function sheetScope(ctx: CheckContext): (c: Component) => boolean {
	if (!ctx.sheets) return () => true;
	const paths = new Set(ctx.sheets.map(s => s.namePath));
	return c => paths.has(c.sheetName);
}

/** Distinct nets of every fitted part with reference prefix `prefix`. */
function partNets(nl: Netlist, prefix: string): Map<string, string[]> {
	const out = new Map<string, string[]>();
	for (const net of nl.nets) {
		for (const node of net.nodes) {
			if (refPrefix(node.ref) !== prefix || !fitted(nl.components.get(node.ref))) continue;
			const nets = out.get(node.ref) ?? [];
			if (!nets.includes(net.name)) nets.push(net.name);
			out.set(node.ref, nets);
		}
	}
	return out;
}

const decoupling: CheckDef = {
	name: "decoupling",
	doc: 'Every fitted board part (not DNP, not "#" power symbols) with a power_in pin on a non-ground net needs a fitted capacitor (ref prefix C) from that net to a ground net on the same sheet; otherwise a warning per part and net naming its power pins. Ground nets: leaf name matching settings.groundNets (default GND*, *GND, VSS*). settings.decouplingExempt {"REF" or "REF.pin": reason} turns the warning into info with the reason.',
	byDefault: true,
	async run(ctx) {
		const nl = await ctx.analysis.netlist();
		const { groundNets, decouplingExempt: exempt } = ctx.project.settings;
		const isGround = wildcards(groundNets);
		const ground = (net: string) => isGround(netLeaf(net));
		const scope = sheetScope(ctx);
		// "net\0sheet path" for every sheet with a capacitor from that net to ground
		const decoupled = new Set<string>();
		for (const [ref, nets] of partNets(nl, "C")) {
			if (!nets.some(ground)) continue;
			const sheet = nl.components.get(ref)!.sheetName;
			for (const n of nets) if (!ground(n)) decoupled.add(`${n}\0${sheet}`);
		}
		const out: Finding[] = [];
		for (const net of nl.nets) {
			if (ground(net.name)) continue;
			const byRef = new Map<string, NetNode[]>();
			for (const node of net.nodes) {
				if (pinType(node) !== "power_in" || node.ref.startsWith("#")) continue;
				const c = nl.components.get(node.ref);
				if (!fitted(c) || !scope(c) || decoupled.has(`${net.name}\0${c.sheetName}`)) continue;
				byRef.set(node.ref, [...(byRef.get(node.ref) ?? []), node]);
			}
			for (const [ref, nodes] of byRef) {
				const c = nl.components.get(ref)!;
				const sheet = sheetOfPath(c.sheetName);
				const base = { check: "decoupling", refs: [ref], net: net.name, sheet };
				const what = (pins: NetNode[]) =>
					`${ref} (${c.value}) power pin${pins.length > 1 ? "s" : ""} ${pins.map(pinLabel).join(", ")} on ${net.name}: no capacitor from ${net.name} to ground on sheet ${sheet}`;
				const plain: NetNode[] = [];
				const waived = new Map<string, NetNode[]>();
				for (const n of nodes) {
					const reason = exempt[`${ref}.${n.pin}`] ?? exempt[ref];
					if (reason === undefined) plain.push(n);
					else waived.set(reason, [...(waived.get(reason) ?? []), n]);
				}
				if (plain.length) out.push({ ...base, severity: "warning", message: what(plain), key: `decoupling|${ref}|${net.name}` });
				for (const [reason, pins] of waived) {
					out.push({
						...base,
						severity: "info",
						message: `${what(pins)} [exempt: ${reason}]`,
						key: `decoupling|${ref}|${net.name}|${pins.map(p => p.pin).join(",")}`,
					});
				}
			}
		}
		return out;
	},
};

const pulls: CheckDef = {
	name: "pulls",
	doc: 'Pull resistors (fitted, ref prefix R) on signal nets: a net with an open_collector pin needs one to a rail, a net with an open_emitter pin one to ground, a net whose leaf name matches settings.pullUpNets (default *SDA*, *SCL*, *MDIO*) one to a rail (warning when missing). An I²C pair (leaf names differing only by SDA/SCL, both pulled up) pulled up to different rails is a warning. A rail is a non-ground net (settings.groundNets) with a power_in or power_out pin. settings.pullsExempt {"net name or leaf": reason} turns a net\'s warnings into info with the reason.',
	byDefault: true,
	async run(ctx) {
		const nl = await ctx.analysis.netlist();
		const { groundNets, pullUpNets, pullsExempt } = ctx.project.settings;
		const isGround = wildcards(groundNets);
		const ground = (net: string) => isGround(netLeaf(net));
		const wantsPullUp = wildcards(pullUpNets);
		const scope = sheetScope(ctx);
		const rails = new Set(
			nl.nets.filter(n => !ground(n.name) && n.nodes.some(x => pinType(x) === "power_in" || pinType(x) === "power_out")).map(n => n.name),
		);
		// net → rail → resistors pulling it up; net → resistors pulling it down
		const up = new Map<string, Map<string, string[]>>();
		const down = new Map<string, string[]>();
		for (const [ref, nets] of partNets(nl, "R")) {
			for (const a of nets) {
				for (const b of nets) {
					if (a === b) continue;
					if (ground(b)) down.set(a, [...(down.get(a) ?? []), ref]);
					else if (rails.has(b)) {
						const m = up.get(a) ?? new Map<string, string[]>();
						m.set(b, [...(m.get(b) ?? []), ref]);
						up.set(a, m);
					}
				}
			}
		}
		const out: Finding[] = [];
		const report = (nets: string[], kind: string, message: string) => {
			const reason = nets.map(n => pullsExempt[n] ?? pullsExempt[netLeaf(n)]).find(r => r !== undefined);
			const sheet = nets[0]!.startsWith("/") ? sheetOfPath(nets[0]!.slice(0, nets[0]!.lastIndexOf("/") + 1)) : undefined;
			out.push({
				check: "pulls",
				severity: reason === undefined ? "warning" : "info",
				message: reason === undefined ? message : `${message} [exempt: ${reason}]`,
				key: `pulls|${kind}|${nets.join("|")}`,
				net: nets[0],
				sheet,
			});
		};
		const checked = new Set<string>();
		for (const net of nl.nets) {
			if (net.name.startsWith("unconnected-(") || rails.has(net.name) || ground(net.name)) continue;
			const nodes = net.nodes.filter(x => !x.ref.startsWith("#") && fitted(nl.components.get(x.ref)));
			if (!nodes.some(x => scope(nl.components.get(x.ref)!))) continue;
			checked.add(net.name);
			const oc = nodes.filter(x => pinType(x) === "open_collector");
			const oe = nodes.filter(x => pinType(x) === "open_emitter");
			const named = wantsPullUp(netLeaf(net.name));
			if ((oc.length || named) && !up.has(net.name)) {
				const why = [oc.length ? `open-collector ${oc.map(pinLabel).join(", ")}` : "", named ? "name matches pullUpNets" : ""].filter(Boolean).join("; ");
				report([net.name], "pullup", `${net.name} (${why}) has no pull-up resistor to a rail`);
			}
			if (oe.length && !down.has(net.name)) {
				report([net.name], "pulldown", `${net.name} (open-emitter ${oe.map(pinLabel).join(", ")}) has no pull-down resistor to ground`);
			}
		}
		// I²C pairs: pulled-up nets whose names differ only by SDA <-> SCL
		const pairs = new Map<string, { sda?: string; scl?: string }>();
		for (const name of up.keys()) {
			const leaf = netLeaf(name);
			const hits = leaf.match(/sda|scl/gi);
			if (hits?.length !== 1) continue;
			const k = `${name.slice(0, name.length - leaf.length)}${leaf.replace(/sda|scl/i, "\0")}`;
			const p = pairs.get(k) ?? {};
			p[hits[0]!.toLowerCase() as "sda" | "scl"] = name;
			pairs.set(k, p);
		}
		const railsOf = (net: string) => [...up.get(net)!.keys()].sort();
		const describe = (net: string) => `${net} → ${[...up.get(net)!].map(([rail, refs]) => `${rail} (${refs.join(", ")})`).join(", ")}`;
		for (const { sda, scl } of pairs.values()) {
			if (!sda || !scl || !(checked.has(sda) || checked.has(scl))) continue;
			if (railsOf(sda).join("\0") === railsOf(scl).join("\0")) continue;
			report([sda, scl], "i2c_rails", `I²C pair pulled up to different rails: ${describe(sda)}; ${describe(scl)}`);
		}
		return out;
	},
};

export const checks: CheckDef[] = [decoupling, pulls];
