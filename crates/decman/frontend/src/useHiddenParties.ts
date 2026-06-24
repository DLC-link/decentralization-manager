import { useCallback, useEffect, useState } from "react";

const STORAGE_KEY = "hidden-parties";

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

export function useHiddenParties() {
  const [hidden, setHidden] = useState<Set<string>>(readStored);

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify([...hidden]));
  }, [hidden]);

  const toggle = useCallback((partyId: string) => {
    setHidden((prev) => {
      const next = new Set(prev);
      if (next.has(partyId)) {
        next.delete(partyId);
      } else {
        next.add(partyId);
      }
      return next;
    });
  }, []);

  const isHidden = useCallback(
    (partyId: string) => hidden.has(partyId),
    [hidden],
  );

  return { hidden, toggle, isHidden };
}
