import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDown } from "reicon-react";
import { useEffect, useState, type ReactNode } from "react";
import { flushSync } from "react-dom";
import { Link, useLocation, useNavigate, useParams, useSearchParams } from "react-router";
import { Histogram } from "@/components/error-charts";
import { StackFrames } from "@/components/event-view";
import { Badge } from "@/components/issue-badge";
import { Button, ErrorText, Input, LayerCard, PageHeader, Select, useCopy } from "@/components/ui";
import { api, type ExceptionInfo, type Issue, type IssueDetail, type NormalizedEvent } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { cn, fullTime, oneOf, splitTitle, timeAgo } from "@/lib/utils";

/** Router state the issue list passes, so the breadcrumb returns to the list the user came from. */
export type IssueLocationState = { listSearch: string };

const ranges = ["24h", "7d", "14d", "30d"] as const;
const tabs = [
  { id: "event", label: "Event" },
  { id: "events", label: "Events" },
] as const;

/** Filters and the tab live in the URL. The open event lives in the path. */
type View = {
  range: (typeof ranges)[number];
  environment: string;
  q: string;
  tab: (typeof tabs)[number]["id"];
};

const defaults: View = { range: "14d", environment: "", q: "", tab: "event" };

function parseView(params: URLSearchParams): View {
  return {
    range: oneOf(params.get("range"), ranges, defaults.range),
    environment: params.get("environment") ?? defaults.environment,
    q: params.get("q") ?? defaults.q,
    tab: oneOf(params.get("tab"), tabs.map((item) => item.id), defaults.tab),
  };
}

function viewSearch(view: View) {
  const search = new URLSearchParams();
  for (const key of Object.keys(defaults) as (keyof View)[]) {
    if (view[key] !== defaults[key]) search.set(key, view[key]);
  }
  return search;
}

type StatusAction = { status: Issue["status"]; label: string; pending: string; highlighted?: boolean };
const resolve: StatusAction = { status: "resolved", label: "Resolve", pending: "Resolving...", highlighted: true };
const archive: StatusAction = { status: "ignored", label: "Archive", pending: "Archiving..." };
const statusActions: Record<Issue["status"], StatusAction[]> = {
  unresolved: [resolve, archive],
  resolved: [{ status: "unresolved", label: "Reopen", pending: "Reopening..." }, archive],
  ignored: [resolve, { status: "unresolved", label: "Unarchive", pending: "Unarchiving..." }],
};

const sections = [
  { key: "stack", label: "Stack trace" },
  { key: "highlights", label: "Highlights" },
  { key: "breadcrumbs", label: "Breadcrumbs" },
  { key: "tags", label: "Tags" },
  { key: "context", label: "Context" },
  { key: "raw", label: "Raw JSON" },
] as const;
type SectionKey = (typeof sections)[number]["key"];

export function IssueDetailPage() {
  const { id = "", eventId } = useParams();
  const navigate = useNavigate();
  const location = useLocation();
  const canEdit = useCanEdit();
  const client = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const view = parseView(searchParams);
  const q = useDebounced(view.q, 300);
  const [inAppOnly, setInAppOnly] = useState(true);
  const [open, setOpen] = useState<Record<SectionKey, boolean>>({
    stack: true,
    highlights: true,
    breadcrumbs: true,
    tags: true,
    context: true,
    raw: false,
  });
  const listSearch = (location.state as IssueLocationState | null)?.listSearch;

  const issue = useQuery({
    queryKey: ["issue", id, view.range, view.environment, q],
    queryFn: () =>
      api<IssueDetail>(
        `/api/error-issues/${encodeURIComponent(id)}?${new URLSearchParams({ range: view.range, environment: view.environment, q })}`,
      ),
    placeholderData: keepPreviousData,
  });
  const latest = issue.data?.latest ?? null;
  const pickedOther = Boolean(eventId) && eventId !== latest?.eventId;
  const picked = useQuery({
    queryKey: ["event", eventId],
    queryFn: () => api<NormalizedEvent>(`/api/error-events/${eventId}`),
    enabled: pickedOther,
    placeholderData: keepPreviousData,
  });
  const update = useMutation({
    mutationFn: (status: Issue["status"]) =>
      api(`/api/error-issues/${issue.data?.id ?? id}`, { method: "PATCH", body: JSON.stringify({ status }) }),
    onSuccess: () =>
      Promise.all([client.invalidateQueries({ queryKey: ["issue", id] }), client.invalidateQueries({ queryKey: ["issues"] })]),
  });

  if (issue.isLoading) return <p className="text-[13px] text-[var(--ui-fg-faint)]">Loading issue...</p>;
  if (issue.isError || !issue.data) {
    return <p className="text-[13px] text-[var(--ui-error)]">{(issue.error as Error)?.message || "Issue not found"}</p>;
  }
  const data = issue.data;
  const event = pickedOther ? picked.data : latest;
  const events = data.events;
  const index = events.findIndex((item) => item.id === event?.eventId);
  const parts = splitTitle(data.title);
  const issueKey = data.shortId || data.id;
  const rangeLabel = `last ${view.range}`;

  // Every navigation carries the router state forward, or the breadcrumb forgets the list filters.
  function setView(patch: Partial<View>, replace = false) {
    setSearchParams(viewSearch({ ...view, ...patch }), { replace, state: location.state });
  }
  function openEvent(nextId: string) {
    navigate(
      { pathname: `/errors/${issueKey}/events/${nextId}`, search: viewSearch({ ...view, tab: "event" }).toString() },
      { state: location.state },
    );
  }
  function reveal(key: SectionKey) {
    flushSync(() => setOpen((state) => ({ ...state, [key]: true })));
    document.getElementById(`event-${key}`)?.scrollIntoView({ behavior: "smooth", block: "start" });
  }
  const toggle = (key: SectionKey) => () => setOpen((state) => ({ ...state, [key]: !state[key] }));
  const jumps: { label: string; target?: string | null }[] = [
    { label: "First", target: data.firstEventId },
    { label: "Older", target: index >= 0 ? events[index + 1]?.id : undefined },
    { label: "Newer", target: index > 0 ? events[index - 1]?.id : undefined },
    { label: "Latest", target: data.latestEventId },
  ];

  return (
    <div className="flex flex-col gap-4">
      <div>
        <nav className="mb-2 flex flex-wrap items-center gap-2 text-[13px] text-[var(--ui-fg-faint)]">
          <Link to={listSearch ? `/errors?${listSearch}` : "/errors"} className="hover:text-[var(--ui-fg)]">
            Issues
          </Link>
          <span>/</span>
          <span className="font-mono text-[var(--ui-fg-dim)]">{data.shortId}</span>
          <CopyButton quiet text={data.shortId} label="Copy ID" />
          <CopyButton quiet text={`${window.location.origin}/errors/${issueKey}`} label="Copy link" />
        </nav>
        <PageHeader
          title={parts.type}
          keepTitle
          description={parts.message || undefined}
          actions={
            canEdit
              ? statusActions[data.status].map((action) => (
                  <Button
                    key={action.status}
                    variant={action.highlighted ? "highlighted" : "default"}
                    disabled={update.isPending}
                    onClick={() => update.mutate(action.status)}
                  >
                    {update.isPending && update.variables === action.status ? action.pending : action.label}
                  </Button>
                ))
              : null
          }
        />
        <div className="-mt-3 flex flex-wrap items-center gap-2 text-[12px] text-[var(--ui-fg-faint)]">
          <Badge tone={data.level}>{data.level}</Badge>
          <Badge tone={data.status}>{data.status === "ignored" ? "Archived" : data.status}</Badge>
          {data.unhandled ? <Badge tone="fatal">Unhandled</Badge> : null}
          {data.culprit ? <span className="break-all font-mono text-[var(--ui-fg-dim)]">{data.culprit}</span> : null}
        </div>
        <ErrorText error={update.error} />
      </div>

      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-[var(--ui-divider)]">
        <nav className="-mb-px flex gap-1" aria-label="Issue view">
          {tabs.map((item) => (
            <button
              key={item.id}
              type="button"
              aria-current={item.id === view.tab ? "page" : undefined}
              className={cn(
                "h-10 shrink-0 border-b-2 px-2.5 text-[13px] font-medium transition-colors",
                item.id === view.tab
                  ? "border-[var(--ui-accent)] text-[var(--ui-fg)]"
                  : "border-transparent text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]",
              )}
              onClick={() => setView({ tab: item.id })}
            >
              {item.label}
              {item.id === "events" ? (
                <span className="ml-1.5 font-normal tabular-nums text-[var(--ui-fg-faint)]">{data.periodEventCount.toLocaleString()}</span>
              ) : null}
            </button>
          ))}
        </nav>
        <div className="flex flex-wrap items-center gap-2 pb-2">
          <div className="w-48">
            <Input
              placeholder="Search events"
              aria-label="Search events"
              value={view.q}
              onChange={(input) => setView({ q: input.target.value }, true)}
            />
          </div>
          <div className="w-40">
            <Select
              aria-label="Environment"
              value={view.environment}
              onChange={(environment) => setView({ environment })}
              options={[{ value: "", label: "All environments" }, ...data.environments.map((item) => ({ value: item, label: item }))]}
            />
          </div>
          <div className="w-28">
            <Select
              aria-label="Time range"
              value={view.range}
              onChange={(range) => setView({ range })}
              options={ranges.map((item) => ({ value: item, label: `Last ${item}` }))}
            />
          </div>
        </div>
      </div>

      <div className="grid min-w-0 gap-6 xl:grid-cols-[minmax(0,1fr)_300px]">
        <div className="flex min-w-0 flex-col gap-4">
          {view.tab === "events" ? (
            <LayerCard
              title="Events"
              description={`Latest ${events.length} of ${data.periodEventCount} in the ${rangeLabel}`}
              flush
            >
              {events.length === 0 ? (
                <p className="p-6 text-center text-[13px] text-[var(--ui-fg-faint)]">No events match these filters.</p>
              ) : (
                <div className="overflow-x-auto">
                  <table className="cool-table">
                    <thead>
                      <tr>
                        <th>Event</th>
                        <th>Time</th>
                        <th>Environment</th>
                        <th>Release</th>
                        <th>User</th>
                      </tr>
                    </thead>
                    <tbody>
                      {events.map((row) => (
                        <tr
                          key={row.id}
                          className="clickable-row"
                          aria-selected={row.id === event?.eventId}
                          onClick={() => openEvent(row.id)}
                        >
                          <td className="font-mono text-[12px]">{row.id.slice(0, 8)}</td>
                          <td className="whitespace-nowrap" title={fullTime(row.timestamp)}>
                            {timeAgo(row.timestamp)}
                          </td>
                          <td>{row.environment || "-"}</td>
                          <td className="break-all">{row.release || "-"}</td>
                          <td className="break-all">{row.user || "-"}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </LayerCard>
          ) : (
            <>
              <LayerCard
                title={event ? `Event ${event.eventId.slice(0, 8)}` : "Event"}
                actions={
                  <>
                    {jumps.map((jump) => (
                      <Button
                        key={jump.label}
                        size="sm"
                        disabled={!jump.target || jump.target === event?.eventId}
                        onClick={() => jump.target && openEvent(jump.target)}
                      >
                        {jump.label}
                      </Button>
                    ))}
                    <Button size="sm" disabled={!event} onClick={() => reveal("raw")}>
                      JSON
                    </Button>
                  </>
                }
              >
                {event ? (
                  <div className="flex flex-col gap-3">
                    <EventChips event={event} />
                    <nav className="flex flex-wrap gap-4 text-[12px] font-medium text-[var(--ui-fg-faint)]" aria-label="Event sections">
                      {sections.map((section) => (
                        <button key={section.key} type="button" className="hover:text-[var(--ui-fg)]" onClick={() => reveal(section.key)}>
                          {section.label}
                        </button>
                      ))}
                    </nav>
                  </div>
                ) : pickedOther && picked.isError ? (
                  <ErrorText error={picked.error} />
                ) : (
                  <p className="py-4 text-center text-[13px] text-[var(--ui-fg-faint)]">
                    {pickedOther ? "Loading event..." : "No events match these filters."}
                  </p>
                )}
              </LayerCard>
              {event ? (
                <EventSections
                  event={event}
                  open={open}
                  toggle={toggle}
                  inAppOnly={inAppOnly}
                  onInAppOnly={setInAppOnly}
                />
              ) : null}
            </>
          )}
        </div>

        <aside className="flex flex-col gap-4 xl:sticky xl:top-[calc(3rem+1.75rem)] xl:self-start">
          <LayerCard
            title={`${data.periodEventCount.toLocaleString()} ${data.periodEventCount === 1 ? "event" : "events"}`}
            description={`In the ${rangeLabel}`}>
            <Histogram values={data.histogram} />
          </LayerCard>
          <LayerCard title="Details">
            <dl className="grid grid-cols-2 gap-3 text-[13px]">
              <Detail label="Last seen">
                <span title={fullTime(data.lastSeen)}>{timeAgo(data.lastSeen)}</span>
              </Detail>
              <Detail label="First seen">
                <span title={fullTime(data.firstSeen)}>{timeAgo(data.firstSeen)}</span>
              </Detail>
              <Detail label="Events, all time">{data.eventCount.toLocaleString()}</Detail>
              <Detail label={`Users, ${rangeLabel}`}>{data.userCount.toLocaleString()}</Detail>
              <Detail label="Environment">{data.environment || "-"}</Detail>
              <Detail label="Release">{data.release || "-"}</Detail>
              <Detail label="App">
                <Link className="text-[var(--ui-accent-light)]" to={`/errors/apps/${data.appId}`}>
                  {data.appName}
                </Link>
              </Detail>
            </dl>
          </LayerCard>
          <LayerCard title="Tags" description={`Share of events in the ${rangeLabel}`}>
            {data.tags.length === 0 ? (
              <p className="text-center text-[13px] text-[var(--ui-fg-faint)]">No tags in this range.</p>
            ) : (
              <div className="flex flex-col gap-4">
                {data.tags.map((group) => (
                  <div key={group.key}>
                    <div className="mb-1.5 text-[12px] font-medium text-[var(--ui-fg)]">{group.key}</div>
                    <ul className="flex flex-col gap-2">
                      {group.values.map((tag) => {
                        const share = (tag.count / Math.max(data.periodEventCount, 1)) * 100;
                        return (
                          <li key={tag.value} title={`${tag.count} of ${data.periodEventCount} events`}>
                            <div className="flex justify-between gap-2 text-[12px] text-[var(--ui-fg-dim)]">
                              <span className="truncate">{tag.value}</span>
                              <span className="tabular-nums">{Math.round(share)}%</span>
                            </div>
                            <div className="mt-1 h-1 rounded-full bg-[var(--ui-hover)]">
                              <div className="h-1 rounded-full bg-[var(--ui-accent)]" style={{ width: `${share}%` }} />
                            </div>
                          </li>
                        );
                      })}
                    </ul>
                  </div>
                ))}
              </div>
            )}
          </LayerCard>
        </aside>
      </div>
    </div>
  );
}

function EventSections({
  event,
  open,
  toggle,
  inAppOnly,
  onInAppOnly,
}: {
  event: NormalizedEvent;
  open: Record<SectionKey, boolean>;
  toggle: (key: SectionKey) => () => void;
  inAppOnly: boolean;
  onInAppOnly: (value: boolean) => void;
}) {
  return (
    <>
      <Section
        id="stack"
        title="Stack trace"
        open={open.stack}
        onToggle={toggle("stack")}
        flush
        actions={
          event.exceptions.length > 0 ? (
            <>
              <label className="flex items-center gap-1.5 text-[12px] text-[var(--ui-fg-faint)]">
                <input type="checkbox" checked={inAppOnly} onChange={(input) => onInAppOnly(input.target.checked)} />
                In-app only
              </label>
              <CopyButton text={stackText(event.exceptions)} label="Copy as text" />
            </>
          ) : null
        }
      >
        {event.exceptions.length === 0 ? (
          <p className="p-4 text-[13px] text-[var(--ui-fg-faint)]">{event.message || "No exception payload."}</p>
        ) : (
          event.exceptions.map((exception, exceptionIndex) => (
            <div key={`${exception.type}-${exceptionIndex}`} className={exceptionIndex > 0 ? "border-t border-[var(--ui-divider)]" : undefined}>
              <div className="flex flex-wrap items-center gap-2 px-4 py-3 text-[13px]">
                <span className="font-medium text-[var(--ui-fg)]">{exception.type || "Error"}</span>
                {exception.value ? <span className="text-[var(--ui-fg-dim)]">{exception.value}</span> : null}
                {exception.handled === false ? <Badge tone="fatal">Unhandled</Badge> : null}
                {exception.handled === true ? <Badge tone="info">Handled</Badge> : null}
              </div>
              <StackFrames frames={exception.frames} inAppOnly={inAppOnly && exception.frames.some((frame) => frame.inApp)} />
            </div>
          ))
        )}
      </Section>
      <Section id="highlights" title="Highlights" open={open.highlights} onToggle={toggle("highlights")}>
        <Highlights event={event} />
      </Section>
      <Section id="breadcrumbs" title="Breadcrumbs" open={open.breadcrumbs} onToggle={toggle("breadcrumbs")}>
        {event.breadcrumbs.length === 0 ? (
          <p className="text-center text-[13px] text-[var(--ui-fg-faint)]">No breadcrumbs on this event.</p>
        ) : (
          <ol className="flex flex-col gap-3">
            {event.breadcrumbs.map((crumb, crumbIndex) => (
              <li key={crumbIndex} className="border-l-2 border-[var(--ui-divider)] pl-3 text-[13px] text-[var(--ui-fg)]">
                <div className="text-[12px] text-[var(--ui-fg-faint)]">
                  {crumb.category || crumb.type || "default"}
                  {crumb.timestamp ? ` · ${fullTime(crumb.timestamp)}` : ""}
                </div>
                <div>{crumb.message}</div>
              </li>
            ))}
          </ol>
        )}
      </Section>
      <Section id="tags" title="Tags" open={open.tags} onToggle={toggle("tags")}>
        {event.tags.length === 0 ? (
          <p className="text-center text-[13px] text-[var(--ui-fg-faint)]">No tags.</p>
        ) : (
          <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1.5 text-[13px]">
            {event.tags.map((tag) => (
              <div key={tag.key} className="contents">
                <dt className="text-[var(--ui-fg-faint)]">{tag.key}</dt>
                <dd className="break-all text-[var(--ui-fg)]">{tag.value}</dd>
              </div>
            ))}
          </dl>
        )}
      </Section>
      <Section id="context" title="Context" open={open.context} onToggle={toggle("context")}>
        <ContextBlock event={event} />
      </Section>
      <Section
        id="raw"
        title="Raw JSON"
        open={open.raw}
        onToggle={toggle("raw")}
        actions={<CopyButton text={JSON.stringify(event.raw, null, 2)} label="Copy" />}
      >
        <pre className="max-h-96 overflow-auto rounded-lg border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] p-3 font-mono text-[12px] leading-5 text-[var(--ui-fg-dim)]">
          {JSON.stringify(event.raw, null, 2)}
        </pre>
      </Section>
    </>
  );
}

function CopyButton({ text, label, quiet }: { text: string; label: string; quiet?: boolean }) {
  const { copied, copy } = useCopy();
  if (quiet) {
    return (
      <button type="button" className="text-[12px] text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]" onClick={() => copy(text)}>
        {copied ? "Copied" : label}
      </button>
    );
  }
  return (
    <Button size="sm" onClick={() => copy(text)}>
      {copied ? "Copied" : label}
    </Button>
  );
}

function useDebounced<T>(value: T, ms: number) {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  return debounced;
}

function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="text-[12px] text-[var(--ui-fg-faint)]">{label}</dt>
      <dd className="mt-0.5 break-all text-[var(--ui-fg)]">{children}</dd>
    </div>
  );
}

function EventChips({ event }: { event: NormalizedEvent }) {
  const runtime = record(event.contexts.runtime);
  const os = record(event.contexts.os);
  const user = stringField(event.user, "email") || stringField(event.user, "username") || stringField(event.user, "id");
  const chips = [
    user,
    runtime.name ? `${runtime.name} ${runtime.version ?? ""}`.trim() : "",
    os.name ? `${os.name} ${os.version ?? ""}`.trim() : "",
    event.release || "",
    event.environment || "",
  ].filter(Boolean);
  return (
    <div className="flex flex-wrap items-center gap-2">
      <span className="text-[12px] text-[var(--ui-fg-dim)]" title={fullTime(event.timestamp)}>
        {timeAgo(event.timestamp)}
      </span>
      {chips.map((chip) => (
        <span
          key={chip}
          className="inline-flex h-6 items-center rounded-full border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] px-2 text-[12px] text-[var(--ui-fg-dim)]"
        >
          {chip}
        </span>
      ))}
    </div>
  );
}

function Highlights({ event }: { event: NormalizedEvent }) {
  const rows: [string, string][] = [
    ["Handled", event.exceptions.length === 0 ? "" : event.exceptions.some((item) => item.handled === false) ? "no" : "yes"],
    ["Level", event.level],
    ["Transaction", event.transaction || ""],
    ["URL", stringField(event.request, "url")],
    ["Environment", event.environment || ""],
    ["Release", event.release || ""],
  ].filter((entry): entry is [string, string] => Boolean(entry[1]));
  return (
    <dl className="grid gap-3 sm:grid-cols-2">
      {rows.map(([key, value]) => (
        <div key={key}>
          <dt className="text-[12px] text-[var(--ui-fg-faint)]">{key}</dt>
          <dd className="mt-0.5 break-all text-[13px] text-[var(--ui-fg)]">{value}</dd>
        </div>
      ))}
    </dl>
  );
}

function ContextBlock({ event }: { event: NormalizedEvent }) {
  const names = Object.keys(event.contexts).filter((name) => name !== "trace");
  if (names.length === 0) return <p className="text-center text-[13px] text-[var(--ui-fg-faint)]">No context.</p>;
  return (
    <div className="flex flex-col gap-4">
      {names.map((name) => (
        <div key={name}>
          <div className="mb-1.5 text-[12px] font-medium text-[var(--ui-fg-dim)] first-letter:uppercase">{name}</div>
          <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-[13px]">
            {Object.entries(record(event.contexts[name])).map(([key, value]) => (
              <div key={key} className="contents">
                <dt className="text-[var(--ui-fg-faint)]">{key}</dt>
                <dd className="break-all text-[var(--ui-fg)]">{String(value)}</dd>
              </div>
            ))}
          </dl>
        </div>
      ))}
    </div>
  );
}

/* Collapsible layer card: same anatomy as LayerCard, the well hides when closed. */
function Section({
  id,
  title,
  open,
  onToggle,
  actions,
  flush,
  children,
}: {
  id: SectionKey;
  title: string;
  open: boolean;
  onToggle: () => void;
  actions?: ReactNode;
  flush?: boolean;
  children: ReactNode;
}) {
  return (
    <section id={`event-${id}`} className="layer-card scroll-mt-20">
      <header className={open ? undefined : "!pb-1"}>
        <button type="button" className="flex items-center gap-1.5 py-0.5 text-left" onClick={onToggle} aria-expanded={open}>
          <ChevronDown className={cn("size-3.5 text-[var(--ui-fg-faint)] transition-transform", !open && "-rotate-90")} aria-hidden="true" />
          <h2>{title}</h2>
        </button>
        {open && actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
      </header>
      {open ? <div className={cn("layer-card-body", flush && "is-flush")}>{children}</div> : null}
    </section>
  );
}

function stackText(exceptions: ExceptionInfo[]) {
  return exceptions
    .map((exception) => {
      const head = `${exception.type || "Error"}: ${exception.value || ""}`;
      const frames = [...exception.frames].reverse().map((frame) => {
        const where = frame.filename || frame.absPath || frame.module || "unknown";
        return `    at ${frame.function || "<anonymous>"} (${where}:${frame.lineno ?? "?"})`;
      });
      return [head, ...frames].join("\n");
    })
    .join("\n\n");
}

function record(value: unknown): Record<string, string> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>)
      .filter(([, item]) => item != null && typeof item !== "object")
      .map(([key, item]) => [key, String(item)]),
  );
}

function stringField(value: Record<string, unknown> | undefined, key: string) {
  const item = value?.[key];
  return typeof item === "string" ? item : "";
}
