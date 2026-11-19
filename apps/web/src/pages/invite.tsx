import { useMutation, useQuery } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "react-router";
import { Button, ErrorText } from "@/components/ui";
import { ApiError, api, type InvitationLookup, signOut } from "@/lib/api";
import { useMe } from "@/lib/me";
import { switchWorkspace } from "@/lib/workspace";
import { AuthShell, rememberPendingInvite } from "@/pages/auth";

/** Public: anyone with the link sees the invite; accepting needs the invited account. */
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

  const { workspace_name, email, role } = invite.data;
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
      ) : signedOut ? (
        <div className="space-y-3">
          <Link className="button button-highlighted w-full" to="/login" state={{ from: here }}>
            Sign in to accept
          </Link>
          <Link className="button w-full" to="/register" onClick={() => rememberPendingInvite(here)}>
            Create an account for {email}
          </Link>
        </div>
      ) : (
        <ErrorText error={me.error} />
      )}
    </AuthShell>
  );
}
