import type { IntakeCounts } from "@/lib/api";

export const LOG_LEVELS = ["trace", "debug", "info", "warn", "error", "fatal"] as const;
export type LogLevel = (typeof LOG_LEVELS)[number];

export type LogRow = {
  id: string;
  timeMs: number;
  level: LogLevel;
  severityText: string;
  service: string;
  environment: string;
  host: string;
  body: string;
  traceId: string | null;
  spanId: string | null;
  sourceId: string;
  sourceName: string;
};

export type LogDetail = LogRow & { attributes: Record<string, unknown>; resource: Record<string, unknown> };

/** Newest first. `tailCursor` is passed back as `?after=` to get rows newer than this page. */
export type LogPage = { items: LogRow[]; nextCursor: string | null; tailCursor: string };

export type LogFacets = {
  levels: { level: LogLevel; count: number }[];
  services: { service: string; count: number }[];
  /** True when the counts cover only the newest 10,000 rows of the search. */
  capped: boolean;
};

export type TelemetrySource = {
  id: string;
  projectId: string;
  name: string;
  tokenHint: string;
  /** Null inherits the instance default. */
  dailyQuota: number | null;
  createdAt: string;
  firstLogAt: string | null;
  intake: { logs: IntakeCounts; traces: IntakeCounts; metrics: IntakeCounts };
};

export type CreatedTelemetrySource = TelemetrySource & { token: string };

const MINUTE = 60_000;
export const MAX_RANGE_MS = 7 * 24 * 60 * MINUTE;

export const RANGE_PRESETS = [
  { id: "15m", label: "Last 15 minutes", ms: 15 * MINUTE },
  { id: "1h", label: "Last hour", ms: 60 * MINUTE },
  { id: "24h", label: "Last 24 hours", ms: 24 * 60 * MINUTE },
  { id: "7d", label: "Last 7 days", ms: MAX_RANGE_MS },
] as const;
export type RangePreset = (typeof RANGE_PRESETS)[number]["id"];

/** A preset slides with the clock; a custom range is fixed epoch ms. */
export type RangeSpec = { kind: "preset"; preset: RangePreset } | { kind: "custom"; from: number; to: number };

const epochMs = (value: string | null) => (value && /^\d+$/.test(value) ? Number(value) : null);

/** `from` and `to` in the URL win; otherwise `range`, else the last 15 minutes. */
export function parseRange(params: URLSearchParams): RangeSpec {
  const from = epochMs(params.get("from"));
  const to = epochMs(params.get("to"));
  if (from !== null && to !== null) return { kind: "custom", from, to };
  const preset = RANGE_PRESETS.find((item) => item.id === params.get("range"));
  return { kind: "preset", preset: preset?.id ?? "15m" };
}

/** Writes `range` into `params`, dropping the keys of the other kind. The default preset writes nothing. */
export function writeRange(params: URLSearchParams, range: RangeSpec) {
  params.delete("range");
  params.delete("from");
  params.delete("to");
  if (range.kind === "custom") {
    params.set("from", String(range.from));
    params.set("to", String(range.to));
  } else if (range.preset !== "15m") {
    params.set("range", range.preset);
  }
}

export function resolveRange(range: RangeSpec, now: number): { from: number; to: number } {
  if (range.kind === "custom") return { from: range.from, to: range.to };
  const ms = RANGE_PRESETS.find((item) => item.id === range.preset)!.ms;
  return { from: now - ms, to: now };
}

/** The reason a custom range cannot be searched, or null. The server checks the same rules. */
export function customRangeError(from: number, to: number): string | null {
  if (!Number.isFinite(from) || !Number.isFinite(to)) return "Pick a start and an end.";
  if (to <= from) return "The end must be after the start.";
  if (to - from > MAX_RANGE_MS) return "A range can span at most 7 days.";
  return null;
}

/** Splits a query on whitespace, keeping `"quoted text"` (with an optional `key:` or `-` before it) as one term. */
export function tokenize(query: string): string[] {
  return query.match(/(?:[^\s"]+|"[^"]*"?)+/g) ?? [];
}

function quoteValue(value: string) {
  return /[\s"]/.test(value) || value === "" ? `"${value.replace(/"/g, "")}"` : value;
}

export function filterTerm(key: string, value: string) {
  return `${key}:${quoteValue(value)}`;
}

export function hasFilter(query: string, key: string, value: string) {
  return tokenize(query).includes(filterTerm(key, value));
}

/**
 * The facet buttons. Two positive terms on one column match nothing (terms are ANDed),
 * so picking a value replaces any other positive `key:` term; picking the active value removes it.
 * Negated terms and free text stay.
 */
export function toggleFilter(query: string, key: string, value: string): string {
  const term = filterTerm(key, value);
  const tokens = tokenize(query);
  if (tokens.includes(term)) return tokens.filter((token) => token !== term).join(" ");
  return [...tokens.filter((token) => !token.startsWith(`${key}:`)), term].join(" ");
}

/** Rows from a tail poll go on top; a row already shown keeps its place. At most `cap` rows stay. */
export function mergeNewer<T extends { id: string }>(newer: T[], current: T[], cap = 1000): T[] {
  const seen = new Set<string>();
  const merged: T[] = [];
  for (const row of [...newer, ...current]) {
    if (seen.has(row.id)) continue;
    seen.add(row.id);
    merged.push(row);
    if (merged.length === cap) break;
  }
  return merged;
}

export const TAIL_INTERVAL_MS = 2000;
export const TAIL_LIMIT_MS = 10 * MINUTE;

/** Live tail: polls with `after=<tail cursor>` until `until`, then stops and says so. */
export type Tail = { status: "off" } | { status: "live"; until: number } | { status: "expired" };

/** The issue page link: the logs of one trace, an hour either side of the event. */
export function traceLogsPath(traceId: string, eventTime: string) {
  const at = Date.parse(eventTime);
  const params = new URLSearchParams({ q: `trace:${traceId}` });
  if (Number.isFinite(at)) writeRange(params, { kind: "custom", from: at - 60 * MINUTE, to: at + 60 * MINUTE });
  return `/logs?${params}`;
}

export const TRACE_ID = /^[0-9a-f]{32}$/i;
