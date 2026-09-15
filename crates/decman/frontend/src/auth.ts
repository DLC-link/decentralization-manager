const TOKEN_KEY = "dec_party_manager_token";
const REFRESH_TOKEN_KEY = "dec_party_manager_refresh_token";
const ID_TOKEN_KEY = "dec_party_manager_id_token";

export function getToken(): string | null {
  return sessionStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string): void {
  sessionStorage.setItem(TOKEN_KEY, token);
}

export function getRefreshToken(): string | null {
  return sessionStorage.getItem(REFRESH_TOKEN_KEY);
}

export function setRefreshToken(token: string): void {
  sessionStorage.setItem(REFRESH_TOKEN_KEY, token);
}

export function getIdToken(): string | null {
  return sessionStorage.getItem(ID_TOKEN_KEY);
}

export function setIdToken(token: string): void {
  sessionStorage.setItem(ID_TOKEN_KEY, token);
}

/** Renews the access token and returns it, or null if the session is gone. */
type TokenRefresher = () => Promise<string | null>;

let refresher: TokenRefresher | null = null;
let refreshInFlight: Promise<string | null> | null = null;

/**
 * Register how `authenticatedFetch` renews an expired token. Only the auth
 * provider can do it — it owns the Keycloak / Auth0 client.
 */
export function setTokenRefresher(fn: TokenRefresher | null): void {
  refresher = fn;
}

/**
 * Renew the access token, sharing one refresh between concurrent callers:
 * every poller on the page hits its 401 in the same second.
 */
export function refreshAccessToken(): Promise<string | null> {
  if (!refresher) return Promise.resolve(null);
  if (!refreshInFlight) {
    refreshInFlight = refresher().finally(() => {
      refreshInFlight = null;
    });
  }
  return refreshInFlight;
}

export function clearToken(): void {
  sessionStorage.removeItem(TOKEN_KEY);
  sessionStorage.removeItem(REFRESH_TOKEN_KEY);
  sessionStorage.removeItem(ID_TOKEN_KEY);
}
