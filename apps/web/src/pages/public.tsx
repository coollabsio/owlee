import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useParams } from "react-router";
import { UptimeBars } from "@/components/charts";
import { Button, Empty, LayerCard, LoadMore, Skeleton, StatusBadge } from "@/components/ui";
import { api, flattenPages, withCursor, type Paged, type PublicIncident, type PublicPage } from "@/lib/api";
import { formatTime, percent, stateLabel } from "@/lib/utils";

type Tone = "neutral" | "success" | "warning" | "error" | "info";

const bannerCopy: Record<PublicPage["banner"], { title: string; label: string; tone: Tone }> = {
  operational: { title: "All systems operational", label: "Operational", tone: "success" },
  degraded: { title: "Degraded performance", label: "Degraded", tone: "warning" },
  partial_outage: { title: "Partial system outage", label: "Partial outage", tone: "warning" },
  major_outage: { title: "Major outage", label: "Major outage", tone: "error" },
};

function tone(state: string): Tone {
  if (state === "up" || state === "success" || state === "resolved" || state === "operational") return "success";
  if (state === "degraded" || state === "minor" || state === "partial_outage") return "warning";
  if (state === "down" || state === "failure" || state === "major" || state === "critical" || state === "major_outage") return "error";
  if (state === "maintenance" || state === "monitoring" || state === "identified") return "info";
  return "neutral";
}

export function PublicStatusPage() {
  const { slug = "" } = useParams();
  const query = useQuery({
    queryKey: ["public", slug],
    queryFn: () => api<PublicPage>(`/api/public/${slug}`),
    refetchInterval: 15_000,
  });
  // Older history loads on request, from where the page's first slice stops.
  const [wantOlder, setWantOlder] = useState(false);
  const olderFrom = query.data?.incidents_next_cursor ?? "";
  const older = useInfiniteQuery({
    queryKey: ["public-incidents", slug, olderFrom],
    queryFn: ({ pageParam }) => api<Paged<PublicIncident>>(withCursor(`/api/public/${slug}/incidents`, pageParam)),
    initialPageParam: olderFrom,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: wantOlder && Boolean(olderFrom),
  });
  if (query.isLoading) {
    return (
      <div className="min-h-svh bg-[var(--ui-canvas)]" aria-busy="true">
        <div className="mx-auto flex max-w-3xl flex-col gap-6 px-4 py-12 sm:py-16">
          <div className="space-y-2">
            <Skeleton className="h-7 w-48" />
            <Skeleton className="h-3 w-72" />
          </div>
          <LayerCard><Skeleton className="h-8 w-56" /></LayerCard>
          <LayerCard><Skeleton className="h-40 w-full" /></LayerCard>
        </div>
      </div>
    );
  }
  // A failed refetch keeps the last page on screen.
  if (!query.data) {
    return (
      <div className="grid min-h-svh place-items-center bg-[var(--ui-canvas)] px-4">
        <div className="w-full max-w-md">
          <Empty title="Status page not found" body="Check the slug, or publish the page from the dashboard." />
        </div>
      </div>
    );
  }
  const page = query.data;
  const banner = bannerCopy[page.banner];
  const active = page.open_incidents;
  const history = flattenPages([page.incidents, ...(older.data?.pages.map((slice) => slice.items) ?? [])]).filter(
    (incident) => incident.status === "resolved",
  );
  const loadOlder = wantOlder
    ? { hasNextPage: older.isLoading || older.hasNextPage, isFetchingNextPage: older.isFetching, fetchNextPage: older.fetchNextPage }
    : { hasNextPage: Boolean(olderFrom), isFetchingNextPage: false, fetchNextPage: () => setWantOlder(true) };
  const maintenances = page.maintenances.filter((item) => item.active || new Date(item.starts_at).getTime() > Date.now());
  return (
    <div className="min-h-svh bg-[var(--ui-canvas)] text-[var(--ui-fg)]">
      <div className="mx-auto flex max-w-3xl flex-col gap-6 px-4 py-12 sm:py-16">
        <header className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <h1 className="text-[1.5rem] font-semibold leading-8 tracking-tight">{page.title}</h1>
            {page.description ? <p className="mt-1 text-[0.8125rem] text-[var(--ui-fg-faint)]">{page.description}</p> : null}
          </div>
          {page.homepage_url ? (
            <a className="shrink-0" href={page.homepage_url}><Button type="button">Homepage</Button></a>
          ) : null}
        </header>
        <LayerCard title="Current status" actions={<StatusBadge tone={banner.tone}>{banner.label}</StatusBadge>}>
          <p className="text-[1.125rem] font-semibold text-[var(--ui-fg)]">{banner.title}</p>
        </LayerCard>
        {maintenances.length > 0 ? (
          <LayerCard title="Maintenance" flush>
            <ul>
              {maintenances.map((item) => (
                <li key={item.id} className="border-t border-[var(--ui-divider)] px-4 py-3 first:border-t-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="text-[0.8125rem] font-medium text-[var(--ui-fg)]">{item.title}</span>
                    <StatusBadge tone={item.active ? "info" : "neutral"}>{item.active ? "In progress" : "Scheduled"}</StatusBadge>
                  </div>
                  {item.message ? <p className="mt-1 text-[0.8125rem] text-[var(--ui-fg-dim)]">{item.message}</p> : null}
                  <p className="mt-1 text-[0.6875rem] text-[var(--ui-fg-faint)]">{formatTime(item.starts_at)} - {formatTime(item.ends_at)}</p>
                </li>
              ))}
            </ul>
          </LayerCard>
        ) : null}
        <LayerCard title="Components" description="Uptime over the last 90 days." flush>
          {page.components.length === 0 ? (
            <p className="px-4 py-10 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">No components on this page yet.</p>
          ) : (
            <ul>
              {page.components.map((component) => (
                <li key={component.id} className="border-t border-[var(--ui-divider)] p-4 first:border-t-0">
                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <div className="text-[0.8125rem] font-medium text-[var(--ui-fg)]">{component.name}</div>
                      {component.description ? <p className="text-[0.75rem] text-[var(--ui-fg-faint)]">{component.description}</p> : null}
                    </div>
                    <div className="flex shrink-0 flex-col items-end gap-1">
                      <StatusBadge tone={tone(component.state)}>{stateLabel[component.state]}</StatusBadge>
                      <div className="text-[0.6875rem] text-[var(--ui-fg-faint)]">{percent(component.uptime_90d)} uptime</div>
                    </div>
                  </div>
                  <UptimeBars bars={component.bars} className="mt-3" />
                  <div className="mt-1 flex justify-between text-[0.6875rem] text-[var(--ui-fg-faint)]">
                    <span>90 days ago</span>
                    <span>Today</span>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </LayerCard>
        <LayerCard title="Incidents" flush>
          {active.length === 0 && history.length === 0 ? (
            <div className="px-4 py-10 text-center">
              <p className="text-[0.875rem] font-medium text-[var(--ui-fg)]">No incidents reported</p>
              <p className="mt-1 text-[0.8125rem] text-[var(--ui-fg-faint)]">Incidents and their updates show up here.</p>
            </div>
          ) : (
            <ul>
              {[...active, ...history].map((incident) => (
                <li key={incident.id} className="border-t border-[var(--ui-divider)] p-4 first:border-t-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <h3 className="text-[0.8125rem] font-medium text-[var(--ui-fg)]">{incident.title}</h3>
                    <StatusBadge tone={tone(incident.status)}>{stateLabel[incident.status]}</StatusBadge>
                    <span className="text-[0.6875rem] text-[var(--ui-fg-faint)]">{formatTime(incident.started_at)}</span>
                  </div>
                  <ol className="mt-3 flex flex-col gap-3 border-l border-[var(--ui-divider)] pl-4">
                    {[...incident.updates].reverse().map((update) => (
                      <li key={update.id}>
                        <div className="text-[0.6875rem] text-[var(--ui-fg-faint)]">{stateLabel[update.status]} · {formatTime(update.created_at)}</div>
                        <p className="text-[0.8125rem] text-[var(--ui-fg-dim)]">{update.message}</p>
                      </li>
                    ))}
                  </ol>
                </li>
              ))}
            </ul>
          )}
          <LoadMore query={loadOlder} label="Older incidents" />
        </LayerCard>
        <footer className="pt-4 text-center text-[0.6875rem] text-[var(--ui-fg-faint)]">
          <a href="https://owlee.io" className="underline decoration-[var(--ui-fg-faint)] underline-offset-2 hover:text-[var(--ui-fg)]">Powered by Owlee</a>
        </footer>
      </div>
    </div>
  );
}
