import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { ContractsRequest } from "../types";
import { ContractsDialog } from "./ContractsDialog";

const partyId = `shared::${"a".repeat(68)}`;
const memberId = `member::${"b".repeat(68)}`;
const proposerId = `proposer::${"c".repeat(68)}`;

afterEach(() => vi.unstubAllGlobals());

const observerId = `observer::${"e".repeat(68)}`;

const openGovernanceRulesForm = async (): Promise<ContractsRequest[]> => {
  const submitted: ContractsRequest[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      let body: unknown = {};
      if (path === "/packages/vetted") {
        body = [{ package_id: "pkg", package_name: "governance-core", package_version: "1.0" }];
      } else if (path.startsWith("/governance/known-members")) {
        body = { members: [] };
      } else if (path === "/contracts" && init?.method === "POST") {
        submitted.push(JSON.parse(init.body as string) as ContractsRequest);
      }
      return new Response(JSON.stringify(body), { status: 200 });
    }),
  );

  render(
    <SnackbarProvider>
      <ContractsDialog
        open
        onClose={() => {}}
        onComplete={() => {}}
        partyId={partyId}
        participantIds={[`node::${"d".repeat(68)}`]}
      />
    </SnackbarProvider>,
  );

  await screen.findByText("Governance Core");
  fireEvent.click(screen.getByText("Governance Core"));
  fireEvent.click(screen.getByText("GovernanceRules"));
  return submitted;
};

const paste = (placeholder: string, party: string) => {
  const input = screen.getByPlaceholderText(placeholder);
  fireEvent.change(input, { target: { value: party } });
  fireEvent.blur(input);
};

describe("ContractsDialog party inputs", () => {
  it("includes pasted members and proposers when Deploy is clicked without pressing Enter", async () => {
    const submitted = await openGovernanceRulesForm();

    const memberInput = screen.getByPlaceholderText("Paste party ID, press Enter or click away");
    fireEvent.change(memberInput, { target: { value: memberId } });
    fireEvent.blur(memberInput);

    const proposerInput = screen.getByPlaceholderText(
      "Paste party ID, press Enter or click away (leave empty for no extra proposers)",
    );
    fireEvent.change(proposerInput, { target: { value: proposerId } });
    fireEvent.blur(proposerInput);

    fireEvent.click(screen.getByRole("button", { name: "Deploy Contracts" }));
    await waitFor(() => expect(submitted).toHaveLength(1));

    expect(submitted[0].participant_parties).toEqual([memberId]);
    expect(submitted[0].contracts[0].fields[1]).toEqual({
      type: "party_set",
      parties: [memberId],
    });
    expect(submitted[0].contracts[0].fields[4]).toEqual({
      type: "optional",
      inner: { type: "party_set", parties: [proposerId] },
    });
    // No observers goes out as None, which a node on governance-core 0.1.0
    // can drop.
    expect(submitted[0].contracts[0].fields[5]).toEqual({ type: "none" });
  });

  it("sends pasted disclosure observers as the last field", async () => {
    const submitted = await openGovernanceRulesForm();
    paste("Paste party ID, press Enter or click away", memberId);
    paste("Paste party ID, press Enter or click away (leave empty for no observers)", observerId);

    fireEvent.click(screen.getByRole("button", { name: "Deploy Contracts" }));
    await waitFor(() => expect(submitted).toHaveLength(1));

    expect(submitted[0].contracts[0].fields).toHaveLength(6);
    expect(submitted[0].contracts[0].fields[4]).toEqual({
      type: "optional",
      inner: { type: "party_set", parties: [] },
    });
    expect(submitted[0].contracts[0].fields[5]).toEqual({
      type: "optional",
      inner: { type: "party_set", parties: [observerId] },
    });
  });
});
