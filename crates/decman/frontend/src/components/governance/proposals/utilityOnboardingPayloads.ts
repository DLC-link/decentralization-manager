import type { ProposalState } from "./useProposalState";
import type { ProposalType } from "../../../types";

// The proposal payloads of the utility onboarding and settings forms.

const buildSetupUtility = ({
  proposalOperator,
  proposalProviderServiceCid,
  proposalInstrumentIdText,
  proposalCreateTransferRule,
  proposalCreateAllocationFactory,
}: Pick<
  ProposalState,
  | "proposalOperator"
  | "proposalProviderServiceCid"
  | "proposalInstrumentIdText"
  | "proposalCreateTransferRule"
  | "proposalCreateAllocationFactory"
>): ProposalType => {
  return {
    type: "setup_utility",
    provider_service_cid: proposalProviderServiceCid,
    operator: proposalOperator,
    instrument_id_text: proposalInstrumentIdText,
    additional_identifiers: [],
    create_transfer_rule: proposalCreateTransferRule,
    create_allocation_factory: proposalCreateAllocationFactory,
  };
};

const buildCreateProviderServiceRequest = ({
  proposalProvider,
  proposalOperator,
}: Pick<ProposalState, "proposalProvider" | "proposalOperator">): ProposalType => {
  return {
    type: "create_provider_service_request",
    operator: proposalOperator,
    provider: proposalProvider,
  };
};

const buildCreateUserServiceRequest = ({
  proposalOperator,
  proposalUser,
}: Pick<ProposalState, "proposalOperator" | "proposalUser">): ProposalType => {
  return {
    type: "create_user_service_request",
    operator: proposalOperator,
    user: proposalUser,
  };
};

const buildSetEnableResultContracts = ({
  proposalRegistrarServiceCid,
  proposalEnableResultContracts,
}: Pick<
  ProposalState,
  | "proposalRegistrarServiceCid"
  | "proposalEnableResultContracts"
>): ProposalType => {
  return {
    type: "set_enable_result_contracts",
    registrar_service_cid: proposalRegistrarServiceCid,
    enable_result_contracts:
      proposalEnableResultContracts === "clear"
        ? undefined
        : proposalEnableResultContracts === "true",
  };
};

const buildCreateDelegatedBatchedMarkersProxy = ({
  proposalOperator,
}: Pick<ProposalState, "proposalOperator">): ProposalType => {
  return {
    type: "create_delegated_batched_markers_proxy",
    operator: proposalOperator,
  };
};

const buildRequestDevNetFeaturedAppRight = ({
  amuletRulesCid,
}: Pick<ProposalState, "amuletRulesCid">): ProposalType => {
  return {
    type: "request_dev_net_featured_app_right",
    amulet_rules_cid: amuletRulesCid.trim(),
  };
};

export {
  buildSetupUtility,
  buildCreateProviderServiceRequest,
  buildCreateUserServiceRequest,
  buildSetEnableResultContracts,
  buildCreateDelegatedBatchedMarkersProxy,
  buildRequestDevNetFeaturedAppRight,
};
