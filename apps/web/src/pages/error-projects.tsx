import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { Stat } from "@/components/charts";
import { Badge } from "@/components/issue-badge";
import { Button, Dialog, ErrorText, Field, Input, LayerCard, PageHeader, Select, StatusBadge } from "@/components/ui";
import { api, type Project } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { type GuideCode, guideFor, isPlatformId, type PlatformId, platformGuides, platformIds } from "@/lib/platform-guides";
import { cn, fullTime, timeAgo } from "@/lib/utils";

export function ErrorProjectsPage() {
  const navigate = useNavigate();
  const canEdit = useCanEdit();
  const client = useQueryClient();
  const projects = useQuery({ queryKey: ["projects"], queryFn: () => api<Project[]>("/api/error-projects") });
  const [name, setName] = useState("");
  const [platform, setPlatform] = useState<PlatformId>("php");
  const [error, setError] = useState("");
  const create = useMutation({
    mutationFn: () =>
      api<Project>("/api/error-projects", { method: "POST", body: JSON.stringify({ name, platform }) }),
    onSuccess: async (project) => {
      setName("");
      client.setQueryData(["project", project.id], project);
      await client.invalidateQueries({ queryKey: ["projects"] });
      navigate(`/error-projects/${project.id}`);
    },
    onError: (reason: Error) => setError(reason.message),
  });

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Projects"
        description="Each project has a Sentry DSN. Paste it into an official SDK and errors land in Issues."
      />
      {canEdit ? (
        <LayerCard title="New project" description="Pick the platform you will install the SDK on. The setup guide follows it.">
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
                    id="new-project-name"
                    value={name}
                    required
                    placeholder={`${platformGuides[platform].label} app`}
                    onChange={(event) => setName(event.target.value)}
                  />
                </Field>
              </div>
              <Button variant="highlighted" type="submit" disabled={create.isPending}>
                {create.isPending ? "Creating…" : "Create project"}
              </Button>
            </div>
          </form>
          {error ? <p className="field-error">{error}</p> : null}
        </LayerCard>
      ) : null}
      <LayerCard title="All projects" flush>
        {projects.isLoading ? <p className="p-4 text-[13px] text-[var(--ui-fg-faint)]">Loading projects...</p> : null}
        {projects.data && projects.data.length === 0 ? (
          <div className="flex min-h-40 flex-col items-center justify-center px-4 py-10 text-center">
            <h2 className="text-[14px] font-medium text-[var(--ui-fg)]">No projects yet</h2>
            <p className="mx-auto mt-1 max-w-[28rem] text-[13px] text-[var(--ui-fg-faint)]">
              {canEdit
                ? "A project gives you a DSN and a setup guide. Most SDKs send their first error within a few minutes."
                : "An editor has to create a project before errors can be collected."}
            </p>
            {canEdit ? (
              <Button className="mt-4" variant="highlighted" onClick={() => document.getElementById("new-project-name")?.focus()}>
                Create your first project
              </Button>
            ) : null}
          </div>
        ) : null}
        {projects.data && projects.data.length > 0 ? (
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
                {projects.data.map((project) => (
                  <tr key={project.id} className="clickable-row" onClick={() => navigate(`/error-projects/${project.id}`)}>
                    <td>
                      <Link className="row-link" to={`/error-projects/${project.id}`} onClick={(event) => event.stopPropagation()}>
                        {project.name}
                      </Link>
                      {project.quotaExceeded ? (
                        <StatusBadge tone="error">Daily quota reached ({project.usedToday}/{project.dailyQuota})</StatusBadge>
                      ) : null}
                    </td>
                    <td>
                      <Badge>{guideFor(project.platform).label}</Badge>
                    </td>
                    <td className="whitespace-nowrap" title={project.lastEventAt ? fullTime(project.lastEventAt) : undefined}>
                      {project.lastEventAt ? (
                        <StatusBadge tone="success">Last event {timeAgo(project.lastEventAt)}</StatusBadge>
                      ) : (
                        <StatusBadge>Waiting for events</StatusBadge>
                      )}
                    </td>
                    <td className="text-right tabular-nums">{project.issueCount.toLocaleString()}</td>
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

export function ProjectDetailPage() {
  const { id = "" } = useParams();
  const navigate = useNavigate();
  const canEdit = useCanEdit();
  const client = useQueryClient();
  const project = useQuery({
    queryKey: ["project", id],
    queryFn: () => api<Project>(`/api/error-projects/${id}`),
    refetchInterval: (query) => (query.state.data && !query.state.data.lastEventAt ? 3000 : false),
  });
  // Remembers that this visit started before any event, so the page can celebrate the first one.
  const [sawWaiting, setSawWaiting] = useState(false);
  const [showGuide, setShowGuide] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const remove = useMutation({
    mutationFn: () => api(`/api/error-projects/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["projects"] });
      navigate("/error-projects");
    },
  });

  if (project.isLoading) return <p className="text-[13px] text-[var(--ui-fg-faint)]">Loading project...</p>;
  if (project.isError || !project.data) {
    return <p className="text-[13px] text-[var(--ui-error)]">{(project.error as Error)?.message || "Project not found"}</p>;
  }
  const data = project.data;
  const receiving = data.lastEventAt != null;
  if (!receiving && !sawWaiting) setSawWaiting(true);

  return (
    <div className="flex flex-col gap-6">
      <div>
        <nav className="mb-2 flex items-center gap-2 text-[13px] text-[var(--ui-fg-faint)]">
          <Link to="/error-projects">Projects</Link>
          <span>/</span>
          <span className="text-[var(--ui-fg-dim)]">{data.name}</span>
        </nav>
        <PageHeader
          title={data.name}
          keepTitle
          description={`Project id ${data.id} · ${guideFor(data.platform).label}`}
          actions={
            canEdit ? (
              <Button variant="danger" onClick={() => setConfirming(true)}>
                Delete project
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
            <Link className="button button-highlighted" to={`/issues?project=${data.id}`}>
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
        <SetupGuide project={data} onHide={receiving ? () => setShowGuide(false) : undefined} />
      )}
      <DsnCard dsn={data.dsn} />
      <ProjectQuota key={String(data.quotaOverride ?? "inherit")} project={data} />
      <Dialog
        open={confirming}
        title={`Delete ${data.name}?`}
        description={`The project, its ${data.issueCount.toLocaleString()} issues, and every stored event are deleted. SDKs using this DSN are rejected. This cannot be undone.`}
        onClose={() => setConfirming(false)}
        footer={
          <>
            <Button type="button" onClick={() => setConfirming(false)}>
              Cancel
            </Button>
            <Button type="button" variant="danger" disabled={remove.isPending} onClick={() => remove.mutate()}>
              {remove.isPending ? "Deleting…" : "Delete project"}
            </Button>
          </>
        }
      >
        <ErrorText error={remove.error} />
      </Dialog>
    </div>
  );
}

function SetupGuide({ project, onHide }: { project: Project; onHide?: () => void }) {
  const [platform, setPlatform] = useState<PlatformId>(isPlatformId(project.platform) ? project.platform : "other");
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
          {guide.configure(project.dsn).map((block) => (
            <CodeBlock key={block.code} {...block} />
          ))}
        </GuideStep>
        <GuideStep number={3} title="Send a test error">
          {guide.test ? <CodeBlock {...guide.test} /> : null}
          <CodeBlock file={guide.test ? "No SDK at hand? Send one with curl" : "Send one with curl"} code={curlTest(project)} />
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
  const [copied, setCopied] = useState(false);
  return (
    <div className="min-w-0">
      <div className="field-label">
        <span className="truncate">{file ?? ""}</span>
        <button
          type="button"
          className="ml-auto text-[11px] font-normal hover:text-[var(--ui-fg)]"
          onClick={async () => {
            await navigator.clipboard.writeText(code);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          }}
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
  const [copied, setCopied] = useState(false);
  return (
    <LayerCard
      title="Sentry DSN"
      description="Use this with any official Sentry SDK. The public key identifies the project. It is not a secret, which is why browser SDKs can ship it."
      actions={
        <Button
          variant="highlighted"
          onClick={async () => {
            await navigator.clipboard.writeText(dsn);
            setCopied(true);
          }}
        >
          {copied ? "Copied" : "Copy DSN"}
        </Button>
      }
    >
      <Input readOnly value={dsn} className="font-mono text-[13px]" onFocus={(event) => event.currentTarget.select()} />
    </LayerCard>
  );
}

function curlTest(project: Project) {
  const dsn = new URL(project.dsn);
  const base = `${dsn.protocol}//${dsn.host}${dsn.pathname.replace(/\/[^/]*$/, "")}`;
  return [
    `curl -X POST '${base}/api/${project.id}/store/' \\`,
    `  -H 'Content-Type: application/json' \\`,
    `  -H 'X-Sentry-Auth: Sentry sentry_key=${project.publicKey}, sentry_version=7' \\`,
    `  -d '{"message":"Owlee test error","level":"error"}'`,
  ].join("\n");
}

function resetWhen(iso: string) {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "00:00 UTC";
  return `${date.toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", timeZone: "UTC" })} UTC`;
}

function ProjectQuota({ project }: { project: Project }) {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const [value, setValue] = useState(project.quotaOverride == null ? "" : String(project.quotaOverride));
  const save = useMutation({
    mutationFn: (daily: number | null) =>
      api<Project>(`/api/error-projects/${project.id}/quota`, {
        method: "PUT",
        body: JSON.stringify({ daily_quota: daily }),
      }),
    onSuccess: (data) => {
      client.setQueryData(["project", project.id], data);
      setValue(data.quotaOverride == null ? "" : String(data.quotaOverride));
      void client.invalidateQueries({ queryKey: ["projects"] });
      void client.invalidateQueries({ queryKey: ["dashboard"] });
    },
  });
  const inherited = project.quotaOverride == null;
  return (
    <LayerCard
      title="Daily cap"
      description={inherited ? "Empty inherits the number from Settings → Ingest quotas." : "This project uses its own cap instead of the instance default."}
      actions={project.quotaExceeded ? <StatusBadge tone="error">Cap reached</StatusBadge> : null}
    >
      <p className="mb-3 text-[13px] text-[var(--ui-fg-faint)]">
        {project.usedToday.toLocaleString()} of {project.dailyQuota.toLocaleString()} errors accepted today.
        {project.quotaExceeded ? " Further events are rejected until the counter resets." : ""} Resets {resetWhen(project.resetsAt)}.
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
        {project.quotaOverride != null ? (
          <Button type="button" disabled={save.isPending} onClick={() => save.mutate(null)}>Inherit</Button>
        ) : null}
      </form> : null}
      {save.error ? <p className="field-error">{(save.error as Error).message}</p> : null}
    </LayerCard>
  );
}
