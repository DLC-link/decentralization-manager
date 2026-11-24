import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useLatestFetch } from "./useLatestFetch";

describe("useLatestFetch", () => {
  it("keeps a request current until a later one begins", () => {
    const { result } = renderHook(() => useLatestFetch());
    const first = result.current();
    expect(first()).toBe(true);

    const second = result.current();
    expect(first()).toBe(false);
    expect(second()).toBe(true);
  });

  it("drops a slow answer that lands after a newer one", async () => {
    const { result } = renderHook(() => useLatestFetch());
    const committed: string[] = [];
    const load = async (value: string, delay: number) => {
      const isLatest = result.current();
      await new Promise((resolve) => setTimeout(resolve, delay));
      if (isLatest()) committed.push(value);
    };

    await Promise.all([load("slow poll", 20), load("manual refresh", 1)]);
    expect(committed).toEqual(["manual refresh"]);
  });

  it("returns the same begin function on every render", () => {
    const { result, rerender } = renderHook(() => useLatestFetch());
    const begin = result.current;
    rerender();
    expect(result.current).toBe(begin);
  });

  it("tracks each hook on its own", () => {
    const a = renderHook(() => useLatestFetch()).result;
    const b = renderHook(() => useLatestFetch()).result;
    const fromA = a.current();
    b.current();
    expect(fromA()).toBe(true);
  });
});
