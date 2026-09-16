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

/**
 * Outcome of a renewal. `stale` is not a failure: the session was replaced
 * while the renewal ran, so the answer belongs to nobody and the caller must
 * leave the new session alone rather than log it out.
 */
export type TokenRenewal =
  | { status: "renewed"; token: string }
  | { status: "failed" }
  | { status: "stale" };

let refresher: TokenRefresher | null = null;
let refreshInFlight: Promise<TokenRenewal> | null = null;
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
export function currentSession(): number {
  return session;
}

export function refreshAccessToken(
  forSession: number = session,
): Promise<TokenRenewal> {
  // The caller asked on behalf of a session that has since been replaced, so
  // renewing now would hand the new user's token to the old user's request.
  if (forSession !== session) return Promise.resolve({ status: "stale" });
  const renew = refresher;
  if (!renew) return Promise.resolve({ status: "failed" });
  if (!refreshInFlight) {
    const mine = session;
    refreshInFlight = renew()
      .catch(() => null)
      .then((token): TokenRenewal => {
        if (mine !== session) return { status: "stale" };
        refreshInFlight = null;
        return token ? { status: "renewed", token } : { status: "failed" };
      });
  }
  return refreshInFlight;
}

export function clearToken(): void {
  sessionStorage.removeItem(TOKEN_KEY);
  sessionStorage.removeItem(REFRESH_TOKEN_KEY);
  sessionStorage.removeItem(ID_TOKEN_KEY);
}
