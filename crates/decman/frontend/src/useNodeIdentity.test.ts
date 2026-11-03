import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { NodeIdentityStatusResponse } from "./types";
import { useNodeIdentity } from "./useNodeIdentity";

const STATUS: NodeIdentityStatusResponse = {
  self: {
    participant_id: `participant1::1220${"a".repeat(64)}`,
    name: "Node 1",
    node_party_id: `decman-node-participant1::1220${"a".repeat(64)}`,
    found: true,
    namespace_matches: true,
    hosting_permission: "submission",
    verified: true,
  },
  peers: [],
  scan_filter: "decman-node-",
  scan: [],
};

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const stubBackend = (answer: () => Response) => {
  const fetchMock = vi.fn(async () => answer());
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
};

describe("useNodeIdentity", () => {
  it("fetches nothing while disabled", () => {
    const fetchMock = stubBackend(() => Response.json(STATUS));

    const { result } = renderHook(() => useNodeIdentity(false));

    expect(fetchMock).not.toHaveBeenCalled();
    expect(result.current.loading).toBe(false);
  });

  it("fetches once on enable and again only on refresh", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const fetchMock = stubBackend(() => Response.json(STATUS));

    const { result } = renderHook(() => useNodeIdentity(true));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.status).toEqual(STATUS);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);

    act(() => result.current.refresh());
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(result.current.loading).toBe(false));
  });

  it("keeps the last result next to a failed refresh", async () => {
    let fail = false;
    stubBackend(() =>
      fail
        ? Response.json({ error: "Canton call failed" }, { status: 500 })
        : Response.json(STATUS),
    );

    const { result } = renderHook(() => useNodeIdentity(true));
    await waitFor(() => expect(result.current.status).toEqual(STATUS));

    fail = true;
    act(() => result.current.refresh());

    await waitFor(() => expect(result.current.error).toBe("Canton call failed"));
    expect(result.current.status).toEqual(STATUS);
  });
});
