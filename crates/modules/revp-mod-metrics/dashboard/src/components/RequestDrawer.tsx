import { useEffect, useState, type ReactNode } from "react";
import { DangerTriangleIcon } from "@solar-icons/react/bold/danger-triangle";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { Button } from "@/components/ui/button";
import { ToneBadge, UserAvatar, pastaButton } from "@/components/pasta/surface";
import type { Filters, Rec } from "@/api";
import { fmtBytes, fmtDateTime, fmtMs, statusTone, userLabel } from "@/format";

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[8rem_1fr] gap-3 border-b-[0.5px] border-foreground/[0.09] py-2.5 last:border-b-0">
      <dt className="metadata-label leading-5">{label}</dt>
      <dd className="min-w-0 break-words text-foreground">{children ?? <span className="text-muted-foreground/70">—</span>}</dd>
    </div>
  );
}

/** Keeps the last item on screen while the sheet animates closed. */
function useLast<T>(item: T | null): T | null {
  const [last, setLast] = useState(item);
  useEffect(() => {
    if (item) setLast(item);
  }, [item]);
  return item ?? last;
}

type Props = {
  rec: Rec | null;
  onClose: () => void;
  onUser?: (user: string) => void;
  onFilter?: (patch: Partial<Filters>) => void;
};

export function RequestDrawer({ rec: open, onClose, onUser, onFilter }: Props) {
  const rec = useLast(open);
  const then = (fn: () => void) => () => {
    fn();
    onClose();
  };
  return (
    <Sheet open={!!open} onOpenChange={o => !o && onClose()}>
      {rec && (
        <SheetContent side="right" className="w-[min(36rem,100vw)] gap-0 overflow-y-auto border-l-0 bg-card font-sans font-[450] tracking-[-0.05px] text-muted-foreground shadow-card data-[side=right]:sm:max-w-[36rem]">
          <SheetHeader className="px-6 pt-6 pb-4">
            <SheetTitle className="card-heading truncate pr-8 font-mono text-[14px] text-foreground">
              {rec.method} {rec.path}
            </SheetTitle>
            <SheetDescription className="metadata-label">
              {fmtDateTime(rec.ts)} · request #{rec.id}
            </SheetDescription>
          </SheetHeader>

          <div className="grid gap-5 px-6 pb-6 text-sm">
            <div className="flex flex-wrap items-center gap-1.5">
              <ToneBadge tone={statusTone(rec.status)}>{rec.status || "no response"}</ToneBadge>
              <ToneBadge tone="neutral">{rec.kind}</ToneBadge>
              <ToneBadge tone="neutral">{rec.proto}</ToneBadge>
              {rec.tls && <ToneBadge tone="neutral">TLS</ToneBadge>}
              {rec.cache && <ToneBadge tone="info">cache {rec.cache}</ToneBadge>}
            </div>

            {rec.error && (
              <div>
                <div className="mb-2 flex items-center gap-2 px-1 text-destructive" role="alert">
                  <DangerTriangleIcon aria-hidden="true" className="size-3.5 shrink-0" />
                  <span>Request failed</span>
                </div>
                <div className="rounded-[16px] bg-muted/40 p-4 font-mono text-xs leading-5 text-foreground shadow-card">{rec.error}</div>
              </div>
            )}

            <dl className="rounded-[12px] px-4 py-1 shadow-card">
              <Row label="URL">
                <span className="font-mono text-xs">
                  {rec.path}
                  {rec.query && <span className="text-muted-foreground">?{rec.query}</span>}
                </span>
              </Row>
              <Row label="Host">{rec.host}</Row>
              <Row label="Duration">{fmtMs(rec.duration_ms)}</Row>
              <Row label="Sent / received">
                {fmtBytes(rec.bytes_out)} / {fmtBytes(rec.bytes_in)}
              </Row>
              <Row label="Route">{rec.route}</Row>
              <Row label="Service">{rec.service}</Row>
              <Row label="Upstream">{rec.upstream && <span className="font-mono text-xs">{rec.upstream}</span>}</Row>
              <Row label="Answered by">{rec.by ?? "upstream"}</Row>
              <Row label="User">
                {rec.user && (
                  <span className="inline-flex items-center gap-2">
                    <UserAvatar user={rec.user} size={22} />
                    {userLabel(rec.user)}
                  </span>
                )}
              </Row>
              <Row label="Client IP">{rec.client && <span className="font-mono text-xs">{rec.client}</span>}</Row>
              <Row label="User agent">{rec.user_agent}</Row>
              <Row label="Referer">{rec.referer}</Row>
            </dl>

            <div className="flex flex-wrap gap-2">
              {rec.user && onUser && (
                <Button size="sm" className={pastaButton.primary} onClick={then(() => onUser(rec.user!))}>
                  View user profile
                </Button>
              )}
              {onFilter && (
                <>
                  <Button size="sm" variant="outline" className={pastaButton.outline} onClick={then(() => onFilter({ route: rec.route }))}>
                    Same route
                  </Button>
                  <Button size="sm" variant="outline" className={pastaButton.outline} onClick={then(() => onFilter({ q: rec.path }))}>
                    Same path
                  </Button>
                  {rec.client && (
                    <Button size="sm" variant="outline" className={pastaButton.outline} onClick={then(() => onFilter({ q: rec.client! }))}>
                      Same IP
                    </Button>
                  )}
                </>
              )}
            </div>
          </div>
        </SheetContent>
      )}
    </Sheet>
  );
}
