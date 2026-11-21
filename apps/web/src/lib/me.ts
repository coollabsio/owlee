import { useQuery } from "@tanstack/react-query";
import { useMemo } from "react";
import { api, type User } from "@/lib/api";
import type { Access } from "@/app/nav";
import { useWorkspace } from "@/lib/workspace";

export function useMe() {
  return useQuery({ queryKey: ["me"], queryFn: () => api<User>("/api/auth/me") });
}

// Members are read-only; the server enforces it, this only hides controls that would 403.
export function useCanEdit() {
  const { role } = useWorkspace().workspace;
  return role === "owner" || role === "admin";
}

export function useAccess(): Access {
  const me = useMe().data;
  const editor = useCanEdit();
  const owner = useWorkspace().workspace.role === "owner";
  const root = me?.root === true;
  const admin = root || me?.installation_admin === true;
  return useMemo(() => ({ editor, owner, admin, root }), [editor, owner, admin, root]);
}
