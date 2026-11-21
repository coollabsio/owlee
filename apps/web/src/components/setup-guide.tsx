import type { ReactNode } from "react";
import { Button, CopyButton } from "@/components/ui";
import { type DnsRecord, downloadText, zoneFile } from "@/lib/dns";
import type { GuideCode } from "@/lib/platform-guides";

export function GuideStep({ number, title, children }: { number: number; title: string; children: ReactNode }) {
  return (
    <li className="flex gap-3">
      <span className="flex size-6 shrink-0 items-center justify-center rounded-full border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] text-[0.75rem] font-medium tabular-nums text-[var(--ui-fg)]">
        {number}
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <h3 className="pt-0.5 text-[0.8125rem] font-medium text-[var(--ui-fg)]">{title}</h3>
        {children}
      </div>
    </li>
  );
}

export function CodeBlock({ file, code }: GuideCode) {
  return (
    <div className="min-w-0">
      <div className="field-label"><span className="truncate">{file ?? ""}</span></div>
      <div className="relative">
        <pre className="overflow-auto rounded-lg border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] py-3 pl-3 pr-10 font-mono text-[0.75rem] leading-5 text-[var(--ui-fg)]">
          {code}
        </pre>
        <CopyButton text={code} />
      </div>
    </div>
  );
}

export function ZoneDownload({ hostname, records }: { hostname: string; records: DnsRecord[] }) {
  return (
    <div className="flex flex-wrap items-center gap-3">
      <Button type="button" onClick={() => downloadText(`${hostname}.zone.txt`, zoneFile(records))}>
        Download zone file
      </Button>
      <span className="text-[0.75rem] text-[var(--ui-fg-faint)]">
        A BIND zone file. Import it at your DNS host if it accepts zone files, or add the records by hand.
      </span>
    </div>
  );
}
