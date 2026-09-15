import { afterEach, describe, expect, it, vi } from "vitest";
import { authenticatedFetch } from "./api";
import {
  getToken,
  refreshAccessToken,
  setToken,
  setTokenRefresher,
} from "./auth";

/**
 * jsdom's `location.reload` is "not implemented", and its own property cannot
 * be redefined (`vi.spyOn` on it throws "Cannot redefine property"), so swap
 * the whole `location` for the duration of a test.
 */
const realLocation = Object.getOwnPropertyDescriptor(window, "location");
const stubReload = () => {
  const reload = vi.fn();
  Object.defineProperty(window, "location", {
    value: { ...window.location, reload },
    configurable: true,
    writable: true,
  });
  return reload;
};

afterEach(() => {
  if (realLocation) Object.defineProperty(window, "location", realLocation);
  setTokenRefresher(null);
  sessionStorage.clear();
  vi.restoreAllMocks();
});

const bearerOf = (call: unknown[]) =>
  new Headers((call[1] as RequestInit).headers).get("Authorization");

describe("authenticatedFetch", () => {
  it("renews the token and retries once when a request outlives it", async () => {
    setToken("stale");
    setTokenRefresher(async () => {
      setToken("fresh");
      return "fresh";
    });
    const reload = stubReload();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(new Response(null, { status: 401 }))
      .mockResolvedValueOnce(new Response('{"runs":[]}', { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);

    const res = await authenticatedFetch("/workflows");

    expect(res.status).toBe(200);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(bearerOf(fetchMock.mock.calls[0])).toBe("Bearer stale");
    expect(bearerOf(fetchMock.mock.calls[1])).toBe("Bearer fresh");
    // The session survives: no wipe, no bounce to the login page.
    expect(getToken()).toBe("fresh");
    expect(reload).not.toHaveBeenCalled();
  });

  it("drops the session when the renewal fails", async () => {
    setToken("stale");
    setTokenRefresher(async () => null);
    const reload = stubReload();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(null, { status: 401 }));
    vi.stubGlobal("fetch", fetchMock);

    const res = await authenticatedFetch("/workflows");

    expect(res.status).toBe(401);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(getToken()).toBeNull();
    expect(reload).toHaveBeenCalled();
  });

  it("renews once for the pollers that all 401 in the same second", async () => {
    setToken("stale");
    const refresher = vi.fn(async () => {
      setToken("fresh");
      return "fresh";
    });
    setTokenRefresher(refresher);
    stubReload();
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) =>
      new Headers(init?.headers).get("Authorization") === "Bearer fresh"
        ? new Response(null, { status: 200 })
        : new Response(null, { status: 401 }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const results = await Promise.all([
      authenticatedFetch("/workflows"),
      authenticatedFetch("/invitations"),
      authenticatedFetch("/participants-status"),
    ]);

    expect(results.map((r) => r.status)).toEqual([200, 200, 200]);
    expect(refresher).toHaveBeenCalledTimes(1);
  });

  it("starts a new renewal after the shared one settles", async () => {
    const refresher = vi.fn(async () => "fresh");
    setTokenRefresher(refresher);

    await refreshAccessToken();
    await refreshAccessToken();

    expect(refresher).toHaveBeenCalledTimes(2);
  });

  it("drops a renewal that lands after the user logged out", async () => {
    let finish: (token: string | null) => void = () => {};
    setTokenRefresher(() => new Promise<string | null>((r) => (finish = r)));
    const pending = refreshAccessToken();

    // Logout unregisters the refresher while the renewal is still running.
    setTokenRefresher(null);
    finish("previous-session-token");

    await expect(pending).resolves.toBeNull();
  });

  it("leaves an unauthenticated 401 alone", async () => {
    const reload = stubReload();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(null, { status: 401 }));
    vi.stubGlobal("fetch", fetchMock);

    const res = await authenticatedFetch("/workflows");

    expect(res.status).toBe(401);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(reload).not.toHaveBeenCalled();
  });
});
