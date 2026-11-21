import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, type ReactNode, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router";
import { Logo } from "@/app/Shell";
import { Button, ErrorText, Field, Input, Skeleton, Spinner } from "@/components/ui";
import {
  ApiError,
  api,
  completeRecovery,
  completeRegistration,
  requestRecovery,
  requestRegistration,
  type Meta,
  type User,
} from "@/lib/api";

export function LoginPage() {
  const meta = useQuery({ queryKey: ["meta"], queryFn: () => api<Meta>("/api/meta") });
  const footer = meta.data?.signup_open ? (
    <>
      Need an account? <Link className="auth-text-link" to="/register">Create one</Link>
    </>
  ) : meta.data && !meta.data.setup_complete ? (
    <>Open the one-time setup URL printed when the server started.</>
  ) : null;
  return (
    <AuthShell
      title="Sign in"
      subtitle="Status pages and uptime for the coolLabs stack."
      footer={footer}
    >
      <LoginForm />
    </AuthShell>
  );
}

export function RegisterPage() {
  const meta = useQuery({ queryKey: ["meta"], queryFn: () => api<Meta>("/api/meta") });
  if (meta.isLoading) {
    return (
      <AuthShell title="Create an account" subtitle="Checking registration." footer={null}>
        <div className="space-y-3" aria-busy="true">
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-32" />
        </div>
      </AuthShell>
    );
  }
  if (!meta.data?.signup_open) {
    return (
      <AuthShell
        title="Registration is closed"
        subtitle="This Owlee does not accept public sign-up."
        footer={
          <>
            Already have an account? <Link className="auth-text-link" to="/login">Sign in</Link>
          </>
        }
      >
        <p className="text-sm text-[var(--ui-fg-faint)]">
          The first account is created from the one-time setup link printed by the server. The root user can open registration in Admin after a mail server is saved.
        </p>
      </AuthShell>
    );
  }
  return (
    <AuthShell
      title="Create an account"
      subtitle="Public registration is open on this install."
      footer={
        <>
          Already registered? <Link className="auth-text-link" to="/login">Sign in</Link>
        </>
      }
    >
      <EmailLinkForm
        submitLabel="Send sign-up link"
        request={requestRegistration}
        sent={(email) => (
          <>
            We sent a message to <span className="text-[var(--ui-fg)]">{email}</span>. Open the link inside to continue. It works for 24 hours.
          </>
        )}
      />
    </AuthShell>
  );
}

export function RegisterCompletePage() {
  const token = useConsumedToken();
  return (
    <AuthShell
      title="Finish your account"
      subtitle="Choose a name and a password."
      footer={
        <>
          Already registered? <Link className="auth-text-link" to="/login">Sign in</Link>
        </>
      }
    >
      {token ? (
        <TokenPasswordForm
          withName
          submitLabel="Create account"
          restart={{ to: "/register", label: "Request a new sign-up link" }}
          submit={(password, displayName) =>
            completeRegistration({ token, password, display_name: displayName || undefined })
          }
        />
      ) : (
        <MissingLink to="/register" label="Request a new sign-up link" />
      )}
    </AuthShell>
  );
}

export function ForgotPasswordPage() {
  return (
    <AuthShell
      title="Reset your password"
      subtitle="We email a link to set a new password."
      footer={
        <>
          Remembered it? <Link className="auth-text-link" to="/login">Sign in</Link>
        </>
      }
    >
      <EmailLinkForm
        submitLabel="Send reset link"
        request={requestRecovery}
        sent={(email) => (
          <>
            If an account exists for <span className="text-[var(--ui-fg)]">{email}</span>, we sent a link to reset its password. It works for one hour.
          </>
        )}
      />
    </AuthShell>
  );
}

export function ResetPasswordPage() {
  const token = useConsumedToken();
  return (
    <AuthShell
      title="Set a new password"
      subtitle="Every other device is signed out."
      footer={
        <>
          Remembered it? <Link className="auth-text-link" to="/login">Sign in</Link>
        </>
      }
    >
      {token ? (
        <TokenPasswordForm
          submitLabel="Set password"
          restart={{ to: "/forgot-password", label: "Request a new reset link" }}
          submit={(password) => completeRecovery({ token, password })}
        />
      ) : (
        <MissingLink to="/forgot-password" label="Request a new reset link" />
      )}
    </AuthShell>
  );
}

function MissingLink({ to, label }: { to: string; label: string }) {
  return (
    <p className="text-sm text-[var(--ui-fg-faint)]">
      Open the link from the email. <Link className="auth-text-link" to={to}>{label}</Link>
    </p>
  );
}

export function SetupPage() {
  const token = useConsumedToken();
  if (!token) {
    return (
      <AuthShell
        title="Set up Owlee"
        subtitle="The first account becomes the owner of this install."
        footer={
          <>
            Already set up? <Link className="auth-text-link" to="/login">Sign in</Link>
          </>
        }
      >
        <p className="text-sm text-[var(--ui-fg-faint)]">
          Open the one-time setup link printed by the server. The account you create there becomes the owner.
        </p>
      </AuthShell>
    );
  }
  return (
    <AuthShell
      title="Create the owner account"
      subtitle="This account manages the install. Setup can only be completed once."
      footer={
        <>
          Already set up? <Link className="auth-text-link" to="/login">Sign in</Link>
        </>
      }
    >
      <SetupForm token={token} />
    </AuthShell>
  );
}

function useConsumedToken() {
  const [token] = useState(() => {
    const url = new URL(window.location.href);
    const value = url.searchParams.get("token");
    if (!value) return null;
    url.searchParams.delete("token");
    window.history.replaceState(null, "", `${url.pathname}${url.search}${url.hash}`);
    return value;
  });
  return token;
}

export function AuthShell({
  title,
  subtitle,
  footer,
  children,
}: {
  title: string;
  subtitle: string;
  footer: ReactNode;
  children: ReactNode;
}) {
  const meta = useQuery({ queryKey: ["meta"], queryFn: () => api<Meta>("/api/meta") });
  return (
    <section className="auth-shell">
      <div className="auth-shell-content">
        <div className="w-full max-w-[27rem]">
          <div className="mb-5 text-center">
            <Logo className="mx-auto mb-3 size-10" />
            <div className="text-[0.9375rem] font-semibold tracking-tight text-[var(--ui-fg)]">Owlee</div>
            <div className="text-[0.6875rem] text-[var(--ui-fg-faint)]">coolLabs</div>
          </div>
          <div className="auth-card">
            <div className="auth-card-heading">
              <h1>{title}</h1>
              <p>{subtitle}</p>
            </div>
            <div className="auth-card-body">{children}</div>
            {footer ? <footer className="auth-card-footer">{footer}</footer> : null}
          </div>
          {meta.data?.demo ? (
            <p className="mt-4 text-center text-xs text-[var(--ui-fg-faint)]">
              Seeded demo: {meta.data.demo.email} / {meta.data.demo.password}
            </p>
          ) : null}
        </div>
      </div>
    </section>
  );
}

function SetupForm({ token }: { token: string }) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [fields, setFields] = useState<Record<string, string>>({});
  const mutation = useMutation({
    mutationFn: () =>
      api<User>("/api/setup/complete", {
        method: "POST",
        body: JSON.stringify({
          token,
          email,
          password,
          display_name: displayName,
        }),
      }),
    onSuccess: () => {
      client.clear();
      navigate("/");
    },
    onError: (error) => {
      setFields(error instanceof ApiError ? (error.fields ?? {}) : {});
    },
  });

  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    mutation.mutate();
  }

  return (
    <form className="space-y-4" onSubmit={submit}>
      <p className="text-[0.8125rem] text-[var(--ui-fg-faint)]">
        This account becomes the owner. Keep the password. The setup link works once.
      </p>
      <Field label="Name" error={fields.display_name}>
        <Input value={displayName} onChange={(event) => setDisplayName(event.target.value)} placeholder="Andras Bacsai" />
      </Field>
      <Field label="Email" error={fields.email}>
        <Input type="email" required value={email} onChange={(event) => setEmail(event.target.value)} placeholder="you@coollabs.io" />
      </Field>
      <Field label="Password" hint="12+ characters" error={fields.password}>
        <Input type="password" required value={password} onChange={(event) => setPassword(event.target.value)} />
      </Field>
      <ErrorText error={mutation.error} />
      <Button className="w-full" variant="primary" type="submit" disabled={mutation.isPending}>
        {mutation.isPending ? <><Spinner />Working…</> : "Create owner account"}
      </Button>
    </form>
  );
}

function LoginForm() {
  const navigate = useNavigate();
  const location = useLocation();
  const client = useQueryClient();
  const demo = useQuery({ queryKey: ["meta"], queryFn: () => api<Meta>("/api/meta") }).data?.demo;
  // Dev servers expose the seeded demo login; it fills the form until the user types.
  const [typedEmail, setEmail] = useState<string | null>(null);
  const [typedPassword, setPassword] = useState<string | null>(null);
  const email = typedEmail ?? demo?.email ?? "";
  const password = typedPassword ?? demo?.password ?? "";
  const [fields, setFields] = useState<Record<string, string>>({});
  const from = (location.state as { from?: unknown } | null)?.from;
  const mutation = useMutation({
    mutationFn: () =>
      api<User>("/api/auth/login", { method: "POST", body: JSON.stringify({ email, password }) }),
    onSuccess: () => {
      client.clear();
      navigate(typeof from === "string" && from.startsWith("/") ? from : "/", { replace: true });
    },
    onError: (error) => {
      setFields(error instanceof ApiError ? (error.fields ?? {}) : {});
    },
  });

  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    mutation.mutate();
  }

  return (
    <form className="space-y-4" onSubmit={submit}>
      <Field label="Email" error={fields.email}>
        <Input type="email" required value={email} onChange={(event) => setEmail(event.target.value)} placeholder="you@coollabs.io" />
      </Field>
      <Field label="Password" error={fields.password}>
        <Input type="password" required value={password} onChange={(event) => setPassword(event.target.value)} />
      </Field>
      <ErrorText error={mutation.error} />
      <Button className="w-full" variant="primary" type="submit" disabled={mutation.isPending}>
        {mutation.isPending ? <><Spinner />Working…</> : "Sign in"}
      </Button>
      <p className="text-center text-[0.8125rem]">
        <Link className="auth-text-link" to="/forgot-password">Forgot password?</Link>
      </p>
    </form>
  );
}

function EmailLinkForm({
  submitLabel,
  request,
  sent,
}: {
  submitLabel: string;
  request: (email: string) => Promise<unknown>;
  sent: (email: string) => ReactNode;
}) {
  const [email, setEmail] = useState("");
  const [fields, setFields] = useState<Record<string, string>>({});
  const mutation = useMutation({
    mutationFn: () => request(email),
    onError: (error) => setFields(error instanceof ApiError ? (error.fields ?? {}) : {}),
  });

  if (mutation.isSuccess) {
    return (
      <div className="space-y-2">
        <p className="text-sm font-medium text-[var(--ui-fg)]">Check your email</p>
        <p className="text-sm text-[var(--ui-fg-faint)]">{sent(email)}</p>
      </div>
    );
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    mutation.mutate();
  }

  return (
    <form className="space-y-4" onSubmit={submit}>
      <Field label="Email" error={fields.email}>
        <Input type="email" required value={email} onChange={(event) => setEmail(event.target.value)} placeholder="you@coollabs.io" />
      </Field>
      <ErrorText error={mutation.error} />
      <Button className="w-full" variant="primary" type="submit" disabled={mutation.isPending}>
        {mutation.isPending ? <><Spinner />Sending…</> : submitLabel}
      </Button>
    </form>
  );
}

function TokenPasswordForm({
  withName = false,
  submitLabel,
  restart,
  submit: send,
}: {
  withName?: boolean;
  submitLabel: string;
  restart: { to: string; label: string };
  submit: (password: string, displayName: string) => Promise<User>;
}) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const [password, setPassword] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [fields, setFields] = useState<Record<string, string>>({});
  const mutation = useMutation({
    mutationFn: () => send(password, displayName),
    onSuccess: () => {
      client.clear();
      navigate("/", { replace: true });
    },
    onError: (error) => setFields(error instanceof ApiError ? (error.fields ?? {}) : {}),
  });

  function submit(event: FormEvent) {
    event.preventDefault();
    setFields({});
    mutation.mutate();
  }

  if (fields.token) {
    return (
      <p className="text-sm text-[var(--ui-fg-faint)]">
        This link is invalid, expired, or already used. <Link className="auth-text-link" to={restart.to}>{restart.label}</Link>
      </p>
    );
  }

  return (
    <form className="space-y-4" onSubmit={submit}>
      {withName ? (
        <Field label="Name" error={fields.display_name}>
          <Input value={displayName} onChange={(event) => setDisplayName(event.target.value)} placeholder="Andras Bacsai" />
        </Field>
      ) : null}
      <Field label="Password" hint="12+ characters" error={fields.password}>
        <Input type="password" required autoComplete="new-password" value={password} onChange={(event) => setPassword(event.target.value)} />
      </Field>
      <ErrorText error={mutation.error} />
      <Button className="w-full" variant="primary" type="submit" disabled={mutation.isPending}>
        {mutation.isPending ? <><Spinner />Working…</> : submitLabel}
      </Button>
    </form>
  );
}
