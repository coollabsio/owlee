import { keepPreviousData, useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { type FormEvent, type ReactNode, useEffect, useRef, useState } from "react";
import { X } from "reicon-react";
import { Link, NavLink, useSearchParams } from "react-router";
import { type Column, DataTable } from "@/components/data-table";
import { Button, ErrorText, Input, LayerCard, LoadMore, PageHeader, Select, Skeleton, StatusBadge } from "@/components/ui";
import { ApiError, api, flattenPages, withCursor } from "@/lib/api";
import {
  customRangeError,
  hasFilter,
  LOG_LEVELS,
  type LogDetail,
  type LogFacets,
  type LogLevel,
  type LogPage,
  type LogRow,
  mergeNewer,
  LOG_RANGES,
  parseRange,
  type RangeOptions,
  type RangeSpec,
  resolveRange,
  TAIL_INTERVAL_MS,
  TAIL_LIMIT_MS,
  type Tail,
  toggleFilter,
  writeRange,
} from "@/lib/logs";
import { cn, toLocalInput } from "@/lib/utils";
import { useListScope, useWorkspace, withProject } from "@/lib/workspace";
import { SourcesCard } from "@/pages/telemetry-sources";

export const LEVEL_TONE: Record<LogLevel, "neutral" | "info" | "warning" | "error"> = {
  trace: "neutral",
  debug: "neutral",
  info: "info",
  warn: "warning",
  error: "error",
  fatal: "error",
};

function LogsTabs() {
  const tab = ({ isActive }: { isActive: boolean }) =>
    cn(
      "flex h-9 shrink-0 items-center border-b-2 px-2.5 text-[0.8125rem] font-medium transition-colors",
      isActive ? "border-[var(--ui-accent)] text-[var(--ui-fg)]" : "border-transparent text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]",
    );
  return (
    <div className="-mt-2 min-w-0 border-b border-[var(--ui-divider)]">
      <nav className="-mb-px flex gap-1 overflow-x-auto overflow-y-hidden" aria-label="Logs sections">
        <NavLink end to="/logs" className={tab}>Search</NavLink>
        <NavLink to="/logs/sources" className={tab}>Sources</NavLink>
      </nav>
    </div>
  );
}

export function LogSourcesPage() {
  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Logs" description="Sources send logs, traces, and metrics over OTLP/HTTP." />
      <LogsTabs />
      <SourcesCard />
    </div>
  );
}

/** The URL holds the search: `q`, the range (`range`, or `from` and `to` in epoch ms), and the open log. */
export function LogsPage() {
  const [params, setParams] = useSearchParams();
  const { workspace, project } = useWorkspace();
  const q = params.get("q") ?? "";
  const range = parseRange(params);
  const update = (change: (next: URLSearchParams) => void) => {
    // Reads the live URL, as on the monitor list: setSearchParams(fn) does not queue.
    const next = new URLSearchParams(window.location.search);
    change(next);
    setParams(next);
  };
  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Logs" description="Search the logs your OpenTelemetry sources send." />
      <LogsTabs />
      {/* A new search remounts the view, so the paged rows, the tail, and the preset's clock start fresh. */}
      <LogsView key={JSON.stringify([workspace.id, project, q, range])} q={q} range={range} selected={params.get("log")} update={update} />
    </div>
  );
}

function LogsView({
  q,
  range,
  selected,
  update,
}: {
  q: string;
  range: RangeSpec;
  selected: string | null;
  update: (change: (next: URLSearchParams) => void) => void;
}) {
  const scope = useListScope();
  const { project } = useWorkspace();
  const [anchor, setAnchor] = useState(Date.now);
  const bounds = resolveRange(range, anchor);
  const search = new URLSearchParams({ q, from: String(bounds.from), to: String(bounds.to) }).toString();
  const logs = useInfiniteQuery({
    queryKey: scope.key("logs", search),
    queryFn: ({ pageParam }) => api<LogPage>(withCursor(scope.path(`/api/logs?${search}`), pageParam)),
    initialPageParam: "",
    getNextPageParam: (page) => page.nextCursor ?? undefined,
    placeholderData: keepPreviousData,
  });
  const facets = useQuery({
    queryKey: scope.key("log-facets", search),
    queryFn: () => api<LogFacets>(scope.path(`/api/logs/facets?${search}`)),
    placeholderData: keepPreviousData,
  });
  const fields = logs.error instanceof ApiError ? (logs.error.fields ?? {}) : {};

  const [tail, setTail] = useState<Tail>({ status: "off" });
  const [tailRows, setTailRows] = useState<LogRow[]>([]);
  const [tailError, setTailError] = useState<unknown>(null);
  // Changes on every poll and never renders, so it is a ref, not state.
  const tailCursor = useRef<string | null>(null);
  const firstPage = logs.data?.pages[0];
  useEffect(() => {
    if (tail.status !== "live") return;
    let busy = false;
    let cancelled = false;
    const timer = setInterval(() => {
      if (Date.now() >= tail.until) {
        setTail({ status: "expired" });
        return;
      }
      if (busy || document.visibilityState !== "visible" || !tailCursor.current) return;
      busy = true;
      // The tail looks at the last hour up to now, whatever range the search shows.
      const now = Date.now();
      const params = new URLSearchParams({ q, from: String(now - 3_600_000), to: String(now + 60_000), after: tailCursor.current });
      api<LogPage>(withProject(`/api/logs?${params}`, project))
        .then((page) => {
          if (cancelled) return;
          tailCursor.current = page.tailCursor;
          if (page.items.length > 0) setTailRows((rows) => mergeNewer(page.items, rows));
          setTailError(null);
        })
        .catch((error: unknown) => {
          if (!cancelled) setTailError(error);
        })
        .finally(() => {
          busy = false;
        });
    }, TAIL_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [tail, q, project]);

  const startTail = () => {
    tailCursor.current ??= firstPage?.tailCursor ?? null;
    setTailError(null);
    setTail({ status: "live", until: Date.now() + TAIL_LIMIT_MS });
  };
  const rows = flattenPages([tailRows, ...(logs.data?.pages.map((page) => page.items) ?? [])]);
  const select = (id: string | null) =>
    update((next) => {
      if (id) next.set("log", id);
      else next.delete("log");
    });

  const columns: Column<LogRow>[] = [
    { key: "time", header: "Time", cellClassName: "whitespace-nowrap font-mono text-[0.75rem] text-[var(--ui-fg-dim)]", cell: (row) => (
        // No handler: Enter on the button bubbles a click to the row, which opens the log.
        <button type="button" className="row-link" aria-label={`Open log from ${logTime(row.timeMs)}`}>{logTime(row.timeMs)}</button>
      ),
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
    <>
      <div className="flex flex-col gap-3">
        <div className="flex flex-col gap-2 lg:flex-row lg:items-start">
          <SearchBox q={q} error={fields.q} label="Search logs" placeholder="timeout service:api level:>=warn -env:dev" help={HELP} onSubmit={(next) => (next === q ? setAnchor(Date.now()) : update((params) => (next ? params.set("q", next) : params.delete("q"))))} />
          <RangePicker range={range} error={fields.to ?? fields.from} onChange={(next) => update((params) => writeRange(params, next))} />
          <TailButton tail={tail} disabled={range.kind === "custom" || !firstPage} onStart={startTail} onStop={() => setTail({ status: "off" })} />
        </div>
        {tail.status === "expired" ? (
          <p className="flex items-center gap-2 text-[0.8125rem] text-[var(--ui-fg-faint)]">
            Live tail stopped after 10 minutes.
            <Button size="sm" onClick={startTail}>Start again</Button>
          </p>
        ) : null}
        {tailError ? <ErrorText error={tailError} /> : null}
        <FacetBar facets={facets.data} q={q} onToggle={(key, value) => update((params) => params.set("q", toggleFilter(q, key, value)))} />
      </div>
      {logs.isError && !fields.q && !fields.to && !fields.from ? <ErrorText error={logs.error} /> : null}
      <div className={cn("grid gap-4", selected && "xl:grid-cols-[minmax(0,1fr)_28rem]")}>
        <LayerCard flush>
          {logs.isSuccess && rows.length === 0 ? (
            <p className="px-4 py-10 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">
              No logs match in this range. Widen the range or change the search.
            </p>
          ) : (
            <DataTable
              columns={columns}
              rows={rows}
              rowKey={(row) => row.id}
              onRowClick={(row) => select(row.id === selected ? null : row.id)}
              rowSelected={(row) => row.id === selected}
              loading={logs.isLoading}
            />
          )}
          <LoadMore query={logs} label="Load older" />
        </LayerCard>
        {selected ? <LogPanel id={selected} onClose={() => select(null)} /> : null}
      </div>
    </>
  );
}

/** The query box of a telemetry search, with its syntax popover. */
export function SearchBox({
  q,
  error,
  label,
  placeholder,
  help,
  onSubmit,
}: {
  q: string;
  error?: string;
  label: string;
  placeholder: string;
  help: [string, string][];
  onSubmit: (q: string) => void;
}) {
  const [draft, setDraft] = useState(q);
  function submit(event: FormEvent) {
    event.preventDefault();
    onSubmit(draft.trim());
  }
  return (
    <form className="flex min-w-0 flex-1 flex-col gap-1" onSubmit={submit} role="search">
      <div className="flex gap-2">
        <Input
          className="font-mono text-[0.8125rem]"
          aria-label={label}
          aria-invalid={error ? true : undefined}
          placeholder={placeholder}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
        />
        <Button type="button" popoverTarget="query-help" aria-label="Search syntax">?</Button>
        <Button type="submit" variant="highlighted">Search</Button>
      </div>
      {error ? <p className="field-error">{error}</p> : null}
      <QueryHelp help={help} />
    </form>
  );
}

const HELP: [string, string][] = [
  ["timeout", "the message contains this text"],
  ['"connection reset"', "the message contains this phrase"],
  ["service:api", "a column: service, level, env, host, trace"],
  ["level:>=warn", "warn, error, and fatal"],
  ["http.route:/checkout", "an attribute"],
  ["-env:dev", "not"],
  ["a b", "both must match"],
];

function QueryHelp({ help }: { help: [string, string][] }) {
  return (
    <div id="query-help" popover="auto" className="fixed inset-0 m-auto h-fit w-[min(28rem,calc(100vw-2rem))] rounded-[10px] border border-[var(--ui-card-border)] bg-[var(--ui-panel)] p-4 shadow-[var(--ui-modal-shadow)]">
      <h2 className="mb-3 text-[0.875rem] font-medium text-[var(--ui-fg)]">Search syntax</h2>
      <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1.5 text-[0.8125rem]">
        {help.map(([example, meaning]) => (
          <div key={example} className="contents">
            <dt className="font-mono text-[var(--ui-fg)]">{example}</dt>
            <dd className="text-[var(--ui-fg-faint)]">{meaning}</dd>
          </div>
        ))}
      </dl>
      <p className="mt-3 text-[0.75rem] text-[var(--ui-fg-faint)]">Text search ignores case. At most 200 rows per page, newest first.</p>
    </div>
  );
}

export function RangePicker({
  range,
  error,
  options = LOG_RANGES,
  onChange,
}: {
  range: RangeSpec;
  error?: string;
  options?: RangeOptions;
  onChange: (range: RangeSpec) => void;
}) {
  const [custom, setCustom] = useState(range.kind === "custom");
  const now = Date.now();
  const initial = resolveRange(range, now, options);
  const [from, setFrom] = useState(toLocalInput(new Date(initial.from).toISOString()));
  const [to, setTo] = useState(toLocalInput(new Date(initial.to).toISOString()));
  const fromMs = new Date(from).getTime();
  const toMs = new Date(to).getTime();
  const invalid = customRangeError(fromMs, toMs, options);
  return (
    <div className="flex flex-col gap-1">
      <div className="flex flex-wrap items-center gap-2">
        <Select
          className="w-44"
          aria-label="Time range"
          value={custom ? "custom" : range.kind === "preset" ? range.preset : "custom"}
          options={[...options.presets.map((item) => ({ value: item.id, label: item.label })), { value: "custom", label: "Custom range" }]}
          onChange={(value) => {
            if (value === "custom") return setCustom(true);
            setCustom(false);
            onChange({ kind: "preset", preset: value });
          }}
        />
        {custom ? (
          <>
            <Input type="datetime-local" aria-label="From" className="w-48" value={from} onChange={(event) => setFrom(event.target.value)} />
            <Input type="datetime-local" aria-label="To" className="w-48" value={to} onChange={(event) => setTo(event.target.value)} />
            <Button disabled={invalid !== null} onClick={() => onChange({ kind: "custom", from: fromMs, to: toMs })}>Apply</Button>
          </>
        ) : null}
      </div>
      {custom && invalid ? <p className="field-error">{invalid}</p> : error ? <p className="field-error">{error}</p> : null}
    </div>
  );
}

function TailButton({ tail, disabled, onStart, onStop }: { tail: Tail; disabled: boolean; onStart: () => void; onStop: () => void }) {
  if (tail.status === "live") {
    return (
      <Button onClick={onStop} aria-pressed="true">
        <span className="size-2 animate-pulse rounded-full bg-[var(--ui-success)]" aria-hidden="true" />
        Live tail on
      </Button>
    );
  }
  return (
    <Button onClick={onStart} disabled={disabled} title={disabled ? "Live tail follows a relative range, such as the last 15 minutes." : "New logs appear at the top every 2 seconds."}>
      Live tail
    </Button>
  );
}

function FacetBar({ facets, q, onToggle }: { facets?: LogFacets; q: string; onToggle: (key: string, value: string) => void }) {
  if (!facets) return null;
  const levels = [...facets.levels].sort((a, b) => LOG_LEVELS.indexOf(a.level) - LOG_LEVELS.indexOf(b.level));
  return (
    <div className="flex flex-col gap-2">
      {levels.length > 0 ? (
        <FacetRow label="Levels">
          {levels.map(({ level, count }) => (
            <FacetButton key={level} active={hasFilter(q, "level", level)} onClick={() => onToggle("level", level)}>
              <StatusBadge tone={LEVEL_TONE[level]}>{level}</StatusBadge>
              <span className="tabular-nums text-[var(--ui-fg-faint)]">{count.toLocaleString()}</span>
            </FacetButton>
          ))}
        </FacetRow>
      ) : null}
      {facets.services.length > 0 ? (
        <FacetRow label="Services">
          {facets.services.slice(0, 10).map(({ service, count }) => (
            <FacetButton key={service} active={hasFilter(q, "service", service)} onClick={() => onToggle("service", service)}>
              <span className="max-w-48 truncate">{service || "(none)"}</span>
              <span className="tabular-nums text-[var(--ui-fg-faint)]">{count.toLocaleString()}</span>
            </FacetButton>
          ))}
        </FacetRow>
      ) : null}
      {facets.capped ? <p className="text-xs text-[var(--ui-fg-faint)]">Counts cover the newest 10,000 matching logs.</p> : null}
    </div>
  );
}

function FacetRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      <span className="w-16 text-[0.75rem] text-[var(--ui-fg-faint)]">{label}</span>
      {children}
    </div>
  );
}

export function FacetButton({ active, onClick, children }: { active: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      aria-pressed={active}
      className={cn(
        "inline-flex h-7 items-center gap-2 rounded-md border px-2 text-[0.75rem] transition-colors",
        active
          ? "border-[var(--ui-accent)] bg-[color-mix(in_oklab,var(--ui-accent)_14%,transparent)] text-[var(--ui-fg)]"
          : "border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] text-[var(--ui-fg-dim)] hover:bg-[var(--ui-hover)]",
      )}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

function LogPanel({ id, onClose }: { id: string; onClose: () => void }) {
  const log = useQuery({ queryKey: ["log", id], queryFn: () => api<LogDetail>(`/api/logs/${encodeURIComponent(id)}`) });
  const data = log.data;
  return (
    <section className="layer-card self-start xl:sticky xl:top-4" aria-label="Log detail">
      <header>
        <div className="min-w-0 py-0.5">
          <h2>Log</h2>
          {data ? <p>{logTime(data.timeMs)}</p> : null}
        </div>
        <button type="button" className="icon-button" aria-label="Close log" onClick={onClose}>
          <X className="size-4" />
        </button>
      </header>
      <div className="layer-card-body flex flex-col gap-4">
        {log.isLoading ? <Skeleton className="h-32 w-full" /> : null}
        <ErrorText error={log.error} />
        {data ? (
          <>
            <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-all rounded-lg border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] p-3 font-mono text-[0.75rem] text-[var(--ui-fg)]">
              {data.body}
            </pre>
            <KeyValues
              entries={[
                ["Level", data.severityText ? `${data.level} (${data.severityText})` : data.level],
                ["Service", data.service],
                ["Environment", data.environment],
                ["Host", data.host],
                ["Source", data.sourceName],
                ["Trace id", data.traceId ?? ""],
                ["Span id", data.spanId ?? ""],
                ["Log id", data.id],
              ]}
            />
            {data.traceId ? (
              <Link className="button self-start" to={`/traces/${encodeURIComponent(data.traceId)}`}>
                View trace
              </Link>
            ) : null}
            <Section title="Attributes" values={data.attributes} />
            <Section title="Resource" values={data.resource} />
          </>
        ) : null}
      </div>
    </section>
  );
}

export function Section({ title, values }: { title: string; values: Record<string, unknown> }) {
  const entries = Object.entries(values).map(([key, value]): [string, string] => [key, typeof value === "string" ? value : JSON.stringify(value)]);
  return (
    <div>
      <h3 className="mb-1.5 text-[0.75rem] font-medium text-[var(--ui-fg-dim)]">{title}</h3>
      {entries.length === 0 ? <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">None.</p> : <KeyValues entries={entries} />}
    </div>
  );
}

export function KeyValues({ entries }: { entries: [string, string][] }) {
  return (
    <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-[0.8125rem]">
      {entries
        .filter(([, value]) => value !== "")
        .map(([key, value]) => (
          <div key={key} className="contents">
            <dt className="text-[var(--ui-fg-faint)]">{key}</dt>
            <dd className="break-all font-mono text-[0.75rem] text-[var(--ui-fg)]">{value}</dd>
          </div>
        ))}
    </dl>
  );
}

export function logTime(ms: number) {
  return new Date(ms).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    fractionalSecondDigits: 3,
    hour12: false,
  });
}
