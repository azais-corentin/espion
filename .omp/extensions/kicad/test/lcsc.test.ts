import { describe, expect, test } from "bun:test";
import { lcscParts } from "../src/queries/lcsc";

/** The endpoint's answer for C25911 (trimmed to the fields the query reads, plus a few it ignores). */
const C25911 = {
	code: 200,
	msg: null,
	ok: true,
	result: {
		productCode: "C25911",
		productModel: "0402WGF6042TCE",
		brandNameEn: "UNI-ROYAL",
		stockNumber: 375900,
		encapStandard: "0402",
		productIntroEn: "60.4kΩ ±1% 62.5mW 0402 Thick Film Resistor",
		pdfUrl: "https://datasheet.lcsc.com/datasheet/pdf/0a975aaa49b7c97f38a963127be4a823.pdf?productCode=C25911",
		productPriceList: [
			{ ladder: 1000, productPrice: "0.0009", usdPrice: 0.0009, currencySymbol: "$" },
			{ ladder: 100, productPrice: "0.0011", usdPrice: 0.0011, currencySymbol: "$" },
		],
		catalogName: "Chip Resistor - Surface Mount",
	},
};

/** A canned answer, or a function of the request's abort signal. */
type Answer = Response | ((signal: AbortSignal) => Promise<Response>);

/** A fetch that answers by product code and records the requests and how many overlap. */
function mockFetch(answers: Record<string, Answer>) {
	const calls: { url: string; ua: string | null }[] = [];
	let inFlight = 0;
	let maxInFlight = 0;
	const fn = (async (input: string | URL | Request, init?: RequestInit) => {
		const url = String(input);
		calls.push({ url, ua: new Headers(init?.headers).get("User-Agent") });
		inFlight++;
		maxInFlight = Math.max(maxInFlight, inFlight);
		try {
			// yield once so every worker has a request open before any answers
			await Promise.resolve();
			const a = answers[new URL(url).searchParams.get("productCode") ?? ""];
			if (!a) throw new TypeError("fetch failed");
			return typeof a === "function" ? await a(init!.signal!) : a.clone();
		} finally {
			inFlight--;
		}
	}) as typeof fetch;
	return { fn, calls, maxInFlight: () => maxInFlight };
}

/** An answer that never comes: rejects only when the request is aborted, as fetch does. */
function never(signal: AbortSignal): Promise<Response> {
	const { promise, reject } = Promise.withResolvers<Response>();
	if (signal.aborted) reject(signal.reason);
	else signal.addEventListener("abort", () => reject(signal.reason));
	return promise;
}

const json = (v: unknown) => new Response(JSON.stringify(v), { headers: { "content-type": "application/json" } });
const now = new Date("2026-10-29T12:00:00Z");

describe("lcsc", () => {
	test("a known code: part, manufacturer, package, stock, sorted USD price breaks, datasheet without query, page URL, date", async () => {
		const m = mockFetch({ C25911: json(C25911) });
		const [p] = await lcscParts(["c25911"], { fetch: m.fn, now });
		expect(p).toEqual({
			code: "C25911",
			mpn: "0402WGF6042TCE",
			manufacturer: "UNI-ROYAL",
			package: "0402",
			description: "60.4kΩ ±1% 62.5mW 0402 Thick Film Resistor",
			stock: 375900,
			prices: [
				[100, 0.0011],
				[1000, 0.0009],
			],
			datasheet: "https://datasheet.lcsc.com/datasheet/pdf/0a975aaa49b7c97f38a963127be4a823.pdf",
			url: "https://www.lcsc.com/product-detail/C25911.html",
			fetched: "2026-10-29",
		});
		expect(m.calls[0]!.url).toBe("https://wmsc.lcsc.com/ftps/wm/product/detail?productCode=C25911");
		expect(m.calls[0]!.ua).toContain("Mozilla/5.0");
	});

	test("each failure is reported on its code; the other codes still answer", async () => {
		const noUsd = structuredClone(C25911);
		noUsd.result.productCode = "C3";
		for (const p of noUsd.result.productPriceList as { usdPrice?: number }[]) delete p.usdPrice;
		const m = mockFetch({
			C25911: json(C25911),
			C1: json({ code: 200, msg: null, result: null, ok: true }),
			C2: new Response("<html>blocked</html>", { status: 200 }),
			C3: json(noUsd),
			C4: new Response("busy", { status: 503, statusText: "Service Unavailable" }),
			C6: json({ data: {} }),
		});
		const out = await lcscParts(["C1", "C2", "C3", "C4", "C5", "C6", "C25911", "R1"], { fetch: m.fn, now });
		const errors = Object.fromEntries(out.map(p => [p.code, "error" in p ? p.error : "ok"]));
		expect(errors).toEqual({
			C1: "LCSC has no product C1",
			C2: expect.stringContaining("not JSON"),
			C3: expect.stringContaining("missing productPriceList[].ladder/usdPrice; the API may have changed"),
			C4: "LCSC answered HTTP 503 Service Unavailable",
			C5: "network error: fetch failed",
			C6: expect.stringContaining("no `result`"),
			C25911: "ok",
			R1: expect.stringContaining("not an LCSC code"),
		});
		expect(m.calls.some(c => c.url.endsWith("R1"))).toBe(false);
	});

	test("requests run in parallel with a small cap, and time out per code", async () => {
		const answers: Record<string, Answer> = {};
		for (let i = 1; i <= 10; i++) answers[`C${i}`] = json({ result: null });
		answers.C99 = never;
		const m = mockFetch(answers);
		// the timeout runs on AbortSignal.timeout's real clock: a short one keeps the test fast
		const out = await lcscParts([...Object.keys(answers)], { fetch: m.fn, now, timeoutMs: 20 });
		expect(m.maxInFlight()).toBe(4);
		expect(out[0]).toEqual({ code: "C1", error: "LCSC has no product C1" });
		expect(out.at(-1)).toEqual({ code: "C99", error: "LCSC did not answer within 0.02 s" });
	});

	test("the caller's abort cancels the whole query", async () => {
		const ctl = new AbortController();
		const p = lcscParts(["C1"], { fetch: mockFetch({ C1: never }).fn, signal: ctl.signal, now });
		ctl.abort();
		await expect(p).rejects.toBeDefined();
	});
});
