import { useCallback, useEffect, useState } from "react";
import { authenticatedFetch } from "./api";
import { API_BASE } from "./constants";
import type { NodeIdentityStatusResponse } from "./types";

/**
 * This node's identity party, its peers' parties and the stored login.
 *
 * Fetched once when `enabled` turns on and again on `refresh`, never polled:
 * every fetch runs topology reads on the participant and checks the login at
 * the IdP. A failed fetch keeps the last result next to the error.
 */
export function useNodeIdentity(enabled: boolean) {
  const [status, setStatus] = useState<NodeIdentityStatusResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  // A fetch is in flight while the last finished one is older than the request.
  const [requested, setRequested] = useState(0);
  const [finished, setFinished] = useState(-1);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    const load = async () => {
      try {
        const res = await authenticatedFetch(`${API_BASE}/node-identity`);
        if (!res.ok) {
          const body = await res.json().catch(() => null);
          throw new Error(
            body?.error || `Failed to load the node identity (HTTP ${res.status})`,
          );
        }
        const data: NodeIdentityStatusResponse = await res.json();
        if (!cancelled) {
          setStatus(data);
          setError(null);
        }
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : "Failed to load the node identity");
        }
      } finally {
        if (!cancelled) setFinished(requested);
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, [enabled, requested]);

  const refresh = useCallback(() => setRequested((r) => r + 1), []);

  return { status, error, loading: enabled && finished !== requested, refresh };
}
