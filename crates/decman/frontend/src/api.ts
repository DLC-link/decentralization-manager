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
