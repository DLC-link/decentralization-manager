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
let session = 0;

/**
 * Register how `authenticatedFetch` renews an expired token. Only the auth
 * provider can do it — it owns the Keycloak / Auth0 client. Registering
 * (`null` on logout) starts a new session, so a refresh still running for the
 * old one cannot hand its token to the new one.
 */
export function setTokenRefresher(fn: TokenRefresher | null): void {
  refresher = fn;
  session += 1;
  refreshInFlight = null;
}

/**
 * Renew the access token, sharing one refresh between concurrent callers:
 * every poller on the page hits its 401 in the same second.
 */
export function refreshAccessToken(): Promise<string | null> {
  const renew = refresher;
  if (!renew) return Promise.resolve(null);
  if (!refreshInFlight) {
    const mine = session;
    refreshInFlight = renew()
      .catch(() => null)
      .then((token) => {
        if (mine !== session) return null;
        refreshInFlight = null;
        return token;
      });
  }
  return refreshInFlight;
}

export function clearToken(): void {
  sessionStorage.removeItem(TOKEN_KEY);
  sessionStorage.removeItem(REFRESH_TOKEN_KEY);
  sessionStorage.removeItem(ID_TOKEN_KEY);
}
