import { useEffect, useState } from "react";
import { CloseCircleIcon } from "@solar-icons/react/linear/close-circle";
import { InputField, SelectField } from "@/components/pasta/field";
import { Button } from "@/components/ui/button";
import { Toggle } from "@/components/ui/toggle";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { ANON, RANGES, type Facet, type FacetKey, type Filters } from "@/api";
import { useDebounced } from "@/hooks";
import { fmtInt, userLabel } from "@/format";

// A select item needs a non-empty value, so "All" is a sentinel.
const ALL = "__all";

const STATUS_CLASSES = ["2xx", "3xx", "4xx", "5xx"];

const chip = "h-7 rounded-full px-3 text-[13px] font-[450] text-foreground/70 hover:bg-foreground/6 aria-pressed:bg-foreground aria-pressed:text-background data-[pressed]:bg-foreground data-[pressed]:text-background";

type Props = {
  filters: Filters;
  facets: Record<FacetKey, Facet> | undefined;
  onChange: (patch: Partial<Filters>) => void;
  /** Fields not to offer, e.g. `user` on a user's own profile. */
  hide?: (keyof Filters)[];
};

function options(facet: Facet | undefined, current: string | undefined, label = (v: string) => v) {
  const opts = [{ value: ALL, label: "All" }];
  for (const [v, n] of facet ?? []) if (v) opts.push({ value: v, label: `${label(v)} · ${fmtInt(n)}` });
  // Keep a value chosen from a link or the URL selectable even if it has aged out.
  if (current && !opts.some(o => o.value === current)) opts.push({ value: current, label: label(current) });
  return opts;
}

export function FilterBar({ filters, facets, onChange, hide = [] }: Props) {
  const [q, setQ] = useState(filters.q ?? "");
  const debounced = useDebounced(q, 250);
  useEffect(() => {
    if ((filters.q ?? "") !== debounced) onChange({ q: debounced || undefined });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [debounced]);
  useEffect(() => setQ(filters.q ?? ""), [filters.q]);

  const select = (key: FacetKey, label: string, fmt?: (v: string) => string) =>
    hide.includes(key) ? null : (
      <SelectField
        key={key}
        label={label}
        value={filters[key] ?? ALL}
        options={options(facets?.[key], filters[key], fmt)}
        onValueChange={v => onChange({ [key]: v === ALL ? undefined : v })}
      />
    );

  const active = (["q", "kind", "route", "service", "method", "user", "status", "host", "errors"] as (keyof Filters)[]).filter(
    k => !hide.includes(k) && filters[k],
  );

  return (
    <section className="grid gap-4 rounded-[16px] bg-card p-5 font-sans text-sm font-[450] tracking-[-0.05px] text-muted-foreground shadow-card">
      <div className="grid gap-4 md:grid-cols-[minmax(16rem,2fr)_minmax(12rem,1fr)]">
        <InputField label="Search" placeholder="Path, user, IP, error, user agent…" value={q} onChange={e => setQ(e.target.value)} type="search" />
        <SelectField label="Time range" value={filters.range ?? "1h"} options={RANGES.map(r => ({ value: r.value, label: r.label }))} onValueChange={v => onChange({ range: v })} />
      </div>
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-5">
        {select("kind", "Type")}
        {select("route", "Route")}
        {select("service", "Service")}
        {select("method", "Method")}
        {select("user", "User", v => (v === ANON ? "Anonymous" : userLabel(v)))}
      </div>
      <div className="flex flex-wrap items-center gap-2 border-t-[0.5px] border-foreground/[0.09] pt-4">
        <span className="mr-1 text-sm font-medium text-muted-foreground">Status</span>
        <ToggleGroup
          aria-label="Status class"
          spacing={1}
          value={[filters.status ?? ALL]}
          onValueChange={v => {
            const next = (v as string[])[0];
            onChange({ status: !next || next === ALL ? undefined : next });
          }}
        >
          {[ALL, ...STATUS_CLASSES].map(s => (
            <ToggleGroupItem key={s} value={s} size="sm" className={chip}>
              {s === ALL ? "All" : s}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
        {filters.status && !STATUS_CLASSES.includes(filters.status) && (
          <span className="inline-flex h-7 items-center rounded-full bg-foreground px-3 text-[13px] text-background tabular-nums">{filters.status}</span>
        )}
        <Toggle
          size="sm"
          pressed={filters.errors === "1"}
          onPressedChange={p => onChange({ errors: p ? "1" : undefined })}
          className="h-7 rounded-full px-3 text-[13px] font-[450] text-foreground/70 hover:bg-foreground/6 aria-pressed:bg-destructive aria-pressed:text-white data-[pressed]:bg-destructive data-[pressed]:text-white"
        >
          Errors only
        </Toggle>
        {filters.host && !hide.includes("host") && (
          <Button size="sm" variant="ghost" className="h-7 rounded-full bg-foreground/6 px-3 text-[13px] font-[450]" onClick={() => onChange({ host: undefined })} aria-label={`Remove host filter ${filters.host}`}>
            Host: {filters.host}
            <CloseCircleIcon />
          </Button>
        )}
        <span className="flex-1" />
        {active.length > 0 && (
          <Button size="sm" variant="ghost" className="h-7 rounded-md px-3 text-[13px] font-[450]" onClick={() => onChange(Object.fromEntries(active.map(k => [k, undefined])) as Partial<Filters>)}>
            Clear filters
          </Button>
        )}
      </div>
    </section>
  );
}
