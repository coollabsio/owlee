import { Check, ChevronDown, Copy, MoreH, Trash9, X } from "reicon-react";
import {
  type ButtonHTMLAttributes,
  type InputHTMLAttributes,
  type KeyboardEvent,
  type ReactNode,
  type TextareaHTMLAttributes,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { cn } from "@/lib/utils";

type ButtonVariant = "default" | "primary" | "highlighted" | "danger" | "error" | "ghost";

export function Button({
  className,
  variant = "default",
  size,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: ButtonVariant; size?: "default" | "sm" }) {
  return (
    <button
      className={cn(
        variant === "ghost" ? "icon-button" : "button",
        (variant === "primary" || variant === "highlighted") && "button-highlighted",
        (variant === "danger" || variant === "error") && "button-error",
        size === "sm" && "h-7 px-2 text-xs",
        className,
      )}
      {...props}
    />
  );
}

export function Switch({
  checked,
  disabled,
  onCheckedChange,
  "aria-label": ariaLabel,
}: {
  checked: boolean;
  disabled?: boolean;
  onCheckedChange: (checked: boolean) => void;
  "aria-label": string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-label={ariaLabel}
      aria-checked={checked}
      disabled={disabled}
      className={cn("switch", checked && "is-on")}
      onClick={() => onCheckedChange(!checked)}
    />
  );
}

/** `included` tints an option that the selected one implies, such as Read when Delete is selected. */
export type SegmentedOption<T extends string> = { value: T; label: string; disabled?: boolean; title?: string; included?: boolean };

/** A single choice among a few options, as one row of joined buttons. */
export function Segmented<T extends string>({
  value,
  options,
  onChange,
  "aria-label": ariaLabel,
}: {
  value: T;
  options: SegmentedOption<T>[];
  onChange: (value: T) => void;
  "aria-label": string;
}) {
  return (
    <span role="group" aria-label={ariaLabel} className="inline-flex shrink-0 rounded-md border border-[var(--ui-control-border)] bg-[var(--ui-control-bg)] p-0.5">
      {options.map((option) => {
        const selected = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            aria-pressed={selected}
            disabled={option.disabled}
            title={option.title}
            className={cn(
              "h-6 rounded px-2.5 text-[0.75rem] font-medium whitespace-nowrap transition-colors disabled:cursor-not-allowed disabled:opacity-40",
              selected
                ? "bg-[var(--ui-accent)] text-[var(--ui-accent-fg)]"
                : option.included
                  ? "bg-[color-mix(in_oklab,var(--ui-accent)_22%,transparent)] text-[var(--ui-fg)]"
                  : "text-[var(--ui-fg-dim)] enabled:hover:bg-[var(--ui-hover)] enabled:hover:text-[var(--ui-fg)]",
            )}
            onClick={() => onChange(option.value)}
          >
            {option.label}
          </button>
        );
      })}
    </span>
  );
}

export function Input(props: InputHTMLAttributes<HTMLInputElement>) {
  return <input {...props} className={cn("input", props.className)} />;
}

/** Copy icon that sits inside the top-right corner of a `relative` field. */
export function CopyButton({ text, label = "Copy" }: { text: string; label?: string }) {
  const { copied, copy } = useCopy();
  return (
    <button type="button" className="icon-button absolute right-1 top-1 size-6!" aria-label={copied ? "Copied" : label} onClick={() => void copy(text)}>
      {copied ? <Check className="size-3.5 text-[var(--ui-success)]" /> : <Copy className="size-3.5" />}
    </button>
  );
}

/** Read-only input with the copy button inside it. */
export function CopyInput({ value, label, className }: { value: string; label?: string; className?: string }) {
  return (
    <div className="relative">
      <Input readOnly value={value} className={cn("pr-9 font-mono", className)} onFocus={(event) => event.currentTarget.select()} />
      <CopyButton text={value} label={label} />
    </div>
  );
}

export function Textarea(props: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return <textarea {...props} className={cn("input", props.className)} />;
}

export type SelectOption<T extends string> = { value: T; label: string };

/* Custom listbox (DESIGN.md P4: no native <select>). The menu renders in a
   portal with fixed position, so a card or modal edge never clips it. */
export function Select<T extends string>({
  value,
  options,
  onChange,
  placeholder = "Choose",
  className,
  disabled,
  "aria-label": ariaLabel,
}: {
  value: T;
  options: SelectOption<T>[];
  onChange: (value: T) => void;
  placeholder?: string;
  className?: string;
  disabled?: boolean;
  "aria-label"?: string;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const [rect, setRect] = useState<DOMRect | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const listId = useId();
  const selected = options.find((option) => option.value === value);

  function show() {
    if (disabled || !trigger.current) return;
    setRect(trigger.current.getBoundingClientRect());
    setActive(Math.max(0, options.findIndex((option) => option.value === value)));
    setOpen(true);
  }

  function choose(option: SelectOption<T>) {
    onChange(option.value);
    setOpen(false);
    trigger.current?.focus();
  }

  useEffect(() => {
    if (!open) return;
    const close = (event: Event) => {
      if (event.type === "mousedown" && (trigger.current?.contains(event.target as Node) || panel.current?.contains(event.target as Node))) return;
      if (event.type === "scroll" && panel.current?.contains(event.target as Node)) return;
      setOpen(false);
    };
    document.addEventListener("mousedown", close);
    window.addEventListener("resize", close);
    window.addEventListener("scroll", close, true);
    return () => {
      document.removeEventListener("mousedown", close);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [open]);

  function onKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(event.key)) {
        event.preventDefault();
        show();
      }
      return;
    }
    if (event.key === "Escape" || event.key === "Tab") setOpen(false);
    else if (event.key === "ArrowDown") setActive((index) => Math.min(options.length - 1, index + 1));
    else if (event.key === "ArrowUp") setActive((index) => Math.max(0, index - 1));
    else if ((event.key === "Enter" || event.key === " ") && options[active]) choose(options[active]);
    else return;
    event.preventDefault();
  }

  const below = rect ? window.innerHeight - rect.bottom > 280 || rect.top < 280 : true;
  return (
    <>
      <button
        ref={trigger}
        type="button"
        className={cn("input select-trigger", className)}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        aria-label={ariaLabel}
        onClick={() => (open ? setOpen(false) : show())}
        onKeyDown={onKeyDown}
      >
        <span className={cn("truncate", !selected && "text-[var(--ui-fg-faint)]")}>{selected?.label ?? placeholder}</span>
        <ChevronDown className="size-4 shrink-0 text-[var(--ui-fg-faint)]" aria-hidden="true" />
      </button>
      {open && rect
        ? createPortal(
            <div
              ref={panel}
              id={listId}
              role="listbox"
              className="listbox-panel"
              style={{
                left: rect.left,
                minWidth: rect.width,
                ...(below ? { top: rect.bottom + 4 } : { bottom: window.innerHeight - rect.top + 4 }),
              }}
            >
              {options.map((option, index) => (
                <button
                  key={option.value}
                  type="button"
                  role="option"
                  aria-selected={option.value === value}
                  className={cn("listbox-option", index === active && "is-active")}
                  onMouseEnter={() => setActive(index)}
                  onClick={() => choose(option)}
                >
                  <span className="truncate">{option.label}</span>
                  {option.value === value ? <Check className="size-3.5 shrink-0 text-[var(--ui-fg)]" aria-hidden="true" /> : null}
                </button>
              ))}
            </div>,
            document.body,
          )
        : null}
    </>
  );
}

export function Field({
  label,
  hint,
  error,
  children,
}: {
  label: ReactNode;
  hint?: string;
  error?: string;
  children: ReactNode;
}) {
  return (
    <label className="block">
      <span className="field-label">
        <span>{label}</span>
        {hint ? <span className="ml-auto text-[0.6875rem] font-normal">{hint}</span> : null}
      </span>
      {children}
      {error ? <p className="field-error">{error}</p> : null}
    </label>
  );
}

/* Like Coolify, the title and summary of a top-level page show only below
   1024px; the sidebar already names the page on desktop. Detail pages keep
   their title, because it names the item that is open. */
export function PageHeader({
  title,
  description,
  actions,
  keepTitle = false,
}: {
  title: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  keepTitle?: boolean;
}) {
  return (
    <div className={cn("page-header", !keepTitle && "is-mobile-title", !keepTitle && !actions && "is-empty-desktop")}>
      <div className="page-header-text">
        <h1>{title}</h1>
        {description ? <p>{description}</p> : null}
      </div>
      {actions ? <div className="page-header-actions flex flex-wrap items-center gap-2">{actions}</div> : null}
    </div>
  );
}

export function LayerCard({
  title,
  description,
  actions,
  flush,
  children,
  className,
}: {
  title?: string;
  description?: string;
  actions?: ReactNode;
  flush?: boolean;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cn("layer-card", className)}>
      {title ? (
        <header>
          <div className="min-w-0 py-0.5">
            <h2>{title}</h2>
            {description ? <p>{description}</p> : null}
          </div>
          {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
        </header>
      ) : null}
      <div className={cn("layer-card-body", flush && "is-flush", !title && "rounded-lg")}>{children}</div>
    </section>
  );
}

export function Collapsible({
  title,
  defaultOpen = false,
  open: controlledOpen,
  onOpenChange,
  children,
  className,
}: {
  title: ReactNode;
  defaultOpen?: boolean;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  children: ReactNode;
  className?: string;
}) {
  const [localOpen, setLocalOpen] = useState(defaultOpen);
  const open = controlledOpen ?? localOpen;
  const setOpen = (next: boolean) => {
    setLocalOpen(next);
    onOpenChange?.(next);
  };
  const id = useId();
  return (
    <div className={cn("rounded-lg border border-[var(--ui-card-border)]", className)}>
      <button
        type="button"
        className="flex w-full items-center justify-between gap-2 rounded-lg px-3 py-2.5 text-left text-[0.8125rem] font-medium text-[var(--ui-fg-dim)] transition-colors hover:bg-[var(--ui-card-bg)] hover:text-[var(--ui-fg)]"
        aria-expanded={open}
        aria-controls={id}
        onClick={() => setOpen(!open)}
      >
        {title}
        <ChevronDown className={cn("size-3.5 shrink-0 transition-transform duration-200", open && "rotate-180")} aria-hidden="true" />
      </button>
      <div
        id={id}
        inert={!open}
        className={cn("grid transition-[grid-template-rows] duration-200 ease-[var(--ui-ease-out)] motion-reduce:transition-none", open ? "grid-rows-[1fr]" : "grid-rows-[0fr]")}
      >
        <div className="min-h-0 overflow-hidden">
          <div className="border-t border-[var(--ui-card-border)] p-3">{children}</div>
        </div>
      </div>
    </div>
  );
}

export function StatusBadge({
  tone = "neutral",
  children,
}: {
  tone?: "neutral" | "success" | "warning" | "error" | "info";
  children: ReactNode;
}) {
  return (
    <span className={cn("status-badge", tone)}>
      <i />
      <span className="truncate">{children}</span>
    </span>
  );
}

const focusable = 'a[href], button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), [tabindex]:not([tabindex="-1"])';

export function Dialog({
  open,
  title,
  description,
  onClose,
  children,
  footer,
  wide,
}: {
  open: boolean;
  title: string;
  description?: string;
  onClose: () => void;
  children?: ReactNode;
  /** Actions on the right, below a divider. */
  footer?: ReactNode;
  wide?: boolean;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const close = useRef(onClose);
  close.current = onClose;

  useEffect(() => {
    if (!open || !panel.current) return;
    const node = panel.current;
    const previous = document.activeElement as HTMLElement | null;
    (node.querySelector<HTMLElement>("[data-autofocus], input:not(:disabled), textarea:not(:disabled)") ?? node).focus();
    function onKeyDown(event: globalThis.KeyboardEvent) {
      // An open Select inside the dialog handles its own Escape first.
      if (event.defaultPrevented) return;
      if (event.key === "Escape") {
        event.preventDefault();
        close.current();
        return;
      }
      if (event.key !== "Tab") return;
      const items = [...node.querySelectorAll<HTMLElement>(focusable)];
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (event.shiftKey && (document.activeElement === first || document.activeElement === node)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      } else if (!node.contains(document.activeElement)) {
        event.preventDefault();
        first.focus();
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      if (previous?.isConnected) previous.focus();
    };
  }, [open]);

  if (!open) return null;
  return (
    <div className="modal-backdrop">
      <button type="button" className="absolute inset-0" tabIndex={-1} aria-label="Close dialog" onClick={onClose} />
      <div ref={panel} tabIndex={-1} className={cn("modal-panel outline-none", wide && "is-wide")} role="dialog" aria-modal="true" aria-labelledby={titleId}>
        <div className="modal-heading">
          <div>
            <h2 id={titleId}>{title}</h2>
            {description ? <p>{description}</p> : null}
          </div>
          <button type="button" className="icon-button" onClick={onClose} aria-label="Close">
            <X className="size-4" />
          </button>
        </div>
        {children ? <div className="modal-body">{children}</div> : null}
        {footer ? <div className="modal-footer">{footer}</div> : null}
      </div>
    </div>
  );
}

/** Asks before a destructive action. Cancel has the first focus, so Enter does not delete by accident. */
export function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel = "Delete",
  confirmDisabled,
  pending,
  error,
  onConfirm,
  onClose,
  children,
}: {
  open: boolean;
  title: string;
  description?: string;
  confirmLabel?: string;
  confirmDisabled?: boolean;
  pending?: boolean;
  error?: unknown;
  onConfirm: () => void;
  onClose: () => void;
  children?: ReactNode;
}) {
  return (
    <Dialog
      open={open}
      title={title}
      description={description}
      onClose={onClose}
      footer={
        <>
          <Button type="button" data-autofocus onClick={onClose}>Cancel</Button>
          <Button type="button" variant="danger" onClick={onConfirm} disabled={pending || confirmDisabled}>
            {pending ? <Spinner /> : null}
            {confirmLabel}
          </Button>
        </>
      }
    >
      {children || error ? (
        <>
          {children}
          <ErrorText error={error} />
        </>
      ) : null}
    </Dialog>
  );
}

export type RowAction = { label: string; onSelect: () => void; danger?: boolean; disabled?: boolean };

/* Coolify's row action pattern: one action is one icon button, two or more
   go in a "..." menu with the destructive ones below a divider. */
export function RowActions({ label, actions }: { label: string; actions: RowAction[] }) {
  const [rect, setRect] = useState<DOMRect | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!rect) return;
    const close = (event: Event) => {
      if (event.type === "mousedown" && (trigger.current?.contains(event.target as Node) || panel.current?.contains(event.target as Node))) return;
      setRect(null);
    };
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      setRect(null);
      trigger.current?.focus();
    };
    panel.current?.querySelector<HTMLElement>("button")?.focus();
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", onKeyDown);
    window.addEventListener("resize", close);
    window.addEventListener("scroll", close, true);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [rect]);

  if (actions.length === 0) return null;
  if (actions.length === 1) {
    const [action] = actions;
    if (!action.danger) {
      return (
        <Button size="sm" className="relative z-[2]" disabled={action.disabled} onClick={action.onSelect}>
          {action.label}
        </Button>
      );
    }
    return (
      <button
        type="button"
        className="icon-button is-danger relative z-[2]"
        aria-label={`${action.label} ${label}`}
        disabled={action.disabled}
        onClick={action.onSelect}
      >
        <Trash9 className="size-4" />
      </button>
    );
  }

  function choose(action: RowAction) {
    setRect(null);
    trigger.current?.focus();
    action.onSelect();
  }

  function onMenuKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    const items = [...(panel.current?.querySelectorAll<HTMLElement>("button:not(:disabled)") ?? [])];
    const index = items.indexOf(document.activeElement as HTMLElement);
    items[(index + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
  }

  const safe = actions.filter((action) => !action.danger);
  const danger = actions.filter((action) => action.danger);
  const width = 176;
  return (
    <>
      <button
        ref={trigger}
        type="button"
        className="icon-button relative z-[2]"
        aria-label={`Actions for ${label}`}
        aria-haspopup="menu"
        aria-expanded={Boolean(rect)}
        onClick={() => setRect(rect ? null : (trigger.current?.getBoundingClientRect() ?? null))}
      >
        <MoreH className="size-4" />
      </button>
      {rect
        ? createPortal(
            <div
              ref={panel}
              role="menu"
              aria-label={`Actions for ${label}`}
              className="listbox-panel row-actions-menu"
              style={{
                width,
                left: Math.max(8, Math.min(rect.right - width, window.innerWidth - width - 8)),
                ...(window.innerHeight - rect.bottom > 200 ? { top: rect.bottom + 4 } : { bottom: window.innerHeight - rect.top + 4 }),
              }}
              onKeyDown={onMenuKeyDown}
            >
              {safe.map((action) => (
                <button key={action.label} type="button" role="menuitem" className="listbox-option" disabled={action.disabled} onClick={() => choose(action)}>
                  {action.label}
                </button>
              ))}
              {safe.length > 0 && danger.length > 0 ? <div className="row-actions-divider" role="separator" /> : null}
              {danger.map((action) => (
                <button key={action.label} type="button" role="menuitem" className="listbox-option is-danger" disabled={action.disabled} onClick={() => choose(action)}>
                  {action.label}
                </button>
              ))}
            </div>,
            document.body,
          )
        : null}
    </>
  );
}

export function Spinner({ className }: { className?: string }) {
  return <span className={cn("spinner", className)} aria-hidden="true" />;
}

export function Skeleton({ className }: { className?: string }) {
  return <span className={cn("skeleton", className)} aria-hidden="true" />;
}

/** A cell value that toggles a list filter on click (Coolify's click-to-filter). */
export function FilterValue({
  active,
  label,
  onToggle,
  children,
}: {
  active: boolean;
  label: string;
  onToggle: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className={cn("table-filter-value", active && "is-active")}
      title={active ? `Remove filter ${label}` : `Filter by ${label}`}
      aria-pressed={active}
      onClick={onToggle}
    >
      {children}
    </button>
  );
}

/** The active filters above a list, each with a remove button. */
export function FilterChips({ filters, onClear }: { filters: { key: string; label: string; onRemove: () => void }[]; onClear: () => void }) {
  if (filters.length === 0) return null;
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      {filters.map((filter) => (
        <span key={filter.key} className="filter-chip">
          {filter.label}
          <button type="button" aria-label={`Remove filter ${filter.label}`} onClick={filter.onRemove}>
            <X className="size-3" />
          </button>
        </span>
      ))}
      {filters.length > 1 ? (
        <button type="button" className="text-[0.75rem] text-[var(--ui-fg-faint)] hover:text-[var(--ui-fg)]" onClick={onClear}>
          Reset filters
        </button>
      ) : null}
    </div>
  );
}

/** The footer of a keyset-paged list. Renders nothing once the last page is in. */
export function LoadMore({
  query,
  label = "Load more",
  className,
}: {
  query: { hasNextPage: boolean; isFetchingNextPage: boolean; fetchNextPage: () => unknown };
  label?: string;
  className?: string;
}) {
  if (!query.hasNextPage) return null;
  return (
    <div className={cn("flex justify-center border-t border-[var(--ui-divider)] p-2", className)}>
      <Button size="sm" disabled={query.isFetchingNextPage} onClick={() => void query.fetchNextPage()}>
        {query.isFetchingNextPage ? "Loading..." : label}
      </Button>
    </div>
  );
}

export function Empty({ title, body, action }: { title: string; body: string; action?: ReactNode }) {
  return (
    <LayerCard>
      <div className="flex min-h-40 flex-col items-center justify-center px-2 py-8 text-center">
        <h2 className="text-[0.875rem] font-medium text-[var(--ui-fg)]">{title}</h2>
        <p className="mx-auto mt-1 max-w-[28rem] text-[0.8125rem] text-[var(--ui-fg-faint)]">{body}</p>
        {action ? <div className="mt-4">{action}</div> : null}
      </div>
    </LayerCard>
  );
}

export function ErrorText({ error }: { error: unknown }) {
  if (!error) return null;
  return <p className="field-error">{error instanceof Error ? error.message : "Something went wrong."}</p>;
}

/** `copied` stays true for a moment after each successful `copy`, for "Copied" feedback. */
export function useCopy() {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  async function copy(text: string) {
    await navigator.clipboard.writeText(text);
    setCopied(true);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setCopied(false), 1500);
  }
  return { copied, copy };
}
