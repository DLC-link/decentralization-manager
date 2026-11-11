import { useCallback, useRef } from "react";

/**
 * Newest request wins. Call `begin()` when a request starts; the check it
 * returns stays true until a later request begins, so a slower earlier
 * answer cannot overwrite a later one.
 */
export function useLatestFetch(): () => () => boolean {
  const seq = useRef(0);
  return useCallback(() => {
    const mine = ++seq.current;
    return () => mine === seq.current;
  }, []);
}
