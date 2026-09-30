import { useEffect, useState } from "react";
import { authenticatedFetch } from "./api";
import { API_BASE } from "./constants";
import type { ExternalPartiesResponse, ExternalPartyInfo } from "./types";

export function useExternalParties(active: boolean) {
  const [parties, setParties] = useState<ExternalPartyInfo[]>([]);
  const [loading, setLoading] = useState(true);
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
          setParties(data.parties);
          setError(null);
        }
      } catch (error) {
        if (!cancelled) {
          setError(error instanceof Error ? error.message : "Failed to load external parties");
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
          // Poll the cheap snapshot, including after a cold-cache 503 or failure.
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

  return { parties, loading, error };
}
