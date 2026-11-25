import { expect, test } from "bun:test";
import { customRangeError, parseRange, resolveRange, SERVICE_RANGES, writeRange } from "./logs";
import {
  barLayout,
  buildSpanTree,
  exceptionOf,
  formatDuration,
  operationQuery,
  operationTracesPath,
  serviceColors,
  type Span,
  servicePath,
  tracePath,
  visibleRows,
} from "./traces";

function span(spanId: string, parentSpanId: string, startMs: number, extra: Partial<Span> = {}): Span {
  return {
    spanId,
    parentSpanId,
    parentMissing: false,
    service: "api",
    name: spanId,
    kind: "server",
    startMs,
    durationMs: 1,
    status: "unset",
    statusMessage: "",
    httpMethod: null,
    httpRoute: null,
    httpStatus: null,
    attributes: {},
    resource: {},
    events: [],
    links: [],
    sourceId: "s",
    sourceName: "S",
    ...extra,
  };
}

const shape = (spans: Span[]) => buildSpanTree(spans).map((row) => `${"  ".repeat(row.depth)}${row.span.spanId}/${row.size}${row.orphan ? "!" : ""}`);

test("spans form a depth-first tree with siblings by start time", () => {
  expect(shape([span("root", "", 0), span("b", "root", 20), span("a", "root", 10), span("a1", "a", 11), span("b1", "b", 25)])).toEqual([
    "root/4",
    "  a/1",
    "    a1/0",
    "  b/1",
    "    b1/0",
  ]);
});

test("a span whose parent never arrived hangs under the root, marked", () => {
  expect(
    shape([span("root", "", 0), span("late", "gone", 5), span("flagged", "root", 3, { parentMissing: true }), span("child", "late", 6)]),
  ).toEqual(["root/3", "  flagged/0!", "  late/1!", "    child/0"]);
});

test("without a root, orphans are top-level rows", () => {
  expect(shape([span("x", "gone", 2), span("y", "gone", 1)])).toEqual(["y/0!", "x/0!"]);
});

test("parent cycles end: each loop is cut at its earliest span and every span shows once", () => {
  expect(shape([span("root", "", 0), span("a", "b", 1), span("b", "a", 2), span("c", "b", 3)])).toEqual([
    "root/3",
    "  a/2!",
    "    b/1",
    "      c/0",
  ]);
  expect(shape([span("self", "self", 0)])).toEqual(["self/0!"]);
});

test("a 2000-span chain builds without blowing the stack", () => {
  const spans = Array.from({ length: 2000 }, (_, index) => span(`s${index}`, index === 0 ? "" : `s${index - 1}`, index));
  const rows = buildSpanTree(spans);
  expect(rows.length).toBe(2000);
  expect(rows[0].size).toBe(1999);
  expect(rows[1999].depth).toBe(1999);
});

test("collapsing a row hides its subtree and only that", () => {
  const rows = buildSpanTree([span("root", "", 0), span("a", "root", 1), span("a1", "a", 2), span("a2", "a", 3), span("b", "root", 4)]);
  expect(visibleRows(rows, new Set())).toEqual([0, 1, 2, 3, 4]);
  expect(visibleRows(rows, new Set([1]))).toEqual([0, 1, 4]);
  expect(visibleRows(rows, new Set([0]))).toEqual([0]);
  expect(visibleRows(rows, new Set([2]))).toEqual([0, 1, 2, 3, 4]);
});

test("bars sit at their offset and width in the trace, clamped and never invisible", () => {
  expect(barLayout(1250, 500, 1000, 1000)).toEqual({ left: 25, width: 50 });
  expect(barLayout(1000, 0, 1000, 1000)).toEqual({ left: 0, width: 0.2 });
  expect(barLayout(2000, 0, 1000, 1000)).toEqual({ left: 99.8, width: 0.2 });
  expect(barLayout(900, 5000, 1000, 1000)).toEqual({ left: 0, width: 100 });
  expect(barLayout(1000, 0, 1000, 0)).toEqual({ left: 0, width: 100 });
});

test("durations read in µs, ms, or s with three significant digits", () => {
  expect(formatDuration(0.0004)).toBe("0 µs");
  expect(formatDuration(0.85)).toBe("850 µs");
  expect(formatDuration(1.234)).toBe("1.23 ms");
  expect(formatDuration(12.34)).toBe("12.3 ms");
  expect(formatDuration(250)).toBe("250 ms");
  expect(formatDuration(0.9998)).toBe("1 ms");
  expect(formatDuration(999.6)).toBe("1 s");
  expect(formatDuration(1500)).toBe("1.5 s");
  expect(formatDuration(125_000)).toBe("125 s");
  expect(formatDuration(-1)).toBe("-");
});

test("services get palette colors by name order, the same in any span order", () => {
  expect([...serviceColors(["web", "api", "web", "db"])]).toEqual([
    ["api", "#3b82f6"],
    ["db", "#10b981"],
    ["web", "#f59e0b"],
  ]);
});

test("an operation row searches its service and the operation text", () => {
  expect(operationQuery("checkout-api", "GET /checkout")).toBe(`service:checkout-api "GET /checkout"`);
  expect(operationQuery("web api", `say "hi"`)).toBe(`service:"web api" "say hi"`);
  expect(operationQuery("api", "(other)")).toBe("service:api");
  expect(operationTracesPath("api", "GET /x", 1000, 2000)).toBe("/traces?q=service%3Aapi+%22GET+%2Fx%22&from=1000&to=2000");
  expect(operationTracesPath("api", "GET /x", 0, 30 * 86_400_000)).toBe("/traces?q=service%3Aapi+%22GET+%2Fx%22");
});

test("service and trace paths encode their parts", () => {
  expect(servicePath("web/api", "src1")).toBe("/traces/services/web%2Fapi?source=src1");
  expect(tracePath("abc")).toBe("/traces/abc");
  expect(tracePath("abc", "def")).toBe("/traces/abc?span=def");
});

test("an exception event exposes its type, message, and stack; other events do not", () => {
  expect(exceptionOf({ name: "exception", timeMs: 0, attributes: { "exception.type": "TypeError", "exception.message": "boom", "exception.stacktrace": "at x" } })).toEqual({
    type: "TypeError",
    message: "boom",
    stack: "at x",
  });
  expect(exceptionOf({ name: "retry", timeMs: 0, attributes: {} })).toBeNull();
});

test("service ranges default to 24 hours and allow 90 days", () => {
  expect(parseRange(new URLSearchParams(""), SERVICE_RANGES)).toEqual({ kind: "preset", preset: "24h" });
  expect(parseRange(new URLSearchParams("range=15m"), SERVICE_RANGES)).toEqual({ kind: "preset", preset: "24h" });
  expect(resolveRange({ kind: "preset", preset: "90d" }, 10_000_000_000, SERVICE_RANGES)).toEqual({ from: 2_224_000_000, to: 10_000_000_000 });
  const params = new URLSearchParams("range=7d");
  writeRange(params, { kind: "preset", preset: "24h" }, SERVICE_RANGES);
  expect(params.toString()).toBe("");
  expect(customRangeError(0, 90 * 86_400_000, SERVICE_RANGES)).toBeNull();
  expect(customRangeError(0, 90 * 86_400_000 + 1, SERVICE_RANGES)).toBe("A range can span at most 90 days.");
});
