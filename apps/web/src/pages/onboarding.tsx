import { useMutation } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Navigate, useNavigate } from "react-router";
import { Button, ErrorText, Field, Input } from "@/components/ui";
import { createWorkspace, signOut } from "@/lib/api";
import { useMe } from "@/lib/me";
import { switchWorkspace } from "@/lib/workspace";
import { AuthShell } from "@/pages/auth";

/** For a signed-in user who belongs to no workspace yet. */
export function OnboardingPage() {
  const navigate = useNavigate();
  const me = useMe();
  const [name, setName] = useState("");
  const create = useMutation({
    mutationFn: () => createWorkspace(name.trim()),
    onSuccess: (workspace) => {
      navigate("/", { replace: true });
      switchWorkspace(workspace.id);
    },
  });

  if ((me.data?.workspaces.length ?? 0) > 0) return <Navigate to="/" replace />;

  async function leave() {
    await signOut();
    navigate("/login");
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    create.mutate();
  }

  return (
    <AuthShell
      title="Create a workspace"
      subtitle="Monitors, apps, and sites live in a workspace. You can invite others to it later."
      footer={
        <>
          Signed in as {me.data?.email}.{" "}
          <button type="button" className="auth-text-link" onClick={() => void leave()}>
            Sign out
          </button>
        </>
      }
    >
      <form className="space-y-4" onSubmit={submit}>
        <Field label="Workspace name">
          <Input required autoFocus value={name} onChange={(event) => setName(event.target.value)} placeholder="Acme" />
        </Field>
        <ErrorText error={create.error} />
        <Button className="w-full" variant="primary" type="submit" disabled={create.isPending || !name.trim()}>
          {create.isPending ? "Creating…" : "Create workspace"}
        </Button>
        <p className="text-[13px] text-[var(--ui-fg-faint)]">
          Joining a team instead? Open the invite link someone sent you. It adds you to their workspace.
        </p>
      </form>
    </AuthShell>
  );
}
