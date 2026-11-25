import { expect, test } from "bun:test";
import { customRangeError, mergeNewer, parseRange, resolveRange, toggleFilter, tokenize, traceLogsPath, writeRange } from "./logs";

test("from and to in the URL make a custom range; otherwise the preset, default 15 minutes", () => {
  expect(parseRange(new URLSearchParams("from=1000&to=2000&range=1h"))).toEqual({ kind: "custom", from: 1000, to: 2000 });
  expect(parseRange(new URLSearchParams("range=24h"))).toEqual({ kind: "preset", preset: "24h" });
  expect(parseRange(new URLSearchParams("range=bogus&from=12"))).toEqual({ kind: "preset", preset: "15m" });
  expect(parseRange(new URLSearchParams("from=abc&to=2000"))).toEqual({ kind: "preset", preset: "15m" });
});

test("writing a range keeps other params and drops the other kind's keys", () => {
  const params = new URLSearchParams("q=timeout&range=1h");
  writeRange(params, { kind: "custom", from: 5, to: 9 });
  expect(params.toString()).toBe("q=timeout&from=5&to=9");
  writeRange(params, { kind: "preset", preset: "7d" });
  expect(params.toString()).toBe("q=timeout&range=7d");
  writeRange(params, { kind: "preset", preset: "15m" });
  expect(params.toString()).toBe("q=timeout");
});

test("presets resolve against now; custom ranges stay fixed", () => {
  const now = 1_800_000_000_000;
  expect(resolveRange({ kind: "preset", preset: "15m" }, now)).toEqual({ from: 1_799_999_100_000, to: now });
  expect(resolveRange({ kind: "preset", preset: "7d" }, now)).toEqual({ from: 1_799_395_200_000, to: now });
  expect(resolveRange({ kind: "custom", from: 1, to: 2 }, now)).toEqual({ from: 1, to: 2 });
});

test("a custom range must end after it starts and span at most 7 days", () => {
  expect(customRangeError(1000, 1000)).toBe("The end must be after the start.");
  expect(customRangeError(0, 604_800_000)).toBeNull();
  expect(customRangeError(0, 604_800_001)).toBe("A range can span at most 7 days.");
  expect(customRangeError(Number.NaN, 5)).toBe("Pick a start and an end.");
});

test("tokenize keeps quoted phrases and key:\"quoted\" values whole", () => {
  expect(tokenize(`  "connection reset" service:"web api" -env:dev level:>=warn  `)).toEqual([
    `"connection reset"`,
    `service:"web api"`,
    "-env:dev",
    "level:>=warn",
  ]);
});

test("a facet button adds its term, replaces another value of the same column, and removes itself", () => {
  expect(toggleFilter("timeout", "level", "error")).toBe("timeout level:error");
  expect(toggleFilter("timeout level:>=warn -level:debug", "level", "error")).toBe("timeout -level:debug level:error");
  expect(toggleFilter("level:error timeout", "level", "error")).toBe("timeout");
  expect(toggleFilter("", "service", "web api")).toBe(`service:"web api"`);
  expect(toggleFilter(`service:"web api" x`, "service", "web api")).toBe("x");
  expect(toggleFilter("service:api", "level", "info")).toBe("service:api level:info");
});

test("tail rows go on top, duplicates keep their first copy, and the list is capped", () => {
  const rows = (ids: string) => ids.split("").map((id) => ({ id }));
  expect(mergeNewer(rows("dc"), rows("cba")).map((row) => row.id).join("")).toBe("dcba");
  expect(mergeNewer(rows("fed"), rows("cba"), 4).map((row) => row.id).join("")).toBe("fedc");
  expect(mergeNewer([], rows("ba")).map((row) => row.id).join("")).toBe("ba");
});

test("the trace link spans an hour either side of the event", () => {
  expect(traceLogsPath("0af7651916cd43dd8448eb211c80319c", "2026-11-23T10:00:00.000Z")).toBe(
    "/logs?q=trace%3A0af7651916cd43dd8448eb211c80319c&from=1795424400000&to=1795431600000",
  );
});
