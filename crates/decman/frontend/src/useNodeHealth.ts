import { useEffect, useState } from "react";
import { authenticatedFetch } from "./api";
import { API_BASE } from "./constants";
import type { NodeHealthResponse } from "./types";

/**
 * Poll interval for `/node-health`. Matches the backend's snapshot TTL, so a
 * watching tab drives roughly one pair of gRPC probes per tick and extra tabs
 * cost nothing (the backend serves them the same cached snapshot).
 */
export const NODE_HEALTH_POLL_MS = 5000;

/**
 * The node's per-hop health, polled while `enabled`.
 *
 * Only the Config tab mounts this, so no poll runs while the operator is on
 * another tab. A failed fetch keeps the last snapshot rather than blanking the
 * card — its `checked_at` age is what tells the reader it has gone stale.
 */
export function useNodeHealth(enabled: boolean): NodeHealthResponse | null {
  const [health, setHealth] = useState<NodeHealthResponse | null>(null);

  useEffect(() => {
    if (!enabled) return;

    let cancelled = false;
    // In-flight guard: the backend bounds a probe at ~2s, but a slow link to
    // the node itself could still outlast the interval. Skip the tick rather
    // than stacking requests.
    let inFlight = false;

    const poll = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const res = await authenticatedFetch(`${API_BASE}/node-health`);
        if (res.ok && !cancelled) setHealth(await res.json());
      } catch {
        // Ignore — the last snapshot stays on screen and visibly ages.
      } finally {
        inFlight = false;
      }
    };

    void poll();
    const timer = window.setInterval(() => void poll(), NODE_HEALTH_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [enabled]);

  return health;
}
