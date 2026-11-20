/** The host the server's `clean_host` stores for a pasted URL: no scheme, path, or port. `www.` stays. */
export function cleanHost(raw: string) {
  const host = raw.trim().replace(/^[a-z][a-z0-9+.-]*:\/\//i, "").split(/[/?#]/)[0];
  return host.replace(/\.+$/, "").split(":")[0].toLowerCase();
}

/** A default site name for a host: `www.coolify.io` becomes `coolify.io`. */
export function siteName(host: string) {
  return host.replace(/^www\./, "");
}
