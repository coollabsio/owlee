export type JobType = "http" | "tcp";

const httpPorts = new Set(["80", "8000", "8080"]);
const httpsPorts = new Set(["443", "8443"]);
const defaultPorts: Record<string, string> = { "http:": "80", "https:": "443" };

function portOf(value: string) {
  return /^[^/?#\s]+:(\d+)$/.exec(value)?.[1];
}

/** A URL or a bare domain is HTTP. `host:port` is TCP unless the port is a web port. */
export function inferType(raw: string): JobType {
  const value = raw.trim();
  if (value.includes("://")) return "http";
  const port = portOf(value);
  return port && !httpPorts.has(port) && !httpsPorts.has(port) ? "tcp" : "http";
}

/** The target the server checks for what the user typed. */
export function normalizeTarget(jobType: JobType, raw: string) {
  const value = raw.trim();
  if (jobType === "http") {
    if (!value || value.includes("://")) return value;
    const port = portOf(value);
    return `${port && httpPorts.has(port) ? "http" : "https"}://${value}`;
  }
  if (!value.includes("://")) return value;
  try {
    const url = new URL(value);
    return `${url.hostname}:${url.port || defaultPorts[url.protocol] || ""}`;
  } catch {
    return value.replace(/^[a-z][a-z0-9+.-]*:\/\//i, "");
  }
}

export function hostOf(jobType: JobType, target: string) {
  if (jobType === "tcp") return target.replace(/:\d*$/, "").replace(/^\[(.*)\]$/, "$1");
  try {
    return new URL(target).hostname;
  } catch {
    return "";
  }
}
