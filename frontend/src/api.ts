import { getToken } from "./auth";

/**
 * Wrapper around fetch() that attaches the Bearer token from sessionStorage.
 * Drop-in replacement for fetch().
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
  return fetch(input, { ...init, headers });
}
