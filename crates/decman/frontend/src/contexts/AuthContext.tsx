import {
  useEffect,
  useRef,
  useState,
  useCallback,
  type ReactNode,
} from "react";
import Keycloak from "keycloak-js";
import { useAuth0 } from "@auth0/auth0-react";
import {
  getToken,
  setToken,
  getRefreshToken,
  setRefreshToken,
  getIdToken,
  setIdToken,
  clearToken,
} from "../auth";
import { LoginPage } from "../components/LoginPage";
import type { AuthConfig } from "../types";
import { AuthContext } from "./AuthContextValue";

function AuthenticatingScreen() {
  return (
    <div
      style={{
        display: "flex",
        justifyContent: "center",
        alignItems: "center",
        height: "100vh",
        fontFamily: "sans-serif",
      }}
    >
      Authenticating...
    </div>
  );
}

function KeycloakAuthProvider({
  config,
  children,
}: {
  config: AuthConfig;
  children: ReactNode;
}) {
  const [token, setTokenState] = useState<string | null>(getToken());
  const [keycloak, setKeycloak] = useState<Keycloak | null>(null);
  const [loading, setLoading] = useState(true);
  const initStarted = useRef(false);
  const refreshTimer = useRef<ReturnType<typeof setTimeout>>(undefined);

  useEffect(() => {
    if (initStarted.current) return;
    initStarted.current = true;

    async function init() {
      try {
        const url = config.keycloak_host!.replace(/\/+$/, "");

        const kc = new Keycloak({
          url,
          realm: config.keycloak_realm!,
          clientId: config.keycloak_client_id!,
        });

        setKeycloak(kc);

        // Save the original hash — Keycloak's OAuth callback processing
        // strips OAuth params from the URL and may clobber our app route.
        const savedHash = (window as { __INITIAL_HASH__?: string })
          .__INITIAL_HASH__ ?? "";
        const cleanHash = savedHash.replace(
          /[&?#](state|session_state|iss|code)=.*/i,
          "",
        );

        // Rehydrate persisted tokens across reloads. kc.init accepts token /
        // refreshToken / idToken and will validate the access token or refresh
        // it via the refresh token — no iframe or third-party cookies needed.
        const savedAccessToken = getToken();
        const savedRefreshToken = getRefreshToken();
        const savedIdToken = getIdToken();

        const authenticated = await kc.init({
          checkLoginIframe: false,
          ...(savedAccessToken && savedRefreshToken
            ? {
                token: savedAccessToken,
                refreshToken: savedRefreshToken,
                idToken: savedIdToken ?? undefined,
              }
            : {}),
        });

        // Restore the original app route after Keycloak's URL cleanup.
        if (cleanHash) {
          window.history.replaceState(null, "", cleanHash);
        }

        if (authenticated && kc.token) {
          setToken(kc.token);
          if (kc.refreshToken) setRefreshToken(kc.refreshToken);
          if (kc.idToken) setIdToken(kc.idToken);
          setTokenState(kc.token);

          function scheduleRefresh() {
            const exp = kc.tokenParsed?.exp;
            if (!exp) return;
            const msUntilRefresh = Math.max(
              exp * 1000 - Date.now() - 60_000,
              10_000,
            );
            refreshTimer.current = setTimeout(() => {
              kc.updateToken(60)
                .then((refreshed: boolean) => {
                  if (refreshed && kc.token) {
                    setToken(kc.token);
                    if (kc.refreshToken) setRefreshToken(kc.refreshToken);
                    if (kc.idToken) setIdToken(kc.idToken);
                    setTokenState(kc.token);
                  }
                  scheduleRefresh();
                })
                .catch(() => {
                  clearToken();
                  setTokenState(null);
                });
            }, msUntilRefresh);
          }
          scheduleRefresh();
        } else {
          clearToken();
          setTokenState(null);
        }
      } catch (err) {
        console.error("Keycloak init failed:", err);
        // Saved tokens may be invalid (expired refresh token, revoked
        // session, etc). Wipe and fall through to the LoginPage rather
        // than keeping the user stuck.
        clearToken();
        setTokenState(null);
      } finally {
        setLoading(false);
      }
    }

    init();
    return () => clearTimeout(refreshTimer.current);
  }, [config]);

  const logout = useCallback(() => {
    clearToken();
    setTokenState(null);
    if (keycloak) {
      keycloak.logout({ redirectUri: window.location.origin });
    }
  }, [keycloak]);

  const login = useCallback(() => {
    keycloak?.login();
  }, [keycloak]);

  if (loading) return <AuthenticatingScreen />;
  if (!token) return <LoginPage onLogin={login} />;

  return (
    <AuthContext.Provider value={{ token, logout }}>
      {children}
    </AuthContext.Provider>
  );
}

function Auth0AuthProvider({ children }: { children: ReactNode }) {
  const {
    isAuthenticated,
    isLoading,
    loginWithRedirect,
    logout: auth0Logout,
    getAccessTokenSilently,
  } = useAuth0();
  const [token, setTokenState] = useState<string | null>(getToken());
  const [tokenLoading, setTokenLoading] = useState(true);

  useEffect(() => {
    if (isLoading) return;

    if (!isAuthenticated) {
      clearToken();
      setTokenState(null);
      setTokenLoading(false);
      return;
    }

    getAccessTokenSilently()
      .then((t) => {
        setToken(t);
        setTokenState(t);
      })
      .catch((err) => {
        console.error("Auth0 token retrieval failed:", err);
        clearToken();
        setTokenState(null);
      })
      .finally(() => setTokenLoading(false));
  }, [isAuthenticated, isLoading, getAccessTokenSilently]);

  const logout = useCallback(() => {
    clearToken();
    setTokenState(null);
    auth0Logout({ logoutParams: { returnTo: window.location.origin } });
  }, [auth0Logout]);

  const login = useCallback(() => {
    loginWithRedirect();
  }, [loginWithRedirect]);

  if (isLoading || tokenLoading) return <AuthenticatingScreen />;
  if (!token) return <LoginPage onLogin={login} />;

  return (
    <AuthContext.Provider value={{ token, logout }}>
      {children}
    </AuthContext.Provider>
  );
}

// Dev-only (mock mode): a fake, toggleable login session so the full
// authenticated UI (logout, the login screen) is previewable offline. The
// `__MOCK__` guard is a compile-time constant, so this is dead-code-eliminated
// from production builds.
function MockAuthProvider({ children }: { children: ReactNode }) {
  const [token, setTokenState] = useState<string | null>("mock-session");
  const logout = useCallback(() => setTokenState(null), []);
  const login = useCallback(() => setTokenState("mock-session"), []);

  if (!token) return <LoginPage onLogin={login} />;

  return (
    <AuthContext.Provider value={{ token, logout }}>
      {children}
    </AuthContext.Provider>
  );
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [config, setConfig] = useState<AuthConfig | null>(null);

  useEffect(() => {
    if (__MOCK__) return;
    fetch("/auth-config")
      .then((res) => res.json())
      .then((c: AuthConfig) => setConfig(c))
      .catch(() => setConfig({ auth_required: false }));
  }, []);

  if (__MOCK__) return <MockAuthProvider>{children}</MockAuthProvider>;

  if (!config) return <AuthenticatingScreen />;

  if (!config.auth_required) {
    return (
      <AuthContext.Provider value={{ token: null, logout: () => {} }}>
        {children}
      </AuthContext.Provider>
    );
  }

  if (config.auth0_domain && config.auth0_client_id) {
    return <Auth0AuthProvider>{children}</Auth0AuthProvider>;
  }

  return (
    <KeycloakAuthProvider config={config}>{children}</KeycloakAuthProvider>
  );
}
