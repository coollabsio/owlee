import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { useNavigate } from "react-router";
import { ProjectDot } from "@/components/project";
import { Button, Dialog, ErrorText, Field, Input, LayerCard, PageHeader, Select, StatusBadge, useCopy } from "@/components/ui";
import {
  ApiError,
  api,
  type CreatedInvitation,
  type Invitation,
  type InviteRole,
  type Member,
  type Project,
} from "@/lib/api";
import { useCanEdit, useMe } from "@/lib/me";
import { formatTime } from "@/lib/utils";
import { resetWorkspace, useWorkspace, workspaceRoleLabel } from "@/lib/workspace";

const roleOptions: { value: InviteRole; label: string }[] = [
  { value: "member", label: "Member" },
  { value: "admin", label: "Admin" },
];

/** Stored lowercase `#rrggbb`, as the server expects. */
const projectColors = ["#6366f1", "#0ea5e9", "#10b981", "#84cc16", "#f59e0b", "#f97316", "#ef4444", "#ec4899", "#a855f7", "#64748b"];

export function WorkspaceSettingsPage() {
  const { workspace } = useWorkspace();
  const canEdit = useCanEdit();
  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-6">
      <PageHeader title="Workspace" description="Its name, its people, and the projects that group its records." />
      <NameCard key={workspace.name} />
      <ProjectsCard />
      <MembersCard />
      {canEdit ? <InvitationsCard /> : null}
      {workspace.role === "owner" ? <DeleteCard /> : null}
    </div>
  );
}

function NameCard() {
  const client = useQueryClient();
  const { workspace } = useWorkspace();
  const canEdit = useCanEdit();
  const [name, setName] = useState(workspace.name);
  const save = useMutation({
    mutationFn: () => api("/api/workspace", { method: "PUT", body: JSON.stringify({ name: name.trim() }) }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["me"] }),
  });
  function submit(event: FormEvent) {
    event.preventDefault();
    save.mutate();
  }
  return (
    <LayerCard title="General" description={`You are ${workspaceRoleLabel[workspace.role].toLowerCase()} of this workspace.`}>
      <form className="flex flex-col gap-3 sm:flex-row sm:items-end" onSubmit={submit}>
        <div className="flex-1">
          <Field label="Name" error={save.error instanceof ApiError ? save.error.fields?.name : undefined}>
            <Input required disabled={!canEdit} value={name} onChange={(event) => setName(event.target.value)} />
          </Field>
        </div>
        {canEdit ? (
          <Button type="submit" variant="primary" disabled={save.isPending || !name.trim() || name.trim() === workspace.name}>
            {save.isPending ? "Saving…" : "Rename"}
          </Button>
        ) : null}
      </form>
      <ErrorText error={save.error} />
    </LayerCard>
  );
}

function ProjectsCard() {
  const client = useQueryClient();
  const { projects } = useWorkspace();
  const canEdit = useCanEdit();
  const [editing, setEditing] = useState<Project | null>(null);
  const [creating, setCreating] = useState(false);
  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/projects/${id}`, { method: "DELETE" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["projects"] }),
  });
  return (
    <div id="projects" className="scroll-mt-20">
      <LayerCard
        title="Projects"
        description="Monitors, pages, incidents, apps, sites, and links each belong to one project. Notification channels and link domains are shared by the whole workspace."
        actions={canEdit ? <Button variant="primary" onClick={() => setCreating(true)}>New project</Button> : null}
        flush
      >
        <ul className="divide-y divide-[var(--ui-divider)]">
          {projects.map((project) => (
            <li key={project.id} className="flex items-center gap-3 px-4 py-2.5">
              <ProjectDot color={project.color} />
              <span className="min-w-0 flex-1 truncate text-[13px] text-[var(--ui-fg)]">{project.name}</span>
              {canEdit ? (
                <span className="flex shrink-0 gap-2">
                  <Button size="sm" onClick={() => setEditing(project)}>Edit</Button>
                  <Button
                    size="sm"
                    variant="danger"
                    title={projects.length === 1 ? "A workspace keeps at least one project" : undefined}
                    disabled={projects.length === 1 || (remove.isPending && remove.variables === project.id)}
                    onClick={() => remove.mutate(project.id)}
                  >
                    Delete
                  </Button>
                </span>
              ) : null}
            </li>
          ))}
        </ul>
        {remove.error ? <div className="px-4 pb-3"><ErrorText error={remove.error} /></div> : null}
      </LayerCard>
      {creating ? <ProjectDialog onClose={() => setCreating(false)} /> : null}
      {editing ? <ProjectDialog project={editing} onClose={() => setEditing(null)} /> : null}
    </div>
  );
}

function ProjectDialog({ project, onClose }: { project?: Project; onClose: () => void }) {
  const client = useQueryClient();
  const { projects } = useWorkspace();
  const [name, setName] = useState(project?.name ?? "");
  const [color, setColor] = useState(project?.color ?? projectColors[projects.length % projectColors.length]);
  const save = useMutation({
    mutationFn: () =>
      api<Project>(project ? `/api/projects/${project.id}` : "/api/projects", {
        method: project ? "PUT" : "POST",
        body: JSON.stringify({ name: name.trim(), color }),
      }),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["projects"] });
      onClose();
    },
  });
  const fields = save.error instanceof ApiError ? (save.error.fields ?? {}) : {};
  function submit(event: FormEvent) {
    event.preventDefault();
    save.mutate();
  }
  return (
    <Dialog open title={project ? "Edit project" : "New project"} onClose={onClose}>
      <form className="space-y-4" onSubmit={submit}>
        <Field label="Name" error={fields.name}>
          <Input required autoFocus value={name} onChange={(event) => setName(event.target.value)} placeholder="Production" />
        </Field>
        <Field label="Color" error={fields.color}>
          <div role="radiogroup" aria-label="Color" className="flex flex-wrap gap-2">
            {projectColors.map((swatch) => (
              <button
                key={swatch}
                type="button"
                role="radio"
                aria-checked={color === swatch}
                aria-label={swatch}
                onClick={() => setColor(swatch)}
                className="size-6 rounded-full border-2 transition-transform hover:scale-110"
                style={{ background: swatch, borderColor: color === swatch ? "var(--ui-fg)" : "transparent" }}
              />
            ))}
          </div>
        </Field>
        <ErrorText error={Object.keys(fields).length ? null : save.error} />
        <div className="flex justify-end gap-2">
          <Button type="button" onClick={onClose}>Cancel</Button>
          <Button type="submit" variant="primary" disabled={save.isPending || !name.trim()}>
            {save.isPending ? "Saving…" : project ? "Save project" : "Create project"}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

function MembersCard() {
  const client = useQueryClient();
  const navigate = useNavigate();
  const me = useMe();
  const canEdit = useCanEdit();
  const members = useQuery({ queryKey: ["members"], queryFn: () => api<Member[]>("/api/members") });
  const changeRole = useMutation({
    mutationFn: ({ id, role }: { id: string; role: InviteRole }) =>
      api(`/api/members/${id}`, { method: "PUT", body: JSON.stringify({ role }) }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["members"] }),
  });
  const remove = useMutation({
    mutationFn: (member: Member) => api(`/api/members/${member.id}`, { method: "DELETE" }),
    onSuccess: (_, member) => {
      if (member.user_id !== me.data?.id) return client.invalidateQueries({ queryKey: ["members"] });
      navigate("/", { replace: true });
      resetWorkspace();
    },
  });
  return (
    <LayerCard title="Members" description="Owners and admins can change anything. Members can view everything and change nothing." flush>
      {members.isLoading ? <p className="px-4 py-3 text-[13px] text-[var(--ui-fg-faint)]">Loading members…</p> : null}
      <ul className="divide-y divide-[var(--ui-divider)]">
        {(members.data ?? []).map((member) => {
          const self = member.user_id === me.data?.id;
          const owner = member.role === "owner";
          // The owner row is immutable; every other role is one an invite can grant.
          const grantable = member.role === "owner" ? null : member.role;
          return (
            <li key={member.id} className="flex flex-wrap items-center gap-3 px-4 py-2.5">
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13px] text-[var(--ui-fg)]">
                  {member.display_name || member.email}
                  {self ? <span className="text-[var(--ui-fg-faint)]"> (you)</span> : null}
                </div>
                <div className="truncate text-xs text-[var(--ui-fg-faint)]">{member.email}</div>
              </div>
              {canEdit && grantable && !self ? (
                <div className="w-32">
                  <Select
                    aria-label={`Role of ${member.email}`}
                    value={grantable}
                    options={roleOptions}
                    disabled={changeRole.isPending}
                    onChange={(role) => changeRole.mutate({ id: member.id, role })}
                  />
                </div>
              ) : (
                <StatusBadge tone={owner ? "info" : "neutral"}>{workspaceRoleLabel[member.role]}</StatusBadge>
              )}
              {!owner && (canEdit || self) ? (
                <Button size="sm" variant="danger" disabled={remove.isPending} onClick={() => remove.mutate(member)}>
                  {self ? "Leave" : "Remove"}
                </Button>
              ) : null}
            </li>
          );
        })}
      </ul>
      {members.error || changeRole.error || remove.error ? (
        <div className="px-4 pb-3"><ErrorText error={members.error || changeRole.error || remove.error} /></div>
      ) : null}
    </LayerCard>
  );
}

function InvitationsCard() {
  const client = useQueryClient();
  const invitations = useQuery({ queryKey: ["invitations"], queryFn: () => api<Invitation[]>("/api/invitations") });
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<InviteRole>("member");
  const [created, setCreated] = useState<CreatedInvitation | null>(null);
  const invite = useMutation({
    mutationFn: () => api<CreatedInvitation>("/api/invitations", { method: "POST", body: JSON.stringify({ email: email.trim(), role }) }),
    onSuccess: async (invitation) => {
      setEmail("");
      setCreated(invitation);
      await client.invalidateQueries({ queryKey: ["invitations"] });
    },
  });
  const revoke = useMutation({
    mutationFn: (id: string) => api(`/api/invitations/${id}`, { method: "DELETE" }),
    onSuccess: (_, id) => {
      if (created?.id === id) setCreated(null);
      return client.invalidateQueries({ queryKey: ["invitations"] });
    },
  });
  const fields = invite.error instanceof ApiError ? (invite.error.fields ?? {}) : {};
  function submit(event: FormEvent) {
    event.preventDefault();
    invite.mutate();
  }
  const pending = invitations.data ?? [];
  return (
    <LayerCard title="Invitations" description="An invite link works for 7 days. When mail is set up, Owlee also emails it.">
      <form className="flex flex-col gap-3 sm:flex-row sm:items-end" onSubmit={submit}>
        <div className="flex-1">
          <Field label="Email" error={fields.email}>
            <Input type="email" required value={email} onChange={(event) => setEmail(event.target.value)} placeholder="teammate@example.com" />
          </Field>
        </div>
        <div className="sm:w-32">
          <Field label="Role" error={fields.role}>
            <Select aria-label="Role" value={role} options={roleOptions} onChange={setRole} />
          </Field>
        </div>
        <Button type="submit" variant="primary" disabled={invite.isPending}>
          {invite.isPending ? "Inviting…" : "Invite"}
        </Button>
      </form>
      <ErrorText error={Object.keys(fields).length ? null : invite.error} />
      {created ? <InviteLink invitation={created} /> : null}
      {pending.length > 0 ? (
        <ul className="mt-4 divide-y divide-[var(--ui-well-border)] border-t border-[var(--ui-well-border)]">
          {pending.map((invitation) => (
            <li key={invitation.id} className="flex items-center gap-3 py-2.5">
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13px] text-[var(--ui-fg)]">{invitation.email}</div>
                <div className="truncate text-xs text-[var(--ui-fg-faint)]">
                  {workspaceRoleLabel[invitation.role]} · expires {formatTime(invitation.expires_at)}
                </div>
              </div>
              <Button size="sm" disabled={revoke.isPending && revoke.variables === invitation.id} onClick={() => revoke.mutate(invitation.id)}>
                Revoke
              </Button>
            </li>
          ))}
        </ul>
      ) : null}
      <ErrorText error={invitations.error || revoke.error} />
    </LayerCard>
  );
}

function InviteLink({ invitation }: { invitation: CreatedInvitation }) {
  const { copied, copy } = useCopy();
  const url = new URL(invitation.url, window.location.origin).toString();
  return (
    <div className="mt-4">
      <div className="field-label">
        <span>Invite link for {invitation.email}</span>
        <button type="button" className="ml-auto text-[11px] font-normal hover:text-[var(--ui-fg)]" onClick={() => void copy(url)}>
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      <Input readOnly value={url} className="font-mono text-[12px]" onFocus={(event) => event.currentTarget.select()} />
      <p className="mt-1 text-xs text-[var(--ui-fg-faint)]">Owlee shows this link once. Send it to them directly if mail is not set up.</p>
    </div>
  );
}

function DeleteCard() {
  const navigate = useNavigate();
  const { workspace } = useWorkspace();
  const [confirming, setConfirming] = useState(false);
  const remove = useMutation({
    mutationFn: () => api("/api/workspace", { method: "DELETE" }),
    onSuccess: () => {
      navigate("/", { replace: true });
      resetWorkspace();
    },
  });
  return (
    <LayerCard title="Delete workspace" description="Deletes every project, monitor, page, incident, app, site, and link in it, and removes every member.">
      <Button variant="danger" onClick={() => setConfirming(true)}>Delete {workspace.name}</Button>
      <Dialog
        open={confirming}
        title={`Delete ${workspace.name}?`}
        description="This cannot be undone. Status pages, short links, and DSNs from this workspace stop working at once."
        onClose={() => setConfirming(false)}
        footer={
          <>
            <Button type="button" onClick={() => setConfirming(false)}>Cancel</Button>
            <Button type="button" variant="danger" disabled={remove.isPending} onClick={() => remove.mutate()}>
              {remove.isPending ? "Deleting…" : "Delete workspace"}
            </Button>
          </>
        }
      >
        <ErrorText error={remove.error} />
      </Dialog>
    </LayerCard>
  );
}
