import { useCallback, useEffect, useState } from "react";

const STORAGE_KEY = "favorite-parties";

function readStored(): Set<string> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return new Set();
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? new Set(parsed) : new Set();
  } catch {
    return new Set();
  }
}

/**
 * Starred parties first, in the order they were starred, then the rest in the
 * backend's order. The starred group doesn't follow the backend's order, so
 * two starred parties can't swap places between a load and a refresh.
 */
export function favoritesFirst<T extends { party_id: string }>(
  parties: T[],
  favorites: ReadonlySet<string>,
): T[] {
  if (favorites.size === 0) return parties;
  // A Set iterates in insertion order, which is star order.
  const rank = new Map([...favorites].map((id, i) => [id, i]));
  const starred = parties
    .filter((p) => rank.has(p.party_id))
    .sort((a, b) => rank.get(a.party_id)! - rank.get(b.party_id)!);
  if (starred.length === 0) return parties;
  return [...starred, ...parties.filter((p) => !rank.has(p.party_id))];
}

/** Parties the operator starred, kept in localStorage like hidden parties. */
export function useFavoriteParties() {
  const [favorites, setFavorites] = useState<Set<string>>(readStored);

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify([...favorites]));
  }, [favorites]);

  const toggle = useCallback((partyId: string) => {
    setFavorites((prev) => {
      const next = new Set(prev);
      if (next.has(partyId)) {
        next.delete(partyId);
      } else {
        next.add(partyId);
      }
      return next;
    });
  }, []);

  const isFavorite = useCallback(
    (partyId: string) => favorites.has(partyId),
    [favorites],
  );

  return { favorites, toggle, isFavorite };
}
