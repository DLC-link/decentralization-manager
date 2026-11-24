import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CredentialsResponse } from "../types";
import { usePartyList } from "./usePartyList";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const partyId = "alpha::1220abc";

const renderList = (fetchMock: ReturnType<typeof vi.fn>) => {
  vi.stubGlobal("fetch", fetchMock);
  return renderHook(() =>
    usePartyList<CredentialsResponse>({
      path: "/credentials",
      partyId,
      field: "credentials",
      what: "credentials",
    }),
  );
};

const credential = { contract_id: "c1" } as CredentialsResponse["credentials"][number];

describe("usePartyList", () => {
  it("fetches nothing until the caller loads it", () => {
    const fetchMock = vi.fn();
    const { result } = renderList(fetchMock);

    expect(fetchMock).not.toHaveBeenCalled();
    expect(result.current.items).toEqual([]);
    expect(result.current.loading).toBe(false);
  });

  it("loads the field of the answer for the party", async () => {
    let answer: (r: Response) => void = () => {};
    const fetchMock = vi
      .fn()
      .mockReturnValue(new Promise<Response>((r) => (answer = r)));
    const { result } = renderList(fetchMock);

    let pending: Promise<void> = Promise.resolve();
    act(() => {
      pending = result.current.load();
    });
    expect(result.current.loading).toBe(true);
    expect(fetchMock.mock.calls[0][0]).toBe(
      "/credentials?party_id=alpha%3A%3A1220abc",
    );

    await act(async () => {
      answer(new Response(JSON.stringify({ credentials: [credential] })));
      await pending;
    });
    expect(result.current.items).toEqual([credential]);
    expect(result.current.loading).toBe(false);
  });

  it("keeps the old items and logs nothing for a non-ok answer", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ credentials: [credential] })),
      )
      .mockResolvedValueOnce(new Response("{}", { status: 500 }));
    const { result } = renderList(fetchMock);

    await act(() => result.current.load());
    await act(() => result.current.load());

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(result.current.items).toEqual([credential]);
    expect(result.current.loading).toBe(false);
    expect(error).not.toHaveBeenCalled();
  });

  it("only logs a failed request", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    const failure = new Error("offline");
    const { result } = renderList(vi.fn().mockRejectedValue(failure));

    await act(() => result.current.load());

    expect(error).toHaveBeenCalledWith("Failed to fetch credentials:", failure);
    expect(result.current.items).toEqual([]);
    expect(result.current.loading).toBe(false);
  });
});
