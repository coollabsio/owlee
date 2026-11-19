import { createContext, useContext, useState, useSyncExternalStore } from "react";
import type { Project, User, UserWorkspace, WorkspaceRole } from "@/lib/api";
import { queryClient } from "@/lib/query-client";

const workspaceKey = "owlee:workspace";
const projectKey = (workspaceId: string) => `owlee:project:${workspaceId}`;

/** `all`, or the id of one project in the current workspace. */
export type ProjectFilter = "all" | (string & {});

// A `?workspace=` link picks the workspace once; after that it is the saved choice,
// so switching later is not overridden by a stale query string.
const linked = new URLSearchParams(window.location.search).get("workspace");
if (linked) {
  localStorage.setItem(workspaceKey, linked);
  const url = new URL(window.location.href);
  url.searchParams.delete("workspace");
  window.history.replaceState(window.history.state, "", url);
}

// Per tab: localStorage is shared, so another tab's switch must not move this tab's requests.
// localStorage only seeds a new tab.
let tabWorkspace = localStorage.getItem(workspaceKey);

const listeners = new Set<() => void>();
let version = 0;

function changed() {
  version += 1;
  for (const listener of listeners) listener();
}

/** Re-renders the caller when the saved workspace or project changes. */
export function useSelectionVersion() {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => version,
  );
}

/** The saved workspace when the user is still a member, else their first one. */
export function workspaceFor(me: User | undefined): UserWorkspace | null {
  const workspaces = me?.workspaces ?? [];
  return workspaces.find((workspace) => workspace.id === tabWorkspace) ?? workspaces[0] ?? null;
}

/** For the `X-Owlee-Workspace` header; reads the cached `me`. */
export function currentWorkspaceId() {
  return workspaceFor(queryClient.getQueryData<User>(["me"]))?.id ?? null;
}

/** Every cached record belongs to the old workspace, so the cache is dropped. */
export function switchWorkspace(id: string) {
  tabWorkspace = id;
  localStorage.setItem(workspaceKey, id);
  resetWorkspace();
}

/** After leaving or deleting the current workspace: refetch `me` and fall back to another one. */
export function resetWorkspace() {
  queryClient.clear();
  changed();
}

export function savedProject(workspaceId: string): ProjectFilter {
  return localStorage.getItem(projectKey(workspaceId)) ?? "all";
}

export function selectProject(workspaceId: string, project: ProjectFilter) {
  localStorage.setItem(projectKey(workspaceId), project);
  changed();
}

export const workspaceRoleLabel: Record<WorkspaceRole, string> = { owner: "Owner", admin: "Admin", member: "Member" };

export type WorkspaceScope = {
  workspace: UserWorkspace;
  projects: Project[];
  project: ProjectFilter;
};

export const WorkspaceContext = createContext<WorkspaceScope | null>(null);

/** Only inside the signed-in shell, where `RequireWorkspace` provides it. */
export function useWorkspace(): WorkspaceScope {
  const scope = useContext(WorkspaceContext);
  if (!scope) throw new Error("useWorkspace outside RequireWorkspace");
  return scope;
}

export function withProject(path: string, project: ProjectFilter) {
  if (project === "all") return path;
  return `${path}${path.includes("?") ? "&" : "?"}project=${encodeURIComponent(project)}`;
}

/**
 * Query key and URL for a list of project-owned records. The key starts with
 * `name`, so `invalidateQueries({ queryKey: [name] })` still matches it.
 * `allProjects` lists the whole workspace, for pickers that may cross projects.
 */
export function useListScope({ allProjects = false } = {}) {
  const { workspace, project: current } = useWorkspace();
  const project = allProjects ? "all" : current;
  return {
    key: (name: string, ...rest: unknown[]) => [name, workspace.id, project, ...rest],
    path: (path: string) => withProject(path, project),
  };
}

/**
 * The project a create form submits: the user's pick, else the current project, else the first.
 * Derived on each render, so it follows a project switch and a late projects load.
 */
export function useProjectChoice() {
  const { projects, project } = useWorkspace();
  const [picked, setPicked] = useState<string | null>(null);
  const fallback = project === "all" ? (projects[0]?.id ?? "") : project;
  return [picked ?? fallback, setPicked] as const;
}
