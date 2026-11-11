import type { ProposalState } from "./useProposalState";
import type { ProposalType } from "../../../types";

// The proposal payloads of the Accept Mint Request and Accept Burn Request form.

const buildAcceptMintRequest = ({
  proposalDescription,
  proposalInstrumentConfigurationCid,
  proposalMintRequestCid,
  proposalIssuerCredentialCids,
  acceptNeedsBlockListShards,
  acceptBlockListShardCids,
}: Pick<
  ProposalState,
  | "proposalDescription"
  | "proposalInstrumentConfigurationCid"
  | "proposalMintRequestCid"
  | "proposalIssuerCredentialCids"
  | "acceptNeedsBlockListShards"
  | "acceptBlockListShardCids"
>): ProposalType => {
  if (acceptNeedsBlockListShards && acceptBlockListShardCids.length === 0) {
    throw new Error("Pick at least one Block List Shard");
  }
  return {
    type: "accept_mint_request",
    mint_request_cid: proposalMintRequestCid,
    instrument_configuration_cid: proposalInstrumentConfigurationCid,
    issuer_credential_cids: proposalIssuerCredentialCids,
    description: proposalDescription,
    block_list_shard_cids: acceptBlockListShardCids,
  };
};

const buildAcceptBurnRequest = ({
  proposalDescription,
  proposalInstrumentConfigurationCid,
  proposalBurnRequestCid,
  proposalIssuerCredentialCids,
  acceptNeedsBlockListShards,
  acceptBlockListShardCids,
}: Pick<
  ProposalState,
  | "proposalDescription"
  | "proposalInstrumentConfigurationCid"
  | "proposalBurnRequestCid"
  | "proposalIssuerCredentialCids"
  | "acceptNeedsBlockListShards"
  | "acceptBlockListShardCids"
>): ProposalType => {
  if (acceptNeedsBlockListShards && acceptBlockListShardCids.length === 0) {
    throw new Error("Pick at least one Block List Shard");
  }
  return {
    type: "accept_burn_request",
    burn_request_cid: proposalBurnRequestCid,
    instrument_configuration_cid: proposalInstrumentConfigurationCid,
    issuer_credential_cids: proposalIssuerCredentialCids,
    description: proposalDescription,
    block_list_shard_cids: acceptBlockListShardCids,
  };
};

export {
  buildAcceptMintRequest,
  buildAcceptBurnRequest,
};
