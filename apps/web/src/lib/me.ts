import { useQuery } from "@tanstack/react-query";
import { api, type User } from "@/lib/api";
import { useWorkspace } from "@/lib/workspace";

export function useMe() {
  return useQuery({ queryKey: ["me"], queryFn: () => api<User>("/api/auth/me") });
}

// Members are read-only; the server enforces it, this only hides controls that would 403.
export function useCanEdit() {
  const { role } = useWorkspace().workspace;
  return role === "owner" || role === "admin";
}
