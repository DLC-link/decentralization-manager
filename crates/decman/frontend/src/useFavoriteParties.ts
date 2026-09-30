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
 * Starred parties first, each group in its original order. Stable, so starred
 * parties keep their relative order and the rest keep the backend's.
 */
export function favoritesFirst<T extends { party_id: string }>(
  parties: T[],
  isFavorite: (partyId: string) => boolean,
): T[] {
  const starred = parties.filter((p) => isFavorite(p.party_id));
  if (starred.length === 0) return parties;
  return [...starred, ...parties.filter((p) => !isFavorite(p.party_id))];
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
