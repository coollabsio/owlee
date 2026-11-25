import type { ReactNode } from "react";
import { InfoCircle } from "reicon-react";
import { Segmented } from "@/components/ui";
import { type ApiScope, type ErrorApp, type Project, SCOPE_ACCESSES, type ScopeAccess, type ScopeProduct, type TokenResources } from "@/lib/api";

export type AccessLevel = "none" | ScopeAccess;

const LEVEL_LABELS: Record<AccessLevel, string> = { none: "No access", read: "Read", write: "Write", delete: "Delete" };

const LEVEL_SUMMARY: Record<AccessLevel, string> = {
  none: "No access.",
  read: "Can read.",
  write: "Can read and write.",
  delete: "Full access: can read, write, and delete.",
};

/** The highest access the scopes grant on a product. Levels are hierarchical, so this is the whole picture. */
export function accessLevel(scopes: readonly ApiScope[], product: ScopeProduct): AccessLevel {
  return [...SCOPE_ACCESSES].reverse().find((access) => scopes.includes(`${product}:${access}`)) ?? "none";
}

function withAccessLevel(scopes: readonly ApiScope[], product: ScopeProduct, level: AccessLevel): ApiScope[] {
  const granted = level === "none" ? [] : SCOPE_ACCESSES.slice(0, SCOPE_ACCESSES.indexOf(level) + 1);
  return [...scopes.filter((scope) => !scope.startsWith(`${product}:`)), ...granted.map((access): ApiScope => `${product}:${access}`)];
}

/** A hint icon. The global icon tooltip shows the aria-label on hover and focus. */
function InfoTip({ text }: { text: string }) {
  return (
    <button type="button" className="icon-button size-5!" aria-label={text}>
      <InfoCircle className="size-3.5" aria-hidden="true" />
    </button>
  );
}

export type AccessRow = { product: ScopeProduct; label: string; hint: string; accesses: readonly ScopeAccess[]; details?: ReactNode };

/** One row per product with a No access / Read / Write / Delete choice. `readOnly` limits every row to Read. */
export function AccessPicker({ rows, scopes, onChange, readOnly }: { rows: AccessRow[]; scopes: ApiScope[]; onChange: (scopes: ApiScope[]) => void; readOnly?: boolean }) {
  return (
    <div className="flex flex-col gap-3">
      {rows.map((row) => {
        const level = accessLevel(scopes, row.product);
        return (
          <div key={row.product} className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <span className="inline-flex items-center gap-1 text-[0.8125rem] font-medium text-[var(--ui-fg)]">
                {row.label}
                <InfoTip text={row.hint} />
              </span>
              <Segmented
                aria-label={`${row.label} access`}
                value={level}
                onChange={(next) => onChange(withAccessLevel(scopes, row.product, next))}
                options={(["none", ...row.accesses] as const).map((option) => {
                  const locked = readOnly === true && (option === "write" || option === "delete");
                  const included = option !== "none" && level !== "none" && SCOPE_ACCESSES.indexOf(option) < SCOPE_ACCESSES.indexOf(level);
                  return { value: option, label: LEVEL_LABELS[option], disabled: locked, included, title: locked ? "Only workspace owners and admins can grant write or delete." : undefined };
                })}
              />
            </div>
            <p className="-mt-2 text-right text-[0.75rem] text-[var(--ui-fg-faint)]">{LEVEL_SUMMARY[level]}</p>
            {level !== "none" && row.details ? <div className="border-l border-[var(--ui-divider)] pl-4">{row.details}</div> : null}
          </div>
        );
      })}
    </div>
  );
}

/** The form state of a resource limit. `all` keeps the lists, so clearing and re-checking it restores the selection. */
export type ResourceSelection = { all: boolean; projects: string[]; error_apps: string[] };

export const ALL_RESOURCES: ResourceSelection = { all: true, projects: [], error_apps: [] };

export function toTokenResources(selection: ResourceSelection): TokenResources {
  return selection.all ? null : { projects: selection.projects, error_apps: selection.error_apps };
}

export function isResourceSelectionEmpty(selection: ResourceSelection) {
  return !selection.all && selection.projects.length === 0 && selection.error_apps.length === 0;
}

function Check({ checked, disabled, onChange, children }: { checked: boolean; disabled?: boolean; onChange: (checked: boolean) => void; children: ReactNode }) {
  return (
    <label className="flex items-center gap-2 text-[0.8125rem] text-[var(--ui-fg)] has-disabled:text-[var(--ui-fg-dim)]">
      <input type="checkbox" className="size-4 shrink-0" checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} />
      <span className="min-w-0 truncate">{children}</span>
    </label>
  );
}

const toggle = (ids: string[], id: string, on: boolean) => (on ? [...ids, id] : ids.filter((other) => other !== id));

/** Projects with their error apps nested under them. A checked project covers its apps, so they show checked and locked. */
export function ResourcePicker({
  projects,
  apps,
  value,
  onChange,
  loading,
}: {
  projects: Project[];
  apps: ErrorApp[];
  value: ResourceSelection;
  onChange: (value: ResourceSelection) => void;
  loading?: boolean;
}) {
  function setProject(project: string, on: boolean) {
    // The project now covers its apps, so their own entries would only linger as hidden picks.
    const ownApps = new Set(apps.filter((app) => app.projectId === project).map((app) => app.id));
    onChange({ ...value, projects: toggle(value.projects, project, on), error_apps: on ? value.error_apps.filter((id) => !ownApps.has(id)) : value.error_apps });
  }
  return (
    <div role="group" aria-label="Resources" className="flex flex-col gap-3">
      <Check checked={value.all} onChange={(all) => onChange({ ...value, all })}>All projects and apps, including future ones</Check>
      {value.all ? null : loading ? (
        <p className="text-[0.75rem] text-[var(--ui-fg-faint)]">Loading projects…</p>
      ) : (
        <div className="-mx-1 flex max-h-72 flex-col gap-3 overflow-y-auto px-1">
          {projects.map((project) => {
            const covered = value.projects.includes(project.id);
            const projectApps = apps.filter((app) => app.projectId === project.id);
            return (
              <div key={project.id} className="flex flex-col gap-2">
                <Check checked={covered} onChange={(on) => setProject(project.id, on)}>{project.name}</Check>
                {projectApps.length ? (
                  <div className="flex flex-col gap-2 pl-6">
                    {projectApps.map((app) => (
                      <Check
                        key={app.id}
                        checked={covered || value.error_apps.includes(app.id)}
                        disabled={covered}
                        onChange={(on) => onChange({ ...value, error_apps: toggle(value.error_apps, app.id, on) })}
                      >
                        {app.name}
                      </Check>
                    ))}
                  </div>
                ) : null}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
