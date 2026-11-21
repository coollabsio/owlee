import { keepPreviousData, useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { Search4 as Search } from "reicon-react";
import { Button, ConfirmDialog, Dialog, Empty, ErrorText, Field, FilterChips, FilterValue, Input, LayerCard, LoadMore, PageHeader, Select, Skeleton, SkeletonRows, SortHeader, Spinner, StatusBadge, Textarea, useSort } from "@/components/ui";
import { ProjectField, ProjectTag } from "@/components/project";
import { ApiError, api, flattenPages, withCursor, type Incident, type Monitor, type Paged, type StatusPage } from "@/lib/api";
import { useDebounced } from "@/pages/issue-detail";
import { useCanEdit } from "@/lib/me";
import { useListScope, useProjectChoice } from "@/lib/workspace";
import { formatTime, stateLabel } from "@/lib/utils";

const statuses = ["investigating", "identified", "monitoring", "resolved"].map((item) => ({ value: item, label: stateLabel[item] }));
const impacts = ["none", "minor", "major", "critical"].map((item) => ({ value: item, label: stateLabel[item] }));

function impactTone(impact: string) {
  return impact === "critical" || impact === "major" ? "error" : impact === "minor" ? "warning" : "neutral";
}

export function statusTone(status: string) {
  return status === "resolved" ? "success" : status === "investigating" ? "warning" : "info";
}

function rowCover(to: string) {
  return <Link className="cell-link-anchor" to={to} tabIndex={-1} aria-hidden="true" />;
}

function CardText({ title, body }: { title: string; body?: string }) {
  return (
    <div className="flex min-h-32 flex-col items-center justify-center px-4 py-8 text-center">
      <p className="text-[0.875rem] font-medium text-[var(--ui-fg)]">{title}</p>
      {body ? <p className="mx-auto mt-1 max-w-[28rem] text-[0.8125rem] text-[var(--ui-fg-faint)]">{body}</p> : null}
    </div>
  );
}

export function IncidentsPage() {
  const scope = useListScope();
  const canEdit = useCanEdit();
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [impactFilter, setImpactFilter] = useState<Incident["impact"] | null>(null);
  const [statusFilter, setStatusFilter] = useState<Incident["status"] | null>(null);
  const needle = useDebounced(search.trim(), 300);
  const filterQuery = new URLSearchParams({
    ...(needle ? { q: needle } : {}),
    ...(impactFilter ? { impact: impactFilter } : {}),
    ...(statusFilter ? { status: statusFilter } : {}),
  }).toString();
  // The server filters, so a match on a page not yet loaded still shows up.
  const query = useInfiniteQuery({
    queryKey: scope.key("incidents", filterQuery),
    queryFn: ({ pageParam }) => api<Paged<Incident>>(withCursor(scope.path(`/api/incidents?${filterQuery}`), pageParam)),
    initialPageParam: "",
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    placeholderData: keepPreviousData,
  });
  const loaded = flattenPages(query.data?.pages.map((page) => page.items));
  const unfiltered = !needle && !impactFilter && !statusFilter;
  const { rows, sort, toggle } = useSort(loaded, {
    title: (incident) => incident.title,
    impact: (incident) => impacts.findIndex((item) => item.value === incident.impact),
    status: (incident) => statuses.findIndex((item) => item.value === incident.status),
    source: (incident) => (incident.auto ? "Automatic" : "Manual"),
    started: (incident) => Date.parse(incident.started_at),
  });
  const filters = [
    ...(impactFilter ? [{ key: "impact", label: `Impact: ${stateLabel[impactFilter]}`, onRemove: () => setImpactFilter(null) }] : []),
    ...(statusFilter ? [{ key: "status", label: `Status: ${stateLabel[statusFilter]}`, onRemove: () => setStatusFilter(null) }] : []),
  ];
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Incidents"
        description="A timeline customers read on the status page. Automatic incidents open after repeated failures."
      />
      {unfiltered && query.data && loaded.length === 0 && !query.isPlaceholderData ? (
        <Empty title="No incidents" body="When a monitor crosses its failure threshold, Owlee files one. You can also write one by hand." action={canEdit ? <Button variant="primary" onClick={() => setOpen(true)}>Report incident</Button> : undefined} />
      ) : (
        <LayerCard
          title="All incidents"
          description={query.data ? (query.hasNextPage ? `Newest ${loaded.length}` : `${loaded.length} ${unfiltered ? "total" : "matching"}`) : undefined}
          actions={canEdit ? <Button variant="primary" onClick={() => setOpen(true)}>Report incident</Button> : null}
          flush
        >
          <div className="flex flex-wrap items-center gap-2 border-b border-[var(--ui-divider)] p-2">
            <div className="relative w-full sm:w-72">
              <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-[var(--ui-fg-faint)]" aria-hidden="true" />
              <Input style={{ paddingLeft: 36 }} placeholder="Search by title" value={search} onChange={(event) => setSearch(event.target.value)} aria-label="Search incidents" />
            </div>
            <FilterChips
              filters={filters}
              onClear={() => {
                setImpactFilter(null);
                setStatusFilter(null);
              }}
            />
          </div>
          {!query.isLoading && rows.length === 0 ? (
            <CardText title="No matching incidents" body="Try a different title, or remove a filter." />
          ) : (
            <div className="overflow-x-auto">
              <table className="cool-table">
                <thead>
                  <tr>
                    <SortHeader column="title" sort={sort} onSort={toggle}>Incident</SortHeader>
                    <SortHeader column="impact" sort={sort} onSort={toggle}>Impact</SortHeader>
                    <SortHeader column="status" sort={sort} onSort={toggle}>Status</SortHeader>
                    <SortHeader column="source" sort={sort} onSort={toggle} className="hidden md:table-cell">Source</SortHeader>
                    <SortHeader column="started" sort={sort} onSort={toggle}>Started</SortHeader>
                  </tr>
                </thead>
                <tbody>
                  {query.isLoading ? <SkeletonRows columns={[undefined, undefined, undefined, "hidden md:table-cell", undefined]} /> : rows.map((incident) => {
                    const to = `/incidents/${incident.id}`;
                    return (
                      <tr key={incident.id} className="clickable-row">
                        <td className="cell-link max-w-[28rem]">
                          <Link className="row-link" to={to}>{incident.title}</Link>
                          <ProjectTag id={incident.project_id} />
                          {rowCover(to)}
                          <div className="truncate text-[0.6875rem] text-[var(--ui-fg-faint)]">{incident.updates.at(-1)?.message || "-"}</div>
                        </td>
                        <td className="cell-link">
                          {rowCover(to)}
                          <FilterValue active={impactFilter === incident.impact} label={stateLabel[incident.impact]} onToggle={() => setImpactFilter(impactFilter === incident.impact ? null : incident.impact)}>
                            <StatusBadge tone={impactTone(incident.impact)}>{stateLabel[incident.impact]}</StatusBadge>
                          </FilterValue>
                        </td>
                        <td className="cell-link">
                          {rowCover(to)}
                          <FilterValue active={statusFilter === incident.status} label={stateLabel[incident.status]} onToggle={() => setStatusFilter(statusFilter === incident.status ? null : incident.status)}>
                            <StatusBadge tone={statusTone(incident.status)}>{stateLabel[incident.status]}</StatusBadge>
                          </FilterValue>
                        </td>
                        <td className="cell-link hidden md:table-cell">
                          {rowCover(to)}
                          {incident.auto ? "Automatic" : "Manual"}
                        </td>
                        <td className="cell-link whitespace-nowrap">
                          {rowCover(to)}
                          {formatTime(incident.started_at)}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
              <LoadMore query={query} label="Load older incidents" />
            </div>
          )}
        </LayerCard>
      )}
      {canEdit ? <IncidentDialog open={open} onClose={() => setOpen(false)} /> : null}
    </div>
  );
}

export function IncidentDetailPage() {
  const { id = "" } = useParams();
  const navigate = useNavigate();
  const canEdit = useCanEdit();
  const client = useQueryClient();
  const query = useQuery({ queryKey: ["incident", id], queryFn: () => api<Incident>(`/api/incidents/${id}`) });
  const [status, setStatus] = useState("monitoring");
  const [message, setMessage] = useState("");
  const [fields, setFields] = useState<Record<string, string>>({});
  const [deleting, setDeleting] = useState(false);
  const [editing, setEditing] = useState(false);
  const [editingUpdate, setEditingUpdate] = useState<Incident["updates"][number] | null>(null);
  const update = useMutation({
    mutationFn: () =>
      api<Incident>(`/api/incidents/${id}/updates`, {
        method: "POST",
        body: JSON.stringify({ status, message }),
      }),
    onSuccess: async (incident) => {
      client.setQueryData(["incident", id], incident);
      setMessage("");
      await client.invalidateQueries({ queryKey: ["incidents"] });
      await client.invalidateQueries({ queryKey: ["overview"] });
    },
    onError: (error) => setFields(error instanceof ApiError ? error.fields ?? {} : {}),
  });
  const remove = useMutation({
    mutationFn: () => api(`/api/incidents/${id}`, { method: "DELETE" }),
    onSuccess: () => navigate("/incidents"),
  });
  const incident = query.data;
  if (query.isLoading) {
    return (
      <div className="flex flex-col gap-6" aria-busy="true">
        <div className="space-y-2">
          <Skeleton className="h-6 w-56" />
          <Skeleton className="h-3 w-40" />
        </div>
        <LayerCard><Skeleton className="h-40 w-full" /></LayerCard>
      </div>
    );
  }
  if (!incident) return <p className="field-error">{(query.error as Error | null)?.message ?? "Not found."}</p>;
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title={incident.title}
        keepTitle
        description={`Opened ${formatTime(incident.started_at)}${incident.auto ? " by the scheduler" : ""}`}
        actions={
          canEdit ? (
            <>
              <Button onClick={() => setEditing(true)}>Edit</Button>
              <Button variant="danger" onClick={() => setDeleting(true)}>Delete</Button>
            </>
          ) : null
        }
      />
      <Dialog open={editing} title="Edit incident" description="Fixes the wording. Nobody is alerted." onClose={() => setEditing(false)}>
        {editing ? <EditIncidentForm incident={incident} onDone={() => setEditing(false)} /> : null}
      </Dialog>
      <Dialog open={editingUpdate !== null} title="Edit update" description="Fixes the wording. The status and time stay the same." onClose={() => setEditingUpdate(null)}>
        {editingUpdate ? <EditUpdateForm key={editingUpdate.id} incidentId={incident.id} update={editingUpdate} onDone={() => setEditingUpdate(null)} /> : null}
      </Dialog>
      <ConfirmDialog
        open={deleting}
        title="Delete incident?"
        description={`${incident.title} and its timeline will be deleted, including from the status page.`}
        pending={remove.isPending}
        error={remove.error}
        onConfirm={() => remove.mutate()}
        onClose={() => setDeleting(false)}
      />
      <div className={canEdit ? "grid items-start gap-6 lg:grid-cols-[1fr_360px]" : "grid items-start gap-6"}>
        <LayerCard
          title="Timeline"
          actions={
            <>
              <StatusBadge tone={impactTone(incident.impact)}>{stateLabel[incident.impact]}</StatusBadge>
              <StatusBadge tone={statusTone(incident.status)}>{stateLabel[incident.status]}</StatusBadge>
            </>
          }
        >
          <ol className="space-y-4 border-l border-[var(--ui-divider)] pl-5">
            {incident.updates.map((item) => (
              <li key={item.id} className="relative">
                <span className="absolute -left-[1.4rem] top-1.5 size-2.5 rounded-full bg-[var(--ui-accent)]" />
                <div className="flex items-center gap-2 text-[0.6875rem] text-[var(--ui-fg-faint)]">
                  {formatTime(item.created_at)} · {stateLabel[item.status]}
                  {canEdit ? (
                    <button type="button" className="text-[var(--ui-fg-faint)] underline-offset-2 hover:text-[var(--ui-fg)] hover:underline" onClick={() => setEditingUpdate(item)}>
                      Edit
                    </button>
                  ) : null}
                </div>
                <p className="mt-1 text-[0.8125rem] text-[var(--ui-fg-dim)]">{item.message}</p>
              </li>
            ))}
          </ol>
        </LayerCard>
        {canEdit ? <LayerCard title="Post an update" description="Updates are added to the public timeline.">
          <div className="space-y-3">
            <Field label="Status">
              <Select value={status} options={statuses} onChange={setStatus} aria-label="Status" />
            </Field>
            <Field label="Message" error={fields.message}>
              <Textarea value={message} onChange={(event) => setMessage(event.target.value)} />
            </Field>
            <ErrorText error={update.error} />
            <div className="flex justify-end">
              <Button variant="primary" onClick={() => update.mutate()} disabled={update.isPending || message.trim().length === 0}>
                {update.isPending ? <><Spinner />Posting…</> : "Post update"}
              </Button>
            </div>
          </div>
        </LayerCard> : null}
      </div>
    </div>
  );
}

function useSaveIncident(path: string, onDone: () => void) {
  const client = useQueryClient();
  const [fields, setFields] = useState<Record<string, string>>({});
  const save = useMutation({
    mutationFn: (body: object) => api<Incident>(path, { method: "PUT", body: JSON.stringify(body) }),
    onSuccess: async (incident) => {
      client.setQueryData(["incident", incident.id], incident);
      await client.invalidateQueries({ queryKey: ["incidents"] });
      onDone();
    },
    onError: (error) => setFields(error instanceof ApiError ? error.fields ?? {} : {}),
  });
  return { save, fields };
}

function EditIncidentForm({ incident, onDone }: { incident: Incident; onDone: () => void }) {
  const [title, setTitle] = useState(incident.title);
  const [impact, setImpact] = useState<string>(incident.impact);
  const { save, fields } = useSaveIncident(`/api/incidents/${incident.id}`, onDone);
  function submit(event: FormEvent) {
    event.preventDefault();
    save.mutate({ title, impact });
  }
  return (
    <form className="space-y-3" onSubmit={submit}>
      <Field label="Title" error={fields.title}><Input value={title} onChange={(event) => setTitle(event.target.value)} required /></Field>
      <Field label="Impact">
        <Select value={impact} options={impacts} onChange={setImpact} aria-label="Impact" />
      </Field>
      <ErrorText error={save.error} />
      <div className="flex justify-end pt-1">
        <Button type="submit" variant="primary" disabled={save.isPending}>{save.isPending ? <><Spinner />Saving…</> : "Save"}</Button>
      </div>
    </form>
  );
}

function EditUpdateForm({ incidentId, update, onDone }: { incidentId: string; update: Incident["updates"][number]; onDone: () => void }) {
  const [message, setMessage] = useState(update.message);
  const { save, fields } = useSaveIncident(`/api/incidents/${incidentId}/updates/${update.id}`, onDone);
  function submit(event: FormEvent) {
    event.preventDefault();
    save.mutate({ message });
  }
  return (
    <form className="space-y-3" onSubmit={submit}>
      <Field label="Message" error={fields.message}><Textarea value={message} onChange={(event) => setMessage(event.target.value)} required /></Field>
      <ErrorText error={save.error} />
      <div className="flex justify-end pt-1">
        <Button type="submit" variant="primary" disabled={save.isPending || message.trim().length === 0}>{save.isPending ? <><Spinner />Saving…</> : "Save"}</Button>
      </div>
    </form>
  );
}

function IncidentDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const workspaceWide = useListScope({ allProjects: true });
  const pages = useQuery({ queryKey: workspaceWide.key("pages"), queryFn: () => api<StatusPage[]>(workspaceWide.path("/api/pages")), enabled: open });
  const monitors = useQuery({ queryKey: workspaceWide.key("monitors"), queryFn: () => api<Monitor[]>(workspaceWide.path("/api/monitors")), enabled: open });
  const [projectId, setProjectId] = useProjectChoice();
  const [title, setTitle] = useState("");
  const [message, setMessage] = useState("");
  const [status, setStatus] = useState("investigating");
  const [impact, setImpact] = useState("major");
  const [pageId, setPageId] = useState("");
  const [monitorId, setMonitorId] = useState("");
  const [fields, setFields] = useState<Record<string, string>>({});
  const save = useMutation({
    mutationFn: () =>
      api<Incident>("/api/incidents", {
        method: "POST",
        body: JSON.stringify({
          title,
          message,
          status,
          impact,
          page_id: pageId || null,
          monitor_id: monitorId || null,
          project_id: projectId,
        }),
      }),
    onSuccess: async (incident) => {
      await client.invalidateQueries({ queryKey: ["incidents"] });
      onClose();
      setTitle("");
      setMessage("");
      navigate(`/incidents/${incident.id}`);
    },
    onError: (error) => setFields(error instanceof ApiError ? error.fields ?? {} : {}),
  });
  function submit(event: FormEvent) {
    event.preventDefault();
    save.mutate();
  }
  return (
    <Dialog open={open} onClose={onClose} title="Report an incident" description="The first message becomes the start of the public timeline.">
      <form className="space-y-3" onSubmit={submit}>
        <Field label="Title" error={fields.title}><Input value={title} onChange={(event) => setTitle(event.target.value)} required /></Field>
        <ProjectField value={projectId} onChange={setProjectId} error={fields.project_id} />
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Status">
            <Select value={status} options={statuses} onChange={setStatus} aria-label="Status" />
          </Field>
          <Field label="Impact">
            <Select value={impact} options={impacts} onChange={setImpact} aria-label="Impact" />
          </Field>
        </div>
        <Field label="Status page">
          <Select
            value={pageId}
            options={[{ value: "", label: "None" }, ...(pages.data ?? []).map((page) => ({ value: page.id, label: page.title }))]}
            onChange={setPageId}
            aria-label="Status page"
          />
        </Field>
        <Field label="Monitor">
          <Select
            value={monitorId}
            options={[{ value: "", label: "None" }, ...(monitors.data ?? []).map((monitor) => ({ value: monitor.id, label: monitor.name }))]}
            onChange={setMonitorId}
            aria-label="Monitor"
          />
        </Field>
        <Field label="Message" error={fields.message}><Textarea value={message} onChange={(event) => setMessage(event.target.value)} required /></Field>
        <ErrorText error={save.error} />
        <div className="flex justify-end gap-2 pt-2"><Button type="button" onClick={onClose}>Cancel</Button><Button type="submit" variant="primary" disabled={save.isPending}>{save.isPending ? <><Spinner />Saving…</> : "Create incident"}</Button></div>
      </form>
    </Dialog>
  );
}
