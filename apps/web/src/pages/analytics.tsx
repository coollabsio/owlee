import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, type ReactNode, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { BarList, DailyBars, Stat } from "@/components/charts";
import { CodeBlock, GuideStep, ZoneDownload } from "@/components/setup-guide";
import { type Column, DataTable } from "@/components/data-table";
import { Button, ConfirmDialog, Dialog, Empty, ErrorText, Field, Input, LayerCard, PageHeader, Select, Skeleton, Spinner, StatusBadge } from "@/components/ui";
import { api, type InsightOverview, type InsightRow, type InsightSite } from "@/lib/api";
import { ProjectField, ProjectTag } from "@/components/project";
import { useCanEdit } from "@/lib/me";
import { cleanHost, siteName } from "@/lib/site-host";
import { cn, fullTime, intakeSummary, oneOf, timeAgo } from "@/lib/utils";
import { useListScope, useProjectChoice } from "@/lib/workspace";

const ranges = [
  { id: "24h", label: "24 hours", seconds: 86_400 },
  { id: "7d", label: "7 days", seconds: 7 * 86_400 },
  { id: "30d", label: "30 days", seconds: 30 * 86_400 },
  { id: "90d", label: "90 days", seconds: 90 * 86_400 },
] as const;
type RangeId = (typeof ranges)[number]["id"];
const defaultRange: RangeId = "30d";

const tabs = [
  { id: "overview", label: "Overview", ranged: true },
  { id: "setup", label: "Setup", ranged: false },
  { id: "realtime", label: "Realtime", ranged: false },
  { id: "events", label: "Events", ranged: true },
  { id: "visitors", label: "Visitors", ranged: true },
  { id: "settings", label: "Settings", ranged: false },
] as const;
type Tab = (typeof tabs)[number]["id"];

/** `tab` is null when the URL names none, and the page picks one from the site. */
type SiteView = { tab: Tab | null; range: RangeId };

function parseView(params: URLSearchParams): SiteView {
  const tab = params.get("tab");
  return {
    tab: tabs.some((item) => item.id === tab) ? (tab as Tab) : null,
    range: oneOf(params.get("range"), ranges.map((range) => range.id), defaultRange),
  };
}

function viewSearch(view: SiteView, landing: Tab) {
  const search = new URLSearchParams();
  if (view.tab && view.tab !== landing) search.set("tab", view.tab);
  if (view.range !== defaultRange) search.set("range", view.range);
  return search;
}

function rangeQuery(range: RangeId) {
  const seconds = ranges.find((item) => item.id === range)!.seconds;
  const to = Math.floor(Date.now() / 1000);
  return `from=${to - seconds}&to=${to}`;
}

function epochIso(seconds: number) {
  return new Date(seconds * 1000).toISOString();
}

export function AnalyticsPage() {
  const { siteId } = useParams();
  if (siteId) return <SiteDetail key={siteId} siteId={siteId} />;
  return <SiteList />;
}

function CardEmpty({ title, body }: { title: string; body?: string }) {
  return (
    <div className="px-4 py-10 text-center">
      <p className="text-[0.875rem] font-medium text-[var(--ui-fg)]">{title}</p>
      {body ? <p className="mx-auto mt-1 max-w-[28rem] text-[0.8125rem] text-[var(--ui-fg-faint)]">{body}</p> : null}
    </div>
  );
}

function SiteList() {
  const scope = useListScope();
  const query = useQuery({ queryKey: scope.key("sites"), queryFn: () => api<InsightSite[]>(scope.path("/api/sites")) });
  const sites = query.data ?? [];
  const canEdit = useCanEdit();
  const [open, setOpen] = useState(false);
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Sites"
        description="Cookieless page views. Visitors are a daily hash of address and browser, and the address is not stored."
      />
      <ErrorText error={query.error} />
      {sites.length === 0 && !query.isLoading ? (
        <Empty title="No sites yet" body="Add the site you want to measure, then paste the tracker snippet." action={canEdit ? <Button variant="primary" onClick={() => setOpen(true)}>New site</Button> : undefined} />
      ) : (
        <LayerCard
          title="Sites"
          description="Open a site to see its traffic."
          actions={canEdit ? <Button variant="primary" onClick={() => setOpen(true)}>New site</Button> : null}
          flush
        >
          <DataTable
            columns={siteColumns}
            rows={sites}
            rowKey={(site) => site.id}
            rowLink={(site) => `/analytics/${site.id}`}
            loading={query.isLoading}
          />
        </LayerCard>
      )}
      <Dialog open={canEdit && open} title="New site" description="Paste the site's address. Only the hostname is kept." onClose={() => setOpen(false)}>
        {open ? <NewSiteForm /> : null}
      </Dialog>
    </div>
  );
}

const siteColumns: Column<InsightSite>[] = [
  {
    key: "name",
    header: "Name",
    sort: (site) => site.name,
    cell: (site) => (
      <span className="inline-flex items-center gap-2">
        <Link className="row-link" to={`/analytics/${site.id}`}>{site.name}</Link>
        <ProjectTag id={site.project_id} />
        {site.last_event_at == null ? <StatusBadge tone="info">Waiting for data</StatusBadge> : null}
      </span>
    ),
  },
  {
    key: "domain",
    header: "Domain",
    className: "hidden sm:table-cell",
    sort: (site) => site.domain,
    cell: (site) => (
      <span className="inline-flex items-center gap-2">
        {site.domain}
        {site.verified ? null : <StatusBadge tone="warning">Unverified</StatusBadge>}
        {site.quota_exceeded ? <StatusBadge tone="error">Quota reached</StatusBadge> : null}
      </span>
    ),
  },
  { key: "visitors", header: "Visitors, 24h", className: "text-right!", cellClassName: "font-mono text-[0.75rem]", sort: (site) => site.visitors_24h, cell: (site) => site.visitors_24h.toLocaleString() },
  { key: "pageviews", header: "Pageviews, 24h", className: "text-right!", cellClassName: "font-mono text-[0.75rem]", sort: (site) => site.pageviews_24h, cell: (site) => site.pageviews_24h.toLocaleString() },
];

function NewSiteForm() {
  const client = useQueryClient();
  const navigate = useNavigate();
  const [domain, setDomain] = useState("");
  // Null while the name follows the domain. Typing a name stops that.
  const [name, setName] = useState<string | null>(null);
  const [projectId, setProjectId] = useProjectChoice();
  const host = cleanHost(domain);
  const shownName = name ?? siteName(host);
  const create = useMutation({
    mutationFn: () => api<InsightSite>("/api/sites", { method: "POST", body: JSON.stringify({ name: shownName, domain: host, project_id: projectId }) }),
    onSuccess: async (site) => {
      await client.invalidateQueries({ queryKey: ["sites"] });
      navigate(`/analytics/${site.id}`);
    },
  });
  return (
    <form className="space-y-4" onSubmit={(event) => { event.preventDefault(); setDomain(host); create.mutate(); }}>
      <Field label="Domain">
        <Input required value={domain} onChange={(event) => setDomain(event.target.value)} onBlur={() => setDomain(host)} placeholder="coolify.io" />
      </Field>
      <Field label="Name">
        <Input required value={shownName} onChange={(event) => setName(event.target.value)} placeholder="coolify.io" />
      </Field>
      <ProjectField value={projectId} onChange={setProjectId} />
      <ErrorText error={create.error} />
      <Button variant="primary" type="submit" disabled={create.isPending}>{create.isPending ? <><Spinner />Creating…</> : "Create site"}</Button>
    </form>
  );
}

function SiteDetail({ siteId }: { siteId: string }) {
  const query = useQuery({
    queryKey: ["sites", "detail", siteId],
    queryFn: () => api<InsightSite>(`/api/sites/${siteId}`),
    refetchInterval: (state) => (state.state.data && state.state.data.last_event_at == null ? 3000 : false),
  });
  const [params, setParams] = useSearchParams();
  const canEdit = useCanEdit();
  // Picked once per visit, so the first page view does not move the user off Setup.
  const [landing, setLanding] = useState<Tab | null>(null);
  const [sawWaiting, setSawWaiting] = useState(false);
  const site = query.data;
  if (site && landing == null) setLanding(site.last_event_at == null && canEdit ? "setup" : "overview");
  if (site && site.last_event_at == null && !sawWaiting) setSawWaiting(true);

  if (query.isLoading || (site && landing == null)) {
    return (
      <div className="flex flex-col gap-6" aria-busy="true">
        <div className="space-y-2">
          <Skeleton className="h-6 w-40" />
          <Skeleton className="h-3 w-72" />
        </div>
        <LayerCard><Skeleton className="h-16 w-full" /></LayerCard>
        <LayerCard><Skeleton className="h-40 w-full" /></LayerCard>
      </div>
    );
  }
  if (!site || !landing) return <ErrorText error={query.error ?? new Error("Site not found.")} />;

  const view = parseView(params);
  const tab = view.tab ?? landing;
  // Reads the live URL, as on the monitor list: setSearchParams(fn) does not queue.
  const setView = (patch: Partial<SiteView>) =>
    setParams(viewSearch({ ...parseView(new URLSearchParams(window.location.search)), ...patch }, landing));
  const overviewSearch = viewSearch({ tab: "overview", range: view.range }, landing).toString();
  const overviewLink = `/analytics/${site.id}${overviewSearch ? `?${overviewSearch}` : ""}`;
  const range = rangeQuery(view.range);
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title={site.name}
        keepTitle
        description={`${site.domain} · no cookies, no stored IP addresses`}
        actions={
          tabs.find((item) => item.id === tab)!.ranged ? (
            <Select
              className="w-36"
              aria-label="Range"
              value={view.range}
              options={ranges.map((range) => ({ value: range.id, label: range.label }))}
              onChange={(range) => setView({ range })}
            />
          ) : null
        }
      />
      {site.ingest_open ? null : <UnverifiedBanner site={site} onOpen={() => setView({ tab: "settings" })} />}
      {/* The rule lives on the wrapper. The nav overlaps it by 1px so the
          active underline covers that rule. Putting the overlap on the
          buttons, inside overflow-x-auto, computes overflow-y to auto and
          the extra pixel shows a scrollbar even when the tabs fit. */}
      <div className="-mt-2 min-w-0 border-b border-[var(--ui-divider)]">
        <nav className="-mb-px flex gap-1 overflow-x-auto overflow-y-hidden" aria-label="Site sections">
          {tabs.map((item) => (
            <button
              key={item.id}
              type="button"
              aria-current={item.id === tab ? "page" : undefined}
              className={cn(
                "h-9 shrink-0 border-b-2 px-2.5 text-[0.8125rem] font-medium transition-colors",
                item.id === tab ? "border-[var(--ui-accent)] text-[var(--ui-fg)]" : "border-transparent text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]",
              )}
              onClick={() => setView({ tab: item.id })}
            >
              {item.label}
            </button>
          ))}
        </nav>
      </div>
      {tab === "overview" ? <OverviewPanel siteId={siteId} query={range} /> : null}
      {tab === "setup" ? <SetupPanel site={site} sawWaiting={sawWaiting} overviewLink={overviewLink} /> : null}
      {tab === "realtime" ? <RealtimePanel siteId={siteId} /> : null}
      {tab === "events" ? <EventsPanel siteId={siteId} query={range} /> : null}
      {tab === "visitors" ? <VisitorsPanel siteId={siteId} query={range} /> : null}
      {tab === "settings" ? <SettingsPanel site={site} /> : null}
    </div>
  );
}

function UnverifiedBanner({ site, onOpen }: { site: InsightSite; onOpen: () => void }) {
  const canEdit = useCanEdit();
  return (
    <div
      role="alert"
      className="flex flex-wrap items-center gap-3 rounded-lg border border-[color-mix(in_oklab,var(--ui-warning)_45%,transparent)] bg-[color-mix(in_oklab,var(--ui-warning)_8%,transparent)] px-4 py-3 text-[0.8125rem]"
    >
      <div className="min-w-0 flex-1 space-y-1">
        <p className="font-medium text-[var(--ui-fg)]">Page views from {site.domain} are dropped.</p>
        <p className="text-[var(--ui-fg-dim)]">
          This server accepts events only after the domain's TXT record verifies.{canEdit ? "" : " An editor has to verify the domain."}
        </p>
      </div>
      {canEdit ? <Button size="sm" onClick={onOpen}>Verify domain</Button> : null}
    </div>
  );
}

function StatGrid({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn("grid gap-6 sm:grid-cols-2", className)}>{children}</div>;
}

function OverviewPanel({ siteId, query }: { siteId: string; query: string }) {
  const overview = useQuery({
    queryKey: ["site-overview", siteId, query],
    queryFn: () => api<InsightOverview>(`/api/sites/${siteId}/overview?${query}`),
  });
  const data = overview.data;
  return (
    <div className="flex flex-col gap-6">
      <ErrorText error={overview.error} />
      <LayerCard title="Summary" description="Totals for the selected range.">
        <StatGrid className="xl:grid-cols-4">
          <Stat label="Pageviews" value={data?.pageviews} />
          <Stat label="Visitors" value={data?.visitors} />
          <Stat label="Visits" value={data?.visits} />
          <Stat label="Bounce rate" value={data && data.visits ? `${Math.round((data.bounces / data.visits) * 100)}%` : "-"} />
        </StatGrid>
      </LayerCard>
      <LayerCard title="Pageviews" description="Views per day.">
        <DailyBars series={data?.series ?? []} field="views" />
      </LayerCard>
      <div className="grid items-start gap-6 lg:grid-cols-2 xl:grid-cols-3">
        <BreakdownCard siteId={siteId} query={query} title="Pages" views={[{ dimension: "path", label: "Path" }]} />
        <BreakdownCard siteId={siteId} query={query} title="Referrers" views={[{ dimension: "referrer", label: "Referrers" }]} />
        <BreakdownCard siteId={siteId} query={query} title="Locations" views={[{ dimension: "country", label: "Countries" }]} />
        <BreakdownCard
          siteId={siteId}
          query={query}
          title="Technology"
          views={[
            { dimension: "browser", label: "Browsers" },
            { dimension: "os", label: "OS" },
            { dimension: "device", label: "Devices" },
          ]}
        />
        <VitalsCard siteId={siteId} query={query} />
      </div>
    </div>
  );
}

function RealtimePanel({ siteId }: { siteId: string }) {
  const realtime = useQuery({
    queryKey: ["site-realtime", siteId],
    queryFn: () => api<{ current_visitors: number; activity: RealtimeItem[] }>(`/api/sites/${siteId}/realtime`),
    refetchInterval: 10_000,
  });
  const activity = realtime.data?.activity ?? [];
  return (
    <div className="flex flex-col gap-6">
      <ErrorText error={realtime.error} />
      <LayerCard title="Right now" description="Visitors in the last 5 minutes.">
        <Stat label="Current visitors" value={realtime.data?.current_visitors ?? 0} />
      </LayerCard>
      <LayerCard title="Activity" flush>
        {activity.length === 0 ? <CardEmpty title="No activity" body="New page views and events show up here." /> : (
          <DataTable columns={realtimeColumns} rows={activity} rowKey={(item, index) => `${item.timestamp}-${index}`} />
        )}
      </LayerCard>
    </div>
  );
}

type RealtimeItem = { timestamp: number; type: string; path: string | null; country: string | null; browser: string | null };

const realtimeColumns: Column<RealtimeItem>[] = [
  { key: "page", header: "Page", cellClassName: "font-medium text-[var(--ui-fg)]", sort: (item) => item.path ?? item.type, cell: (item) => item.path ?? item.type },
  { key: "type", header: "Type", sort: (item) => item.type, cell: (item) => item.type },
  { key: "country", header: "Country", className: "hidden sm:table-cell", sort: (item) => item.country, cell: (item) => item.country ?? "-" },
  { key: "browser", header: "Browser", className: "hidden sm:table-cell", sort: (item) => item.browser, cell: (item) => item.browser ?? "-" },
];

function VitalsCard({ siteId, query }: { siteId: string; query: string }) {
  const vitals = useQuery({
    queryKey: ["site-vitals", siteId, query],
    queryFn: () => api<{ rows: { metric: string; device: string; p75: number | null; samples: number }[] }>(`/api/sites/${siteId}/vitals?${query}`),
  });
  const rows = vitals.data?.rows ?? [];
  const metrics = ["lcp", "inp", "cls", "fcp", "ttfb"];
  return (
    <LayerCard title="Web vitals" description="p75 from real visits, in ms (CLS has no unit)." flush>
      {vitals.error ? <div className="px-4 pt-3"><ErrorText error={vitals.error} /></div> : null}
      <ul className="flex flex-col gap-0.5 p-2">
        {metrics.map((metric) => {
          const sample = rows.find((row) => row.metric === metric && row.device === "Desktop") ?? rows.find((row) => row.metric === metric);
          const value = sample?.p75 == null ? "-" : metric === "cls" ? sample.p75.toFixed(3) : Math.round(sample.p75);
          return (
            <li key={metric} className="flex h-[1.875rem] items-center justify-between rounded-[6px] px-2.5 text-[0.75rem]">
              <span className="font-medium text-[var(--ui-fg-dim)]">{metric.toUpperCase()}</span>
              <span className="font-mono text-[0.6875rem] text-[var(--ui-fg-dim)]">{value}</span>
            </li>
          );
        })}
      </ul>
    </LayerCard>
  );
}

const BREAKDOWN_ROWS = 8;

function BreakdownCard({
  siteId,
  query,
  title,
  views,
}: {
  siteId: string;
  query: string;
  title: string;
  views: { dimension: string; label: string }[];
}) {
  const [dimension, setDimension] = useState(views[0].dimension);
  const [expanded, setExpanded] = useState(false);
  const data = useQuery({
    queryKey: ["site-breakdown", siteId, dimension, query],
    queryFn: () => api<{ rows: InsightRow[] }>(`/api/sites/${siteId}/breakdown/${dimension}?${query}`),
  });
  const rows = data.data?.rows ?? [];
  return (
    <LayerCard
      title={title}
      flush
      actions={
        <div className="flex items-center gap-3">
          {views.length > 1 ? (
            <span className="inline-flex rounded-md border border-[var(--ui-control-border)] p-0.5">
              {views.map((view) => (
                <button
                  key={view.dimension}
                  type="button"
                  aria-pressed={view.dimension === dimension}
                  className={cn(
                    "h-5 rounded px-1.5 text-[0.6875rem] font-medium",
                    view.dimension === dimension ? "bg-[var(--ui-hover)] text-[var(--ui-fg)]" : "text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]",
                  )}
                  onClick={() => {
                    setDimension(view.dimension);
                    setExpanded(false);
                  }}
                >
                  {view.label}
                </button>
              ))}
            </span>
          ) : null}
          {rows.length > BREAKDOWN_ROWS ? (
            <button type="button" className="text-[0.6875rem] font-medium text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]" onClick={() => setExpanded((value) => !value)}>
              {expanded ? "Show less" : "View all"}
            </button>
          ) : null}
        </div>
      }
    >
      {data.error ? <div className="px-4 pt-3"><ErrorText error={data.error} /></div> : null}
      <BarList rows={expanded ? rows : rows.slice(0, BREAKDOWN_ROWS)} total={rows.reduce((sum, row) => sum + row.count, 0)} />
    </LayerCard>
  );
}

function EventsPanel({ siteId, query }: { siteId: string; query: string }) {
  const events = useQuery({
    queryKey: ["site-events", siteId, query],
    queryFn: () => api<{ total: number; rows: EventRow[] }>(`/api/sites/${siteId}/events?${query}`),
  });
  const rows = events.data?.rows ?? [];
  return (
    <LayerCard title="Custom events" description={`${events.data?.total ?? 0} events in this range. Call owleeEvent(name, data) from the tracker.`} flush>
      {events.error ? <div className="px-4 pt-3"><ErrorText error={events.error} /></div> : null}
      {rows.length === 0 ? <CardEmpty title="No events yet" body="Events you send from the tracker show up here." /> : (
        <DataTable columns={eventColumns} rows={rows} rowKey={(row) => row.name} />
      )}
    </LayerCard>
  );
}

type EventRow = { name: string; count: number; visitors: number };

const eventColumns: Column<EventRow>[] = [
  { key: "name", header: "Name", cellClassName: "font-medium text-[var(--ui-fg)]", sort: (row) => row.name, cell: (row) => row.name },
  { key: "count", header: "Count", className: "text-right!", cellClassName: "font-mono text-[0.75rem]", sort: (row) => row.count, cell: (row) => row.count },
  { key: "visitors", header: "Visitors", className: "text-right!", cellClassName: "font-mono text-[0.75rem]", sort: (row) => row.visitors, cell: (row) => row.visitors },
];

type VisitorRow = { id: string; views: number; events: number; last_seen: number };

function VisitorsPanel({ siteId, query }: { siteId: string; query: string }) {
  const visitors = useQuery({
    queryKey: ["site-visitors", siteId, query],
    queryFn: () => api<{ rows: VisitorRow[] }>(`/api/sites/${siteId}/visitors?${query}`),
  });
  const rows = visitors.data?.rows ?? [];
  const columns: Column<VisitorRow>[] = [
    { key: "visitor", header: "Visitor", sort: (row) => row.id, cell: (row) => <Link className="row-link font-mono text-[0.75rem]" to={`/analytics/${siteId}/visitors/${row.id}`}>{row.id}</Link> },
    { key: "views", header: "Views", className: "text-right!", cellClassName: "font-mono text-[0.75rem]", sort: (row) => row.views, cell: (row) => row.views },
    { key: "events", header: "Events", className: "text-right!", cellClassName: "font-mono text-[0.75rem]", sort: (row) => row.events, cell: (row) => row.events },
  ];
  return (
    <LayerCard title="Visitors" description="Identity resets each month. The list shows the hash, not an address." flush>
      {visitors.error ? <div className="px-4 pt-3"><ErrorText error={visitors.error} /></div> : null}
      {rows.length === 0 ? <CardEmpty title="No visitors" body="Visitors in this range show up here." /> : (
        <DataTable columns={columns} rows={rows} rowKey={(row) => row.id} rowLink={(row) => `/analytics/${siteId}/visitors/${row.id}`} />
      )}
    </LayerCard>
  );
}

export function VisitorPage() {
  const { siteId, visitorId } = useParams();
  const detail = useQuery({
    queryKey: ["visitor", siteId, visitorId],
    queryFn: () => api<{ visitor: { id: string; browser: string | null; os: string | null; country: string | null; device: string | null }; activity: VisitorActivity[] }>(`/api/sites/${siteId}/visitors/${visitorId}`),
    enabled: Boolean(siteId && visitorId),
  });
  const visitor = detail.data?.visitor;
  const activity = detail.data?.activity ?? [];
  if (detail.isLoading) {
    return (
      <div className="flex flex-col gap-6" aria-busy="true">
        <div className="space-y-2">
          <Skeleton className="h-6 w-40" />
          <Skeleton className="h-3 w-72" />
        </div>
        <LayerCard><Skeleton className="h-16 w-full" /></LayerCard>
        <LayerCard><Skeleton className="h-40 w-full" /></LayerCard>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-6">
      <PageHeader keepTitle title="Visitor" description={visitor ? `Hash ${visitor.id}` : undefined} />
      <ErrorText error={detail.error} />
      <LayerCard title="Details">
        <StatGrid className="xl:grid-cols-4">
          <Stat label="Country" value={visitor?.country ?? "-"} />
          <Stat label="Browser" value={visitor?.browser ?? "-"} />
          <Stat label="Operating system" value={visitor?.os ?? "-"} />
          <Stat label="Device" value={visitor?.device ?? "-"} />
        </StatGrid>
      </LayerCard>
      <LayerCard title="Activity" flush>
        {activity.length === 0 ? <CardEmpty title="No activity" /> : (
          <DataTable columns={visitorActivityColumns} rows={activity} rowKey={(item, index) => `${item.timestamp}-${index}`} />
        )}
      </LayerCard>
    </div>
  );
}

type VisitorActivity = { timestamp: number; type: string; path: string | null; title: string | null };

const visitorActivityColumns: Column<VisitorActivity>[] = [
  { key: "page", header: "Page", cellClassName: "font-medium text-[var(--ui-fg)]", sort: (item) => item.path ?? item.type, cell: (item) => item.path ?? item.type },
  { key: "title", header: "Title", sort: (item) => item.title, cell: (item) => item.title || "-" },
  { key: "type", header: "Type", className: "text-right!", sort: (item) => item.type, cell: (item) => item.type },
];

function VerifyRecord({ site }: { site: InsightSite }) {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const verify = useMutation({
    mutationFn: () => api<InsightSite>(`/api/sites/${site.id}/verify`, { method: "POST" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["sites"] }),
  });
  return (
    <>
      <CodeBlock file="TXT name" code={site.verify_name} />
      <CodeBlock file="TXT value" code={site.verify_value} />
      <ZoneDownload hostname={site.domain} records={[{ name: site.verify_name, type: "TXT", value: site.verify_value }]} />
      {canEdit ? (
        <div>
          <Button variant="primary" type="button" disabled={verify.isPending} onClick={() => verify.mutate()}>
            {verify.isPending ? <><Spinner />Verifying…</> : "Verify"}
          </Button>
        </div>
      ) : null}
      <ErrorText error={verify.error} />
    </>
  );
}

const trackerVariants = [
  { id: "html", label: "HTML", file: "index.html", hint: "Paste the tag into the <head> of every page.", code: (tag: string) => tag },
  {
    id: "next",
    label: "Next.js",
    file: "app/layout.tsx",
    hint: "Add the Script to the root layout, inside <body>. Client-side navigation counts as page views.",
    code: (_tag: string, site: InsightSite) =>
      `import Script from "next/script";\n\n<Script src="${site.tracker_url}" data-site="${site.public_id}" strategy="afterInteractive" />`,
  },
  {
    id: "other",
    label: "Other frameworks",
    file: undefined,
    hint: "Put the tag in the layout or template that renders <head> on every page, such as a base layout or app.html.",
    code: (tag: string) => tag,
  },
] as const;
type TrackerVariant = (typeof trackerVariants)[number]["id"];

function SetupPanel({ site, sawWaiting, overviewLink }: { site: InsightSite; sawWaiting: boolean; overviewLink: string }) {
  const [variantId, setVariantId] = useState<TrackerVariant>("html");
  const variant = trackerVariants.find((item) => item.id === variantId)!;
  const canEdit = useCanEdit();
  const tag = `<script defer src="${site.tracker_url}" data-site="${site.public_id}"></script>`;
  const last = site.last_event_at;
  return (
    <LayerCard title="Set up the tracker" description="Three steps. The tracker sets no cookies.">
      <ol className="flex flex-col gap-6">
        <GuideStep number={1} title="Verify the domain">
          {site.verified ? (
            <p className="text-[0.8125rem] text-[var(--ui-fg-dim)]"><StatusBadge tone="success">Verified</StatusBadge> {site.domain} is verified.</p>
          ) : (
            <>
              <p className="text-[0.8125rem] text-[var(--ui-fg-dim)]">
                {canEdit ? "Add this TXT record at your DNS host, then verify." : "An editor adds this TXT record at the DNS host and verifies the domain."}
                {site.ingest_open ? " This server runs in development, so it accepts events before the domain verifies." : " Events are dropped until it verifies."}
              </p>
              <VerifyRecord site={site} />
            </>
          )}
        </GuideStep>
        <GuideStep number={2} title="Add the tracker">
          <span className="inline-flex self-start rounded-md border border-[var(--ui-control-border)] p-0.5" role="group" aria-label="Framework">
            {trackerVariants.map((item) => (
              <button
                key={item.id}
                type="button"
                aria-pressed={item.id === variantId}
                className={cn(
                  "h-6 rounded px-2 text-[0.75rem] font-medium",
                  item.id === variantId ? "bg-[var(--ui-hover)] text-[var(--ui-fg)]" : "text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]",
                )}
                onClick={() => setVariantId(item.id)}
              >
                {item.label}
              </button>
            ))}
          </span>
          <p className="text-[0.8125rem] text-[var(--ui-fg-dim)]">{variant.hint}</p>
          <CodeBlock file={variant.file} code={variant.code(tag, site)} />
          <CodeBlock file="Optional: custom events" code={`owleeEvent("signup", { plan: "hobby" })`} />
        </GuideStep>
        <GuideStep number={3} title="Wait for the first page view">
          {last == null ? (
            <div className="flex flex-wrap items-center gap-3 rounded-lg border border-[var(--ui-control-border)] px-4 py-3 text-[0.8125rem]">
              <StatusBadge tone="info">Listening</StatusBadge>
              <p className="min-w-0 flex-1 text-[var(--ui-fg-dim)]">Open {site.domain} in a browser. This page checks every 3 seconds.</p>
            </div>
          ) : (
            <div className="flex flex-wrap items-center gap-3 rounded-lg border border-[var(--ui-control-border)] px-4 py-3 text-[0.8125rem]">
              <StatusBadge tone="success">{sawWaiting ? "First page view received" : "Receiving page views"}</StatusBadge>
              <p className="min-w-0 flex-1 text-[var(--ui-fg-dim)]">
                Last event <time dateTime={epochIso(last)} title={fullTime(epochIso(last))}>{timeAgo(epochIso(last))}</time>.
              </p>
              <Link className="button button-highlighted h-7 px-2 text-xs" to={overviewLink}>Open Overview</Link>
            </div>
          )}
        </GuideStep>
      </ol>
    </LayerCard>
  );
}

function SettingsPanel({ site }: { site: InsightSite }) {
  return (
    <div className="grid items-start gap-6 lg:grid-cols-2">
      <NameCard key={site.name} site={site} />
      <LayerCard
        title="Domain verification"
        description="Owlee re-checks verified domains about once an hour."
        actions={<StatusBadge tone={site.verified ? "success" : "warning"}>{site.verified ? "Verified" : "Unverified"}</StatusBadge>}
      >
        <div className="flex flex-col gap-3">
          <p className="text-[0.8125rem] text-[var(--ui-fg-dim)]">
            {site.verified
              ? `${site.domain} is verified.`
              : site.ingest_open
                ? "This server runs in development, so it accepts events before the domain verifies. Production drops them."
                : "Events are dropped until the domain verifies."}
          </p>
          <VerifyRecord site={site} />
        </div>
      </LayerCard>
      <SiteQuota key={String(site.quota_override ?? "inherit")} site={site} />
      <PublicCard site={site} />
      <DangerCard site={site} />
    </div>
  );
}

function NameCard({ site }: { site: InsightSite }) {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const [name, setName] = useState(site.name);
  const save = useMutation({
    mutationFn: () => api<InsightSite>(`/api/sites/${site.id}`, { method: "PUT", body: JSON.stringify({ name, domain: site.domain, color: site.color }) }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["sites"] }),
  });
  return (
    <LayerCard title="Site" description={`Tracks ${site.domain}.`}>
      <form className="flex flex-col gap-3 sm:flex-row sm:items-end" onSubmit={(event) => { event.preventDefault(); save.mutate(); }}>
        <div className="sm:flex-1">
          <Field label="Name"><Input required disabled={!canEdit} value={name} onChange={(event) => setName(event.target.value)} /></Field>
        </div>
        {canEdit ? <Button variant="primary" type="submit" disabled={save.isPending || name.trim() === site.name}>Rename</Button> : null}
      </form>
      <ErrorText error={save.error} />
    </LayerCard>
  );
}

function DangerCard({ site }: { site: InsightSite }) {
  const client = useQueryClient();
  const navigate = useNavigate();
  const canEdit = useCanEdit();
  const [confirming, setConfirming] = useState<"reset" | "delete" | null>(null);
  const reset = useMutation({
    mutationFn: () => api(`/api/sites/${site.id}/analytics`, { method: "DELETE" }),
    onSuccess: async () => {
      setConfirming(null);
      await client.invalidateQueries({ predicate: (query) => String(query.queryKey[0]).startsWith("site") });
    },
  });
  const remove = useMutation({
    mutationFn: () => api(`/api/sites/${site.id}`, { method: "DELETE" }),
    onSuccess: async () => {
      navigate("/analytics");
      await client.invalidateQueries({ queryKey: ["sites"] });
    },
  });
  if (!canEdit) return null;
  return (
    <LayerCard title="Danger zone" description="Neither can be undone.">
      <div className="flex flex-wrap gap-3">
        <Button variant="danger" onClick={() => setConfirming("reset")}>Reset data</Button>
        <Button variant="danger" onClick={() => setConfirming("delete")}>Delete site</Button>
      </div>
      <ConfirmDialog
        open={confirming === "reset"}
        title="Reset data?"
        description={`Every page view, event, and visitor of ${site.name} is deleted. The site and its tracker keep working. This cannot be undone.`}
        confirmLabel="Reset data"
        pending={reset.isPending}
        error={reset.error}
        onConfirm={() => reset.mutate()}
        onClose={() => setConfirming(null)}
      />
      <ConfirmDialog
        open={confirming === "delete"}
        title="Delete site?"
        description={`${site.name} and all its data are deleted. The tracker on ${site.domain} is rejected. This cannot be undone.`}
        confirmLabel="Delete site"
        pending={remove.isPending}
        error={remove.error}
        onConfirm={() => remove.mutate()}
        onClose={() => setConfirming(null)}
      />
    </LayerCard>
  );
}

function resetWhen(iso: string) {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "00:00 UTC";
  return `${date.toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", timeZone: "UTC" })} UTC`;
}

function SiteQuota({ site }: { site: InsightSite }) {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const [value, setValue] = useState(site.quota_override == null ? "" : String(site.quota_override));
  const save = useMutation({
    mutationFn: (daily: number | null) =>
      api<InsightSite>(`/api/sites/${site.id}/quota`, {
        method: "PUT",
        body: JSON.stringify({ daily_quota: daily }),
      }),
    onSuccess: async (data) => {
      setValue(data.quota_override == null ? "" : String(data.quota_override));
      await client.invalidateQueries({ queryKey: ["sites"] });
      await client.invalidateQueries({ queryKey: ["dashboard"] });
    },
  });
  return (
    <LayerCard title="Daily cap" description="A safety valve against floods, not a bill.">
      <p className={`mb-3 text-[0.8125rem] ${site.quota_exceeded ? "text-[var(--ui-error)]" : "text-[var(--ui-fg-faint)]"}`}>
        {site.used_today.toLocaleString()} of {site.daily_quota.toLocaleString()} events today
        {site.quota_override == null ? " (instance default)" : " (this site)"}
        {site.quota_exceeded ? ". Further events are rejected until the counter resets." : "."} Resets {resetWhen(site.resets_at)}.
      </p>
      <p className="mb-3 text-[13px] text-[var(--ui-fg-faint)]">{intakeSummary(site.intake)}</p>
      {canEdit ? <form
        className="flex flex-col gap-3 sm:flex-row sm:items-end"
        onSubmit={(event) => {
          event.preventDefault();
          const trimmed = value.trim();
          save.mutate(trimmed === "" ? null : Number(trimmed));
        }}
      >
        <div className="sm:w-56">
          <Field label="Events per day">
            <Input inputMode="numeric" placeholder="Inherit" value={value} onChange={(event) => setValue(event.target.value)} />
          </Field>
        </div>
        <Button variant="primary" type="submit" disabled={save.isPending}>Save cap</Button>
        {site.quota_override != null ? (
          <Button type="button" disabled={save.isPending} onClick={() => save.mutate(null)}>Inherit</Button>
        ) : null}
      </form> : null}
      <ErrorText error={save.error} />
    </LayerCard>
  );
}

function PublicCard({ site }: { site: InsightSite }) {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const [enabled, setEnabled] = useState(site.public_enabled);
  const save = useMutation({
    mutationFn: () =>
      api(`/api/sites/${site.id}/public`, {
        method: "PUT",
        body: JSON.stringify({
          enabled,
          slug: site.public_slug,
          threshold: site.public_threshold,
          sections: site.public_sections,
        }),
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["sites"] }),
  });
  return (
    <LayerCard title="Public dashboard" description="Share aggregate numbers without a login.">
      <form className="space-y-4" onSubmit={(event: FormEvent) => { event.preventDefault(); save.mutate(); }}>
        <label className="flex items-center gap-2 text-[0.8125rem] font-medium text-[var(--ui-fg-dim)]">
          <input type="checkbox" className="accent-[var(--ui-accent)]" disabled={!canEdit} checked={enabled} onChange={(event) => setEnabled(event.target.checked)} />
          Public page enabled
        </label>
        {site.public_enabled ? (
          <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">
            Share path <Link className="auth-text-link font-mono text-[0.75rem]" to={`/share/sites/${site.public_slug}`}>/share/sites/{site.public_slug}</Link>
          </p>
        ) : null}
        <ErrorText error={save.error} />
        {canEdit ? <Button type="submit" variant="primary" disabled={save.isPending}>Save sharing</Button> : null}
      </form>
    </LayerCard>
  );
}
