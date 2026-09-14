import { useState } from "react";

/** Samples a sparkline holds. At the 5s poll that is the last ~2.5 minutes. */
export const LATENCY_HISTORY = 30;

/**
 * A rolling window of the last `limit` latency samples.
 *
 * `token` must change on every observation — a latency that reads the same
 * twice in a row is still a new sample, and keying on the value alone would
 * silently drop it, flattening a steady link to a single point.
 *
 * A sample of `null`/`undefined` (the probe failed) is not recorded: the line
 * stops where the data stops rather than drawing a fake zero.
 *
 * Appended while rendering rather than from an effect, the same way
 * `usePagination` clamps its page: the window is derived from the observation
 * this render already carries, so recording it in an effect would render the
 * old window first and only then correct it.
 *
 * History is deliberately client-side and resets on reload. It is a shape, not
 * a record — the durable series is the Prometheus gauge the backend exports.
 */
export function useLatencyHistory(
  sample: number | null | undefined,
  token: string | number,
  limit = LATENCY_HISTORY,
): number[] {
  const [history, setHistory] = useState<number[]>([]);
  const [seen, setSeen] = useState<string | number | null>(null);

  if (token !== seen) {
    setSeen(token);
    if (sample != null) {
      setHistory((prev) => [...prev, sample].slice(-limit));
    }
  }

  return history;
}
