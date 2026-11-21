import { useMutation } from "@tanstack/react-query";
import { type FormEvent, type ReactNode, useEffect, useRef, useState } from "react";
import { Link, useNavigate } from "react-router";
import { Check, ChevronExpandY, Plus, Settings } from "reicon-react";
import { ProjectDot } from "@/components/project";
import { Button, ErrorText, Input } from "@/components/ui";
import { createWorkspace } from "@/lib/api";
import { useMe } from "@/lib/me";
import { selectProject, switchWorkspace, useWorkspace } from "@/lib/workspace";

/**
 * One sidebar control for the current workspace and project, above the search row.
 * The menu lists the projects first (the frequent switch), then the workspaces.
 */
export function ScopeSwitcher({ onNavigate }: { onNavigate?: () => void }) {
  const navigate = useNavigate();
  const me = useMe();
  const { workspace, projects, project } = useWorkspace();
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const menu = useMenu(() => setCreating(false));
  const current = projects.find((item) => item.id === project);
  const projectName = current?.name ?? "All projects";

  function done() {
    menu.close();
    onNavigate?.();
  }

  function chooseProject(id: string) {
    selectProject(workspace.id, id);
    menu.close();
  }

  function openWorkspace(id: string) {
    done();
    if (id === workspace.id) return;
    navigate("/");
    switchWorkspace(id);
  }

  const create = useMutation({
    mutationFn: () => createWorkspace(name.trim()),
    onSuccess: (created) => {
      setName("");
      openWorkspace(created.id);
    },
  });

  function submit(event: FormEvent) {
    event.preventDefault();
    create.mutate();
  }

  return (
    <div className="scope-switcher profile-menu" ref={menu.root}>
      <button
        type="button"
        className="scope-trigger"
        title={`${workspace.name} / ${projectName}`}
        aria-label={`Workspace ${workspace.name}, project ${projectName}`}
        aria-haspopup="true"
        aria-expanded={menu.open}
        onClick={menu.toggle}
      >
        <span className="scope-avatar" aria-hidden="true">
          {workspace.name.trim().charAt(0).toUpperCase() || "W"}
        </span>
        <span className="scope-text">
          <span className="scope-workspace">{workspace.name}</span>
          <span className="scope-project">
            <ProjectDot color={current?.color ?? "var(--ui-fg-faint)"} />
            <span className="truncate">{projectName}</span>
          </span>
        </span>
        <ChevronExpandY className="scope-chevron" aria-hidden="true" />
      </button>
      {menu.open ? (
        <div className="profile-menu-panel">
          <div className="scope-heading">Projects</div>
          <Option selected={project === "all"} onClick={() => chooseProject("all")}>
            <ProjectDot color="var(--ui-fg-faint)" />
            <span className="truncate">All projects</span>
          </Option>
          {projects.map((item) => (
            <Option key={item.id} selected={item.id === project} onClick={() => chooseProject(item.id)}>
              <ProjectDot color={item.color} />
              <span className="truncate">{item.name}</span>
            </Option>
          ))}
          <Link className="listbox-option scope-muted" to="/settings/projects" onClick={done}>
            <Settings className="size-3.5 shrink-0" aria-hidden="true" />
            Manage projects
          </Link>
          <div className="profile-menu-divider" />
          <div className="scope-heading">Workspaces</div>
          {(me.data?.workspaces ?? []).map((item) => (
            <Option key={item.id} selected={item.id === workspace.id} onClick={() => openWorkspace(item.id)}>
              <span className="scope-avatar is-small" aria-hidden="true">
                {item.name.trim().charAt(0).toUpperCase() || "W"}
              </span>
              <span className="truncate">{item.name}</span>
            </Option>
          ))}
          {creating ? (
            <form className="flex flex-col gap-2 p-1" onSubmit={submit}>
              <Input autoFocus required value={name} onChange={(event) => setName(event.target.value)} placeholder="Workspace name" aria-label="Workspace name" />
              <ErrorText error={create.error} />
              <div className="flex justify-end gap-2">
                <Button type="button" size="sm" onClick={() => setCreating(false)}>Cancel</Button>
                <Button type="submit" size="sm" variant="primary" disabled={create.isPending || !name.trim()}>
                  {create.isPending ? "Creating…" : "Create"}
                </Button>
              </div>
            </form>
          ) : (
            <button type="button" className="listbox-option scope-muted" onClick={() => setCreating(true)}>
              <Plus className="size-3.5 shrink-0" aria-hidden="true" />
              Create workspace
            </button>
          )}
          <Link className="listbox-option scope-muted" to="/settings" onClick={done}>
            <Settings className="size-3.5 shrink-0" aria-hidden="true" />
            Workspace settings
          </Link>
        </div>
      ) : null}
    </div>
  );
}

function Option({ selected, onClick, children }: { selected: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button type="button" className="listbox-option" onClick={onClick}>
      <span className="flex min-w-0 items-center gap-2">{children}</span>
      {selected ? <Check className="size-3.5 shrink-0" aria-hidden="true" /> : null}
    </button>
  );
}

function useMenu(onClose?: () => void) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const close = () => {
    setOpen(false);
    onClose?.();
  };
  const closeRef = useRef(close);
  closeRef.current = close;

  useEffect(() => {
    if (!open) return;
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.stopPropagation();
        closeRef.current();
      }
    }
    function onPointer(event: PointerEvent) {
      if (!root.current?.contains(event.target as Node)) closeRef.current();
    }
    document.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onPointer);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("pointerdown", onPointer);
    };
  }, [open]);

  return { open, root, close, toggle: () => (open ? close() : setOpen(true)) };
}
