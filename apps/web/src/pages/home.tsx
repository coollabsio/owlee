import { useQuery } from "@tanstack/react-query";
import { type ReactNode } from "react";
import { Link } from "react-router";
import { products } from "@/app/nav";
import { Sparkline } from "@/components/error-charts";
import { type Column, DataTable } from "@/components/data-table";
import { ErrorText, LayerCard, PageHeader, Skeleton, StatusBadge } from "@/components/ui";
import { api, type Dashboard, type DashboardFailing, type DashboardIncident, type DashboardIssue, type DashboardPageRow, type DashboardSite } from "@/lib/api";
import { duration, percent, stateLabel, timeAgo } from "@/lib/utils";
import { useListScope } from "@/lib/workspace";

function tone(state: string): "success" | "warning" | "error" | "neutral" | "info" {
  if (state === "up" || state === "operational" || state === "resolved") return "success";
  if (state === "degraded" || state === "partial_outage" || state === "minor") return "warning";
  if (state === "down" || state === "major_outage" || state === "major" || state === "critical" || state === "fatal") return "error";
  if (state === "investigating" || state === "identified" || state === "monitoring") return "info";
  return "neutral";
}

type ProductId = "status" | "errors" | "analytics";

const setup: Record<ProductId, { title: string; text: string; actions: { to: string; label: string }[] }> = {
  status: {
    title: "Uptime monitoring",
    text: "Get an alert when a site or service goes down, and publish a status page.",
    actions: [{ to: "/monitors", label: "Create a monitor" }],
  },
  errors: {
    title: "Error tracking",
    text: "Collect exceptions from your apps, grouped into issues.",
    actions: [{ to: "/errors/apps", label: "Connect an app" }],
  },
  analytics: {
    title: "Web analytics",
    text: "Cookieless visitor stats and short links for your sites.",
    actions: [
      { to: "/analytics", label: "Add a site" },
      { to: "/links", label: "Create a short link" },
    ],
  },
};

function inUse(data: Dashboard): Record<ProductId, boolean> {
  const { up, down, degraded, pending, paused } = data.monitors;
  return {
    status: up + down + degraded + pending + paused > 0 || data.pages.length > 0 || data.incidents.open > 0,
    errors: data.errors.apps > 0,
    analytics: data.analytics.sites > 0 || data.analytics.links > 0,
  };
}

function SetupCard({ id }: { id: ProductId }) {
  const card = setup[id];
  const Icon = products.find((product) => product.id === id)!.icon;
  return (
    <LayerCard className="[&>.layer-card-body]:flex-1">
      <div className="flex h-full flex-col gap-3 p-1">
        <div className="flex items-center gap-2.5">
          <span className="flex size-8 shrink-0 items-center justify-center rounded-md border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] text-[var(--ui-fg)]">
            <Icon className="size-4" aria-hidden="true" />
          </span>
          <h2 className="text-[0.875rem] font-medium text-[var(--ui-fg)]">{card.title}</h2>
        </div>
        <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">{card.text}</p>
        <div className="mt-auto flex flex-wrap gap-2">
          {card.actions.map((action) => (
            <Link key={action.to} className="button button-highlighted" to={action.to}>{action.label}</Link>
          ))}
        </div>
      </div>
    </LayerCard>
  );
}

export function HomePage() {
  const scope = useListScope();
  const home = useQuery({
    queryKey: scope.key("dashboard"),
    queryFn: () => api<Dashboard>(scope.path("/api/dashboard")),
    refetchInterval: 15_000,
  });
  const data = home.data;

  if (!data) {
    return (
      <div className="flex flex-col gap-6">
        <PageHeader title="Dashboard" description="Monitors, incidents, errors, and traffic." />
        {home.error ? (
          <ErrorText error={home.error} />
        ) : (
          <div className="flex flex-col gap-6" aria-busy="true">
            <LayerCard><Skeleton className="h-32 w-full" /></LayerCard>
            <div className="grid items-start gap-6 lg:grid-cols-2">
              <LayerCard><Skeleton className="h-24 w-full" /></LayerCard>
              <LayerCard><Skeleton className="h-24 w-full" /></LayerCard>
            </div>
          </div>
        )}
      </div>
    );
  }

  const used = inUse(data);

  if (!used.status && !used.errors && !used.analytics) {
    return (
      <div className="flex flex-col gap-6">
        <PageHeader title="Get started" description="Owlee does three things. Set up the ones you need." />
        <div className="grid gap-6 lg:grid-cols-3">
          <SetupCard id="status" />
          <SetupCard id="errors" />
          <SetupCard id="analytics" />
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Dashboard" description="Monitors, incidents, errors, and traffic." />
      <ErrorText error={home.error} />
      <QuotaNotice hits={data.quota_hits} />
      {used.status ? (
        <>
          <MonitorsCard data={data} />
          <div className="grid items-start gap-6 lg:grid-cols-2">
            <IncidentsCard incidents={data.incidents} />
            <PagesCard pages={data.pages} />
          </div>
        </>
      ) : null}
      <div className="grid items-start gap-6 lg:grid-cols-2">
        {used.errors ? <ErrorsCard errors={data.errors} /> : <SetupCard id="errors" />}
        {used.analytics ? <TrafficCard analytics={data.analytics} /> : <SetupCard id="analytics" />}
      </div>
      {used.status ? null : <SetupCard id="status" />}
    </div>
  );
}

function resetWhen(iso: string) {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "00:00 UTC";
  return `${date.toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", timeZone: "UTC" })} UTC`;
}

function QuotaNotice({ hits }: { hits: Dashboard["quota_hits"] }) {
  if (hits.length === 0) return null;
  return (
    <LayerCard title="Ingest stopped" description="An app or site reached its daily cap. New events are rejected until the counter resets.">
      <ul className="space-y-2">
        {hits.map((hit) => {
          const to = hit.kind === "error" ? `/errors/apps/${hit.id}` : `/analytics/${hit.id}`;
          const noun = hit.kind === "error" ? "errors" : "events";
          return (
            <li key={`${hit.kind}-${hit.id}`} className="flex flex-wrap items-baseline gap-x-2 text-[0.8125rem]">
              <StatusBadge tone="error">Cap reached</StatusBadge>
              <Link className="font-medium text-[var(--ui-fg)] hover:underline" to={to}>{hit.name}</Link>
              <span className="text-[var(--ui-fg-faint)]">
                {hit.used.toLocaleString()} / {hit.quota.toLocaleString()} {noun}. Resets {resetWhen(hit.resets_at)}.
              </span>
            </li>
          );
        })}
      </ul>
    </LayerCard>
  );
}

function viewAll(to: string, label = "View all") {
  return <Link className="text-[0.6875rem] font-medium text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]" to={to}>{label}</Link>;
}

function Note({ children }: { children: ReactNode }) {
  return <p className="px-4 py-8 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">{children}</p>;
}

function Metric({ label, value, previous, emphasize }: { label: string; value: string; previous?: { current: number; previous: number }; emphasize?: boolean }) {
  return (
    <div className="min-w-0">
      <p className="text-[0.6875rem] font-medium text-[var(--ui-fg-faint)]">{label}</p>
      <p className={emphasize ? "mt-1 text-[1.5rem] leading-8 font-semibold tracking-tight text-[var(--ui-error)]" : "mt-1 text-[1.5rem] leading-8 font-semibold tracking-tight text-[var(--ui-fg)]"}>{value}</p>
      {previous ? (
        <p className="mt-0.5 text-[0.75rem] font-medium">
          <Delta current={previous.current} previous={previous.previous} />
        </p>
      ) : null}
    </div>
  );
}

function Delta({ current, previous }: { current: number; previous: number }) {
  if (current === 0 && previous === 0) return null;
  const label = previous === 0 ? "new" : `${current > previous ? "+" : ""}${Math.round(((current - previous) / previous) * 100)}%`;
  const className = current > previous ? "text-[var(--ui-success)]" : current < previous ? "text-[var(--ui-error)]" : "text-[var(--ui-fg-faint)]";
  return <span className={`text-[0.75rem] font-medium ${className}`}>{label}</span>;
}

function summary(data: Dashboard) {
  const { up, down, degraded, pending, paused } = data.monitors;
  const active = up + down + degraded + pending;
  if (active === 0 && paused === 0) return { tone: "neutral" as const, title: "No monitors" };
  if (down > 0 && down === active) return { tone: "error" as const, title: "Major outage" };
  if (down > 0) return { tone: "warning" as const, title: "Partial outage" };
  if (degraded > 0) return { tone: "warning" as const, title: "Degraded" };
  if (pending > 0 && up === 0) return { tone: "neutral" as const, title: "Waiting for checks" };
  return { tone: "success" as const, title: "All up" };
}

function MonitorsCard({ data }: { data: Dashboard }) {
  const { monitors } = data;
  const count = monitors.up + monitors.down + monitors.degraded + monitors.pending + monitors.paused;
  const failing = monitors.down + monitors.degraded;
  const status = summary(data);
  const extra = [
    monitors.pending > 0 ? `${monitors.pending} pending` : null,
    monitors.paused > 0 ? `${monitors.paused} paused` : null,
  ].filter(Boolean);
  return (
    <LayerCard
      title="Monitors"
      description={failing > monitors.failing.length ? `Showing ${monitors.failing.length} of ${failing} failing` : undefined}
      actions={<><StatusBadge tone={status.tone}>{status.title}</StatusBadge>{viewAll("/monitors")}</>}
      flush
    >
      {count === 0 ? (
        <Note>No monitors yet. <Link className="auth-text-link" to="/monitors">Create a monitor</Link></Note>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-6 border-b border-[var(--ui-divider)] px-4 py-4 sm:grid-cols-3 xl:grid-cols-5">
            <Metric label="Up" value={String(monitors.up)} />
            <Metric label="Down" value={String(monitors.down)} emphasize={monitors.down > 0} />
            <Metric label="Degraded" value={String(monitors.degraded)} />
            <Metric label="Uptime today" value={percent(monitors.uptime_today)} />
            <Metric label="Response today" value={monitors.avg_latency_ms == null ? "-" : `${monitors.avg_latency_ms} ms`} />
          </div>
          {extra.length > 0 ? <p className="border-b border-[var(--ui-divider)] px-4 py-2 text-[0.75rem] text-[var(--ui-fg-faint)]">{extra.join(" · ")}</p> : null}
          {monitors.failing.length === 0 ? (
            <Note>Nothing is failing.</Note>
          ) : (
            <DataTable columns={failingColumns} rows={monitors.failing} rowKey={(monitor) => monitor.id} rowLink={(monitor) => `/monitors/${monitor.id}`} />
          )}
        </>
      )}
    </LayerCard>
  );
}

const failingColumns: Column<DashboardFailing>[] = [
  {
    key: "name",
    header: "Failing",
    sort: (monitor) => monitor.name,
    cellClassName: "max-w-[16rem]",
    cell: (monitor) => (
      <>
        <Link className="row-link" to={`/monitors/${monitor.id}`}>{monitor.name}</Link>
        <div className="truncate text-[0.6875rem] text-[var(--ui-fg-faint)]">{monitor.url}</div>
      </>
    ),
  },
  {
    key: "state",
    header: "State",
    sort: (monitor) => stateLabel[monitor.state] ?? monitor.state,
    cell: (monitor) => <StatusBadge tone={tone(monitor.state)}>{stateLabel[monitor.state] ?? monitor.state}</StatusBadge>,
  },
  // Negated so ascending puts the shortest outage first, like the duration it shows.
  { key: "for", header: "For", cellClassName: "whitespace-nowrap", sort: (monitor) => (monitor.since ? -Date.parse(monitor.since) : null), cell: (monitor) => duration(monitor.since) },
  { key: "today", header: "Today", className: "hidden sm:table-cell", sort: (monitor) => monitor.uptime_today, cell: (monitor) => percent(monitor.uptime_today) },
  {
    key: "latency",
    header: "Latency",
    className: "hidden md:table-cell",
    sort: (monitor) => monitor.avg_latency_ms,
    cell: (monitor) => (monitor.avg_latency_ms == null ? "-" : `${monitor.avg_latency_ms} ms`),
  },
];

function IncidentsCard({ incidents }: { incidents: Dashboard["incidents"] }) {
  return (
    <LayerCard title="Open incidents" description={incidents.open > 0 ? `${incidents.open} open` : undefined} actions={viewAll("/incidents")} flush>
      {incidents.items.length === 0 ? (
        <Note>No open incidents.</Note>
      ) : (
        <DataTable hideHeader columns={incidentColumns} rows={incidents.items} rowKey={(incident) => incident.id} rowLink={(incident) => `/incidents/${incident.id}`} />
      )}
    </LayerCard>
  );
}

const incidentColumns: Column<DashboardIncident>[] = [
  {
    key: "title",
    header: "Incident",
    cell: (incident) => (
      <>
        <Link className="row-link" to={`/incidents/${incident.id}`}>{incident.title}</Link>
        <div className="text-[0.6875rem] text-[var(--ui-fg-faint)]">{stateLabel[incident.impact] ?? incident.impact} · {duration(incident.started_at)}</div>
      </>
    ),
  },
  { key: "status", header: "Status", cellClassName: "text-right", cell: (incident) => <StatusBadge tone={tone(incident.status)}>{stateLabel[incident.status] ?? incident.status}</StatusBadge> },
];

function PagesCard({ pages }: { pages: DashboardPageRow[] }) {
  return (
    <LayerCard title="Status pages" actions={viewAll("/pages")} flush>
      {pages.length === 0 ? (
        <Note>No status pages. <Link className="auth-text-link" to="/pages">Create a page</Link></Note>
      ) : (
        <DataTable hideHeader columns={pageColumns} rows={pages} rowKey={(page) => page.id} rowLink={(page) => `/pages/${page.id}`} />
      )}
    </LayerCard>
  );
}

const pageColumns: Column<DashboardPageRow>[] = [
  {
    key: "title",
    header: "Page",
    cell: (page) => (
      <>
        <Link className="row-link" to={`/pages/${page.id}`}>{page.title}</Link>
        <div className="truncate text-[0.6875rem] text-[var(--ui-fg-faint)]">/status/{page.slug}</div>
      </>
    ),
  },
  {
    key: "banner",
    header: "Status",
    cellClassName: "text-right",
    cell: (page) => (
      <span className="inline-flex flex-wrap items-center justify-end gap-1">
        {!page.published ? <StatusBadge tone="neutral">Draft</StatusBadge> : null}
        <StatusBadge tone={tone(page.banner)}>{stateLabel[page.banner] ?? page.banner}</StatusBadge>
      </span>
    ),
  },
];

function ErrorsCard({ errors }: { errors: Dashboard["errors"] }) {
  const events = errors.events_24h.reduce((sum, value) => sum + value, 0);
  return (
    <LayerCard title="Errors" description="Last 24 hours" actions={viewAll("/errors")} flush>
      <div className="flex items-end justify-between gap-4 border-b border-[var(--ui-divider)] px-4 py-4">
        <div className="grid flex-1 grid-cols-3 gap-4">
          <Metric label="Unresolved" value={String(errors.unresolved)} emphasize={errors.unresolved > 0} />
          <Metric label="New" value={String(errors.new_24h)} />
          <Metric label="Events" value={events.toLocaleString()} />
        </div>
        <div aria-label="Events over the last 24 hours">
          <Sparkline values={errors.events_24h} />
        </div>
      </div>
      {errors.issues.length === 0 ? (
        <Note>No unresolved issues.</Note>
      ) : (
        <DataTable hideHeader columns={issueColumns} rows={errors.issues} rowKey={(issue) => issue.id} rowLink={(issue) => `/errors/${issue.short_id || issue.id}`} />
      )}
    </LayerCard>
  );
}

const issueColumns: Column<DashboardIssue>[] = [
  {
    key: "title",
    header: "Issue",
    cellClassName: "max-w-[18rem]",
    cell: (issue) => (
      <>
        <Link className="row-link" to={`/errors/${issue.short_id || issue.id}`}>{issue.title}</Link>
        <div className="truncate text-[0.6875rem] text-[var(--ui-fg-faint)]">{issue.short_id ? `${issue.short_id} · ` : ""}{issue.app_name}</div>
      </>
    ),
  },
  {
    key: "events",
    header: "Events",
    cellClassName: "whitespace-nowrap text-right",
    cell: (issue) => (
      <>
        <div className="font-mono text-[0.75rem]">{issue.event_count.toLocaleString()}</div>
        <div className="text-[0.6875rem] text-[var(--ui-fg-faint)]">{timeAgo(issue.last_seen)}</div>
      </>
    ),
  },
];

function TrafficCard({ analytics }: { analytics: Dashboard["analytics"] }) {
  return (
    <LayerCard title="Traffic" description={`Last ${analytics.days} days, against the ${analytics.days} days before`} actions={<>{viewAll("/analytics", "Sites")}{viewAll("/links", "Links")}</>} flush>
      <div className="grid grid-cols-3 gap-4 border-b border-[var(--ui-divider)] px-4 py-4">
        <Metric label="Visitors" value={analytics.visitors.toLocaleString()} previous={{ current: analytics.visitors, previous: analytics.previous_visitors }} />
        <Metric label="Pageviews" value={analytics.pageviews.toLocaleString()} previous={{ current: analytics.pageviews, previous: analytics.previous_pageviews }} />
        <Metric label="Link clicks" value={analytics.clicks.toLocaleString()} previous={{ current: analytics.clicks, previous: analytics.previous_clicks }} />
      </div>
      {analytics.top_sites.length === 0 ? (
        <Note>No sites yet. <Link className="auth-text-link" to="/analytics">Add a site</Link></Note>
      ) : (
        <DataTable columns={siteColumns} rows={analytics.top_sites} rowKey={(site) => site.id} rowLink={(site) => `/analytics/${site.id}`} />
      )}
    </LayerCard>
  );
}

const siteColumns: Column<DashboardSite>[] = [
  {
    key: "name",
    header: "Site",
    sort: (site) => site.name,
    cell: (site) => (
      <>
        <Link className="row-link" to={`/analytics/${site.id}`}>{site.name}</Link>
        <div className="truncate text-[0.6875rem] text-[var(--ui-fg-faint)]">{site.domain}</div>
      </>
    ),
  },
  { key: "visitors", header: "Visitors", className: "text-right", cellClassName: "font-mono", sort: (site) => site.visitors, cell: (site) => site.visitors.toLocaleString() },
  { key: "pageviews", header: "Views", className: "text-right", cellClassName: "font-mono", sort: (site) => site.pageviews, cell: (site) => site.pageviews.toLocaleString() },
];
