import { useState } from "react";
import { getJson, requestsUrl, type Filters, type Rec, type RequestsResp, type Sort } from "@/api";
import { usePoll } from "@/hooks";
import { FilterBar } from "@/components/FilterBar";
import { RequestTable } from "@/components/RequestTable";
import { RequestDrawer } from "@/components/RequestDrawer";
import { LatencyChart, StatusDonut, TrafficChart } from "@/components/Charts";
import { Notice, Panel, Stat } from "@/components/pasta/surface";
import { fmtInt, fmtMs, fmtPct } from "@/format";

type Props = {
  filters: Filters;
  sort: Sort;
  offset: number;
  limit: number;
  live: boolean;
  onFilters: (patch: Partial<Filters>) => void;
  onSort: (s: Sort) => void;
  onPage: (offset: number) => void;
  onUser: (user: string) => void;
};

export function RequestsView({ filters, sort, offset, limit, live, onFilters, onSort, onPage, onUser }: Props) {
  const [selected, setSelected] = useState<Rec | null>(null);
  const url = requestsUrl(filters, sort, offset, limit);
  const { data, error, loading } = usePoll(url, (u, s) => getJson<RequestsResp>(u, s), live);

  const s = data?.summary;
  const errRate = s && s.requests ? s.errors / s.requests : 0;
  const span = s?.first_ts && s.last_ts ? Math.max(1, (s.last_ts - s.first_ts) / 1000) : 0;

  return (
    <div className="grid gap-5">
      {error && <Notice tone="danger" title="Admin API unreachable">Can't reach revp's admin API: {error}</Notice>}
      {data && !data.recording && (
        <Notice title="Recording is off">
          Request recording is off, so there is nothing to show yet. Turn it on in revp's config and save:
          <pre className="mt-3 rounded-[10px] bg-card px-3 py-2 font-mono text-xs text-foreground shadow-card">{"[defaults.modules.metrics]\nrequests = 50000"}</pre>
        </Notice>
      )}

      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-5">
        <Stat label="Requests" value={s ? fmtInt(s.requests) : "—"} hint={data ? `${fmtInt(data.held)} held of ${fmtInt(data.capacity)}` : undefined} />
        <Stat
          label="Server errors"
          value={s ? fmtPct(errRate) : "—"}
          hint={s ? `${fmtInt(s.errors)} 5xx or failed · ${fmtInt(s.client_errors)} 4xx` : undefined}
          tone={errRate > 0.05 ? "danger" : errRate > 0 ? "warning" : undefined}
        />
        <Stat label="Median latency" value={s ? fmtMs(s.p50_ms) : "—"} hint="p50" />
        <Stat label="Slow requests" value={s ? fmtMs(s.p95_ms) : "—"} hint={s ? `p95 · p99 ${fmtMs(s.p99_ms)}` : "p95"} tone={s && s.p95_ms > 1000 ? "warning" : undefined} />
        <Stat label="Throughput" value={s && span ? `${(s.requests / span).toFixed(s.requests / span < 10 ? 2 : 0)}/s` : "—"} hint="over the matched requests" />
      </div>

      <div className="grid gap-4 lg:grid-cols-2 xl:grid-cols-[1fr_1fr_18rem]">
        <Panel>
          <TrafficChart points={data?.series.points ?? []} stepMs={data?.series.step_ms ?? 1000} loading={loading} />
        </Panel>
        <Panel>
          <LatencyChart points={data?.series.points ?? []} loading={loading} />
        </Panel>
        <Panel className="lg:col-span-2 xl:col-span-1">
          {/* A real width: the chart sizes itself to its container and collapses to 0 inside a centering grid. */}
          <div className="mx-auto w-full max-w-[18rem]">
            <StatusDonut points={data?.series.points ?? []} />
          </div>
        </Panel>
      </div>

      <FilterBar filters={filters} facets={data?.facets} onChange={onFilters} />

      <RequestTable
        rows={data?.items ?? []}
        total={data?.matched ?? 0}
        sort={sort}
        onSort={onSort}
        offset={offset}
        limit={limit}
        onPage={onPage}
        onSelect={setSelected}
        onFilter={onFilters}
        onUser={onUser}
        hide={(data?.facets.host.length ?? 0) <= 1 ? ["host"] : []}
        loading={loading}
      />

      <RequestDrawer rec={selected} onClose={() => setSelected(null)} onUser={onUser} onFilter={onFilters} />
    </div>
  );
}
