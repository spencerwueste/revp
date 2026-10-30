import type { Tone } from "@/components/pasta/surface";

const int = new Intl.NumberFormat();
const pct = new Intl.NumberFormat(undefined, { style: "percent", maximumFractionDigits: 1 });

export const fmtInt = (n: number) => int.format(n);
export const fmtPct = (n: number) => pct.format(Number.isFinite(n) ? n : 0);

export function fmtMs(ms: number): string {
  if (ms < 1) return `${Math.round(ms * 1000)} µs`;
  if (ms < 100) return `${ms.toFixed(1)} ms`;
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} s`;
}

export function fmtBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = b / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${units[i]}`;
}

const clock = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "medium" });

export const fmtClock = (ts: number) => clock.format(ts);
export const fmtDateTime = (ts: number) => dateTime.format(ts);

export function fmtAgo(ts: number | null, now = Date.now()): string {
  if (ts == null) return "—";
  const s = Math.max(0, Math.round((now - ts) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 48) return `${h}h ago`;
  return `${Math.round(h / 24)}d ago`;
}

export function statusTone(status: number): Tone {
  if (status >= 500 || status === 0) return "danger";
  if (status >= 400) return "warning";
  if (status >= 300) return "info";
  return "success";
}

export const userLabel = (u: string | null) => (!u || u === "-" ? "Anonymous" : u);

export function initials(u: string | null): string {
  if (!u || u === "-") return "?";
  const name = u.split("@")[0];
  const parts = name.split(/[._\-\s]+/).filter(Boolean);
  return ((parts[0]?.[0] ?? "") + (parts[1]?.[0] ?? "")).toUpperCase() || name.slice(0, 2).toUpperCase();
}
