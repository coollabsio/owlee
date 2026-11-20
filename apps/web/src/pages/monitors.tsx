import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, type ReactNode, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { Search4 as Search } from "reicon-react";
import { LatencyChart, UptimeBars } from "@/components/charts";
import { Button, ConfirmDialog, Dialog, Empty, ErrorText, Field, FilterChips, FilterValue, Input, LayerCard, PageHeader, Select, Skeleton, SkeletonRows, SortHeader, Spinner, StatusBadge, Textarea, useSort } from "@/components/ui";
import { ProjectField, ProjectTag } from "@/components/project";
import { ApiError, type Agent, api, type Assertion, type CheckOutcome, type Monitor, type MonitorDetail } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { hostOf, inferType, type JobType, normalizeTarget } from "@/lib/monitor-target";
import { useListScope, useProjectChoice } from "@/lib/workspace";
import { fullTime, percent, providerLabel, stateLabel, timeAgo } from "@/lib/utils";

type Draft = {
  name: string;
  /** Until the user types a name, it follows the host. */
  name_edited: boolean;
  description: string;
  job_type: JobType;
  /** Until the user picks a type, it follows the URL. */
  type_edited: boolean;
  url: string;
  method: string;
  interval_seconds: string;
  timeout_seconds: string;
  expected_status: string;
  degraded_after_ms: string;
  failure_threshold: string;
  body_contains: string;
  active: boolean;
};

const emptyDraft: Draft = {
  name: "",
  name_edited: false,
  description: "",
  job_type: "http",
  type_edited: false,
  url: "",
  method: "GET",
  interval_seconds: "60",
  timeout_seconds: "30",
  expected_status: "",
  degraded_after_ms: "",
  failure_threshold: "3",
  body_contains: "",
  active: true,
};

function draftFrom(monitor: Monitor): Draft {
  const body = monitor.assertions.find((assertion) => assertion.type === "body" && assertion.compare === "contains");
  return {
    name: monitor.name,
    name_edited: true,
    description: monitor.description,
    job_type: monitor.job_type,
    type_edited: true,
    url: monitor.url,
    method: monitor.method,
    interval_seconds: String(monitor.interval_seconds),
    timeout_seconds: String(Math.round(monitor.timeout_ms / 1000)),
    expected_status: monitor.expected_status == null ? "" : String(monitor.expected_status),
    degraded_after_ms: monitor.degraded_after_ms == null ? "" : String(monitor.degraded_after_ms),
    failure_threshold: String(monitor.failure_threshold),
    body_contains: typeof body?.target === "string" ? body.target : "",
    active: monitor.active,
  };
}

/** What the draft checks once the type, target, and name are filled in from the URL. */
function resolved(draft: Draft) {
  const job_type = draft.type_edited ? draft.job_type : inferType(draft.url);
  const url = normalizeTarget(job_type, draft.url);
  return { job_type, url, name: draft.name_edited ? draft.name : hostOf(job_type, url) };
}

function payload(draft: Draft) {
  const { job_type, url, name } = resolved(draft);
  const assertions: Assertion[] = [];
  if (job_type === "http" && draft.body_contains.trim()) {
    assertions.push({ type: "body", compare: "contains", target: draft.body_contains.trim() });
  }
  return {
    name,
    description: draft.description,
    job_type,
    url,
    method: draft.method,
    interval_seconds: Number(draft.interval_seconds),
    timeout_ms: Number(draft.timeout_seconds) * 1000,
    expected_status: draft.expected_status ? Number(draft.expected_status) : null,
    degraded_after_ms: draft.degraded_after_ms ? Number(draft.degraded_after_ms) : null,
    failure_threshold: Number(draft.failure_threshold),
    assertions,
    follow_redirects: true,
    active: draft.active,
    headers: [],
    body: "",
  };
}

function rowCover(to: string) {
  return <Link className="cell-link-anchor" to={to} tabIndex={-1} aria-hidden="true" />;
}

type ShownState = Monitor["state"] | "paused";
type Tone = "neutral" | "success" | "warning" | "error";

/** A paused monitor keeps its last state, but the list shows it as paused. */
function shownState(monitor: Monitor): ShownState {
  return monitor.active ? monitor.state : "paused";
}

const stateRank: Record<ShownState, number> = { down: 0, degraded: 1, pending: 2, up: 3, paused: 4 };
const stateTone: Record<ShownState, Tone> = { down: "error", degraded: "warning", pending: "neutral", up: "success", paused: "neutral" };
const checkTone: Record<string, Tone | "info"> = { success: "success", failure: "error", degraded: "warning" };

function Ago({ at }: { at: string | null }) {
  return at ? <time dateTime={at} title={fullTime(at)}>{timeAgo(at)}</time> : <>never</>;
}

function epoch(value: string | null) {
  return value ? Date.parse(value) : null;
}

function MonitorRow({ monitor, subtitle, stateFilter, onStateFilter }: { monitor: Monitor; subtitle: string; stateFilter: ShownState | null; onStateFilter: (state: ShownState) => void }) {
  const to = `/monitors/${monitor.id}`;
  const state = shownState(monitor);
  return (
    <tr className="clickable-row">
      <td className="cell-link">
        <Link className="row-link" to={to}>{monitor.name}</Link>
        <ProjectTag id={monitor.project_id} />
        {rowCover(to)}
        <div className="truncate text-[0.6875rem] text-[var(--ui-fg-faint)]">{subtitle}</div>
      </td>
      <td className="cell-link">
        {rowCover(to)}
        <FilterValue active={stateFilter === state} label={stateLabel[state]} onToggle={() => onStateFilter(state)}>
          <StatusBadge tone={stateTone[state]}>{stateLabel[state]}</StatusBadge>
        </FilterValue>
      </td>
      <td className="cell-link hidden md:table-cell">
        {rowCover(to)}
        {percent(monitor.uptime_24h)}
      </td>
      <td className="cell-link hidden sm:table-cell">
        {rowCover(to)}
        {percent(monitor.uptime_30d)}
      </td>
      <td className="cell-link whitespace-nowrap">
        {rowCover(to)}
        <Ago at={monitor.last_checked_at} />
      </td>
    </tr>
  );
}

export function MonitorsPage() {
  const scope = useListScope();
  const query = useQuery({ queryKey: scope.key("monitors"), queryFn: () => api<Monitor[]>(scope.path("/api/monitors")), refetchInterval: 10_000 });
  const label = useMonitorLabel();
  const canEdit = useCanEdit();
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [stateFilter, setStateFilter] = useState<ShownState | null>(null);
  const needle = search.trim().toLowerCase();
  const filtered = (query.data ?? [])
    .filter(
      (monitor) =>
        (!needle || monitor.name.toLowerCase().includes(needle) || monitor.url.toLowerCase().includes(needle)) &&
        (!stateFilter || shownState(monitor) === stateFilter),
    )
    .sort((a, b) => Number(b.active) - Number(a.active));
  const { rows, sort, toggle } = useSort(filtered, {
    name: (monitor) => monitor.name,
    state: (monitor) => stateRank[shownState(monitor)],
    today: (monitor) => monitor.uptime_24h,
    month: (monitor) => monitor.uptime_30d,
    checked: (monitor) => epoch(monitor.last_checked_at),
  });
  const filters = stateFilter ? [{ key: "state", label: `State: ${stateLabel[stateFilter]}`, onRemove: () => setStateFilter(null) }] : [];
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Monitors"
        description="HTTP and TCP checks from this server. Agents add checks from their own config files."
        actions={canEdit ? <Button variant="primary" onClick={() => setOpen(true)}>New monitor</Button> : null}
      />
      {(query.data?.length ?? 0) === 0 && !query.isLoading ? (
        <Empty title="No monitors" body="Paste a URL or a host:port. Owlee checks it as soon as you save." action={canEdit ? <Button variant="primary" onClick={() => setOpen(true)}>New monitor</Button> : undefined} />
      ) : (
        <LayerCard title="All monitors" description={query.data ? `${query.data.length} total` : undefined} flush>
          <div className="flex flex-wrap items-center gap-2 border-b border-[var(--ui-divider)] p-2">
            <div className="relative w-full sm:w-72">
              <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-[var(--ui-fg-faint)]" aria-hidden="true" />
              <Input style={{ paddingLeft: 36 }} placeholder="Search by name or URL" value={search} onChange={(event) => setSearch(event.target.value)} aria-label="Search monitors" />
            </div>
            <FilterChips filters={filters} onClear={() => setStateFilter(null)} />
          </div>
          {!query.isLoading && rows.length === 0 ? (
            <CardText title="No matching monitors" body="Try a different name or URL, or remove a filter." />
          ) : (
            <div className="overflow-x-auto">
              <table className="cool-table">
                <thead>
                  <tr>
                    <SortHeader column="name" sort={sort} onSort={toggle}>Name</SortHeader>
                    <SortHeader column="state" sort={sort} onSort={toggle}>State</SortHeader>
                    <SortHeader column="today" sort={sort} onSort={toggle} className="hidden md:table-cell">Today</SortHeader>
                    <SortHeader column="month" sort={sort} onSort={toggle} className="hidden sm:table-cell">30 days</SortHeader>
                    <SortHeader column="checked" sort={sort} onSort={toggle}>Last check</SortHeader>
                  </tr>
                </thead>
                <tbody>
                  {query.isLoading ? (
                    <SkeletonRows columns={[undefined, undefined, "hidden md:table-cell", "hidden sm:table-cell", undefined]} />
                  ) : (
                    rows.map((monitor) => (
                      <MonitorRow
                        key={monitor.id}
                        monitor={monitor}
                        subtitle={label(monitor)}
                        stateFilter={stateFilter}
                        onStateFilter={(state) => setStateFilter(stateFilter === state ? null : state)}
                      />
                    ))
                  )}
                </tbody>
              </table>
            </div>
          )}
        </LayerCard>
      )}
      {canEdit ? <MonitorDialog open={open} onClose={() => setOpen(false)} /> : null}
    </div>
  );
}

export function MonitorDetailPage() {
  const { id = "" } = useParams();
  const navigate = useNavigate();
  const canEdit = useCanEdit();
  const client = useQueryClient();
  const [editing, setEditing] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [checksPage, setChecksPage] = useState(0);
  const query = useQuery({
    queryKey: ["monitor", id],
    queryFn: () => api<MonitorDetail>(`/api/monitors/${id}`),
    refetchInterval: 10_000,
  });
  const label = useMonitorLabel();
  const run = useMutation({
    mutationFn: () => api<MonitorDetail>(`/api/monitors/${id}/checks`, { method: "POST" }),
    onSuccess: (monitor) => client.setQueryData(["monitor", id], monitor),
  });
  const pause = useMutation({
    mutationFn: (active: boolean) =>
      api<MonitorDetail>(`/api/monitors/${id}/active`, { method: "POST", body: JSON.stringify({ active }) }),
    onSuccess: (monitor) => client.setQueryData(["monitor", id], monitor),
  });
  const remove = useMutation({
    mutationFn: () => api(`/api/monitors/${id}`, { method: "DELETE" }),
    onSuccess: () => navigate("/monitors"),
  });
  const monitor = query.data;
  if (query.isLoading) {
    return (
      <div className="flex flex-col gap-6" aria-busy="true">
        <div className="space-y-2">
          <Skeleton className="h-6 w-48" />
          <Skeleton className="h-3 w-72" />
        </div>
        <LayerCard><Skeleton className="h-24 w-full" /></LayerCard>
        <LayerCard><Skeleton className="h-40 w-full" /></LayerCard>
      </div>
    );
  }
  if (!monitor) return <p className="field-error">{(query.error as Error | null)?.message ?? "Monitor not found."}</p>;
  const checks = monitor.recent ?? [];
  const pages = Math.ceil(checks.length / checksPerPage);
  const pageIndex = Math.min(checksPage, Math.max(0, pages - 1));
  const shown = checks.slice(pageIndex * checksPerPage, (pageIndex + 1) * checksPerPage);
  const state = shownState(monitor);
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title={monitor.name}
        keepTitle
        description={label(monitor)}
        actions={
          <>
            <StatusBadge tone={stateTone[state]}>{stateLabel[state]}</StatusBadge>
            {canEdit ? (
              <>
                {monitor.agent_id ? null : <Button variant="primary" onClick={() => run.mutate()} disabled={run.isPending}>{run.isPending ? <><Spinner />Checking…</> : "Run check"}</Button>}
                <Button onClick={() => pause.mutate(!monitor.active)}>{monitor.active ? "Pause" : "Resume"}</Button>
                {monitor.agent_id ? null : <Button onClick={() => setEditing(true)}>Edit</Button>}
                <Button variant="danger" onClick={() => setDeleting(true)}>Delete</Button>
              </>
            ) : null}
          </>
        }
      />
      <ErrorText error={run.error || pause.error} />
      {monitor.agent_id ? (
        <LayerCard title="Managed by an agent">
          <p className="text-[0.8125rem] text-[var(--ui-fg-dim)]">This check is defined in the agent's config file. Change it there and restart the agent.</p>
        </LayerCard>
      ) : null}
      <SetupCard monitor={monitor} canEdit={canEdit} />
      <LayerCard title="Uptime" description="Last 90 days">
        <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
          <Stat label="Today" value={percent(monitor.uptime_24h)} />
          <Stat label="30 days" value={percent(monitor.uptime_30d)} />
          <Stat label="90 days" value={percent(monitor.uptime_90d)} />
          <Stat label="Avg latency today" value={monitor.avg_latency_ms == null ? "-" : `${monitor.avg_latency_ms} ms`} />
        </div>
        <UptimeBars bars={monitor.bars} className="mt-4" />
      </LayerCard>
      <LayerCard title="Latency">
        <LatencyChart checks={monitor.recent ?? []} />
      </LayerCard>
      <LayerCard title="Recent checks" flush>
        {checks.length === 0 ? (
          <CardText title="No checks yet" body={monitor.active ? "The first result appears here in a few seconds." : "Resume the monitor to run checks."} />
        ) : (
          <div className="overflow-x-auto">
            <table className="cool-table">
              <thead>
                <tr>
                  <th>Result</th>
                  <th>Status</th>
                  <th>Latency</th>
                  <th>Error</th>
                  <th>From</th>
                  <th>When</th>
                </tr>
              </thead>
              <tbody>
                {shown.map((check) => (
                  <tr key={check.id}>
                    <td><StatusBadge tone={checkTone[check.status] ?? "info"}>{stateLabel[check.status] ?? check.status}</StatusBadge></td>
                    <td>{check.status_code ?? "-"}</td>
                    <td className="whitespace-nowrap">{check.latency_ms} ms</td>
                    <td>{check.error || "-"}</td>
                    <td>{check.region === "local" ? "This server" : check.region}</td>
                    <td className="whitespace-nowrap"><Ago at={check.checked_at} /></td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {pages > 1 ? (
          <div className="flex items-center justify-between gap-3 border-t border-[var(--ui-divider)] px-4 py-2 text-[0.8125rem] text-[var(--ui-fg-faint)]">
            <span>
              Showing {pageIndex * checksPerPage + 1}-{pageIndex * checksPerPage + shown.length} of {checks.length}
            </span>
            <div className="flex gap-2">
              <Button size="sm" onClick={() => setChecksPage(pageIndex - 1)} disabled={pageIndex === 0}>Previous</Button>
              <Button size="sm" onClick={() => setChecksPage(pageIndex + 1)} disabled={pageIndex >= pages - 1}>Next</Button>
            </div>
          </div>
        ) : null}
      </LayerCard>
      {monitor.agent_id || !canEdit ? null : <MonitorDialog open={editing} onClose={() => setEditing(false)} monitor={monitor} />}
      <ConfirmDialog
        open={deleting}
        title="Delete monitor?"
        description={`${monitor.name} and its check history will be deleted.`}
        pending={remove.isPending}
        error={remove.error}
        onConfirm={() => remove.mutate()}
        onClose={() => setDeleting(false)}
      />
    </div>
  );
}

const checksPerPage = 20;

const linkClass = "font-medium text-[var(--ui-fg)] underline-offset-2 hover:underline";
const smallButton = "button h-7 px-2 text-xs";

/** What this monitor is connected to. Editors get a link to fix each missing part. */
function SetupCard({ monitor, canEdit }: { monitor: MonitorDetail; canEdit: boolean }) {
  const channels = monitor.alert_channel_count;
  const pages = monitor.status_pages;
  return (
    <LayerCard title="Setup" description="Who hears about a failure, and where it shows.">
      <ul className="flex flex-col gap-4">
        <SetupItem
          tone={channels > 0 ? "success" : "neutral"}
          label={channels > 0 ? "On" : "Off"}
          title="Alerts"
          action={canEdit && channels === 0 ? <Link className={smallButton} to="/notifications">Add a channel</Link> : null}
        >
          {channels === 0
            ? "No alert channel. A failure opens an incident, but nobody is told."
            : canEdit
              ? `Sent to ${monitor.alert_channels.map((channel) => `${channel.name} (${providerLabel[channel.provider]})`).join(", ")}.`
              : `Sent to ${channels} alert ${channels === 1 ? "channel" : "channels"}.`}
        </SetupItem>
        <SetupItem
          tone={pages.length > 0 ? "success" : "neutral"}
          label={pages.length > 0 ? "On" : "Off"}
          title="Status pages"
          action={canEdit && pages.length === 0 ? <Link className={smallButton} to="/pages">Add to a page</Link> : null}
        >
          {pages.length === 0 ? (
            "Not shown on a status page."
          ) : (
            <>
              Shown on{" "}
              {pages.map((page, index) => (
                <span key={page.id}>
                  {index > 0 ? ", " : null}
                  {canEdit ? <Link className={linkClass} to={`/pages/${page.id}`}>{page.title}</Link> : page.title}
                </span>
              ))}
              .
            </>
          )}
        </SetupItem>
        <SetupItem
          tone={monitor.open_incident_id ? "error" : "success"}
          label={monitor.open_incident_id ? "Open" : "None"}
          title="Incident"
          action={monitor.open_incident_id ? <Link className={smallButton} to={`/incidents/${monitor.open_incident_id}`}>View incident</Link> : null}
        >
          {monitor.open_incident_id ? "An incident is open for this monitor." : "No open incident."}
        </SetupItem>
      </ul>
    </LayerCard>
  );
}

function SetupItem({ tone, label, title, action, children }: { tone: Tone; label: string; title: string; action: ReactNode; children: ReactNode }) {
  return (
    <li className="flex flex-wrap items-center gap-3">
      <span className="w-16 shrink-0"><StatusBadge tone={tone}>{label}</StatusBadge></span>
      <div className="min-w-0 flex-1">
        <p className="text-[0.8125rem] font-medium text-[var(--ui-fg)]">{title}</p>
        <p className="text-[0.8125rem] text-[var(--ui-fg-dim)]">{children}</p>
      </div>
      {action}
    </li>
  );
}

function useMonitorLabel() {
  const scope = useListScope({ allProjects: true });
  const agents = useQuery({ queryKey: scope.key("agents"), queryFn: () => api<Agent[]>(scope.path("/api/agents")) });
  return (monitor: Monitor) => {
    const base = `${monitor.check_type.toUpperCase()} · ${monitor.url}`;
    if (!monitor.agent_id) return base;
    const agent = agents.data?.find((agent) => agent.id === monitor.agent_id)?.name ?? "unknown";
    return `${base} · agent ${agent}`;
  };
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-[0.6875rem] font-medium text-[var(--ui-fg-faint)]">{label}</div>
      <div className="mt-1 text-xl font-semibold text-[var(--ui-fg)]">{value}</div>
    </div>
  );
}

function CardText({ title, body }: { title: string; body?: string }) {
  return (
    <div className="flex min-h-32 flex-col items-center justify-center px-4 py-8 text-center">
      <p className="text-[0.875rem] font-medium text-[var(--ui-fg)]">{title}</p>
      {body ? <p className="mx-auto mt-1 max-w-[28rem] text-[0.8125rem] text-[var(--ui-fg-faint)]">{body}</p> : null}
    </div>
  );
}

const intervals = [15, 30, 60, 300, 600, 1800, 3600].map((seconds) => ({
  value: String(seconds),
  label: seconds < 60 ? `${seconds}s` : `${seconds / 60} min`,
}));
const methods = ["GET", "POST", "HEAD", "PUT", "PATCH", "DELETE"].map((method) => ({ value: method, label: method }));

const advancedFields = ["method", "expected_status", "timeout_ms", "degraded_after_ms", "failure_threshold", "assertions", "description"];

function MonitorDialog({ open, onClose, monitor }: { open: boolean; onClose: () => void; monitor?: MonitorDetail }) {
  const client = useQueryClient();
  const navigate = useNavigate();
  const [projectId, setProjectId] = useProjectChoice();
  const [draft, setDraft] = useState<Draft>(monitor ? draftFrom(monitor) : emptyDraft);
  const [advanced, setAdvanced] = useState(Boolean(monitor));
  const [fields, setFields] = useState<Record<string, string>>({});
  const target = resolved(draft);
  const body = () => JSON.stringify({ ...payload(draft), project_id: monitor?.project_id ?? projectId });
  function showFields(error: Error) {
    const next = error instanceof ApiError ? error.fields ?? {} : {};
    setFields(next);
    if (advancedFields.some((field) => next[field])) setAdvanced(true);
  }
  const test = useMutation({
    mutationFn: () => api<CheckOutcome>("/api/monitors/test", { method: "POST", body: body() }),
    onError: showFields,
  });
  const save = useMutation({
    mutationFn: () =>
      api<MonitorDetail>(monitor ? `/api/monitors/${monitor.id}` : "/api/monitors", {
        method: monitor ? "PUT" : "POST",
        body: body(),
      }),
    onSuccess: async (saved) => {
      client.setQueryData(["monitor", saved.id], saved);
      await client.invalidateQueries({ queryKey: ["monitors"] });
      await client.invalidateQueries({ queryKey: ["overview"] });
      close();
      if (!monitor) navigate(`/monitors/${saved.id}`);
    },
    onError: showFields,
  });

  function update(patch: Partial<Draft>) {
    setDraft((current) => ({ ...current, ...patch }));
    test.reset();
  }

  function close() {
    setDraft(monitor ? draftFrom(monitor) : emptyDraft);
    setAdvanced(Boolean(monitor));
    setFields({});
    test.reset();
    save.reset();
    onClose();
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    save.mutate();
  }

  const fieldError = Object.keys(fields).length > 0;
  return (
    <Dialog open={open} onClose={close} title={monitor ? "Edit monitor" : "New monitor"} description="Paste what to check. Owlee fills in the rest.">
      <form className="space-y-3" onSubmit={submit}>
        <Field label="URL or host:port" error={fields.url}>
          <Input
            value={draft.url}
            onChange={(event) => update({ url: event.target.value })}
            placeholder="example.com or db.internal:5432"
            autoFocus={!monitor}
            required
          />
        </Field>
        {target.url && target.url !== draft.url.trim() ? (
          <p className="-mt-1 text-[0.75rem] text-[var(--ui-fg-faint)]">Checks {target.url}</p>
        ) : null}
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Name" error={fields.name}>
            <Input value={target.name} onChange={(event) => update({ name: event.target.value, name_edited: true })} placeholder="Taken from the URL" required />
          </Field>
          <Field label="Type">
            <Select
              value={target.job_type}
              options={[{ value: "http", label: "HTTP" }, { value: "tcp", label: "TCP" }]}
              onChange={(job_type) => update({ job_type, type_edited: true, url: normalizeTarget(job_type, draft.url) })}
              aria-label="Type"
            />
          </Field>
        </div>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Interval">
            <Select value={draft.interval_seconds} options={intervals} onChange={(interval_seconds) => update({ interval_seconds })} aria-label="Interval" />
          </Field>
          {monitor ? null : <ProjectField value={projectId} onChange={setProjectId} error={fields.project_id} />}
        </div>
        <button type="button" className="text-[0.8125rem] font-medium text-[var(--ui-fg-dim)] hover:text-[var(--ui-fg)]" aria-expanded={advanced} onClick={() => setAdvanced(!advanced)}>
          {advanced ? "Hide advanced" : "Advanced"}
        </button>
        {advanced ? (
          <div className="space-y-3">
            {target.job_type === "http" ? (
              <div className="grid gap-3 sm:grid-cols-2">
                <Field label="Method">
                  <Select value={draft.method} options={methods} onChange={(method) => update({ method })} aria-label="Method" />
                </Field>
                <Field label="Expected status" hint="blank = any 2xx" error={fields.expected_status}>
                  <Input value={draft.expected_status} onChange={(event) => update({ expected_status: event.target.value })} inputMode="numeric" />
                </Field>
              </div>
            ) : null}
            <div className="grid gap-3 sm:grid-cols-3">
              <Field label="Timeout" hint="seconds" error={fields.timeout_ms}>
                <Input value={draft.timeout_seconds} onChange={(event) => update({ timeout_seconds: event.target.value })} />
              </Field>
              <Field label="Degraded after" hint="ms" error={fields.degraded_after_ms}>
                <Input value={draft.degraded_after_ms} onChange={(event) => update({ degraded_after_ms: event.target.value })} />
              </Field>
              <Field label="Failures" hint="before down" error={fields.failure_threshold}>
                <Input value={draft.failure_threshold} onChange={(event) => update({ failure_threshold: event.target.value })} />
              </Field>
            </div>
            {target.job_type === "http" ? (
              <Field label="Body contains" error={fields.assertions}>
                <Input value={draft.body_contains} onChange={(event) => update({ body_contains: event.target.value })} placeholder="optional assertion" />
              </Field>
            ) : null}
            <Field label="Description">
              <Textarea value={draft.description} onChange={(event) => update({ description: event.target.value })} />
            </Field>
          </div>
        ) : null}
        {test.data ? <TestResult outcome={test.data} /> : null}
        <ErrorText error={fieldError ? null : save.error || test.error} />
        <div className="flex justify-end gap-2 pt-2">
          <Button type="button" onClick={close}>Cancel</Button>
          <Button type="button" onClick={() => { setFields({}); test.mutate(); }} disabled={!draft.url.trim() || test.isPending}>
            {test.isPending ? <><Spinner />Testing…</> : "Test"}
          </Button>
          <Button type="submit" variant="primary" disabled={save.isPending}>{save.isPending ? <><Spinner />Saving…</> : "Save monitor"}</Button>
        </div>
      </form>
    </Dialog>
  );
}

function TestResult({ outcome }: { outcome: CheckOutcome }) {
  const details = [outcome.status_code == null ? null : `HTTP ${outcome.status_code}`, `${outcome.latency_ms} ms`].filter(Boolean).join(" · ");
  return (
    <div role="status" className="rounded-md border border-[var(--ui-divider)] px-3 py-2 text-[0.8125rem]">
      <div className="flex items-center gap-2">
        <StatusBadge tone={checkTone[outcome.status] ?? "neutral"}>{stateLabel[outcome.status]}</StatusBadge>
        <span className="text-[var(--ui-fg-dim)]">{details}</span>
      </div>
      {outcome.error ? <p className="mt-1 text-[var(--ui-fg-dim)]">{outcome.error}</p> : null}
    </div>
  );
}
