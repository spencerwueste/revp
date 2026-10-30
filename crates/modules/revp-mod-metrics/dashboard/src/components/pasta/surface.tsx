// Dashboard pieces Pasta UI has no component for, drawn in its visual language:
// card surfaces with the 0.5px `shadow-card` outline, 16px radii, 450-weight
// muted text and -0.05px tracking (see context-breakdown, tool-error and
// data-table in components/ai).

import type { ReactNode } from "react";
import { DangerTriangleIcon } from "@solar-icons/react/bold/danger-triangle";
import { InfoCircleIcon } from "@solar-icons/react/linear/info-circle";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import { fmtInt, initials } from "@/format";

const surface = "rounded-[16px] bg-card font-sans text-sm font-[450] tracking-[-0.05px] text-muted-foreground shadow-card";

export function Panel({ title, action, children, className }: { title?: ReactNode; action?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <section className={cn(surface, "p-5", className)}>
      {(title || action) && (
        <header className="mb-4 flex items-center justify-between gap-3">
          {title && <h3 className="text-sm leading-5 font-medium text-muted-foreground">{title}</h3>}
          {action}
        </header>
      )}
      {children}
    </section>
  );
}

export function Stat({ label, value, hint, tone }: { label: string; value: ReactNode; hint?: ReactNode; tone?: "danger" | "warning" | "success" }) {
  return (
    <section className={cn(surface, "px-5 py-4")}>
      <h3 className="text-sm leading-5 font-medium text-muted-foreground">{label}</h3>
      <p
        className={cn(
          "mt-1 text-[22px] leading-7 font-medium tracking-[-0.02em] text-foreground tabular-nums",
          tone === "danger" && "text-destructive",
          tone === "warning" && "text-warning",
          tone === "success" && "text-success",
        )}
      >
        {value}
      </p>
      {hint && <p className="mt-1 metadata-label tabular-nums">{hint}</p>}
    </section>
  );
}

export function UserAvatar({ user, size = 32, className }: { user: string | null; size?: number; className?: string }) {
  return (
    <Avatar aria-hidden="true" className={cn("after:hidden", className)} style={{ width: size, height: size }}>
      <AvatarFallback className="bg-foreground/[0.07] font-medium text-foreground/75" style={{ fontSize: Math.max(9, size * 0.38) }}>
        {initials(user)}
      </AvatarFallback>
    </Avatar>
  );
}

const METHOD_TONE: Record<string, string> = {
  GET: "text-chart-1",
  POST: "text-success",
  PUT: "text-warning",
  PATCH: "text-warning",
  DELETE: "text-destructive",
};

export function MethodTag({ method }: { method: string }) {
  return <span className={cn("font-mono text-xs font-semibold", METHOD_TONE[method] ?? "text-muted-foreground")}>{method}</span>;
}

export type Tone = "success" | "info" | "warning" | "danger" | "neutral";

const TONE: Record<Tone, string> = {
  success: "bg-success/10 text-success",
  info: "bg-info/10 text-info",
  warning: "bg-warning/12 text-warning",
  danger: "bg-destructive/10 text-destructive",
  neutral: "bg-foreground/[0.06] text-foreground/75",
};

/** shadcn's Badge with Pasta's tinted status treatments. */
export function ToneBadge({ tone, children, className }: { tone: Tone; children: ReactNode; className?: string }) {
  return (
    <Badge variant="secondary" className={cn("font-[450] tabular-nums", TONE[tone], className)}>
      {children}
    </Badge>
  );
}

/** Ranked list with proportional bars, in the row style of Pasta's Context Breakdown. */
export function BarList({ items, onPick, empty = "Nothing yet" }: { items: [string, number][]; onPick?: (v: string) => void; empty?: string }) {
  if (!items.length) return <p className="text-[13px] text-muted-foreground">{empty}</p>;
  const max = Math.max(...items.map(([, n]) => n));
  return (
    <ul className="-mx-2 grid grid-cols-1 gap-0.5 tabular-nums">
      {items.map(([v, n]) => {
        const body = (
          <>
            <span className="absolute inset-y-0.5 left-0 rounded-md bg-foreground/[0.06]" style={{ width: `${(n / max) * 100}%` }} />
            <span className="relative min-w-0 flex-1 truncate text-foreground/80" title={v}>
              {v}
            </span>
            <span className="relative shrink-0 text-foreground/80">{fmtInt(n)}</span>
          </>
        );
        const cls = "relative flex w-full items-center gap-2.5 rounded-md px-2 py-1 text-left";
        return (
          <li key={v}>
            {onPick ? (
              <button
                type="button"
                className={cn(cls, "cursor-pointer transition-colors duration-150 outline-none hover:bg-foreground/[0.04] focus-visible:ring-2 focus-visible:ring-ring/40")}
                onClick={() => onPick(v)}
              >
                {body}
              </button>
            ) : (
              <div className={cls}>{body}</div>
            )}
          </li>
        );
      })}
    </ul>
  );
}

/** A status line over a tinted card, after Pasta's Tool Error. */
export function Notice({ tone = "neutral", title, children }: { tone?: "neutral" | "danger"; title?: string; children: ReactNode }) {
  const danger = tone === "danger";
  const Icon = danger ? DangerTriangleIcon : InfoCircleIcon;
  return (
    <section
      role={danger ? "alert" : "status"}
      className="font-sans text-sm font-[450] tracking-[-0.05px] text-muted-foreground transition-[opacity,translate] duration-200 ease-[cubic-bezier(0.23,1,0.32,1)] starting:translate-y-1 starting:opacity-0"
    >
      <div className={cn("mb-2 flex items-center gap-2 px-1", danger ? "text-destructive" : "text-foreground/75")}>
        <Icon aria-hidden="true" className="size-3.5 shrink-0" />
        <span>{title ?? (danger ? "Needs attention" : "Heads up")}</span>
      </div>
      <div className="rounded-[16px] bg-muted/40 p-4 leading-5 text-foreground shadow-card">{children}</div>
    </section>
  );
}

/** Button treatments from Pasta's cards (Tool Error, Approval Card), applied over shadcn's Button. */
export const pastaButton = {
  primary:
    "h-7 rounded-md border-0 bg-primary bg-linear-to-b from-[color-mix(in_oklch,var(--primary),var(--primary-foreground)_30%)] to-primary px-3 text-sm font-[450] text-primary-foreground shadow-[inset_0_1px_0_rgba(255,255,255,0.14),0_1px_2px_rgba(0,0,0,0.18)] hover:brightness-110 dark:bg-none",
  outline: "h-7 rounded-md bg-card px-3 text-sm font-[450]",
  ghost: "h-7 rounded-md px-2.5 text-sm font-[450] text-foreground/75 hover:bg-foreground/6",
};
