import { useState } from "react";
import {
  Box,
  Collapse,
  Select,
  MenuItem,
  FormControl,
  InputLabel,
  Divider,
  ListSubheader,
} from "@mui/material";
import { API_BASE } from "../../../constants";
import { authenticatedFetch } from "../../../api";
import { TextHelp } from "../../FieldHelp";
import { SubmitButton } from "../fields/SubmitButton";
import { TokenCustodyForms } from "./TokenCustodyForms";
import {
  buildSetupCcPreapproval,
  buildSetupTokenPreapproval,
  buildGenericVote,
} from "./tokenCustodyPayloads";
import { TransferForm } from "./TransferForm";
import { buildTransfer, buildAcceptTransfer } from "./transferPayloads";
import { UtilityOnboardingForms } from "./UtilityOnboardingForms";
import {
  buildSetupUtility,
  buildCreateProviderServiceRequest,
  buildCreateUserServiceRequest,
  buildSetEnableResultContracts,
  buildCreateDelegatedBatchedMarkersProxy,
  buildRequestDevNetFeaturedAppRight,
} from "./utilityOnboardingPayloads";
import { RewardBeneficiariesForm } from "./RewardBeneficiariesForm";
import { buildSetProviderAppRewardBeneficiaries } from "./rewardBeneficiariesPayload";
import { CouponDelegationForms } from "./CouponDelegationForms";
import {
  buildSetupCouponReassignmentDelegation,
  buildRevokeCouponReassignmentDelegation,
} from "./couponDelegationPayloads";
import { MintingDelegationForms } from "./MintingDelegationForms";
import {
  buildSetupMintingDelegation,
  buildAcceptExternalPartySetup,
} from "./mintingDelegationPayloads";
import { CredentialForms } from "./CredentialForms";
import {
  buildOfferFreeCredential,
  buildAcceptFreeCredential,
  buildOfferPaidCredential,
  buildAcceptPaidCredential,
} from "./credentialPayloads";
import { TokenRequestForm } from "./TokenRequestForm";
import { buildAcceptMintRequest, buildAcceptBurnRequest } from "./tokenRequestPayloads";
import { RegistrarForms } from "./RegistrarForms";
import {
  buildCreateProviderConfiguration,
  buildCreateRegistrarServiceRequest,
  buildOnboardRegistrar,
} from "./registrarPayloads";
import { InstrumentForms } from "./InstrumentForms";
import {
  buildProvisionInstrument,
  buildCreateTransferRule,
  buildUpdateInstrumentConfiguration,
  buildOnboardInstrumentIssuers,
  buildOffboardInstrumentIssuers,
} from "./instrumentPayloads";
import { PROPOSAL_MENU, type FormProposalType } from "./proposalMenu";
import type { ProposalState } from "./useProposalState";
import type {
  Network,
  ProposalType,
  ProposeActionRequest,
} from "../../../types";

// The payload builder of each proposal type the menu offers.
const PROPOSAL_BUILDERS: {
  [T in FormProposalType]: (state: ProposalState) => ProposalType;
} = {
  setup_cc_preapproval: buildSetupCcPreapproval,
  setup_token_preapproval: buildSetupTokenPreapproval,
  transfer: buildTransfer,
  accept_transfer: buildAcceptTransfer,
  generic_vote: buildGenericVote,
  setup_utility: buildSetupUtility,
  create_provider_service_request: buildCreateProviderServiceRequest,
  create_user_service_request: buildCreateUserServiceRequest,
  set_provider_app_reward_beneficiaries: buildSetProviderAppRewardBeneficiaries,
  setup_coupon_reassignment_delegation: buildSetupCouponReassignmentDelegation,
  revoke_coupon_reassignment_delegation: buildRevokeCouponReassignmentDelegation,
  set_enable_result_contracts: buildSetEnableResultContracts,
  create_delegated_batched_markers_proxy: buildCreateDelegatedBatchedMarkersProxy,
  request_dev_net_featured_app_right: buildRequestDevNetFeaturedAppRight,
  setup_minting_delegation: buildSetupMintingDelegation,
  accept_external_party_setup: buildAcceptExternalPartySetup,
  accept_mint_request: buildAcceptMintRequest,
  accept_burn_request: buildAcceptBurnRequest,
  offer_free_credential: buildOfferFreeCredential,
  accept_free_credential: buildAcceptFreeCredential,
  offer_paid_credential: buildOfferPaidCredential,
  accept_paid_credential: buildAcceptPaidCredential,
  create_provider_configuration: buildCreateProviderConfiguration,
  create_registrar_service_request: buildCreateRegistrarServiceRequest,
  onboard_registrar: buildOnboardRegistrar,
  provision_instrument: buildProvisionInstrument,
  create_transfer_rule: buildCreateTransferRule,
  update_instrument_configuration: buildUpdateInstrumentConfiguration,
  onboard_instrument_issuers: buildOnboardInstrumentIssuers,
  offboard_instrument_issuers: buildOffboardInstrumentIssuers,
};

interface ProposalFormProps {
  /** The form fields and lookup lists, from useProposalState. */
  state: ProposalState;
  rulesContractId: string;
  network?: Network;
  setError: (error: string | null) => void;
  fetchGovernance: () => Promise<void>;
  onAfterAction?: () => void;
  onProposalCreated?: () => void;
  submitPortalEl?: HTMLElement | null;
}

/** The new-proposal form: the type menu, the fields of the chosen type and submit. */
export const ProposalForm = ({
  state,
  rulesContractId,
  network,
  setError,
  fetchGovernance,
  onAfterAction,
  onProposalCreated,
  submitPortalEl,
}: ProposalFormProps) => {
  const {
    partyId,
    proposalType,
    setProposalType,
    resetProposalForm,
    beneficiaryProblem,
    blockListProblem,
  } = state;
  const [proposalLoading, setProposalLoading] = useState(false);

  const handleSubmitProposal = async () => {
    if (!rulesContractId) return;
    setProposalLoading(true);
    setError(null);

    try {
      const proposal = PROPOSAL_BUILDERS[proposalType](state);

      const request: ProposeActionRequest = {
        party_id: partyId,
        rules_contract_id: rulesContractId,
        proposal,
      };

      const res = await authenticatedFetch(`${API_BASE}/governance/propose`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const errData = await res.json().catch(() => ({}));
        throw new Error(errData.error || "Failed to create proposal");
      }

      // Clear fields and let the host close the dialog. The created proposal
      // shows up in the notification queue — no separate success message
      // needed.
      resetProposalForm();
      await fetchGovernance();
      onAfterAction?.();
      onProposalCreated?.();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to create proposal");
    } finally {
      setProposalLoading(false);
    }
  };

  return (
    <Box>
      <Collapse in>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 1.5 }}>
          <FormControl size="small" fullWidth>
            <InputLabel>
              <TextHelp text="What kind of proposal to create. The form fields below adapt to the selected type.">
                Proposal Type
              </TextHelp>
            </InputLabel>
            <Select
              value={proposalType}
              label="Proposal Type"
              onChange={(e) => {
                const next = e.target.value as FormProposalType;
                if (next !== proposalType) resetProposalForm(next);
                setProposalType(next);
              }}
            >
              {PROPOSAL_MENU.map((entry, idx) => {
                switch (entry.kind) {
                  case "group":
                    return (
                      <ListSubheader key={idx} sx={{ color: "primary.main", fontWeight: 600 }}>
                        {entry.label}
                      </ListSubheader>
                    );
                  case "step":
                    return (
                      <ListSubheader key={idx} sx={{ fontStyle: "italic", lineHeight: 1.5, pl: 4 }}>
                        {entry.label}
                      </ListSubheader>
                    );
                  case "type":
                    return (
                      <MenuItem key={idx} value={entry.value}>
                        {entry.label}
                      </MenuItem>
                    );
                  case "divider":
                    return <Divider key={idx} />;
                }
              })}
            </Select>
          </FormControl>

          <Divider />

          <UtilityOnboardingForms state={state} network={network} />

          <TokenCustodyForms state={state} />

          <TransferForm state={state} />

          <CouponDelegationForms state={state} />

          <RewardBeneficiariesForm state={state} />

          <MintingDelegationForms state={state} />

          <TokenRequestForm state={state} />

          <CredentialForms state={state} />

          <RegistrarForms state={state} />

          <InstrumentForms state={state} />

          <SubmitButton
            label="Submit Proposal"
            loading={proposalLoading}
            disabled={
              proposalLoading ||
              beneficiaryProblem !== null ||
              blockListProblem !== null
            }
            onClick={handleSubmitProposal}
            portalEl={submitPortalEl}
          />
        </Box>
      </Collapse>
    </Box>
  );
};
