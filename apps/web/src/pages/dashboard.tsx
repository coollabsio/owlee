import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router";
import { UptimeBars } from "@/components/charts";
import { type Column, DataTable } from "@/components/data-table";
import { Empty, LayerCard, PageHeader, StatusBadge } from "@/components/ui";
import { api, type Overview } from "@/lib/api";
import { formatTime, percent, stateLabel, timeAgo } from "@/lib/utils";
import { useListScope } from "@/lib/workspace";

function tone(state: string): "success" | "warning" | "error" | "neutral" | "info" {
  if (state === "up" || state === "resolved" || state === "success") return "success";
  if (state === "degraded" || state === "minor" || state === "partial_outage") return "warning";
  if (state === "down" || state === "failure" || state === "major" || state === "critical") return "error";
  if (state === "identified" || state === "monitoring" || state === "investigating") return "info";
  return "neutral";
}

export function DashboardPage() {
  const scope = useListScope();
  const overview = useQuery({
    queryKey: scope.key("overview"),
    queryFn: () => api<Overview>(scope.path("/api/overview")),
    refetchInterval: 10_000,
  });
  const monitors = overview.data?.monitors ?? [];
  const down = monitors.filter((monitor) => monitor.state === "down").length;
  const degraded = monitors.filter((monitor) => monitor.state === "degraded").length;
  const summary =
    monitors.length === 0
      ? { tone: "neutral" as const, title: "No monitors yet", body: "Add a check and Owlee will start recording it." }
      : down === monitors.length
        ? { tone: "error" as const, title: "Major outage", body: "Every monitor is down." }
        : down > 0
          ? {
              tone: "warning" as const,
              title: "Partial outage",
              body: `${down} of ${monitors.length} monitors ${down === 1 ? "is" : "are"} down.`,
            }
          : degraded > 0
            ? { tone: "warning" as const, title: "Degraded performance", body: "Some checks are slower than their threshold." }
            : { tone: "success" as const, title: "All systems operational", body: `${monitors.length} monitors reporting.` };

  const incidents = overview.data?.incidents ?? [];
  const activity = overview.data?.activity ?? [];

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Overview" description="Live checks, uptime, and the incidents customers can see." />
      <LayerCard title="System status" actions={<StatusBadge tone={summary.tone}>{summary.title}</StatusBadge>}>
        <p className="text-[0.8125rem] text-[var(--ui-fg-dim)]">{summary.body}</p>
      </LayerCard>
      {overview.error ? <p className="field-error">{(overview.error as Error).message}</p> : null}
      {monitors.length === 0 && !overview.isLoading ? (
        <Empty
          title="Nothing is being watched"
          body="Create an HTTP or TCP monitor. The scheduler probes it on the interval you set and opens an incident after repeated failures."
          action={<Link className="auth-text-link" to="/monitors">Create a monitor</Link>}
        />
      ) : (
        <LayerCard
          title="Monitors"
          description="30-day history"
          actions={<Link className="text-[0.6875rem] font-medium text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]" to="/monitors">View all</Link>}
          flush
        >
          <DataTable
            columns={monitorColumns}
            rows={monitors}
            rowKey={(monitor) => monitor.id}
            rowLink={(monitor) => `/monitors/${monitor.id}`}
            loading={overview.isLoading}
          />
        </LayerCard>
      )}
      <div className="grid items-start gap-6 lg:grid-cols-2">
        <LayerCard title="Open incidents" flush>
          {incidents.length === 0 ? (
            <CardText title="No open incidents" body="Incidents open here after repeated failures or when you report one." />
          ) : (
            <DataTable
              hideHeader
              columns={incidentColumns}
              rows={incidents}
              rowKey={(incident) => incident.id}
              rowLink={(incident) => `/incidents/${incident.id}`}
            />
          )}
        </LayerCard>
        <LayerCard title="Activity" flush>
          {activity.length === 0 ? (
            <CardText title="No activity yet" body="Changes to monitors, incidents, and channels show up here." />
          ) : (
            <DataTable hideHeader columns={activityColumns} rows={activity} rowKey={(event) => event.id} />
          )}
        </LayerCard>
      </div>
    </div>
  );
}

type OverviewMonitor = Overview["monitors"][number];

const monitorColumns: Column<OverviewMonitor>[] = [
  {
    key: "name",
    header: "Name",
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
    sort: (monitor) => stateLabel[monitor.state],
    cell: (monitor) => <StatusBadge tone={tone(monitor.state)}>{stateLabel[monitor.state]}</StatusBadge>,
  },
  { key: "history", header: "History", className: "hidden lg:table-cell", cellClassName: "w-[30%]", cell: (monitor) => <UptimeBars bars={monitor.bars} className="h-6" /> },
  { key: "today", header: "Today", className: "hidden sm:table-cell", sort: (monitor) => monitor.uptime_24h, cell: (monitor) => percent(monitor.uptime_24h) },
  {
    key: "latency",
    header: "Latency",
    className: "hidden md:table-cell",
    sort: (monitor) => monitor.avg_latency_ms,
    cell: (monitor) => (monitor.avg_latency_ms == null ? "-" : `${monitor.avg_latency_ms} ms`),
  },
  {
    key: "checked",
    header: "Last check",
    sort: (monitor) => monitor.last_checked_at,
    cellClassName: "whitespace-nowrap",
    cell: (monitor) => timeAgo(monitor.last_checked_at),
  },
];

const incidentColumns: Column<Overview["incidents"][number]>[] = [
  {
    key: "title",
    header: "Incident",
    cell: (incident) => (
      <>
        <Link className="row-link" to={`/incidents/${incident.id}`}>{incident.title}</Link>
        <div className="text-[0.6875rem] text-[var(--ui-fg-faint)]">{formatTime(incident.started_at)}</div>
      </>
    ),
  },
  { key: "status", header: "Status", cellClassName: "text-right", cell: (incident) => <StatusBadge tone={tone(incident.status)}>{stateLabel[incident.status]}</StatusBadge> },
];

const activityColumns: Column<Overview["activity"][number]>[] = [
  { key: "action", header: "Action", cell: (event) => event.action.replaceAll(".", " ") },
  { key: "when", header: "When", cellClassName: "whitespace-nowrap text-right text-[var(--ui-fg-faint)]", cell: (event) => timeAgo(event.occurred_at) },
];

function CardText({ title, body }: { title: string; body?: string }) {
  return (
    <div className="flex min-h-32 flex-col items-center justify-center px-4 py-8 text-center">
      <p className="text-[0.875rem] font-medium text-[var(--ui-fg)]">{title}</p>
      {body ? <p className="mx-auto mt-1 max-w-[28rem] text-[0.8125rem] text-[var(--ui-fg-faint)]">{body}</p> : null}
    </div>
  );
}
