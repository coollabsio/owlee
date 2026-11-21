import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Button, ConfirmDialog, Dialog, ErrorText, Field, FilterChips, FilterValue, Input, LayerCard, PageHeader, RowActions, Select, SkeletonRows, SortHeader, Spinner, StatusBadge, Switch, useSort } from "@/components/ui";
import { ApiError, api, type Channel, type Monitor } from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { useListScope } from "@/lib/workspace";
import { formatTime, providerLabel } from "@/lib/utils";

const providers = ["email", "webhook", "slack", "discord"] as const;

export function NotificationsPage() {
  if (!useCanEdit()) {
    return (
      <div className="flex flex-col gap-6">
        <PageHeader title="Notifications" description="Only an admin can view notification channels." />
      </div>
    );
  }
  return <NotificationChannels />;
}

function NotificationChannels() {
  const client = useQueryClient();
  const query = useQuery({ queryKey: ["notifications"], queryFn: () => api<Channel[]>("/api/notifications") });
  const [open, setOpen] = useState(false);
  const [editing, setEditing] = useState<Channel | null>(null);
  const [deleting, setDeleting] = useState<Channel | null>(null);
  const [providerFilter, setProviderFilter] = useState<Channel["provider"] | null>(null);
  const test = useMutation({
    mutationFn: (id: string) => api<Channel>(`/api/notifications/${id}/test`, { method: "POST" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["notifications"] }),
  });
  const remove = useMutation({
    mutationFn: (id: string) => api(`/api/notifications/${id}`, { method: "DELETE" }),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["notifications"] });
      setDeleting(null);
    },
  });

  const all = query.data ?? [];
  const { rows: channels, sort, toggle } = useSort(
    all.filter((channel) => !providerFilter || channel.provider === providerFilter),
    {
      name: (channel) => channel.name,
      scope: (channel) => channel.monitor_ids.length,
      delivery: (channel) => (channel.deliveries[0] ? Date.parse(channel.deliveries[0].created_at) : null),
      status: (channel) => (channel.enabled ? "Enabled" : "Off"),
    },
  );

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Notifications" description="Email and webhooks ship today. Slack and Discord use the same incoming-webhook shape." />
      <ErrorText error={test.error} />
      <LayerCard
        title="Channels"
        description={query.data ? `${all.length} total` : undefined}
        actions={<Button variant="primary" onClick={() => setOpen(true)}>New channel</Button>}
        flush
      >
        {providerFilter ? (
          <div className="border-b border-[var(--ui-divider)] p-2">
            <FilterChips filters={[{ key: "provider", label: `Type: ${providerLabel[providerFilter]}`, onRemove: () => setProviderFilter(null) }]} onClear={() => setProviderFilter(null)} />
          </div>
        ) : null}
        {!query.isLoading && all.length === 0 ? (
          <CardText title="No channels" body="A channel with no monitors attached hears every transition. Link one to narrow it." />
        ) : !query.isLoading && channels.length === 0 ? (
          <CardText title="No matching channels" body="Remove the type filter to see every channel." />
        ) : (
          <div className="overflow-x-auto">
            <table className="cool-table">
              <thead>
                <tr>
                  <SortHeader column="name" sort={sort} onSort={toggle}>Channel</SortHeader>
                  <SortHeader column="scope" sort={sort} onSort={toggle} className="hidden md:table-cell">Scope</SortHeader>
                  <SortHeader column="delivery" sort={sort} onSort={toggle} className="hidden xl:table-cell">Last delivery</SortHeader>
                  <SortHeader column="status" sort={sort} onSort={toggle}>Status</SortHeader>
                  <th className="text-right">Actions</th>
                </tr>
              </thead>
              <tbody>
                {query.isLoading ? <SkeletonRows columns={[undefined, "hidden md:table-cell", "hidden xl:table-cell", undefined, "text-right"]} /> : null}
                {channels.map((channel) => {
                  const last = channel.deliveries[0];
                  return (
                    <tr key={channel.id}>
                      <td className="max-w-[20rem]">
                        <div className="font-medium text-[var(--ui-fg)]">{channel.name}</div>
                        <div className="truncate text-[0.6875rem] text-[var(--ui-fg-faint)]">
                          <FilterValue active={providerFilter === channel.provider} label={providerLabel[channel.provider]} onToggle={() => setProviderFilter(providerFilter === channel.provider ? null : channel.provider)}>
                            {providerLabel[channel.provider]}
                          </FilterValue>{" "}
                          · {channel.config.to || channel.config.url}
                        </div>
                      </td>
                      <td className="hidden whitespace-nowrap md:table-cell">
                        {channel.monitor_ids.length === 0 ? "All monitors" : `${channel.monitor_ids.length} monitors`}
                      </td>
                      <td className="hidden max-w-[16rem] xl:table-cell">
                        {last ? (
                          <>
                            <div className="truncate">{last.status} · {last.event}</div>
                            <div className="truncate text-[0.6875rem] text-[var(--ui-fg-faint)]">
                              {formatTime(last.created_at)}
                              {last.error ? ` · ${last.error}` : ""}
                            </div>
                          </>
                        ) : (
                          "-"
                        )}
                      </td>
                      <td>
                        <StatusBadge tone={channel.enabled ? "success" : "neutral"}>{channel.enabled ? "Enabled" : "Off"}</StatusBadge>
                      </td>
                      <td className="text-right">
                        <RowActions
                          label={channel.name}
                          actions={[
                            { label: "Edit", onSelect: () => setEditing(channel) },
                            { label: "Send test", onSelect: () => test.mutate(channel.id), disabled: test.isPending },
                            { label: "Delete", danger: true, onSelect: () => setDeleting(channel) },
                          ]}
                        />
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </LayerCard>
      <Dialog open={open} title="New channel" description="Alerts fire when a monitor changes state." onClose={() => setOpen(false)}>
        {open ? <ChannelForm onDone={() => setOpen(false)} /> : null}
      </Dialog>
      <Dialog open={editing !== null} title="Edit channel" description={editing?.name} onClose={() => setEditing(null)}>
        {editing ? <ChannelForm key={editing.id} channel={editing} onDone={() => setEditing(null)} /> : null}
      </Dialog>
      <ConfirmDialog
        open={deleting !== null}
        title="Delete notification channel?"
        description={`${deleting?.name ?? "This channel"} will stop sending alerts and its delivery history will be deleted.`}
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

function ChannelForm({ channel, onDone }: { channel?: Channel; onDone: () => void }) {
  const client = useQueryClient();
  const workspaceWide = useListScope({ allProjects: true });
  const monitors = useQuery({ queryKey: workspaceWide.key("monitors"), queryFn: () => api<Monitor[]>(workspaceWide.path("/api/monitors")) });
  const [name, setName] = useState(channel?.name ?? "");
  const [provider, setProvider] = useState<(typeof providers)[number]>(channel?.provider ?? "email");
  const [target, setTarget] = useState(channel ? channel.config.to || channel.config.url || "" : "");
  const [secret, setSecret] = useState(channel?.config.secret ?? "");
  // The form picks one monitor. Keep a wider set made through the API until the user picks another.
  const initialMonitors = channel?.monitor_ids ?? [];
  const [monitorId, setMonitorId] = useState(initialMonitors.length > 1 ? keepMonitors : (initialMonitors[0] ?? ""));
  const [enabled, setEnabled] = useState(channel?.enabled ?? true);
  const [fields, setFields] = useState<Record<string, string>>({});
  const save = useMutation({
    mutationFn: () =>
      api<Channel>(channel ? `/api/notifications/${channel.id}` : "/api/notifications", {
        method: channel ? "PUT" : "POST",
        body: JSON.stringify({
          name,
          provider,
          config: provider === "email" ? { to: target } : { url: target, secret },
          monitor_ids: monitorId === keepMonitors ? initialMonitors : monitorId ? [monitorId] : [],
          enabled,
        }),
      }),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["notifications"] });
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
      <Field label="Name" error={fields.name}><Input value={name} onChange={(event) => setName(event.target.value)} required /></Field>
      <Field label="Provider" error={fields.provider}>
        <Select value={provider} options={providers.map((item) => ({ value: item, label: providerLabel[item] }))} onChange={setProvider} aria-label="Provider" />
      </Field>
      <Field label={provider === "email" ? "Recipient" : "Webhook URL"} error={fields.config}>
        <Input value={target} onChange={(event) => setTarget(event.target.value)} placeholder={provider === "email" ? "ops@coollabs.io" : "https://"} required />
      </Field>
      {provider === "webhook" ? (
        <Field label="Signing secret" hint="optional">
          <Input value={secret} onChange={(event) => setSecret(event.target.value)} />
        </Field>
      ) : null}
      <Field label="Monitor">
        <Select
          value={monitorId}
          options={[
            { value: "", label: "All monitors" },
            ...(initialMonitors.length > 1 ? [{ value: keepMonitors, label: `Keep current ${initialMonitors.length} monitors` }] : []),
            ...(monitors.data ?? []).map((monitor) => ({ value: monitor.id, label: monitor.name })),
          ]}
          onChange={setMonitorId}
          aria-label="Monitor"
        />
      </Field>
      {channel ? (
        <div className="flex items-center justify-between">
          <span className="text-[0.8125rem] text-[var(--ui-fg)]">Enabled</span>
          <Switch checked={enabled} onCheckedChange={setEnabled} aria-label="Enabled" />
        </div>
      ) : null}
      <ErrorText error={save.error} />
      <div className="flex justify-end pt-1">
        <Button type="submit" variant="primary" disabled={save.isPending}>{save.isPending ? <><Spinner />Saving…</> : channel ? "Save" : "Add channel"}</Button>
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
