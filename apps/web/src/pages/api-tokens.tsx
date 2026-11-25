import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Key } from "reicon-react";
import { ALL_RESOURCES, AccessPicker, ResourcePicker, type ResourceSelection, isResourceSelectionEmpty, toTokenResources } from "@/components/scope-access";
import { SettingsView } from "@/components/settings-view";
import { CodeBlock } from "@/components/setup-guide";
import { Button, ConfirmDialog, CopyInput, Dialog, ErrorText, Field, Input, LayerCard, RowActions, Select, Spinner, StatusBadge } from "@/components/ui";
import {
  API_PRODUCTS,
  type ApiScope,
  type ApiToken,
  ApiError,
  api,
  createApiToken,
  type ErrorApp,
  listApiTokens,
  revokeApiToken,
  rotateApiToken,
  setApiTokenPaused,
  type TokenExpiry,
  type TokenResources,
} from "@/lib/api";
import { useCanEdit } from "@/lib/me";
import { timeAgo } from "@/lib/utils";
import { useListScope, useWorkspace } from "@/lib/workspace";

const EXPIRY_OPTIONS: { value: string; label: string; days: TokenExpiry }[] = [
  { value: "7", label: "7 days", days: 7 },
  { value: "30", label: "30 days", days: 30 },
  { value: "90", label: "90 days", days: 90 },
  { value: "365", label: "1 year", days: 365 },
  { value: "never", label: "Never", days: null },
];

function formatDate(iso: string) {
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium" }).format(new Date(iso));
}

/** The plaintext of a token just created or rotated. */
type Issued = { name: string; token: string };

const isExpired = (token: ApiToken) => token.expires_at !== null && Date.parse(token.expires_at) <= Date.now();

/** Every project and error app in the workspace, whatever project the switcher shows. */
function useWorkspaceResources() {
  const { projects } = useWorkspace();
  const scope = useListScope({ allProjects: true });
  const apps = useQuery({ queryKey: scope.key("apps"), queryFn: () => api<ErrorApp[]>(scope.path("/api/error-apps")) });
  return { projects, apps: apps.data ?? [], loading: apps.isPending };
}

export function ApiTokensPage() {
  const origin = window.location.origin;
  const [creating, setCreating] = useState(false);
  const [issued, setIssued] = useState<Issued | null>(null);
  return (
    <SettingsView title="API & MCP" description="Personal tokens let scripts and AI tools such as Claude Code and Cursor read and triage errors in this workspace.">
      <LayerCard title="MCP endpoint" description="Add this URL to your AI tool as a remote MCP server, with a token in the Authorization header.">
        <div className="flex flex-col gap-3">
          <CopyInput value={`${origin}/api/mcp`} label="Copy MCP endpoint" />
          <div className="flex flex-col gap-1">
            <span className="text-[0.75rem] text-[var(--ui-fg-dim)]">OpenAPI document of the token API, readable without a token</span>
            <CopyInput value={`${origin}/api/v1/openapi.json`} label="Copy OpenAPI URL" />
          </div>
          <p className="text-[0.75rem] text-[var(--ui-fg-faint)]">
            A token acts as you, with your current role in this workspace. Its access can narrow that, never widen it. If your role changes or you leave the workspace, the token follows.
          </p>
        </div>
      </LayerCard>
      <TokenList onCreate={() => setCreating(true)} onIssued={setIssued} />
      <CreateTokenDialog
        open={creating}
        onClose={() => setCreating(false)}
        onIssued={(token) => {
          setCreating(false);
          setIssued(token);
        }}
      />
      <Dialog
        open={issued !== null}
        wide
        title={issued ? `${issued.name} is ready` : "Token"}
        description="Copy this token now. Owlee stores only its hash and cannot show it again."
        onClose={() => setIssued(null)}
        footer={<Button type="button" variant="highlighted" onClick={() => setIssued(null)}>Done</Button>}
      >
        {issued ? <IssuedToken token={issued.token} origin={origin} /> : null}
      </Dialog>
    </SettingsView>
  );
}

function CreateTokenDialog({ open, onClose, onIssued }: { open: boolean; onClose: () => void; onIssued: (issued: Issued) => void }) {
  const client = useQueryClient();
  const canGrantAll = useCanEdit();
  const resources = useWorkspaceResources();
  const [name, setName] = useState("");
  const [expiry, setExpiry] = useState("90");
  const [scopes, setScopes] = useState<ApiScope[]>(["errors:read"]);
  const [limit, setLimit] = useState<ResourceSelection>(ALL_RESOURCES);
  const [fields, setFields] = useState<Record<string, string>>({});
  const save = useMutation({
    mutationFn: () =>
      createApiToken({
        name: name.trim(),
        scopes,
        expires_in_days: EXPIRY_OPTIONS.find((option) => option.value === expiry)!.days,
        resources: scopes.length === 0 ? null : toTokenResources(limit),
      }),
    onSuccess: async (token) => {
      onIssued({ name: token.name, token: token.token });
      setName("");
      setScopes(["errors:read"]);
      setLimit(ALL_RESOURCES);
      await client.invalidateQueries({ queryKey: ["api-tokens"] });
    },
    onError: (error) => setFields(error instanceof ApiError ? error.fields ?? {} : {}),
  });

  const noResources = isResourceSelectionEmpty(limit);
  const ready = name.trim() !== "" && scopes.length > 0 && !noResources;

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!ready) return;
    setFields({});
    save.mutate();
  }

  const fieldError = fields.name ?? fields.scopes ?? fields.expires_in_days ?? fields.resources;
  return (
    <Dialog
      open={open}
      wide
      title="Create API token"
      description="Pick what the token may do. It can never do more than your role allows. Owlee shows the token once."
      onClose={onClose}
      footer={
        <>
          <Button type="button" onClick={onClose}>Cancel</Button>
          <Button type="submit" form="create-api-token" variant="highlighted" disabled={save.isPending || !ready}>{save.isPending ? <><Spinner />Creating…</> : "Create token"}</Button>
        </>
      }
    >
      <form id="create-api-token" className="flex flex-col gap-4" onSubmit={submit}>
        <div className="grid gap-4 sm:grid-cols-2">
          <Field label="Name" error={fields.name}>
            <Input value={name} onChange={(event) => setName(event.target.value)} placeholder="Claude Code on my laptop" maxLength={100} required />
          </Field>
          <Field label="Expires" error={fields.expires_in_days}>
            <Select value={expiry} options={EXPIRY_OPTIONS} onChange={setExpiry} aria-label="Expires" />
          </Field>
        </div>
        <div>
          <div className="field-label">
            <span>Access</span>
            {canGrantAll ? null : <span className="ml-auto text-[0.6875rem] font-normal">Only workspace owners and admins can grant write or delete.</span>}
          </div>
          <AccessPicker
            readOnly={!canGrantAll}
            scopes={scopes}
            onChange={setScopes}
            rows={API_PRODUCTS.map((row) => ({
              ...row,
              details: row.product === "errors" ? <ResourcePicker projects={resources.projects} apps={resources.apps} loading={resources.loading} value={limit} onChange={setLimit} /> : undefined,
            }))}
          />
          {fields.scopes ? <p className="field-error">{fields.scopes}</p> : scopes.length === 0 ? <p className="field-error">Choose access for at least one product.</p> : null}
          {fields.resources ? <p className="field-error">{fields.resources}</p> : noResources && scopes.length > 0 ? <p className="text-[0.75rem] text-[var(--ui-fg-faint)]">Choose at least one project or app.</p> : null}
        </div>
        <ErrorText error={fieldError ? null : save.error} />
      </form>
    </Dialog>
  );
}

function IssuedToken({ token, origin }: { token: string; origin: string }) {
  const mcp = `${origin}/api/mcp`;
  const cursor = JSON.stringify({ mcpServers: { owlee: { url: mcp, headers: { Authorization: `Bearer ${token}` } } } }, null, 2);
  return (
    <div role="status" className="flex flex-col gap-4">
      <Field label="Token">
        <CopyInput value={token} label="Copy token" />
      </Field>
      <CodeBlock file="Claude Code" code={`claude mcp add --transport http owlee ${mcp} --header "Authorization: Bearer ${token}"`} />
      <CodeBlock file="Cursor: ~/.cursor/mcp.json" code={cursor} />
      <CodeBlock file="curl" code={`curl -H "Authorization: Bearer ${token}" ${origin}/api/error-issues`} />
    </div>
  );
}

function resourceSummary(resources: TokenResources, projectNames: Map<string, string>, appNames: Map<string, string>) {
  if (resources === null) return "All resources";
  const names = [
    ...resources.projects.map((id) => projectNames.get(id) ?? "Deleted project"),
    ...resources.error_apps.map((id) => appNames.get(id) ?? "Deleted app"),
  ];
  return names.join(", ");
}

const CONFIRM = {
  rotate: {
    label: "Rotate",
    description: "The current secret stops working at once. Integrations that use it must get the new token.",
  },
  revoke: {
    label: "Revoke",
    description: "Integrations that use this token lose access at once. You cannot undo this.",
  },
} as const;

function TokenList({ onCreate, onIssued }: { onCreate: () => void; onIssued: (issued: Issued) => void }) {
  const client = useQueryClient();
  const query = useQuery({ queryKey: ["api-tokens"], queryFn: listApiTokens });
  const resources = useWorkspaceResources();
  const projectNames = new Map(resources.projects.map((project) => [project.id, project.name]));
  const appNames = new Map(resources.apps.map((app) => [app.id, app.name]));
  const [confirming, setConfirming] = useState<{ kind: keyof typeof CONFIRM; token: ApiToken } | null>(null);
  const [typed, setTyped] = useState("");
  const refresh = () => client.invalidateQueries({ queryKey: ["api-tokens"] });
  const close = () => {
    setConfirming(null);
    setTyped("");
    confirm.reset();
  };
  const confirm = useMutation({
    mutationFn: async ({ kind, token }: { kind: keyof typeof CONFIRM; token: ApiToken }) => {
      if (kind === "revoke") return revokeApiToken(token.id);
      const rotated = await rotateApiToken(token.id);
      onIssued({ name: rotated.name, token: rotated.token });
    },
    onSuccess: async () => {
      await refresh();
      close();
    },
  });
  const pause = useMutation({
    mutationFn: ({ token, paused }: { token: ApiToken; paused: boolean }) => setApiTokenPaused(token.id, paused),
    onSuccess: refresh,
  });
  const tokens = query.data ?? [];
  const ask = (kind: keyof typeof CONFIRM, token: ApiToken) => {
    pause.reset();
    setConfirming({ kind, token });
  };

  return (
    <LayerCard
      title="Active API tokens"
      description="Tokens you created in this workspace."
      actions={<Button type="button" variant="highlighted" onClick={onCreate}>Create token</Button>}
      flush
    >
      {query.isLoading ? <p className="px-4 py-3 text-[0.8125rem] text-[var(--ui-fg-faint)]">Loading API tokens…</p> : null}
      {query.error ? <div className="px-4 py-3"><ErrorText error={query.error} /></div> : null}
      {pause.error ? <div className="px-4 py-3"><ErrorText error={pause.error} /></div> : null}
      {query.isSuccess && tokens.length === 0 ? <p className="px-4 py-3 text-[0.8125rem] text-[var(--ui-fg-faint)]">No active API tokens.</p> : null}
      <ul className="divide-y divide-[var(--ui-divider)]">
        {tokens.map((token) => {
          const expired = isExpired(token);
          const paused = token.paused_at !== null;
          return (
            <li key={token.id} className="flex items-center gap-3 px-4 py-2.5">
              <div className={`flex min-w-0 flex-1 items-center gap-3${paused ? " opacity-60" : ""}`}>
                <span className="flex size-8 shrink-0 items-center justify-center rounded-md border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] text-[var(--ui-fg-dim)]">
                  <Key className="size-4" aria-hidden="true" />
                </span>
                <div className="flex min-w-0 flex-1 flex-col gap-2 sm:flex-row sm:items-center sm:gap-3">
                  <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                    <span className="flex items-center gap-2 text-[0.8125rem] font-medium text-[var(--ui-fg)]">
                      <span className="truncate">{token.name}</span>
                      {paused ? <StatusBadge tone="warning">Paused</StatusBadge> : null}
                      {expired ? <StatusBadge tone="error">Expired</StatusBadge> : null}
                    </span>
                    <span className="text-xs text-[var(--ui-fg-faint)] [overflow-wrap:anywhere]">
                      <code className="font-mono">{token.prefix}…</code>
                      {` · ${resourceSummary(token.resources, projectNames, appNames)} · `}
                      {token.expires_at ? `${expired ? "expired" : "expires"} ${formatDate(token.expires_at)}` : "never expires"}
                      {` · created ${timeAgo(token.created_at)}`}
                      {token.last_used_at ? ` · used ${timeAgo(token.last_used_at)}` : null}
                    </span>
                  </div>
                  <span className="flex flex-wrap gap-1">
                    {token.scopes.map((item) => (
                      <span key={item} className="rounded border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] px-1.5 py-px font-mono text-[0.6875rem] text-[var(--ui-fg)]">{item}</span>
                    ))}
                  </span>
                </div>
              </div>
              <RowActions
                label={token.name}
                actions={[
                  { label: "Rotate", onSelect: () => ask("rotate", token) },
                  { label: paused ? "Resume" : "Pause", disabled: pause.isPending, onSelect: () => pause.mutate({ token, paused: !paused }) },
                  { label: "Revoke", danger: true, onSelect: () => ask("revoke", token) },
                ]}
              />
            </li>
          );
        })}
      </ul>
      <ConfirmDialog
        open={confirming !== null}
        title={`${confirming ? CONFIRM[confirming.kind].label : "Revoke"} ${confirming?.token.name ?? "token"}?`}
        description={confirming ? CONFIRM[confirming.kind].description : undefined}
        confirmLabel={confirming ? CONFIRM[confirming.kind].label : "Revoke"}
        confirmDisabled={typed !== confirming?.token.name}
        pending={confirm.isPending}
        error={confirm.error}
        onConfirm={() => confirming && typed === confirming.token.name && confirm.mutate(confirming)}
        onClose={close}
      >
        <Field
          label={
            <span>
              Type{" "}
              <code className="rounded border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] px-1.5 py-px font-mono font-semibold text-[var(--ui-fg)] select-all">{confirming?.token.name}</code>{" "}
              to confirm
            </span>
          }
        >
          <Input value={typed} onChange={(event) => setTyped(event.target.value)} autoComplete="off" spellCheck={false} />
        </Field>
      </ConfirmDialog>
    </LayerCard>
  );
}
