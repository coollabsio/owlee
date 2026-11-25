/** One rrweb event. Only the fields the app reads are typed; the player gets the whole object. */
export type RrwebEvent = { type: number; timestamp: number; data?: unknown };

export type ReplaySummary = {
  id: string;
  replayType: "session" | "buffer";
  /** False when the segment with the full page snapshot was not stored, so nothing can be played. */
  hasStart: boolean;
  startedMs: number | null;
  lastMs: number;
  receivedMs: number;
  lastReceivedMs: number;
  durationMs: number | null;
  segmentCount: number;
  bytes: number;
  firstUrl: string | null;
  urlCount: number;
  release: string | null;
  environment: string | null;
  browser: string | null;
  os: string | null;
  userLabel: string | null;
  errorCount: number;
  live: boolean;
};

export type ReplayError = { eventId: string; issueId: string; title: string; timestampMs: number };

export type ReplayDetail = ReplaySummary & { urls: string[]; segments: number[]; errors: ReplayError[] };

export type ReplayPage = { items: ReplaySummary[]; nextCursor: string | null };

export type ReplaySettings = {
  enabled: boolean;
  dailyBytes: number;
  hourlyLimit: number;
  todayBytes: number;
  hourReplays: number;
  totalBytes: number;
};

/** Messages between the app page and `/replay-player.html`. `timeMs` counts from the first event. */
export type ToPlayer =
  | { type: "load"; events: RrwebEvent[] }
  | { type: "append"; events: RrwebEvent[] }
  | { type: "seek"; timeMs: number };

export type FromPlayer = { type: "ready" } | { type: "state"; timeMs: number; playing: boolean };

export const REPLAY_PRIVACY_TEXT =
  "A replay holds what the user saw on the page and what they did, as far as the SDK did not mask it. Owlee cannot check the masking. Keep the SDK defaults: maskAllText, maskAllInputs, and blockAllMedia.";

/** Below this an rrweb timestamp is in seconds: the Sentry SDK writes its `performanceSpan` custom events that way. */
const SECONDS_BEFORE = 1e11;

/** Segments in segment order, then by time in ms. The sort is stable, so events with one timestamp keep the order they were recorded in. */
export function mergeSegments(segments: RrwebEvent[][]): RrwebEvent[] {
  return segments
    .flat()
    .map((event) => (event.timestamp < SECONDS_BEFORE ? { ...event, timestamp: event.timestamp * 1000 } : event))
    .sort((a, b) => a.timestamp - b.timestamp);
}

export type TimelineItem = {
  kind: "error" | "breadcrumb" | "navigation";
  /** Epoch ms. */
  atMs: number;
  label: string;
  detail: string | null;
  /** Set for errors: the issue and event to open. */
  error?: ReplayError;
};

const CUSTOM_EVENT = 5;

type BreadcrumbPayload = { timestamp?: number; category?: string; message?: string; data?: { to?: string; url?: string } };
type SpanPayload = { op?: string; description?: string; startTimestamp?: number };

/** Breadcrumbs and navigations from the SDK's custom rrweb events, plus the linked errors, by time. */
export function replayTimeline(events: RrwebEvent[], errors: ReplayError[]): TimelineItem[] {
  const items: TimelineItem[] = errors.map((error) => ({ kind: "error", atMs: error.timestampMs, label: error.title, detail: null, error }));
  for (const event of events) {
    if (event.type !== CUSTOM_EVENT) continue;
    const data = event.data as { tag?: string; payload?: unknown } | undefined;
    if (data?.tag === "breadcrumb") {
      const crumb = (data.payload ?? {}) as BreadcrumbPayload;
      const atMs = typeof crumb.timestamp === "number" ? crumb.timestamp * 1000 : event.timestamp;
      if (crumb.category === "navigation") {
        items.push({ kind: "navigation", atMs, label: crumb.data?.to ?? crumb.message ?? "Navigation", detail: null });
      } else {
        items.push({ kind: "breadcrumb", atMs, label: crumb.category ?? "Breadcrumb", detail: crumb.message ?? null });
      }
    } else if (data?.tag === "performanceSpan") {
      const span = (data.payload ?? {}) as SpanPayload;
      if (!span.op?.startsWith("navigation.") || !span.description) continue;
      const atMs = typeof span.startTimestamp === "number" ? span.startTimestamp * 1000 : event.timestamp;
      items.push({ kind: "navigation", atMs, label: span.description, detail: null });
    }
  }
  return items.sort((a, b) => a.atMs - b.atMs);
}

/** The replay page for an event, opened 5 seconds before the error. */
export function watchReplayPath(appId: string, replay: { id: string; startedMs: number | null }, eventTimestamp: string) {
  const path = `/errors/apps/${appId}/replays/${replay.id}`;
  const errorMs = Date.parse(eventTimestamp);
  if (replay.startedMs == null || Number.isNaN(errorMs)) return path;
  return `${path}?t=${Math.max(0, errorMs - replay.startedMs - 5000)}`;
}

/** "1:05", or "1:02:05" past an hour. */
export function clock(ms: number) {
  const total = Math.max(0, Math.floor(ms / 1000));
  const seconds = String(total % 60).padStart(2, "0");
  const minutes = Math.floor(total / 60) % 60;
  const hours = Math.floor(total / 3600);
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, "0")}:${seconds}` : `${minutes}:${seconds}`;
}

const MIB = 1024 * 1024;

export function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < MIB) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * MIB) return `${(bytes / MIB).toFixed(1)} MB`;
  return `${(bytes / 1024 / MIB).toFixed(2)} GB`;
}

export const bytesToMb = (bytes: number) => Math.round((bytes / MIB) * 100) / 100;
export const mbToBytes = (mb: number) => Math.round(mb * MIB);
