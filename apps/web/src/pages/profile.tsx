import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Item, SettingsView } from "@/components/settings-view";
import { Button, ConfirmDialog, ErrorText, Field, Input, LayerCard, Spinner, StatusBadge } from "@/components/ui";
import {
  ApiError,
  type AuthSession,
  changePassword,
  listSessions,
  revokeSession,
  type User,
} from "@/lib/api";
import { useMe } from "@/lib/me";
import { useWorkspace, workspaceRoleLabel } from "@/lib/workspace";
import { formatTime, timeAgo } from "@/lib/utils";

export function ProfilePage() {
  const me = useMe();
  const { workspace } = useWorkspace();
  return (
    <SettingsView title="Profile" description="The operator signed in to this browser.">
      <LayerCard title="Account">
        <dl className="grid gap-4 sm:grid-cols-2">
          <Item label="Name">{me.data?.display_name ?? "…"}</Item>
          <Item label="Email">{me.data?.email ?? "…"}</Item>
          <Item label={`Role in ${workspace.name}`}>{workspaceRoleLabel[workspace.role]}</Item>
          <Item label="Instance access">{instanceAccess(me.data)}</Item>
        </dl>
      </LayerCard>
    </SettingsView>
  );
}

export function SecurityPage() {
  return (
    <SettingsView title="Security" description="How you sign in to Owlee.">
      <PasswordCard />
    </SettingsView>
  );
}

export function SessionsPage() {
  return (
    <SettingsView title="Sessions" description="Browsers signed in to your account.">
      <SessionsCard />
    </SettingsView>
  );
}


function PasswordCard() {
  const client = useQueryClient();
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [fields, setFields] = useState<Record<string, string>>({});
  const save = useMutation({
    mutationFn: () => changePassword({ current_password: current, new_password: next }),
    onSuccess: () => {
      setCurrent("");
      setNext("");
      setFields({});
      void client.invalidateQueries({ queryKey: ["auth-sessions"] });
    },
    onError: (error) => setFields(error instanceof ApiError ? (error.fields ?? {}) : {}),
  });
  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    save.mutate();
  }
  return (
    <LayerCard title="Password" description="Changing it signs out every other device.">
      <form className="grid gap-3 sm:grid-cols-2" onSubmit={submit}>
        <Field label="Current password" error={fields.current_password}>
          <Input type="password" required autoComplete="current-password" value={current} onChange={(event) => setCurrent(event.target.value)} />
        </Field>
        <Field label="New password" hint="12+ characters" error={fields.new_password}>
          <Input type="password" required autoComplete="new-password" value={next} onChange={(event) => setNext(event.target.value)} />
        </Field>
        <div className="sm:col-span-2">
          <Button variant="primary" type="submit" disabled={save.isPending}>
            {save.isPending ? <><Spinner />Saving…</> : "Change password"}
          </Button>
          {save.isSuccess ? <p className="mt-2 text-xs text-[var(--ui-fg-faint)]">Password changed. Other devices are signed out.</p> : null}
          {Object.keys(fields).length === 0 ? <ErrorText error={save.error} /> : null}
        </div>
      </form>
    </LayerCard>
  );
}

function SessionsCard() {
  const client = useQueryClient();
  const sessions = useQuery({ queryKey: ["auth-sessions"], queryFn: listSessions });
  const [revoking, setRevoking] = useState<AuthSession | null>(null);
  const revoke = useMutation({
    mutationFn: revokeSession,
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["auth-sessions"] });
      setRevoking(null);
    },
  });
  return (
    <LayerCard title="Signed-in browsers" description="Sign out a browser you no longer use.">
      <ErrorText error={sessions.error} />
      <ul className="divide-y divide-[var(--ui-well-border)]">
        {(sessions.data ?? []).map((session: AuthSession) => (
          <li key={session.id} className="flex items-center gap-3 py-2.5">
            <div className="min-w-0 flex-1">
              <div className="truncate text-[0.8125rem] text-[var(--ui-fg)]">Signed in {formatTime(session.created_at)}</div>
              <div className="truncate text-xs text-[var(--ui-fg-faint)]">Last active {timeAgo(session.last_activity_at)}</div>
            </div>
            {session.current ? (
              <StatusBadge tone="info">This device</StatusBadge>
            ) : (
              <Button type="button" size="sm" onClick={() => setRevoking(session)}>
                Sign out
              </Button>
            )}
          </li>
        ))}
      </ul>
      <ConfirmDialog
        open={revoking !== null}
        title="Sign out session?"
        description={`The browser signed in ${revoking ? formatTime(revoking.created_at) : ""} is signed out and has to sign in again.`}
        confirmLabel="Sign out"
        pending={revoke.isPending}
        error={revoke.error}
        onConfirm={() => revoking && revoke.mutate(revoking.id)}
        onClose={() => {
          setRevoking(null);
          revoke.reset();
        }}
      />
    </LayerCard>
  );
}

function instanceAccess(user: User | undefined) {
  if (!user) return "…";
  if (user.root) return "Root";
  return user.installation_admin ? "Installation admin" : "None";
}

