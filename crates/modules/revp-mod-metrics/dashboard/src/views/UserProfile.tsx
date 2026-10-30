import { useState } from "react";
import { AltArrowLeftIcon } from "@solar-icons/react/linear/alt-arrow-left";
import { Button } from "@/components/ui/button";
import { ANON, getJson, requestsUrl, usersUrl, type Filters, type Rec, type RequestsResp, type Sort, type UserResp } from "@/api";
import { usePoll } from "@/hooks";
import { BarList, Notice, Panel, Stat, UserAvatar, pastaButton } from "@/components/pasta/surface";
import { FilterBar } from "@/components/FilterBar";
import { RequestTable } from "@/components/RequestTable";
import { RequestDrawer } from "@/components/RequestDrawer";
import { LatencyChart, TrafficChart } from "@/components/Charts";
import { fmtAgo, fmtDateTime, fmtInt, fmtMs, fmtPct, userLabel } from "@/format";

type Props = {
  user: string;
  filters: Filters;
  live: boolean;
  onBack: () => void;
  onFilters: (patch: Partial<Filters>) => void;
  /** Leave the profile for the Requests tab with these filters. */
  onRequests: (patch: Partial<Filters>) => void;
};

const LIMIT = 50;

export function UserProfile({ user, filters, live, onBack, onFilters, onRequests }: Props) {
  const scoped: Filters = { ...filters, user };
  const [sort, setSort] = useState<Sort>({ sort: "ts", order: "desc" });
  const [offset, setOffset] = useState(0);
  const [selected, setSelected] = useState<Rec | null>(null);

  const profile = usePoll(usersUrl(scoped), (u, s) => getJson<UserResp>(u, s), live, 4000);
  const reqs = usePoll(requestsUrl(scoped, sort, offset, LIMIT), (u, s) => getJson<RequestsResp>(u, s), live);

  const row = profile.data?.user;
  const errRate = row && row.requests ? row.errors / row.requests : 0;
  const points = profile.data?.series ?? [];
  const change = (patch: Partial<Filters>) => {
    setOffset(0);
    onFilters(patch);
  };

  return (
    <div className="grid gap-5">
      <div className="flex flex-wrap items-center gap-4">
        <Button variant="ghost" size="sm" className={pastaButton.ghost} onClick={onBack}>
          <AltArrowLeftIcon /> All users
        </Button>
      </div>

      <header className="flex flex-wrap items-center gap-5 rounded-[16px] bg-card p-6 font-sans font-[450] tracking-[-0.05px] shadow-card">
        <UserAvatar user={user} size={56} />
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-[22px] leading-7 font-medium tracking-[-0.02em] text-foreground">{userLabel(user)}</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            {user === ANON
              ? "Requests that carried no login token."
              : row
                ? `First seen ${fmtDateTime(row.first_ts ?? 0)} · last seen ${fmtAgo(row.last_ts)}`
                : profile.loading
                  ? "Loading…"
                  : "No requests from this user in the selected range."}
          </p>
        </div>
      </header>

      {profile.error && <Notice tone="danger" title="Admin API unreachable">Can't reach revp's admin API: {profile.error}</Notice>}

      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <Stat label="Requests" value={row ? fmtInt(row.requests) : "—"} />
        <Stat
          label="Server errors"
          value={row ? fmtPct(errRate) : "—"}
          hint={row ? `${fmtInt(row.errors)} 5xx or failed · ${fmtInt(row.client_errors)} 4xx` : undefined}
          tone={errRate > 0.05 ? "danger" : errRate > 0 ? "warning" : undefined}
        />
        <Stat label="Median latency" value={row ? fmtMs(row.p50_ms) : "—"} hint="p50" />
        <Stat label="Slow requests" value={row ? fmtMs(row.p95_ms) : "—"} hint={row ? `p95 · p99 ${fmtMs(row.p99_ms)}` : "p95"} />
      </div>

      <div className="grid gap-4 xl:grid-cols-2">
        <Panel>
          <TrafficChart points={points} stepMs={points.length > 1 ? points[1].t - points[0].t : 1000} loading={profile.loading} />
        </Panel>
        <Panel>
          <LatencyChart points={points} loading={profile.loading} />
        </Panel>
      </div>

      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
        <Panel title="Routes">
          <BarList items={row?.routes ?? []} onPick={v => change({ route: v })} />
        </Panel>
        <Panel title="Services">
          <BarList items={row?.services ?? []} onPick={v => change({ service: v })} />
        </Panel>
        <Panel title="IP addresses">
          <BarList items={row?.clients ?? []} onPick={v => change({ q: v })} />
        </Panel>
        <Panel title="Browsers and clients">
          <BarList items={row?.user_agents ?? []} />
        </Panel>
      </div>

      <FilterBar filters={filters} facets={reqs.data?.facets} onChange={change} hide={["user"]} />

      <RequestTable
        rows={reqs.data?.items ?? []}
        total={reqs.data?.matched ?? 0}
        sort={sort}
        onSort={s => {
          setOffset(0);
          setSort(s);
        }}
        offset={offset}
        limit={LIMIT}
        onPage={setOffset}
        onSelect={setSelected}
        onFilter={change}
        hide={["user", ...((reqs.data?.facets.host.length ?? 0) <= 1 ? ["host"] : [])]}
        loading={reqs.loading}
      />

      <RequestDrawer rec={selected} onClose={() => setSelected(null)} onFilter={p => onRequests({ ...p, user })} />
    </div>
  );
}
