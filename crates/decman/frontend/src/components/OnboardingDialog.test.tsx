import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { OnboardingRequest } from "../types";
import { OnboardingDialog } from "./OnboardingDialog";

const selfId = `self::1220${"a".repeat(64)}`;
const peers = [
  { participant_id: `peer-1::1220${"b".repeat(64)}`, name: "Peer One" },
  { participant_id: `peer-2::1220${"c".repeat(64)}`, name: "Peer Two" },
].map((peer) => ({ ...peer, address: "localhost", port: 7000, public_key: "" }));

afterEach(() => vi.unstubAllGlobals());

/** Stub the backend and collect every onboarding request the dialog posts. */
const stubBackend = () => {
  const submitted: OnboardingRequest[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      let body: unknown = {};
      if (path === "/network-config") {
        body = { peers };
      } else if (path === "/node-config") {
        body = { node: { participant_id: selfId } };
      } else if (path === "/onboarding" && init?.method === "POST") {
        submitted.push(JSON.parse(init.body as string) as OnboardingRequest);
        body = { status: "inprogress", message: "started", instance_name: "x" };
      }
      return new Response(JSON.stringify(body), { status: 200 });
    }),
  );
  return submitted;
};

/** Open the dialog, name the party and invite both peers: three owners. */
const openWithBothPeers = async () => {
  render(
    <SnackbarProvider>
      <OnboardingDialog open onClose={() => {}} onComplete={() => {}} />
    </SnackbarProvider>,
  );
  fireEvent.change(screen.getByLabelText("Party ID prefix"), {
    target: { value: "xyz-network" },
  });
  fireEvent.click(await screen.findByText("Peer One"));
  fireEvent.click(screen.getByText("Peer Two"));
};

const valueOf = (label: string) =>
  (screen.getByLabelText(label) as HTMLInputElement).value;

describe("OnboardingDialog confirmation threshold", () => {
  it("mirrors the threshold and sends none of its own until edited", async () => {
    const submitted = stubBackend();
    await openWithBothPeers();

    expect(valueOf("Threshold")).toBe("2");
    expect(valueOf("Confirmation threshold")).toBe("2");

    fireEvent.change(screen.getByLabelText("Threshold"), {
      target: { value: "3" },
    });
    expect(valueOf("Confirmation threshold")).toBe("3");

    fireEvent.click(screen.getByRole("button", { name: "Start onboarding" }));
    await waitFor(() => expect(submitted).toHaveLength(1));

    expect(submitted[0].threshold).toBe(3);
    expect(submitted[0]).not.toHaveProperty("confirmation_threshold");
  });

  it("posts an edited confirmation threshold beside the threshold", async () => {
    const submitted = stubBackend();
    await openWithBothPeers();

    fireEvent.change(screen.getByLabelText("Confirmation threshold"), {
      target: { value: "3" },
    });
    // Once edited it no longer follows the threshold.
    fireEvent.change(screen.getByLabelText("Threshold"), {
      target: { value: "1" },
    });
    expect(valueOf("Confirmation threshold")).toBe("3");

    fireEvent.click(screen.getByRole("button", { name: "Start onboarding" }));
    await waitFor(() => expect(submitted).toHaveLength(1));

    expect(submitted[0].threshold).toBe(1);
    expect(submitted[0].confirmation_threshold).toBe(3);
  });

  it("will not start with more confirmations than hosts", async () => {
    stubBackend();
    await openWithBothPeers();

    fireEvent.change(screen.getByLabelText("Confirmation threshold"), {
      target: { value: "4" },
    });

    expect(screen.getByText("Must be between 1 and 3")).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "Start onboarding" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });
});
