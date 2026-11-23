import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Link, useParams } from "react-router";
import { BarList, DailyBars, Stat } from "@/components/charts";
import { type Column, DataTable } from "@/components/data-table";
import { Button, Dialog, ErrorText, Field, Input, LayerCard, PageHeader, Select, Skeleton, Spinner, StatusBadge } from "@/components/ui";
import { ProjectField, ProjectTag } from "@/components/project";
import { CodeBlock, ZoneDownload } from "@/components/setup-guide";
import { api, type InsightDomain, type InsightLink, type InsightRow, type LinkOverview, type Meta } from "@/lib/api";
import { type DnsRecord, pointRecord } from "@/lib/dns";
import { useCanEdit } from "@/lib/me";
import { useListScope, useProjectChoice } from "@/lib/workspace";

function VerifyDomain({ domain, onDone }: { domain: InsightDomain; onDone: () => void }) {
  const client = useQueryClient();
  const verify = useMutation({
    mutationFn: () => api<InsightDomain>(`/api/domains/${domain.id}/verify`, { method: "POST" }),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["domains"] });
      onDone();
    },
  });
  const origin = useQuery({ queryKey: ["meta"], queryFn: () => api<Meta>("/api/meta") }).data?.public_origin;
  const point = origin ? pointRecord(domain.hostname, origin) : undefined;
  const records: DnsRecord[] = [{ name: domain.verify_name, type: "TXT", value: domain.verify_value }, ...(point ? [point] : [])];
  return (
    <div className="space-y-4">
      <p className="text-[0.8125rem] text-[var(--ui-fg-dim)]">
        Add these records at your DNS host, then verify. The TXT record proves that you own the hostname. The {point?.type ?? "CNAME"} record sends its visitors to Owlee.
        {domain.redirect_open ? "" : " Links on this hostname do not redirect until it verifies."}
      </p>
      <CodeBlock file="TXT name" code={domain.verify_name} />
      <CodeBlock file="TXT value" code={domain.verify_value} />
      {point ? <CodeBlock file={`${point.type} ${point.name}`} code={point.value} /> : null}
      <ZoneDownload hostname={domain.hostname} records={records} />
      <ErrorText error={verify.error} />
      <Button variant="primary" type="button" disabled={verify.isPending} onClick={() => verify.mutate()}>
        {verify.isPending ? <><Spinner />Verifying…</> : "Verify"}
      </Button>
    </div>
  );
}

function domainStatus(domain: InsightDomain) {
  if (!domain.enabled) return { label: "Off", tone: "neutral" } as const;
  if (!domain.verified) return { label: "Unverified", tone: "warning" } as const;
  return { label: "Enabled", tone: "success" } as const;
}

export function LinksPage() {
  const client = useQueryClient();
  const canEdit = useCanEdit();
  const domains = useQuery({ queryKey: ["domains"], queryFn: () => api<InsightDomain[]>("/api/domains") });
  const scope = useListScope();
  const links = useQuery({ queryKey: scope.key("links"), queryFn: () => api<InsightLink[]>(scope.path("/api/links")) });
  const [projectId, setProjectId] = useProjectChoice();
  const [openDomain, setOpenDomain] = useState(false);
  const [openLink, setOpenLink] = useState(false);
  const [hostname, setHostname] = useState("");
  const [verifyId, setVerifyId] = useState("");
  const [slug, setSlug] = useState("");
  const [destination, setDestination] = useState("https://");
  const [domainId, setDomainId] = useState("");
  const createDomain = useMutation({
    mutationFn: () => api<InsightDomain>("/api/domains", { method: "POST", body: JSON.stringify({ hostname, enabled: true }) }),
    onSuccess: async () => {
      setOpenDomain(false);
      setHostname("");
      await client.invalidateQueries({ queryKey: ["domains"] });
    },
  });
  const createLink = useMutation({
    mutationFn: () =>
      api<InsightLink>("/api/links", {
        method: "POST",
        body: JSON.stringify({ domain_id: domainId || domains.data?.[0]?.id, slug, destination, redirect_type: "temporary", enabled: true, project_id: projectId }),
      }),
    onSuccess: async () => {
      setOpenLink(false);
      setSlug("");
      await client.invalidateQueries({ queryKey: ["links"] });
    },
  });
  const domainRows = domains.data ?? [];
  const linkRows = links.data ?? [];
  const verifying = domains.data?.find((domain) => domain.id === verifyId);
  const domainColumns: Column<InsightDomain>[] = [
    { key: "hostname", header: "Hostname", sort: (domain) => domain.hostname, cellClassName: "font-medium text-[var(--ui-fg)]", cell: (domain) => domain.hostname },
    {
      key: "status",
      header: "Status",
      className: "text-right!",
      sort: (domain) => domainStatus(domain).label,
      cell: (domain) => (
        <span className="inline-flex items-center gap-2">
          {domain.verified || !canEdit ? null : <Button size="sm" onClick={() => setVerifyId(domain.id)}>Verify</Button>}
          <StatusBadge tone={domainStatus(domain).tone}>{domainStatus(domain).label}</StatusBadge>
        </span>
      ),
    },
  ];
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Links"
        description="Short links on your own hostnames, plus /go/{slug} on this Owlee server. Clicks are counted the same way as page views: no cookies."
      />
      <LayerCard
        title="Short links"
        description="Open a link to see its clicks and referrers."
        actions={canEdit ? <Button variant="primary" onClick={() => setOpenLink(true)} disabled={domainRows.length === 0}>New link</Button> : null}
        flush
      >
        {!links.isLoading && linkRows.length === 0 ? (
          <CardEmpty title="No short links" body="Create a link. Visitors who open /go/your-slug are redirected, and the click is stored." />
        ) : (
          <DataTable columns={linkColumns} rows={linkRows} rowKey={(link) => link.id} rowLink={(link) => `/links/${link.id}`} loading={links.isLoading} />
        )}
      </LayerCard>
      <LayerCard
        title="Domains"
        description="A hostname redirects after its DNS TXT record verifies. /go/{slug} on this server works without it."
        actions={canEdit ? <Button onClick={() => setOpenDomain(true)}>New domain</Button> : null}
        flush
      >
        {!domains.isLoading && domainRows.length === 0 ? <CardEmpty title="No domains" body="Add a domain before you create a link." /> : (
          <DataTable columns={domainColumns} rows={domainRows} rowKey={(domain) => domain.id} loading={domains.isLoading} />
        )}
      </LayerCard>
      <Dialog open={canEdit && openDomain} title="New domain" onClose={() => setOpenDomain(false)}>
        <form className="space-y-4" onSubmit={(event: FormEvent) => { event.preventDefault(); createDomain.mutate(); }}>
          <Field label="Hostname"><Input required value={hostname} onChange={(event) => setHostname(event.target.value)} placeholder="go.example.com" /></Field>
          <ErrorText error={createDomain.error} />
          <Button variant="primary" type="submit" disabled={createDomain.isPending}>{createDomain.isPending ? <><Spinner />Saving…</> : "Save domain"}</Button>
        </form>
      </Dialog>
      <Dialog open={canEdit && verifying !== undefined} title={`Verify ${verifying?.hostname ?? "domain"}`} onClose={() => setVerifyId("")}>
        {verifying ? <VerifyDomain domain={verifying} onDone={() => setVerifyId("")} /> : null}
      </Dialog>
      <Dialog open={canEdit && openLink} title="New link" onClose={() => setOpenLink(false)}>
        <form className="space-y-4" onSubmit={(event: FormEvent) => { event.preventDefault(); createLink.mutate(); }}>
          <Field label="Domain">
            <Select
              aria-label="Domain"
              className="w-full"
              value={domainId || domains.data?.[0]?.id || ""}
              options={(domains.data ?? []).map((domain) => ({ value: domain.id, label: domain.hostname }))}
              onChange={(value) => setDomainId(value)}
            />
          </Field>
          <ProjectField value={projectId} onChange={setProjectId} />
          <Field label="Slug"><Input required value={slug} onChange={(event) => setSlug(event.target.value)} placeholder="coolify" /></Field>
          <Field label="Destination"><Input required value={destination} onChange={(event) => setDestination(event.target.value)} /></Field>
          <ErrorText error={createLink.error} />
          <Button variant="primary" type="submit" disabled={createLink.isPending}>{createLink.isPending ? <><Spinner />Creating…</> : "Create link"}</Button>
        </form>
      </Dialog>
    </div>
  );
}

export function LinkDetailPage() {
  const { linkId } = useParams();
  const to = Math.floor(Date.now() / 1000);
  const query = `from=${to - 30 * 86_400}&to=${to}`;
  const link = useQuery({
    queryKey: ["link", linkId],
    queryFn: () => api<InsightLink>(`/api/links/${linkId}`),
    enabled: Boolean(linkId),
  });
  const overview = useQuery({
    queryKey: ["link-overview", linkId, query],
    queryFn: () => api<LinkOverview>(`/api/links/${linkId}/overview?${query}`),
    enabled: Boolean(linkId),
  });
  const referrers = useQuery({
    queryKey: ["link-referrers", linkId, query],
    queryFn: () => api<{ rows: InsightRow[] }>(`/api/links/${linkId}/breakdown/referrer?${query}`),
    enabled: Boolean(linkId),
  });
  const item = link.data;
  if (link.isLoading) {
    return (
      <div className="flex flex-col gap-6" aria-busy="true">
        <div className="space-y-2">
          <Skeleton className="h-6 w-40" />
          <Skeleton className="h-3 w-80" />
        </div>
        <LayerCard><Skeleton className="h-16 w-full" /></LayerCard>
        <LayerCard><Skeleton className="h-40 w-full" /></LayerCard>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-6">
      <PageHeader keepTitle title={item ? `/${item.slug}` : "Link"} description={item ? `${item.destination} · ${window.location.origin}/go/${item.slug}` : undefined} />
      <ErrorText error={link.error ?? overview.error} />
      <LayerCard title="Summary" description="Last 30 days.">
        <div className="grid gap-6 sm:grid-cols-2">
          <Stat label="Clicks" value={overview.data?.clicks ?? item?.total_clicks} />
          <Stat label="Visitors" value={overview.data?.visitors} />
        </div>
      </LayerCard>
      <LayerCard title="Clicks" description="Clicks per day over the last 30 days.">
        <DailyBars series={overview.data?.series ?? []} field="clicks" />
      </LayerCard>
      <LayerCard title="Referrers" flush>
        <BarList rows={referrers.data?.rows ?? []} empty="No referrers in the last 30 days." />
      </LayerCard>
    </div>
  );
}

const linkColumns: Column<InsightLink>[] = [
  {
    key: "slug",
    header: "Slug",
    sort: (link) => link.slug,
    cell: (link) => (
      <>
        <Link className="row-link" to={`/links/${link.id}`}>/{link.slug}</Link>
        <ProjectTag id={link.project_id} />
        <div className="font-mono text-[0.6875rem] text-[var(--ui-fg-faint)]">/go/{link.slug}</div>
      </>
    ),
  },
  { key: "destination", header: "Destination", className: "hidden md:table-cell", sort: (link) => link.destination, cell: (link) => <div className="max-w-[24rem] truncate">{link.destination}</div> },
  { key: "domain", header: "Domain", className: "hidden sm:table-cell", sort: (link) => link.hostname, cell: (link) => link.hostname },
  { key: "clicks", header: "Clicks", className: "text-right!", cellClassName: "font-mono text-[0.75rem]", sort: (link) => link.total_clicks, cell: (link) => link.total_clicks },
];

function CardEmpty({ title, body }: { title: string; body: string }) {
  return (
    <div className="px-4 py-10 text-center">
      <p className="text-[0.875rem] font-medium text-[var(--ui-fg)]">{title}</p>
      <p className="mx-auto mt-1 max-w-[28rem] text-[0.8125rem] text-[var(--ui-fg-faint)]">{body}</p>
    </div>
  );
}
