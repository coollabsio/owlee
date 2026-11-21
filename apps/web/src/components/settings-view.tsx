import type { ReactNode } from "react";
import { Navigate } from "react-router";
import type { Access } from "@/app/nav";
import { PageHeader } from "@/components/ui";
import { useAccess, useMe } from "@/lib/me";

/** One settings view. Sends a user without the access to the workspace settings; the server enforces it anyway. */
export function SettingsView({
  title,
  description,
  requires,
  children,
}: {
  title: string;
  description: string;
  requires?: keyof Access;
  children: ReactNode;
}) {
  const me = useMe();
  const access = useAccess();
  if (requires && me.isPending) return null;
  if (requires && !access[requires]) return <Navigate to="/settings" replace />;
  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-6">
      <PageHeader title={title} description={description} />
      {children}
    </div>
  );
}

export function Item({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="text-[0.6875rem] font-medium text-[var(--ui-fg-faint)]">{label}</dt>
      <dd className="mt-1 truncate text-[0.8125rem] text-[var(--ui-fg)]">{children}</dd>
    </div>
  );
}
