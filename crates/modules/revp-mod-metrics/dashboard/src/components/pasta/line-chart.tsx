// A time-series line chart in Pasta UI's visual language (Pasta ships no
// chart): hairline gridlines at foreground/9%, 450-weight muted labels, a
// card-shadowed tooltip like its menus, and legend toggles in the dot-and-label
// style of Context Breakdown. Missing values leave a gap rather than a zero.

import { useEffect, useId, useMemo, useRef, useState, type PointerEvent } from "react";
import { cn } from "@/lib/utils";

export interface LineChartSeries {
  key: string;
  label: string;
  /** Any CSS color. */
  color: string;
  dashed?: boolean;
  /** Tint the area under the line. */
  area?: boolean;
}

export interface LineChartDatum {
  key: string;
  /** Full label for the tooltip. */
  label: string;
  /** Short x-axis label. */
  axisLabel?: string;
  values: Record<string, number | undefined>;
}

export interface LineChartProps {
  data: LineChartDatum[];
  series: LineChartSeries[];
  /** Chart title; also names the chart for assistive technology. */
  label: string;
  height?: number;
  formatValue?: (v: number) => string;
  formatTick?: (v: number) => string;
  loading?: boolean;
  emptyLabel?: string;
  legend?: boolean;
  className?: string;
}

const PAD = { top: 10, right: 8, bottom: 24, left: 56 };
const compact = new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 });

/** Rounds the top of the value axis up to a 1/2/2.5/5 step. */
function niceMax(v: number) {
  if (v <= 0) return 1;
  const exp = Math.pow(10, Math.floor(Math.log10(v)));
  for (const m of [1, 2, 2.5, 5, 10]) if (v <= m * exp) return m * exp;
  return 10 * exp;
}

/** Monotone cubic path (Fritsch–Carlson), so curves never swing past the data. */
function smooth(pts: [number, number][]) {
  if (pts.length < 2) return pts.length ? `M${pts[0][0]},${pts[0][1]}` : "";
  const n = pts.length;
  const d: number[] = [];
  const m: number[] = [];
  for (let i = 0; i < n - 1; i++) d.push((pts[i + 1][1] - pts[i][1]) / (pts[i + 1][0] - pts[i][0] || 1));
  m[0] = d[0];
  m[n - 1] = d[n - 2];
  for (let i = 1; i < n - 1; i++) m[i] = d[i - 1] * d[i] <= 0 ? 0 : (d[i - 1] + d[i]) / 2;
  for (let i = 0; i < n - 1; i++) {
    if (d[i] === 0) {
      m[i] = m[i + 1] = 0;
      continue;
    }
    const a = m[i] / d[i];
    const b = m[i + 1] / d[i];
    const s = a * a + b * b;
    if (s > 9) {
      const t = 3 / Math.sqrt(s);
      m[i] = t * a * d[i];
      m[i + 1] = t * b * d[i];
    }
  }
  let path = `M${pts[0][0]},${pts[0][1]}`;
  for (let i = 0; i < n - 1; i++) {
    const [x0, y0] = pts[i];
    const [x1, y1] = pts[i + 1];
    const h = (x1 - x0) / 3;
    path += `C${x0 + h},${y0 + m[i] * h} ${x1 - h},${y1 - m[i + 1] * h} ${x1},${y1}`;
  }
  return path;
}

function useWidth<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    setWidth(el.clientWidth);
    return () => ro.disconnect();
  }, []);
  return [ref, width] as const;
}

export function LineChart({ data, series, label, height = 200, formatValue = v => compact.format(v), formatTick = v => compact.format(v), loading, emptyLabel = "No data", legend, className }: LineChartProps) {
  const id = useId();
  const [ref, width] = useWidth<HTMLDivElement>();
  const [hidden, setHidden] = useState<string[]>([]);
  const [active, setActive] = useState<number | null>(null);
  const shown = series.filter(s => !hidden.includes(s.key));

  const plotW = Math.max(0, width - PAD.left - PAD.right);
  const plotH = height - PAD.top - PAD.bottom;
  const max = useMemo(() => niceMax(Math.max(0, ...data.flatMap(d => shown.map(s => d.values[s.key] ?? 0)))), [data, shown]);
  const x = (i: number) => PAD.left + (data.length <= 1 ? plotW / 2 : (i / (data.length - 1)) * plotW);
  const y = (v: number) => PAD.top + plotH - (v / max) * plotH;
  const ticks = [0, 0.25, 0.5, 0.75, 1].map(f => f * max);

  // Every value present is drawn; a run breaks wherever a value is missing.
  const runs = (key: string) => {
    const out: [number, number][][] = [];
    let cur: [number, number][] = [];
    data.forEach((d, i) => {
      const v = d.values[key];
      if (v == null || !Number.isFinite(v)) {
        if (cur.length) out.push(cur);
        cur = [];
      } else cur.push([x(i), y(v)]);
    });
    if (cur.length) out.push(cur);
    return out;
  };

  const labelEvery = Math.max(1, Math.ceil(data.length / Math.max(1, Math.floor(plotW / 64))));

  const onMove = (e: PointerEvent<SVGSVGElement>) => {
    if (!data.length || plotW <= 0) return;
    const r = e.currentTarget.getBoundingClientRect();
    const px = e.clientX - r.left - PAD.left;
    setActive(Math.max(0, Math.min(data.length - 1, Math.round((px / plotW) * (data.length - 1)))));
  };

  const at = active != null ? data[active] : null;
  const tipLeft = active != null ? x(active) : 0;

  return (
    <figure className={cn("font-sans text-sm font-[450] tracking-[-0.05px] text-muted-foreground", className)} aria-label={label}>
      <figcaption className="mb-3 flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
        <span className="text-sm leading-5 font-medium text-muted-foreground">{label}</span>
        {legend && (
          <span className="-mx-1 flex flex-wrap gap-0.5">
            {series.map(s => {
              const off = hidden.includes(s.key);
              return (
                <button
                  key={s.key}
                  type="button"
                  aria-pressed={!off}
                  onClick={() => setHidden(h => (off ? h.filter(k => k !== s.key) : [...h, s.key]))}
                  className={cn(
                    "inline-flex h-6 cursor-pointer items-center gap-1.5 rounded-md px-1.5 text-[12px] transition-[opacity,background-color] duration-150 outline-none hover:bg-foreground/6 focus-visible:ring-2 focus-visible:ring-ring/40",
                    off ? "opacity-45" : "text-foreground/80",
                  )}
                >
                  <span aria-hidden="true" className="size-2 rounded-full" style={off ? { boxShadow: `inset 0 0 0 1px ${s.color}` } : { background: s.color }} />
                  {s.label}
                </button>
              );
            })}
          </span>
        )}
      </figcaption>

      <div ref={ref} className="relative" style={{ height }}>
        {width > 0 && (
          <svg
            width={width}
            height={height}
            role="img"
            aria-label={label}
            className={cn("block touch-none transition-opacity duration-200", loading && "opacity-50")}
            onPointerMove={onMove}
            onPointerLeave={() => setActive(null)}
          >
            <defs>
              {shown.filter(s => s.area).map(s => (
                <linearGradient key={s.key} id={`${id}-${s.key}`} x1="0" x2="0" y1="0" y2="1">
                  <stop offset="0" stopColor={s.color} stopOpacity="0.18" />
                  <stop offset="1" stopColor={s.color} stopOpacity="0" />
                </linearGradient>
              ))}
            </defs>

            {ticks.map(t => (
              <g key={t}>
                <line x1={PAD.left} x2={width - PAD.right} y1={y(t)} y2={y(t)} stroke="color-mix(in oklch, var(--foreground) 9%, transparent)" strokeWidth={0.5} strokeDasharray={t === 0 ? undefined : "2 3"} />
                <text x={PAD.left - 8} y={y(t)} dy="0.32em" textAnchor="end" className="fill-muted-foreground/80 text-[11px] tabular-nums">
                  {formatTick(t)}
                </text>
              </g>
            ))}

            {data.map((d, i) =>
              d.axisLabel && i % labelEvery === 0 ? (
                <text key={d.key} x={x(i)} y={height - 6} textAnchor="middle" className="fill-muted-foreground/80 text-[11px] tabular-nums">
                  {d.axisLabel}
                </text>
              ) : null,
            )}

            {shown.map(s =>
              runs(s.key).map((run, ri) => {
                const line = smooth(run);
                return (
                  <g key={`${s.key}-${ri}`}>
                    {s.area && run.length > 1 && (
                      <path d={`${line}L${run[run.length - 1][0]},${y(0)}L${run[0][0]},${y(0)}Z`} fill={`url(#${id}-${s.key})`} />
                    )}
                    <path d={line} fill="none" stroke={s.color} strokeWidth={1.75} strokeLinecap="round" strokeLinejoin="round" strokeDasharray={s.dashed ? "4 4" : undefined} />
                    {run.length === 1 && <circle cx={run[0][0]} cy={run[0][1]} r={2} fill={s.color} />}
                  </g>
                );
              }),
            )}

            {at && (
              <g pointerEvents="none">
                <line x1={tipLeft} x2={tipLeft} y1={PAD.top} y2={PAD.top + plotH} stroke="color-mix(in oklch, var(--foreground) 22%, transparent)" strokeWidth={1} />
                {shown.map(s => {
                  const v = at.values[s.key];
                  return v == null ? null : <circle key={s.key} cx={tipLeft} cy={y(v)} r={3.5} fill="var(--card)" stroke={s.color} strokeWidth={1.75} />;
                })}
              </g>
            )}
          </svg>
        )}

        {!loading && data.length === 0 && (
          <div className="absolute inset-0 grid place-items-center text-[13px] text-muted-foreground">{emptyLabel}</div>
        )}

        {at && (
          <div
            role="status"
            className="pointer-events-none absolute top-0 z-10 min-w-36 rounded-[14px] bg-card p-2.5 text-[13px] text-foreground/85 shadow-card"
            style={{
              left: tipLeft,
              transform: tipLeft > width / 2 ? "translateX(calc(-100% - 12px))" : "translateX(12px)",
            }}
          >
            <div className="mb-1.5 metadata-label tabular-nums">{at.label}</div>
            <ul className="grid gap-1 tabular-nums">
              {shown.map(s => (
                <li key={s.key} className="flex items-center gap-2">
                  <span aria-hidden="true" className="size-2 rounded-full" style={{ background: s.color }} />
                  <span className="flex-1 text-foreground/70">{s.label}</span>
                  <span className="text-foreground">{at.values[s.key] == null ? "—" : formatValue(at.values[s.key]!)}</span>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </figure>
  );
}
