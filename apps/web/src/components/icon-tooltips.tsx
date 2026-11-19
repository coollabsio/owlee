import { useEffect, useState } from "react";
import { createPortal } from "react-dom";

type Tip = { text: string; left: number; top: number; above: boolean };

/* Coolify's icon tooltips: an icon-only button shows its aria-label on hover
   and keyboard focus. Close and dismiss buttons explain themselves. Mounted
   once, so pages only need a good aria-label. */
export function IconTooltips() {
  const [tip, setTip] = useState<Tip | null>(null);

  useEffect(() => {
    let current: HTMLElement | null = null;
    function show(event: Event) {
      const target = (event.target as Element | null)?.closest?.<HTMLElement>(".icon-button[aria-label]");
      if (!target || target === current) return;
      const text = target.getAttribute("aria-label") ?? "";
      if (/^(close|dismiss)/i.test(text) || target.getAttribute("aria-expanded") === "true") return;
      current = target;
      const rect = target.getBoundingClientRect();
      const above = rect.bottom + 36 > window.innerHeight;
      setTip({ text, left: rect.left + rect.width / 2, top: above ? rect.top - 6 : rect.bottom + 6, above });
    }
    function hide(event: Event) {
      if (!current) return;
      const next = (event as FocusEvent | PointerEvent).relatedTarget as Node | null;
      if (event.type !== "pointerdown" && event.type !== "scroll" && next && current.contains(next)) return;
      current = null;
      setTip(null);
    }
    document.addEventListener("pointerover", show);
    document.addEventListener("focusin", show);
    document.addEventListener("pointerout", hide);
    document.addEventListener("focusout", hide);
    document.addEventListener("pointerdown", hide);
    window.addEventListener("scroll", hide, true);
    return () => {
      document.removeEventListener("pointerover", show);
      document.removeEventListener("focusin", show);
      document.removeEventListener("pointerout", hide);
      document.removeEventListener("focusout", hide);
      document.removeEventListener("pointerdown", hide);
      window.removeEventListener("scroll", hide, true);
    };
  }, []);

  if (!tip) return null;
  return createPortal(
    <div
      role="presentation"
      className="app-tooltip"
      style={{ left: tip.left, top: tip.top, transform: `translate(-50%, ${tip.above ? "-100%" : "0"})` }}
    >
      {tip.text}
    </div>,
    document.body,
  );
}
