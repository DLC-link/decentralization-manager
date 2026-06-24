import type { BrowserContext } from "@playwright/test";

export type AuthConfig = {
  keycloak_host: string;
  keycloak_realm: string;
  keycloak_client_id: string;
  auth_required: boolean;
};

const USERNAME = process.env.E2E_KC_USERNAME ?? "cvault-finoa-lp-1";
const PASSWORD = process.env.E2E_KC_PASSWORD;

export async function getAuthConfig(port: number): Promise<AuthConfig> {
  const res = await fetch(`http://localhost:${port}/auth-config`);
  if (!res.ok) throw new Error(`/auth-config on :${port} returned ${res.status}`);
  return res.json();
}

function tokenUrl(cfg: AuthConfig): string {
  // Mirror src/auth/mod.rs / tests/common/auth.rs: tolerate trailing /auth.
  const base = cfg.keycloak_host.replace(/\/+$/, "").replace(/\/auth$/, "");
  return `${base}/auth/realms/${cfg.keycloak_realm}/protocol/openid-connect/token`;
}

export async function fetchRopcTokens(cfg: AuthConfig) {
  if (!PASSWORD) throw new Error("E2E_KC_PASSWORD is not set");
  const body = new URLSearchParams({
    grant_type: "password",
    client_id: cfg.keycloak_client_id,
    username: USERNAME,
    password: PASSWORD,
  });
  const res = await fetch(tokenUrl(cfg), {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body,
  });
  if (!res.ok) {
    throw new Error(`ROPC token request failed (${res.status}); is Direct Access Grants enabled for ${cfg.keycloak_client_id}?`);
  }
  const j = await res.json();
  return {
    access_token: j.access_token as string,
    refresh_token: j.refresh_token as string,
    id_token: j.id_token as string | undefined,
  };
}

export async function seedAuth(
  context: BrowserContext,
  tokens: { access_token: string; refresh_token: string; id_token?: string },
) {
  // Runs before any page script on every page in the context.
  await context.addInitScript((t) => {
    sessionStorage.setItem("dec_party_manager_token", t.access_token);
    sessionStorage.setItem("dec_party_manager_refresh_token", t.refresh_token);
    if (t.id_token) sessionStorage.setItem("dec_party_manager_id_token", t.id_token);
  }, tokens);
}
