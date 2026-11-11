import { describe, expect, it } from "vitest";

import {
  buildCreateTransferRule,
  buildOffboardInstrumentIssuers,
  buildOnboardInstrumentIssuers,
  buildProvisionInstrument,
  buildUpdateInstrumentConfiguration,
} from "./instrumentPayloads";

const openWindow = { proposalCredentialValidFrom: "", proposalCredentialValidUntil: "" };

describe("buildProvisionInstrument", () => {
  const fields = {
    ...openWindow,
    proposalRegistrarServiceCid: "rs-1",
    proposalInstrumentIdText: "cTM",
    proposalIssuerRequirements: [{ issuer: "gov::1", claimsText: "issuer,true" }],
    proposalHolderRequirements: [],
    proposalInitialInstrumentIssuersText: "a::1\nb::1",
  };

  it("sends the requirements, the initial issuers and an open window", () => {
    expect(buildProvisionInstrument(fields)).toEqual({
      type: "provision_instrument",
      registrar_service_cid: "rs-1",
      instrument_id_text: "cTM",
      additional_identifiers: [],
      issuer_requirements: [
        { issuer: "gov::1", required_claims: [{ property: "issuer", value: "true" }] },
      ],
      holder_requirements: [],
      initial_instrument_issuers: ["a::1", "b::1"],
      credential_valid_from_micros: undefined,
      credential_valid_until_micros: undefined,
    });
  });

  it("checks the issuer lines before the credential window", () => {
    expect(() =>
      buildProvisionInstrument({
        ...fields,
        proposalInitialInstrumentIssuersText: "a::1\na::1",
        proposalCredentialValidFrom: "bad",
      }),
    ).toThrow("Party id listed more than once: a::1");
  });
});

describe("buildOnboardInstrumentIssuers", () => {
  it("sends the issuers and the credential window", () => {
    expect(
      buildOnboardInstrumentIssuers({
        proposalInstrumentConfigurationCid: "inst-1",
        proposalInstrumentIssuersText: "a::1",
        proposalCredentialValidFrom: "",
        proposalCredentialValidUntil: "2027-01-01T00:00",
      }),
    ).toEqual({
      type: "onboard_instrument_issuers",
      instrument_configuration_cid: "inst-1",
      instrument_issuers: ["a::1"],
      credential_valid_from_micros: undefined,
      credential_valid_until_micros: new Date("2027-01-01T00:00").getTime() * 1000,
    });
  });
});

describe("buildUpdateInstrumentConfiguration", () => {
  const fields = {
    proposalRegistrarServiceCid: "rs-1",
    proposalInstrumentConfigurationCid: "inst-1",
    proposalInstrumentIdText: "cTM",
    proposalAdditionalIdentifiers: [{ source: " gov::1 ", id: " ISIN-1 ", scheme: "" }],
    proposalIssuerRequirements: [],
    proposalHolderRequirements: [],
    configurationToUpdate: undefined,
    blockListProblem: null,
  };

  it("trims only the identifier source and keeps an unread block list flag unset", () => {
    expect(buildUpdateInstrumentConfiguration(fields)).toEqual({
      type: "update_instrument_configuration",
      registrar_service_cid: "rs-1",
      instrument_configuration_cid: "inst-1",
      instrument_id_text: "cTM",
      additional_identifiers: [{ source: "gov::1", id: " ISIN-1 ", scheme: "" }],
      issuer_requirements: [],
      holder_requirements: [],
      use_block_list: null,
    });
  });

  it("names an identifier without a source or id", () => {
    expect(() =>
      buildUpdateInstrumentConfiguration({
        ...fields,
        proposalAdditionalIdentifiers: [
          { source: "gov::1", id: "x", scheme: "" },
          { source: "gov::1", id: "", scheme: "s" },
        ],
      }),
    ).toThrow("Additional identifier 2: source and id are required");
  });

  it("refuses an update the block list check rules out", () => {
    expect(() =>
      buildUpdateInstrumentConfiguration({ ...fields, blockListProblem: "Checks a block list" }),
    ).toThrow("Checks a block list");
  });
});

describe("single-field instrument proposals", () => {
  it("sends the registrar service of a new transfer rule", () => {
    expect(buildCreateTransferRule({ proposalRegistrarServiceCid: "rs-1" })).toEqual({
      type: "create_transfer_rule",
      registrar_service_cid: "rs-1",
    });
  });

  it("sends one entry per offboarded issuer", () => {
    expect(
      buildOffboardInstrumentIssuers({
        proposalOffboardRows: [
          { party: "a::1", cids: ["c1", "c2"] },
          { party: "b::1", cids: [] },
        ],
      }),
    ).toEqual({
      type: "offboard_instrument_issuers",
      instrument_issuers: [
        { instrument_issuer: "a::1", credential_cids: ["c1", "c2"] },
        { instrument_issuer: "b::1", credential_cids: [] },
      ],
    });
  });
});
