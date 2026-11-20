import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

import type { Channel, DropReason, IntakeCounts } from "@/lib/api";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function oneOf<T extends string>(value: string | null, allowed: readonly T[], fallback: T): T {
  return allowed.find((item) => item === value) ?? fallback;
}

/** Splits an issue title like `TypeError: x is undefined` into its type and message. */
export function splitTitle(title: string) {
  const index = title.indexOf(": ");
  if (index <= 0) return { type: title, message: "" };
  return { type: title.slice(0, index), message: title.slice(index + 2) };
}

export function percent(value?: number | null) {
  if (value == null || Number.isNaN(value)) return "-";
  return `${(value * 100).toFixed(2)}%`;
}

export function duration(iso?: string | null) {
  if (!iso) return "-";
  const delta = Date.now() - new Date(iso).getTime();
  if (Number.isNaN(delta) || delta < 0) return "just now";
  const minutes = Math.round(delta / 60_000);
  if (minutes < 1) return "<1m";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours}h`;
  return `${Math.round(hours / 24)}d`;
}

const DROP_REASONS: Record<DropReason, string> = {
  client_rate_limit: "one client sent too fast",
  source_rate_limit: "too many events for this source",
  daily_quota: "daily cap reached",
  malformed: "the event was not valid",
  origin_not_allowed: "the page origin is not allowed",
  unverified_domain: "the domain is not verified",
  storage_failed: "the server could not store it",
};

export function intakeSummary(intake: IntakeCounts) {
  const counts = `Since the server started: ${intake.stored.toLocaleString()} stored, ${intake.dropped.toLocaleString()} dropped.`;
  if (!intake.last_drop) return counts;
  return `${counts} Last drop ${timeAgo(intake.last_drop.at)}: ${DROP_REASONS[intake.last_drop.reason]}.`;
}

export function timeAgo(iso?: string | null) {
  if (!iso) return "never";
  const delta = Date.now() - new Date(iso).getTime();
  const minutes = Math.round(delta / 60_000);
  if (Math.abs(minutes) < 1) return "just now";
  if (Math.abs(minutes) < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (Math.abs(hours) < 48) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}

export function fullTime(iso?: string | null) {
  if (!iso) return "-";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "medium",
  }).format(new Date(iso));
}

export function formatTime(iso?: string | null) {
  if (!iso) return "-";
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(iso));
}

export function toLocalInput(iso?: string) {
  const date = iso ? new Date(iso) : new Date();
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

export function fromLocalInput(value: string) {
  return new Date(value).toISOString();
}

export const providerLabel: Record<Channel["provider"], string> = { email: "Email", webhook: "Webhook", slack: "Slack", discord: "Discord" };

export const stateLabel: Record<string, string> = {
  pending: "Pending",
  paused: "Paused",
  up: "Operational",
  degraded: "Degraded",
  down: "Down",
  operational: "Operational",
  partial_outage: "Partial outage",
  major_outage: "Major outage",
  success: "Up",
  failure: "Down",
  maintenance: "Maintenance",
  investigating: "Investigating",
  identified: "Identified",
  monitoring: "Monitoring",
  resolved: "Resolved",
  none: "None",
  minor: "Minor",
  major: "Major",
  critical: "Critical",
};

export function stateTone(state: string) {
  if (state === "up" || state === "success" || state === "resolved" || state === "operational") {
    return "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300";
  }
  if (state === "degraded" || state === "minor" || state === "partial_outage") {
    return "bg-amber-500/15 text-amber-800 dark:text-amber-200";
  }
  if (state === "down" || state === "failure" || state === "major" || state === "critical" || state === "major_outage") {
    return "bg-rose-500/15 text-rose-700 dark:text-rose-300";
  }
  if (state === "maintenance" || state === "monitoring" || state === "identified") {
    return "bg-sky-500/15 text-sky-800 dark:text-sky-200";
  }
  return "bg-muted text-muted-foreground";
}
