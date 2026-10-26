import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { authenticatedFetch } from "./api";
import { PURGE_POLL_MS, useFormerParties } from "./useFormerParties";
import type { FormerParty, PurgeStatus } from "./types";

vi.mock("./api", () => ({ authenticatedFetch: vi.fn() }));
const fetchMock = vi.mocked(authenticatedFetch);

const PARTY_ID = `old-pool::1220${"d".repeat(64)}`;
const former = (purge_status: PurgeStatus): FormerParty => ({
  party_id: PARTY_ID,
  removed_at: 1_791_000_000,
  detected_at: 1_791_000_060,
  purge_status,
});
const list = (...parties: FormerParty[]) =>
  Promise.resolve(Response.json({ parties }));

beforeEach(() => {
  vi.useFakeTimers();
  fetchMock.mockReset();
});
afterEach(() => {
  vi.useRealTimers();
});

const tick = () =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(PURGE_POLL_MS);
  });

it("polls while a purge runs and stops once none does", async () => {
  fetchMock
    .mockImplementationOnce(() => list(former("running")))
    .mockImplementationOnce(() => list(former("running")))
    .mockImplementation(() => list(former("succeeded")));
  const { result } = renderHook(() => useFormerParties());
  expect(fetchMock).not.toHaveBeenCalled();

  await act(() => result.current.reload());
  expect(fetchMock).toHaveBeenCalledTimes(1);
  expect(fetchMock).toHaveBeenLastCalledWith("/former-parties");
  expect(result.current.formerParties[0].purge_status).toBe("running");

  await tick();
  expect(fetchMock).toHaveBeenCalledTimes(2);
  await tick();
  expect(fetchMock).toHaveBeenCalledTimes(3);
  expect(result.current.formerParties[0].purge_status).toBe("succeeded");

  await act(async () => {
    await vi.advanceTimersByTimeAsync(PURGE_POLL_MS * 5);
  });
  expect(fetchMock).toHaveBeenCalledTimes(3);
});

it("does not poll when nothing is running", async () => {
  fetchMock.mockImplementation(() => list(former("none")));
  const { result } = renderHook(() => useFormerParties());
  await act(() => result.current.reload());
  await act(async () => {
    await vi.advanceTimersByTimeAsync(PURGE_POLL_MS * 5);
  });
  expect(fetchMock).toHaveBeenCalledTimes(1);
  expect(result.current.loaded).toBe(true);
});

it("stops polling on unmount", async () => {
  fetchMock.mockImplementation(() => list(former("running")));
  const { result, unmount } = renderHook(() => useFormerParties());
  await act(() => result.current.reload());
  await tick();
  expect(fetchMock).toHaveBeenCalledTimes(2);
  unmount();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(PURGE_POLL_MS * 5);
  });
  expect(fetchMock).toHaveBeenCalledTimes(2);
});

it("keeps the newest answer when an older one lands after it", async () => {
  let answerOlder!: (response: Response) => void;
  fetchMock
    .mockImplementationOnce(
      () => new Promise<Response>((resolve) => (answerOlder = resolve)),
    )
    .mockImplementationOnce(() => list());
  const { result } = renderHook(() => useFormerParties());
  let older!: Promise<void>;
  act(() => {
    older = result.current.reload();
  });
  await act(() => result.current.reload());
  await act(async () => {
    answerOlder(Response.json({ parties: [former("none")] }));
    await older;
  });
  expect(result.current.formerParties).toEqual([]);
});

it("counts a failed load as loaded and keeps the last list", async () => {
  fetchMock
    .mockImplementationOnce(() => list(former("none")))
    .mockImplementationOnce(() => Promise.reject(new Error("offline")));
  const { result } = renderHook(() => useFormerParties());
  expect(result.current.loaded).toBe(false);
  await act(() => result.current.reload());
  await act(() => result.current.reload());
  expect(result.current.loaded).toBe(true);
  expect(result.current.formerParties.map((p) => p.party_id)).toEqual([PARTY_ID]);
});

it("polls after a purge starts, even when the next read fails", async () => {
  fetchMock
    .mockImplementationOnce(() => list(former("none")))
    .mockImplementationOnce(() => Promise.reject(new Error("offline")))
    .mockImplementation(() => list(former("succeeded")));
  const { result } = renderHook(() => useFormerParties());
  await act(() => result.current.reload());

  act(() => result.current.markPurging(PARTY_ID));
  expect(result.current.formerParties[0].purge_status).toBe("running");

  await tick();
  expect(fetchMock).toHaveBeenCalledTimes(2);
  expect(result.current.formerParties[0].purge_status).toBe("running");
  await tick();
  expect(fetchMock).toHaveBeenCalledTimes(3);
  expect(result.current.formerParties[0].purge_status).toBe("succeeded");
});

it("ignores a read sent before the purge started", async () => {
  let answerOlder!: (response: Response) => void;
  fetchMock
    .mockImplementationOnce(() => list(former("none")))
    .mockImplementationOnce(
      () => new Promise<Response>((resolve) => (answerOlder = resolve)),
    );
  const { result } = renderHook(() => useFormerParties());
  await act(() => result.current.reload());
  let older!: Promise<void>;
  act(() => {
    older = result.current.reload();
  });

  act(() => result.current.markPurging(PARTY_ID));
  await act(async () => {
    answerOlder(Response.json({ parties: [former("none")] }));
    await older;
  });
  expect(result.current.formerParties[0].purge_status).toBe("running");
});
