import { act, render, renderHook, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { authenticatedFetch } from "./api";
import { useExternalParties } from "./useExternalParties";
import { SnackbarProvider } from "./contexts/SnackbarContext";
import { ExternalPartyList } from "./components/ExternalPartyList";

vi.mock("./api", () => ({ authenticatedFetch: vi.fn() }));
const fetchMock = vi.mocked(authenticatedFetch);
const View = () => {
  const state = useExternalParties(true);
  return (
    <SnackbarProvider>
      <ExternalPartyList {...state} />
    </SnackbarProvider>
  );
};
const empty = "No external parties hosted on this node";

beforeEach(() => {
  vi.useFakeTimers();
  fetchMock.mockReset();
});
afterEach(() => {
  vi.useRealTimers();
});

it("shows loading until the first snapshot arrives", async () => {
  let resolve!: (response: Response) => void;
  fetchMock.mockReturnValue(new Promise<Response>((r) => {
    resolve = r;
  }));
  render(<View />);
  expect(screen.queryByText(empty)).toBeNull();
  expect(screen.getByLabelText("Loading external parties")).toBeTruthy();
  await act(async () => {
    resolve(Response.json({ parties: [] }));
  });
  expect(screen.getByText(empty)).toBeTruthy();
});

it.each([
  [
    "HTTP error",
    () => Promise.resolve(Response.json({ error: "Discovery failed" }, { status: 503 })),
    "Discovery failed",
  ],
  [
    "network error",
    () => Promise.reject(new Error("Network unavailable")),
    "Network unavailable",
  ],
  [
    "proxy error",
    () => Promise.resolve(new Response("Bad gateway", { status: 502 })),
    "HTTP 502",
  ],
] as const)("shows %s and recovers on the next poll", async (_name, failure, message) => {
  fetchMock.mockImplementationOnce(failure).mockResolvedValue(Response.json({ parties: [] }));
  await act(async () => {
    render(<View />);
  });
  expect(screen.getByRole("alert").textContent).toContain(message);
  expect(screen.queryByText(empty)).toBeNull();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10_000);
  });
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.getByText(empty)).toBeTruthy();
});

it("displays a hosted party when background discovery finishes", async () => {
  fetchMock
    .mockResolvedValueOnce(
      Response.json({ error: "Discovery in progress" }, { status: 503 }),
    )
    .mockResolvedValueOnce(
      Response.json({
        parties: [{
          party_id: "wallet::key",
          fingerprint: "key",
          threshold: 1,
          host_count: 1,
          created_at: null,
          onboarding: false,
          hosts: [{ participant_uid: "node::namespace", permission: "Confirmation" }],
        }],
      }),
    );
  await act(async () => {
    render(<View />);
  });
  expect(screen.getByRole("alert")).toBeTruthy();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10_000);
  });
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.getByTestId("external-party-row").textContent).toContain("wallet");
  expect(screen.getByText("Live")).toBeTruthy();
});

it("polls only while the tab is active", async () => {
  fetchMock.mockImplementation(() => Promise.resolve(Response.json({ parties: [] })));
  const { rerender, unmount } = renderHook(({ active }) => useExternalParties(active), {
    initialProps: { active: false },
  });
  expect(fetchMock).not.toHaveBeenCalled();
  await act(async () => {
    rerender({ active: true });
  });
  expect(fetchMock).toHaveBeenCalledTimes(1);
  rerender({ active: false });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(20_000);
  });
  expect(fetchMock).toHaveBeenCalledTimes(1);
  await act(async () => {
    rerender({ active: true });
  });
  expect(fetchMock).toHaveBeenCalledTimes(2);
  unmount();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(20_000);
  });
  expect(fetchMock).toHaveBeenCalledTimes(2);
});
