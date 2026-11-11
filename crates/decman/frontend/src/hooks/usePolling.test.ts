import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePolling } from "./usePolling";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

/** A refresh whose answers the test releases by hand. */
const pendingRefresh = () => {
  const releases: (() => void)[] = [];
  const refresh = vi.fn(
    () => new Promise<void>((resolve) => releases.push(resolve)),
  );
  return { refresh, release: () => releases.shift()?.() };
};

describe("usePolling", () => {
  it("refreshes at once and then on every interval", async () => {
    const refresh = vi.fn(() => Promise.resolve());
    renderHook(() => usePolling(refresh, 2000));
    expect(refresh).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1999);
    });
    expect(refresh).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(refresh).toHaveBeenCalledTimes(2);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(4000);
    });
    expect(refresh).toHaveBeenCalledTimes(4);
  });

  it("skips the ticks that come while a refresh is still running", async () => {
    const { refresh, release } = pendingRefresh();
    renderHook(() => usePolling(refresh, 2000));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(6000);
    });
    expect(refresh).toHaveBeenCalledTimes(1);

    await act(async () => {
      release();
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("stops when unmounted", async () => {
    const refresh = vi.fn(() => Promise.resolve());
    const { unmount } = renderHook(() => usePolling(refresh, 2000));
    unmount();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("restarts the cycle for a new refresh", async () => {
    const first = vi.fn(() => Promise.resolve());
    const second = vi.fn(() => Promise.resolve());
    const { rerender } = renderHook(({ refresh }) => usePolling(refresh, 2000), {
      initialProps: { refresh: first },
    });
    rerender({ refresh: second });
    expect(second).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(2);
  });
});
