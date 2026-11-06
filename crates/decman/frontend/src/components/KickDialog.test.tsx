import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { authenticatedFetch } from "../api";
import { SnackbarProvider } from "../contexts";
import { KickDialog } from "./KickDialog";

vi.mock("../api", () => ({
  authenticatedFetch: vi.fn(),
}));

const partyId = `alpha::1220${"a".repeat(64)}`;
const claimant = `p3::1220${"c".repeat(64)}`;
const ownerKey = `1220${"d".repeat(64)}`;

const mockedFetch = vi.mocked(authenticatedFetch);

const renderDialog = (props: {
  participantUid?: string;
  hosting: boolean;
}) =>
  render(
    <SnackbarProvider>
      <KickDialog
        open
        onClose={() => {}}
        onKickComplete={() => {}}
        partyId={partyId}
        participantUid={props.participantUid}
        participantOwnerKey={ownerKey}
        hosting={props.hosting}
        currentThreshold={2}
        currentOwnerCount={3}
        currentHostCount={2}
      />
    </SnackbarProvider>,
  );

/** The JSON body of the one POST /kick the dialog sent. */
const postedKick = async () => {
  await waitFor(() =>
    expect(mockedFetch).toHaveBeenCalledWith(
      expect.stringMatching(/\/kick$/),
      expect.anything(),
    ),
  );
  const call = mockedFetch.mock.calls.find(([url]) =>
    String(url).endsWith("/kick"),
  );
  return JSON.parse(String(call?.[1]?.body));
};

afterEach(() => {
  mockedFetch.mockReset();
});

describe("KickDialog for an owner that does not host the party", () => {
  it("names the owner key and the participant that claimed it", async () => {
    mockedFetch.mockResolvedValue(new Response("{}", { status: 202 }));
    renderDialog({ participantUid: claimant, hosting: false });

    expect(screen.getByTestId("kick-owner-only-note")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Kick Owner" }));

    expect(await postedKick()).toEqual({
      decentralized_party_id: partyId,
      participant_id: claimant,
      owner_key: ownerKey,
      new_threshold: 2,
      previous_threshold: 2,
    });
  });

  it("names only the owner key when no peer claimed it", async () => {
    mockedFetch.mockResolvedValue(new Response("{}", { status: 202 }));
    renderDialog({ hosting: false });

    expect(
      (screen.getByLabelText("Participant ID to Kick") as HTMLInputElement)
        .value,
    ).toBe("Not claimed by any peer");
    fireEvent.click(screen.getByRole("button", { name: "Kick Owner" }));

    const body = await postedKick();
    expect(body.owner_key).toBe(ownerKey);
    expect(body).not.toHaveProperty("participant_id");
  });

  // Two hosts stay after an owner-only kick, so the threshold may stay at 2.
  // A host's kick on the same party leaves one host key, so it may not.
  it("allows every host to sign after an owner-only kick", () => {
    renderDialog({ hosting: false });
    expect(screen.getByText(/max: 2\)/)).toBeTruthy();
  });
});

describe("KickDialog for a host", () => {
  it("names the participant and leaves the owner key to the server", async () => {
    mockedFetch.mockResolvedValue(new Response("{}", { status: 202 }));
    renderDialog({ participantUid: claimant, hosting: true });

    expect(screen.queryByTestId("kick-owner-only-note")).toBeNull();
    expect(screen.getByText(/max: 1\)/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Kick Participant" }));

    const body = await postedKick();
    expect(body.participant_id).toBe(claimant);
    expect(body).not.toHaveProperty("owner_key");
    expect(body.new_threshold).toBe(1);
  });
});
