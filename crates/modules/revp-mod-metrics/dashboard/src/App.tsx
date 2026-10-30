import { useEffect } from "react";
import { MoonIcon } from "@solar-icons/react/linear/moon";
import { PauseIcon } from "@solar-icons/react/linear/pause";
import { PlayIcon } from "@solar-icons/react/linear/play";
import { Sun2Icon } from "@solar-icons/react/linear/sun-2";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Button } from "@/components/ui/button";
import { pastaButton } from "@/components/pasta/surface";
import { FILTER_KEYS, type Filters, type Sort } from "@/api";
import { useHashState, useStored } from "@/hooks";
import { RequestsView } from "@/views/RequestsView";
import { UsersView } from "@/views/UsersView";
import { UserProfile } from "@/views/UserProfile";

const PAGE_SIZE = 100;

export function App() {
  const [hash, setHash] = useHashState();
  const [theme, setTheme] = useStored("revp-dashboard-theme", window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  const [liveStored, setLive] = useStored("revp-dashboard-live", "1");
  const live = liveStored === "1";

  useEffect(() => {
    document.documentElement.classList.toggle("dark", theme === "dark");
  }, [theme]);

  const tab = hash.tab === "users" ? "users" : "requests";
  const filters: Filters = Object.fromEntries(FILTER_KEYS.filter(k => hash[k]).map(k => [k, hash[k]]));
  const sort: Sort = { sort: hash.sort ?? "ts", order: hash.order === "asc" ? "asc" : "desc" };
  const offset = Number(hash.o ?? 0) || 0;

  // Any change to what is shown starts again from the first page.
  const setFilters = (patch: Partial<Filters>) => setHash({ ...patch, o: undefined }, true);
  const openUser = (user: string) => setHash({ tab: "users", profile: user, o: undefined });

  return (
    <div className="mx-auto grid max-w-[1600px] gap-6 px-4 py-6 sm:px-8">
      <header className="flex flex-wrap items-center gap-3 font-sans font-[450] tracking-[-0.05px]">
        <span aria-hidden="true" className="grid size-9 place-items-center rounded-[10px] bg-card font-mono text-sm font-semibold text-foreground shadow-card">
          rp
        </span>
        <div className="flex-1">
          <h1 className="card-heading text-foreground">revp</h1>
          <p className="metadata-label">Requests through this proxy, newest first</p>
        </div>
        <span className="inline-flex h-7 items-center gap-2 rounded-full bg-card px-3 text-[13px] text-muted-foreground shadow-card" aria-live="polite">
          <span className={`size-1.5 rounded-full ${live ? "animate-pulse bg-success" : "bg-muted-foreground/50"}`} />
          {live ? "Live" : "Paused"}
        </span>
        <Button variant="outline" size="sm" className={pastaButton.outline} onClick={() => setLive(live ? "0" : "1")} aria-label={live ? "Pause live updates" : "Resume live updates"}>
          {live ? <PauseIcon /> : <PlayIcon />}
          {live ? "Pause" : "Resume"}
        </Button>
        <Button variant="ghost" size="icon-sm" className="rounded-md text-foreground/75 hover:bg-foreground/6" onClick={() => setTheme(theme === "dark" ? "light" : "dark")} aria-label="Toggle dark mode">
          {theme === "dark" ? <Sun2Icon /> : <MoonIcon />}
        </Button>
      </header>

      <Tabs className="flex-col" value={tab} onValueChange={v => setHash({ tab: String(v), profile: undefined, o: undefined })}>
        <TabsList className="h-9 rounded-[12px] bg-foreground/[0.06] p-1">
          <TabsTrigger value="requests" className="rounded-[9px] px-3 text-[13px] font-[450] data-active:shadow-card dark:data-active:border-transparent dark:data-active:bg-card">
            Requests
          </TabsTrigger>
          <TabsTrigger value="users" className="rounded-[9px] px-3 text-[13px] font-[450] data-active:shadow-card dark:data-active:border-transparent dark:data-active:bg-card">
            Users
          </TabsTrigger>
        </TabsList>
        <TabsContent value="requests" className="pt-3">
          <RequestsView
            filters={filters}
            sort={sort}
            offset={offset}
            limit={PAGE_SIZE}
            live={live}
            onFilters={setFilters}
            onSort={s => setHash({ sort: s.sort, order: s.order, o: undefined }, true)}
            onPage={o => setHash({ o: o ? String(o) : undefined }, true)}
            onUser={openUser}
          />
        </TabsContent>
        <TabsContent value="users" className="pt-3">
          {hash.profile ? (
            <UserProfile
              key={hash.profile}
              user={hash.profile}
              filters={{ ...filters, user: undefined }}
              live={live}
              onBack={() => setHash({ profile: undefined })}
              onFilters={setFilters}
              onRequests={p => setHash({ ...p, tab: "requests", profile: undefined, o: undefined })}
            />
          ) : (
            <UsersView range={filters.range} live={live} onRange={r => setFilters({ range: r })} onOpen={openUser} />
          )}
        </TabsContent>
      </Tabs>
    </div>
  );
}
