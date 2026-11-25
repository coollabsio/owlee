import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { ChevronDown } from "reicon-react";
import { Link, useNavigate, useParams } from "react-router";
import { Stat } from "@/components/charts";
import { type Column, DataTable } from "@/components/data-table";
import { Badge } from "@/components/issue-badge";
import { ProjectField, ProjectTag } from "@/components/project";
import { CodeBlock, GuideStep } from "@/components/setup-guide";
import { Button, CopyInput, ConfirmDialog, Dialog, ErrorText, Field, FilterChips, FilterValue, Input, LayerCard, PageHeader, Select, Skeleton, Spinner, StatusBadge, Switch, Textarea, useCopy } from "@/components/ui";
import { api, ApiError, type ErrorApp } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { type GuideCode, guideFor, isPlatformId, type PlatformGuide, type PlatformId, platformGuides, platformIds } from "@/lib/platform-guides";
import { cn, fullTime, intakeSummary, timeAgo } from "@/lib/utils";
import { useListScope, useProjectChoice } from "@/lib/workspace";
import { ReplaySettingsCard, ReplaysCard } from "@/pages/replays";

export function ErrorAppsPage() {
  const canEdit = useCanEdit();
  const scope = useListScope();
  const apps = useQuery({ queryKey: scope.key("apps"), queryFn: () => api<ErrorApp[]>(scope.path("/api/error-apps")) });
  const [open, setOpen] = useState(false);
  const [platformFilter, setPlatformFilter] = useState<string | null>(null);
  const rows = (apps.data ?? []).filter((app) => !platformFilter || app.platform === platformFilter);
  const columns: Column<ErrorApp>[] = [
    {
      key: "name",
      header: "Name",
      sort: (app) => app.name,
      cell: (app) => (
        <>
          <Link className="row-link" to={`/errors/apps/${app.id}`}>
            {app.name}
          </Link>
          <ProjectTag id={app.projectId} />
          {app.quotaExceeded ? (
            <StatusBadge tone="error">Daily quota reached ({app.usedToday}/{app.dailyQuota})</StatusBadge>
          ) : null}
        </>
      ),
    },
    {
      key: "platform",
      header: "Platform",
      sort: (app) => guideFor(app.platform).label,
      cell: (app) => (
        <FilterValue
          active={platformFilter === app.platform}
          label={guideFor(app.platform).label}
          onToggle={() => setPlatformFilter(platformFilter === app.platform ? null : app.platform)}
        >
          <Badge>{guideFor(app.platform).label}</Badge>
        </FilterValue>
      ),
    },
    {
      key: "status",
      header: "Status",
      sort: (app) => (app.lastEventAt ? Date.parse(app.lastEventAt) : null),
      cellClassName: "whitespace-nowrap",
      cell: (app) => (
        <span title={app.lastEventAt ? fullTime(app.lastEventAt) : undefined}>
          {app.lastEventAt ? <StatusBadge tone="success">Last event {timeAgo(app.lastEventAt)}</StatusBadge> : <StatusBadge>Waiting for events</StatusBadge>}
        </span>
      ),
    },
    {
      key: "issues",
      header: "Issues",
      className: "text-right",
      cellClassName: "tabular-nums",
      sort: (app) => app.issueCount,
      cell: (app) => app.issueCount.toLocaleString(),
    },
  ];

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Apps"
        description="Each app has a Sentry DSN. Paste it into an official SDK and errors land in Issues."
      />
      <LayerCard
        title="All apps"
        actions={canEdit && apps.data?.length !== 0 ? <Button variant="highlighted" onClick={() => setOpen(true)}>New app</Button> : null}
        flush
      >
        {platformFilter ? (
          <div className="border-b border-[var(--ui-divider)] p-2">
            <FilterChips filters={[{ key: "platform", label: `Platform: ${guideFor(platformFilter).label}`, onRemove: () => setPlatformFilter(null) }]} onClear={() => setPlatformFilter(null)} />
          </div>
        ) : null}
        {apps.data && apps.data.length === 0 ? (
          <div className="flex min-h-40 flex-col items-center justify-center px-4 py-10 text-center">
            <h2 className="text-[0.875rem] font-medium text-[var(--ui-fg)]">No apps yet</h2>
            <p className="mx-auto mt-1 max-w-[28rem] text-[0.8125rem] text-[var(--ui-fg-faint)]">
              {canEdit
                ? "An app gives you a DSN and a setup guide. Most SDKs send their first error within a few minutes."
                : "An editor has to create an app before errors can be collected."}
            </p>
            {canEdit ? (
              <Button className="mt-4" variant="highlighted" onClick={() => setOpen(true)}>
                Create your first app
              </Button>
            ) : null}
          </div>
        ) : null}
        {apps.isLoading || (apps.data && apps.data.length > 0) ? (
          <DataTable
            columns={columns}
            rows={rows}
            rowKey={(app) => app.id}
            rowLink={(app) => `/errors/apps/${app.id}`}
            loading={apps.isLoading}
          />
        ) : null}
      </LayerCard>
      <Dialog open={canEdit && open} title="New app" description="Pick the platform you will install the SDK on. The setup guide follows it." onClose={() => setOpen(false)}>
        {open ? <NewAppForm /> : null}
      </Dialog>
    </div>
  );
}

function NewAppForm() {
  const navigate = useNavigate();
  const client = useQueryClient();
  const [name, setName] = useState("");
  const [platform, setPlatform] = useState<PlatformId>("php");
  const [projectId, setProjectId] = useProjectChoice();
  const create = useMutation({
    mutationFn: () =>
      api<ErrorApp>("/api/error-apps", { method: "POST", body: JSON.stringify({ name, platform, project_id: projectId }) }),
    onSuccess: async (app) => {
      client.setQueryData(["app", app.id], app);
      await client.invalidateQueries({ queryKey: ["apps"] });
      navigate(`/errors/apps/${app.id}`);
    },
  });
  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        create.mutate();
      }}
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Name">
          <Input value={name} required placeholder={`${platformGuides[platform].label} app`} onChange={(event) => setName(event.target.value)} />
        </Field>
        <Field label="Platform">
          <Select
            aria-label="Platform"
            value={platform}
            options={platformIds.map((id) => ({ value: id, label: platformGuides[id].label }))}
            onChange={setPlatform}
          />
        </Field>
      </div>
      <ProjectField value={projectId} onChange={setProjectId} />
      <ErrorText error={create.error} />
      <div className="flex justify-end">
        <Button variant="highlighted" type="submit" disabled={create.isPending}>
          {create.isPending ? <><Spinner />Creating…</> : "Create app"}
        </Button>
      </div>
    </form>
  );
}

export function ErrorAppDetailPage() {
  const { id = "" } = useParams();
  const navigate = useNavigate();
  const canEdit = useCanEdit();
  const client = useQueryClient();
  const app = useQuery({
    queryKey: ["app", id],
    queryFn: () => api<ErrorApp>(`/api/error-apps/${id}`),
    refetchInterval: (query) => (query.state.data && !query.state.data.lastEventAt ? 3000 : false),
  });
  // Remembers that this visit started before any event, so the page can celebrate the first one.
  const [sawWaiting, setSawWaiting] = useState(false);
  const [showGuide, setShowGuide] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const remove = useMutation({
    mutationFn: () => api(`/api/error-apps/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["apps"] });
      navigate("/errors/apps");
    },
  });

  if (app.isLoading) {
    return (
      <div className="flex flex-col gap-6" aria-busy="true">
        <div className="space-y-2">
          <Skeleton className="h-3 w-32" />
          <Skeleton className="h-6 w-48" />
          <Skeleton className="h-3 w-72" />
        </div>
        <LayerCard><Skeleton className="h-16 w-full" /></LayerCard>
        <LayerCard><Skeleton className="h-40 w-full" /></LayerCard>
      </div>
    );
  }
  if (app.isError || !app.data) {
    return <p className="text-[0.8125rem] text-[var(--ui-error)]">{(app.error as Error)?.message || "App not found"}</p>;
  }
  const data = app.data;
  const receiving = data.lastEventAt != null;
  if (!receiving && !sawWaiting) setSawWaiting(true);

  return (
    <div className="flex flex-col gap-6">
      <div>
        <nav className="mb-2 flex items-center gap-2 text-[0.8125rem] text-[var(--ui-fg-faint)]">
          <Link to="/errors/apps">Apps</Link>
          <span>/</span>
          <span className="text-[var(--ui-fg-dim)]">{data.name}</span>
        </nav>
        <PageHeader
          title={data.name}
          keepTitle
          description={`App id ${data.id} · ${guideFor(data.platform).label}`}
          actions={
            canEdit ? (
              <Button variant="danger" onClick={() => setConfirming(true)}>
                Delete app
              </Button>
            ) : null
          }
        />
      </div>
      {receiving ? (
        <LayerCard
          title={sawWaiting ? "First event received" : "Activity"}
          description={sawWaiting ? "The SDK is connected. Errors from now on are grouped into issues." : undefined}
          actions={
            <Link className="button button-highlighted" to={`/errors?app=${data.id}`}>
              View issues
            </Link>
          }
        >
          <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
            <Stat label="Issues" value={data.issueCount.toLocaleString()} />
            <Stat label="Events" value={data.eventCount.toLocaleString()} />
            <div title={fullTime(data.lastEventAt)}>
              <Stat label="Last event" value={timeAgo(data.lastEventAt)} />
            </div>
            <Stat label="Today" value={`${data.usedToday.toLocaleString()} / ${data.dailyQuota.toLocaleString()}`} />
          </div>
        </LayerCard>
      ) : (
        <LayerCard
          title="Waiting for the first event"
          description="Follow the steps below. This page checks for new events every few seconds."
          actions={<StatusBadge tone="info">Listening</StatusBadge>}
        >
          <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">
            Nothing received yet. If the test error does not show up within a minute, check that the DSN below matches the one in
            your app and that your app can reach this server.
          </p>
        </LayerCard>
      )}
      <ReplaysCard app={data} />
      <SetupGuide app={data} open={!receiving || showGuide} onToggle={receiving ? () => setShowGuide((value) => !value) : undefined} />
      <DsnCard dsn={data.dsn} />
      <AppQuota key={String(data.quotaOverride ?? "inherit")} app={data} />
      <IngestProtection key={JSON.stringify([data.allowedDomains, data.inboundFilters, data.spikeProtection])} app={data} />
      <ReplaySettingsCard key={JSON.stringify([data.replays.enabled, data.replays.dailyBytes, data.replays.hourlyLimit])} app={data} />
      <ConfirmDialog
        open={confirming}
        title="Delete app?"
        description={`${data.name}, its ${data.issueCount.toLocaleString()} issues, and every stored event are deleted. SDKs using this DSN are rejected. This cannot be undone.`}
        confirmLabel="Delete app"
        pending={remove.isPending}
        error={remove.error}
        onConfirm={() => remove.mutate()}
        onClose={() => setConfirming(false)}
      />
    </div>
  );
}

/* While the app has no events the guide stays open; after that it collapses like the issue page sections. */
function SetupGuide({ app, open, onToggle }: { app: ErrorApp; open: boolean; onToggle?: () => void }) {
  const [platform, setPlatform] = useState<PlatformId>(isPlatformId(app.platform) ? app.platform : "other");
  const guide = guideFor(platform);
  const { copied, copy } = useCopy();
  const heading = (
    <div className="min-w-0 py-0.5">
      <h2>Set up the SDK</h2>
      {open ? <p>{`Three steps for ${guide.label}. Full reference in the official docs.`}</p> : null}
    </div>
  );
  return (
    <section className="layer-card">
      <header className={open ? undefined : "!pb-1"}>
        {onToggle ? (
          <button type="button" className="flex items-start gap-1.5 text-left" onClick={onToggle} aria-expanded={open}>
            <ChevronDown className={cn("mt-1.5 size-3.5 shrink-0 text-[var(--ui-fg-faint)] transition-transform", !open && "-rotate-90")} aria-hidden="true" />
            {heading}
          </button>
        ) : (
          heading
        )}
        {open ? (
          <div className="flex flex-wrap items-center gap-2">
            <div className="w-44">
              <Select
                aria-label="Platform"
                value={platform}
                onChange={setPlatform}
                options={platformIds.map((id) => ({ value: id, label: platformGuides[id].label }))}
              />
            </div>
            <Button onClick={() => void copy(agentPrompt(app, guide))}>{copied ? "Copied" : "Copy AI prompt"}</Button>
            <a className="button" href={guide.docs} target="_blank" rel="noreferrer">
              Docs
            </a>
          </div>
        ) : null}
      </header>
      {open ? (
        <div className="layer-card-body">
          <ol className="flex flex-col gap-6">
            <GuideStep number={1} title={`Install ${guide.sdk}`}>
              <CodeBlock {...guide.install} />
            </GuideStep>
            <GuideStep number={2} title="Configure it with your DSN">
              {guide.configure(app.dsn).map((block) => (
                <CodeBlock key={block.code} {...block} />
              ))}
            </GuideStep>
            <GuideStep number={3} title="Send a test error">
              {guide.test ? <CodeBlock {...guide.test} /> : null}
              <CodeBlock file={guide.test ? "No SDK at hand? Send one with curl" : "Send one with curl"} code={curlTest(app)} />
            </GuideStep>
          </ol>
        </div>
      ) : null}
    </section>
  );
}

function DsnCard({ dsn }: { dsn: string }) {
  return (
    <LayerCard
      title="Sentry DSN"
      description="Use this with any official Sentry SDK. The public key identifies the app. It is not a secret, which is why browser SDKs can ship it."
    >
      <CopyInput value={dsn} label="Copy DSN" className="text-[0.8125rem]" />
    </LayerCard>
  );
}

/** A prompt for an AI coding agent that sets up the SDK in the user's codebase. */
function agentPrompt(app: ErrorApp, guide: PlatformGuide) {
  const block = ({ file, code }: GuideCode) => `${file ? `${file}:\n` : ""}\`\`\`\n${code}\n\`\`\``;
  return [
    `Set up error tracking in this ${guide.label} project. Owlee is a Sentry-compatible server, so use the official Sentry SDK (${guide.sdk}) and point it at the DSN below.`,
    `DSN: ${app.dsn}`,
    `1. Install ${guide.sdk}:\n${block(guide.install)}`,
    `2. Configure it with the DSN. Follow the conventions of this codebase (for example, read the DSN from an environment variable or config file if the project does that):\n${guide.configure(app.dsn).map(block).join("\n\n")}`,
    `3. Tell me how to send a test error, so I can confirm that events arrive in Owlee:\n${block(guide.test ?? { file: "With curl", code: curlTest(app) })}`,
    `Do not change unrelated code. Official reference: ${guide.docs}`,
  ].join("\n\n");
}

function curlTest(app: ErrorApp) {
  const dsn = new URL(app.dsn);
  const base = `${dsn.protocol}//${dsn.host}${dsn.pathname.replace(/\/[^/]*$/, "")}`;
  return [
    `curl -X POST '${base}/api/${app.id}/store/' \\`,
    `  -H 'Content-Type: application/json' \\`,
    `  -H 'X-Sentry-Auth: Sentry sentry_key=${app.publicKey}, sentry_version=7' \\`,
    `  -d '{"message":"Owlee test error","level":"error"}'`,
  ].join("\n");
}

function resetWhen(iso: string) {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "00:00 UTC";
  return `${date.toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", timeZone: "UTC" })} UTC`;
}

function AppQuota({ app }: { app: ErrorApp }) {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const [value, setValue] = useState(app.quotaOverride == null ? "" : String(app.quotaOverride));
  const save = useMutation({
    mutationFn: (daily: number | null) =>
      api<ErrorApp>(`/api/error-apps/${app.id}/quota`, {
        method: "PUT",
        body: JSON.stringify({ daily_quota: daily }),
      }),
    onSuccess: (data) => {
      client.setQueryData(["app", app.id], data);
      setValue(data.quotaOverride == null ? "" : String(data.quotaOverride));
      void client.invalidateQueries({ queryKey: ["apps"] });
      void client.invalidateQueries({ queryKey: ["dashboard"] });
    },
  });
  const inherited = app.quotaOverride == null;
  return (
    <LayerCard
      title="Daily cap"
      description={inherited ? (canEdit ? "Empty inherits the number from Admin → Ingest quotas." : "This app uses the instance default cap.") : "This app uses its own cap instead of the instance default."}
      actions={app.quotaExceeded ? <StatusBadge tone="error">Cap reached</StatusBadge> : null}
    >
      <p className="mb-3 text-[0.8125rem] text-[var(--ui-fg-faint)]">
        {app.usedToday.toLocaleString()} of {app.dailyQuota.toLocaleString()} errors accepted today.
        {app.quotaExceeded ? " Further events are rejected until the counter resets." : ""} Resets {resetWhen(app.resetsAt)}.
      </p>
      <p className="mb-3 text-[13px] text-[var(--ui-fg-faint)]">{intakeSummary(app.intake)}</p>
      {canEdit ? <form
        className="flex flex-col gap-3 sm:flex-row sm:items-end"
        onSubmit={(event) => {
          event.preventDefault();
          const trimmed = value.trim();
          save.mutate(trimmed === "" ? null : Number(trimmed));
        }}
      >
        <div className="sm:w-56">
          <Field label="Errors per day">
            <Input inputMode="numeric" placeholder="Inherit" value={value} onChange={(event) => setValue(event.target.value)} />
          </Field>
        </div>
        <Button variant="highlighted" type="submit" disabled={save.isPending}>{save.isPending ? <><Spinner />Saving…</> : "Save cap"}</Button>
        {app.quotaOverride != null ? (
          <Button type="button" disabled={save.isPending} onClick={() => save.mutate(null)}>Inherit</Button>
        ) : null}
      </form> : null}
      {save.error ? <p className="field-error">{(save.error as Error).message}</p> : null}
    </LayerCard>
  );
}

function IngestProtection({ app }: { app: ErrorApp }) {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const [domains, setDomains] = useState(app.allowedDomains.join("\n"));
  const [filters, setFilters] = useState(app.inboundFilters);
  const [spike, setSpike] = useState(app.spikeProtection);
  const save = useMutation({
    mutationFn: () =>
      api<ErrorApp>(`/api/error-apps/${app.id}/protection`, {
        method: "PUT",
        body: JSON.stringify({ allowedDomains: domains.split("\n"), inboundFilters: filters, spikeProtection: spike }),
      }),
    onSuccess: (data) => client.setQueryData(["app", app.id], data),
  });
  const fields = save.error instanceof ApiError ? (save.error.fields ?? {}) : {};
  const toggle = (id: string, on: boolean) => setFilters((current) => (on ? [...current, id] : current.filter((value) => value !== id)));
  return (
    <LayerCard title="Ingest protection" description="Keeps noise and floods from filling issues and spending the daily cap.">
      <form
        className="flex flex-col gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field
          label="Allowed domains"
          hint="One per line. A domain also allows its subdomains. Empty allows every origin. Requests without an Origin, such as from server SDKs, always pass."
          error={fields.allowed_domains}
        >
          <Textarea rows={3} placeholder="example.com" value={domains} readOnly={!canEdit} onChange={(event) => setDomains(event.target.value)} />
        </Field>
        <div className="flex flex-col gap-3">
          <span className="text-[0.8125rem] font-medium text-[var(--ui-fg)]">Inbound filters</span>
          {app.filterOptions.map((option) => (
            <div key={option.id} className="flex items-center justify-between gap-4">
              <div>
                <p className="text-[0.8125rem] text-[var(--ui-fg)]">{option.label}</p>
                <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">{option.description}</p>
              </div>
              <Switch aria-label={option.label} checked={filters.includes(option.id)} disabled={!canEdit} onCheckedChange={(on) => toggle(option.id, on)} />
            </div>
          ))}
          {fields.inbound_filters ? <p className="field-error">{fields.inbound_filters}</p> : null}
        </div>
        <div className="flex items-center justify-between gap-4">
          <div>
            <p className="text-[0.8125rem] text-[var(--ui-fg)]">Spike protection</p>
            <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">
              {app.usedThisHour.toLocaleString()} of {app.spikeLimit.toLocaleString()} this hour. The hourly limit is the larger of 1/24 of the daily cap
              and three times the average plus three standard deviations of the past week's hours.
            </p>
          </div>
          <Switch aria-label="Spike protection" checked={spike} disabled={!canEdit} onCheckedChange={setSpike} />
        </div>
        {canEdit ? (
          <div>
            <Button variant="highlighted" type="submit" disabled={save.isPending}>{save.isPending ? <><Spinner />Saving…</> : "Save protection"}</Button>
          </div>
        ) : null}
        {save.error && !fields.allowed_domains && !fields.inbound_filters ? <ErrorText error={save.error} /> : null}
      </form>
    </LayerCard>
  );
}
