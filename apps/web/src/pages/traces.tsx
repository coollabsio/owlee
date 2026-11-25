import { keepPreviousData, useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, X } from "reicon-react";
import { Link, NavLink, useParams, useSearchParams } from "react-router";
import { SeriesChart, Stat } from "@/components/charts";
import { type Column, DataTable } from "@/components/data-table";
import { CopyButton, Empty, ErrorText, LayerCard, LoadMore, PageHeader, Skeleton, StatusBadge } from "@/components/ui";
import { ApiError, api, flattenPages, withCursor } from "@/lib/api";
import {
  hasFilter,
  type LogRow,
  parseRange,
  type RangeOptions,
  type RangeSpec,
  resolveRange,
  SERVICE_RANGES,
  toggleFilter,
  traceLogsPath,
  writeRange,
} from "@/lib/logs";
import {
  barLayout,
  buildSpanTree,
  exceptionOf,
  formatDuration,
  type OperationRow,
  operationTracesPath,
  type ServiceDetail,
  type ServiceRow,
  servicePath,
  serviceColors,
  type Span,
  type SpanStatus,
  type TraceDetail,
  type TraceError,
  type TraceListPage,
  tracePath,
  type TraceRow,
  type TreeRow,
  visibleRows,
} from "@/lib/traces";
import { cn, fullTime, percent } from "@/lib/utils";
import { useListScope, useWorkspace } from "@/lib/workspace";
import { FacetButton, KeyValues, LEVEL_TONE, logTime, RangePicker, SearchBox, Section } from "@/pages/logs";
import { SourcesCard } from "@/pages/telemetry-sources";

const STATUS_TONE: Record<SpanStatus, "neutral" | "success" | "error"> = { unset: "neutral", ok: "success", error: "error" };

const HELP: [string, string][] = [
  ["checkout", "the span name contains this text"],
  ['"GET /checkout"', "the span name contains this phrase"],
  ["service:api", "a column: service, env, host, trace"],
  ["http.route:/checkout", "an attribute"],
  ["duration:>500ms", "also >=, <, <=; units us, ms, s"],
  ["status:error", "error, ok, or unset"],
  ["kind:server", "server, client, producer, consumer, internal"],
  ["-env:dev", "not"],
  ["a b", "both must match"],
];

function TracesTabs() {
  const tab = ({ isActive }: { isActive: boolean }) =>
    cn(
      "flex h-9 shrink-0 items-center border-b-2 px-2.5 text-[0.8125rem] font-medium transition-colors",
      isActive ? "border-[var(--ui-accent)] text-[var(--ui-fg)]" : "border-transparent text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]",
    );
  return (
    <div className="-mt-2 min-w-0 border-b border-[var(--ui-divider)]">
      <nav className="-mb-px flex gap-1 overflow-x-auto overflow-y-hidden" aria-label="Traces sections">
        <NavLink end to="/traces" className={tab}>Traces</NavLink>
        <NavLink to="/traces/services" className={tab}>Services</NavLink>
        <NavLink to="/traces/sources" className={tab}>Sources</NavLink>
      </nav>
    </div>
  );
}

/** Changes the live URL, as on the logs page: setSearchParams(fn) does not queue. */
function useUpdateParams() {
  const [, setParams] = useSearchParams();
  return (change: (next: URLSearchParams) => void, replace = false) => {
    const next = new URLSearchParams(window.location.search);
    change(next);
    setParams(next, { replace });
  };
}

function withRange(path: string, range: RangeSpec, options: RangeOptions) {
  const [base, query = ""] = path.split("?");
  const params = new URLSearchParams(query);
  writeRange(params, range, options);
  const search = params.toString();
  return search ? `${base}?${search}` : base;
}

export function TraceSourcesPage() {
  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Traces" description="Sources send logs, traces, and metrics over OTLP/HTTP." />
      <TracesTabs />
      <SourcesCard />
    </div>
  );
}

/** The URL holds the search: `q` and the range, as on the logs page. */
export function TracesPage() {
  const [params] = useSearchParams();
  const { workspace, project } = useWorkspace();
  const q = params.get("q") ?? "";
  const range = parseRange(params);
  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Traces" description="Requests that entered a service, newest first." />
      <TracesTabs />
      <TraceList key={JSON.stringify([workspace.id, project, q, range])} q={q} range={range} />
    </div>
  );
}

function TraceList({ q, range }: { q: string; range: RangeSpec }) {
  const scope = useListScope();
  const update = useUpdateParams();
  const [anchor, setAnchor] = useState(Date.now);
  const bounds = resolveRange(range, anchor);
  const search = new URLSearchParams({ q, from: String(bounds.from), to: String(bounds.to) }).toString();
  const traces = useInfiniteQuery({
    queryKey: scope.key("traces", search),
    queryFn: ({ pageParam }) => api<TraceListPage>(withCursor(scope.path(`/api/traces?${search}`), pageParam)),
    initialPageParam: "",
    getNextPageParam: (page) => page.nextCursor ?? undefined,
    placeholderData: keepPreviousData,
  });
  const fields = traces.error instanceof ApiError ? (traces.error.fields ?? {}) : {};
  const rows = flattenPages(traces.data?.pages.map((page) => page.items.map((row) => ({ ...row, id: `${row.traceId}:${row.spanId}` }))));
  const setQuery = (next: string) => update((params) => (next ? params.set("q", next) : params.delete("q")));

  const columns: Column<TraceRow & { id: string }>[] = [
    {
      key: "time",
      header: "Time",
      cellClassName: "whitespace-nowrap font-mono text-[0.75rem] text-[var(--ui-fg-dim)]",
      cell: (row) => <Link className="row-link" to={tracePath(row.traceId, row.spanId)}>{logTime(row.startMs)}</Link>,
    },
    { key: "service", header: "Service", cellClassName: "max-w-40 truncate text-[0.8125rem]", cell: (row) => <ServiceName service={row.service} /> },
    {
      key: "operation",
      header: "Operation",
      cellClassName: "max-w-0 w-full",
      cell: (row) => <span className="block truncate font-mono text-[0.75rem]" title={row.operation}>{row.operation}</span>,
    },
    { key: "duration", header: "Duration", cellClassName: "whitespace-nowrap text-right font-mono text-[0.75rem]", cell: (row) => formatDuration(row.durationMs) },
    { key: "status", header: "Status", cellClassName: "w-24", cell: (row) => <StatusBadge tone={STATUS_TONE[row.status]}>{row.status}</StatusBadge> },
    {
      key: "http",
      header: "HTTP",
      cellClassName: "w-16 font-mono text-[0.75rem]",
      cell: (row) => (row.httpStatus === null ? "" : <span className={cn(row.httpStatus >= 500 && "text-[var(--ui-error)]")}>{row.httpStatus}</span>),
    },
  ];

  return (
    <>
      <div className="flex flex-col gap-3">
        <div className="flex flex-col gap-2 lg:flex-row lg:items-start">
          <SearchBox
            q={q}
            error={fields.q}
            label="Search traces"
            placeholder="service:api duration:>500ms status:error"
            help={HELP}
            onSubmit={(next) => (next === q ? setAnchor(Date.now()) : setQuery(next))}
          />
          <RangePicker range={range} error={fields.to ?? fields.from} onChange={(next) => update((params) => writeRange(params, next))} />
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <FacetButton active={hasFilter(q, "status", "error")} onClick={() => setQuery(toggleFilter(q, "status", "error"))}>Errors only</FacetButton>
          <FacetButton active={hasFilter(q, "duration", ">1s")} onClick={() => setQuery(toggleFilter(q, "duration", ">1s"))}>Slower than 1 s</FacetButton>
        </div>
      </div>
      {traces.isError && !fields.q && !fields.to && !fields.from ? <ErrorText error={traces.error} /> : null}
      <LayerCard flush>
        {traces.isSuccess && rows.length === 0 ? (
          <p className="px-4 py-10 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">
            No traces match in this range. Widen the range or change the search.
          </p>
        ) : (
          <DataTable columns={columns} rows={rows} rowKey={(row) => row.id} rowLink={(row) => tracePath(row.traceId, row.spanId)} loading={traces.isLoading} />
        )}
        <LoadMore query={traces} label="Load older" />
      </LayerCard>
    </>
  );
}

function ServiceName({ service }: { service: string }) {
  return service ? <>{service}</> : <span className="text-[var(--ui-fg-faint)]">(none)</span>;
}

const latencyColumns = <T extends { p50Ms: number; p95Ms: number; p99Ms: number }>(): Column<T>[] =>
  (["p50Ms", "p95Ms", "p99Ms"] as const).map((key) => ({
    key,
    header: key.slice(0, 3),
    sort: (row: T) => row[key],
    cellClassName: "whitespace-nowrap text-right font-mono text-[0.75rem]",
    cell: (row: T) => formatDuration(row[key]),
  }));

function countColumns<T extends { requests: number; errorRate: number }>(): Column<T>[] {
  return [
    { key: "requests", header: "Requests", sort: (row) => row.requests, cellClassName: "text-right font-mono text-[0.75rem]", cell: (row) => row.requests.toLocaleString() },
    {
      key: "errorRate",
      header: "Error rate",
      sort: (row) => row.errorRate,
      cellClassName: "text-right font-mono text-[0.75rem]",
      cell: (row) => <span className={cn(row.errorRate > 0 && "text-[var(--ui-error)]")}>{percent(row.errorRate)}</span>,
    },
  ];
}

export function ServicesPage() {
  const [params] = useSearchParams();
  const update = useUpdateParams();
  const scope = useListScope();
  const range = parseRange(params, SERVICE_RANGES);
  const [anchor, setAnchor] = useState(Date.now);
  const bounds = resolveRange(range, anchor, SERVICE_RANGES);
  const search = new URLSearchParams({ from: String(bounds.from), to: String(bounds.to) }).toString();
  const services = useQuery({
    queryKey: scope.key("services", search),
    queryFn: () => api<{ items: ServiceRow[] }>(scope.path(`/api/services?${search}`)),
    placeholderData: keepPreviousData,
  });
  const fields = services.error instanceof ApiError ? (services.error.fields ?? {}) : {};
  const path = (row: ServiceRow) => withRange(servicePath(row.service, row.sourceId), range, SERVICE_RANGES);
  const columns: Column<ServiceRow>[] = [
    {
      key: "service",
      header: "Service",
      sort: (row) => row.service,
      cellClassName: "text-[0.8125rem]",
      cell: (row) => <Link className="row-link" to={path(row)}><ServiceName service={row.service} /></Link>,
    },
    { key: "source", header: "Source", sort: (row) => row.sourceName, cellClassName: "text-[0.8125rem] text-[var(--ui-fg-dim)]", cell: (row) => row.sourceName },
    ...countColumns<ServiceRow>(),
    ...latencyColumns<ServiceRow>(),
  ];
  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Traces" description="Request rate, errors, and latency of the entry spans of each service." />
      <TracesTabs />
      <RangePicker
        key={JSON.stringify(range)}
        range={range}
        options={SERVICE_RANGES}
        error={fields.to ?? fields.from}
        onChange={(next) => {
          setAnchor(Date.now());
          update((params) => writeRange(params, next, SERVICE_RANGES));
        }}
      />
      {services.isError && !fields.to && !fields.from ? <ErrorText error={services.error} /> : null}
      <LayerCard flush>
        {services.isSuccess && services.data.items.length === 0 ? (
          <p className="px-4 py-10 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">
            No service sent spans in this range. Services show up once a source sends traces.
          </p>
        ) : (
          <DataTable
            columns={columns}
            rows={services.data?.items ?? []}
            rowKey={(row) => `${row.sourceId}:${row.service}`}
            rowLink={path}
            loading={services.isLoading}
          />
        )}
      </LayerCard>
    </div>
  );
}

const LATENCY_COLORS = { p50: "var(--ui-accent)", p95: "var(--ui-warning)", p99: "var(--ui-error)" };

export function ServicePage() {
  const { service = "" } = useParams();
  const [params] = useSearchParams();
  const update = useUpdateParams();
  const scope = useListScope();
  const source = params.get("source") ?? "";
  const range = parseRange(params, SERVICE_RANGES);
  const [anchor, setAnchor] = useState(Date.now);
  const bounds = resolveRange(range, anchor, SERVICE_RANGES);
  const search = new URLSearchParams({ ...(source ? { source } : {}), from: String(bounds.from), to: String(bounds.to) }).toString();
  const detail = useQuery({
    queryKey: scope.key("service", service, search),
    queryFn: () => api<ServiceDetail>(scope.path(`/api/services/${encodeURIComponent(service)}?${search}`)),
    placeholderData: keepPreviousData,
  });
  const fields = detail.error instanceof ApiError ? (detail.error.fields ?? {}) : {};
  const data = detail.data;
  const series = data?.series ?? [];
  const times = series.map((point) => point.t);
  const path = (row: OperationRow) => operationTracesPath(service, row.operation, bounds.from, bounds.to);
  const columns: Column<OperationRow>[] = [
    {
      key: "operation",
      header: "Operation",
      sort: (row) => row.operation,
      cellClassName: "max-w-0 w-full",
      cell: (row) => (
        <Link className="row-link block truncate font-mono text-[0.75rem]" to={path(row)} title={row.operation}>
          {row.operation}
        </Link>
      ),
    },
    ...countColumns<OperationRow>(),
    ...latencyColumns<OperationRow>(),
  ];
  const count = (value: number) => Math.round(value).toLocaleString();
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title={service || "(none)"}
        keepTitle
        description={
          <>
            <Link className="hover:text-[var(--ui-fg)]" to={withRange("/traces/services", range, SERVICE_RANGES)}>Services</Link>
            {data ? ` · one point per ${formatDuration(data.bucketMs)}` : null}
          </>
        }
        actions={
          <Link className="button" to={operationTracesPath(service, "", bounds.from, bounds.to)}>
            View traces
          </Link>
        }
      />
      <TracesTabs />
      <RangePicker
        key={JSON.stringify(range)}
        range={range}
        options={SERVICE_RANGES}
        error={fields.to ?? fields.from}
        onChange={(next) => {
          setAnchor(Date.now());
          update((params) => writeRange(params, next, SERVICE_RANGES));
        }}
      />
      {detail.isError && !fields.to && !fields.from ? <ErrorText error={detail.error} /> : null}
      <div className="grid gap-4 lg:grid-cols-3">
        <LayerCard title="Requests">
          {detail.isLoading ? <Skeleton className="h-36 w-full" /> : <SeriesChart label="Requests per bucket" times={times} format={count} lines={[{ label: "Requests", color: "var(--ui-accent)", values: series.map((point) => point.requests) }]} />}
        </LayerCard>
        <LayerCard title="Errors">
          {detail.isLoading ? <Skeleton className="h-36 w-full" /> : <SeriesChart label="Errors per bucket" times={times} format={count} lines={[{ label: "Errors", color: "var(--ui-error)", values: series.map((point) => point.errors) }]} />}
        </LayerCard>
        <LayerCard title="Latency">
          {detail.isLoading ? (
            <Skeleton className="h-36 w-full" />
          ) : (
            <SeriesChart
              label="Latency percentiles per bucket"
              times={times}
              format={formatDuration}
              lines={[
                { label: "p50", color: LATENCY_COLORS.p50, values: series.map((point) => point.p50Ms) },
                { label: "p95", color: LATENCY_COLORS.p95, values: series.map((point) => point.p95Ms) },
                { label: "p99", color: LATENCY_COLORS.p99, values: series.map((point) => point.p99Ms) },
              ]}
            />
          )}
        </LayerCard>
      </div>
      <LayerCard title="Operations" flush>
        {detail.isSuccess && data && data.operations.length === 0 ? (
          <p className="px-4 py-10 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">No requests in this range.</p>
        ) : (
          <DataTable columns={columns} rows={data?.operations ?? []} rowKey={(row) => row.operation} rowLink={path} loading={detail.isLoading} />
        )}
      </LayerCard>
    </div>
  );
}

export function TracePage() {
  const { traceId = "" } = useParams();
  const scope = useListScope();
  const trace = useQuery({
    queryKey: scope.key("trace", traceId),
    queryFn: () => api<TraceDetail>(scope.path(`/api/traces/${encodeURIComponent(traceId)}`)),
  });
  if (trace.isLoading) return <Skeleton className="h-64 w-full" />;
  if (trace.error instanceof ApiError && trace.error.status === 404) {
    return (
      <Empty
        title="Trace not found"
        body="No span, log, or error event you can see has this trace id. Spans can arrive a few seconds late, and old traces leave with retention."
        action={<Link className="button" to="/traces">All traces</Link>}
      />
    );
  }
  if (!trace.data) return <ErrorText error={trace.error} />;
  return <TraceView key={trace.data.traceId} trace={trace.data} />;
}

function TraceView({ trace }: { trace: TraceDetail }) {
  const [params] = useSearchParams();
  const update = useUpdateParams();
  const rows = useMemo(() => buildSpanTree(trace.spans), [trace.spans]);
  const colors = useMemo(() => serviceColors(trace.spans.map((span) => span.service)), [trace.spans]);
  const selectedId = params.get("span");
  const selected = trace.spans.find((span) => span.spanId === selectedId) ?? null;
  const root = rows[0]?.span;
  // A stable callback keeps the memoized rows from re-rendering on every selection.
  const updateRef = useRef(update);
  useEffect(() => {
    updateRef.current = update;
  });
  const select = useCallback(
    (spanId: string | null) =>
      updateRef.current((next) => {
        if (spanId && spanId !== next.get("span")) next.set("span", spanId);
        else next.delete("span");
      }, true),
    [],
  );
  return (
    <div className="flex flex-col gap-6">
      <div>
        <nav className="mb-2 flex items-center gap-2 text-[0.8125rem] text-[var(--ui-fg-faint)]">
          <Link to="/traces">Traces</Link>
          <span>/</span>
          <span className="font-mono text-[var(--ui-fg-dim)]">{trace.traceId}</span>
          <span className="relative inline-block size-8">
            <CopyButton text={trace.traceId} label="Copy trace id" />
          </span>
        </nav>
        <PageHeader title={root ? root.name : "Trace"} keepTitle />
      </div>
      <LayerCard>
        <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
          <Stat label="Duration" value={formatDuration(trace.durationMs)} />
          <Stat label="Spans" value={trace.spans.length.toLocaleString()} />
          <Stat label="Services" value={colors.size} />
          <Stat label="Started" value={logTime(trace.startMs)} />
        </div>
        <ul className="mt-4 flex flex-wrap gap-3 text-[0.75rem] text-[var(--ui-fg-dim)]" aria-label="Services">
          {[...colors].map(([service, color]) => (
            <li key={service} className="flex items-center gap-1.5">
              <i className="inline-block size-2.5 rounded-sm" style={{ background: color }} aria-hidden="true" />
              <ServiceName service={service} />
            </li>
          ))}
        </ul>
      </LayerCard>
      {trace.truncated ? (
        <p className="text-[0.8125rem] text-[var(--ui-warning)]">
          This trace has more than {trace.spans.length.toLocaleString()} spans. The waterfall shows the first {trace.spans.length.toLocaleString()} by start time.
        </p>
      ) : null}
      <div className={cn("grid gap-4", selected && "xl:grid-cols-[minmax(0,1fr)_28rem]")}>
        <LayerCard title="Waterfall" flush>
          {rows.length === 0 ? (
            <p className="px-4 py-10 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">No spans of this trace arrived. Its logs and errors are below.</p>
          ) : (
            <Waterfall trace={trace} rows={rows} colors={colors} selectedId={selected?.spanId ?? null} onSelect={select} />
          )}
        </LayerCard>
        {selected ? <SpanPanel span={selected} color={colors.get(selected.service) ?? ""} onClose={() => select(null)} /> : null}
      </div>
      <TraceLogs trace={trace} />
      <TraceErrors errors={trace.errors} />
    </div>
  );
}

function Waterfall({
  trace,
  rows,
  colors,
  selectedId,
  onSelect,
}: {
  trace: TraceDetail;
  rows: TreeRow[];
  colors: Map<string, string>;
  selectedId: string | null;
  onSelect: (spanId: string) => void;
}) {
  const [collapsed, setCollapsed] = useState<ReadonlySet<number>>(() => new Set());
  const visible = useMemo(() => visibleRows(rows, collapsed), [rows, collapsed]);
  const toggle = useCallback(
    (index: number) =>
      setCollapsed((current) => {
        const next = new Set(current);
        if (!next.delete(index)) next.add(index);
        return next;
      }),
    [],
  );
  return (
    <div className="overflow-x-auto">
      <div className="min-w-[40rem]">
        <div className="flex h-7 items-center border-b border-[var(--ui-divider)] font-mono text-[0.625rem] text-[var(--ui-fg-faint)]">
          <div className="w-[40%] shrink-0 px-3">Span</div>
          <div className="relative h-full flex-1">
            {[0, 0.25, 0.5, 0.75, 1].map((ratio) => (
              <span
                key={ratio}
                className={cn("absolute top-1/2 -translate-y-1/2 px-1", ratio === 1 ? "-translate-x-full" : ratio > 0 && "-translate-x-1/2")}
                style={{ left: `${ratio * 100}%` }}
              >
                {formatDuration(trace.durationMs * ratio)}
              </span>
            ))}
          </div>
        </div>
        {visible.map((index) => {
          const row = rows[index];
          return (
            <SpanRow
              key={index}
              index={index}
              row={row}
              traceStartMs={trace.startMs}
              traceDurationMs={trace.durationMs}
              color={colors.get(row.span.service) ?? ""}
              selected={row.span.spanId === selectedId}
              collapsed={collapsed.has(index)}
              onSelect={onSelect}
              onToggle={toggle}
            />
          );
        })}
      </div>
    </div>
  );
}

const SpanRow = memo(function SpanRow({
  index,
  row,
  traceStartMs,
  traceDurationMs,
  color,
  selected,
  collapsed,
  onSelect,
  onToggle,
}: {
  index: number;
  row: TreeRow;
  traceStartMs: number;
  traceDurationMs: number;
  color: string;
  selected: boolean;
  collapsed: boolean;
  onSelect: (spanId: string) => void;
  onToggle: (index: number) => void;
}) {
  const { span, depth, size, orphan } = row;
  const bar = barLayout(span.startMs, span.durationMs, traceStartMs, traceDurationMs);
  const error = span.status === "error";
  const label = `${span.name}, ${span.service || "no service"}, ${formatDuration(span.durationMs)}${error ? ", error" : ""}${orphan ? ", parent not received" : ""}`;
  return (
    <div
      className={cn(
        "relative flex h-7 items-center border-b border-[var(--ui-divider)] text-[0.75rem] last:border-b-0 hover:bg-[var(--ui-hover)]",
        selected && "bg-[color-mix(in_oklab,var(--ui-accent)_12%,transparent)]",
      )}
      style={{ contentVisibility: "auto", containIntrinsicSize: "auto 1.75rem" }}
    >
      <button
        type="button"
        className="absolute inset-0 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-[var(--ui-accent)]"
        aria-label={label}
        aria-pressed={selected}
        onClick={() => onSelect(span.spanId)}
      />
      <div className="pointer-events-none relative flex w-[40%] min-w-0 shrink-0 items-center gap-1.5 pr-2" style={{ paddingLeft: `${0.5 + Math.min(depth, 20) * 0.875}rem` }}>
        {size > 0 ? (
          <button
            type="button"
            className="pointer-events-auto grid size-5 shrink-0 place-items-center rounded text-[var(--ui-fg-faint)] hover:bg-[var(--ui-hover)] hover:text-[var(--ui-fg)]"
            aria-label={`${collapsed ? "Expand" : "Collapse"} ${span.name}`}
            aria-expanded={!collapsed}
            onClick={() => onToggle(index)}
          >
            <ChevronDown className={cn("size-3.5 transition-transform", collapsed && "-rotate-90")} />
          </button>
        ) : (
          <span className="size-5 shrink-0" />
        )}
        <i className="inline-block size-2 shrink-0 rounded-sm" style={{ background: color }} aria-hidden="true" />
        <span className="shrink-0 text-[var(--ui-fg-faint)]">{span.service}</span>
        <span className={cn("min-w-0 truncate", error ? "text-[var(--ui-error)]" : "text-[var(--ui-fg)]")} title={span.name}>{span.name}</span>
        {orphan ? <span className="shrink-0 text-[0.6875rem] italic text-[var(--ui-fg-faint)]">parent not received</span> : null}
        {collapsed ? <span className="shrink-0 text-[0.6875rem] text-[var(--ui-fg-faint)]">+{size}</span> : null}
      </div>
      <div className="pointer-events-none relative h-full flex-1 border-l border-[var(--ui-divider)]">
        <div
          className="absolute top-1/2 h-3 -translate-y-1/2 rounded-[2px]"
          style={{ left: `${bar.left}%`, width: `${bar.width}%`, background: color, boxShadow: error ? "0 0 0 1.5px var(--ui-error)" : undefined }}
        />
        <span
          className="absolute top-1/2 -translate-y-1/2 px-1.5 font-mono text-[0.625rem] whitespace-nowrap text-[var(--ui-fg-dim)]"
          style={bar.left + bar.width > 75 ? { right: `${100 - bar.left}%` } : { left: `${bar.left + bar.width}%` }}
        >
          {formatDuration(span.durationMs)}
        </span>
      </div>
    </div>
  );
});

function SpanPanel({ span, color, onClose }: { span: Span; color: string; onClose: () => void }) {
  return (
    <section className="layer-card self-start xl:sticky xl:top-4" aria-label="Span detail">
      <header>
        <div className="min-w-0 py-0.5">
          <h2 className="break-all">{span.name}</h2>
          <p className="flex items-center gap-1.5">
            <i className="inline-block size-2 rounded-sm" style={{ background: color }} aria-hidden="true" />
            {span.service}
          </p>
        </div>
        <button type="button" className="icon-button" aria-label="Close span" onClick={onClose}>
          <X className="size-4" />
        </button>
      </header>
      <div className="layer-card-body flex max-h-[80vh] flex-col gap-4 overflow-y-auto">
        <div className="flex flex-wrap items-center gap-2">
          <StatusBadge tone={STATUS_TONE[span.status]}>{span.status}</StatusBadge>
          <span className="font-mono text-[0.75rem] text-[var(--ui-fg-dim)]">{formatDuration(span.durationMs)}</span>
        </div>
        {span.statusMessage ? <p className="break-words text-[0.8125rem] text-[var(--ui-error)]">{span.statusMessage}</p> : null}
        <KeyValues
          entries={[
            ["Kind", span.kind],
            ["Started", logTime(span.startMs)],
            ["HTTP method", span.httpMethod ?? ""],
            ["HTTP route", span.httpRoute ?? ""],
            ["HTTP status", span.httpStatus === null ? "" : String(span.httpStatus)],
            ["Source", span.sourceName],
            ["Span id", span.spanId],
            ["Parent id", span.parentSpanId],
          ]}
        />
        <Section title="Attributes" values={span.attributes} />
        <Section title="Resource" values={span.resource} />
        <div>
          <h3 className="mb-1.5 text-[0.75rem] font-medium text-[var(--ui-fg-dim)]">Events</h3>
          {span.events.length === 0 ? (
            <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">None.</p>
          ) : (
            <ul className="flex flex-col gap-2">
              {span.events.map((event, index) => {
                const exception = exceptionOf(event);
                return (
                  <li
                    key={index}
                    className={cn(
                      "rounded-lg border p-2.5 text-[0.8125rem]",
                      exception ? "border-[color-mix(in_oklab,var(--ui-error)_40%,transparent)] bg-[color-mix(in_oklab,var(--ui-error)_6%,transparent)]" : "border-[var(--ui-control-border)]",
                    )}
                  >
                    <p className="flex justify-between gap-2">
                      <span className={cn("font-medium", exception && "text-[var(--ui-error)]")}>{exception?.type || event.name}</span>
                      <span className="font-mono text-[0.6875rem] text-[var(--ui-fg-faint)]">+{formatDuration(event.timeMs - span.startMs)}</span>
                    </p>
                    {exception ? (
                      <>
                        {exception.message ? <p className="mt-1 break-words">{exception.message}</p> : null}
                        {exception.stack ? (
                          <pre className="mt-2 max-h-60 overflow-auto rounded-md bg-[var(--ui-control-bg)] p-2 font-mono text-[0.6875rem] whitespace-pre text-[var(--ui-fg-dim)]">{exception.stack}</pre>
                        ) : null}
                      </>
                    ) : Object.keys(event.attributes).length > 0 ? (
                      <div className="mt-1.5">
                        <KeyValues entries={Object.entries(event.attributes).map(([key, value]) => [key, typeof value === "string" ? value : JSON.stringify(value)])} />
                      </div>
                    ) : null}
                  </li>
                );
              })}
            </ul>
          )}
        </div>
        <div>
          <h3 className="mb-1.5 text-[0.75rem] font-medium text-[var(--ui-fg-dim)]">Links</h3>
          {span.links.length === 0 ? (
            <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">None.</p>
          ) : (
            <ul className="flex flex-col gap-1.5">
              {span.links.map((link, index) => (
                <li key={index} className="font-mono text-[0.75rem]">
                  <Link className="break-all text-[var(--ui-accent)] hover:underline" to={tracePath(link.traceId, link.spanId)}>
                    {link.traceId} / {link.spanId}
                  </Link>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </section>
  );
}

function TraceLogs({ trace }: { trace: TraceDetail }) {
  const search = traceLogsPath(trace.traceId, new Date(trace.startMs).toISOString());
  const logPath = (row: LogRow) => `${search}&log=${encodeURIComponent(row.id)}`;
  const columns: Column<LogRow>[] = [
    {
      key: "time",
      header: "Time",
      cellClassName: "whitespace-nowrap font-mono text-[0.75rem] text-[var(--ui-fg-dim)]",
      cell: (row) => <Link className="row-link" to={logPath(row)}>{logTime(row.timeMs)}</Link>,
    },
    { key: "level", header: "Level", cellClassName: "w-24", cell: (row) => <StatusBadge tone={LEVEL_TONE[row.level]}>{row.level}</StatusBadge> },
    { key: "service", header: "Service", cellClassName: "max-w-40 truncate text-[0.8125rem]", cell: (row) => row.service },
    {
      key: "body",
      header: "Message",
      cellClassName: "max-w-0 w-full",
      cell: (row) => <span className="block truncate font-mono text-[0.75rem]" title={row.body}>{row.body}</span>,
    },
  ];
  return (
    <LayerCard
      title="Logs"
      description={trace.logs.length >= 200 ? "The first 200 logs of the trace." : undefined}
      actions={<Link className="button" to={search}>Search these logs</Link>}
      flush
    >
      {trace.logs.length === 0 ? (
        <p className="px-4 py-8 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">No logs carry this trace id.</p>
      ) : (
        <DataTable columns={columns} rows={trace.logs} rowKey={(row) => row.id} rowLink={logPath} />
      )}
    </LayerCard>
  );
}

function TraceErrors({ errors }: { errors: TraceError[] }) {
  const path = (row: TraceError) => `/errors/${encodeURIComponent(row.issueId)}/events/${encodeURIComponent(row.eventId)}`;
  const columns: Column<TraceError>[] = [
    {
      key: "title",
      header: "Error",
      cellClassName: "max-w-0 w-full",
      cell: (row) => <Link className="row-link block truncate text-[0.8125rem]" to={path(row)} title={row.title}>{row.title}</Link>,
    },
    { key: "level", header: "Level", cellClassName: "w-24", cell: (row) => <StatusBadge tone={row.level === "warning" ? "warning" : row.level === "info" ? "info" : "error"}>{row.level}</StatusBadge> },
    { key: "time", header: "Time", cellClassName: "whitespace-nowrap text-[0.75rem] text-[var(--ui-fg-dim)]", cell: (row) => fullTime(row.timestamp) },
  ];
  return (
    <LayerCard title="Errors" flush>
      {errors.length === 0 ? (
        <p className="px-4 py-8 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">No error events carry this trace id.</p>
      ) : (
        <DataTable columns={columns} rows={errors} rowKey={(row) => row.eventId} rowLink={path} />
      )}
    </LayerCard>
  );
}
