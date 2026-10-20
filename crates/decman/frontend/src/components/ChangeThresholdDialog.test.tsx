import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { ChangeThresholdRequest } from "../types";
import { ChangeThresholdDialog } from "./ChangeThresholdDialog";

const partyId = `shared::1220${"a".repeat(64)}`;

afterEach(() => vi.unstubAllGlobals());

/** Stub the backend and collect every change-threshold request posted. */
const stubBackend = () => {
  const submitted: ChangeThresholdRequest[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === "/change-threshold" && init?.method === "POST") {
        submitted.push(JSON.parse(init.body as string) as ChangeThresholdRequest);
      }
      return new Response(JSON.stringify({ status: "idle" }), { status: 200 });
    }),
  );
  return submitted;
};

/** A three-member party whose two thresholds are both 2. */
const renderDialog = () =>
  render(
    <SnackbarProvider>
      <ChangeThresholdDialog
        open
        onClose={() => {}}
        onComplete={() => {}}
        partyId={partyId}
        currentThreshold={2}
        currentOwnerCount={3}
        currentConfirmationThreshold={2}
        currentHostCount={3}
      />
    </SnackbarProvider>,
  );

const valueOf = (label: string) =>
  (screen.getByLabelText(label) as HTMLInputElement).value;

const submitButton = () =>
  screen.getByRole("button", { name: "Change Threshold" }) as HTMLButtonElement;

describe("ChangeThresholdDialog confirmation threshold", () => {
  it("follows the new threshold and sends none of its own until edited", async () => {
    const submitted = stubBackend();
    renderDialog();

    expect(valueOf("Confirmation Threshold")).toBe("2");
    expect(submitButton().disabled).toBe(true);

    fireEvent.change(screen.getByLabelText("New Threshold"), {
      target: { value: "1" },
    });
    expect(valueOf("Confirmation Threshold")).toBe("1");

    fireEvent.click(submitButton());
    await waitFor(() => expect(submitted).toHaveLength(1));

    expect(submitted[0]).toEqual({
      decentralized_party_id: partyId,
      new_threshold: 1,
      previous_threshold: 2,
    });
  });

  it("posts an edited confirmation threshold, even with the threshold unchanged", async () => {
    const submitted = stubBackend();
    renderDialog();

    fireEvent.change(screen.getByLabelText("Confirmation Threshold"), {
      target: { value: "3" },
    });
    expect(submitButton().disabled).toBe(false);

    fireEvent.click(submitButton());
    await waitFor(() => expect(submitted).toHaveLength(1));

    expect(submitted[0].new_threshold).toBe(2);
    expect(submitted[0].confirmation_threshold).toBe(3);
  });

  it("will not submit more confirmations than hosts", () => {
    stubBackend();
    renderDialog();

    fireEvent.change(screen.getByLabelText("Confirmation Threshold"), {
      target: { value: "4" },
    });

    expect(screen.getByText("Must be between 1 and 3")).toBeTruthy();
    expect(submitButton().disabled).toBe(true);
  });
});
