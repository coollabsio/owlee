import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, type ReactNode, useState } from "react";
import { Button, Dialog, ErrorText, Field, Input, LayerCard, PageHeader, StatusBadge, Switch } from "@/components/ui";
import {
  ApiError,
  api,
  type AdminUser,
  type AuthSession,
  changePassword,
  deleteUserErrors,
  exportUserErrors,
  type InstanceSettings,
  listSessions,
  type Meta,
  revokeSession,
  type User,
} from "@/lib/api";
import { useMe } from "@/lib/me";
import { formatTime, timeAgo } from "@/lib/utils";

export function SettingsPage() {
  const me = useMe();
  const meta = useQuery({ queryKey: ["meta"], queryFn: () => api<Meta>("/api/meta") });
  const root = me.data?.root === true;
  const installationAdmin = me.data?.installation_admin === true;
  const settings = useQuery({
    queryKey: ["instance-settings"],
    queryFn: () => api<InstanceSettings>("/api/admin/settings"),
    enabled: root,
  });
  const users = useQuery({
    queryKey: ["admin-users"],
    queryFn: () => api<AdminUser[]>("/api/admin/users"),
    enabled: installationAdmin,
  });

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-6">
      <PageHeader title="Settings" description="This Owlee install and the signed-in operator." />
      <LayerCard title="Account" description="The operator signed in to this browser.">
        <dl className="grid gap-4 sm:grid-cols-3">
          <Item label="Name">{me.data?.display_name ?? "…"}</Item>
          <Item label="Email">{me.data?.email ?? "…"}</Item>
          <Item label="Role">{roleLabel(me.data)}</Item>
        </dl>
      </LayerCard>
      <PasswordCard />
      <SessionsCard />
      <LayerCard title="Instance" description="The server this dashboard talks to.">
        <dl className="grid gap-4 sm:grid-cols-2">
          <Item label="Version">{meta.data?.version ?? "…"}</Item>
          <Item label="Database"><span className="capitalize">{meta.data?.database ?? "…"}</span></Item>
          <Item label="Setup">{meta.data ? (meta.data.setup_complete ? "Initialized" : "Waiting for setup") : "…"}</Item>
          <Item label="Registration">{meta.data ? (meta.data.signup_open ? "Open" : "Closed") : "…"}</Item>
          {root ? (
            <>
              <Item label="Root user">{settings.data?.root_user?.display_name ?? "…"}</Item>
              <Item label="Root email">{settings.data?.root_user?.email ?? "…"}</Item>
            </>
          ) : null}
        </dl>
        {root ? <RegistrationRow settings={settings.data} error={settings.error} /> : null}
        {settings.error && root ? <ErrorText error={settings.error} /> : null}
      </LayerCard>
      {root ? <EmailCard settings={settings.data} /> : null}
      {root ? <QuotaCard settings={settings.data} /> : null}
      {installationAdmin ? <UsersCard users={users.data} error={users.error} canChange={root} /> : null}
      {installationAdmin ? <PrivacyCard /> : null}
    </div>
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
            {save.isPending ? "Saving…" : "Change password"}
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
  const revoke = useMutation({
    mutationFn: revokeSession,
    onSuccess: () => client.invalidateQueries({ queryKey: ["auth-sessions"] }),
  });
  return (
    <LayerCard title="Sessions" description="Browsers signed in to your account.">
      <ErrorText error={sessions.error || revoke.error} />
      <ul className="divide-y divide-[var(--ui-well-border)]">
        {(sessions.data ?? []).map((session: AuthSession) => (
          <li key={session.id} className="flex items-center gap-3 py-2.5">
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13px] text-[var(--ui-fg)]">Signed in {formatTime(session.created_at)}</div>
              <div className="truncate text-xs text-[var(--ui-fg-faint)]">Last active {timeAgo(session.last_activity_at)}</div>
            </div>
            {session.current ? (
              <StatusBadge tone="info">This device</StatusBadge>
            ) : (
              <Button
                type="button"
                size="sm"
                disabled={revoke.isPending && revoke.variables === session.id}
                onClick={() => revoke.mutate(session.id)}
              >
                Sign out
              </Button>
            )}
          </li>
        ))}
      </ul>
    </LayerCard>
  );
}

function QuotaCard({ settings }: { settings?: InstanceSettings }) {
  const key = settings ? `${settings.error_daily_quota}:${settings.analytics_daily_quota}` : "loading";
  return <QuotaForm key={key} settings={settings} />;
}

function QuotaForm({ settings }: { settings?: InstanceSettings }) {
  const client = useQueryClient();
  const [errorQuota, setErrorQuota] = useState(settings ? String(settings.error_daily_quota) : "10000");
  const [analyticsQuota, setAnalyticsQuota] = useState(settings ? String(settings.analytics_daily_quota) : "100000");
  const [fields, setFields] = useState<Record<string, string>>({});
  const save = useMutation({
    mutationFn: () =>
      api<InstanceSettings>("/api/admin/settings/quotas", {
        method: "PUT",
        body: JSON.stringify({
          error_daily: Number(errorQuota),
          analytics_daily: Number(analyticsQuota),
        }),
      }),
    onSuccess: (data) => {
      client.setQueryData(["instance-settings"], data);
      setErrorQuota(String(data.error_daily_quota));
      setAnalyticsQuota(String(data.analytics_daily_quota));
      setFields({});
    },
    onError: (error) => setFields(error instanceof ApiError ? (error.fields ?? {}) : {}),
  });
  return (
    <LayerCard
      title="Ingest quotas"
      description="A safety valve so a runaway client cannot fill the disk. Each project and site uses this number unless that project or site sets its own cap. Normal traffic should not reach it."
    >
      <form
        className="grid gap-3 sm:grid-cols-2"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field label="Errors per project per day" error={fields.error_daily}>
          <Input
            inputMode="numeric"
            value={errorQuota}
            onChange={(event) => setErrorQuota(event.target.value)}
            required
          />
        </Field>
        <Field label="Analytics events per site per day" error={fields.analytics_daily}>
          <Input
            inputMode="numeric"
            value={analyticsQuota}
            onChange={(event) => setAnalyticsQuota(event.target.value)}
            required
          />
        </Field>
        <div className="sm:col-span-2">
          <Button variant="primary" type="submit" disabled={save.isPending || !settings}>
            Save quotas
          </Button>
          <ErrorText error={save.error} />
        </div>
      </form>
    </LayerCard>
  );
}

function roleLabel(user: User | undefined) {
  if (!user) return "…";
  if (user.root) return "Root";
  return user.role.charAt(0).toUpperCase() + user.role.slice(1);
}

function RegistrationRow({ settings, error }: { settings?: InstanceSettings; error: unknown }) {
  const client = useQueryClient();
  const mutation = useMutation({
    mutationFn: (open: boolean) =>
      api<InstanceSettings>("/api/admin/settings/registration", {
        method: "PUT",
        body: JSON.stringify({ open }),
      }),
    onSuccess: (data) => {
      client.setQueryData(["instance-settings"], data);
      void client.invalidateQueries({ queryKey: ["meta"] });
    },
  });
  const open = settings?.registration_open ?? false;
  const ready = settings?.smtp_ready ?? false;
  const disabled = mutation.isPending || !settings || (!ready && !open);
  return (
    <div className="mt-4 flex items-center gap-4 border-t border-[var(--ui-well-border)] pt-4">
      <div className="min-w-0 flex-1">
        <div className="text-[13px] font-medium text-[var(--ui-fg)]">Open registration</div>
        <p className="mt-0.5 text-xs text-[var(--ui-fg-faint)]">
          {error
            ? "Instance settings could not be loaded."
            : ready
              ? "On: anyone can create an account. Off: public sign-up stays closed."
              : "Save a mail server first. Registration stays closed until this instance can send mail."}
        </p>
        <ErrorText error={mutation.error} />
      </div>
      <Switch
        aria-label="Open registration"
        checked={open}
        disabled={disabled}
        onCheckedChange={(checked) => mutation.mutate(checked)}
      />
    </div>
  );
}

function EmailCard({ settings }: { settings?: InstanceSettings }) {
  const smtpKey = settings?.smtp ? `${settings.smtp.source}:${settings.smtp.host}:${settings.smtp.port}:${settings.smtp.from}` : "none";
  return <EmailForm key={smtpKey} settings={settings} />;
}

function EmailForm({ settings }: { settings?: InstanceSettings }) {
  const client = useQueryClient();
  const [host, setHost] = useState(settings?.smtp?.host ?? "");
  const [port, setPort] = useState(settings?.smtp ? String(settings.smtp.port) : "587");
  const [username, setUsername] = useState(settings?.smtp?.username ?? "");
  const [password, setPassword] = useState("");
  const [from, setFrom] = useState(settings?.smtp?.from ?? "");
  const [fields, setFields] = useState<Record<string, string>>({});
  const save = useMutation({
    mutationFn: () =>
      api<InstanceSettings>("/api/admin/settings/smtp", {
        method: "PUT",
        body: JSON.stringify({
          host,
          port: Number(port),
          username,
          password: password.trim() ? password : null,
          from,
        }),
      }),
    onSuccess: (data) => {
      client.setQueryData(["instance-settings"], data);
      setPassword("");
      setFields({});
      void client.invalidateQueries({ queryKey: ["meta"] });
    },
    onError: (error) => setFields(error instanceof ApiError ? (error.fields ?? {}) : {}),
  });
  const clear = useMutation({
    mutationFn: () => api<InstanceSettings>("/api/admin/settings/smtp", { method: "DELETE" }),
    onSuccess: (data) => {
      client.setQueryData(["instance-settings"], data);
      setPassword("");
      void client.invalidateQueries({ queryKey: ["meta"] });
    },
  });
  const source = settings?.smtp?.source;
  function submit(event: FormEvent) {
    event.preventDefault();
    save.mutate();
  }
  return (
    <LayerCard
      title="Email"
      description="Registration can open only after a mail server is available."
    >
      <form className="space-y-3" onSubmit={submit}>
        {source === "environment" ? (
          <p className="text-xs text-[var(--ui-fg-faint)]">
            A mail server is set from the environment. Saving stores one for this instance and uses it instead. Clearing the saved server closes registration.
          </p>
        ) : (
          <p className="text-xs text-[var(--ui-fg-faint)]">
            Saved here, or with OWLEE_SMTP_HOST and OWLEE_SMTP_FROM. Clearing the saved server closes registration.
          </p>
        )}
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Host" error={fields.host}>
            <Input value={host} onChange={(event) => setHost(event.target.value)} placeholder="smtp.example.com" required />
          </Field>
          <Field label="Port" error={fields.port}>
            <Input value={port} onChange={(event) => setPort(event.target.value)} inputMode="numeric" required />
          </Field>
          <Field label="Username" hint="optional" error={fields.username}>
            <Input value={username} onChange={(event) => setUsername(event.target.value)} autoComplete="off" />
          </Field>
          <Field label="Password" hint={settings?.smtp?.has_password ? "leave blank to keep" : "optional"} error={fields.password}>
            <Input type="password" value={password} onChange={(event) => setPassword(event.target.value)} autoComplete="new-password" />
          </Field>
        </div>
        <Field label="From" error={fields.from}>
          <Input type="email" value={from} onChange={(event) => setFrom(event.target.value)} placeholder="status@example.com" required />
        </Field>
        <ErrorText error={save.error || clear.error} />
        <div className="flex justify-end gap-2 pt-1">
          {source === "saved" || settings?.registration_open ? (
            <Button type="button" disabled={clear.isPending} onClick={() => clear.mutate()}>
              {clear.isPending ? "Clearing…" : "Clear"}
            </Button>
          ) : null}
          <Button type="submit" variant="primary" disabled={save.isPending}>
            {save.isPending ? "Saving…" : "Save mail server"}
          </Button>
        </div>
      </form>
    </LayerCard>
  );
}

function UsersCard({
  users,
  error,
  canChange,
}: {
  users?: AdminUser[];
  error: unknown;
  canChange: boolean;
}) {
  const client = useQueryClient();
  const mutation = useMutation({
    mutationFn: ({ id, admin }: { id: string; admin: boolean }) =>
      api<AdminUser>(`/api/admin/users/${id}/admin`, {
        method: "PUT",
        body: JSON.stringify({ admin }),
      }),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: ["admin-users"] });
    },
  });
  return (
    <LayerCard
      title="Users"
      description="The root user is created at setup and cannot be changed. Only the root user can make or remove an installation admin."
    >
      <ErrorText error={error || mutation.error} />
      <ul className="divide-y divide-[var(--ui-well-border)]">
        {(users ?? []).map((user) => (
          <li key={user.id} className="flex items-center gap-3 py-2.5">
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13px] text-[var(--ui-fg)]">{user.display_name}</div>
              <div className="truncate text-xs text-[var(--ui-fg-faint)]">{user.email}</div>
            </div>
            <span className="flex shrink-0 items-center gap-1.5">
              {user.root ? <StatusBadge tone="info">Root</StatusBadge> : null}
              {user.installation_admin && !user.root ? <StatusBadge>Admin</StatusBadge> : null}
            </span>
            {canChange && !user.root ? (
              <Button
                type="button"
                size="sm"
                disabled={mutation.isPending}
                onClick={() => mutation.mutate({ id: user.id, admin: !user.installation_admin })}
              >
                {user.installation_admin ? "Remove admin" : "Make admin"}
              </Button>
            ) : null}
          </li>
        ))}
      </ul>
    </LayerCard>
  );
}

function PrivacyCard() {
  const [user, setUser] = useState("");
  const [confirming, setConfirming] = useState(false);
  const exported = useMutation({
    mutationFn: () => exportUserErrors(user.trim()),
    onSuccess: (data) => {
      const url = URL.createObjectURL(new Blob([JSON.stringify(data, null, 2)], { type: "application/json" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = "owlee-error-events.json";
      link.click();
      URL.revokeObjectURL(url);
    },
  });
  const removed = useMutation({
    mutationFn: () => deleteUserErrors(user.trim()),
    onSuccess: () => setConfirming(false),
  });
  function submit(event: FormEvent) {
    event.preventDefault();
    exported.mutate();
  }
  return (
    <LayerCard
      title="Privacy requests"
      description="Export or erase the error events stored for one person. Analytics data cannot be tied to a person, so it needs no request."
    >
      <form className="flex flex-col gap-3" onSubmit={submit}>
        <Field label="User" hint="Id, email, username, or IP address">
          <Input required value={user} onChange={(event) => setUser(event.target.value)} />
        </Field>
        <div className="flex gap-2">
          <Button type="submit" disabled={!user.trim() || exported.isPending}>
            {exported.isPending ? "Exporting…" : "Export JSON"}
          </Button>
          <Button type="button" variant="danger" disabled={!user.trim()} onClick={() => setConfirming(true)}>
            Delete events
          </Button>
        </div>
        {exported.isSuccess ? (
          <p className="text-xs text-[var(--ui-fg-faint)]">Exported {exported.data.events.length} events.</p>
        ) : null}
        {removed.isSuccess ? (
          <p className="text-xs text-[var(--ui-fg-faint)]">Deleted {removed.data.deleted} events.</p>
        ) : null}
        <ErrorText error={exported.error} />
      </form>
      <Dialog
        open={confirming}
        title="Delete error events?"
        description={`Every stored error event for ${user.trim()} is deleted. This cannot be undone.`}
        onClose={() => setConfirming(false)}
        footer={
          <>
            <Button type="button" onClick={() => setConfirming(false)}>
              Cancel
            </Button>
            <Button type="button" variant="danger" disabled={removed.isPending} onClick={() => removed.mutate()}>
              {removed.isPending ? "Deleting…" : "Delete"}
            </Button>
          </>
        }
      >
        <ErrorText error={removed.error} />
      </Dialog>
    </LayerCard>
  );
}

function Item({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="text-[11px] font-medium text-[var(--ui-fg-faint)]">{label}</dt>
      <dd className="mt-1 truncate text-[13px] text-[var(--ui-fg)]">{children}</dd>
    </div>
  );
}
