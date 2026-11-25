import { render as rtlRender, screen, within } from "@testing-library/react";
import type { ReactElement } from "react";
import { describe, expect, it } from "vitest";

import { SnackbarProvider } from "../contexts";
import { GovernanceRules } from "./GovernanceRules";
import type { GovernanceState } from "../types";

const ns = "a".repeat(68);
const alice = `alice::${ns}`;
const bob = `bob::${ns}`;
const carol = `carol::${ns}`;

// CopyableText reports copies through the snackbar.
const render = (ui: ReactElement) => rtlRender(<SnackbarProvider>{ui}</SnackbarProvider>);

const state = (over: Partial<GovernanceState> = {}): GovernanceState => ({
  contract_id: "00rules",
  governance_party: `gov::${ns}`,
  members: [alice, bob],
  threshold: 2,
  additional_proposers: [],
  disclosure_observers: [],
  out_of_date: false,
  ...over,
});

const rows = () =>
  screen.getAllByTestId("governance-party-row").map((row) => ({
    party: within(row).getAllByRole("cell")[0].textContent,
    role: within(row).getAllByRole("cell")[1].textContent,
  }));

describe("GovernanceRules", () => {
  it("lists every member and additional proposer with its role", () => {
    render(<GovernanceRules state={state({ additional_proposers: [carol] })} />);
    const listed = rows();
    expect(listed.map((r) => r.role)).toEqual(["Member", "Member", "Additional proposer"]);
    // CopyableText truncates long ids; each row still shows its own party.
    expect(listed[0].party).toContain("alice::");
    expect(listed[2].party).toContain("carol::");
    expect(screen.queryByText(/No additional proposers/)).toBeNull();
  });

  it("lists disclosure observers with their role", () => {
    render(<GovernanceRules state={state({ disclosure_observers: [carol] })} />);
    const listed = rows();
    expect(listed.map((r) => r.role)).toEqual(["Member", "Member", "Disclosure observer"]);
    expect(listed[2].party).toContain("carol::");
    expect(screen.queryByText(/No disclosure observers/)).toBeNull();
  });

  it("says when only hosting nodes can see the rules", () => {
    render(<GovernanceRules state={state()} />);
    expect(
      screen.getByText(
        "No disclosure observers: only nodes that host the governance party can see these rules.",
      ),
    ).toBeTruthy();
  });

  it("says when only members can propose", () => {
    render(<GovernanceRules state={state()} />);
    expect(screen.getByText("No additional proposers: only members can propose actions.")).toBeTruthy();
  });

  it("says the member set could not be read when it is empty", () => {
    render(<GovernanceRules state={state({ members: [] })} />);
    expect(screen.getByTestId("governance-members-unread").textContent).toBe(
      "The member set could not be read",
    );
  });

  it("marks this node's member party", () => {
    render(<GovernanceRules state={state()} memberPartyId={bob} />);
    const marked = screen.getAllByTestId("governance-party-row").filter((row) => within(row).queryByText("This node"));
    expect(marked).toHaveLength(1);
    expect(marked[0].textContent).toContain("bob::");
  });

  it("marks every party under this node's namespace", () => {
    const otherNs = "b".repeat(68);
    render(
      <GovernanceRules
        state={state({ members: [alice, `dave::${otherNs}`], additional_proposers: [carol] })}
        selfParticipantId={`node-1::${ns}`}
      />,
    );
    const marked = screen
      .getAllByTestId("governance-party-row")
      .filter((row) => within(row).queryByText("This node"))
      .map((row) => row.textContent);
    expect(marked).toHaveLength(2);
    expect(marked[0]).toContain("alice::");
    expect(marked[1]).toContain("carol::");
  });

  it("leaves out the governance party when it is the party on the page", () => {
    render(<GovernanceRules state={state()} partyId={`gov::${ns}`} />);
    expect(screen.queryByText("Governance party")).toBeNull();
    expect(screen.getByText("Rules contract")).toBeTruthy();
  });

  it("names the governance party when it is another party", () => {
    render(<GovernanceRules state={state()} partyId={`other::${ns}`} />);
    expect(screen.getByText("Governance party")).toBeTruthy();
  });

  it("shows the package the rules live under", () => {
    render(
      <GovernanceRules
        state={state({ package_ref: "#governance-core-v0-rc4", out_of_date: true })}
      />,
    );
    expect(screen.getByText("#governance-core-v0-rc4 (older package)")).toBeTruthy();
  });
});
