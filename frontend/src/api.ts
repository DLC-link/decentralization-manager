import { clearToken, getToken } from "./auth";

/**
 * Wrapper around fetch() that attaches the Bearer token from sessionStorage.
 * Drop-in replacement for fetch(). On 401 we drop the stale session and
 * reload so the AuthProvider kicks the user back to Keycloak login.
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
  if (response.status === 401) {
    clearToken();
    window.location.reload();
  }
  return response;
}
