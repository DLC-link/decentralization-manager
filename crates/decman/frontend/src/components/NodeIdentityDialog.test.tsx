import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { NodeIdentityRequest, NodeIdentityResponse, NodeLoginSummary } from "../types";
import { NodeIdentityDialog } from "./NodeIdentityDialog";

const nodeParty = `decman-node-participant1::1220${"a".repeat(64)}`;

const RESPONSE: NodeIdentityResponse = {
  node_party_id: nodeParty,
  participant_id: `participant1::1220${"a".repeat(64)}`,
  allocated: true,
  namespace_matches: true,
  hosting_permission: "submission",
  user_id: "node-user",
  act_as: true,
  read_as: false,
  rights_granted: false,
};

const AUTH0_LOGIN: NodeLoginSummary = {
  idp: "auth0",
  client_id: "node-m2m",
  auth0_domain: "tenant.us.auth0.com",
  auth0_audience: "https://canton.network.global",
};

afterEach(() => vi.unstubAllGlobals());

/** Stub the backend and collect every node identity request posted. */
const stubBackend = (answer = () => Response.json(RESPONSE)) => {
  const submitted: NodeIdentityRequest[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === "/node-identity" && init?.method === "POST") {
        submitted.push(JSON.parse(init.body as string) as NodeIdentityRequest);
      }
      return answer();
    }),
  );
  return submitted;
};

const type = (label: string, value: string) =>
  fireEvent.change(screen.getByLabelText(label), { target: { value } });

const valueOf = (label: string) => (screen.getByLabelText(label) as HTMLInputElement).value;

const saveButton = () => screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;

const typeAdmin = () => {
  type("Admin Client ID", "validator-admin");
  type("Admin Client Secret", "admin-secret");
};

describe("NodeIdentityDialog", () => {
  it("posts a new Keycloak login with the admin client", async () => {
    const submitted = stubBackend();
    const onSaved = vi.fn();
    render(<NodeIdentityDialog open onClose={() => {}} onSaved={onSaved} />);

    type("URL", "https://kc.example");
    type("Realm", "canton");
    type("Node Client ID", "node-client");
    typeAdmin();
    expect(saveButton().disabled).toBe(true);
    type("Node Client Secret", "node-secret");
    fireEvent.click(saveButton());
    await waitFor(() => expect(onSaved).toHaveBeenCalledWith(RESPONSE));

    expect(submitted).toEqual([
      {
        admin_client_id: "validator-admin",
        admin_client_secret: "admin-secret",
        keycloak_url: "https://kc.example",
        keycloak_realm: "canton",
        keycloak_client_id: "node-client",
        keycloak_client_secret: "node-secret",
      },
    ]);
  });

  it("omits the secret to keep the stored one", async () => {
    const submitted = stubBackend();
    render(
      <NodeIdentityDialog open onClose={() => {}} onSaved={() => {}} login={AUTH0_LOGIN} />,
    );

    expect(valueOf("Domain")).toBe("tenant.us.auth0.com");
    expect(valueOf("Node Client ID")).toBe("node-m2m");
    typeAdmin();
    fireEvent.click(saveButton());
    await waitFor(() => expect(submitted).toHaveLength(1));

    expect(submitted[0]).toEqual({
      admin_client_id: "validator-admin",
      admin_client_secret: "admin-secret",
      auth0_domain: "tenant.us.auth0.com",
      auth0_audience: "https://canton.network.global",
      auth0_client_id: "node-m2m",
    });
  });

  it("asks for the secret again when the domain changes", () => {
    stubBackend();
    render(
      <NodeIdentityDialog open onClose={() => {}} onSaved={() => {}} login={AUTH0_LOGIN} />,
    );
    typeAdmin();
    expect(saveButton().disabled).toBe(false);

    type("Domain", "other.eu.auth0.com");

    expect(saveButton().disabled).toBe(true);
    type("Node Client Secret", "new-secret");
    expect(saveButton().disabled).toBe(false);
  });

  it("asks for the secret again when the provider changes", () => {
    stubBackend();
    render(
      <NodeIdentityDialog open onClose={() => {}} onSaved={() => {}} login={AUTH0_LOGIN} />,
    );
    typeAdmin();

    fireEvent.click(screen.getByRole("button", { name: "Keycloak" }));
    type("URL", "https://kc.example");
    type("Realm", "canton");

    expect(saveButton().disabled).toBe(true);
  });

  it("wipes the secrets and the admin client when it opens again", () => {
    stubBackend();
    const props = { onClose: () => {}, onSaved: () => {}, login: AUTH0_LOGIN };
    const { rerender } = render(<NodeIdentityDialog open {...props} />);
    type("Node Client Secret", "node-secret");
    typeAdmin();

    rerender(<NodeIdentityDialog open={false} {...props} />);
    rerender(<NodeIdentityDialog open {...props} />);

    expect(valueOf("Node Client Secret")).toBe("");
    expect(valueOf("Admin Client Secret")).toBe("");
    expect(valueOf("Admin Client ID")).toBe("");
    expect(valueOf("Node Client ID")).toBe("node-m2m");
  });

  it("posts an empty request in insecure mode", async () => {
    const submitted = stubBackend();
    const onSaved = vi.fn();
    render(<NodeIdentityDialog open onClose={() => {}} onSaved={onSaved} testMode />);

    expect(screen.queryByLabelText("Node Client ID")).toBeNull();
    expect(screen.queryByLabelText("Admin Client Secret")).toBeNull();
    fireEvent.click(saveButton());
    await waitFor(() => expect(onSaved).toHaveBeenCalledWith(RESPONSE));

    expect(submitted).toEqual([{}]);
  });

  it("shows the backend's error and keeps the dialog open", async () => {
    stubBackend(() => Response.json({ error: "Node login Auth0 auth failed" }, { status: 422 }));
    const onClose = vi.fn();
    const onSaved = vi.fn();
    render(
      <NodeIdentityDialog open onClose={onClose} onSaved={onSaved} login={AUTH0_LOGIN} />,
    );
    typeAdmin();

    fireEvent.click(saveButton());

    expect((await screen.findByRole("alert")).textContent).toContain(
      "Node login Auth0 auth failed",
    );
    expect(valueOf("Admin Client Secret")).toBe("");
    expect(valueOf("Admin Client ID")).toBe("validator-admin");
    expect(onSaved).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });
});
