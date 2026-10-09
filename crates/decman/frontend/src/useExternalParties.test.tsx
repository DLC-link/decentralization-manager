import { act, render, renderHook, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { authenticatedFetch } from "./api";
import { useExternalParties } from "./useExternalParties";
import { SnackbarProvider } from "./contexts/SnackbarContext";
import { ExternalPartyList } from "./components/ExternalPartyList";
import type { ExternalPartyInfo } from "./types";

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
const NOW = new Date("2026-10-05T10:05:00Z");
const FETCHED_AT = "2026-10-05T10:00:00Z";
const WALLET: ExternalPartyInfo = {
  party_id: "wallet::key",
  fingerprint: "key",
  threshold: 1,
  host_count: 1,
  created_at: undefined,
  onboarding: false,
  hosts: [{ participant_uid: "node::namespace", permission: "confirmation" }],
};
const snapshot = (parties: ExternalPartyInfo[] = [], extra = {}) =>
  Response.json({ parties, fetched_at: FETCHED_AT, refreshing: false, ...extra });
const pending = () => Response.json({ parties: [], refreshing: true });

beforeEach(() => {
  vi.useFakeTimers({ now: NOW });
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
    resolve(snapshot());
  });
  expect(screen.getByText(empty)).toBeTruthy();
  expect(screen.getByText("updated 5m ago")).toBeTruthy();
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
  fetchMock.mockImplementationOnce(failure).mockResolvedValue(snapshot());
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

it("shows progress, not an empty list or an error, while the first scan runs", async () => {
  fetchMock.mockResolvedValueOnce(pending()).mockResolvedValueOnce(snapshot([WALLET]));
  await act(async () => {
    render(<View />);
  });
  expect(screen.getByLabelText("Loading external parties")).toBeTruthy();
  expect(screen.getByText(/first scan after a restart/)).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByText(empty)).toBeNull();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10_000);
  });
  expect(screen.queryByLabelText("Loading external parties")).toBeNull();
  expect(screen.getByTestId("external-party-row").textContent).toContain("wallet");
  expect(screen.getByText("Live")).toBeTruthy();
});

it("keeps showing the last good list, with a warning, when a refresh fails", async () => {
  fetchMock.mockResolvedValue(
    snapshot([WALLET], { refreshing: true, refresh_error: "Canton unavailable" }),
  );
  await act(async () => {
    render(<View />);
  });
  expect(screen.getByTestId("external-party-row").textContent).toContain("wallet");
  const warning = screen.getByRole("alert");
  expect(warning.textContent).toContain("earlier scan");
  expect(warning.textContent).toContain("Canton unavailable");
  expect(screen.getByText("updated 5m ago · refreshing")).toBeTruthy();
});

it("polls only while the tab is active", async () => {
  fetchMock.mockImplementation(() => Promise.resolve(snapshot()));
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
