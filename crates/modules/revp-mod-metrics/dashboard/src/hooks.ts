import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";

/**
 * Fetches `url` now and again every `everyMs` while `live`. Keeps the last good
 * result on screen while the next one loads, and drops responses that arrive
 * after the URL has moved on.
 */
export function usePoll<T>(url: string | null, fetcher: (url: string, signal: AbortSignal) => Promise<T>, live: boolean, everyMs = 2000) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const fetcherRef = useRef(fetcher);
  fetcherRef.current = fetcher;

  useEffect(() => {
    if (!url) return;
    let ctl: AbortController | null = null;
    let stopped = false;
    const run = async () => {
      ctl?.abort();
      ctl = new AbortController();
      setLoading(true);
      try {
        const d = await fetcherRef.current(url, ctl.signal);
        if (!stopped) {
          setData(d);
          setError(null);
        }
      } catch (e) {
        if (!stopped && !(e instanceof DOMException && e.name === "AbortError")) setError(String(e instanceof Error ? e.message : e));
      } finally {
        if (!stopped) setLoading(false);
      }
    };
    run();
    const timer = live ? window.setInterval(run, everyMs) : undefined;
    return () => {
      stopped = true;
      ctl?.abort();
      window.clearInterval(timer);
    };
  }, [url, live, everyMs]);

  return { data, error, loading };
}

/** View state in the URL hash (`#tab=users&route=api`), so a view can be bookmarked or shared. */
export type HashState = Record<string, string>;

function readHash(): HashState {
  const out: HashState = {};
  new URLSearchParams(window.location.hash.slice(1)).forEach((v, k) => {
    if (v) out[k] = v;
  });
  return out;
}

const subscribe = (cb: () => void) => {
  window.addEventListener("hashchange", cb);
  return () => window.removeEventListener("hashchange", cb);
};

export type HashPatch = Record<string, string | undefined>;

export function useHashState(): [HashState, (patch: HashPatch, replace?: boolean) => void] {
  const raw = useSyncExternalStore(subscribe, () => window.location.hash);
  const state = useRef<{ raw: string; value: HashState }>({ raw: "\0", value: {} });
  if (state.current.raw !== raw) state.current = { raw, value: readHash() };

  const update = useCallback((patch: HashPatch, replace = false) => {
    const next = { ...readHash(), ...patch };
    const p = new URLSearchParams();
    for (const [k, v] of Object.entries(next)) if (v) p.set(k, v);
    const hash = `#${p.toString()}`;
    if (replace) history.replaceState(null, "", hash);
    else history.pushState(null, "", hash);
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  }, []);

  return [state.current.value, update];
}

export function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = window.setTimeout(() => setV(value), ms);
    return () => window.clearTimeout(t);
  }, [value, ms]);
  return v;
}

/** Per-viewer preference; storage can be missing or throw (private windows). */
export function useStored(key: string, fallback: string): [string, (v: string) => void] {
  const [v, setV] = useState(() => {
    try {
      return window.localStorage.getItem(key) ?? fallback;
    } catch {
      return fallback;
    }
  });
  const set = useCallback(
    (next: string) => {
      setV(next);
      try {
        window.localStorage.setItem(key, next);
      } catch {
        /* not persisted; fine */
      }
    },
    [key],
  );
  return [v, set];
}
