import { useCallback, useState } from "react";
import { API_BASE } from "../constants";
import { authenticatedFetch, pingLatency } from "../api";
import type { ParticipantStatus } from "../types";
import { usePolling } from "./usePolling";

/**
 * The participant statuses and this node's own latency to the backend,
 * polled every 2 seconds.
 */
export function useParticipantStatuses() {
  const [participantStatuses, setParticipantStatuses] = useState<
    ParticipantStatus[]
  >([]);
  const [selfWireVersion, setSelfWireVersion] = useState<number | undefined>();
  // Our own round-trip latency to the backend (ms). The peers table fills the
  // peer rows from Noise health probes but never the "you" row; this fills it.
  //
  // `seq` advances on every measurement, so the node health card's sparkline
  // still records a sample when the latency reads the same twice in a row.
  const [selfLatency, setSelfLatency] = useState<{ ms?: number; seq: number }>({
    seq: 0,
  });

  // Poll participant statuses every 2 seconds, and on the same cadence measure
  // our own round-trip to the backend so the "you" row of the peers table shows
  // a latency too (peer latencies come from the status response itself).
  const refreshStatuses = useCallback(async () => {
    const fetchStatuses = async () => {
      try {
        const res = await authenticatedFetch(`${API_BASE}/participants-status`);
        if (res.ok) {
          const data = await res.json();
          setParticipantStatuses(data.statuses);
          setSelfWireVersion(data.wire_version);
        }
      } catch {
        // Ignore polling errors
      }
    };

    const measureSelfLatency = async () => {
      // Clear on failure so the row drops a stale number during an outage,
      // matching how an unreachable peer shows no latency.
      const ms = await pingLatency(`${API_BASE}/healthz`);
      setSelfLatency((prev) => ({ ms: ms ?? undefined, seq: prev.seq + 1 }));
    };

    await Promise.all([fetchStatuses(), measureSelfLatency()]);
  }, []);

  // In-flight guarded: if a cycle (the status probe in particular) runs
  // longer than the 2s interval, the tick is skipped instead of stacking
  // concurrent requests.
  usePolling(refreshStatuses, 2000);

  return { participantStatuses, selfWireVersion, selfLatency };
}
