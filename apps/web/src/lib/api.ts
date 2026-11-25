import { queryClient } from "@/lib/query-client";
import type { ReplaySettings } from "@/lib/replay";
import { currentWorkspaceId } from "@/lib/workspace";

export class ApiError extends Error {
  status: number;
  fields?: Record<string, string>;

  constructor(message: string, status: number, fields?: Record<string, string>) {
    super(message);
    this.status = status;
    this.fields = fields;
  }
}

export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers);
  if (init?.body && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  const workspaceId = currentWorkspaceId();
  if (workspaceId && !headers.has("x-owlee-workspace")) headers.set("x-owlee-workspace", workspaceId);
  const response = await fetch(path, { ...init, headers });
  if (response.status === 204) return undefined as T;
  const text = await response.text();
  const data = text ? (JSON.parse(text) as Record<string, unknown>) : {};
  if (!response.ok) {
    throw new ApiError(
      typeof data.detail === "string" ? data.detail : "Request failed",
      response.status,
      data.fields as Record<string, string> | undefined,
    );
  }
  return data as T;
}

export type Bar = {
  day: string;
  status: "up" | "degraded" | "down" | "maintenance" | "empty" | string;
  uptime_ratio: number | null;
  total: number;
  avg_latency_ms: number | null;
};

export type Check = {
  id: string;
  status: string;
  status_code: number | null;
  latency_ms: number;
  error: string | null;
  region: string;
  checked_at: string;
};

/** A saved request header. Its value never leaves the server. */
export type SavedHeader = { name: string; has_value: boolean };
export type Assertion = { type: string; compare?: string; target?: string | number; key?: string };

export type Monitor = {
  id: string;
  project_id: string;
  name: string;
  description: string;
  job_type: "http" | "tcp";
  check_type: "http" | "tcp" | "postgres" | "mysql" | "redis";
  url: string;
  method: string;
  headers: SavedHeader[];
  has_body: boolean;
  /** The saved headers and body do not open with OWLEE_APP_KEY. */
  request_unreadable: boolean;
  timeout_ms: number;
  interval_seconds: number;
  degraded_after_ms: number | null;
  expected_status: number | null;
  assertions: Assertion[];
  follow_redirects: boolean;
  failure_threshold: number;
  active: boolean;
  agent_id: string | null;
  /** Down because the agent is silent. No check failed. */
  agent_offline: boolean;
  state: "pending" | "up" | "degraded" | "down";
  consecutive_failures: number;
  last_checked_at: string | null;
  next_check_at: string;
  created_at: string;
  /** When the shown state (including paused) last changed. */
  state_changed_at: string;
  uptime_24h: number | null;
  uptime_30d: number | null;
  uptime_90d: number | null;
  avg_latency_ms: number | null;
  bars: Bar[];
  recent?: Check[];
};

/** The detail view: a monitor plus what it is connected to. */
export type MonitorDetail = Monitor & {
  open_incident_id: string | null;
  /** Empty for members, who may not read channels. */
  alert_channels: { id: string; name: string; provider: Channel["provider"] }[];
  alert_channel_count: number;
  status_pages: { id: string; title: string; slug: string }[];
  /** The newest check with an error, even when it is older than `recent`. */
  last_error: Check | null;
  /** Checks that got an answer (success or degraded) in the last 24 hours. */
  latency_24h: { p50: number; p95: number; max: number } | null;
  /** The newest 10, newest first. */
  incidents: Pick<Incident, "id" | "title" | "status" | "started_at" | "resolved_at">[];
};

/** One check run by `POST /api/monitors/test`. Nothing is stored. */
export type CheckOutcome = {
  status: "success" | "degraded" | "failure";
  status_code: number | null;
  latency_ms: number;
  error: string | null;
};

export type Agent = {
  id: string;
  project_id: string;
  name: string;
  last_seen_at: string | null;
  online: boolean;
  monitor_count: number;
  created_at: string;
};

export type CreatedAgent = Agent & { token: string };

export const createAgent = (input: { name: string; project_id: string }) =>
  api<CreatedAgent>("/api/agents", { method: "POST", body: JSON.stringify(input) });

export const renameAgent = (id: string, name: string) => api<Agent>(`/api/agents/${id}`, { method: "PUT", body: JSON.stringify({ name }) });

export const rotateAgentToken = (id: string) => api<CreatedAgent>(`/api/agents/${id}/token`, { method: "POST" });

export const deleteAgent = (id: string) => api<void>(`/api/agents/${id}`, { method: "DELETE" });

export type Component = {
  id: string;
  monitor_id: string;
  name: string;
  description: string;
  position: number;
};

export type StatusPage = {
  id: string;
  project_id: string;
  title: string;
  description: string;
  slug: string;
  published: boolean;
  homepage_url: string | null;
  created_at: string;
  components: Component[];
};

export type IncidentUpdate = {
  id: string;
  status: string;
  message: string;
  created_at: string;
};

export type Incident = {
  id: string;
  project_id: string;
  page_id: string | null;
  monitor_id: string | null;
  title: string;
  status: "investigating" | "identified" | "monitoring" | "resolved";
  impact: "none" | "minor" | "major" | "critical";
  auto: boolean;
  started_at: string;
  resolved_at: string | null;
  updates: IncidentUpdate[];
};

export type Maintenance = {
  id: string;
  project_id: string;
  page_id: string | null;
  title: string;
  message: string;
  starts_at: string;
  ends_at: string;
  monitor_ids: string[];
  active: boolean;
};

export type Delivery = {
  id: string;
  event: string;
  status: "pending" | "sent" | "failed" | "skipped";
  error: string | null;
  attempts: number;
  /** Set while a failed delivery waits for its next attempt. */
  next_attempt_at: string | null;
  created_at: string;
};

export type Channel = {
  id: string;
  name: string;
  provider: "email" | "webhook" | "slack" | "discord";
  /** URLs and secrets are credentials: the API returns only their origin and whether a secret is set. */
  config: { to?: string; url_preview?: string; has_secret?: boolean; unreadable?: boolean };
  enabled: boolean;
  /** Gets new and regressed error-tracking issues. */
  error_issues: boolean;
  monitor_ids: string[];
  deliveries: Delivery[];
};

export type WorkspaceRole = "owner" | "admin" | "member";

export type UserWorkspace = { id: string; name: string; role: WorkspaceRole };

export type User = {
  id: string;
  email: string;
  display_name: string;
  root: boolean;
  installation_admin: boolean;
  workspaces: UserWorkspace[];
};

/** The container for project-owned records inside a workspace. */
export type Project = { id: string; name: string; color: string; created_at: string };

export type Member = {
  id: string;
  user_id: string;
  email: string;
  display_name: string;
  role: WorkspaceRole;
  created_at: string;
};

export type InviteRole = "admin" | "member";

export type Invitation = {
  id: string;
  email: string;
  role: InviteRole;
  expires_at: string;
  created_at: string;
};

export type CreatedInvitation = Invitation & { url: string };

export type InvitationLookup = { workspace_name: string; email: string; role: InviteRole; account_exists: boolean; can_register: boolean };

const post = <T>(path: string, body: unknown) => api<T>(path, { method: "POST", body: JSON.stringify(body) });

/** Drops every cached record too, so the next account starts clean. */
export async function signOut() {
  await api("/api/auth/logout", { method: "POST" });
  queryClient.clear();
}

export const requestRegistration = (email: string) => post<Record<string, never>>("/api/auth/register", { email });

export const completeRegistration = (input: { token: string; password: string; display_name?: string }) =>
  post<User>("/api/auth/register/complete", input);

export const requestRecovery = (email: string) => post<Record<string, never>>("/api/auth/recovery", { email });

export const completeRecovery = (input: { token: string; password: string }) =>
  post<User>("/api/auth/recovery/complete", input);

export const changePassword = (input: { current_password: string; new_password: string }) =>
  post<void>("/api/auth/password", input);

export type AuthSession = {
  id: string;
  created_at: string;
  last_activity_at: string;
  current: boolean;
};

export const listSessions = () => api<AuthSession[]>("/api/auth/sessions");

export const revokeSession = (id: string) => api<void>(`/api/auth/sessions/${id}`, { method: "DELETE" });

export type ScopeProduct = "errors" | "replays";
export type ScopeAccess = "read" | "write" | "delete";
export type ApiScope = `${ScopeProduct}:${ScopeAccess}`;

export const SCOPE_ACCESSES: ScopeAccess[] = ["read", "write", "delete"];

/** One access row in the token form for each product that tokens can reach. */
export const API_PRODUCTS: { product: ScopeProduct; label: string; hint: string; accesses: ScopeAccess[] }[] = [
  {
    product: "errors",
    label: "Errors",
    hint: "Read lists apps, issues, and events. Write also changes issue status and edits apps. Delete also deletes apps.",
    accesses: SCOPE_ACCESSES,
  },
  {
    product: "replays",
    label: "Replay recordings",
    hint: "Read downloads the recorded pages of session replays. The replay list and details need only Errors read.",
    accesses: ["read"],
  },
];

/** `null` is every resource, including future ones. A listed project covers all of its apps. */
export type TokenResources = { projects: string[]; error_apps: string[] } | null;

export type ApiToken = {
  id: string;
  name: string;
  prefix: string;
  scopes: ApiScope[];
  resources: TokenResources;
  created_at: string;
  last_used_at: string | null;
  expires_at: string | null;
  paused_at: string | null;
};

export type CreatedApiToken = ApiToken & { token: string };

export type TokenExpiry = 7 | 30 | 90 | 365 | null;

export const listApiTokens = () => api<ApiToken[]>("/api/api-tokens");

export const createApiToken = (input: { name: string; scopes: ApiScope[]; expires_in_days: TokenExpiry; resources: TokenResources }) =>
  post<CreatedApiToken>("/api/api-tokens", input);

export const revokeApiToken = (id: string) => api<void>(`/api/api-tokens/${id}`, { method: "DELETE" });

export const rotateApiToken = (id: string) => api<CreatedApiToken>(`/api/api-tokens/${id}/rotate`, { method: "POST" });

export const setApiTokenPaused = (id: string, paused: boolean) => api<ApiToken>(`/api/api-tokens/${id}/${paused ? "pause" : "resume"}`, { method: "POST" });

export type PrivacyExport = {
  user: string;
  events: { id: string; appId: string; appName: string; timestamp: string; title: string; payload: unknown }[];
};

const privacyPath = (user: string) => `/api/privacy/errors?user=${encodeURIComponent(user)}`;

export const exportUserErrors = (user: string) => api<PrivacyExport>(privacyPath(user));

export const deleteUserErrors = (user: string) => api<{ deleted: number }>(privacyPath(user), { method: "DELETE" });

export type InstanceSmtp = {
  host: string;
  port: number;
  username: string | null;
  from: string;
  has_password: boolean;
  source: "saved" | "environment";
};

export type RootUser = { id: string; email: string; display_name: string };

export type InstanceSettings = {
  registration_open: boolean;
  smtp_ready: boolean;
  smtp: InstanceSmtp | null;
  root_user: RootUser | null;
  initialized: boolean;
  version: string;
  database: string;
  error_daily_quota: number;
  analytics_daily_quota: number;
};

export type AdminUser = {
  id: string;
  email: string;
  display_name: string;
  root: boolean;
  installation_admin: boolean;
  disabled: boolean;
};

export type Audit = {
  id: string;
  action: string;
  resource_type: string;
  resource_id: string | null;
  occurred_at: string;
};

export type Overview = { monitors: Monitor[]; incidents: Incident[]; activity: Audit[] };

export type DashboardFailing = {
  id: string;
  name: string;
  url: string;
  state: string;
  since: string | null;
  uptime_today: number | null;
  avg_latency_ms: number | null;
};

export type DashboardIncident = {
  id: string;
  title: string;
  status: string;
  impact: string;
  started_at: string;
};

export type DashboardPageRow = {
  id: string;
  title: string;
  slug: string;
  published: boolean;
  banner: string;
};

export type DashboardIssue = {
  id: string;
  short_id: string | null;
  title: string;
  app_name: string;
  level: string;
  event_count: number;
  last_seen: string;
};

export type DashboardSite = {
  id: string;
  name: string;
  domain: string;
  visitors: number;
  pageviews: number;
};

export type QuotaHit = {
  kind: "error" | "analytics";
  id: string;
  name: string;
  used: number;
  quota: number;
  resets_at: string;
};

export type Dashboard = {
  monitors: {
    up: number;
    down: number;
    degraded: number;
    pending: number;
    paused: number;
    uptime_today: number | null;
    avg_latency_ms: number | null;
    failing: DashboardFailing[];
  };
  incidents: { open: number; items: DashboardIncident[] };
  pages: DashboardPageRow[];
  errors: {
    apps: number;
    unresolved: number;
    new_24h: number;
    events_24h: number[];
    issues: DashboardIssue[];
  };
  analytics: {
    sites: number;
    links: number;
    days: number;
    visitors: number;
    pageviews: number;
    previous_visitors: number;
    previous_pageviews: number;
    clicks: number;
    previous_clicks: number;
    top_sites: DashboardSite[];
  };
  quota_hits: QuotaHit[];
};

export type PublicComponent = {
  id: string;
  name: string;
  description: string;
  state: Monitor["state"];
  uptime_90d: number | null;
  bars: Bar[];
};

export type PublicPage = {
  title: string;
  description: string;
  slug: string;
  homepage_url: string | null;
  banner: "operational" | "degraded" | "partial_outage" | "major_outage";
  components: PublicComponent[];
  /** Every unresolved incident, however old. */
  open_incidents: PublicIncident[];
  /** The newest page of the history, open or not. `/incidents?cursor=` serves older ones. */
  incidents: PublicIncident[];
  incidents_next_cursor: string | null;
  maintenances: PublicMaintenance[];
};

/** `path` with `?cursor=` appended, or `path` alone for the first page. */
export function withCursor(path: string, cursor: string) {
  if (!cursor) return path;
  return `${path}${path.includes("?") ? "&" : "?"}cursor=${encodeURIComponent(cursor)}`;
}

/** Concatenates loaded pages, keeping the first copy of a row that moved between pages while they loaded. */
export function flattenPages<T extends { id: string }>(pages: T[][] | undefined) {
  const seen = new Set<string>();
  return (pages ?? []).flat().filter((row) => {
    if (seen.has(row.id)) return false;
    seen.add(row.id);
    return true;
  });
}

/** One page of a keyset-paged list. Pass `next_cursor` back as `?cursor=`; null means the last page. */
export type Paged<T> = { items: T[]; next_cursor: string | null };

export type PublicIncident = Omit<Incident, "project_id" | "page_id" | "monitor_id">;

export type PublicMaintenance = Omit<Maintenance, "project_id" | "page_id" | "monitor_ids">;

export type Meta = {
  version: string;
  signup_open: boolean;
  setup_complete: boolean;
  database: string;
  public_origin: string;
  demo: { email: string; password: string } | null;
};

export type InsightSite = {
  id: string;
  project_id: string;
  public_id: string;
  name: string;
  domain: string;
  color: string;
  public_slug: string;
  public_enabled: boolean;
  public_threshold: number;
  public_sections: string[];
  created_at: number;
  verified: boolean;
  verified_at: number | null;
  verify_name: string;
  verify_value: string;
  ingest_open: boolean;
  /** The script URL, from the server's public origin. */
  tracker_url: string;
  /** Unix seconds. Null until the site records its first event. */
  last_event_at: number | null;
  visitors_24h: number;
  pageviews_24h: number;
  daily_quota: number;
  quota_override: number | null;
  used_today: number;
  quota_exceeded: boolean;
  resets_at: string;
  intake: IntakeCounts;
};

export type DropReason =
  | "client_rate_limit"
  | "source_rate_limit"
  | "daily_quota"
  | "malformed"
  | "origin_not_allowed"
  | "unverified_domain"
  | "filtered"
  | "spike_protection"
  | "storage_failed";

/** Counts since the server started. */
export type IntakeCounts = {
  stored: number;
  dropped: number;
  last_drop: { at: string; reason: DropReason } | null;
};

export type InsightPoint = {
  day: number;
  views?: number;
  visitors?: number;
  events?: number;
  visits?: number;
  bounces?: number;
  time?: number;
  clicks?: number;
};

export type InsightOverview = {
  pageviews: number;
  visitors: number;
  events: number;
  visits: number;
  bounces: number;
  total_time: number;
  from: number;
  to: number;
  series: InsightPoint[];
};

export type InsightRow = { label: string; count: number; visitors?: number };

export type InsightDomain = {
  id: string;
  site_id: string | null;
  hostname: string;
  note: string;
  enabled: boolean;
  created_at: number;
  verified: boolean;
  verified_at: number | null;
  verify_name: string;
  verify_value: string;
  /** False when redirects on this hostname wait for verification. */
  redirect_open: boolean;
};

export type InsightLink = {
  id: string;
  project_id: string;
  domain_id: string;
  hostname: string;
  slug: string;
  destination: string;
  redirect_type: "temporary" | "permanent";
  enabled: boolean;
  public_slug: string;
  public_enabled: boolean;
  public_threshold: number;
  public_sections: string[];
  created_at: number;
  total_clicks: number;
};

export type LinkOverview = {
  clicks: number;
  visitors: number;
  from: number;
  to: number;
  series: InsightPoint[];
};

export type PublicInsight = {
  name: string;
  subtitle: string;
  sections: string[];
  pageviews?: number | null;
  clicks?: number | null;
  visitors: number | null;
  from: number;
  to: number;
  series: InsightPoint[];
};

/** An error-tracking app: one DSN, inside a project. */
export type ErrorApp = {
  id: string;
  projectId: string;
  name: string;
  slug: string;
  platform: string;
  publicKey: string;
  dsn: string;
  createdAt: string;
  dailyQuota: number;
  quotaOverride: number | null;
  usedToday: number;
  quotaExceeded: boolean;
  resetsAt: string;
  issueCount: number;
  eventCount: number;
  lastEventAt: string | null;
  intake: IntakeCounts;
  /** Empty allows every origin. */
  allowedDomains: string[];
  inboundFilters: string[];
  filterOptions: InboundFilterOption[];
  spikeProtection: boolean;
  /** The most events this app may store in the current UTC hour. */
  spikeLimit: number;
  usedThisHour: number;
  replays: ReplaySettings;
};

export type InboundFilterOption = { id: string; label: string; description: string };

export type Issue = {
  id: string;
  appId: string;
  appName: string;
  appSlug: string;
  shortId: string;
  title: string;
  culprit?: string | null;
  status: "unresolved" | "resolved" | "ignored";
  level: string;
  platform?: string | null;
  environment?: string | null;
  release?: string | null;
  firstSeen: string;
  lastSeen: string;
  eventCount: number;
  unhandled: boolean;
  priority: string;
  userCount: number;
  periodEventCount: number;
  trend24h: number[];
  trend14d: number[];
};

export type IssueFeed = {
  issues: Issue[];
  environments: string[];
  /** Per status under the same filters, ignoring the status filter. */
  counts: Record<Issue["status"], number>;
  /** Pass back as `?cursor=` for the next page. Null on the last page. */
  nextCursor: string | null;
};

/** One tag key with its most common values among the events in range. */
export type TagGroup = { key: string; values: { value: string; count: number }[] };

export type EventSummary = {
  id: string;
  timestamp: string;
  level: string;
  title: string;
  environment?: string | null;
  release?: string | null;
  /** Email, username, or id from the event's user. */
  user?: string | null;
};

export type Frame = {
  filename?: string | null;
  absPath?: string | null;
  function?: string | null;
  module?: string | null;
  lineno?: number | null;
  colno?: number | null;
  contextLine?: string | null;
  preContext: string[];
  postContext: string[];
  inApp: boolean;
};

export type ExceptionInfo = {
  type?: string | null;
  value?: string | null;
  mechanism?: string | null;
  handled?: boolean | null;
  frames: Frame[];
};

export type Breadcrumb = {
  timestamp?: string | null;
  type?: string | null;
  category?: string | null;
  level?: string | null;
  message?: string | null;
  data: Record<string, unknown>;
};

export type NormalizedEvent = {
  eventId: string;
  timestamp: string;
  level: string;
  title: string;
  message?: string | null;
  culprit?: string | null;
  platform?: string | null;
  environment?: string | null;
  release?: string | null;
  serverName?: string | null;
  transaction?: string | null;
  logger?: string | null;
  fingerprint: string;
  sdk?: { name: string; version: string } | null;
  exceptions: ExceptionInfo[];
  breadcrumbs: Breadcrumb[];
  tags: { key: string; value: string }[];
  contexts: Record<string, unknown>;
  user: Record<string, unknown>;
  request: Record<string, unknown>;
  extra: Record<string, unknown>;
  raw: unknown;
  /** The session replay this event happened in, when it exists in the same app. */
  replay: { id: string; startedMs: number | null } | null;
};

export type EventPage = { items: EventSummary[]; nextCursor: string | null };

/** The filtered events either side of one event. */
export type EventNeighbors = { olderId: string | null; newerId: string | null };

/** `periodEventCount` and `userCount` cover the filtered range. */
export type IssueDetail = Issue & {
  /** The newest filtered event. */
  latest: NormalizedEvent | null;
  /** The issue's oldest and newest stored events, ignoring filters. */
  firstEventId: string | null;
  latestEventId: string | null;
  /** Every environment in the issue's app. */
  environments: string[];
  histogram: number[];
  tags: TagGroup[];
  /** Events of this issue that have a stored replay. */
  replayEventCount: number;
};

export type Workspace = UserWorkspace & { created_at: string };

export const createWorkspace = (name: string) => post<Workspace>("/api/workspaces", { name });
