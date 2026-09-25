import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { PartyDetail } from "./PartyDetail";
import type { DecentralizedParty, PartyAuthStatus, RightsStatus } from "../types";

// `PartyDetail` and its two gated children each fetch on mount. The gate under
// test runs before any of that, so one stub that never resolves keeps the
// render deterministic without describing any endpoint's payload.
vi.mock("../api", () => ({
  authenticatedFetch: vi.fn(() => new Promise(() => {})),
}));

/** A 34-byte namespace, the length CantonId parses. */
const ns = "a".repeat(68);

const party: DecentralizedParty = {
  party_id: `alpha::${ns}`,
  threshold: 2,
  owners: [],
  participants: [],
  contracts: [],
};

const rights = (over: Partial<RightsStatus>): RightsStatus => ({
  member_party_act_as: false,
  member_party_read_as: false,
  dec_party_act_as: false,
  dec_party_read_as: false,
  ...over,
});

const authStatus = (over?: Partial<RightsStatus>): PartyAuthStatus => ({
  dec_party_id: party.party_id,
  member_party_id: `member::${ns}`,
  user_id: "reviewer",
  status: { status: "authenticated" },
  rights: over === undefined ? undefined : rights(over),
});

const renderDetail = (status: PartyAuthStatus) =>
  render(
    <SnackbarProvider>
      <PartyDetail
        party={party}
        onBack={() => {}}
        onRefresh={() => {}}
        onNavigateToNotifications={() => {}}
        authStatus={status}
      />
    </SnackbarProvider>,
  );

/**
 * Both sections only read. Gating them on `actAs` hid the audit trail from a
 * node configured with a read-only party credential, which is the case the
 * audit trail exists to serve. See #473.
 */
describe("PartyDetail read-only sections", () => {
  it("shows Holdings and Audit Trail when the credential holds readAs only", () => {
    renderDetail(authStatus({ dec_party_read_as: true }));

    expect(screen.getByText("Holdings")).toBeTruthy();
    expect(screen.getByText("Audit Trail")).toBeTruthy();
  });

  it("still shows both when the credential holds actAs only", () => {
    renderDetail(authStatus({ dec_party_act_as: true }));

    expect(screen.getByText("Holdings")).toBeTruthy();
    expect(screen.getByText("Audit Trail")).toBeTruthy();
  });

  it("hides both when the credential holds neither right", () => {
    renderDetail(authStatus({}));

    expect(screen.queryByText("Holdings")).toBeNull();
    expect(screen.queryByText("Audit Trail")).toBeNull();
  });

  it("hides both when the rights are unknown", () => {
    renderDetail(authStatus());

    expect(screen.queryByText("Holdings")).toBeNull();
    expect(screen.queryByText("Audit Trail")).toBeNull();
  });
});

describe("PartyDetail Check DARs", () => {
  it("opens the package comparison for this party", () => {
    const onCheckDars = vi.fn();
    render(
      <SnackbarProvider>
        <PartyDetail
          party={party}
          onBack={() => {}}
          onRefresh={() => {}}
          onNavigateToNotifications={() => {}}
          authStatus={authStatus({})}
          onCheckDars={onCheckDars}
        />
      </SnackbarProvider>,
    );

    screen.getByRole("button", { name: "Check DARs" }).click();

    expect(onCheckDars).toHaveBeenCalledTimes(1);
  });
});
