import { LineChart, type LineChartDatum, type LineChartSeries } from "@/components/pasta/line-chart";
import { DonutChart } from "@/components/pasta/donut-chart";
import type { Point } from "@/api";
import { fmtClock, fmtInt, fmtMs } from "@/format";

const STATUS_SERIES: LineChartSeries[] = [
  { key: "2xx", label: "2xx", color: "var(--success)", area: true },
  { key: "3xx", label: "3xx", color: "var(--chart-1)" },
  { key: "4xx", label: "4xx", color: "var(--warning)" },
  { key: "5xx", label: "5xx", color: "var(--destructive)" },
];

const LATENCY_SERIES: LineChartSeries[] = [
  { key: "p50", label: "p50", color: "var(--chart-1)", area: true },
  { key: "p95", label: "p95", color: "var(--chart-2)", dashed: true },
];

function datum(p: Point, values: LineChartDatum["values"]): LineChartDatum {
  return { key: String(p.t), label: fmtClock(p.t), axisLabel: fmtClock(p.t).slice(0, 5), values };
}

export function TrafficChart({ points, stepMs, loading }: { points: Point[]; stepMs: number; loading?: boolean }) {
  const step = stepMs >= 60_000 ? `${Math.round(stepMs / 60_000)} min` : `${Math.round(stepMs / 1000)} s`;
  return (
    <LineChart
      label={`Requests per ${step}, by status`}
      data={points.map(p => datum(p, { "2xx": p["2xx"], "3xx": p["3xx"], "4xx": p["4xx"], "5xx": p["5xx"] }))}
      series={STATUS_SERIES}
      formatValue={v => fmtInt(v)}
      loading={loading && points.length === 0}
      emptyLabel="No requests in this range"
      height={200}
      legend
    />
  );
}

export function LatencyChart({ points, loading }: { points: Point[]; loading?: boolean }) {
  // Buckets with no requests have no latency; leave a gap, not a zero.
  const data = points.map(p => {
    const any = p["2xx"] + p["3xx"] + p["4xx"] + p["5xx"] + p.other > 0;
    return datum(p, { p50: any ? p.p50_ms : undefined, p95: any ? p.p95_ms : undefined });
  });
  return (
    <LineChart
      label="Latency"
      data={data}
      series={LATENCY_SERIES}
      formatValue={v => fmtMs(v)}
      formatTick={v => fmtMs(v)}
      loading={loading && points.length === 0}
      emptyLabel="No requests in this range"
      height={200}
      legend
    />
  );
}

export function StatusDonut({ points }: { points: Point[] }) {
  const sum = (k: "2xx" | "3xx" | "4xx" | "5xx" | "other") => points.reduce((n, p) => n + p[k], 0);
  const data = [
    { key: "2xx", label: "2xx", value: sum("2xx"), color: "var(--success)" },
    { key: "3xx", label: "3xx", value: sum("3xx"), color: "var(--chart-1)" },
    { key: "4xx", label: "4xx", value: sum("4xx"), color: "var(--warning)" },
    { key: "5xx", label: "5xx", value: sum("5xx"), color: "var(--destructive)" },
    { key: "other", label: "No response", value: sum("other"), color: "var(--muted-foreground)" },
  ].filter(d => d.value > 0);
  return <DonutChart label="Responses by status" data={data} totalLabel="Requests" size={168} thickness={14} emptyLabel="No requests in this range" />;
}
