import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { NodeIdentityStatusResponse, StoredNodeIdentity } from "../types";
import { NodeIdentityCard } from "./NodeIdentityCard";

const NS = `1220${"a".repeat(64)}`;
const OTHER_NS = `1220${"b".repeat(64)}`;

const STATUS: NodeIdentityStatusResponse = {
  self: {
    participant_id: `participant1::${NS}`,
    name: "",
    node_party_id: `decman-node-participant1::${NS}`,
    found: true,
    namespace_matches: true,
    hosting_permission: "submission",
    verified: true,
  },
  peers: [
    {
      participant_id: `participant2::${OTHER_NS}`,
      name: "Node 2",
      node_party_id: `decman-node-participant2::${OTHER_NS}`,
      found: false,
      namespace_matches: true,
      verified: false,
    },
  ],
  scan_filter: "decman-node-",
  scan: [`decman-node-participant1::${NS}`],
};

const IDENTITY: StoredNodeIdentity = {
  node_party_id: `decman-node-participant1::${NS}`,
  participant_id: `participant1::${NS}`,
  participant_matches: true,
  user_id: "node-user",
  login: {
    idp: "keycloak",
    client_id: "node-client",
    keycloak_url: "https://kc.example",
    keycloak_realm: "canton",
  },
  token: { status: "ok" },
  updated_at: 1_790_000_000,
};

const renderCard = (status: NodeIdentityStatusResponse | null, error: string | null = null) =>
  render(
    <SnackbarProvider>
      <NodeIdentityCard status={status} error={error} loading={false} onRefresh={() => {}} />
    </SnackbarProvider>,
  );

describe("NodeIdentityCard", () => {
  it("offers set-up when no identity is stored", () => {
    renderCard(STATUS);

    expect(screen.getByText("not set up")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Set up" })).toBeTruthy();
  });

  it("shows the stored login and its user", () => {
    renderCard({ ...STATUS, identity: IDENTITY });

    expect(screen.getByText("login ok")).toBeTruthy();
    expect(screen.getByText("node-user")).toBeTruthy();
    expect(screen.getByText("Keycloak · node-client · https://kc.example · canton")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Change login" })).toBeTruthy();
  });

  it("names the unsafe token in insecure mode", () => {
    renderCard({
      ...STATUS,
      identity: { ...IDENTITY, login: undefined, token: { status: "mock" } },
    });

    expect(screen.getByText("unsafe token")).toBeTruthy();
    expect(screen.getByText("Unsafe token (insecure mode)")).toBeTruthy();
  });

  it("shows why the login failed", () => {
    const error = "The Keycloak node login failed. The decman log has the IdP's answer";
    renderCard({ ...STATUS, identity: { ...IDENTITY, token: { status: "failed", error } } });

    expect(screen.getByText("login failed")).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toBe(error);
  });

  it("warns when the identity belongs to another participant", () => {
    renderCard({ ...STATUS, identity: { ...IDENTITY, participant_matches: false } });

    expect(screen.getByRole("alert").textContent).toContain("Set it up again");
  });

  it("checks this node and each peer by exact party", () => {
    renderCard(STATUS);

    expect(screen.getByText("this node")).toBeTruthy();
    expect(screen.getByText("verified")).toBeTruthy();
    expect(screen.getByText("Node 2")).toBeTruthy();
    expect(screen.getByText("no node party")).toBeTruthy();
  });

  it("shows a failed fetch", () => {
    renderCard(null, "Caller is not an admin");

    expect(screen.getByRole("alert").textContent).toBe("Caller is not an admin");
    expect(screen.queryByText("not set up")).toBeNull();
  });
});
