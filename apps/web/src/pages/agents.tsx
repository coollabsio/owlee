import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Button, ConfirmDialog, Empty, ErrorText, Field, FilterChips, FilterValue, Input, LayerCard, PageHeader, RowActions, SkeletonRows, SortHeader, Spinner, StatusBadge, useSort } from "@/components/ui";
import { ProjectField, ProjectTag } from "@/components/project";
import { type Agent, ApiError, api, createAgent, type CreatedAgent, deleteAgent } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { useListScope, useProjectChoice } from "@/lib/workspace";
import { timeAgo } from "@/lib/utils";

export function AgentsPage() {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const scope = useListScope();
  const query = useQuery({ queryKey: scope.key("agents"), queryFn: () => api<Agent[]>(scope.path("/api/agents")), refetchInterval: 10_000 });
  const [name, setName] = useState("");
  const [projectId, setProjectId] = useProjectChoice();
  const [fields, setFields] = useState<Record<string, string>>({});
  const [created, setCreated] = useState<CreatedAgent | null>(null);
  const [search, setSearch] = useState("");
  const [onlineFilter, setOnlineFilter] = useState<boolean | null>(null);
  const [deleting, setDeleting] = useState<Agent | null>(null);
  const save = useMutation({
    mutationFn: () => createAgent({ name, project_id: projectId }),
    onSuccess: async (agent) => {
      setName("");
      setCreated(agent);
      await client.invalidateQueries({ queryKey: ["agents"] });
    },
    onError: (error) => setFields(error instanceof ApiError ? error.fields ?? {} : {}),
  });
  const remove = useMutation({
    mutationFn: deleteAgent,
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["agents"] });
      setDeleting(null);
    },
  });

  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    save.mutate();
  }

  const filtered = (query.data ?? []).filter(
    (agent) => agent.name.toLowerCase().includes(search.trim().toLowerCase()) && (onlineFilter === null || agent.online === onlineFilter),
  );
  const { rows: agents, sort, toggle } = useSort(filtered, {
    name: (agent) => agent.name,
    status: (agent) => (agent.online ? "Online" : "Offline"),
    monitors: (agent) => agent.monitor_count,
    seen: (agent) => (agent.last_seen_at ? Date.parse(agent.last_seen_at) : null),
  });
  const filters = onlineFilter === null ? [] : [{ key: "status", label: `Status: ${onlineFilter ? "Online" : "Offline"}`, onRemove: () => setOnlineFilter(null) }];

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Agents" description="Agents run checks inside private networks. Each agent reads its checks from a config file on its host and reports results over outbound HTTPS, so nothing needs to be exposed." />
      <div className={canEdit ? "grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_340px]" : "grid items-start gap-6"}>
        {!query.isLoading && (query.data?.length ?? 0) === 0 ? (
          <Empty title="No agents" body="Add one, list the checks in its config file, and run it next to the services you want to check. Its checks show up under Monitors, in the agent's project." />
        ) : (
          <LayerCard title="All agents" flush>
            <div className="flex items-center gap-2 border-b border-[var(--ui-divider)] p-2">
              <Input className="max-w-64" value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Search agents" aria-label="Search agents" />
              <FilterChips filters={filters} onClear={() => setOnlineFilter(null)} />
            </div>
            <div className="overflow-x-auto">
              <table className="cool-table">
                <thead>
                  <tr>
                    <SortHeader column="name" sort={sort} onSort={toggle}>Name</SortHeader>
                    <SortHeader column="status" sort={sort} onSort={toggle}>Status</SortHeader>
                    <SortHeader column="monitors" sort={sort} onSort={toggle} className="hidden sm:table-cell">Monitors</SortHeader>
                    <SortHeader column="seen" sort={sort} onSort={toggle} className="hidden md:table-cell">Last seen</SortHeader>
                    {canEdit ? <th className="text-right">Actions</th> : null}
                  </tr>
                </thead>
                <tbody>
                  {query.isLoading ? <SkeletonRows columns={[undefined, undefined, "hidden sm:table-cell", "hidden md:table-cell", ...(canEdit ? ["text-right"] : [])]} /> : null}
                  {agents.map((agent) => (
                    <tr key={agent.id}>
                      <td className="font-medium text-[var(--ui-fg)]">{agent.name}<ProjectTag id={agent.project_id} /></td>
                      <td>
                        <FilterValue active={onlineFilter === agent.online} label={agent.online ? "Online" : "Offline"} onToggle={() => setOnlineFilter(onlineFilter === agent.online ? null : agent.online)}>
                          <StatusBadge tone={agent.online ? "success" : "neutral"}>{agent.online ? "Online" : "Offline"}</StatusBadge>
                        </FilterValue>
                      </td>
                      <td className="hidden sm:table-cell">{agent.monitor_count}</td>
                      <td className="hidden md:table-cell">{timeAgo(agent.last_seen_at)}</td>
                      {canEdit ? (
                        <td className="text-right">
                          <RowActions label={agent.name} actions={[{ label: "Delete", danger: true, onSelect: () => setDeleting(agent) }]} />
                        </td>
                      ) : null}
                    </tr>
                  ))}
                  {!query.isLoading && agents.length === 0 ? (
                    <tr>
                      <td colSpan={5} className="text-center text-[var(--ui-fg-faint)]">No agents match your search or filters.</td>
                    </tr>
                  ) : null}
                </tbody>
              </table>
            </div>
          </LayerCard>
        )}
        {!canEdit ? null : created ? (
          <AgentToken agent={created} onDone={() => setCreated(null)} />
        ) : (
          <LayerCard title="Add an agent" description="The name is shown as the check region.">
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
          </LayerCard>
        )}
      </div>
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

const configPath = "/etc/owlee/agent.toml";

const exampleConfig = `[[check]]
name = "Main DB"
type = "postgres"          # http, tcp, postgres, mysql, redis
target = "localhost:5432"
user = "status"
password_env = "PG_PASSWORD"
database = "app"
interval = 30`;

function AgentToken({ agent, onDone }: { agent: CreatedAgent; onDone: () => void }) {
  const origin = window.location.origin;
  const docker = `docker run -d --name owlee-agent --restart unless-stopped --no-healthcheck -e OWLEE_AGENT_SERVER=${origin} -e OWLEE_AGENT_TOKEN=${agent.token} -e OWLEE_AGENT_CONFIG=${configPath} -e PG_PASSWORD=... -v ${configPath}:${configPath}:ro ghcr.io/coollabsio/owlee:latest agent`;
  const binary = `PG_PASSWORD=... owlee agent --server ${origin} --token ${agent.token} --config ${configPath}`;
  return (
    <LayerCard title={`${agent.name} is ready`} description="Copy the token now. Owlee stores only its hash and cannot show it again." >
      <div className="flex flex-col gap-4">
        <Snippet label="Token" value={agent.token} />
        <Snippet label={configPath} value={exampleConfig} multiline />
        <p className="text-[0.75rem] text-[var(--ui-fg-faint)]">Passwords stay on the agent host. The config names an environment variable, not the password.</p>
        <Snippet label="Docker" value={docker} />
        <Snippet label="Binary" value={binary} />
        <div className="flex justify-end">
          <Button variant="highlighted" onClick={onDone}>Done</Button>
        </div>
      </div>
    </LayerCard>
  );
}

function Snippet({ label, value, multiline = false }: { label: string; value: string; multiline?: boolean }) {
  const [copied, setCopied] = useState(false);

  async function copy() {
    await navigator.clipboard.writeText(value);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  return (
    <div>
      <div className="field-label">
        <span>{label}</span>
        <button type="button" className="ml-auto text-[0.6875rem] font-normal hover:text-[var(--ui-fg)]" onClick={() => void copy()}>
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      <code className={`block select-all ${multiline ? "whitespace-pre overflow-x-auto" : "break-all"} rounded-lg border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] px-2.5 py-2 font-mono text-[0.75rem] text-[var(--ui-fg)]`}>
        {value}
      </code>
    </div>
  );
}
