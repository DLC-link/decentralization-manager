import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { authenticatedFetch } from "../api";
import { SnackbarProvider } from "../contexts";
import { PartyDetail } from "./PartyDetail";
import type {
  DecentralizedParty,
  GovernanceStateResponse,
  PartyAuthStatus,
  RightsStatus,
} from "../types";

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

const detail = (status: PartyAuthStatus, forParty = party) => (
  <SnackbarProvider>
    <PartyDetail
      party={forParty}
      onBack={() => {}}
      onRefresh={() => {}}
      onNavigateToNotifications={() => {}}
      authStatus={status}
    />
  </SnackbarProvider>
);

const renderDetail = (status: PartyAuthStatus) => render(detail(status));

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

describe("PartyDetail governance membership", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("shows membership that a later poll returns", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const status = authStatus();
    const withMembers = (members: string[]): GovernanceStateResponse => ({
      state: {
        contract_id: "00rules",
        governance_party: party.party_id,
        members,
        threshold: 1,
        out_of_date: false,
      },
    });
    const responses = [
      withMembers([`other::${ns}`]),
      withMembers([`other::${ns}`, status.member_party_id]),
    ];
    let calls = 0;
    vi.mocked(authenticatedFetch).mockImplementation((url) => {
      if (!String(url).includes("/governance/state"))
        return new Promise(() => {});
      const body = responses[Math.min(calls, responses.length - 1)];
      calls += 1;
      return Promise.resolve(new Response(JSON.stringify(body)));
    });

    render(detail(status));
    await waitFor(() =>
      expect(
        screen.getByLabelText(
          "Authenticated, but the member party is not a governance member",
        ),
      ).toBeTruthy(),
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    await waitFor(() =>
      expect(screen.getByLabelText("Authenticated")).toBeTruthy(),
    );
    expect(calls).toBe(2);
  });

  it("drops the previous party's membership when the party changes", async () => {
    const status = authStatus();
    const loaded: GovernanceStateResponse = {
      state: {
        contract_id: "00rules",
        governance_party: party.party_id,
        members: [status.member_party_id],
        threshold: 1,
        out_of_date: false,
      },
    };
    vi.mocked(authenticatedFetch).mockImplementation((url) =>
      String(url).includes(encodeURIComponent(party.party_id))
        ? Promise.resolve(new Response(JSON.stringify(loaded)))
        : new Promise(() => {}),
    );

    const { rerender } = render(detail(status));
    await waitFor(() =>
      expect(screen.getByLabelText("Authenticated")).toBeTruthy(),
    );

    const other: DecentralizedParty = { ...party, party_id: `beta::${ns}` };
    rerender(detail({ ...status, dec_party_id: other.party_id }, other));

    expect(screen.queryByLabelText("Authenticated")).toBeNull();
    expect(
      screen.getByLabelText("Authenticated, governance membership unknown"),
    ).toBeTruthy();
  });
});
