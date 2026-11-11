import { useEffect } from "react";

/**
 * Wrap a refresh so a tick is skipped while the previous one is still running.
 * Without it a response slower than the interval stacks requests until the
 * browser's connection pool is full and nothing else on the page loads.
 */
function pollGuard(refresh: () => Promise<void>): () => void {
  let inFlight = false;
  return () => {
    if (inFlight) return;
    inFlight = true;
    void refresh().finally(() => {
      inFlight = false;
    });
  };
}

/**
 * Run `refresh` now and every `intervalMs` after, through `pollGuard`. A new
 * `refresh` restarts the cycle; unmounting stops it.
 */
export function usePolling(refresh: () => Promise<void>, intervalMs: number) {
  useEffect(() => {
    const poll = pollGuard(refresh);
    poll();
    const interval = window.setInterval(poll, intervalMs);
    return () => clearInterval(interval);
  }, [refresh, intervalMs]);
}
