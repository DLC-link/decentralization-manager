import { act, render, screen, waitFor, within } from "@testing-library/react";
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
  non_hosting_owners: [],
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

describe("PartyDetail thresholds", () => {
  afterEach(() => {
    vi.mocked(authenticatedFetch).mockImplementation(() => new Promise(() => {}));
  });

  it("shows the confirmation threshold beside the threshold", () => {
    const host = (tag: string) => ({
      participant_uid: `${tag}::${ns}`,
      permission: "confirmation" as const,
    });
    render(
      detail(authStatus(), {
        ...party,
        confirmation_threshold: 3,
        participants: [host("p1"), host("p2"), host("p3")],
      }),
    );

    expect(screen.getByTestId("party-threshold").textContent).toBe("2");
    expect(
      screen.getByTestId("party-confirmation-threshold").textContent,
    ).toBe("3");
  });

  it("leaves it out when the party list does not report one", () => {
    renderDetail(authStatus());

    expect(screen.queryByTestId("party-confirmation-threshold")).toBeNull();
  });

  it("shows the governance threshold against the member count", async () => {
    const governance: GovernanceStateResponse = {
      state: {
        contract_id: "00rules",
        governance_party: party.party_id,
        members: [`m1::${ns}`, `m2::${ns}`, `m3::${ns}`],
        threshold: 1,
        additional_proposers: [],
        disclosure_observers: [],
        out_of_date: false,
      },
    };
    vi.mocked(authenticatedFetch).mockImplementation((url) =>
      String(url).includes("/governance/state")
        ? Promise.resolve(new Response(JSON.stringify(governance)))
        : new Promise(() => {}),
    );

    renderDetail(authStatus());

    const value = await screen.findByTestId("party-gov-threshold");
    expect(value.textContent).toBe("1");
    expect(within(value.parentElement!).getByText("of 3")).toBeTruthy();
  });
});

describe("PartyDetail Check Peer DARs", () => {
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

    screen.getByRole("button", { name: "Check Peer DARs" }).click();

    expect(onCheckDars).toHaveBeenCalledTimes(1);
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
        additional_proposers: [],
        disclosure_observers: [],
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
        additional_proposers: [],
        disclosure_observers: [],
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
    // The Governance section lists the member set from the same response.
    expect(screen.getByText("Governance")).toBeTruthy();
    expect(screen.getAllByTestId("governance-party-row")).toHaveLength(1);

    const other: DecentralizedParty = { ...party, party_id: `beta::${ns}` };
    rerender(detail({ ...status, dec_party_id: other.party_id }, other));

    expect(screen.queryByLabelText("Authenticated")).toBeNull();
    expect(
      screen.getByLabelText("Authenticated, governance membership unknown"),
    ).toBeTruthy();
  });
});

describe("PartyDetail rewards", () => {
  it("shows a failed reward status read as a load error, not a missing DAR", async () => {
    vi.mocked(authenticatedFetch).mockImplementation((url) =>
      String(url).includes("/reward-status")
        ? Promise.resolve(
            new Response(JSON.stringify({ error: "ledger unavailable" }), {
              status: 503,
            }),
          )
        : new Promise(() => {}),
    );

    renderDetail(authStatus({ dec_party_read_as: true }));

    await waitFor(() =>
      expect(
        screen.getByText(
          "Could not load the reward status: ledger unavailable",
        ),
      ).toBeTruthy(),
    );
    expect(screen.queryByText(/rewards DAR/)).toBeNull();
  });

  it("replaces a failed read with the next poll's status", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const responses = [
      new Response(JSON.stringify({ error: "ledger unavailable" }), {
        status: 503,
      }),
      new Response(
        JSON.stringify({
          rewards_dar_resolvable: false,
          delegation: null,
          member_party_id: authStatus().member_party_id,
          member_is_assigner: false,
          beneficiaries: [],
        }),
      ),
    ];
    let calls = 0;
    vi.mocked(authenticatedFetch).mockImplementation((url) => {
      if (!String(url).includes("/reward-status")) return new Promise(() => {});
      const response = responses[Math.min(calls, responses.length - 1)];
      calls += 1;
      return Promise.resolve(response.clone());
    });

    try {
      renderDetail(authStatus({ dec_party_read_as: true }));
      await waitFor(() =>
        expect(
          screen.getByText("Could not load the reward status: ledger unavailable"),
        ).toBeTruthy(),
      );

      await act(async () => {
        await vi.advanceTimersByTimeAsync(30_000);
      });

      await waitFor(() =>
        expect(
          screen.getByText("This participant does not have the rewards DAR"),
        ).toBeTruthy(),
      );
      expect(screen.queryByText(/Could not load the reward status/)).toBeNull();
      expect(calls).toBe(2);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("PartyDetail owners that do not host the party", () => {
  const host = `p1::1220${"1".repeat(64)}`;
  const claimant = `p3::1220${"3".repeat(64)}`;
  const claimedKey = `1220${"c".repeat(64)}`;
  const unclaimedKey = `1220${"d".repeat(64)}`;
  const withOwners: DecentralizedParty = {
    ...party,
    owners: [`1220${"b".repeat(64)}`, claimedKey, unclaimedKey],
    participants: [{ participant_uid: host, permission: "confirmation" }],
    non_hosting_owners: [
      { owner_key: claimedKey, participant_uid: claimant },
      { owner_key: unclaimedKey },
    ],
  };

  it("lists each one after the hosts, marked owner only", () => {
    render(detail(authStatus(), withOwners));

    const rows = screen.getAllByTestId("owner-only-row");
    expect(rows.map((row) => row.getAttribute("data-owner-key"))).toEqual([
      claimedKey,
      unclaimedKey,
    ]);
    expect(screen.getAllByText("Owner only")).toHaveLength(2);
    expect(screen.getByText("Unclaimed owner key")).toBeTruthy();
    expect(screen.getAllByTestId("participant-row")).toHaveLength(1);
  });

  it("opens the kick for an owner-only member", () => {
    render(detail(authStatus(), withOwners));

    act(() => {
      screen.getAllByRole("button", { name: "Kick owner" })[1].click();
    });

    expect(screen.getByRole("heading", { name: "Kick Owner" })).toBeTruthy();
    expect(screen.getByTestId("kick-owner-only-note")).toBeTruthy();
  });

  it("lists none when every owner hosts the party", () => {
    render(detail(authStatus()));
    expect(screen.queryAllByTestId("owner-only-row")).toEqual([]);
  });
});

