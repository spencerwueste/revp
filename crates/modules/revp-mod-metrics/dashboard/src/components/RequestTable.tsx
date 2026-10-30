// The request grid, on Pasta UI's DataTable. Sorting and paging happen in revp
// (the ring can hold far more rows than a browser should), so the table runs
// with `manualSorting` and only reports what the user asked for. Column keys
// are record fields; SORT_ID maps the two that differ from the API's sort keys.

import { useMemo, type ReactNode } from "react";
import { DataTable, type DataTableColumn, type DataTableSort } from "@/components/ai/data-table";
import { Button } from "@/components/ui/button";
import { MethodTag, ToneBadge, UserAvatar, pastaButton } from "@/components/pasta/surface";
import type { Filters, Rec, Sort } from "@/api";
import { fmtBytes, fmtClock, fmtInt, fmtMs, statusTone, userLabel } from "@/format";
import { cn } from "@/lib/utils";

type Props = {
  rows: Rec[];
  total: number;
  sort: Sort;
  onSort: (s: Sort) => void;
  offset: number;
  limit: number;
  onPage: (offset: number) => void;
  onSelect: (r: Rec) => void;
  onFilter?: (patch: Partial<Filters>) => void;
  onUser?: (user: string) => void;
  hide?: string[];
  loading?: boolean;
};

const SORT_ID: Record<string, string> = { duration_ms: "duration", bytes_out: "bytes" };
const COLUMN_KEY: Record<string, string> = Object.fromEntries(Object.entries(SORT_ID).map(([k, v]) => [v, k]));
const DEFAULT_SORT: Sort = { sort: "ts", order: "desc" };

/** Click a value to filter on it. */
function Pick({ children, onClick, title }: { children: ReactNode; onClick?: () => void; title: string }) {
  if (!onClick) return <>{children}</>;
  return (
    <button
      type="button"
      title={title}
      className="max-w-full cursor-pointer truncate rounded-md text-left outline-none hover:underline hover:decoration-foreground/30 hover:underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring/40"
      onClick={e => {
        e.stopPropagation();
        onClick();
      }}
    >
      {children}
    </button>
  );
}

export function RequestTable({ rows, total, sort, onSort, offset, limit, onPage, onSelect, onFilter, onUser, hide = [], loading }: Props) {
  const columns = useMemo<DataTableColumn<Rec>[]>(() => {
    const all: DataTableColumn<Rec>[] = [
      { key: "ts", header: "Time", width: 104, cell: r => <span className="text-foreground/75">{fmtClock(r.ts)}</span> },
      { key: "method", header: "Method", width: 92, cell: r => <MethodTag method={r.method} /> },
      {
        key: "status",
        header: "Status",
        width: 92,
        cell: r => (
          <Pick title="Filter by this status" onClick={onFilter && (() => onFilter({ status: String(r.status) }))}>
            <ToneBadge tone={statusTone(r.status)}>{r.status || "—"}</ToneBadge>
          </Pick>
        ),
      },
      {
        key: "path",
        header: "Path",
        width: 320,
        cell: r => (
          <span className="font-mono text-xs text-foreground" title={r.path + (r.query ? `?${r.query}` : "")}>
            {r.path}
            {r.query && <span className="text-muted-foreground">?{r.query}</span>}
          </span>
        ),
      },
      {
        key: "host",
        header: "Host",
        width: 180,
        sortable: false,
        cell: r => (
          <Pick title="Filter by this host" onClick={onFilter && r.host ? () => onFilter({ host: r.host! }) : undefined}>
            {r.host ?? "—"}
          </Pick>
        ),
      },
      {
        key: "route",
        header: "Route",
        width: 150,
        cell: r => (
          <Pick title="Filter by this route" onClick={onFilter && (() => onFilter({ route: r.route }))}>
            <span className="text-foreground/85">{r.route}</span>
          </Pick>
        ),
      },
      {
        key: "service",
        header: "Service",
        width: 150,
        cell: r => (
          <Pick title="Filter by this service" onClick={onFilter && (() => onFilter({ service: r.service }))}>
            {r.service}
          </Pick>
        ),
      },
      {
        key: "kind",
        header: "Type",
        width: 110,
        sortable: false,
        cell: r => (
          <Pick title="Filter by this type" onClick={onFilter && (() => onFilter({ kind: r.kind }))}>
            <ToneBadge tone="neutral">{r.kind}</ToneBadge>
          </Pick>
        ),
      },
      {
        key: "user",
        header: "User",
        width: 210,
        cell: r => {
          const u = r.user;
          if (!u) return <span className="text-muted-foreground/70">—</span>;
          return (
            <Pick title="Open this user's profile" onClick={onUser && (() => onUser(u))}>
              <span className="inline-flex max-w-full items-center gap-2 text-foreground/85">
                <UserAvatar user={u} size={20} />
                <span className="truncate">{userLabel(u)}</span>
              </span>
            </Pick>
          );
        },
      },
      {
        key: "duration_ms",
        header: "Duration",
        width: 108,
        align: "end",
        cell: r => (
          <span className={cn(r.duration_ms >= 1000 ? "text-destructive" : r.duration_ms >= 300 ? "text-warning" : "text-foreground")}>{fmtMs(r.duration_ms)}</span>
        ),
      },
      { key: "bytes_out", header: "Size", width: 96, align: "end", cell: r => fmtBytes(r.bytes_out) },
    ];
    return all.filter(c => !hide.includes(c.key));
  }, [onFilter, onUser, hide.join()]);

  const tableSort: DataTableSort = { key: COLUMN_KEY[sort.sort] ?? sort.sort, direction: sort.order };
  const setSort = (s: DataTableSort | null) => onSort(s ? { sort: SORT_ID[s.key] ?? s.key, order: s.direction } : DEFAULT_SORT);

  const from = total === 0 ? 0 : offset + 1;
  const to = Math.min(offset + limit, total);

  return (
    <DataTable
      title="Requests"
      className={cn("max-h-[min(72vh,760px)] transition-opacity", loading && rows.length === 0 && "opacity-70")}
      columns={columns}
      rows={rows}
      sort={tableSort}
      onSortChange={setSort}
      manualSorting
      getRowId={r => String(r.id)}
      getRowClassName={r => (r.status >= 500 || !!r.error) && "[&>td]:bg-[color-mix(in_oklch,var(--card),var(--destructive)_5%)]"}
      onRowClick={onSelect}
      empty={loading ? "Loading…" : "No requests match these filters."}
      footer={
        <>
          <span>
            {fmtInt(from)}–{fmtInt(to)} of {fmtInt(total)}
          </span>
          <span className="flex gap-2">
            <Button size="sm" variant="outline" className={pastaButton.outline} disabled={offset === 0} onClick={() => onPage(Math.max(0, offset - limit))}>
              Previous
            </Button>
            <Button size="sm" variant="outline" className={pastaButton.outline} disabled={to >= total} onClick={() => onPage(offset + limit)}>
              Next
            </Button>
          </span>
        </>
      }
    />
  );
}
