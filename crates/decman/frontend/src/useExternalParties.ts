import { useEffect, useState } from "react";
import { authenticatedFetch } from "./api";
import { API_BASE } from "./constants";
import type { ExternalPartiesResponse } from "./types";

export function useExternalParties(active: boolean) {
  const [snapshot, setSnapshot] = useState<ExternalPartiesResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      try {
        const response = await authenticatedFetch(`${API_BASE}/external-parties`);
        if (!response.ok) {
          const body = await response.json().catch(() => null);
          throw new Error(body?.error || `Failed to load external parties (HTTP ${response.status})`);
        }
        const data: ExternalPartiesResponse = await response.json();
        if (!cancelled) {
          setSnapshot(data);
          setError(null);
        }
      } catch (error) {
        if (!cancelled) {
          setError(error instanceof Error ? error.message : "Failed to load external parties");
        }
      } finally {
        if (!cancelled) {
          // Poll the cheap snapshot. A poll of a stale snapshot starts a
          // server scan, so scans run only while the tab is open.
          timer = setTimeout(refresh, 10_000);
        }
      }
    };
    void refresh();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [active]);

  return {
    parties: snapshot?.parties ?? [],
    // Without `fetched_at`, the server's first scan has not finished, so an
    // empty list means "not known yet" rather than "hosts nothing".
    loading: !error && !snapshot?.fetched_at,
    error,
    fetchedAt: snapshot?.fetched_at ?? null,
    refreshing: snapshot?.refreshing ?? false,
    refreshError: snapshot?.refresh_error ?? null,
  };
}
