import { buildRequirements, credentialValidity, parsePartyLines } from "../proposalParse";
import type { ProposalState } from "./useProposalState";
import type { ProposalType } from "../../../types";

// The proposal payloads of the instrument forms.

const buildProvisionInstrument = ({
  proposalCredentialValidFrom,
  proposalCredentialValidUntil,
  proposalInstrumentIdText,
  proposalRegistrarServiceCid,
  proposalHolderRequirements,
  proposalIssuerRequirements,
  proposalInitialInstrumentIssuersText,
}: Pick<
  ProposalState,
  | "proposalCredentialValidFrom"
  | "proposalCredentialValidUntil"
  | "proposalInstrumentIdText"
  | "proposalRegistrarServiceCid"
  | "proposalHolderRequirements"
  | "proposalIssuerRequirements"
  | "proposalInitialInstrumentIssuersText"
>): ProposalType => {
  return {
    type: "provision_instrument",
    registrar_service_cid: proposalRegistrarServiceCid,
    instrument_id_text: proposalInstrumentIdText,
    // No identifier editor, matching the Setup Utility form; extra
    // identifiers go through the API directly.
    additional_identifiers: [],
    issuer_requirements: buildRequirements(
      proposalIssuerRequirements,
      "Issuer",
    ),
    holder_requirements: buildRequirements(
      proposalHolderRequirements,
      "Holder",
    ),
    initial_instrument_issuers: parsePartyLines(
      proposalInitialInstrumentIssuersText,
    ),
    ...credentialValidity(proposalCredentialValidFrom, proposalCredentialValidUntil),
  };
};

const buildCreateTransferRule = ({
  proposalRegistrarServiceCid,
}: Pick<ProposalState, "proposalRegistrarServiceCid">): ProposalType => {
  return {
    type: "create_transfer_rule",
    registrar_service_cid: proposalRegistrarServiceCid,
  };
};

const buildUpdateInstrumentConfiguration = ({
  proposalInstrumentIdText,
  proposalAdditionalIdentifiers,
  proposalInstrumentConfigurationCid,
  proposalRegistrarServiceCid,
  proposalHolderRequirements,
  proposalIssuerRequirements,
  configurationToUpdate,
  blockListProblem,
}: Pick<
  ProposalState,
  | "proposalInstrumentIdText"
  | "proposalAdditionalIdentifiers"
  | "proposalInstrumentConfigurationCid"
  | "proposalRegistrarServiceCid"
  | "proposalHolderRequirements"
  | "proposalIssuerRequirements"
  | "configurationToUpdate"
  | "blockListProblem"
>): ProposalType => {
  if (blockListProblem) throw new Error(blockListProblem);
  return {
    type: "update_instrument_configuration",
    registrar_service_cid: proposalRegistrarServiceCid,
    instrument_configuration_cid: proposalInstrumentConfigurationCid,
    instrument_id_text: proposalInstrumentIdText,
    additional_identifiers: proposalAdditionalIdentifiers.map((row, idx) => {
      // The id and scheme go out verbatim, so an unchanged identifier
      // stays unchanged. The ledger requires only a nonempty id; an
      // empty scheme is valid.
      const identifier = {
        source: row.source.trim(),
        id: row.id,
        scheme: row.scheme,
      };
      if (!identifier.source || !identifier.id) {
        throw new Error(
          `Additional identifier ${idx + 1}: source and id are required`,
        );
      }
      return identifier;
    }),
    issuer_requirements: buildRequirements(proposalIssuerRequirements, "Issuer"),
    holder_requirements: buildRequirements(proposalHolderRequirements, "Holder"),
    // The current flag, so the update keeps it.
    use_block_list: configurationToUpdate?.check_block_list === false ? false : null,
  };
};

const buildOnboardInstrumentIssuers = ({
  proposalCredentialValidFrom,
  proposalCredentialValidUntil,
  proposalInstrumentConfigurationCid,
  proposalInstrumentIssuersText,
}: Pick<
  ProposalState,
  | "proposalCredentialValidFrom"
  | "proposalCredentialValidUntil"
  | "proposalInstrumentConfigurationCid"
  | "proposalInstrumentIssuersText"
>): ProposalType => {
  return {
    type: "onboard_instrument_issuers",
    instrument_configuration_cid: proposalInstrumentConfigurationCid,
    instrument_issuers: parsePartyLines(proposalInstrumentIssuersText),
    ...credentialValidity(proposalCredentialValidFrom, proposalCredentialValidUntil),
  };
};

const buildOffboardInstrumentIssuers = ({
  proposalOffboardRows,
}: Pick<ProposalState, "proposalOffboardRows">): ProposalType => {
  return {
    type: "offboard_instrument_issuers",
    instrument_issuers: proposalOffboardRows.map((row) => ({
      instrument_issuer: row.party,
      credential_cids: row.cids,
    })),
  };
};

export {
  buildProvisionInstrument,
  buildCreateTransferRule,
  buildUpdateInstrumentConfiguration,
  buildOnboardInstrumentIssuers,
  buildOffboardInstrumentIssuers,
};
