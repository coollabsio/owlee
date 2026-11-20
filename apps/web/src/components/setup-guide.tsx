import type { ReactNode } from "react";
import { useCopy } from "@/components/ui";
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
  const { copied, copy } = useCopy();
  return (
    <div className="min-w-0">
      <div className="field-label">
        <span className="truncate">{file ?? ""}</span>
        <button
          type="button"
          className="ml-auto text-[0.6875rem] font-normal hover:text-[var(--ui-fg)]"
          onClick={() => copy(code)}
        >
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      <pre className="overflow-auto rounded-lg border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] p-3 font-mono text-[0.75rem] leading-5 text-[var(--ui-fg)]">
        {code}
      </pre>
    </div>
  );
}
