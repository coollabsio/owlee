import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { type Column, DataTable } from "@/components/data-table";
import { Button, ConfirmDialog, Dialog, Empty, ErrorText, Field, FilterChips, FilterValue, Input, LayerCard, PageHeader, RowActions, Select, Skeleton, Spinner, StatusBadge, Textarea } from "@/components/ui";
import { ProjectField, ProjectTag } from "@/components/project";
import { ApiError, api, type Monitor, type StatusPage } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { useListScope, useProjectChoice } from "@/lib/workspace";

export function PagesPage() {
  const scope = useListScope();
  const query = useQuery({ queryKey: scope.key("pages"), queryFn: () => api<StatusPage[]>(scope.path("/api/pages")) });
  const canEdit = useCanEdit();
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [publishedFilter, setPublishedFilter] = useState<boolean | null>(null);
  const needle = search.trim().toLowerCase();
  const filtered = (query.data ?? []).filter(
    (page) =>
      (page.title.toLowerCase().includes(needle) || page.slug.toLowerCase().includes(needle)) &&
      (publishedFilter === null || page.published === publishedFilter),
  );
  const columns: Column<StatusPage>[] = [
    {
      key: "title",
      header: "Title",
      sort: (page) => page.title,
      cell: (page) => <><Link className="row-link" to={`/pages/${page.id}`}>{page.title}</Link><ProjectTag id={page.project_id} /></>,
    },
    { key: "slug", header: "Slug", className: "hidden sm:table-cell", cellClassName: "font-mono text-[0.75rem]", sort: (page) => page.slug, cell: (page) => `/${page.slug}` },
    { key: "components", header: "Components", className: "hidden md:table-cell", sort: (page) => page.components.length, cell: (page) => page.components.length },
    {
      key: "status",
      header: "Status",
      sort: (page) => (page.published ? "Published" : "Draft"),
      cell: (page) => (
        <FilterValue active={publishedFilter === page.published} label={page.published ? "Published" : "Draft"} onToggle={() => setPublishedFilter(publishedFilter === page.published ? null : page.published)}>
          <StatusBadge tone={page.published ? "success" : "neutral"}>{page.published ? "Published" : "Draft"}</StatusBadge>
        </FilterValue>
      ),
    },
    {
      key: "actions",
      header: "Actions",
      className: "text-right",
      cell: (page) => (
        <RowActions
          label={page.title}
          actions={[
            ...(canEdit ? [{ label: "Edit", onSelect: () => navigate(`/pages/${page.id}`) }] : []),
            ...(page.published ? [{ label: "View", onSelect: () => navigate(`/status/${page.slug}`) }] : []),
          ]}
        />
      ),
    },
  ];
  const filters = publishedFilter === null ? [] : [{ key: "status", label: `Status: ${publishedFilter ? "Published" : "Draft"}`, onRemove: () => setPublishedFilter(null) }];
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Status pages"
        description="Public pages at /status/your-slug."
      />
      {!query.isLoading && (query.data?.length ?? 0) === 0 ? (
        <Empty title="No status pages" body="Group monitors onto a page your customers can open without signing in." action={canEdit ? <Button variant="highlighted" onClick={() => setOpen(true)}>New page</Button> : undefined} />
      ) : (
        <LayerCard
          title="All pages"
          description="Customers open these pages without signing in."
          actions={canEdit ? <Button variant="highlighted" onClick={() => setOpen(true)}>New page</Button> : null}
          flush
        >
          <div className="flex items-center gap-2 border-b border-[var(--ui-divider)] p-2">
            <Input className="max-w-64" value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Search pages" aria-label="Search pages" />
            <FilterChips filters={filters} onClear={() => setPublishedFilter(null)} />
          </div>
          <DataTable columns={columns} rows={filtered} rowKey={(page) => page.id} loading={query.isLoading} />
          {!query.isLoading && filtered.length === 0 ? (
            <p className="flex h-12 items-center justify-center px-4 text-[0.8125rem] text-[var(--ui-fg-faint)]">No pages match your search or filters.</p>
          ) : null}
        </LayerCard>
      )}
      <Dialog open={canEdit && open} title="New status page" description="Pick the monitors to show. You can add a description and homepage after." onClose={() => setOpen(false)}>
        {open ? <NewPageForm /> : null}
      </Dialog>
    </div>
  );
}

function NewPageForm() {
  const navigate = useNavigate();
  const client = useQueryClient();
  const monitors = useAllMonitors();
  const [projectId, setProjectId] = useProjectChoice();
  const [title, setTitle] = useState("");
  const [slug, setSlug] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [fields, setFields] = useState<Record<string, string>>({});
  const create = useMutation({
    mutationFn: () =>
      api<StatusPage>("/api/pages", {
        method: "POST",
        body: JSON.stringify({ title, description: "", slug, homepage_url: "", published: true, monitor_ids: selected, project_id: projectId }),
      }),
    onSuccess: async (page) => {
      await client.invalidateQueries({ queryKey: ["pages"] });
      navigate(`/pages/${page.id}`);
    },
    onError: (error) => setFields(error instanceof ApiError ? error.fields ?? {} : {}),
  });
  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        setFields({});
        create.mutate();
      }}
    >
      <Field label="Title" error={fields.title}><Input value={title} onChange={(event) => setTitle(event.target.value)} required /></Field>
      <Field label="Slug" hint="public URL" error={fields.slug}><Input value={slug} onChange={(event) => setSlug(event.target.value)} placeholder="coollabs" required /></Field>
      <ProjectField value={projectId} onChange={setProjectId} error={fields.project_id} />
      <div role="group" aria-label="Monitors">
        <span className="field-label">Monitors</span>
        <MonitorPicker monitors={monitors.data ?? []} selected={selected} onChange={setSelected} />
        {fields.monitor_ids ? <p className="field-error">{fields.monitor_ids}</p> : null}
      </div>
      <ErrorText error={create.error} />
      <div className="flex justify-end">
        <Button type="submit" variant="highlighted" disabled={create.isPending}>{create.isPending ? <><Spinner />Creating…</> : "Create page"}</Button>
      </div>
    </form>
  );
}

function useAllMonitors() {
  const workspaceWide = useListScope({ allProjects: true });
  return useQuery({ queryKey: workspaceWide.key("monitors"), queryFn: () => api<Monitor[]>(workspaceWide.path("/api/monitors")) });
}

function MonitorPicker({ monitors, selected, onChange }: { monitors: Monitor[]; selected: string[]; onChange: (ids: string[]) => void }) {
  if (monitors.length === 0) return <p className="py-4 text-center text-[0.8125rem] text-[var(--ui-fg-faint)]">Create a monitor first.</p>;
  return (
    <div className="grid gap-2 sm:grid-cols-2">
      {monitors.map((monitor) => (
        <label key={monitor.id} className="check-row">
          <input
            type="checkbox"
            className="size-4 accent-[var(--ui-accent)]"
            checked={selected.includes(monitor.id)}
            onChange={(event) => onChange(event.target.checked ? [...selected, monitor.id] : selected.filter((item) => item !== monitor.id))}
          />
          {monitor.name}
        </label>
      ))}
    </div>
  );
}

export function PageEditor() {
  const { id = "" } = useParams();
  const navigate = useNavigate();
  const canEdit = useCanEdit();
  const client = useQueryClient();
  const existing = useQuery({
    queryKey: ["page", id],
    queryFn: () => api<StatusPage>(`/api/pages/${id}`),
  });
  const monitors = useAllMonitors();
  const page = existing.data;
  const [title, setTitle] = useState(page?.title ?? "");
  const [description, setDescription] = useState(page?.description ?? "");
  const [slug, setSlug] = useState(page?.slug ?? "");
  const [homepage, setHomepage] = useState(page?.homepage_url ?? "");
  const [published, setPublished] = useState(page?.published ?? true);
  const [selected, setSelected] = useState<string[]>(page?.components.map((component) => component.monitor_id) ?? []);
  const [fields, setFields] = useState<Record<string, string>>({});
  const [deleting, setDeleting] = useState(false);

  useEffect(() => {
    if (!page) return;
    setTitle(page.title);
    setDescription(page.description);
    setSlug(page.slug);
    setHomepage(page.homepage_url ?? "");
    setPublished(page.published);
    setSelected(page.components.map((component) => component.monitor_id));
  }, [page]);

  const save = useMutation({
    mutationFn: () =>
      api<StatusPage>(`/api/pages/${id}`, {
        method: "PUT",
        body: JSON.stringify({
          title,
          description,
          slug,
          homepage_url: homepage,
          published,
          monitor_ids: selected,
        }),
      }),
    onSuccess: async (saved) => {
      await client.invalidateQueries({ queryKey: ["pages"] });
      navigate(`/pages/${saved.id}`);
    },
    onError: (error) => setFields(error instanceof ApiError ? error.fields ?? {} : {}),
  });
  const remove = useMutation({
    mutationFn: () => api(`/api/pages/${id}`, { method: "DELETE" }),
    onSuccess: () => navigate("/pages"),
  });

  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    save.mutate();
  }

  if (existing.isLoading) {
    return (
      <div className="mx-auto flex w-full max-w-3xl flex-col gap-6" aria-busy="true">
        <div className="space-y-2">
          <Skeleton className="h-6 w-48" />
          <Skeleton className="h-3 w-72" />
        </div>
        <LayerCard><Skeleton className="h-48 w-full" /></LayerCard>
        <LayerCard><Skeleton className="h-24 w-full" /></LayerCard>
      </div>
    );
  }

  return (
    <form className="mx-auto flex w-full max-w-3xl flex-col gap-6" onSubmit={submit}>
      <PageHeader
        title={canEdit ? "Edit status page" : "Status page"}
        description="Choose a slug and the monitors that appear on it."
        actions={page?.published ? <Link to={`/status/${page.slug}`}><Button type="button">View public page</Button></Link> : null}
      />
      <fieldset disabled={!canEdit} className="contents">
      <LayerCard title="Details" description={page?.published ? `Public URL: /status/${page.slug}` : "What customers see at the top of the page."}>
        <div className="flex flex-col gap-4">
          <Field label="Title" error={fields.title}><Input value={title} onChange={(event) => setTitle(event.target.value)} required /></Field>
          <Field label="Description" error={fields.description}><Textarea value={description} onChange={(event) => setDescription(event.target.value)} /></Field>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Slug" hint="public URL" error={fields.slug}><Input value={slug} onChange={(event) => setSlug(event.target.value)} placeholder="coollabs" required /></Field>
            <Field label="Homepage" error={fields.homepage_url}><Input value={homepage} onChange={(event) => setHomepage(event.target.value)} placeholder="https://coolify.io" /></Field>
          </div>
          <Field label="Visibility">
            <Select
              value={published ? "published" : "draft"}
              options={[
                { value: "published", label: "Published" },
                { value: "draft", label: "Draft" },
              ]}
              onChange={(value) => setPublished(value === "published")}
              aria-label="Visibility"
            />
          </Field>
        </div>
      </LayerCard>
      <LayerCard title="Monitors" description="Each selected monitor shows as a component on the page.">
        <MonitorPicker monitors={monitors.data ?? []} selected={selected} onChange={setSelected} />
        {fields.monitor_ids ? <p className="field-error">{fields.monitor_ids}</p> : null}
      </LayerCard>
      </fieldset>
      <ErrorText error={save.error} />
      {canEdit ? (
        <div className="flex justify-between gap-2">
          <Button type="button" variant="danger" onClick={() => setDeleting(true)}>Delete</Button>
          <Button type="submit" variant="highlighted" disabled={save.isPending}>{save.isPending ? <><Spinner />Saving…</> : "Save page"}</Button>
        </div>
      ) : null}
      <ConfirmDialog
        open={deleting}
        title="Delete status page?"
        description={`${page?.title ?? "This page"} will be deleted and /status/${page?.slug ?? slug} will stop working. Its monitors are kept.`}
        pending={remove.isPending}
        error={remove.error}
        onConfirm={() => remove.mutate()}
        onClose={() => setDeleting(false)}
      />
    </form>
  );
}
