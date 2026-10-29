/**
 * lcsc: LCSC's live product data (part, manufacturer, package, stock, price breaks, datasheet) for
 * LCSC codes, from the JSON endpoint behind lcsc.com's product pages. One code's failure (unknown
 * code, changed answer format, network) is reported on that code; the others still answer.
 */
import type { QueryDef } from "../types";

const ENDPOINT = "https://wmsc.lcsc.com/ftps/wm/product/detail?productCode=";
/** The endpoint answers browser-like clients. */
const USER_AGENT = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
const CONCURRENCY = 4;
const TIMEOUT_MS = 15_000;

export interface LcscPart {
	code: string;
	mpn: string;
	manufacturer: string;
	package: string;
	description: string;
	stock: number;
	/** [quantity, USD unit price], ascending quantity. */
	prices: [number, number][];
	datasheet?: string;
	url: string;
	/** Date of the fetch, YYYY-MM-DD: stock and prices change daily. */
	fetched: string;
}

export interface LcscError {
	code: string;
	error: string;
}

export interface LcscOptions {
	fetch?: typeof fetch;
	signal?: AbortSignal;
	now?: Date;
	timeoutMs?: number;
}

/** The part from the endpoint's JSON, or why it isn't one. */
function parseAnswer(code: string, body: unknown, fetched: string): LcscPart | LcscError {
	const b = body as { ok?: unknown; result?: unknown } | null;
	if (!b || typeof b !== "object" || !("result" in b)) return { code, error: "unexpected answer from LCSC (no `result`; the API may have changed)" };
	if (b.result === null) return { code, error: `LCSC has no product ${code}` };
	const r = b.result as Record<string, unknown>;
	const str = (k: string) => (typeof r[k] === "string" ? (r[k] as string) : undefined);
	const priceList = Array.isArray(r.productPriceList) ? (r.productPriceList as Record<string, unknown>[]) : undefined;
	const prices = priceList?.map(p => [p.ladder, p.usdPrice]);
	const missing = [
		...(str("productModel") === undefined ? ["productModel"] : []),
		...(typeof r.stockNumber !== "number" ? ["stockNumber"] : []),
		...(!prices?.every(p => p.every(v => typeof v === "number")) ? ["productPriceList[].ladder/usdPrice"] : []),
	];
	if (missing.length) return { code, error: `unexpected answer from LCSC (missing ${missing.join(", ")}; the API may have changed)` };
	if (str("productCode") && str("productCode") !== code) return { code, error: `LCSC answered with ${str("productCode")} for ${code}` };
	const pdf = str("pdfUrl");
	return {
		code,
		mpn: str("productModel")!,
		manufacturer: str("brandNameEn") ?? "",
		package: str("encapStandard") ?? "",
		description: str("productIntroEn") ?? "",
		stock: r.stockNumber as number,
		prices: (prices as [number, number][]).sort((x, y) => x[0] - y[0]),
		...(pdf ? { datasheet: pdf.replace(/\?.*$/, "") } : {}),
		url: `https://www.lcsc.com/product-detail/${code}.html`,
		fetched,
	};
}

async function fetchOne(code: string, opts: LcscOptions, fetched: string): Promise<LcscPart | LcscError> {
	const timeoutMs = opts.timeoutMs ?? TIMEOUT_MS;
	const timeout = AbortSignal.timeout(timeoutMs);
	const signal = opts.signal ? AbortSignal.any([opts.signal, timeout]) : timeout;
	try {
		const res = await (opts.fetch ?? fetch)(`${ENDPOINT}${encodeURIComponent(code)}`, {
			signal,
			headers: { "User-Agent": USER_AGENT, Accept: "application/json" },
		});
		if (!res.ok) return { code, error: `LCSC answered HTTP ${res.status} ${res.statusText}`.trim() };
		const text = await res.text();
		let body: unknown;
		try {
			body = JSON.parse(text);
		} catch {
			return { code, error: `unexpected answer from LCSC (not JSON: ${JSON.stringify(text.slice(0, 60))}; the API may have changed)` };
		}
		return parseAnswer(code, body, fetched);
	} catch (e) {
		if (opts.signal?.aborted) throw e;
		if (timeout.aborted) return { code, error: `LCSC did not answer within ${timeoutMs / 1000} s` };
		return { code, error: `network error: ${(e as Error).message}` };
	}
}

/** LCSC data for each code, in the given order; at most CONCURRENCY requests at a time. */
export async function lcscParts(codes: string[], opts: LcscOptions = {}): Promise<(LcscPart | LcscError)[]> {
	const fetched = (opts.now ?? new Date()).toISOString().slice(0, 10);
	const out: (LcscPart | LcscError)[] = new Array(codes.length);
	let next = 0;
	const worker = async () => {
		while (next < codes.length) {
			const i = next++;
			const code = codes[i]!.trim().toUpperCase();
			out[i] = /^C\d+$/.test(code) ? await fetchOne(code, opts, fetched) : { code: codes[i]!, error: 'not an LCSC code (C followed by digits, e.g. "C25911")' };
		}
	};
	await Promise.all(Array.from({ length: Math.min(CONCURRENCY, codes.length) }, worker));
	return out;
}

const lcsc: QueryDef = {
	what: "lcsc",
	doc: "LCSC's live data for LCSC part codes (network): code, mpn, manufacturer, package, description, stock, prices [[quantity, USD each]], datasheet (PDF URL), url (product page) and fetched (date: stock and prices change). A code LCSC doesn't know, a changed answer format or a network failure is an {code, error} entry; the other codes still answer.",
	fields: {
		codes: { t: "strings", req: true, doc: 'LCSC codes, e.g. ["C25911", "C485916"]' },
	},
	async run(ctx, args) {
		const codes = args.codes as string[];
		if (!codes.length) throw new Error("codes: give at least one LCSC code");
		return { parts: await lcscParts(codes, { signal: ctx.analysis.signal }) };
	},
};

export const queries: QueryDef[] = [lcsc];
