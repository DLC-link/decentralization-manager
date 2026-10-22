import { useCallback, useEffect, useRef, useState } from "react";
import { authenticatedFetch } from "./api";
import { API_BASE } from "./constants";
import type { FormerPartiesResponse, FormerParty } from "./types";

/** How often the list is re-read while a purge runs. */
export const PURGE_POLL_MS = 3000;

/**
 * The parties this node hosted once and no longer hosts.
 *
 * The server notices a removal when the parties list is refreshed from Canton,
 * so the caller reloads after each parties load. While a purge runs the list
 * polls itself, to pick up the outcome.
 */
export function useFormerParties() {
  const [formerParties, setFormerParties] = useState<FormerParty[]>([]);
  // True after the first load settles, even a failed one, so a deep link to a
  // former party stops waiting for it.
  const [loaded, setLoaded] = useState(false);
  // Newest request wins: a poll and a manual reload can overlap.
  const seq = useRef(0);

  const reload = useCallback(async () => {
    const mine = ++seq.current;
    try {
      const res = await authenticatedFetch(`${API_BASE}/former-parties`);
      if (!res.ok) return;
      const data: FormerPartiesResponse = await res.json();
      if (mine !== seq.current) return;
      setFormerParties(data.parties);
    } catch {
      // Keep the last list; the next load tries again.
    } finally {
      setLoaded(true);
    }
  }, []);

  // The server claims the purge before it answers 202, so the party runs from
  // that moment. Marking it here starts the poll even if the next read fails.
  const markPurging = useCallback((partyId: string) => {
    // A read sent before the purge started would bring back the old status.
    ++seq.current;
    setFormerParties((parties) =>
      parties.map((p) =>
        p.party_id === partyId ? { ...p, purge_status: "running" } : p,
      ),
    );
  }, []);

  const purging = formerParties.some((p) => p.purge_status === "running");
  useEffect(() => {
    if (!purging) return;
    const interval = window.setInterval(() => void reload(), PURGE_POLL_MS);
    return () => clearInterval(interval);
  }, [purging, reload]);

  return { formerParties, loaded, reload, markPurging };
}
