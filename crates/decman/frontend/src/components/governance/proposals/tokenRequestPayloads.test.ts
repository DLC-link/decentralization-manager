import { describe, expect, it } from "vitest";

import { buildAcceptBurnRequest, buildAcceptMintRequest } from "./tokenRequestPayloads";

const fields = {
  proposalDescription: "Holder passed KYC",
  proposalInstrumentConfigurationCid: "inst-1",
  proposalMintRequestCid: "mr-1",
  proposalBurnRequestCid: "br-1",
  proposalIssuerCredentialCids: ["cred-1"],
  acceptNeedsBlockListShards: false,
  acceptBlockListShardCids: [],
};

describe("buildAcceptMintRequest", () => {
  it("sends the request, instrument, credentials and description", () => {
    expect(buildAcceptMintRequest(fields)).toEqual({
      type: "accept_mint_request",
      mint_request_cid: "mr-1",
      instrument_configuration_cid: "inst-1",
      issuer_credential_cids: ["cred-1"],
      description: "Holder passed KYC",
      block_list_shard_cids: [],
    });
  });

  it("sends the shards of an instrument that checks a block list", () => {
    expect(
      buildAcceptMintRequest({
        ...fields,
        acceptNeedsBlockListShards: true,
        acceptBlockListShardCids: ["shard-1", "shard-2"],
      }),
    ).toMatchObject({ block_list_shard_cids: ["shard-1", "shard-2"] });
  });

  it("refuses an instrument that checks a block list when no shard is picked", () => {
    expect(() => buildAcceptMintRequest({ ...fields, acceptNeedsBlockListShards: true })).toThrow(
      "Pick at least one Block List Shard",
    );
  });
});

describe("buildAcceptBurnRequest", () => {
  it("sends the burn request with the same context", () => {
    expect(buildAcceptBurnRequest(fields)).toEqual({
      type: "accept_burn_request",
      burn_request_cid: "br-1",
      instrument_configuration_cid: "inst-1",
      issuer_credential_cids: ["cred-1"],
      description: "Holder passed KYC",
      block_list_shard_cids: [],
    });
  });

  it("refuses an instrument that checks a block list when no shard is picked", () => {
    expect(() => buildAcceptBurnRequest({ ...fields, acceptNeedsBlockListShards: true })).toThrow(
      "Pick at least one Block List Shard",
    );
  });
});
