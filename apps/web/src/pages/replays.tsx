import { keepPreviousData, useInfiniteQuery, useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { type Column, DataTable } from "@/components/data-table";
import { Button, ConfirmDialog, ErrorText, Field, Input, LayerCard, LoadMore, PageHeader, Skeleton, Spinner, StatusBadge, Switch } from "@/components/ui";
import { api, ApiError, flattenPages, withCursor, type ErrorApp } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import {
  bytesToMb,
  clock,
  formatBytes,
  type FromPlayer,
  mbToBytes,
  mergeSegments,
  REPLAY_PRIVACY_TEXT,
  type ReplayDetail,
  type ReplayPage as ReplayList,
  type ReplaySummary,
  replayTimeline,
  type RrwebEvent,
  type TimelineItem,
  type ToPlayer,
} from "@/lib/replay";
import { cn, fullTime, timeAgo } from "@/lib/utils";
import { useDebounced } from "@/pages/issue-detail";

const iso = (ms: number) => new Date(ms).toISOString();
const replayPath = (appId: string, replayId: string) => `/errors/apps/${appId}/replays/${replayId}`;

const replayColumns = (appId: string): Column<ReplaySummary>[] => [
  {
    key: "started",
    header: "Started",
    cellClassName: "whitespace-nowrap",
    cell: (row) => {
      const at = iso(row.startedMs ?? row.receivedMs);
      return (
        <span className="flex items-center gap-2" title={fullTime(at)}>
          <Link className="row-link" to={replayPath(appId, row.id)}>{timeAgo(at)}</Link>
          {row.live ? <StatusBadge tone="success">Live</StatusBadge> : null}
          {row.hasStart ? null : <StatusBadge tone="warning">Incomplete</StatusBadge>}
        </span>
      );
    },
  },
  {
    key: "duration",
    header: "Length",
    className: "text-right",
    cellClassName: "tabular-nums",
    cell: (row) => (row.durationMs == null ? "-" : clock(row.durationMs)),
  },
  {
    key: "url",
    header: "First page",
    cellClassName: "max-w-[22rem]",
    cell: (row) => (
      <span className="flex min-w-0 items-center gap-2">
        <span className="truncate" title={row.firstUrl ?? undefined}>{row.firstUrl ?? "-"}</span>
        {row.urlCount > 1 ? <span className="shrink-0 text-[var(--ui-fg-faint)]">+{row.urlCount - 1} more</span> : null}
      </span>
    ),
  },
  { key: "user", header: "User", cellClassName: "break-all", cell: (row) => row.userLabel ?? "-" },
  {
    key: "browser",
    header: "Browser",
    cell: (row) => [row.browser, row.os].filter(Boolean).join(" on ") || "-",
  },
  { key: "release", header: "Release", cellClassName: "break-all", cell: (row) => row.release ?? "-" },
  {
    key: "errors",
    header: "Errors",
    className: "text-right",
    cellClassName: "tabular-nums",
    cell: (row) => (row.errorCount > 0 ? <span className="text-[var(--ui-error)]">{row.errorCount}</span> : "0"),
  },
];

/** The replay list on the app page. */
export function ReplaysCard({ app }: { app: ErrorApp }) {
  const [url, setUrl] = useState("");
  const [release, setRelease] = useState("");
  const [environment, setEnvironment] = useState("");
  const [hasErrors, setHasErrors] = useState(false);
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries({ url, release, environment })) if (value.trim()) params.set(key, value.trim());
  if (hasErrors) params.set("hasErrors", "true");
  const query = useDebounced(params.toString(), 300);
  const off = !app.replays.enabled;
  const list = useInfiniteQuery({
    queryKey: ["replays", app.id, query],
    queryFn: ({ pageParam }) => api<ReplayList>(withCursor(`/api/error-apps/${app.id}/replays${query ? `?${query}` : ""}`, pageParam)),
    initialPageParam: "",
    getNextPageParam: (page) => page.nextCursor ?? undefined,
    placeholderData: keepPreviousData,
    enabled: !off || app.replays.totalBytes > 0,
  });
  const rows = flattenPages(list.data?.pages.map((page) => page.items));

  if (off && app.replays.totalBytes === 0) {
    return (
      <LayerCard title="Replays" description="Recordings of what users did on the page before an error." actions={<StatusBadge>Off</StatusBadge>}>
        <div className="flex flex-col gap-2 text-[0.8125rem] text-[var(--ui-fg-faint)]">
          <p>Replays are off for this app. An editor can turn them on under Session replay below, then add the Replay integration to the browser SDK.</p>
          <p>{REPLAY_PRIVACY_TEXT}</p>
        </div>
      </LayerCard>
    );
  }

  return (
    <LayerCard
      title="Replays"
      description={off ? "Replays are off. New recordings are not stored. Stored ones stay until they expire or you delete them." : "Newest first."}
      actions={off ? <StatusBadge>Off</StatusBadge> : null}
      flush
    >
      <div className="flex flex-wrap items-center gap-2 border-b border-[var(--ui-divider)] p-2">
        <div className="w-56">
          <Input placeholder="Page URL contains" aria-label="Page URL contains" value={url} onChange={(event) => setUrl(event.target.value)} />
        </div>
        <div className="w-36">
          <Input placeholder="Release" aria-label="Release" value={release} onChange={(event) => setRelease(event.target.value)} />
        </div>
        <div className="w-36">
          <Input placeholder="Environment" aria-label="Environment" value={environment} onChange={(event) => setEnvironment(event.target.value)} />
        </div>
        <label className="flex items-center gap-1.5 px-1 text-[0.75rem] text-[var(--ui-fg-faint)]">
          <input type="checkbox" checked={hasErrors} onChange={(event) => setHasErrors(event.target.checked)} />
          With errors only
        </label>
      </div>
      {list.isError ? (
        <div className="p-4"><ErrorText error={list.error} /></div>
      ) : !list.isLoading && rows.length === 0 ? (
        <p className="p-6 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">
          {query ? "No replays match these filters." : "No replays yet. They show up here a few seconds after a recorded page sends its first part."}
        </p>
      ) : (
        <>
          <DataTable columns={replayColumns(app.id)} rows={rows} rowKey={(row) => row.id} rowLink={(row) => replayPath(app.id, row.id)} loading={list.isLoading} />
          <LoadMore query={list} label="Load older replays" />
        </>
      )}
    </LayerCard>
  );
}

/** Replay switch and limits on the app page. */
export function ReplaySettingsCard({ app }: { app: ErrorApp }) {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const settings = app.replays;
  const [enabled, setEnabled] = useState(settings.enabled);
  const [dailyMb, setDailyMb] = useState(String(bytesToMb(settings.dailyBytes)));
  const [hourly, setHourly] = useState(String(settings.hourlyLimit));
  const save = useMutation({
    mutationFn: () =>
      api<ErrorApp>(`/api/error-apps/${app.id}/replays-settings`, {
        method: "PUT",
        body: JSON.stringify({ enabled, dailyBytes: mbToBytes(Number(dailyMb)), hourlyLimit: Number(hourly) }),
      }),
    onSuccess: async (data) => {
      client.setQueryData(["app", app.id], data);
      await client.invalidateQueries({ queryKey: ["replays", app.id] });
    },
  });
  const fields = save.error instanceof ApiError ? (save.error.fields ?? {}) : {};
  const dailyError = fields.daily_bytes ?? fields.dailyBytes;
  const hourlyError = fields.hourly_limit ?? fields.hourlyLimit;
  return (
    <LayerCard title="Session replay" description="Stores recordings that the browser SDK sends. Off until an editor turns it on.">
      <form
        className="flex flex-col gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <div className="flex items-center justify-between gap-4">
          <div>
            <p className="text-[0.8125rem] text-[var(--ui-fg)]">Store replays</p>
            <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">{REPLAY_PRIVACY_TEXT}</p>
          </div>
          <Switch aria-label="Store replays" checked={enabled} disabled={!canEdit} onCheckedChange={setEnabled} />
        </div>
        <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">
          {formatBytes(settings.todayBytes)} stored today. {settings.hourReplays.toLocaleString()} new {settings.hourReplays === 1 ? "replay" : "replays"} this hour.{" "}
          {formatBytes(settings.totalBytes)} stored in total.
        </p>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Storage per day (MB)" hint="1 MB to 100 GB" error={dailyError}>
            <Input inputMode="decimal" value={dailyMb} readOnly={!canEdit} onChange={(event) => setDailyMb(event.target.value)} />
          </Field>
          <Field label="New replays per hour" hint="1 to 100,000" error={hourlyError}>
            <Input inputMode="numeric" value={hourly} readOnly={!canEdit} onChange={(event) => setHourly(event.target.value)} />
          </Field>
        </div>
        {canEdit ? (
          <div>
            <Button variant="highlighted" type="submit" disabled={save.isPending}>{save.isPending ? <><Spinner />Saving…</> : "Save replay settings"}</Button>
          </div>
        ) : null}
        {save.error && !dailyError && !hourlyError ? <ErrorText error={save.error} /> : null}
      </form>
    </LayerCard>
  );
}

/** Fetches every listed segment once. `ready` is the leading run of segments that have arrived, in segment order. */
function useSegments(base: string, segments: number[]) {
  return useQueries({
    queries: segments.map((segment) => ({
      queryKey: ["replay-segment", base, segment],
      queryFn: () => api<RrwebEvent[]>(`${base}/segments/${segment}`),
      staleTime: Infinity,
    })),
    combine: readySegments,
  });
}

// Module level, so React Query reruns it only when a result changes and `ready` keeps its identity.
function readySegments(results: { data?: RrwebEvent[]; error: unknown }[]) {
  const firstMissing = results.findIndex((result) => !result.data);
  const ready = (firstMissing === -1 ? results : results.slice(0, firstMissing)).map((result) => result.data!);
  return { ready, error: results.find((result) => result.error)?.error ?? null, loading: firstMissing !== -1 };
}

type Seek = { timeMs: number; key: number };

/* Owns the postMessage link to /replay-player.html. Accepts only messages from its own frame on this origin. */
function ReplayPlayer({ segments, seek, onState }: { segments: RrwebEvent[][]; seek: Seek | null; onState: (timeMs: number, playing: boolean) => void }) {
  const frame = useRef<HTMLIFrameElement>(null);
  const ready = useRef(false);
  const posted = useRef(0);
  const seeked = useRef<number | null>(null);
  const latest = useRef({ segments, seek, onState });
  latest.current = { segments, seek, onState };

  function send(message: ToPlayer) {
    frame.current?.contentWindow?.postMessage(message, location.origin);
  }
  function flush() {
    const { segments: all, seek: wanted } = latest.current;
    if (!ready.current || all.length === 0) return;
    if (posted.current === 0) send({ type: "load", events: mergeSegments(all) });
    else if (all.length > posted.current) send({ type: "append", events: mergeSegments(all.slice(posted.current)) });
    posted.current = all.length;
    if (wanted && seeked.current !== wanted.key) {
      seeked.current = wanted.key;
      send({ type: "seek", timeMs: wanted.timeMs });
    }
  }

  useEffect(() => {
    function receive(event: MessageEvent) {
      if (!frame.current || event.source !== frame.current.contentWindow || event.origin !== location.origin) return;
      const message = event.data as FromPlayer;
      if (message.type === "ready") {
        ready.current = true;
        posted.current = 0;
        seeked.current = null;
        flush();
      } else if (message.type === "state") {
        latest.current.onState(message.timeMs, message.playing);
      }
    }
    window.addEventListener("message", receive);
    return () => window.removeEventListener("message", receive);
  }, []);
  useEffect(flush, [segments, seek]);

  return <iframe ref={frame} src="/replay-player.html" title="Replay player" className="block h-[600px] w-full rounded-b-lg bg-black" />;
}

const kindLabel: Record<TimelineItem["kind"], string> = { error: "Error", breadcrumb: "Event", navigation: "Page" };

export function ReplayPage() {
  const { id = "", replayId = "" } = useParams();
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const canEdit = useCanEdit();
  const client = useQueryClient();
  const base = `/api/error-apps/${id}/replays/${replayId}`;
  const app = useQuery({ queryKey: ["app", id], queryFn: () => api<ErrorApp>(`/api/error-apps/${id}`) });
  // React Query skips interval refetches while the tab is hidden.
  const detail = useQuery({
    queryKey: ["replay", id, replayId],
    queryFn: () => api<ReplayDetail>(base),
    refetchInterval: (query) => (query.state.data?.live ? 10_000 : false),
  });
  const playable = detail.data?.hasStart === true;
  const segments = useSegments(base, playable ? detail.data!.segments : []);
  const [seek, setSeek] = useState<Seek | null>(null);
  const [now, setNow] = useState({ timeMs: 0, playing: false });
  const [confirming, setConfirming] = useState(false);
  const remove = useMutation({
    mutationFn: () => api(base, { method: "DELETE" }),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["replays", id] });
      client.removeQueries({ queryKey: ["replay", id, replayId] });
      navigate(`/errors/apps/${id}`);
    },
  });

  const events = useMemo(() => mergeSegments(segments.ready), [segments.ready]);
  // Player time 0 is the first recorded event.
  const origin = events[0]?.timestamp ?? detail.data?.startedMs ?? 0;
  const total = Math.max(1, (events.at(-1)?.timestamp ?? origin) - origin);
  const errors = detail.data?.errors;
  const timeline = useMemo(() => (errors ? replayTimeline(events, errors) : []), [events, errors]);
  const startAt = Number(searchParams.get("t"));
  const hasStartAt = searchParams.has("t") && Number.isFinite(startAt);
  const startedMs = detail.data?.startedMs ?? null;
  const loaded = events.length > 0;

  useEffect(() => {
    if (!loaded || !hasStartAt) return;
    setSeek({ timeMs: Math.max(0, (startedMs ?? origin) + startAt - origin), key: 0 });
  }, [loaded]); // Once, when the first events arrive.

  if (detail.isLoading) {
    return (
      <div className="flex flex-col gap-6" aria-busy="true">
        <Skeleton className="h-6 w-80" />
        <LayerCard><Skeleton className="h-[600px] w-full" /></LayerCard>
      </div>
    );
  }
  if (detail.isError || !detail.data) {
    return <p className="text-[0.8125rem] text-[var(--ui-error)]">{(detail.error as Error)?.message || "Replay not found"}</p>;
  }
  const data = detail.data;
  const jump = (atMs: number) => setSeek({ timeMs: Math.max(0, atMs - origin), key: Date.now() });
  const currentAt = origin + now.timeMs;
  const facts = [
    data.startedMs != null ? fullTime(iso(data.startedMs)) : null,
    data.durationMs != null ? clock(data.durationMs) : null,
    data.userLabel,
    [data.browser, data.os].filter(Boolean).join(" on ") || null,
    data.release ? `Release ${data.release}` : null,
    data.environment,
  ].filter(Boolean);

  return (
    <div className="flex flex-col gap-4">
      <div>
        <nav className="mb-2 flex flex-wrap items-center gap-2 text-[0.8125rem] text-[var(--ui-fg-faint)]">
          <Link to="/errors/apps">Apps</Link>
          <span>/</span>
          <Link to={`/errors/apps/${id}`}>{app.data?.name ?? "App"}</Link>
          <span>/</span>
          <span className="text-[var(--ui-fg-dim)]">Replay</span>
        </nav>
        <PageHeader
          title={data.firstUrl ?? "Replay"}
          keepTitle
          description={facts.join(" · ")}
          actions={canEdit ? <Button variant="danger" onClick={() => setConfirming(true)}>Delete replay</Button> : null}
        />
        <div className="-mt-3 flex flex-wrap gap-2">
          {data.live ? <StatusBadge tone="success">Live: new segments may arrive</StatusBadge> : null}
          {data.hasStart ? null : <StatusBadge tone="warning">Incomplete: the start of this replay was not stored</StatusBadge>}
        </div>
      </div>

      <div className="grid min-w-0 gap-4 xl:grid-cols-[minmax(0,1fr)_320px]">
        <div className="flex min-w-0 flex-col gap-4">
          {playable ? (
            <LayerCard flush>
              <ReplayPlayer segments={segments.ready} seek={seek} onState={(timeMs, playing) => setNow({ timeMs, playing })} />
              <div className="border-t border-[var(--ui-divider)] px-4 py-3">
                <div className="relative h-6" aria-label="Errors on the timeline">
                  <div className="absolute inset-x-0 top-1/2 h-1 -translate-y-1/2 rounded-full bg-[var(--ui-hover)]" />
                  <div
                    className="absolute top-1/2 h-1 -translate-y-1/2 rounded-full bg-[var(--ui-accent)]"
                    style={{ width: `${Math.min(100, (now.timeMs / total) * 100)}%` }}
                  />
                  {data.errors.map((error) => (
                    <button
                      key={error.eventId}
                      type="button"
                      title={`${clock(error.timestampMs - origin)} ${error.title}`}
                      aria-label={`Go to error at ${clock(error.timestampMs - origin)}`}
                      className="absolute top-0 h-6 w-1.5 -translate-x-1/2 rounded-sm bg-[var(--ui-error)] hover:scale-x-150"
                      style={{ left: `${Math.min(100, Math.max(0, ((error.timestampMs - origin) / total) * 100))}%` }}
                      onClick={() => jump(error.timestampMs)}
                    />
                  ))}
                </div>
                {segments.loading ? (
                  <p className="mt-2 text-[0.75rem] text-[var(--ui-fg-faint)]">
                    <Spinner /> Loading {segments.ready.length} of {data.segments.length} parts…
                  </p>
                ) : null}
                <ErrorText error={segments.error} />
              </div>
            </LayerCard>
          ) : (
            <LayerCard>
              <p className="py-10 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">
                Incomplete: the start of this replay was not stored, so it cannot be played. The errors and pages below are still listed.
              </p>
            </LayerCard>
          )}
          <LayerCard title="Pages" description={`${data.urls.length} ${data.urls.length === 1 ? "page" : "pages"} visited`}>
            <ul className="flex flex-col gap-1 text-[0.8125rem]">
              {data.urls.map((url) => (
                <li key={url} className="break-all text-[var(--ui-fg-dim)]">{url}</li>
              ))}
            </ul>
          </LayerCard>
        </div>

        <aside className="flex flex-col gap-4 xl:sticky xl:top-7 xl:self-start">
          <LayerCard title="Activity" description={playable ? "Click a line to move the player there." : undefined} flush>
            {timeline.length === 0 ? (
              <p className="p-4 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">{segments.loading ? "Loading…" : "No errors, clicks, or page changes recorded."}</p>
            ) : (
              <ol className="max-h-[640px] overflow-y-auto py-1">
                {timeline.map((item, index) => {
                  const passed = loaded && item.atMs <= currentAt;
                  return (
                    <li key={`${item.kind}-${item.atMs}-${index}`} className="flex items-start gap-2 px-3 py-1.5 text-[0.75rem]">
                      <button
                        type="button"
                        disabled={!playable || !loaded}
                        className={cn("flex min-w-0 flex-1 items-start gap-2 text-left hover:text-[var(--ui-fg)]", passed ? "text-[var(--ui-fg)]" : "text-[var(--ui-fg-faint)]")}
                        onClick={() => jump(item.atMs)}
                      >
                        <span className="w-12 shrink-0 tabular-nums">{loaded ? clock(item.atMs - origin) : "-"}</span>
                        <span className={cn("w-10 shrink-0", item.kind === "error" && "text-[var(--ui-error)]")}>{kindLabel[item.kind]}</span>
                        <span className="min-w-0 break-all">
                          {item.label}
                          {item.detail ? <span className="block text-[var(--ui-fg-faint)]">{item.detail}</span> : null}
                        </span>
                      </button>
                      {item.error ? (
                        <Link className="shrink-0 text-[var(--ui-accent-light)]" to={`/errors/${item.error.issueId}/events/${item.error.eventId}`}>
                          Open
                        </Link>
                      ) : null}
                    </li>
                  );
                })}
              </ol>
            )}
          </LayerCard>
          <LayerCard title="Details">
            <dl className="grid grid-cols-2 gap-3 text-[0.8125rem]">
              <Fact label="Received">{timeAgo(iso(data.lastReceivedMs))}</Fact>
              <Fact label="Parts">{data.segmentCount.toLocaleString()}</Fact>
              <Fact label="Size">{formatBytes(data.bytes)}</Fact>
              <Fact label="Errors">{data.errorCount.toLocaleString()}</Fact>
              <Fact label="Recorded">{data.replayType === "buffer" ? "Before an error" : "Whole session"}</Fact>
              <Fact label="Environment">{data.environment ?? "-"}</Fact>
            </dl>
          </LayerCard>
        </aside>
      </div>
      <ConfirmDialog
        open={confirming}
        title="Delete replay?"
        description="The recording and its stored parts are deleted. Linked error events stay. This cannot be undone."
        confirmLabel="Delete replay"
        pending={remove.isPending}
        error={remove.error}
        onConfirm={() => remove.mutate()}
        onClose={() => setConfirming(false)}
      />
    </div>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="text-[0.75rem] text-[var(--ui-fg-faint)]">{label}</dt>
      <dd className="mt-0.5 break-all text-[var(--ui-fg)]">{children}</dd>
    </div>
  );
}
