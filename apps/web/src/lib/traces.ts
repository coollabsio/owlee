import { filterTerm, LOG_RANGES, type LogRow, type RangeSpec, writeRange } from "@/lib/logs";

export type SpanStatus = "unset" | "ok" | "error";

export type ServiceRow = {
  service: string;
  sourceId: string;
  sourceName: string;
  requests: number;
  errors: number;
  errorRate: number;
  p50Ms: number;
  p95Ms: number;
  p99Ms: number;
};

export type ServicePoint = { t: number; requests: number; errors: number; p50Ms: number; p95Ms: number; p99Ms: number };

export type OperationRow = { operation: string; requests: number; errors: number; errorRate: number; p50Ms: number; p95Ms: number; p99Ms: number };

export type ServiceDetail = { service: string; sourceId: string; bucketMs: number; series: ServicePoint[]; operations: OperationRow[] };

/** An entry span of the trace list. */
export type TraceRow = {
  traceId: string;
  spanId: string;
  service: string;
  name: string;
  operation: string;
  startMs: number;
  durationMs: number;
  status: SpanStatus;
  httpMethod: string | null;
  httpRoute: string | null;
  httpStatus: number | null;
  sourceId: string;
  sourceName: string;
};

export type TraceListPage = { items: TraceRow[]; nextCursor: string | null };

export type SpanEvent = { name: string; timeMs: number; attributes: Record<string, unknown> };
export type SpanLink = { traceId: string; spanId: string; attributes: Record<string, unknown> };

export type Span = {
  spanId: string;
  /** Empty for a root span. */
  parentSpanId: string;
  parentMissing: boolean;
  service: string;
  name: string;
  kind: string;
  startMs: number;
  durationMs: number;
  status: SpanStatus;
  statusMessage: string;
  httpMethod: string | null;
  httpRoute: string | null;
  httpStatus: number | null;
  attributes: Record<string, unknown>;
  resource: Record<string, unknown>;
  events: SpanEvent[];
  links: SpanLink[];
  sourceId: string;
  sourceName: string;
};

export type TraceError = { eventId: string; issueId: string; title: string; level: string; timestamp: string; appId: string };

export type TraceDetail = {
  traceId: string;
  startMs: number;
  durationMs: number;
  /** More than 2000 spans: `spans` holds the first 2000 by start. */
  truncated: boolean;
  spans: Span[];
  logs: LogRow[];
  errors: TraceError[];
};

/** One waterfall row, in depth-first order. The row's subtree is the `size` rows after it. */
export type TreeRow = {
  span: Span;
  depth: number;
  size: number;
  /** Hung under the root because its parent never arrived (or its parent chain loops). */
  orphan: boolean;
};

/**
 * Spans as a tree in depth-first order, siblings sorted by start. A span whose parent is not in the
 * trace hangs under the first root, as does one span of each parent cycle, so every span shows once.
 */
export function buildSpanTree(spans: Span[]): TreeRow[] {
  const byId = new Map<string, number>();
  spans.forEach((span, index) => {
    if (!byId.has(span.spanId)) byId.set(span.spanId, index);
  });
  const order = spans.map((_, index) => index).sort((a, b) => spans[a].startMs - spans[b].startMs || a - b);
  const root = order.find((index) => spans[index].parentSpanId === "") ?? -1;
  const orphan = spans.map(() => false);
  const parent = spans.map((span, index) => {
    if (span.parentSpanId === "") return -1;
    const found = byId.get(span.parentSpanId);
    if (found === undefined || span.parentMissing) {
      orphan[index] = true;
      return root;
    }
    return found;
  });

  const childrenOf = () => {
    const children = spans.map((): number[] => []);
    const top: number[] = [];
    for (const index of order) (parent[index] === -1 ? top : children[parent[index]]).push(index);
    return { children, top };
  };

  // Spans no root reaches sit on a parent cycle or below one. Each loop is cut at its earliest span.
  let { children, top } = childrenOf();
  const reached = spans.map(() => false);
  const reach = (start: number) => {
    const stack = [start];
    while (stack.length > 0) {
      const index = stack.pop()!;
      if (reached[index]) continue;
      reached[index] = true;
      stack.push(...children[index]);
    }
  };
  top.forEach(reach);
  let cut = false;
  for (const index of order) {
    if (reached[index]) continue;
    parent[index] = root;
    orphan[index] = true;
    cut = true;
    reach(index);
  }
  if (cut) ({ children, top } = childrenOf());

  const rows: TreeRow[] = [];
  const visit = (index: number, depth: number) => {
    const at = rows.length;
    rows.push({ span: spans[index], depth, size: 0, orphan: orphan[index] });
    for (const child of children[index]) visit(child, depth + 1);
    rows[at].size = rows.length - at - 1;
  };
  for (const index of top) visit(index, 0);
  return rows;
}

/** The rows to draw: a collapsed row (by position in `rows`) hides its subtree. Returns positions. */
export function visibleRows(rows: TreeRow[], collapsed: ReadonlySet<number>): number[] {
  const visible: number[] = [];
  for (let index = 0; index < rows.length; index += collapsed.has(index) ? rows[index].size + 1 : 1) visible.push(index);
  return visible;
}

const MIN_BAR_PERCENT = 0.2;

/** A span bar as percentages of the trace: `left` from the trace start, `width` at least a sliver, both inside 0..100. */
export function barLayout(startMs: number, durationMs: number, traceStartMs: number, traceDurationMs: number) {
  if (traceDurationMs <= 0) return { left: 0, width: 100 };
  const clamp = (value: number) => Math.min(100, Math.max(0, value));
  const width = Math.max(MIN_BAR_PERCENT, clamp((durationMs / traceDurationMs) * 100));
  const left = Math.min(clamp(((startMs - traceStartMs) / traceDurationMs) * 100), 100 - width);
  return { left, width };
}

const trim = (value: number, digits: number) => String(Number(value.toFixed(digits)));

/** `850 µs`, `12.3 ms`, `1.25 s`: three significant digits at most. */
export function formatDuration(ms: number) {
  if (!Number.isFinite(ms) || ms < 0) return "-";
  // The cut-offs sit just below 1 and 1000 so rounding never prints `1000 µs` or `1000 ms`.
  if (ms < 0.9995) return `${Math.round(ms * 1000)} µs`;
  const [value, unit] = ms < 999.5 ? [ms, "ms"] : [ms / 1000, "s"];
  return `${trim(value, value < 10 ? 2 : value < 100 ? 1 : 0)} ${unit}`;
}

const PALETTE = ["#3b82f6", "#10b981", "#f59e0b", "#8b5cf6", "#ec4899", "#14b8a6", "#f97316", "#6366f1", "#84cc16", "#06b6d4"];

/** One color per service of the trace, by service name order, so a trace always draws the same way. */
export function serviceColors(services: Iterable<string>): Map<string, string> {
  const sorted = [...new Set(services)].sort();
  return new Map(sorted.map((service, index) => [service, PALETTE[index % PALETTE.length]]));
}

/** The trace list of one operation: `service:X "operation"`. `(other)` lists the whole service. */
export function operationQuery(service: string, operation: string) {
  const term = filterTerm("service", service);
  if (operation === "(other)" || operation === "") return term;
  return `${term} "${operation.replace(/"/g, "")}"`;
}

/** The trace list link from the service page. The range goes along when the trace list can search it. */
export function operationTracesPath(service: string, operation: string, from: number, to: number) {
  const params = new URLSearchParams({ q: operationQuery(service, operation) });
  const range: RangeSpec = { kind: "custom", from, to };
  if (to - from <= LOG_RANGES.maxMs) writeRange(params, range);
  return `/traces?${params}`;
}

export function servicePath(service: string, sourceId: string) {
  return `/traces/services/${encodeURIComponent(service)}?${new URLSearchParams({ source: sourceId })}`;
}

export function tracePath(traceId: string, spanId?: string) {
  return `/traces/${encodeURIComponent(traceId)}${spanId ? `?${new URLSearchParams({ span: spanId })}` : ""}`;
}

/** The `exception.*` attributes of a span event named `exception`, else null. */
export function exceptionOf(event: SpanEvent) {
  if (event.name !== "exception") return null;
  const text = (key: string) => {
    const value = event.attributes[key];
    return typeof value === "string" ? value : value == null ? "" : JSON.stringify(value);
  };
  return { type: text("exception.type"), message: text("exception.message"), stack: text("exception.stacktrace") };
}
