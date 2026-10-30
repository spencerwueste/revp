// Types and URL building for revp's dashboard API (crates/modules/revp-mod-metrics/src/requests.rs).

export type Rec = {
  id: number;
  ts: number;
  method: string;
  host: string | null;
  path: string;
  query: string | null;
  status: number;
  duration_ms: number;
  bytes_in: number;
  bytes_out: number;
  route: string;
  service: string;
  upstream: string | null;
  client: string | null;
  user: string | null;
  kind: string;
  by: string | null;
  error: string | null;
  user_agent: string | null;
  referer: string | null;
  tls: boolean;
  proto: string;
  cache: string | null;
};

/** [value, count], most frequent first. */
export type Facet = [string, number][];

export type Summary = {
  requests: number;
  errors: number;
  client_errors: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
  bytes_out: number;
  first_ts: number | null;
  last_ts: number | null;
};

export type Point = {
  t: number;
  "2xx": number;
  "3xx": number;
  "4xx": number;
  "5xx": number;
  other: number;
  p50_ms: number;
  p95_ms: number;
};

export type FacetKey = "method" | "status" | "route" | "service" | "kind" | "host" | "user";

export type RequestsResp = {
  recording: boolean;
  capacity: number;
  held: number;
  now: number;
  matched: number;
  summary: Summary;
  series: { step_ms: number; points: Point[] };
  facets: Record<FacetKey, Facet>;
  items: Rec[];
};

export type UserRow = Summary & {
  user: string;
  routes: Facet;
  services: Facet;
  clients: Facet;
  user_agents: Facet;
};

export type UsersResp = { users: UserRow[] };
export type UserResp = { user: UserRow | null; series: Point[] };

/** `-` is how the API names requests with no user. */
export const ANON = "-";

export type Filters = {
  q?: string;
  kind?: string;
  route?: string;
  service?: string;
  method?: string;
  user?: string;
  status?: string;
  host?: string;
  range?: string;
  errors?: string;
};

export const FILTER_KEYS: (keyof Filters)[] = ["q", "kind", "route", "service", "method", "user", "status", "host", "range", "errors"];

export const RANGES: { value: string; label: string; ms: number }[] = [
  { value: "5m", label: "Last 5 minutes", ms: 5 * 60_000 },
  { value: "15m", label: "Last 15 minutes", ms: 15 * 60_000 },
  { value: "1h", label: "Last hour", ms: 60 * 60_000 },
  { value: "24h", label: "Last 24 hours", ms: 24 * 60 * 60_000 },
  { value: "all", label: "Everything held", ms: 0 },
];

export type Sort = { sort: string; order: "asc" | "desc" };

const BASE = "/dashboard/api";

function params(f: Filters, extra: Record<string, string | number | undefined> = {}): string {
  const p = new URLSearchParams();
  for (const k of FILTER_KEYS) {
    const v = f[k];
    if (!v || k === "range") continue;
    p.set(k, v);
  }
  const range = RANGES.find(r => r.value === (f.range ?? "1h"));
  if (range && range.ms > 0) p.set("since", String(Date.now() - range.ms));
  for (const [k, v] of Object.entries(extra)) if (v !== undefined && v !== "") p.set(k, String(v));
  return p.toString();
}

export const requestsUrl = (f: Filters, s: Sort, offset: number, limit: number) =>
  `${BASE}/requests?${params(f, { sort: s.sort, order: s.order, offset, limit })}`;

export const usersUrl = (f: Filters) => `${BASE}/users?${params(f)}`;

export async function getJson<T>(url: string, signal?: AbortSignal): Promise<T> {
  const r = await fetch(url, { signal, headers: { accept: "application/json" } });
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
  return (await r.json()) as T;
}
