import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, ChevronDown, X } from "reicon-react";
import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { PriorityMark, Sparkline } from "@/components/error-charts";
import { Badge } from "@/components/issue-badge";
import { Button, Input, LayerCard, PageHeader, Select } from "@/components/ui";
import { api, type Issue, type IssueFeed, type Project } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { cn, fullTime, oneOf, splitTitle, timeAgo } from "@/lib/utils";
import type { IssueLocationState } from "@/pages/issue-detail";

const statuses = [
  { id: "unresolved", label: "Unresolved" },
  { id: "resolved", label: "Resolved" },
  { id: "ignored", label: "Archived" },
  { id: "all", label: "All" },
] as const;
const ranges = ["24h", "7d", "14d", "30d"] as const;
const sorts = [
  { id: "last_seen", label: "Last seen" },
  { id: "age", label: "Age" },
  { id: "events", label: "Events" },
  { id: "users", label: "Users" },
] as const;

type Status = (typeof statuses)[number]["id"];

/** The whole view lives in the URL, so reloads, back/forward, and shared links reproduce it. */
type Filters = {
  status: Status;
  project: string;
  environment: string;
  since: (typeof ranges)[number];
  q: string;
  sort: (typeof sorts)[number]["id"];
};

const defaults: Filters = { status: "unresolved", project: "", environment: "", since: "14d", q: "", sort: "last_seen" };

function parseFilters(params: URLSearchParams): Filters {
  return {
    status: oneOf(params.get("status"), statuses.map((item) => item.id), defaults.status),
    project: params.get("project") ?? defaults.project,
    environment: params.get("environment") ?? defaults.environment,
    since: oneOf(params.get("since"), ranges, defaults.since),
    q: params.get("q") ?? defaults.q,
    sort: oneOf(params.get("sort"), sorts.map((item) => item.id), defaults.sort),
  };
}

function filterSearch(filters: Filters) {
  const search = new URLSearchParams();
  for (const key of Object.keys(defaults) as (keyof Filters)[]) {
    if (filters[key] !== defaults[key]) search.set(key, filters[key]);
  }
  return search;
}

function feedSearch(filters: Filters) {
  const search = new URLSearchParams({ since: filters.since, sort: filters.sort });
  if (filters.status !== "all") search.set("status", filters.status);
  if (filters.project) search.set("project_id", filters.project);
  if (filters.environment) search.set("environment", filters.environment);
  if (filters.q) search.set("q", filters.q);
  return search.toString();
}

type BulkAction = { status: Issue["status"]; label: string; highlighted?: boolean };
const resolve: BulkAction = { status: "resolved", label: "Resolve", highlighted: true };
const archive: BulkAction = { status: "ignored", label: "Archive" };
const reopen: BulkAction = { status: "unresolved", label: "Reopen" };
const bulkActions: Record<Status, BulkAction[]> = {
  unresolved: [resolve, archive],
  resolved: [reopen],
  ignored: [reopen],
  all: [resolve, archive, reopen],
};

const DAY_MS = 24 * 60 * 60 * 1000;

export function IssuesPage() {
  const navigate = useNavigate();
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const [searchParams, setSearchParams] = useSearchParams();
  const filters = parseFilters(searchParams);
  const viewQuery = filterSearch(filters).toString();
  const feedQuery = feedSearch(filters);
  function setFilters(patch: Partial<Filters>) {
    setSearchParams(filterSearch({ ...filters, ...patch }));
  }
  const [draft, setDraft] = useState("");
  const [trend, setTrend] = useState<"24h" | "14d">("14d");
  // Tagged with the feed query, so changing any filter drops the selection.
  const [selection, setSelection] = useState<{ query: string; ids: string[] }>({ query: "", ids: [] });

  const projects = useQuery({ queryKey: ["projects"], queryFn: () => api<Project[]>("/api/error-projects") });
  const feed = useQuery({
    queryKey: ["issues", feedQuery],
    queryFn: () => api<IssueFeed>(`/api/error-issues?${feedQuery}`),
    refetchInterval: 4000,
  });
  const bulk = useMutation({
    mutationFn: (body: { ids: string[]; status: Issue["status"] }) =>
      api("/api/error-issues/bulk", { method: "POST", body: JSON.stringify(body) }),
    onSuccess: async () => {
      setSelection({ query: "", ids: [] });
      await client.invalidateQueries({ queryKey: ["issues"] });
    },
  });

  const issues = feed.data?.issues ?? [];
  const counts = feed.data?.counts;
  const total = counts ? counts.unresolved + counts.resolved + counts.ignored : 0;
  const visible = new Set(issues.map((issue) => issue.id));
  const selected = selection.query === feedQuery ? selection.ids.filter((id) => visible.has(id)) : [];
  const allChecked = issues.length > 0 && selected.length === issues.length;
  const waitingProject = projects.data?.find((project) => project.id === filters.project && !project.lastEventAt);
  const narrowed = total > 0 || filters.q !== "" || filters.environment !== "";

  function select(ids: string[]) {
    setSelection({ query: feedQuery, ids });
  }

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Issues" description="Errors from your Sentry SDKs, grouped into issues. The list refreshes every few seconds." />
      <LayerCard
        title="Issue feed"
        flush
        actions={
          <>
            {canEdit && selected.length > 0 ? (
              <>
                <span className="text-[12px] text-[var(--ui-fg-faint)]">{selected.length} selected</span>
                {bulkActions[filters.status].map((action) => (
                  <Button
                    key={action.status}
                    variant={action.highlighted ? "highlighted" : "default"}
                    size="sm"
                    disabled={bulk.isPending}
                    onClick={() => bulk.mutate({ ids: selected, status: action.status })}
                  >
                    {action.label}
                  </Button>
                ))}
              </>
            ) : null}
            <ViewsMenu current={viewQuery} onApply={(query) => setSearchParams(filterSearch(parseFilters(new URLSearchParams(query))))} />
          </>
        }
      >
        <div className="min-w-0 border-b border-[var(--ui-divider)] px-3">
          <nav className="-mb-px flex gap-1 overflow-x-auto overflow-y-hidden" aria-label="Issue status">
            {statuses.map((item) => {
              const count = !counts ? null : item.id === "all" ? total : counts[item.id];
              return (
                <button
                  key={item.id}
                  type="button"
                  aria-current={item.id === filters.status ? "page" : undefined}
                  className={cn(
                    "h-10 shrink-0 border-b-2 px-2.5 text-[13px] font-medium transition-colors",
                    item.id === filters.status
                      ? "border-[var(--ui-accent)] text-[var(--ui-fg)]"
                      : "border-transparent text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]",
                  )}
                  onClick={() => setFilters({ status: item.id })}
                >
                  {item.label}
                  {count !== null ? <span className="ml-1.5 font-normal tabular-nums text-[var(--ui-fg-faint)]">{count.toLocaleString()}</span> : null}
                </button>
              );
            })}
          </nav>
        </div>
        <div className="flex flex-wrap items-center gap-2 border-b border-[var(--ui-divider)] p-3">
          <form
            className="flex h-8 min-w-[240px] flex-1 items-center gap-1 overflow-hidden rounded-lg border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] px-1.5 focus-within:border-[var(--ui-accent)]"
            onSubmit={(event) => {
              event.preventDefault();
              setFilters({ q: draft.trim() });
              setDraft("");
            }}
          >
            {filters.q ? <Token label={filters.q} onRemove={() => setFilters({ q: "" })} /> : null}
            <input
              className="h-full min-w-[8rem] flex-1 bg-transparent px-1 text-[14px] text-[var(--ui-fg)] outline-none placeholder:text-[var(--ui-fg-faint)]"
              placeholder="Search title, culprit, or id"
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
            />
          </form>
          <div className="w-40">
            <Select
              aria-label="Project"
              value={filters.project}
              onChange={(project) => setFilters({ project })}
              options={[{ value: "", label: "All projects" }, ...(projects.data ?? []).map((project) => ({ value: project.id, label: project.name }))]}
            />
          </div>
          <div className="w-44">
            <Select
              aria-label="Environment"
              value={filters.environment}
              onChange={(environment) => setFilters({ environment })}
              options={[{ value: "", label: "All environments" }, ...(feed.data?.environments ?? []).map((item) => ({ value: item, label: item }))]}
            />
          </div>
          <div className="w-28">
            <Select
              aria-label="Time range"
              value={filters.since}
              onChange={(since) => setFilters({ since })}
              options={ranges.map((item) => ({ value: item, label: `Last ${item}` }))}
            />
          </div>
          <div className="w-32">
            <Select
              aria-label="Sort"
              value={filters.sort}
              onChange={(sort) => setFilters({ sort })}
              options={sorts.map((item) => ({ value: item.id, label: item.label }))}
            />
          </div>
        </div>
        {feed.isLoading ? <p className="p-4 text-[13px] text-[var(--ui-fg-faint)]">Loading issues...</p> : null}
        {feed.isError ? <p className="p-4 text-[13px] text-[var(--ui-error)]">{(feed.error as Error).message}</p> : null}
        {projects.data?.length === 0 ? (
          <div className="flex min-h-40 flex-col items-center justify-center px-4 py-10 text-center">
            <h2 className="text-[14px] font-medium text-[var(--ui-fg)]">No projects yet</h2>
            <p className="mx-auto mt-1 max-w-[28rem] text-[13px] text-[var(--ui-fg-faint)]">
              {canEdit
                ? "Create a project to get a DSN and a step-by-step SDK guide. Its errors show up here."
                : "An editor has to create a project before errors can be collected."}
            </p>
            {canEdit ? (
              <Link className="button button-highlighted mt-4" to="/error-projects">
                Create a project
              </Link>
            ) : null}
          </div>
        ) : null}
        {projects.data?.length !== 0 && feed.data && issues.length === 0 ? (
          <div className="flex min-h-40 flex-col items-center justify-center px-4 py-10 text-center">
            <h2 className="text-[14px] font-medium text-[var(--ui-fg)]">{narrowed ? "No issues match this view" : "No issues in this view"}</h2>
            <p className="mx-auto mt-1 max-w-[28rem] text-[13px] text-[var(--ui-fg-faint)]">
              {narrowed
                ? "Try another tab, a longer time range, or a broader search."
                : "Point a Sentry SDK at a project DSN and throw an error. Events in the selected range show up here."}
            </p>
            {waitingProject ? (
              <Link className="button mt-4" to={`/error-projects/${waitingProject.id}`}>
                Open the {waitingProject.name} setup guide
              </Link>
            ) : null}
          </div>
        ) : null}
        {issues.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="cool-table min-w-[960px]">
              <thead>
                <tr>
                  {canEdit ? (
                    <th className="w-10 !pr-0">
                      <input
                        type="checkbox"
                        className="align-middle"
                        ref={(input) => {
                          if (input) input.indeterminate = selected.length > 0 && !allChecked;
                        }}
                        checked={allChecked}
                        onChange={() => select(allChecked ? [] : issues.map((issue) => issue.id))}
                        aria-label="Select all issues"
                      />
                    </th>
                  ) : null}
                  <th>Issue</th>
                  <th>Last seen</th>
                  <th>Age</th>
                  <th>
                    <span className="inline-flex items-center gap-2">
                      Trend
                      <span className="inline-flex gap-px rounded-md border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] p-px">
                        {(["24h", "14d"] as const).map((item) => (
                          <button
                            key={item}
                            type="button"
                            className={cn(
                              "rounded-[5px] px-1.5 py-0.5 text-[11px] text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]",
                              trend === item && "bg-[var(--ui-hover)] text-[var(--ui-fg)]",
                            )}
                            onClick={() => setTrend(item)}
                          >
                            {item}
                          </button>
                        ))}
                      </span>
                    </span>
                  </th>
                  <th>Events</th>
                  <th>Users</th>
                  <th>Priority</th>
                </tr>
              </thead>
              <tbody>
                {issues.map((issue) => (
                  <IssueRowView
                    key={issue.id}
                    issue={issue}
                    trend={trend}
                    showProject={!filters.project}
                    checked={selected.includes(issue.id)}
                    onCheck={
                      canEdit ? (checked) => select(checked ? [...selected, issue.id] : selected.filter((id) => id !== issue.id)) : undefined
                    }
                    onOpen={() =>
                      navigate(`/errors/${issue.shortId || issue.id}`, { state: { listSearch: viewQuery } satisfies IssueLocationState })
                    }
                  />
                ))}
              </tbody>
            </table>
          </div>
        ) : null}
        {feed.data?.truncated ? (
          <p className="border-t border-[var(--ui-divider)] px-4 py-2.5 text-[12px] text-[var(--ui-fg-faint)]">
            Showing the first {issues.length} issues. Narrow the filters to see more.
          </p>
        ) : null}
      </LayerCard>
    </div>
  );
}

type SavedView = { name: string; query: string };
const VIEWS_KEY = "owlee-error-views";

function ViewsMenu({ current, onApply }: { current: string; onApply: (query: string) => void }) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [views, setViews] = useState<SavedView[]>(() => JSON.parse(localStorage.getItem(VIEWS_KEY) ?? "[]") as SavedView[]);
  const root = useRef<HTMLDivElement>(null);
  const active = views.find((view) => view.query === current);

  useEffect(() => {
    if (!open) return;
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") setOpen(false);
    }
    function onPointer(event: PointerEvent) {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    }
    document.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onPointer);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("pointerdown", onPointer);
    };
  }, [open]);

  function store(next: SavedView[]) {
    setViews(next);
    localStorage.setItem(VIEWS_KEY, JSON.stringify(next));
  }

  function save() {
    const trimmed = name.trim();
    if (!trimmed) return;
    store([...views.filter((view) => view.name !== trimmed), { name: trimmed, query: current }]);
    setName("");
  }

  return (
    <div className="relative" ref={root}>
      <Button type="button" size="sm" aria-haspopup="true" aria-expanded={open} onClick={() => setOpen((value) => !value)}>
        <span className="max-w-[10rem] truncate">{active ? active.name : "Views"}</span>
        <ChevronDown className="ml-1 size-3.5 text-[var(--ui-fg-faint)]" aria-hidden="true" />
      </Button>
      {open ? (
        <div className="absolute right-0 z-20 mt-1 flex w-64 flex-col gap-px rounded-[10px] border border-[var(--ui-menu-border)] bg-[var(--ui-menu-bg)] p-1 shadow-[var(--ui-menu-shadow)]">
          {views.length === 0 ? <p className="px-2 py-1.5 text-[12px] text-[var(--ui-fg-faint)]">No saved views yet.</p> : null}
          {views.map((view) => (
            <div key={view.name} className="flex items-center gap-1">
              <button
                type="button"
                className="listbox-option min-w-0 flex-1"
                onClick={() => {
                  onApply(view.query);
                  setOpen(false);
                }}
              >
                <span className="truncate">{view.name}</span>
                {view === active ? <Check className="size-3.5 shrink-0 text-[var(--ui-fg)]" aria-label="Active view" /> : null}
              </button>
              <button
                type="button"
                className="icon-button size-7 shrink-0"
                aria-label={`Delete view ${view.name}`}
                onClick={() => store(views.filter((item) => item !== view))}
              >
                <X className="size-3.5" />
              </button>
            </div>
          ))}
          <form
            className="mt-1 flex flex-col gap-2 border-t border-[var(--ui-divider)] p-1 pt-2"
            onSubmit={(event) => {
              event.preventDefault();
              save();
            }}
          >
            <Input placeholder="Name this view" aria-label="View name" value={name} onChange={(event) => setName(event.target.value)} />
            <Button variant="highlighted" size="sm" type="submit" disabled={!name.trim()}>
              Save current view
            </Button>
          </form>
        </div>
      ) : null}
    </div>
  );
}

function IssueRowView({
  issue,
  trend,
  showProject,
  checked,
  onCheck,
  onOpen,
}: {
  issue: Issue;
  trend: "24h" | "14d";
  showProject: boolean;
  checked: boolean;
  onCheck?: (checked: boolean) => void;
  onOpen: () => void;
}) {
  const parts = splitTitle(issue.title);
  const isNew = Date.now() - new Date(issue.firstSeen).getTime() < DAY_MS;
  return (
    <tr className="clickable-row" onClick={onOpen}>
      {onCheck ? (
        <td className="!pr-0" onClick={(event) => event.stopPropagation()}>
          <input type="checkbox" className="align-middle" checked={checked} aria-label={`Select ${issue.shortId}`} onChange={(event) => onCheck(event.target.checked)} />
        </td>
      ) : null}
      <td>
        <div className="font-medium text-[var(--ui-fg)]">{parts.type}</div>
        {parts.message ? <div className="text-[13px] text-[var(--ui-fg-dim)]">{parts.message}</div> : null}
        <div className="mt-1 flex flex-wrap items-center gap-2 text-[12px] text-[var(--ui-fg-faint)]">
          <span className="font-mono text-[var(--ui-fg-dim)]">{issue.shortId}</span>
          {showProject ? <Badge>{issue.projectName}</Badge> : null}
          {isNew ? <Badge tone="info">New</Badge> : null}
          {issue.unhandled ? <Badge tone="fatal">Unhandled</Badge> : null}
          <span>{issue.culprit || "No culprit"}</span>
        </div>
      </td>
      <td className="whitespace-nowrap" title={fullTime(issue.lastSeen)}>
        {timeAgo(issue.lastSeen)}
      </td>
      <td className="whitespace-nowrap" title={fullTime(issue.firstSeen)}>
        {timeAgo(issue.firstSeen)}
      </td>
      <td>{trend === "24h" ? <Sparkline values={issue.trend24h} /> : <Sparkline values={issue.trend14d} />}</td>
      <td className="tabular-nums">{issue.periodEventCount}</td>
      <td className="tabular-nums">{issue.userCount}</td>
      <td>
        <PriorityMark priority={issue.priority} />
      </td>
    </tr>
  );
}

function Token({ label, onRemove }: { label: string; onRemove: () => void }) {
  return (
    <span className="inline-flex h-6 shrink-0 items-center gap-1 rounded-md border border-[var(--ui-control-border)] bg-[var(--ui-hover)] px-1.5 text-[12px] text-[var(--ui-fg)]">
      {label}
      <button type="button" className="text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]" onClick={onRemove} aria-label={`Remove ${label}`}>
        ×
      </button>
    </span>
  );
}
