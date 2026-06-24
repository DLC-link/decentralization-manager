import "@fontsource/space-grotesk/300.css";
import "@fontsource/space-grotesk/400.css";
import "@fontsource/space-grotesk/500.css";
import "@fontsource/space-grotesk/600.css";
import "@fontsource/space-grotesk/700.css";

import { StrictMode, useEffect, useState, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { Auth0Provider } from "@auth0/auth0-react";

import { AuthProvider, ThemeProvider, SnackbarProvider } from "./contexts";
import type { AuthConfig } from "./types";
import "./index.css";
import App from "./App.tsx";

// Wraps the tree in Auth0Provider only when the backend reports Auth0
// credentials via `/auth-config`. Operators choose Auth0 vs Keycloak with
// DECPM_AUTH0_* vs DECPM_KEYCLOAK_* env vars at deploy time, so the SDK
// must read them from the backend rather than baked-in build-time values.
function Auth0Bootstrap({ children }: { children: ReactNode }) {
  const [config, setConfig] = useState<AuthConfig | null>(null);

  useEffect(() => {
    fetch("/auth-config")
      .then((res) => res.json())
      .then((c: AuthConfig) => setConfig(c))
      .catch(() => setConfig({ auth_required: false }));
  }, []);

  if (!config) return null;

  if (config.auth0_domain && config.auth0_client_id) {
    return (
      <Auth0Provider
        domain={config.auth0_domain}
        clientId={config.auth0_client_id}
        authorizationParams={{
          redirect_uri: window.location.origin,
          ...(config.auth0_audience
            ? { audience: config.auth0_audience }
            : {}),
        }}
      >
        {children}
      </Auth0Provider>
    );
  }

  return <>{children}</>;
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Auth0Bootstrap>
      <ThemeProvider>
        <AuthProvider>
          <SnackbarProvider>
            <App />
          </SnackbarProvider>
        </AuthProvider>
      </ThemeProvider>
    </Auth0Bootstrap>
  </StrictMode>,
);
