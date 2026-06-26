import { clearToken, getToken } from "./auth";

/**
 * Wrapper around fetch() that attaches the Bearer token from sessionStorage.
 * Drop-in replacement for fetch(). On 401 with a stale token we drop it and
 * reload so the AuthProvider kicks the user back to Keycloak login.
 *
 * The reload is gated on having had a token: a 401 with no token in the
 * first place means the backend rejected an unauthenticated request, which
 * a reload won't fix and would just produce an infinite refresh loop if the
 * backend itself is misconfigured.
 */
export async function authenticatedFetch(
  input: RequestInfo | URL,
  init?: RequestInit,
): Promise<Response> {
  const headers = new Headers(init?.headers);
  const token = getToken();
  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }
  const response = await fetch(input, { ...init, headers });
  if (token && response.status === 401) {
    clearToken();
    window.location.reload();
  }
  return response;
}

/**
 * Measure round-trip latency to a backend endpoint by timing GET requests.
 * Pings `samples` times and returns the smallest result in ms — the minimum
 * is the least distorted by client-side jitter (GC, render work), the same
 * reason `ping` reports min/avg/max. Returns null if any request fails.
 *
 * Point this at a no-auth, no-I/O endpoint (`/healthz`) so the number reflects
 * transport + handler overhead rather than backend work. `cache: "no-store"`
 * stops the browser from serving a cached 200 (which would always read ~0 ms).
 *
 * Each sample is bounded by `timeoutMs` via an `AbortController` so a stalled
 * connection can't hang the ping (and pile up behind the poll interval) — a
 * timed-out sample fails the whole measurement (returns null).
 */
export async function pingLatency(
  path: string,
  samples = 3,
  timeoutMs = 5000,
): Promise<number | null> {
  let best: number | null = null;
  for (let i = 0; i < samples; i++) {
    const controller = new AbortController();
    const timer = window.setTimeout(() => controller.abort(), timeoutMs);
    const start = performance.now();
    try {
      const res = await fetch(path, {
        cache: "no-store",
        signal: controller.signal,
      });
      if (!res.ok) return null;
      // Drain the body so timing covers the full response, not just headers.
      await res.arrayBuffer();
    } catch {
      return null;
    } finally {
      window.clearTimeout(timer);
    }
    const elapsed = performance.now() - start;
    if (best === null || elapsed < best) best = elapsed;
  }
  return best === null ? null : Math.round(best);
}
