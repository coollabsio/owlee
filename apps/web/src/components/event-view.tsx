import type { Frame } from "@/lib/api";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/issue-badge";

export function StackFrames({ frames, inAppOnly }: { frames: Frame[]; inAppOnly: boolean }) {
  const visible = [...frames].reverse().filter((frame) => (inAppOnly ? frame.inApp : true));
  if (visible.length === 0) {
    return <p className="border-t border-[var(--ui-divider)] px-4 py-4 text-[0.8125rem] text-[var(--ui-fg-faint)]">No stack frames on this exception.</p>;
  }
  return (
    <ol>
      {visible.map((frame, index) => (
        <li key={index} className="border-t border-[var(--ui-divider)]">
          <div className="flex items-start justify-between gap-3 px-4 py-2.5">
            <div className="min-w-0">
              <div className={frame.inApp ? "text-[0.8125rem] font-medium text-[var(--ui-fg)]" : "text-[0.8125rem] text-[var(--ui-fg-dim)]"}>
                {frame.function || "<anonymous>"}
              </div>
              <div className="break-all font-mono text-[0.75rem] text-[var(--ui-fg-faint)]">
                {frame.filename || frame.absPath || frame.module || "unknown"}
                {frame.lineno ? `:${frame.lineno}` : ""}
                {frame.colno ? `:${frame.colno}` : ""}
              </div>
            </div>
            {frame.inApp ? <Badge tone="info">In app</Badge> : <Badge>Library</Badge>}
          </div>
          <Context frame={frame} />
        </li>
      ))}
    </ol>
  );
}

function Context({ frame }: { frame: Frame }) {
  const line = frame.lineno ?? 0;
  const rows: { n: number; text: string; current: boolean }[] = [];
  frame.preContext.forEach((text, index) => {
    rows.push({ n: line - frame.preContext.length + index, text, current: false });
  });
  if (frame.contextLine != null) rows.push({ n: line, text: frame.contextLine, current: true });
  frame.postContext.forEach((text, index) => {
    rows.push({ n: line + 1 + index, text, current: false });
  });
  if (rows.length === 0) return null;
  return (
    <div className="mx-4 mb-3 overflow-auto rounded-lg border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] py-1 font-mono text-[0.75rem] leading-5">
      {rows.map((row) => (
        <div
          key={`${row.n}-${row.text}`}
          className={cn(
            "grid min-w-max grid-cols-[3rem_1fr] gap-3 whitespace-pre pr-3",
            row.current ? "bg-[color-mix(in_oklab,var(--ui-accent)_14%,transparent)] text-[var(--ui-fg)]" : "text-[var(--ui-fg-dim)]",
          )}
        >
          <span className="select-none text-right text-[var(--ui-fg-faint)]">{row.n || ""}</span>
          <span>{row.text || " "}</span>
        </div>
      ))}
    </div>
  );
}
