import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { Stat } from "@/components/charts";
import { Badge } from "@/components/issue-badge";
import { ProjectField, ProjectTag } from "@/components/project";
import { Button, Dialog, ErrorText, Field, Input, LayerCard, PageHeader, Select, StatusBadge, useCopy } from "@/components/ui";
import { api, type ErrorApp } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { type GuideCode, guideFor, isPlatformId, type PlatformId, platformGuides, platformIds } from "@/lib/platform-guides";
import { cn, fullTime, timeAgo } from "@/lib/utils";
import { useListScope, useProjectChoice } from "@/lib/workspace";

export function ErrorAppsPage() {
  const navigate = useNavigate();
  const canEdit = useCanEdit();
  const client = useQueryClient();
  const scope = useListScope();
  const apps = useQuery({ queryKey: scope.key("apps"), queryFn: () => api<ErrorApp[]>(scope.path("/api/error-apps")) });
  const [name, setName] = useState("");
  const [platform, setPlatform] = useState<PlatformId>("php");
  const [projectId, setProjectId] = useProjectChoice();
  const [error, setError] = useState("");
  const create = useMutation({
    mutationFn: () =>
      api<ErrorApp>("/api/error-apps", { method: "POST", body: JSON.stringify({ name, platform, project_id: projectId }) }),
    onSuccess: async (app) => {
      setName("");
      client.setQueryData(["app", app.id], app);
      await client.invalidateQueries({ queryKey: ["apps"] });
      navigate(`/errors/apps/${app.id}`);
    },
    onError: (reason: Error) => setError(reason.message),
  });

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Apps"
        description="Each app has a Sentry DSN. Paste it into an official SDK and errors land in Issues."
      />
      {canEdit ? (
        <LayerCard title="New app" description="Pick the platform you will install the SDK on. The setup guide follows it.">
          <form
            className="flex flex-col gap-4"
            onSubmit={(event) => {
              event.preventDefault();
              setError("");
              create.mutate();
            }}
          >
            <div role="radiogroup" aria-label="Platform" className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-6">
              {platformIds.map((id) => (
                <button
                  key={id}
                  type="button"
                  role="radio"
                  aria-checked={platform === id}
                  onClick={() => setPlatform(id)}
                  className={cn(
                    "rounded-lg border px-3 py-2.5 text-left transition-colors",
                    platform === id
                      ? "border-[var(--ui-accent)] bg-[var(--ui-hover)]"
                      : "border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] hover:bg-[var(--ui-hover)]",
                  )}
                >
                  <span className="block text-[13px] font-medium text-[var(--ui-fg)]">{platformGuides[id].label}</span>
                  <span className="block truncate font-mono text-[11px] text-[var(--ui-fg-faint)]">{platformGuides[id].sdk}</span>
                </button>
              ))}
            </div>
            <div className="flex flex-col gap-3 md:flex-row md:items-end">
              <div className="flex-1">
                <Field label="Name">
                  <Input
                    id="new-app-name"
                    value={name}
                    required
                    placeholder={`${platformGuides[platform].label} app`}
                    onChange={(event) => setName(event.target.value)}
                  />
                </Field>
              </div>
              <div className="md:w-56">
                <ProjectField value={projectId} onChange={setProjectId} />
              </div>
              <Button variant="highlighted" type="submit" disabled={create.isPending}>
                {create.isPending ? "Creating…" : "Create app"}
              </Button>
            </div>
          </form>
          {error ? <p className="field-error">{error}</p> : null}
        </LayerCard>
      ) : null}
      <LayerCard title="All apps" flush>
        {apps.isLoading ? <p className="p-4 text-[13px] text-[var(--ui-fg-faint)]">Loading apps...</p> : null}
        {apps.data && apps.data.length === 0 ? (
          <div className="flex min-h-40 flex-col items-center justify-center px-4 py-10 text-center">
            <h2 className="text-[14px] font-medium text-[var(--ui-fg)]">No apps yet</h2>
            <p className="mx-auto mt-1 max-w-[28rem] text-[13px] text-[var(--ui-fg-faint)]">
              {canEdit
                ? "An app gives you a DSN and a setup guide. Most SDKs send their first error within a few minutes."
                : "An editor has to create an app before errors can be collected."}
            </p>
            {canEdit ? (
              <Button className="mt-4" variant="highlighted" onClick={() => document.getElementById("new-app-name")?.focus()}>
                Create your first app
              </Button>
            ) : null}
          </div>
        ) : null}
        {apps.data && apps.data.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="cool-table">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Platform</th>
                  <th>Status</th>
                  <th className="text-right">Issues</th>
                </tr>
              </thead>
              <tbody>
                {apps.data.map((app) => (
                  <tr key={app.id} className="clickable-row" onClick={() => navigate(`/errors/apps/${app.id}`)}>
                    <td>
                      <Link className="row-link" to={`/errors/apps/${app.id}`} onClick={(event) => event.stopPropagation()}>
                        {app.name}
                      </Link>
                      <ProjectTag id={app.projectId} />
                      {app.quotaExceeded ? (
                        <StatusBadge tone="error">Daily quota reached ({app.usedToday}/{app.dailyQuota})</StatusBadge>
                      ) : null}
                    </td>
                    <td>
                      <Badge>{guideFor(app.platform).label}</Badge>
                    </td>
                    <td className="whitespace-nowrap" title={app.lastEventAt ? fullTime(app.lastEventAt) : undefined}>
                      {app.lastEventAt ? (
                        <StatusBadge tone="success">Last event {timeAgo(app.lastEventAt)}</StatusBadge>
                      ) : (
                        <StatusBadge>Waiting for events</StatusBadge>
                      )}
                    </td>
                    <td className="text-right tabular-nums">{app.issueCount.toLocaleString()}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : null}
      </LayerCard>
    </div>
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

  if (app.isLoading) return <p className="text-[13px] text-[var(--ui-fg-faint)]">Loading app...</p>;
  if (app.isError || !app.data) {
    return <p className="text-[13px] text-[var(--ui-error)]">{(app.error as Error)?.message || "App not found"}</p>;
  }
  const data = app.data;
  const receiving = data.lastEventAt != null;
  if (!receiving && !sawWaiting) setSawWaiting(true);

  return (
    <div className="flex flex-col gap-6">
      <div>
        <nav className="mb-2 flex items-center gap-2 text-[13px] text-[var(--ui-fg-faint)]">
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
          <p className="text-[13px] text-[var(--ui-fg-faint)]">
            Nothing received yet. If the test error does not show up within a minute, check that the DSN below matches the one in
            your app and that your app can reach this server.
          </p>
        </LayerCard>
      )}
      {receiving && !showGuide ? (
        <div>
          <Button onClick={() => setShowGuide(true)}>Show setup guide</Button>
        </div>
      ) : (
        <SetupGuide app={data} onHide={receiving ? () => setShowGuide(false) : undefined} />
      )}
      <DsnCard dsn={data.dsn} />
      <AppQuota key={String(data.quotaOverride ?? "inherit")} app={data} />
      <Dialog
        open={confirming}
        title={`Delete ${data.name}?`}
        description={`The app, its ${data.issueCount.toLocaleString()} issues, and every stored event are deleted. SDKs using this DSN are rejected. This cannot be undone.`}
        onClose={() => setConfirming(false)}
        footer={
          <>
            <Button type="button" onClick={() => setConfirming(false)}>
              Cancel
            </Button>
            <Button type="button" variant="danger" disabled={remove.isPending} onClick={() => remove.mutate()}>
              {remove.isPending ? "Deleting…" : "Delete app"}
            </Button>
          </>
        }
      >
        <ErrorText error={remove.error} />
      </Dialog>
    </div>
  );
}

function SetupGuide({ app, onHide }: { app: ErrorApp; onHide?: () => void }) {
  const [platform, setPlatform] = useState<PlatformId>(isPlatformId(app.platform) ? app.platform : "other");
  const guide = guideFor(platform);
  return (
    <LayerCard
      title="Set up the SDK"
      description={`Three steps for ${guide.label}. Full reference in the official docs.`}
      actions={
        <>
          <div className="w-44">
            <Select
              aria-label="Platform"
              value={platform}
              onChange={setPlatform}
              options={platformIds.map((id) => ({ value: id, label: platformGuides[id].label }))}
            />
          </div>
          <a className="button" href={guide.docs} target="_blank" rel="noreferrer">
            Docs
          </a>
          {onHide ? <Button onClick={onHide}>Hide</Button> : null}
        </>
      }
    >
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
    </LayerCard>
  );
}

function GuideStep({ number, title, children }: { number: number; title: string; children: ReactNode }) {
  return (
    <li className="flex gap-3">
      <span className="flex size-6 shrink-0 items-center justify-center rounded-full border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] text-[12px] font-medium tabular-nums text-[var(--ui-fg)]">
        {number}
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <h3 className="pt-0.5 text-[13px] font-medium text-[var(--ui-fg)]">{title}</h3>
        {children}
      </div>
    </li>
  );
}

function CodeBlock({ file, code }: GuideCode) {
  const { copied, copy } = useCopy();
  return (
    <div className="min-w-0">
      <div className="field-label">
        <span className="truncate">{file ?? ""}</span>
        <button
          type="button"
          className="ml-auto text-[11px] font-normal hover:text-[var(--ui-fg)]"
          onClick={() => copy(code)}
        >
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      <pre className="overflow-auto rounded-lg border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] p-3 font-mono text-[12px] leading-5 text-[var(--ui-fg)]">
        {code}
      </pre>
    </div>
  );
}

function DsnCard({ dsn }: { dsn: string }) {
  const { copied, copy } = useCopy();
  return (
    <LayerCard
      title="Sentry DSN"
      description="Use this with any official Sentry SDK. The public key identifies the app. It is not a secret, which is why browser SDKs can ship it."
      actions={
        <Button
          variant="highlighted"
          onClick={() => copy(dsn)}
        >
          {copied ? "Copied" : "Copy DSN"}
        </Button>
      }
    >
      <Input readOnly value={dsn} className="font-mono text-[13px]" onFocus={(event) => event.currentTarget.select()} />
    </LayerCard>
  );
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
      description={inherited ? "Empty inherits the number from Settings → Ingest quotas." : "This app uses its own cap instead of the instance default."}
      actions={app.quotaExceeded ? <StatusBadge tone="error">Cap reached</StatusBadge> : null}
    >
      <p className="mb-3 text-[13px] text-[var(--ui-fg-faint)]">
        {app.usedToday.toLocaleString()} of {app.dailyQuota.toLocaleString()} errors accepted today.
        {app.quotaExceeded ? " Further events are rejected until the counter resets." : ""} Resets {resetWhen(app.resetsAt)}.
      </p>
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
        <Button variant="highlighted" type="submit" disabled={save.isPending}>Save cap</Button>
        {app.quotaOverride != null ? (
          <Button type="button" disabled={save.isPending} onClick={() => save.mutate(null)}>Inherit</Button>
        ) : null}
      </form> : null}
      {save.error ? <p className="field-error">{(save.error as Error).message}</p> : null}
    </LayerCard>
  );
}
