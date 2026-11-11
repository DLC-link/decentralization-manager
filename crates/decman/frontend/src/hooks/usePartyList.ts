import { useCallback, useState } from "react";
import { authenticatedFetch } from "../api";

interface PartyListSource<R, K extends keyof R> {
  /** The list endpoint. The hook adds `?party_id=<partyId>`. */
  path: string;
  partyId: string;
  /** The field of the answer that holds the list. */
  field: K;
  /** What the list holds, for the log line when the request fails. */
  what: string;
}

/**
 * A party-scoped list that the caller loads on demand. `load` GETs the list
 * and keeps `field` of the answer. A non-ok answer keeps the old items, and a
 * failed request is only logged. `loading` is true while `load` runs.
 *
 * Each answer type has one field, so `usePartyList<CredentialsResponse>` is
 * enough; `K` then defaults to that field.
 */
export function usePartyList<R, K extends keyof R = keyof R>({
  path,
  partyId,
  field,
  what,
}: PartyListSource<R, K>) {
  const [items, setItems] = useState<R[K]>([] as R[K]);
  const [loading, setLoading] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const res = await authenticatedFetch(
        `${path}?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: R = await res.json();
        setItems(response[field]);
      }
    } catch (e) {
      console.error(`Failed to fetch ${what}:`, e);
    } finally {
      setLoading(false);
    }
  }, [path, partyId, field, what]);

  return { items, loading, load };
}
