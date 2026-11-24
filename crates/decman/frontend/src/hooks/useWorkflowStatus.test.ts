import { renderHook, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { WorkflowStatusResponse } from "../types";
import { useWorkflowStatus } from "./useWorkflowStatus";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const answer = (status: string) =>
  new Response(JSON.stringify({ status }), { status: 200 });

/**
 * Render the hook with a dialog status and a stubbed `fetch`. The interval is
 * captured instead of scheduled, so a test runs each tick by hand. Testing
 * Library's `waitFor` polls with `setInterval`, so the tests use `vi.waitFor`.
 */
const renderPolling = (
  status: WorkflowStatusResponse | null,
  fetchMock: ReturnType<typeof vi.fn>,
) => {
  vi.stubGlobal("fetch", fetchMock);
  const ticks: { fn: () => unknown; ms: number | undefined }[] = [];
  vi.spyOn(window, "setInterval").mockImplementation(((
    fn: () => unknown,
    ms?: number,
  ) => {
    ticks.push({ fn, ms });
    return ticks.length;
  }) as typeof window.setInterval);
  const props = {
    path: "/kick/status",
    cancelledMessage: "Kick workflow cancelled",
    status,
    setStatus: vi.fn(),
    setLoading: vi.fn(),
    onComplete: vi.fn(),
    onClose: vi.fn(),
  };
  const view = renderHook((p) => useWorkflowStatus(p), {
    initialProps: props,
    wrapper: SnackbarProvider,
  });
  return { ...props, ticks, view };
};

describe("useWorkflowStatus", () => {
  it("does not poll while the workflow is not in progress", () => {
    const fetchMock = vi.fn();
    const { ticks } = renderPolling({ status: "completed" }, fetchMock);

    expect(fetchMock).not.toHaveBeenCalled();
    expect(ticks).toHaveLength(0);
  });

  it("polls at once, then every two seconds", async () => {
    const fetchMock = vi
      .fn()
      .mockImplementation(async () => answer("inprogress"));
    const { ticks, setStatus, setLoading, onComplete } = renderPolling(
      { status: "inprogress" },
      fetchMock,
    );

    await vi.waitFor(() => expect(setStatus).toHaveBeenCalledTimes(1));
    expect(fetchMock.mock.calls[0][0]).toBe("/kick/status");
    expect(ticks).toHaveLength(1);
    expect(ticks[0].ms).toBe(2000);

    ticks[0].fn();
    await vi.waitFor(() => expect(setStatus).toHaveBeenCalledTimes(2));
    expect(setStatus).toHaveBeenLastCalledWith({ status: "inprogress" });
    expect(setLoading).not.toHaveBeenCalled();
    expect(onComplete).not.toHaveBeenCalled();
  });

  it("shows the message and closes when the workflow was cancelled", async () => {
    const { setStatus, setLoading, onClose, onComplete } = renderPolling(
      { status: "inprogress" },
      vi.fn().mockResolvedValue(answer("cancelled")),
    );

    expect(await screen.findByText("Kick workflow cancelled")).toBeTruthy();
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(setStatus).not.toHaveBeenCalled();
    expect(setLoading).not.toHaveBeenCalled();
    expect(onComplete).not.toHaveBeenCalled();
  });

  it("stores a completed status, stops loading and reports completion", async () => {
    const { setStatus, setLoading, onComplete, onClose } = renderPolling(
      { status: "inprogress" },
      vi.fn().mockResolvedValue(answer("completed")),
    );

    await vi.waitFor(() => expect(onComplete).toHaveBeenCalledTimes(1));
    expect(setStatus).toHaveBeenCalledWith({ status: "completed" });
    expect(setLoading).toHaveBeenCalledWith(false);
    expect(onClose).not.toHaveBeenCalled();
  });

  it("stores a failed status and stops loading without completing", async () => {
    const { setStatus, setLoading, onComplete, onClose } = renderPolling(
      { status: "inprogress" },
      vi.fn().mockResolvedValue(answer("failed")),
    );

    await vi.waitFor(() => expect(setLoading).toHaveBeenCalledWith(false));
    expect(setStatus).toHaveBeenCalledWith({ status: "failed" });
    expect(onComplete).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("ignores a failed request and a non-ok answer", async () => {
    const fetchMock = vi
      .fn()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce(new Response("{}", { status: 500 }));
    const { ticks, setStatus, setLoading, onClose } = renderPolling(
      { status: "inprogress" },
      fetchMock,
    );

    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    // The tick returns the poll, so awaiting it waits for the whole answer.
    await ticks[0].fn();

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(setStatus).not.toHaveBeenCalled();
    expect(setLoading).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("stops polling once the status leaves in progress", () => {
    const clearSpy = vi.spyOn(globalThis, "clearInterval");
    const { ticks, view, ...props } = renderPolling(
      { status: "inprogress" },
      vi.fn().mockResolvedValue(answer("inprogress")),
    );
    expect(ticks).toHaveLength(1);

    view.rerender({ ...props, status: { status: "completed" } });

    expect(clearSpy).toHaveBeenCalledWith(1);
    expect(ticks).toHaveLength(1);
  });
});
