// A donut in Pasta UI's visual language (Pasta ships no chart): the ring and
// legend share Context Breakdown's hover behaviour, where pointing at a segment
// or its row dims the others, and the centre reads like its headline figure.

import { useState } from "react";
import { cn } from "@/lib/utils";
import { fmtInt } from "@/format";

export interface DonutDatum {
  key: string;
  label: string;
  value: number;
  color: string;
}

export interface DonutChartProps {
  data: DonutDatum[];
  label: string;
  totalLabel?: string;
  size?: number;
  thickness?: number;
  emptyLabel?: string;
  className?: string;
}

export function DonutChart({ data, label, totalLabel = "Total", size = 168, thickness = 14, emptyLabel = "No data", className }: DonutChartProps) {
  const [activeKey, setActiveKey] = useState<string>();
  const total = data.reduce((n, d) => n + Math.max(0, d.value), 0);
  const r = (size - thickness) / 2;
  const c = 2 * Math.PI * r;
  // A small gap between segments, like the hairline between Context Breakdown's bars.
  const gap = data.length > 1 ? Math.min(4, c / data.length / 4) : 0;
  const dim = (k: string) => activeKey !== undefined && activeKey !== k;
  const active = data.find(d => d.key === activeKey);
  const pct = (v: number) => (total > 0 ? Math.round((v / total) * 100) : 0);

  let offset = 0;
  return (
    <figure aria-label={label} className={cn("font-sans text-sm font-[450] tracking-[-0.05px] text-muted-foreground", className)}>
      <figcaption className="mb-4 text-sm leading-5 font-medium text-muted-foreground">{label}</figcaption>
      <div className="relative mx-auto" style={{ width: size, height: size }}>
        <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} role="img" aria-label={label} className="-rotate-90">
          <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke="color-mix(in oklch, var(--foreground) 8%, transparent)" strokeWidth={thickness} />
          {total > 0 &&
            data.map(d => {
              const len = (d.value / total) * c;
              const seg = (
                <circle
                  key={d.key}
                  cx={size / 2}
                  cy={size / 2}
                  r={r}
                  fill="none"
                  stroke={d.color}
                  strokeWidth={thickness}
                  strokeDasharray={`${Math.max(0, len - gap)} ${c}`}
                  strokeDashoffset={-offset}
                  className={cn("cursor-default transition-[opacity,stroke-dasharray] duration-500 ease-[cubic-bezier(0.23,1,0.32,1)] motion-reduce:transition-none", dim(d.key) && "opacity-30")}
                  onPointerEnter={() => setActiveKey(d.key)}
                  onPointerLeave={() => setActiveKey(undefined)}
                />
              );
              offset += len;
              return seg;
            })}
        </svg>
        <div className="pointer-events-none absolute inset-0 grid place-content-center text-center tabular-nums">
          {total > 0 ? (
            <>
              <span className="text-[22px] leading-7 font-medium tracking-[-0.02em] text-foreground">{fmtInt(active ? active.value : total)}</span>
              <span className="metadata-label">{active ? active.label : totalLabel}</span>
            </>
          ) : (
            <span className="text-[13px]">{emptyLabel}</span>
          )}
        </div>
      </div>

      {total > 0 && (
        <ul className="mt-5 space-y-1 tabular-nums">
          {data.map(d => (
            <li
              key={d.key}
              className={cn(
                "-mx-2 flex items-center gap-2.5 rounded-md px-2 py-1 transition-[opacity,background-color] duration-150",
                activeKey === d.key && "bg-foreground/8",
                dim(d.key) && "opacity-50",
              )}
              onPointerEnter={() => setActiveKey(d.key)}
              onPointerLeave={() => setActiveKey(undefined)}
            >
              <span aria-hidden="true" className="size-2 shrink-0 rounded-full" style={{ background: d.color }} />
              <span className="min-w-0 flex-1 truncate text-foreground/80">{d.label}</span>
              <span className="text-foreground/80">{fmtInt(d.value)}</span>
              <span className="w-10 text-right text-muted-foreground/70">{pct(d.value)}%</span>
            </li>
          ))}
        </ul>
      )}
    </figure>
  );
}
