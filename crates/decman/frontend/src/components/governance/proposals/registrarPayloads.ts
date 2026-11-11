import { buildRequirements } from "../proposalParse";
import type { ProposalState } from "./useProposalState";
import type { ProposalType } from "../../../types";

// The proposal payloads of the registrar onboarding forms.

const buildCreateProviderConfiguration = ({
  proposalProviderServiceCid,
  proposalRegistrarRequirements,
  proposalHolderRequirements,
}: Pick<
  ProposalState,
  | "proposalProviderServiceCid"
  | "proposalRegistrarRequirements"
  | "proposalHolderRequirements"
>): ProposalType => {
  return {
    type: "create_provider_configuration",
    provider_service_cid: proposalProviderServiceCid,
    registrar_requirements: buildRequirements(
      proposalRegistrarRequirements,
      "Registrar",
    ),
    holder_requirements: buildRequirements(
      proposalHolderRequirements,
      "Holder",
    ),
  };
};

const buildCreateRegistrarServiceRequest = ({
  proposalProvider,
  proposalOperator,
  proposalCreateTransferRule,
  proposalCreateAllocationFactory,
}: Pick<
  ProposalState,
  | "proposalProvider"
  | "proposalOperator"
  | "proposalCreateTransferRule"
  | "proposalCreateAllocationFactory"
>): ProposalType => {
  return {
    type: "create_registrar_service_request",
    operator: proposalOperator,
    provider: proposalProvider,
    create_transfer_rule: proposalCreateTransferRule,
    create_allocation_factory: proposalCreateAllocationFactory,
  };
};

const buildOnboardRegistrar = ({
  proposalProviderServiceCid,
  proposalRegistrarServiceRequestCid,
  proposalProviderConfigurationCid,
}: Pick<
  ProposalState,
  | "proposalProviderServiceCid"
  | "proposalRegistrarServiceRequestCid"
  | "proposalProviderConfigurationCid"
>): ProposalType => {
  return {
    type: "onboard_registrar",
    provider_service_cid: proposalProviderServiceCid,
    registrar_service_request_cid: proposalRegistrarServiceRequestCid,
    provider_configuration_cid: proposalProviderConfigurationCid,
  };
};

export {
  buildCreateProviderConfiguration,
  buildCreateRegistrarServiceRequest,
  buildOnboardRegistrar,
};
