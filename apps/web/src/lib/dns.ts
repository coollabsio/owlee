export type DnsRecord = {
  name: string;
  type: "TXT" | "CNAME" | "A" | "AAAA";
  value: string;
};

const TTL = 3600;

/** The record that sends a link hostname to this Owlee server. */
export function pointRecord(hostname: string, publicOrigin: string): DnsRecord {
  const host = new URL(publicOrigin).hostname.replace(/^\[|\]$/g, "");
  if (host.includes(":")) return { name: hostname, type: "AAAA", value: host };
  if (/^\d{1,3}(\.\d{1,3}){3}$/.test(host)) return { name: hostname, type: "A", value: host };
  return { name: hostname, type: "CNAME", value: host };
}

const absolute = (name: string) => (name.endsWith(".") ? name : `${name}.`);

function rdata(record: DnsRecord) {
  if (record.type === "TXT") return `"${record.value.replace(/["\\]/g, "\\$&")}"`;
  if (record.type === "CNAME") return absolute(record.value);
  return record.value;
}

/**
 * A BIND zone file. Cloudflare imports it under DNS > Records > Import and
 * Export. `cf-proxied:false` keeps the address record DNS-only, so Owlee sees
 * the visitor's address and not a Cloudflare one.
 */
export function zoneFile(records: DnsRecord[]) {
  const lines = records.map((record) => {
    const line = `${absolute(record.name)}\t${TTL}\tIN\t${record.type}\t${rdata(record)}`;
    return record.type === "TXT" ? line : `${line} ; cf_tags=cf-proxied:false`;
  });
  return `; DNS records for Owlee. Import this file at your DNS host.\n${lines.join("\n")}\n`;
}

export function downloadText(filename: string, text: string, type = "text/plain") {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.click();
  URL.revokeObjectURL(url);
}
