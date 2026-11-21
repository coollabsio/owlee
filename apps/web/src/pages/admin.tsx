import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Button, ConfirmDialog, ErrorText, Field, Input, LayerCard, Spinner, StatusBadge, Switch } from "@/components/ui";
import {
  ApiError,
  api,
  type AdminUser,
  type InstanceSettings,
  type Meta,
} from "@/lib/api";
import { useAccess } from "@/lib/me";
import { Item, SettingsView } from "@/components/settings-view";

function useInstanceSettings(enabled: boolean) {
  return useQuery({
    queryKey: ["instance-settings"],
    queryFn: () => api<InstanceSettings>("/api/admin/settings"),
    enabled,
  });
}

export function AdminGeneralPage() {
  const { root } = useAccess();
  const meta = useQuery({ queryKey: ["meta"], queryFn: () => api<Meta>("/api/meta") });
  const settings = useInstanceSettings(root);
  return (
    <SettingsView title="General" description="The server this dashboard talks to." requires="admin">
      <LayerCard title="Overview">
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
    </SettingsView>
  );
}

export function AdminEmailPage() {
  const settings = useInstanceSettings(useAccess().root);
  return (
    <SettingsView title="Email" description="The mail server Owlee sends from." requires="root">
      <ErrorText error={settings.error} />
      <EmailCard settings={settings.data} />
    </SettingsView>
  );
}

export function AdminQuotasPage() {
  const settings = useInstanceSettings(useAccess().root);
  return (
    <SettingsView title="Ingest quotas" description="Daily caps on the events each app or site can send." requires="root">
      <ErrorText error={settings.error} />
      <QuotaCard settings={settings.data} />
    </SettingsView>
  );
}

export function AdminUsersPage() {
  const { admin, root } = useAccess();
  const users = useQuery({
    queryKey: ["admin-users"],
    queryFn: () => api<AdminUser[]>("/api/admin/users"),
    enabled: admin,
  });
  return (
    <SettingsView title="Users" description="Every account on this Owlee install." requires="admin">
      <UsersCard users={users.data} error={users.error} canChange={root} />
    </SettingsView>
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
      title="Default caps"
      description="A safety valve so a runaway client cannot fill the disk. Each error app and site uses this number unless that app or site sets its own cap. Normal traffic should not reach it."
    >
      <form
        className="grid gap-3 sm:grid-cols-2"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field label="Errors per app per day" error={fields.error_daily}>
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
        <div className="text-[0.8125rem] font-medium text-[var(--ui-fg)]">Open registration</div>
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
  const [clearing, setClearing] = useState(false);
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
      setClearing(false);
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
      title="Mail server"
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
        <ErrorText error={save.error} />
        <div className="flex justify-end gap-2 pt-1">
          {source === "saved" || settings?.registration_open ? (
            <Button type="button" disabled={clear.isPending} onClick={() => setClearing(true)}>
              Clear
            </Button>
          ) : null}
          <Button type="submit" variant="primary" disabled={save.isPending}>
            {save.isPending ? <><Spinner />Saving…</> : "Save mail server"}
          </Button>
        </div>
      </form>
      <ConfirmDialog
        open={clearing}
        title="Delete mail server settings?"
        description={`The saved server${settings?.smtp?.host ? ` ${settings.smtp.host}` : ""} and its password are deleted. Owlee cannot send email until a mail server is configured again.`}
        pending={clear.isPending}
        error={clear.error}
        onConfirm={() => clear.mutate()}
        onClose={() => {
          setClearing(false);
          clear.reset();
        }}
      />
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
  const [demoting, setDemoting] = useState<AdminUser | null>(null);
  const mutation = useMutation({
    mutationFn: ({ id, admin }: { id: string; admin: boolean }) =>
      api<AdminUser>(`/api/admin/users/${id}/admin`, {
        method: "PUT",
        body: JSON.stringify({ admin }),
      }),
    onSuccess: () => {
      setDemoting(null);
      void client.invalidateQueries({ queryKey: ["admin-users"] });
    },
  });
  return (
    <LayerCard
      title="Accounts"
      description="The root user is created at setup and cannot be changed. Only the root user can make or remove an installation admin."
    >
      <ErrorText error={error || (demoting ? null : mutation.error)} />
      <ul className="divide-y divide-[var(--ui-well-border)]">
        {(users ?? []).map((user) => (
          <li key={user.id} className="flex items-center gap-3 py-2.5">
            <div className="min-w-0 flex-1">
              <div className="truncate text-[0.8125rem] text-[var(--ui-fg)]">{user.display_name}</div>
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
                onClick={() => (user.installation_admin ? setDemoting(user) : mutation.mutate({ id: user.id, admin: true }))}
              >
                {user.installation_admin ? "Remove admin" : "Make admin"}
              </Button>
            ) : null}
          </li>
        ))}
      </ul>
      <ConfirmDialog
        open={demoting !== null}
        title="Remove admin?"
        description={`${demoting?.display_name ?? "This user"} loses installation admin access. The root user can make them an admin again.`}
        confirmLabel="Remove admin"
        pending={mutation.isPending}
        error={mutation.error}
        onConfirm={() => demoting && mutation.mutate({ id: demoting.id, admin: false })}
        onClose={() => {
          setDemoting(null);
          mutation.reset();
        }}
      />
    </LayerCard>
  );
}

