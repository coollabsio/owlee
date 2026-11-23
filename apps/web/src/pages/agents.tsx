import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { type Column, DataTable } from "@/components/data-table";
import { Button, CopyButton, ConfirmDialog, Dialog, Empty, ErrorText, Field, FilterChips, FilterValue, Input, LayerCard, PageHeader, RowActions, Spinner, StatusBadge } from "@/components/ui";
import { ProjectField, ProjectTag } from "@/components/project";
import { type Agent, ApiError, api, createAgent, type CreatedAgent, deleteAgent, renameAgent, rotateAgentToken } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { useListScope, useProjectChoice } from "@/lib/workspace";
import { timeAgo } from "@/lib/utils";

export function AgentsPage() {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const scope = useListScope();
  const query = useQuery({ queryKey: scope.key("agents"), queryFn: () => api<Agent[]>(scope.path("/api/agents")), refetchInterval: 10_000 });
  const [open, setOpen] = useState(false);
  const [created, setCreated] = useState<CreatedAgent | null>(null);
  const [search, setSearch] = useState("");
  const [onlineFilter, setOnlineFilter] = useState<boolean | null>(null);
  const [deleting, setDeleting] = useState<Agent | null>(null);
  const [viewing, setViewing] = useState<Agent | null>(null);
  const remove = useMutation({
    mutationFn: deleteAgent,
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["agents"] });
      setDeleting(null);
    },
  });

  const filtered = (query.data ?? []).filter(
    (agent) => agent.name.toLowerCase().includes(search.trim().toLowerCase()) && (onlineFilter === null || agent.online === onlineFilter),
  );
  const agentColumns: Column<Agent>[] = [
    {
      key: "name",
      header: "Name",
      sort: (agent) => agent.name,
      cellClassName: "font-medium text-[var(--ui-fg)]",
      cell: (agent) => <>{agent.name}<ProjectTag id={agent.project_id} /></>,
    },
    {
      key: "status",
      header: "Status",
      sort: (agent) => (agent.online ? "Online" : "Offline"),
      cell: (agent) => (
        <FilterValue active={onlineFilter === agent.online} label={agent.online ? "Online" : "Offline"} onToggle={() => setOnlineFilter(onlineFilter === agent.online ? null : agent.online)}>
          <StatusBadge tone={agent.online ? "success" : "neutral"}>{agent.online ? "Online" : "Offline"}</StatusBadge>
        </FilterValue>
      ),
    },
    { key: "monitors", header: "Monitors", className: "hidden sm:table-cell", sort: (agent) => agent.monitor_count, cell: (agent) => agent.monitor_count },
    {
      key: "seen",
      header: "Last seen",
      className: "hidden md:table-cell",
      sort: (agent) => (agent.last_seen_at ? Date.parse(agent.last_seen_at) : null),
      cell: (agent) => timeAgo(agent.last_seen_at),
    },
  ];
  const actionsColumn: Column<Agent> = {
    key: "actions",
    header: "Actions",
    className: "text-right",
    cell: (agent) => (
      <RowActions label={agent.name} actions={[
          { label: "Details", onSelect: () => setViewing(agent) },
          { label: "Delete", danger: true, onSelect: () => setDeleting(agent) },
        ]} />
    ),
  };
  const newAgent = <Button variant="primary" onClick={() => setOpen(true)}>New agent</Button>;
  function closeNew() {
    setOpen(false);
    setCreated(null);
  }
  const filters = onlineFilter === null ? [] : [{ key: "status", label: `Status: ${onlineFilter ? "Online" : "Offline"}`, onRemove: () => setOnlineFilter(null) }];

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Agents" description="Agents run checks inside private networks. Each agent reads its checks from a config file on its host and reports results over outbound HTTPS, so nothing needs to be exposed." />
      {!query.isLoading && (query.data?.length ?? 0) === 0 ? (
        <Empty title="No agents" body="Add one, list the checks in its config file, and run it next to the services you want to check. Its checks show up under Monitors, in the agent's project." action={canEdit ? newAgent : undefined} />
      ) : (
        <LayerCard title="All agents" actions={canEdit ? newAgent : null} flush>
          <div className="flex items-center gap-2 border-b border-[var(--ui-divider)] p-2">
            <Input className="max-w-64" value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Search agents" aria-label="Search agents" />
            <FilterChips filters={filters} onClear={() => setOnlineFilter(null)} />
          </div>
          <DataTable columns={canEdit ? [...agentColumns, actionsColumn] : agentColumns} rows={filtered} rowKey={(agent) => agent.id} loading={query.isLoading} />
          {!query.isLoading && filtered.length === 0 ? (
            <p className="flex h-12 items-center justify-center px-4 text-[0.8125rem] text-[var(--ui-fg-faint)]">No agents match your search or filters.</p>
          ) : null}
        </LayerCard>
      )}
      <Dialog
        open={canEdit && open}
        wide={created !== null}
        title={created ? `${created.name} is ready` : "New agent"}
        description={created ? "Copy the token now. Owlee stores only its hash and cannot show it again." : "The name is shown as the check region."}
        onClose={closeNew}
      >
        {created ? <AgentSetup token={created.token} onDone={closeNew} /> : open ? <NewAgentForm onCreated={setCreated} /> : null}
      </Dialog>
      <Dialog
        open={canEdit && viewing !== null}
        wide
        title={viewing?.name ?? "Agent"}
        description="Owlee stores only a hash of the token, so it cannot show it again. Regenerate the token to get a new one."
        onClose={() => setViewing(null)}
      >
        {viewing ? <AgentDetails key={viewing.id} agent={viewing} onRenamed={setViewing} onDone={() => setViewing(null)} /> : null}
      </Dialog>
      <ConfirmDialog
        open={deleting !== null}
        title="Delete agent?"
        description={`${deleting?.name ?? "This agent"} and its token will be deleted, so its host can no longer report checks. An agent that still has monitors cannot be deleted.`}
        pending={remove.isPending}
        error={remove.error}
        onConfirm={() => deleting && remove.mutate(deleting.id)}
        onClose={() => {
          setDeleting(null);
          remove.reset();
        }}
      />
    </div>
  );
}

function NewAgentForm({ onCreated }: { onCreated: (agent: CreatedAgent) => void }) {
  const client = useQueryClient();
  const [name, setName] = useState("");
  const [projectId, setProjectId] = useProjectChoice();
  const [fields, setFields] = useState<Record<string, string>>({});
  const save = useMutation({
    mutationFn: () => createAgent({ name, project_id: projectId }),
    onSuccess: async (agent) => {
      onCreated(agent);
      await client.invalidateQueries({ queryKey: ["agents"] });
    },
    onError: (error) => setFields(error instanceof ApiError ? error.fields ?? {} : {}),
  });

  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    save.mutate();
  }

  return (
    <form className="flex flex-col gap-4" onSubmit={submit}>
      <Field label="Name" error={fields.name}>
        <Input value={name} onChange={(event) => setName(event.target.value)} placeholder="office-vpc" required />
      </Field>
      <ProjectField value={projectId} onChange={setProjectId} error={fields.project_id} />
      <ErrorText error={fields.name ? null : save.error} />
      <div className="flex justify-end">
        <Button type="submit" variant="highlighted" disabled={save.isPending}>{save.isPending ? <><Spinner />Saving…</> : "Add agent"}</Button>
      </div>
    </form>
  );
}

const exampleChecks = [
  "OWLEE_AGENT_CHECK_1_NAME='Main DB'",
  "OWLEE_AGENT_CHECK_1_TYPE=postgres",
  "OWLEE_AGENT_CHECK_1_TARGET=localhost:5432",
  "OWLEE_AGENT_CHECK_1_USER=status",
  "OWLEE_AGENT_CHECK_1_PASSWORD=...",
  "OWLEE_AGENT_CHECK_1_DATABASE=app",
  "OWLEE_AGENT_CHECK_1_INTERVAL=30",
];

function RenameAgentForm({ agent, onRenamed }: { agent: Agent; onRenamed: (agent: Agent) => void }) {
  const client = useQueryClient();
  const [name, setName] = useState(agent.name);
  const [fields, setFields] = useState<Record<string, string>>({});
  const save = useMutation({
    mutationFn: () => renameAgent(agent.id, name),
    onSuccess: async (renamed) => {
      onRenamed(renamed);
      setName(renamed.name);
      await client.invalidateQueries({ queryKey: ["agents"] });
    },
    onError: (error) => setFields(error instanceof ApiError ? error.fields ?? {} : {}),
  });

  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    save.mutate();
  }

  return (
    <form className="flex flex-col gap-2" onSubmit={submit}>
      <Field label="Name" hint="New checks use the new name as their region. Old checks keep the old name." error={fields.name}>
        <div className="flex gap-2">
          <Input value={name} onChange={(event) => setName(event.target.value)} maxLength={60} required />
          <Button type="submit" disabled={save.isPending || name.trim() === agent.name || name.trim() === ""}>{save.isPending ? <><Spinner />Saving…</> : "Rename"}</Button>
        </div>
      </Field>
      <ErrorText error={fields.name ? null : save.error} />
    </form>
  );
}

function AgentDetails({ agent, onRenamed, onDone }: { agent: Agent; onRenamed: (agent: Agent) => void; onDone: () => void }) {
  const [confirming, setConfirming] = useState(false);
  const rotate = useMutation({ mutationFn: () => rotateAgentToken(agent.id), onSuccess: () => setConfirming(false) });
  const token = rotate.data?.token ?? null;
  return (
    <div className="flex flex-col gap-4">
      <RenameAgentForm agent={agent} onRenamed={onRenamed} />
      <dl className="grid grid-cols-3 gap-2 text-[0.8125rem]">
        <div><dt className="field-label">Status</dt><dd><StatusBadge tone={agent.online ? "success" : "neutral"}>{agent.online ? "Online" : "Offline"}</StatusBadge></dd></div>
        <div><dt className="field-label">Monitors</dt><dd>{agent.monitor_count}</dd></div>
        <div><dt className="field-label">Last seen</dt><dd>{timeAgo(agent.last_seen_at)}</dd></div>
      </dl>
      {token ? (
        <p className="text-[0.75rem] text-[var(--ui-fg-faint)]">Copy the new token now. The old token no longer works, so update the agent host.</p>
      ) : confirming ? (
        <div className="flex flex-wrap items-center gap-2 rounded-lg border border-[var(--ui-control-border)] p-2.5 text-[0.75rem]">
          <span className="mr-auto">The current token stops working at once. The agent cannot report until you give it the new token.</span>
          <Button onClick={() => setConfirming(false)}>Cancel</Button>
          <Button variant="danger" disabled={rotate.isPending} onClick={() => rotate.mutate()}>{rotate.isPending ? <><Spinner />Regenerating…</> : "Regenerate"}</Button>
        </div>
      ) : (
        <div>
          <Button onClick={() => setConfirming(true)}>Regenerate token</Button>
        </div>
      )}
      <ErrorText error={rotate.error} />
      <AgentSetup token={token} onDone={onDone} />
    </div>
  );
}

function AgentSetup({ token, onDone }: { token: string | null; onDone: () => void }) {
  const origin = window.location.origin;
  const shown = token ?? "<token>";
  const docker = `docker run -d --name owlee-agent --restart unless-stopped --no-healthcheck -e OWLEE_AGENT_SERVER=${origin} -e OWLEE_AGENT_TOKEN=${shown} ${exampleChecks.map((variable) => `-e ${variable}`).join(" ")} ghcr.io/coollabsio/owlee:latest agent`;
  const binary = [
    "curl -fsSL -o owlee https://github.com/coollabsio/owlee/releases/latest/download/owlee-$(uname -m)-linux",
    "chmod +x owlee",
    `${exampleChecks.join(" ")} ./owlee agent --server ${origin} --token ${shown}`,
  ].join("\n");
  return (
    <div className="flex flex-col gap-4">
      {token ? <Snippet label="Token" value={token} /> : null}
      <p className="text-[0.75rem] text-[var(--ui-fg-faint)]">Each check is a set of OWLEE_AGENT_CHECK_&lt;n&gt;_&lt;KEY&gt; variables. TYPE is http, tcp, postgres, mysql, or redis. Add more checks with 2, 3, and so on. Passwords stay on the agent host.</p>
      <Snippet label="Docker" value={docker} />
      <Snippet label="Binary" value={binary} />
      <div className="flex justify-end">
        <Button variant="highlighted" onClick={onDone}>Done</Button>
      </div>
    </div>
  );
}

function Snippet({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="field-label">{label}</div>
      <div className="relative">
        <code className="block select-all whitespace-pre-wrap break-all rounded-lg border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] py-2 pl-2.5 pr-9 font-mono text-[0.75rem] text-[var(--ui-fg)]">
          {value}
        </code>
        <CopyButton text={value} label={`Copy ${label.toLowerCase()}`} />
      </div>
    </div>
  );
}
