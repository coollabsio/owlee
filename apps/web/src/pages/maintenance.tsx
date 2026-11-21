import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Button, ConfirmDialog, Dialog, ErrorText, Field, FilterChips, FilterValue, Input, LayerCard, PageHeader, RowActions, Select, SkeletonRows, SortHeader, Spinner, StatusBadge, Textarea, useSort } from "@/components/ui";
import { ProjectField, ProjectTag } from "@/components/project";
import { ApiError, api, type Maintenance, type Monitor, type StatusPage } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { useListScope, useProjectChoice } from "@/lib/workspace";
import { formatTime, fromLocalInput, toLocalInput } from "@/lib/utils";

export function MaintenancePage() {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const scope = useListScope();
  const query = useQuery({ queryKey: scope.key("maintenances"), queryFn: () => api<Maintenance[]>(scope.path("/api/maintenances")) });
  const [open, setOpen] = useState(false);
  const [editing, setEditing] = useState<Maintenance | null>(null);
  const [deleting, setDeleting] = useState<Maintenance | null>(null);
  const [activeFilter, setActiveFilter] = useState<boolean | null>(null);
  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/maintenances/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["maintenances"] });
      setDeleting(null);
    },
  });

  const all = query.data ?? [];
  const { rows: windows, sort, toggle } = useSort(
    all.filter((item) => activeFilter === null || item.active === activeFilter),
    {
      title: (item) => item.title,
      when: (item) => Date.parse(item.starts_at),
      status: (item) => (item.active ? "In progress" : "Scheduled"),
    },
  );

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Maintenance" description="Checks inside a window are recorded, not counted as downtime, and do not page anyone." />
      <LayerCard
        title="Windows"
        description={query.data ? `${all.length} scheduled` : undefined}
        actions={canEdit ? <Button variant="primary" onClick={() => setOpen(true)}>New window</Button> : null}
        flush
      >
        {activeFilter !== null ? (
          <div className="border-b border-[var(--ui-divider)] p-2">
            <FilterChips filters={[{ key: "status", label: `Status: ${activeFilter ? "In progress" : "Scheduled"}`, onRemove: () => setActiveFilter(null) }]} onClear={() => setActiveFilter(null)} />
          </div>
        ) : null}
        {!query.isLoading && all.length === 0 ? (
          <CardText title="No windows" body="Schedule one before a failover or a deploy so the status page stays calm." />
        ) : !query.isLoading && windows.length === 0 ? (
          <CardText title="No matching windows" body="Remove the status filter to see every window." />
        ) : (
          <div className="overflow-x-auto">
            <table className="cool-table">
              <thead>
                <tr>
                  <SortHeader column="title" sort={sort} onSort={toggle}>Window</SortHeader>
                  <SortHeader column="when" sort={sort} onSort={toggle} className="hidden md:table-cell">When</SortHeader>
                  <SortHeader column="status" sort={sort} onSort={toggle}>Status</SortHeader>
                  {canEdit ? <th className="text-right">Actions</th> : null}
                </tr>
              </thead>
              <tbody>
                {query.isLoading ? <SkeletonRows columns={[undefined, "hidden md:table-cell", undefined, ...(canEdit ? ["text-right"] : [])]} /> : null}
                {windows.map((item) => (
                  <tr key={item.id}>
                    <td className="max-w-[24rem]">
                      <div className="font-medium text-[var(--ui-fg)]">{item.title}<ProjectTag id={item.project_id} /></div>
                      <div className="truncate text-[0.6875rem] text-[var(--ui-fg-faint)]">{item.message || "-"}</div>
                      <div className="text-[0.6875rem] text-[var(--ui-fg-faint)] md:hidden">{formatTime(item.starts_at)} - {formatTime(item.ends_at)}</div>
                    </td>
                    <td className="hidden whitespace-nowrap md:table-cell">{formatTime(item.starts_at)} - {formatTime(item.ends_at)}</td>
                    <td>
                      <FilterValue active={activeFilter === item.active} label={item.active ? "In progress" : "Scheduled"} onToggle={() => setActiveFilter(activeFilter === item.active ? null : item.active)}>
                        <StatusBadge tone={item.active ? "info" : "neutral"}>{item.active ? "In progress" : "Scheduled"}</StatusBadge>
                      </FilterValue>
                    </td>
                    {canEdit ? (
                      <td className="text-right">
                        <RowActions label={item.title} actions={[
                            { label: "Edit", onSelect: () => setEditing(item) },
                            { label: "Delete", danger: true, onSelect: () => setDeleting(item) },
                          ]} />
                      </td>
                    ) : null}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </LayerCard>
      <Dialog open={canEdit && open} title="Schedule a window" description="Shown on the status page while it runs." onClose={() => setOpen(false)}>
        {open ? <WindowForm onDone={() => setOpen(false)} /> : null}
      </Dialog>
      <Dialog open={canEdit && editing !== null} title="Edit window" description={editing?.title} onClose={() => setEditing(null)}>
        {editing ? <WindowForm key={editing.id} item={editing} onDone={() => setEditing(null)} /> : null}
      </Dialog>
      <ConfirmDialog
        open={deleting !== null}
        title="Delete maintenance window?"
        description={`${deleting?.title ?? "This window"} will be deleted and removed from its status page.`}
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

const keepMonitors = "__keep";

function WindowForm({ item, onDone }: { item?: Maintenance; onDone: () => void }) {
  const client = useQueryClient();
  const workspaceWide = useListScope({ allProjects: true });
  const pages = useQuery({ queryKey: workspaceWide.key("pages"), queryFn: () => api<StatusPage[]>(workspaceWide.path("/api/pages")) });
  const monitors = useQuery({ queryKey: workspaceWide.key("monitors"), queryFn: () => api<Monitor[]>(workspaceWide.path("/api/monitors")) });
  const [projectId, setProjectId] = useProjectChoice();
  const [title, setTitle] = useState(item?.title ?? "");
  const [message, setMessage] = useState(item?.message ?? "");
  const [pageId, setPageId] = useState(item?.page_id ?? "");
  // The form picks one monitor. Keep a wider set made through the API until the user picks another.
  const initialMonitors = item?.monitor_ids ?? [];
  const [monitorId, setMonitorId] = useState(initialMonitors.length > 1 ? keepMonitors : (initialMonitors[0] ?? ""));
  const [starts, setStarts] = useState(toLocalInput(item?.starts_at));
  const [ends, setEnds] = useState(toLocalInput(item?.ends_at ?? new Date(Date.now() + 2 * 60 * 60 * 1000).toISOString()));
  const [fields, setFields] = useState<Record<string, string>>({});
  const save = useMutation({
    mutationFn: () =>
      api<Maintenance>(item ? `/api/maintenances/${item.id}` : "/api/maintenances", {
        method: item ? "PUT" : "POST",
        body: JSON.stringify({
          title,
          message,
          page_id: pageId,
          monitor_ids: monitorId === keepMonitors ? initialMonitors : monitorId ? [monitorId] : [],
          starts_at: fromLocalInput(starts),
          ends_at: fromLocalInput(ends),
          project_id: projectId,
        }),
      }),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["maintenances"] });
      onDone();
    },
    onError: (error) => setFields(error instanceof ApiError ? error.fields ?? {} : {}),
  });

  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    save.mutate();
  }

  return (
    <form className="space-y-3" onSubmit={submit}>
      <Field label="Title" error={fields.title}><Input value={title} onChange={(event) => setTitle(event.target.value)} required /></Field>
      <Field label="Message" error={fields.message}><Textarea value={message} onChange={(event) => setMessage(event.target.value)} required /></Field>
      {item ? null : <ProjectField value={projectId} onChange={setProjectId} error={fields.project_id} />}
      <Field label="Status page" error={fields.page_id}>
        <Select
          value={pageId}
          options={(pages.data ?? []).map((page) => ({ value: page.id, label: page.title }))}
          onChange={setPageId}
          aria-label="Status page"
        />
      </Field>
      <Field label="Monitor">
        <Select
          value={monitorId}
          options={[
            { value: "", label: "Every monitor on the page" },
            ...(initialMonitors.length > 1 ? [{ value: keepMonitors, label: `Keep current ${initialMonitors.length} monitors` }] : []),
            ...(monitors.data ?? []).map((monitor) => ({ value: monitor.id, label: monitor.name })),
          ]}
          onChange={setMonitorId}
          aria-label="Monitor"
        />
      </Field>
      <Field label="Starts" error={fields.starts_at}><Input type="datetime-local" value={starts} onChange={(event) => setStarts(event.target.value)} required /></Field>
      <Field label="Ends" error={fields.ends_at}><Input type="datetime-local" value={ends} onChange={(event) => setEnds(event.target.value)} required /></Field>
      <ErrorText error={save.error} />
      <div className="flex justify-end pt-1">
        <Button type="submit" variant="primary" disabled={save.isPending || !pageId}>{save.isPending ? <><Spinner />Saving…</> : item ? "Save" : "Schedule"}</Button>
      </div>
    </form>
  );
}

function CardText({ title, body }: { title: string; body?: string }) {
  return (
    <div className="flex min-h-32 flex-col items-center justify-center px-4 py-8 text-center">
      <p className="text-[0.875rem] font-medium text-[var(--ui-fg)]">{title}</p>
      {body ? <p className="mx-auto mt-1 max-w-[28rem] text-[0.8125rem] text-[var(--ui-fg-faint)]">{body}</p> : null}
    </div>
  );
}
