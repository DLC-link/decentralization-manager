import { render as rtlRender, screen, within } from "@testing-library/react";
import type { ReactElement } from "react";
import { describe, expect, it } from "vitest";

import { SnackbarProvider } from "../contexts";
import { RewardStatus } from "./RewardStatus";
import type { RewardStatusResponse } from "../types";

const ns = "a".repeat(68);
const alice = `alice::${ns}`;
const bob = `bob::${ns}`;
const carol = `carol::${ns}`;

// CopyableText reports copies through the snackbar.
const render = (ui: ReactElement) => rtlRender(<SnackbarProvider>{ui}</SnackbarProvider>);

const status = (over: Partial<RewardStatusResponse> = {}): RewardStatusResponse => ({
  rewards_dar_resolvable: true,
  member_party_id: alice,
  member_is_assigner: false,
  beneficiaries: [],
  ...over,
});

const delegated = (member: string, isAssigner: boolean) =>
  status({
    member_party_id: member,
    member_is_assigner: isAssigner,
    delegation: {
      cid: "00delegation",
      dso: `dso::${ns}`,
      assigners: [alice, bob],
      beneficiary_count: 2,
    },
    beneficiaries: [
      { beneficiary: alice, percentage: "0.8000000000" },
      { beneficiary: bob, percentage: "0.2000000000" },
    ],
  });

// CopyableText truncates long ids, so each row is read by its party's prefix.
const rows = () =>
  screen.getAllByTestId("reward-party-row").map((row) => {
    const cells = within(row).getAllByRole("cell");
    return {
      party: cells[0].textContent?.split("::")[0],
      role: cells[1].textContent,
      share: cells[2].textContent,
      marked: within(row).queryByText("This node") !== null,
    };
  });

describe("RewardStatus", () => {
  it("says the rewards DAR is missing and how to fix it", () => {
    render(<RewardStatus status={status({ rewards_dar_resolvable: false })} error={null} />);
    expect(screen.getByText("This participant does not have the rewards DAR")).toBeTruthy();
    expect(screen.getByText(/Upload that DAR to this participant/)).toBeTruthy();
    expect(screen.queryByText(/No reward delegation/)).toBeNull();
  });

  it("says when no delegation is active", () => {
    render(<RewardStatus status={status()} error={null} />);
    expect(screen.getByText(/No reward delegation is active for this party/)).toBeTruthy();
    expect(screen.queryByText(/rewards DAR/)).toBeNull();
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("marks this node when the delegation lists it as an assigner", () => {
    render(<RewardStatus status={delegated(bob, true)} error={null} />);
    expect(screen.getByText(/This node is a listed assigner/)).toBeTruthy();
    expect(rows()).toEqual([
      { party: "alice", role: "Assigner", share: "", marked: false },
      { party: "bob", role: "Assigner", share: "", marked: true },
      { party: "alice", role: "Beneficiary", share: "0.8", marked: false },
      { party: "bob", role: "Beneficiary", share: "0.2", marked: true },
    ]);
  });

  it("says when the delegation does not list this node", () => {
    render(<RewardStatus status={delegated(carol, false)} error={null} />);
    expect(screen.getByText(/This node is not a listed assigner/)).toBeTruthy();
    // The assigners and the split still show, with no row marked.
    expect(rows().map((r) => r.role)).toEqual(["Assigner", "Assigner", "Beneficiary", "Beneficiary"]);
    expect(screen.queryByText("This node")).toBeNull();
  });

  it("reports a failed load as an error, not as a missing DAR or delegation", () => {
    render(<RewardStatus status={null} error="cannot read this party's reward status" />);
    expect(
      screen.getByText("Could not load the reward status: cannot read this party's reward status"),
    ).toBeTruthy();
    expect(screen.queryByText(/rewards DAR/)).toBeNull();
    expect(screen.queryByText(/No reward delegation/)).toBeNull();
  });
});
