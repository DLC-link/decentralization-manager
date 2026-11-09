import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { RotateKeyRequest } from "../types";
import { RotateKeyDialog } from "./RotateKeyDialog";

const partyId = `shared::1220${"a".repeat(64)}`;

afterEach(() => vi.unstubAllGlobals());

/** Stub the backend and collect every rotate-key request posted. */
const stubBackend = (response: Response) => {
  const submitted: RotateKeyRequest[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === "/rotate-key" && init?.method === "POST") {
        submitted.push(JSON.parse(init.body as string) as RotateKeyRequest);
      }
      return response.clone();
    }),
  );
  return submitted;
};

const renderDialog = ({
  memberCount = 3,
  onClose = () => {},
  onComplete = () => {},
} = {}) =>
  render(
    <SnackbarProvider>
      <RotateKeyDialog
        open
        onClose={onClose}
        onComplete={onComplete}
        partyId={partyId}
        memberCount={memberCount}
      />
    </SnackbarProvider>,
  );

const submitButton = () =>
  screen.getByRole("button", { name: "Rotate Key" }) as HTMLButtonElement;

describe("RotateKeyDialog", () => {
  it("posts only the party id, then closes and opens the feed", async () => {
    const submitted = stubBackend(
      new Response(JSON.stringify({ instance_name: "rk-1" }), { status: 202 }),
    );
    const onClose = vi.fn();
    const onComplete = vi.fn();
    renderDialog({ onClose, onComplete });

    fireEvent.click(submitButton());
    await waitFor(() => expect(onComplete).toHaveBeenCalled());

    expect(submitted).toEqual([{ decentralized_party_id: partyId }]);
    expect(onClose).toHaveBeenCalled();
  });

  it("shows the server's refusal and stays open", async () => {
    stubBackend(
      new Response(
        JSON.stringify({ error: "This participant keeps its keys in Canton's software vault" }),
        { status: 400 },
      ),
    );
    const onClose = vi.fn();
    renderDialog({ onClose });

    fireEvent.click(submitButton());

    expect(
      await screen.findByText(/keeps its keys in Canton's software vault/),
    ).toBeTruthy();
    expect(onClose).not.toHaveBeenCalled();
    expect(submitButton().disabled).toBe(false);
  });

  it("says how many other members must sign", () => {
    stubBackend(new Response("{}", { status: 200 }));
    renderDialog({ memberCount: 3 });

    expect(screen.getByText(/The other 2 members must sign the change/)).toBeTruthy();
  });

  it("will not start for a party with one member", () => {
    stubBackend(new Response("{}", { status: 200 }));
    renderDialog({ memberCount: 1 });

    expect(submitButton().disabled).toBe(true);
  });
});
