import { useEffect, useMemo, useState } from "react";
import { authenticatedFetch } from "./api";
import { API_BASE } from "./constants";
import { sharedPartyCounts } from "./sharedParties";
import type {
  DecentralizedPartiesResponse,
  NetworkConfig,
  Peer,
} from "./types";

/// One GET on mount. Null until it answers, and stays null if it fails: the
/// callers only add labels and links, so a failed read leaves the view as is.
function useJsonOnce<T>(path: string): T | null {
  const [data, setData] = useState<T | null>(null);
  useEffect(() => {
    let cancelled = false;
    authenticatedFetch(`${API_BASE}${path}`)
      .then((res) => (res.ok ? (res.json() as Promise<T>) : null))
      .then((body) => {
        if (!cancelled && body) setData(body);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [path]);
  return data;
}

/// This node's configured peers.
export function usePeers(): Peer[] | null {
  return useJsonOnce<NetworkConfig>("/network-config")?.peers ?? null;
}

/// How many parties this node shares with each participant. It reads the
/// whole list without a prefix, because the list on the parties tab can be
/// narrowed by one and would undercount.
export function useSharedPartyCounts(): Map<string, number> | null {
  const data = useJsonOnce<DecentralizedPartiesResponse>(
    "/decentralized-parties",
  );
  return useMemo(() => (data ? sharedPartyCounts(data.parties) : null), [data]);
}
