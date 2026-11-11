import { describe, expect, it } from "vitest";

import {
  buildGenericVote,
  buildSetupCcPreapproval,
  buildSetupTokenPreapproval,
} from "./tokenCustodyPayloads";

describe("buildSetupCcPreapproval", () => {
  it("sends the provider and the expected DSO as typed", () => {
    expect(
      buildSetupCcPreapproval({ proposalProvider: "prov::1", proposalExpectedDso: "DSO::1" }),
    ).toEqual({ type: "setup_cc_preapproval", provider: "prov::1", expected_dso: "DSO::1" });
  });
});

describe("buildSetupTokenPreapproval", () => {
  it("drops the row uid and the blank allowance rows", () => {
    expect(
      buildSetupTokenPreapproval({
        proposalOperator: "op::1",
        proposalInstrumentAdmin: "admin::1",
        proposalInstrumentAllowances: [
          { uid: "u1", id: "TOK" },
          { uid: "u2", id: "  " },
          { uid: "u3", id: "OTHER" },
        ],
      }),
    ).toEqual({
      type: "setup_token_preapproval",
      operator: "op::1",
      instrument_admin: "admin::1",
      instrument_allowances: [{ id: "TOK" }, { id: "OTHER" }],
    });
  });

  it("sends an empty allowance list when no row has an id", () => {
    const proposal = buildSetupTokenPreapproval({
      proposalOperator: "op::1",
      proposalInstrumentAdmin: "admin::1",
      proposalInstrumentAllowances: [],
    });
    expect(proposal).toMatchObject({ instrument_allowances: [] });
  });
});

describe("buildGenericVote", () => {
  it("sends the description untrimmed", () => {
    expect(buildGenericVote({ proposalDescription: " Raise the fee " })).toEqual({
      type: "generic_vote",
      description: " Raise the fee ",
    });
  });
});
