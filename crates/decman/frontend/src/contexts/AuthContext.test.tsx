import { StrictMode } from "react";
import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { AuthProvider } from "./AuthContext";
import { refreshAccessToken, setTokenRefresher } from "../auth";

// The identity provider is the external boundary, so it is the only thing
// stood in for here. Everything else is the real provider.
const updateToken = vi.fn(async () => true);
vi.mock("keycloak-js", () => ({
  default: class {
    token = "kc-token";
    refreshToken = "kc-refresh";
    idToken = "kc-id";
    tokenParsed = { exp: Math.floor(Date.now() / 1000) + 300 };
    init = async () => true;
    updateToken = updateToken;
    logout = vi.fn();
    login = vi.fn();
  },
}));

const AUTH_CONFIG = {
  auth_required: true,
  keycloak_host: "https://keycloak.example",
  keycloak_realm: "realm",
  keycloak_client_id: "dec-party-manager",
};

beforeEach(() => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(JSON.stringify(AUTH_CONFIG))),
  );
});

afterEach(() => {
  setTokenRefresher(null);
  sessionStorage.clear();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("KeycloakAuthProvider under StrictMode", () => {
  it("unregisters the refresher when it really unmounts", async () => {
    const { unmount } = render(
      <StrictMode>
        <AuthProvider>
          <div>signed in</div>
        </AuthProvider>
      </StrictMode>,
    );

    await screen.findByText("signed in");
    // The session is live: a 401 here would be renewed, not logged out.
    await expect(refreshAccessToken()).resolves.toEqual({
      status: "renewed",
      token: "kc-token",
    });

    unmount();

    // StrictMode replayed the setup, so the cleanup React holds is the one the
    // replay returned. Without it the refresher outlives the provider and can
    // write this session's token over whatever mounts next.
    await waitFor(() =>
      expect(refreshAccessToken()).resolves.toEqual({ status: "failed" }),
    );
  });
});
