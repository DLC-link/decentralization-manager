import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useState,
  useCallback,
  type ReactNode,
} from "react";
import Keycloak from "keycloak-js";
import { getToken, setToken, clearToken } from "../auth";
import { LoginPage } from "../components/LoginPage";
import type { AuthConfig } from "../types";

interface AuthContextValue {
  token: string | null;
  logout: () => void;
}

const AuthContext = createContext<AuthContextValue | null>(null);

export function useAuth(): AuthContextValue {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error("useAuth must be used within AuthProvider");
  return ctx;
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [token, setTokenState] = useState<string | null>(getToken());
  const [keycloak, setKeycloak] = useState<Keycloak | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [authDisabled, setAuthDisabled] = useState(false);
  const initStarted = useRef(false);
  const refreshTimer = useRef<ReturnType<typeof setTimeout>>(undefined);

  useEffect(() => {
    if (initStarted.current) return;
    initStarted.current = true;

    async function init() {
      try {
        const res = await fetch("/auth-config");
        const config: AuthConfig = await res.json();

        if (!config.auth_required) {
          setAuthDisabled(true);
          setLoading(false);
          return;
        }

        const url = config.keycloak_host!.replace(/\/+$/, "");

        const kc = new Keycloak({
          url,
          realm: config.keycloak_realm!,
          clientId: config.keycloak_client_id!,
        });

        setKeycloak(kc);

        const authenticated = await kc.init({
          onLoad: "check-sso",
          checkLoginIframe: false,
        });

        if (authenticated && kc.token) {
          setToken(kc.token);
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
        setError(err instanceof Error ? err.message : "Authentication failed");
      } finally {
        setLoading(false);
      }
    }

    init();
    return () => clearTimeout(refreshTimer.current);
  }, []);

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

  if (authDisabled) {
    return (
      <AuthContext.Provider value={{ token: null, logout: () => {} }}>
        {children}
      </AuthContext.Provider>
    );
  }

  if (loading) {
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

  if (!token) {
    return <LoginPage onLogin={login} error={error} />;
  }

  return (
    <AuthContext.Provider value={{ token, logout }}>
      {children}
    </AuthContext.Provider>
  );
}
