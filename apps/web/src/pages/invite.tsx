import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { Button, ErrorText, Field, Input, Spinner } from "@/components/ui";
import { ApiError, api, type InvitationLookup, signOut, type User } from "@/lib/api";
import { useMe } from "@/lib/me";
import { switchWorkspace } from "@/lib/workspace";
import { AuthShell } from "@/pages/auth";

/** Public: anyone with the link sees the invite. Accepting needs the invited
    account, or creates it from the link when the address has none yet and an
    installation admin sent the invite. */
export function InvitePage() {
  const { token = "" } = useParams();
  const navigate = useNavigate();
  const here = `/invite/${token}`;
  const me = useMe();
  const invite = useQuery({
    queryKey: ["invitation", token],
    queryFn: () => api<InvitationLookup>(`/api/invitations/${encodeURIComponent(token)}`),
  });
  const accept = useMutation({
    mutationFn: () => api<{ workspace_id: string }>(`/api/invitations/${encodeURIComponent(token)}/accept`, { method: "POST" }),
    onSuccess: ({ workspace_id }) => {
      navigate("/", { replace: true });
      switchWorkspace(workspace_id);
    },
  });

  if (invite.isLoading || me.isLoading) {
    return (
      <AuthShell title="Invitation" subtitle="Checking the link." footer={null}>
        <p className="text-sm text-[var(--ui-fg-faint)]">Opening…</p>
      </AuthShell>
    );
  }

  if (!invite.data) {
    return (
      <AuthShell
        title="Invitation not found"
        subtitle="The link is invalid, expired, revoked, or already used."
        footer={<Link className="auth-text-link" to="/">Go to Owlee</Link>}
      >
        <p className="text-sm text-[var(--ui-fg-faint)]">Ask the person who invited you for a new link.</p>
      </AuthShell>
    );
  }

  const { workspace_name, email, role, account_exists, can_register } = invite.data;
  const user = me.data;
  const signedOut = me.error instanceof ApiError && me.error.status === 401;
  const wrongAccount = user != null && user.email.toLowerCase() !== email.toLowerCase();

  return (
    <AuthShell
      title={`Join ${workspace_name}`}
      subtitle={`You are invited as ${role === "admin" ? "an admin" : "a member"}. The invite is for ${email}.`}
      footer={null}
    >
      {user ? (
        <div className="space-y-4">
          <p className="text-sm text-[var(--ui-fg-faint)]">
            Signed in as <span className="text-[var(--ui-fg)]">{user.email}</span>.
            {wrongAccount ? ` Sign in as ${email} to accept this invite.` : ""}
          </p>
          {wrongAccount ? (
            <Button className="w-full" onClick={() => void signOut().then(() => navigate("/login", { state: { from: here } }))}>
              Sign out and switch account
            </Button>
          ) : null}
          <ErrorText error={accept.error} />
          <Button className="w-full" variant="primary" disabled={accept.isPending || wrongAccount} onClick={() => accept.mutate()}>
            {accept.isPending ? "Joining…" : `Accept and open ${workspace_name}`}
          </Button>
        </div>
      ) : signedOut && can_register ? (
        <InviteSignUpForm token={token} workspaceName={workspace_name} />
      ) : signedOut && !account_exists ? (
        <div className="space-y-4">
          <p className="text-sm text-[var(--ui-fg-faint)]">
            {email} has no account yet. Create one with Register, or ask an installation admin to create it, then open this link again.
          </p>
          <Link className="button button-highlighted w-full" to="/register">
            Register
          </Link>
        </div>
      ) : signedOut ? (
        <Link className="button button-highlighted w-full" to="/login" state={{ from: here }}>
          Sign in to accept
        </Link>
      ) : (
        <ErrorText error={me.error} />
      )}
    </AuthShell>
  );
}

function InviteSignUpForm({ token, workspaceName }: { token: string; workspaceName: string }) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const [displayName, setDisplayName] = useState("");
  const [password, setPassword] = useState("");
  const [fields, setFields] = useState<Record<string, string>>({});
  const register = useMutation({
    mutationFn: () =>
      api<User>(`/api/invitations/${encodeURIComponent(token)}/register`, {
        method: "POST",
        body: JSON.stringify({ password, display_name: displayName || undefined }),
      }),
    onSuccess: (user) => {
      client.clear();
      navigate("/", { replace: true });
      // A new account has exactly one workspace, the one it was invited to.
      if (user.workspaces[0]) switchWorkspace(user.workspaces[0].id);
    },
    onError: (error) => setFields(error instanceof ApiError ? (error.fields ?? {}) : {}),
  });
  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    register.mutate();
  }
  return (
    <form className="space-y-4" onSubmit={submit}>
      <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">Choose a name and a password to create your account and join.</p>
      <Field label="Name" error={fields.display_name}>
        <Input value={displayName} onChange={(event) => setDisplayName(event.target.value)} placeholder="Andras Bacsai" />
      </Field>
      <Field label="Password" hint="12+ characters" error={fields.password}>
        <Input type="password" required autoComplete="new-password" value={password} onChange={(event) => setPassword(event.target.value)} />
      </Field>
      <ErrorText error={Object.keys(fields).length ? null : register.error} />
      <Button className="w-full" variant="primary" type="submit" disabled={register.isPending}>
        {register.isPending ? <><Spinner />Working…</> : `Create account and join ${workspaceName}`}
      </Button>
    </form>
  );
}
