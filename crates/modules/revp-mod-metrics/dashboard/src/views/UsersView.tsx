import { useMemo, useState } from "react";
import { DataTable, type DataTableColumn } from "@/components/ai/data-table";
import { InputField, SelectField } from "@/components/pasta/field";
import { Notice, UserAvatar } from "@/components/pasta/surface";
import { ANON, RANGES, getJson, usersUrl, type Filters, type UsersResp } from "@/api";
import { usePoll } from "@/hooks";
import { fmtAgo, fmtInt, fmtMs, fmtPct, userLabel } from "@/format";
import { cn } from "@/lib/utils";

type Props = { range: string | undefined; live: boolean; onRange: (r: string) => void; onOpen: (user: string) => void };

/** One flat, sortable row per user; DataTable sorts on these fields. */
type Row = { user: string; name: string; requests: number; errors: number; client: number; p95: number; route: string; last: number };

const COLUMNS: DataTableColumn<Row>[] = [
  {
    key: "name",
    header: "User",
    width: 260,
    pin: "left",
    cell: r => (
      <span className="inline-flex max-w-full items-center gap-2.5">
        <UserAvatar user={r.user} size={24} />
        <span className={cn("truncate", r.user === ANON ? "text-muted-foreground" : "text-foreground")}>{r.name}</span>
      </span>
    ),
  },
  { key: "requests", header: "Requests", width: 112, align: "end", cell: r => <span className="text-foreground">{fmtInt(r.requests)}</span> },
  {
    key: "errors",
    header: "Server errors",
    width: 128,
    align: "end",
    cell: r => <span className={cn(r.errors > 0.05 ? "text-destructive" : r.errors > 0 ? "text-warning" : undefined)}>{fmtPct(r.errors)}</span>,
  },
  { key: "client", header: "4xx", width: 88, align: "end", cell: r => fmtInt(r.client) },
  { key: "p95", header: "p95", width: 104, align: "end", cell: r => <span className="text-foreground">{fmtMs(r.p95)}</span> },
  { key: "route", header: "Top route", width: 200, cell: r => r.route || "—" },
  { key: "last", header: "Last seen", width: 120, align: "end", cell: r => fmtAgo(r.last || null) },
];

export function UsersView({ range, live, onRange, onOpen }: Props) {
  const filters: Filters = { range };
  const { data, error, loading } = usePoll(usersUrl(filters), (u, s) => getJson<UsersResp>(u, s), live, 4000);
  const [search, setSearch] = useState("");

  const rows = useMemo<Row[]>(() => {
    const q = search.trim().toLowerCase();
    return (data?.users ?? [])
      .filter(u => !q || userLabel(u.user).toLowerCase().includes(q))
      .map(u => ({
        user: u.user,
        name: userLabel(u.user),
        requests: u.requests,
        errors: u.requests ? u.errors / u.requests : 0,
        client: u.client_errors,
        p95: u.p95_ms,
        route: u.routes[0]?.[0] ?? "",
        last: u.last_ts ?? 0,
      }));
  }, [data, search]);

  return (
    <div className="grid gap-5">
      {error && (
        <Notice tone="danger" title="Admin API unreachable">
          Can't reach revp's admin API: {error}
        </Notice>
      )}
      <section className="grid gap-4 rounded-[16px] bg-card p-5 shadow-card md:grid-cols-[2fr_1fr]">
        <InputField label="Find a user" placeholder="Email or name" type="search" value={search} onChange={e => setSearch(e.target.value)} />
        <SelectField label="Time range" value={range ?? "1h"} options={RANGES.map(r => ({ value: r.value, label: r.label }))} onValueChange={onRange} />
      </section>
      <p className="px-1 metadata-label">
        Users are read from the login token each request carries (its email, name or sub claim). revp does not verify the token; your backend does.
      </p>

      <DataTable
        title="Users"
        className="max-h-[min(72vh,760px)]"
        columns={COLUMNS}
        rows={rows}
        defaultSort={{ key: "last", direction: "desc" }}
        getRowId={r => r.user}
        onRowClick={r => onOpen(r.user)}
        empty={loading && !data ? "Loading…" : search ? "No user matches that search." : "No requests in this range."}
      />
    </div>
  );
}
