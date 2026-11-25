import { expect, test } from "bun:test";
import { clock, mergeSegments, replayTimeline, watchReplayPath } from "./replay";

test("segments merge by time and keep recorded order on equal timestamps", () => {
  const merged = mergeSegments([
    [{ type: 4, timestamp: 1_700_000_000_100 }, { type: 2, timestamp: 1_700_000_000_100 }, { type: 3, timestamp: 1_700_000_000_300 }],
    [{ type: 3, timestamp: 1_700_000_000_200 }],
  ]);
  expect(merged.map((event) => `${event.type}@${event.timestamp}`)).toEqual([
    "4@1700000000100",
    "2@1700000000100",
    "3@1700000000200",
    "3@1700000000300",
  ]);
});

test("custom events with timestamps in seconds merge in ms order", () => {
  const merged = mergeSegments([
    [{ type: 4, timestamp: 1_795_468_793_300 }, { type: 5, timestamp: 1_795_468_793.266 }, { type: 3, timestamp: 1_795_468_794_000 }],
  ]);
  expect(merged.map((event) => `${event.type}@${event.timestamp}`)).toEqual([
    "5@1795468793266",
    "4@1795468793300",
    "3@1795468794000",
  ]);
});

test("the timeline reads SDK breadcrumbs, navigations, and errors in time order", () => {
  const events = [
    { type: 3, timestamp: 1_000 },
    { type: 5, timestamp: 1_500, data: { tag: "breadcrumb", payload: { timestamp: 2, category: "ui.click", message: "button#save" } } },
    { type: 5, timestamp: 1_600, data: { tag: "breadcrumb", payload: { timestamp: 1.25, category: "navigation", data: { from: "/", to: "/cart" } } } },
    { type: 5, timestamp: 1_700, data: { tag: "performanceSpan", payload: { op: "navigation.push", description: "/checkout", startTimestamp: 4 } } },
    { type: 5, timestamp: 1_800, data: { tag: "performanceSpan", payload: { op: "resource.fetch", description: "/api", startTimestamp: 1 } } },
  ];
  const error = { eventId: "e1", issueId: "i1", title: "TypeError: x is undefined", timestampMs: 3_000 };
  expect(replayTimeline(events, [error])).toEqual([
    { kind: "navigation", atMs: 1_250, label: "/cart", detail: null },
    { kind: "breadcrumb", atMs: 2_000, label: "ui.click", detail: "button#save" },
    { kind: "error", atMs: 3_000, label: "TypeError: x is undefined", detail: null, error },
    { kind: "navigation", atMs: 4_000, label: "/checkout", detail: null },
  ]);
});

test("watch replay opens 5 seconds before the error, never before the start", () => {
  const replay = { id: "r1", startedMs: Date.parse("2026-11-23T10:00:00Z") };
  expect(watchReplayPath("a1", replay, "2026-11-23T10:01:00Z")).toBe("/errors/apps/a1/replays/r1?t=55000");
  expect(watchReplayPath("a1", replay, "2026-11-23T10:00:02Z")).toBe("/errors/apps/a1/replays/r1?t=0");
  expect(watchReplayPath("a1", { id: "r1", startedMs: null }, "2026-11-23T10:00:02Z")).toBe("/errors/apps/a1/replays/r1");
});

test("clock shows minutes and seconds, and hours past an hour", () => {
  expect(clock(65_400)).toBe("1:05");
  expect(clock(3_725_000)).toBe("1:02:05");
});
