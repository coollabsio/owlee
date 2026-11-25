import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useEffect, useState } from "react";
import { Link, useLocation, useNavigate, useParams } from "react-router";
import { type Column, DataTable } from "@/components/data-table";
import { ProjectField, ProjectTag } from "@/components/project";
import { CodeBlock } from "@/components/setup-guide";
import { Button, ConfirmDialog, CopyInput, Dialog, ErrorText, Field, Input, LayerCard, PageHeader, RowActions, Select, Skeleton, Spinner, StatusBadge } from "@/components/ui";
import { ApiError, api, type IntakeCounts } from "@/lib/api";
import type { CreatedTelemetrySource, TelemetrySource } from "@/lib/logs";
import { useCanEdit } from "@/lib/me";
import { collectorConfig, otelEnv, otlpEndpoint, sdkGuides } from "@/lib/otel-guides";
import { DROP_REASONS, fullTime, timeAgo } from "@/lib/utils";
import { useListScope, useProjectChoice, useWorkspace } from "@/lib/workspace";

/** A token just made or rotated. Owlee shows it this one time. */
type Issued = { id: string; name: string; token: string };

const sourcePath = (id: string) => `/logs/sources/${id}`;

function useSources({ allProjects = false } = {}) {
  const scope = useListScope({ allProjects });
  return useQuery({ queryKey: scope.key("telemetry-sources"), queryFn: () => api<TelemetrySource[]>(scope.path("/api/telemetry-sources")) });
}

const patchSource = (id: string, body: { name?: string; dailyQuota?: number | null }) =>
  api<TelemetrySource>(`/api/telemetry-sources/${id}`, { method: "PATCH", body: JSON.stringify(body) });

const quotaLabel = (source: TelemetrySource) => (source.dailyQuota === null ? "Instance default" : `${source.dailyQuota.toLocaleString()} / day`);

function intakeLine(intake: IntakeCounts) {
  if (intake.stored === 0 && intake.dropped === 0) return "none";
  return intake.dropped ? `${intake.stored.toLocaleString()} stored, ${intake.dropped.toLocaleString()} dropped` : `${intake.stored.toLocaleString()} stored`;
}

/** The source list, with create, rename, quota, rotate, and delete. On the Logs page and in project settings. */
export function SourcesCard({ allProjects = false }: { allProjects?: boolean }) {
  const canEdit = useCanEdit();
  const sources = useSources({ allProjects });
  const { projects } = useWorkspace();
  const [creating, setCreating] = useState(false);
  const [issued, setIssued] = useState<Issued | null>(null);
  const [editing, setEditing] = useState<{ kind: "rename" | "quota"; source: TelemetrySource } | null>(null);
  const [confirming, setConfirming] = useState<{ kind: "rotate" | "delete"; source: TelemetrySource } | null>(null);
  const client = useQueryClient();
  const act = useMutation({
    mutationFn: async ({ kind, source }: { kind: "rotate" | "delete"; source: TelemetrySource }) => {
      if (kind === "delete") return api<void>(`/api/telemetry-sources/${source.id}`, { method: "DELETE" });
      const { token } = await api<{ token: string }>(`/api/telemetry-sources/${source.id}/rotate`, { method: "POST" });
      setIssued({ id: source.id, name: source.name, token });
    },
    onSuccess: async () => {
      setConfirming(null);
      await client.invalidateQueries({ queryKey: ["telemetry-sources"] });
    },
  });
  const projectName = (id: string) => projects.find((project) => project.id === id)?.name ?? "";
  const columns: Column<TelemetrySource>[] = [
    {
      key: "name",
      header: "Name",
      sort: (source) => source.name,
      cell: (source) => (
        <>
          <Link className="row-link" to={sourcePath(source.id)}>{source.name}</Link>
          {allProjects && projects.length > 1 ? <span className="ml-2 text-[0.75rem] text-[var(--ui-fg-faint)]">{projectName(source.projectId)}</span> : <ProjectTag id={source.projectId} />}
        </>
      ),
    },
    { key: "token", header: "Token", cellClassName: "font-mono text-[0.75rem]", cell: (source) => `…${source.tokenHint}` },
    { key: "quota", header: "Daily quota", sort: (source) => source.dailyQuota, cellClassName: "whitespace-nowrap", cell: quotaLabel },
    {
      key: "intake",
      header: "Intake since restart",
      cellClassName: "text-[0.75rem] text-[var(--ui-fg-dim)]",
      cell: (source) =>
        source.firstLogAt === null && source.intake.logs.stored === 0 ? (
          <StatusBadge>Waiting for the first data</StatusBadge>
        ) : (
          <span>Logs {intakeLine(source.intake.logs)} · Traces {intakeLine(source.intake.traces)} · Metrics {intakeLine(source.intake.metrics)}</span>
        ),
    },
    {
      key: "actions",
      header: "",
      stopRowClick: true,
      className: "w-10",
      cell: (source) =>
        canEdit ? (
          <RowActions
            label={source.name}
            actions={[
              { label: "Rename", onSelect: () => setEditing({ kind: "rename", source }) },
              { label: "Daily quota", onSelect: () => setEditing({ kind: "quota", source }) },
              { label: "Rotate token", onSelect: () => setConfirming({ kind: "rotate", source }) },
              { label: "Delete", danger: true, onSelect: () => setConfirming({ kind: "delete", source }) },
            ]}
          />
        ) : null,
    },
  ];
  const rows = sources.data ?? [];
  return (
    <LayerCard
      title="Sources"
      description="A source is one OTLP token. Send logs, traces, and metrics with it from an OpenTelemetry SDK or Collector."
      actions={canEdit ? <Button variant="highlighted" onClick={() => setCreating(true)}>New source</Button> : null}
      flush
    >
      {sources.isError ? <div className="p-4"><ErrorText error={sources.error} /></div> : null}
      {sources.isSuccess && rows.length === 0 ? (
        <p className="px-4 py-6 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">
          {canEdit ? "No sources yet. Create one to get an OTLP endpoint and token." : "No sources yet. An editor has to create one."}
        </p>
      ) : (
        <DataTable columns={columns} rows={rows} rowKey={(source) => source.id} rowLink={(source) => sourcePath(source.id)} loading={sources.isLoading} />
      )}
      <Dialog open={canEdit && creating} title="New source" description="Name the app or Collector that sends data. Owlee shows the token once." onClose={() => setCreating(false)}>
        {creating ? (
          <CreateSourceForm
            onCreated={(source) => {
              setCreating(false);
              setIssued({ id: source.id, name: source.name, token: source.token });
            }}
          />
        ) : null}
      </Dialog>
      <IssuedTokenDialog issued={issued} onClose={() => setIssued(null)} />
      {editing ? <EditSourceDialog kind={editing.kind} source={editing.source} onClose={() => setEditing(null)} /> : null}
      <ConfirmDialog
        open={confirming !== null}
        title={confirming?.kind === "delete" ? `Delete ${confirming.source.name}?` : `Rotate the token of ${confirming?.source.name ?? ""}?`}
        description={
          confirming?.kind === "delete"
            ? "Its logs, traces, and metrics are deleted, and senders using its token are rejected. This cannot be undone."
            : "The current token stops working at once. Senders must get the new one."
        }
        confirmLabel={confirming?.kind === "delete" ? "Delete source" : "Rotate token"}
        pending={act.isPending}
        error={act.error}
        onConfirm={() => confirming && act.mutate(confirming)}
        onClose={() => {
          setConfirming(null);
          act.reset();
        }}
      />
    </LayerCard>
  );
}

function CreateSourceForm({ onCreated }: { onCreated: (source: CreatedTelemetrySource) => void }) {
  const client = useQueryClient();
  const [name, setName] = useState("");
  const [projectId, setProjectId] = useProjectChoice();
  const create = useMutation({
    mutationFn: () => api<CreatedTelemetrySource>("/api/telemetry-sources", { method: "POST", body: JSON.stringify({ project_id: projectId, name }) }),
    onSuccess: async (source) => {
      onCreated(source);
      await client.invalidateQueries({ queryKey: ["telemetry-sources"] });
    },
  });
  const fields = create.error instanceof ApiError ? (create.error.fields ?? {}) : {};
  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        create.mutate();
      }}
    >
      <Field label="Name" error={fields.name}>
        <Input value={name} required placeholder="checkout-api" onChange={(event) => setName(event.target.value)} />
      </Field>
      <ProjectField value={projectId} onChange={setProjectId} error={fields.project_id} />
      {fields.name || fields.project_id ? null : <ErrorText error={create.error} />}
      <div className="flex justify-end">
        <Button variant="highlighted" type="submit" disabled={create.isPending}>
          {create.isPending ? <><Spinner />Creating…</> : "Create source"}
        </Button>
      </div>
    </form>
  );
}

function IssuedTokenDialog({ issued, onClose }: { issued: Issued | null; onClose: () => void }) {
  const navigate = useNavigate();
  return (
    <Dialog
      open={issued !== null}
      wide
      title={issued ? `${issued.name} is ready` : "Token"}
      description="Copy this token now. Owlee stores only its hash and cannot show it again."
      onClose={onClose}
      footer={
        <>
          <Button type="button" onClick={onClose}>Done</Button>
          <Button
            type="button"
            variant="highlighted"
            onClick={() => {
              if (!issued) return;
              onClose();
              navigate(sourcePath(issued.id), { state: { token: issued.token } });
            }}
          >
            Open setup guide
          </Button>
        </>
      }
    >
      {issued ? <TokenFields token={issued.token} /> : null}
    </Dialog>
  );
}

function TokenFields({ token }: { token: string }) {
  const origin = window.location.origin;
  return (
    <div role="status" className="flex flex-col gap-4">
      <Field label="Token">
        <CopyInput value={token} label="Copy token" />
      </Field>
      <CodeBlock file="Environment variables" code={otelEnv(origin, token).join("\n")} />
    </div>
  );
}

function EditSourceDialog({ kind, source, onClose }: { kind: "rename" | "quota"; source: TelemetrySource; onClose: () => void }) {
  const client = useQueryClient();
  const [value, setValue] = useState(kind === "rename" ? source.name : source.dailyQuota === null ? "" : String(source.dailyQuota));
  const save = useMutation({
    mutationFn: () => {
      const trimmed = value.trim();
      return patchSource(source.id, kind === "rename" ? { name: trimmed } : { dailyQuota: trimmed === "" ? null : Number(trimmed) });
    },
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["telemetry-sources"] });
      onClose();
    },
  });
  const field = kind === "rename" ? "name" : "dailyQuota";
  const fields = save.error instanceof ApiError ? (save.error.fields ?? {}) : {};
  const fieldError = fields[field] ?? fields.daily_quota;
  function submit(event: FormEvent) {
    event.preventDefault();
    save.mutate();
  }
  return (
    <Dialog
      open
      title={kind === "rename" ? "Rename source" : "Daily quota"}
      description={kind === "rename" ? undefined : "Items per day across logs, spans, and metric points. Empty inherits the instance default."}
      onClose={onClose}
      footer={
        <>
          <Button type="button" onClick={onClose}>Cancel</Button>
          <Button type="submit" form="edit-source" variant="highlighted" disabled={save.isPending}>{save.isPending ? <><Spinner />Saving…</> : "Save"}</Button>
        </>
      }
    >
      <form id="edit-source" className="flex flex-col gap-3" onSubmit={submit}>
        <Field label={kind === "rename" ? "Name" : "Items per day"} error={fieldError}>
          {kind === "rename" ? (
            <Input value={value} required onChange={(event) => setValue(event.target.value)} />
          ) : (
            <Input inputMode="numeric" placeholder="Inherit" value={value} onChange={(event) => setValue(event.target.value)} />
          )}
        </Field>
        {fieldError ? null : <ErrorText error={save.error} />}
      </form>
    </Dialog>
  );
}

export function LogSourceDetailPage() {
  const { id = "" } = useParams();
  const location = useLocation();
  const navigate = useNavigate();
  const canEdit = useCanEdit();
  const client = useQueryClient();
  const scope = useListScope({ allProjects: true });
  // The token arrives once, in the navigation state from create or rotate. Clear it from history so a reload or Back does not show it again.
  const [token, setToken] = useState<string | null>(() => (location.state as { token?: string } | null)?.token ?? null);
  useEffect(() => {
    if (location.state) navigate(location.pathname, { replace: true, state: null });
  }, [location.state, location.pathname, navigate]);
  const sources = useQuery({
    queryKey: scope.key("telemetry-sources"),
    queryFn: () => api<TelemetrySource[]>(scope.path("/api/telemetry-sources")),
    refetchInterval: (query) => (query.state.data?.find((item) => item.id === id)?.firstLogAt ? false : 3000),
  });
  const [sawWaiting, setSawWaiting] = useState(false);
  const [editing, setEditing] = useState<"rename" | "quota" | null>(null);
  const [confirming, setConfirming] = useState<"rotate" | "delete" | null>(null);
  const act = useMutation({
    mutationFn: async (kind: "rotate" | "delete") => {
      if (kind === "delete") {
        await api<void>(`/api/telemetry-sources/${id}`, { method: "DELETE" });
        navigate("/logs/sources");
        return;
      }
      const rotated = await api<{ token: string }>(`/api/telemetry-sources/${id}/rotate`, { method: "POST" });
      setToken(rotated.token);
    },
    onSuccess: async () => {
      setConfirming(null);
      await client.invalidateQueries({ queryKey: ["telemetry-sources"] });
    },
  });

  if (sources.isLoading) {
    return (
      <div className="flex flex-col gap-6" aria-busy="true">
        <Skeleton className="h-6 w-48" />
        <LayerCard><Skeleton className="h-16 w-full" /></LayerCard>
        <LayerCard><Skeleton className="h-40 w-full" /></LayerCard>
      </div>
    );
  }
  const source = sources.data?.find((item) => item.id === id);
  if (!source) return <ErrorText error={sources.error ?? new Error("Source not found.")} />;
  const receiving = source.firstLogAt !== null;
  if (!receiving && !sawWaiting) setSawWaiting(true);
  const origin = window.location.origin;
  const shownToken = token ?? "<token>";

  return (
    <div className="flex flex-col gap-6">
      <div>
        <nav className="mb-2 flex items-center gap-2 text-[0.8125rem] text-[var(--ui-fg-faint)]">
          <Link to="/logs/sources">Sources</Link>
          <span>/</span>
          <span className="text-[var(--ui-fg-dim)]">{source.name}</span>
        </nav>
        <PageHeader
          title={source.name}
          keepTitle
          description={`Token …${source.tokenHint} · daily quota ${quotaLabel(source).toLowerCase()} · created ${timeAgo(source.createdAt)}`}
          actions={
            canEdit ? (
              <>
                <Button onClick={() => setEditing("rename")}>Rename</Button>
                <Button onClick={() => setEditing("quota")}>Daily quota</Button>
                <Button onClick={() => setConfirming("rotate")}>Rotate token</Button>
                <Button variant="danger" onClick={() => setConfirming("delete")}>Delete</Button>
              </>
            ) : null
          }
        />
      </div>
      {receiving ? (
        <LayerCard
          title={sawWaiting ? "First log received" : "Receiving data"}
          description={`First log ${timeAgo(source.firstLogAt)}.`}
          actions={<Link className="button button-highlighted" to="/logs">View logs</Link>}
        >
          <IntakeTable source={source} />
        </LayerCard>
      ) : (
        <LayerCard
          title="Waiting for the first data"
          description="Set the two variables below and start your app. This page checks every few seconds."
          actions={<StatusBadge tone="info">Listening</StatusBadge>}
        >
          <IntakeTable source={source} />
        </LayerCard>
      )}
      {token ? (
        <LayerCard title="Token" description="Copy it now. Owlee stores only its hash and cannot show it again. The examples below include it.">
          <CopyInput value={token} label="Copy token" />
        </LayerCard>
      ) : null}
      <LayerCard
        title="Endpoint"
        description={token ? "Set these in the environment of the app or Collector." : "Replace <token> with the token shown when the source was made. Lost it? Rotate the token."}
      >
        <div className="flex flex-col gap-3">
          <CopyInput value={otlpEndpoint(origin)} label="Copy endpoint" />
          <CodeBlock file="Environment variables" code={otelEnv(origin, shownToken).join("\n")} />
        </div>
      </LayerCard>
      <SetupExamples origin={origin} token={shownToken} />
      {editing ? <EditSourceDialog kind={editing} source={source} onClose={() => setEditing(null)} /> : null}
      <ConfirmDialog
        open={confirming !== null}
        title={confirming === "delete" ? `Delete ${source.name}?` : `Rotate the token of ${source.name}?`}
        description={
          confirming === "delete"
            ? "Its logs, traces, and metrics are deleted, and senders using its token are rejected. This cannot be undone."
            : "The current token stops working at once. Senders must get the new one."
        }
        confirmLabel={confirming === "delete" ? "Delete source" : "Rotate token"}
        pending={act.isPending}
        error={act.error}
        onConfirm={() => confirming && act.mutate(confirming)}
        onClose={() => {
          setConfirming(null);
          act.reset();
        }}
      />
    </div>
  );
}

function IntakeTable({ source }: { source: TelemetrySource }) {
  const signals = [
    ["Logs", source.intake.logs],
    ["Traces", source.intake.traces],
    ["Metrics", source.intake.metrics],
  ] as const;
  return (
    <div className="flex flex-col gap-2">
      <table className="w-full text-[0.8125rem]">
        <thead className="text-left text-[0.75rem] text-[var(--ui-fg-faint)]">
          <tr>
            <th className="py-1 font-medium">Signal</th>
            <th className="py-1 text-right font-medium">Stored</th>
            <th className="py-1 text-right font-medium">Dropped</th>
            <th className="py-1 pl-6 font-medium">Last drop</th>
          </tr>
        </thead>
        <tbody>
          {signals.map(([label, intake]) => (
            <tr key={label} className="border-t border-[var(--ui-divider)]">
              <td className="py-1.5 text-[var(--ui-fg)]">{label}</td>
              <td className="py-1.5 text-right tabular-nums">{intake.stored.toLocaleString()}</td>
              <td className="py-1.5 text-right tabular-nums">{intake.dropped.toLocaleString()}</td>
              <td className="py-1.5 pl-6 text-[var(--ui-fg-dim)]" title={intake.last_drop ? fullTime(intake.last_drop.at) : undefined}>
                {intake.last_drop ? `${timeAgo(intake.last_drop.at)}: ${DROP_REASONS[intake.last_drop.reason]}` : "-"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="text-[0.75rem] text-[var(--ui-fg-faint)]">Counts since the server started.</p>
    </div>
  );
}

function SetupExamples({ origin, token }: { origin: string; token: string }) {
  const [choice, setChoice] = useState("collector");
  const guide = sdkGuides.find((item) => item.id === choice);
  return (
    <LayerCard
      title="Send data"
      description="Each SDK reads the endpoint and token from the variables above. A Collector takes them in its config."
      actions={
        <>
          <div className="w-40">
            <Select
              aria-label="Example"
              value={choice}
              onChange={setChoice}
              options={[{ value: "collector", label: "Collector" }, ...sdkGuides.map((item) => ({ value: item.id, label: item.label }))]}
            />
          </div>
          <a className="button" href={guide?.docs ?? "https://opentelemetry.io/docs/collector/configuration/"} target="_blank" rel="noreferrer">
            Docs
          </a>
        </>
      }
    >
      {guide ? (
        <div className="flex flex-col gap-4">
          <CodeBlock file="Install" code={guide.install.code} />
          <CodeBlock {...guide.code} />
        </div>
      ) : (
        <CodeBlock file="otel-collector.yaml" code={collectorConfig(origin, token)} />
      )}
    </LayerCard>
  );
}
