import { useCallback, useState } from "react";
import {
  Autocomplete,
  Box,
  Typography,
  Button,
  CircularProgress,
  Alert,
  Collapse,
  IconButton,
  TextField,
  Tooltip,
  Select,
  MenuItem,
  FormControl,
  InputLabel,
  Divider,
  Checkbox,
  FormControlLabel,
  ListSubheader,
} from "@mui/material";
import RefreshIcon from "@mui/icons-material/Refresh";
import { API_BASE } from "../../../constants";
import { authenticatedFetch } from "../../../api";
import { } from "../../../rewardBeneficiaries";
import {
  buildBillingParams,
  buildInitialDeposit,
  formatDecimal,
} from "../../../paidCredential";
import { fieldHelpAdornment, TextHelp } from "../../FieldHelp";
import { ContractSelect } from "../fields/ContractSelect";
import { CredentialOfferPicker } from "../fields/CredentialOfferPicker";
import { HelpTextField } from "../fields/HelpTextField";
import { RequirementRows, type RequirementRow } from "../fields/RequirementRows";
import { SubmitButton } from "../fields/SubmitButton";
import {
  buildRequirements,
  credentialValidity,
  parseClaimsText,
  parsePartyLines,
  requiredClaimsText,
} from "../proposalParse";
import {
  formatSplitShare,
  parseSplitWeights,
  splitFromWeights,
  splitRemainderRow,
  validateDelegationSplit,
} from "../rewardSplit";
import { TokenCustodyForms } from "./TokenCustodyForms";
import {
  buildSetupCcPreapproval,
  buildSetupTokenPreapproval,
  buildGenericVote,
} from "./tokenCustodyPayloads";
import { TransferForm } from "./TransferForm";
import { buildTransfer, buildAcceptTransfer } from "./transferPayloads";
import { AmuletRulesCidField } from "./AmuletRulesCidField";
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
import { PROPOSAL_MENU, type FormProposalType } from "./proposalMenu";
import type { ProposalState } from "./useProposalState";
import type {
  CouponReassignmentDelegationSummary,
  Network,
  PartyCredentialRequirement,
  ProposalType,
  ProposeActionRequest,
  TokenRequestInfo,
} from "../../../types";

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
    proposalProvider,
    setProposalProvider,
    proposalExpectedDso,
    setProposalExpectedDso,
    proposalDelegate,
    setProposalDelegate,
    proposalDelegationExpiresAt,
    setProposalDelegationExpiresAt,
    proposalAmuletMergeLimit,
    setProposalAmuletMergeLimit,
    proposalCredentialValidFrom,
    setProposalCredentialValidFrom,
    proposalCredentialValidUntil,
    setProposalCredentialValidUntil,
    proposalOperator,
    setProposalOperator,
    proposalDescription,
    setProposalDescription,
    proposalProviderServiceCid,
    setProposalProviderServiceCid,
    proposalInstrumentIdText,
    setProposalInstrumentIdText,
    proposalCreateTransferRule,
    setProposalCreateTransferRule,
    proposalCreateAllocationFactory,
    setProposalCreateAllocationFactory,
    proposalAdditionalIdentifiers,
    setProposalAdditionalIdentifiers,
    proposalInstrumentConfigurationCid,
    setProposalInstrumentConfigurationCid,
    proposalDelegationDso,
    setProposalDelegationDso,
    proposalDelegationAssigners,
    setProposalDelegationAssigners,
    proposalDelegationSplit,
    setProposalDelegationSplit,
    proposalPriorDelegation,
    setProposalPriorDelegation,
    activeDelegations,
    activeDelegationLoading,
    activeDelegationError,
    proposalRevokeDelegationCid,
    setProposalRevokeDelegationCid,
    proposalRegistrarServiceCid,
    setProposalRegistrarServiceCid,
    proposalHolder,
    setProposalHolder,
    proposalUserServiceCid,
    setProposalUserServiceCid,
    proposalCredentialId,
    setProposalCredentialId,
    proposalCredentialClaimsText,
    setProposalCredentialClaimsText,
    proposalCredentialOfferCid,
    setProposalCredentialOfferCid,
    proposalFeePerDayUsd,
    setProposalFeePerDayUsd,
    proposalBillingPeriodMinutes,
    setProposalBillingPeriodMinutes,
    proposalDepositTargetAmountUsd,
    setProposalDepositTargetAmountUsd,
    proposalHolderActivityWeight,
    setProposalHolderActivityWeight,
    proposalDepositInitialAmountUsd,
    setProposalDepositInitialAmountUsd,
    proposalDepositAmuletCids,
    setProposalDepositAmuletCids,
    proposalOpenMiningRoundCid,
    setProposalOpenMiningRoundCid,
    proposalFeaturedAppRightCid,
    setProposalFeaturedAppRightCid,
    openMiningRound,
    openMiningRoundError,
    openMiningRoundLoading,
    proposalMintRequestCid,
    setProposalMintRequestCid,
    proposalBurnRequestCid,
    setProposalBurnRequestCid,
    proposalIssuerCredentialCids,
    setProposalIssuerCredentialCids,
    proposalBlockListShardCids,
    setProposalBlockListShardCids,
    setBlockListShardsEdited,
    proposalRegistrarServiceRequestCid,
    setProposalRegistrarServiceRequestCid,
    proposalProviderConfigurationCid,
    setProposalProviderConfigurationCid,
    proposalRegistrarRequirements,
    setProposalRegistrarRequirements,
    proposalHolderRequirements,
    setProposalHolderRequirements,
    proposalIssuerRequirements,
    setProposalIssuerRequirements,
    proposalInitialInstrumentIssuersText,
    setProposalInitialInstrumentIssuersText,
    proposalInstrumentIssuersText,
    setProposalInstrumentIssuersText,
    proposalOffboardRows,
    setProposalOffboardRows,
    proposalExternalPartySetupCid,
    setProposalExternalPartySetupCid,
    amuletRulesCid,
    providerServices,
    userServices,
    credentialOffersLoading,
    ownAmulets,
    ownAmuletsLoading,
    mintRequestContracts,
    mintRequestsLoading,
    burnRequestContracts,
    burnRequestsLoading,
    availableCredentials,
    credentialsLoading,
    blockListShardsLoading,
    registrarServiceRequestsLoading,
    registrarServicesLoading,
    providerConfigurations,
    providerConfigurationsLoading,
    availableInstruments,
    instrumentsLoading,
    servicesLoading,
    acceptableCredentialOffers,
    acceptablePaidCredentialOffers,
    chosenPaidOffer,
    chosenDepositUsd,
    acceptableRegistrarServiceRequests,
    ownRegistrarServices,
    ownInstruments,
    configurationToUpdate,
    configurationToUpdateUnreadable,
    fetchOpenMiningRound,
    resetProposalForm,
    beneficiaryProblem,
    blockListProblem,
    acceptInstrument,
    acceptNeedsBlockListShards,
    registrarBlockListShards,
    acceptBlockListShardCids,
  } = state;
  const [proposalLoading, setProposalLoading] = useState(false);

  // The update replaces every field, so picking a configuration fills the
  // form with its current values. A field left empty is cleared.
  const selectConfigurationToUpdate = (cid: string) => {
    setProposalInstrumentConfigurationCid(cid);
    const instrument = ownInstruments.find((inst) => inst.contract_id === cid);
    const current = instrument?.configuration;
    const toRows = (requirements: PartyCredentialRequirement[]): RequirementRow[] =>
      requirements.map((requirement) => ({
        issuer: requirement.issuer,
        claimsText: requiredClaimsText(requirement.required_claims),
        claims: requirement.required_claims,
      }));
    setProposalInstrumentIdText(instrument?.instrument_id ?? "");
    setProposalAdditionalIdentifiers(current?.additional_identifiers ?? []);
    setProposalIssuerRequirements(toRows(current?.issuer_requirements ?? []));
    setProposalHolderRequirements(toRows(current?.holder_requirements ?? []));
  };

  // Candidate credentials for one offboard row: self-issued by this governance
  // party, carrying claims that all name the row's party, and tagged as an
  // instrument-issuer credential so a dual-role decparty's registrar
  // credentials stay out.
  const offboardableCredentialsFor = useCallback(
    (party: string) =>
      availableCredentials.filter(
        (c) =>
          c.issuer === partyId &&
          c.holder === partyId &&
          c.claims.length > 0 &&
          c.claims.every((cl) => cl.subject === party) &&
          c.credential_id.includes("-instrument-issuer-credential/"),
      ),
    [availableCredentials, partyId],
  );

  // UserService select shared by the credential proposal forms.
  const renderUserServiceSelect = (help: string) => (
    <ContractSelect
      label="UserService Contract ID"
      help={help}
      value={proposalUserServiceCid}
      onChange={setProposalUserServiceCid}
      items={userServices}
      loading={servicesLoading}
      loadingText="Loading services…"
      emptyText="No user services found"
    />
  );

  // One delegation field, three states: nothing to pick, one prefilled, or a
  // dropdown when the ledger holds several. Shared by the setup and revoke
  // forms, which differ only in wording. Several active is an anomaly the
  // singleton guard cannot prevent, so it is called out rather than hidden.
  const delegationPicker = (opts: {
    label: string;
    value: string;
    onChange: (v: string) => void;
    help: string;
    emptyText: string;
    /// What going ahead on a failed read costs, so the warning names the stake.
    blindRisk: string;
  }) => {
    const many = activeDelegations.length > 1;
    const one = activeDelegations.length === 1 ? activeDelegations[0] : null;
    const describe = (d: CouponReassignmentDelegationSummary) =>
      `${d.assigners.length} assigner${d.assigners.length === 1 ? "" : "s"}, ` +
      `${d.beneficiary_count} beneficiar${d.beneficiary_count === 1 ? "y" : "ies"}`;
    return (
      <>
        {many && (
          <Alert severity="warning" sx={{ mb: 1 }}>
            <Typography variant="caption" component="div">
              This party has <strong>{activeDelegations.length} active delegations</strong>.
              Only one should exist. The automation uses the newest, marked{" "}
              <em>in use</em> below; the others are inert but still exerciseable.
            </Typography>
          </Alert>
        )}
        <TextField
          label={opts.label}
          value={opts.value}
          onChange={(e) => opts.onChange(e.target.value)}
          fullWidth
          select={many}
          required={activeDelegations.length > 0}
          // Disabled only when the ledger genuinely holds none. After a failed
          // read we do not know, so leave it typeable — telling someone to fill
          // a field in by hand while disabling it is worse than either alone.
          disabled={activeDelegations.length === 0 && !activeDelegationError}
          error={!!activeDelegationError}
          helperText={
            activeDelegationLoading
              ? "Reading this party's current delegations…"
              : activeDelegationError
                ? `${activeDelegationError} Fill it in by hand or retry — ${opts.blindRisk}`
                : many
                  ? "Pick the delegation this vote acts on."
                  : one
                    ? `Prefilled with this party's active delegation (${describe(one)}).`
                    : opts.emptyText
          }
          slotProps={
            many
              ? undefined
              : {
                  input: {
                    sx: { fontFamily: "var(--font-mono)", fontSize: "0.8rem" },
                    endAdornment: fieldHelpAdornment(opts.help, `Help for ${opts.label}`),
                  },
                }
          }
        >
          {many &&
            activeDelegations.map((d, idx) => (
              <MenuItem key={d.cid} value={d.cid}>
                <Typography variant="caption" sx={{ fontFamily: "var(--font-mono)" }}>
                  {d.cid.slice(0, 24)}… — {describe(d)}
                  {idx === 0 ? " — in use" : ""}
                </Typography>
              </MenuItem>
            ))}
        </TextField>
      </>
    );
  };

  const handleSubmitProposal = async () => {
    if (!rulesContractId) return;
    setProposalLoading(true);
    setError(null);

    try {
      let proposal: ProposalType;
      switch (proposalType) {
        case "setup_cc_preapproval":
          proposal = buildSetupCcPreapproval(state);
          break;
        case "setup_token_preapproval":
          proposal = buildSetupTokenPreapproval(state);
          break;
        case "transfer":
          proposal = buildTransfer(state);
          break;
        case "accept_transfer":
          proposal = buildAcceptTransfer(state);
          break;
        case "generic_vote":
          proposal = buildGenericVote(state);
          break;
        case "setup_utility":
          proposal = buildSetupUtility(state);
          break;
        case "create_provider_service_request":
          proposal = buildCreateProviderServiceRequest(state);
          break;
        case "create_user_service_request":
          proposal = buildCreateUserServiceRequest(state);
          break;
        case "set_provider_app_reward_beneficiaries":
          proposal = buildSetProviderAppRewardBeneficiaries(state);
          break;
        case "setup_coupon_reassignment_delegation": {
          const assigners = proposalDelegationAssigners
            .map((a) => a.trim())
            .filter((a) => a.length > 0);
          if (assigners.length === 0) {
            throw new Error("At least one assigner is required");
          }
          if (new Set(assigners).size !== assigners.length) {
            throw new Error("Assigners must be unique");
          }
          const splitError = validateDelegationSplit(proposalDelegationSplit);
          if (splitError) {
            throw new Error(splitError);
          }
          // Submit the derived decimals, not the weights — the delegation bakes
          // in exact percentages and that is what a confirmer reviews.
          const weights = parseSplitWeights(proposalDelegationSplit)!;
          const shares = splitFromWeights(weights)!;
          const split = proposalDelegationSplit.map((b, idx) => ({
            beneficiary: b.beneficiary.trim(),
            percentage: formatSplitShare(shares[idx]),
          }));
          proposal = {
            type: "setup_coupon_reassignment_delegation",
            dso: proposalDelegationDso.trim(),
            assigners,
            new_beneficiaries: split,
            prior_delegation: proposalPriorDelegation.trim() || undefined,
          };
          break;
        }
        case "revoke_coupon_reassignment_delegation":
          proposal = {
            type: "revoke_coupon_reassignment_delegation",
            delegation: proposalRevokeDelegationCid.trim(),
          };
          break;
        case "set_enable_result_contracts":
          proposal = buildSetEnableResultContracts(state);
          break;
        case "create_delegated_batched_markers_proxy":
          proposal = buildCreateDelegatedBatchedMarkersProxy(state);
          break;
        case "request_dev_net_featured_app_right":
          proposal = buildRequestDevNetFeaturedAppRight(state);
          break;
        case "setup_minting_delegation": {
          const expiresAtMs = new Date(proposalDelegationExpiresAt).getTime();
          if (!Number.isFinite(expiresAtMs)) {
            throw new Error("Expires At must be a valid date and time");
          }
          // Micros beyond MAX_SAFE_INTEGER (~year 2255) would silently lose
          // precision; the datetime-local input allows dates up to year 9999.
          if (!Number.isSafeInteger(expiresAtMs * 1000)) {
            throw new Error("Expires At is too far in the future");
          }
          const mergeLimit = Number(proposalAmuletMergeLimit);
          if (!Number.isInteger(mergeLimit) || mergeLimit <= 0) {
            throw new Error("Amulet Merge Limit must be a positive integer");
          }
          proposal = {
            type: "setup_minting_delegation",
            delegate: proposalDelegate,
            dso: proposalExpectedDso,
            expires_at_micros: expiresAtMs * 1000,
            amulet_merge_limit: mergeLimit,
            description: proposalDescription,
          };
          break;
        }
        case "accept_external_party_setup": {
          const proposalCid = proposalExternalPartySetupCid.trim();
          if (!proposalCid) {
            throw new Error(
              "External Party Setup Proposal Contract Id must not be empty",
            );
          }
          proposal = {
            type: "accept_external_party_setup",
            proposal_cid: proposalCid,
          };
          break;
        }
        case "accept_mint_request":
          if (acceptNeedsBlockListShards && acceptBlockListShardCids.length === 0) {
            throw new Error("Pick at least one Block List Shard");
          }
          proposal = {
            type: "accept_mint_request",
            mint_request_cid: proposalMintRequestCid,
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
            issuer_credential_cids: proposalIssuerCredentialCids,
            description: proposalDescription,
            block_list_shard_cids: acceptBlockListShardCids,
          };
          break;
        case "accept_burn_request":
          if (acceptNeedsBlockListShards && acceptBlockListShardCids.length === 0) {
            throw new Error("Pick at least one Block List Shard");
          }
          proposal = {
            type: "accept_burn_request",
            burn_request_cid: proposalBurnRequestCid,
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
            issuer_credential_cids: proposalIssuerCredentialCids,
            description: proposalDescription,
            block_list_shard_cids: acceptBlockListShardCids,
          };
          break;
        case "offer_free_credential": {
          const claims = parseClaimsText(proposalCredentialClaimsText);
          proposal = {
            type: "offer_free_credential",
            user_service_cid: proposalUserServiceCid,
            holder: proposalHolder,
            id: proposalCredentialId,
            description: proposalDescription,
            claims,
          };
          break;
        }
        case "accept_free_credential":
          proposal = {
            type: "accept_free_credential",
            user_service_cid: proposalUserServiceCid,
            credential_offer_cid: proposalCredentialOfferCid,
          };
          break;
        case "offer_paid_credential":
          proposal = {
            type: "offer_paid_credential",
            user_service_cid: proposalUserServiceCid,
            holder: proposalHolder,
            id: proposalCredentialId,
            description: proposalDescription,
            claims: parseClaimsText(proposalCredentialClaimsText),
            billing_params: buildBillingParams({
              feePerDayUsd: proposalFeePerDayUsd,
              billingPeriodMinutes: proposalBillingPeriodMinutes,
              depositTargetAmountUsd: proposalDepositTargetAmountUsd,
              holderActivityWeight: proposalHolderActivityWeight,
            }),
            ...buildInitialDeposit(proposalDepositInitialAmountUsd),
          };
          break;
        case "accept_paid_credential": {
          if (proposalDepositAmuletCids.length === 0) {
            throw new Error("Pick at least one Deposit Amulet");
          }
          const featuredAppRightCid = proposalFeaturedAppRightCid.trim();
          proposal = {
            type: "accept_paid_credential",
            user_service_cid: proposalUserServiceCid,
            credential_offer_cid: proposalCredentialOfferCid,
            deposit_amulet_cids: proposalDepositAmuletCids,
            app_transfer_context: {
              amulet_rules_cid: amuletRulesCid.trim(),
              open_mining_round_cid: proposalOpenMiningRoundCid.trim(),
              ...(featuredAppRightCid ? { featured_app_right_cid: featuredAppRightCid } : {}),
            },
            // The vote's description shows this amount, and execution checks
            // it against the offer.
            ...(chosenDepositUsd ? { deposit_amount_usd: chosenDepositUsd } : {}),
          };
          break;
        }
        case "create_provider_configuration":
          proposal = {
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
          break;
        case "create_registrar_service_request":
          proposal = {
            type: "create_registrar_service_request",
            operator: proposalOperator,
            provider: proposalProvider,
            create_transfer_rule: proposalCreateTransferRule,
            create_allocation_factory: proposalCreateAllocationFactory,
          };
          break;
        case "onboard_registrar":
          proposal = {
            type: "onboard_registrar",
            provider_service_cid: proposalProviderServiceCid,
            registrar_service_request_cid: proposalRegistrarServiceRequestCid,
            provider_configuration_cid: proposalProviderConfigurationCid,
          };
          break;
        case "provision_instrument":
          proposal = {
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
          break;
        case "create_transfer_rule":
          proposal = {
            type: "create_transfer_rule",
            registrar_service_cid: proposalRegistrarServiceCid,
          };
          break;
        case "update_instrument_configuration":
          if (blockListProblem) throw new Error(blockListProblem);
          proposal = {
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
          break;
        case "onboard_instrument_issuers":
          proposal = {
            type: "onboard_instrument_issuers",
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
            instrument_issuers: parsePartyLines(proposalInstrumentIssuersText),
            ...credentialValidity(proposalCredentialValidFrom, proposalCredentialValidUntil),
          };
          break;
        case "offboard_instrument_issuers":
          proposal = {
            type: "offboard_instrument_issuers",
            instrument_issuers: proposalOffboardRows.map((row) => ({
              instrument_issuer: row.party,
              credential_cids: row.cids,
            })),
          };
          break;
      }

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

          {proposalType === "revoke_coupon_reassignment_delegation" &&
            delegationPicker({
              label: "Delegation Contract ID",
              value: proposalRevokeDelegationCid,
              onChange: setProposalRevokeDelegationCid,
              help: "Contract id of the active CouponReassignmentDelegation to archive. It is read from the ledger, not typed. Reassignment stops for this party until a new delegation is voted in.",
              emptyText:
                "This party has no active delegation, so there is nothing to revoke.",
              blindRisk:
                "a wrong contract id fails at execute, after the vote.",
            })}

          {proposalType === "setup_coupon_reassignment_delegation" && (
            <>
              <Alert severity="info" sx={{ mb: 1 }}>
                <Typography variant="caption" component="div">
                  The split below is <strong>baked into the delegation</strong>.
                  Changing it later needs another vote. Two rules the ledger
                  enforces exactly, and which reject a vote at execute:
                </Typography>
                <Typography variant="caption" component="ul" sx={{ pl: 2, mb: 0, mt: 0.5 }}>
                  <li>
                    Shares must sum to <strong>exactly 1.0</strong>, compared as
                    exact Decimal — so an even 3-way split is not expressible as
                    a repeated decimal. Enter <strong>whole-number weights</strong>{" "}
                    and the exact percentages are derived; the rounding
                    remainder goes to the largest weight, so a confirmer can
                    reproduce the split from the weights alone.
                  </li>
                  <li>
                    Nothing is implicitly left to this party. To keep a
                    remainder, <strong>add this party as its own beneficiary</strong>.
                  </li>
                </Typography>
              </Alert>
              <HelpTextField
                label="DSO Party"
                value={proposalDelegationDso}
                onChange={(e) => setProposalDelegationDso(e.target.value)}
                fullWidth
                required
                help="The DSO whose coupons this delegation may assign. Anyone can mint a coupon naming themselves DSO, so the automation ignores every coupon whose DSO is not this one. Getting it wrong silently assigns nothing."
              />
              {delegationPicker({
                label: "Replaces Delegation",
                value: proposalPriorDelegation,
                onChange: setProposalPriorDelegation,
                help: "Contract id of the delegation this one replaces — it is archived in the same transaction. It is read from the ledger, not typed: creating a second delegation stops assignment entirely, so leaving it blank while one is live is rejected with 409.",
                emptyText:
                  "This party has no active delegation, so there is nothing to replace.",
                blindRisk:
                  "leaving it blank while a delegation is live is rejected with 409.",
              })}
              <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
                <TextHelp text="Member parties allowed to run the reassignment. Any ONE of them suffices (1-of-n), so this is liveness, not a threshold. An assigner's participant must also host this governance party, or it cannot read the coupons.">
                  Assigners (any one may reassign)
                </TextHelp>
              </Typography>
              {/* Same shape as a beneficiary row, without the weight: the
                  party id gets the full width, Remove sits beneath it.
                  Sharing the row truncated the id past its namespace. */}
              {proposalDelegationAssigners.map((a, idx) => (
                <Box key={idx} sx={{ mb: 2 }}>
                  <TextField
                    label={`Assigner ${idx + 1}`}
                    value={a}
                    onChange={(e) => {
                      const updated = [...proposalDelegationAssigners];
                      updated[idx] = e.target.value;
                      setProposalDelegationAssigners(updated);
                    }}
                    size="small"
                    fullWidth
                    slotProps={{
                      input: { sx: { fontFamily: "var(--font-mono)", fontSize: "0.8rem" } },
                    }}
                  />
                  <Box sx={{ display: "flex", mt: 1 }}>
                    <Button
                      size="small"
                      color="error"
                      onClick={() =>
                        setProposalDelegationAssigners(
                          proposalDelegationAssigners.filter((_, i) => i !== idx),
                        )
                      }
                    >
                      Remove
                    </Button>
                  </Box>
                </Box>
              ))}
              <Box>
                <Button
                  size="small"
                  onClick={() =>
                    setProposalDelegationAssigners([...proposalDelegationAssigners, ""])
                  }
                >
                  Add Assigner
                </Button>
              </Box>
              <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
                <TextHelp text="Who receives the reassigned coupons, and in what share. Enter whole-number weights — equal thirds is 1/1/1 — and the exact percentages are derived below. Each beneficiary mints its own coupons afterwards; this party does not mint for them.">
                  Beneficiary split (party + weight)
                </TextHelp>
              </Typography>
              {(() => {
                const weights = parseSplitWeights(proposalDelegationSplit);
                const shares = weights ? splitFromWeights(weights) : null;
                const remainderRow = weights ? splitRemainderRow(weights) : -1;
                return (
                  <>
                    {/* Party id on its own full-width row, weight beneath it.
                        A party id is ~70 characters and only its prefix
                        distinguishes two parties in the same namespace, so
                        sharing the row with the weight hides the part that
                        matters. */}
                    {proposalDelegationSplit.map((b, idx) => (
                      <Box key={idx} sx={{ mb: 2 }}>
                        <TextField
                          label={`Beneficiary Party ${idx + 1}`}
                          value={b.beneficiary}
                          onChange={(e) => {
                            const updated = [...proposalDelegationSplit];
                            updated[idx] = { ...b, beneficiary: e.target.value };
                            setProposalDelegationSplit(updated);
                          }}
                          size="small"
                          fullWidth
                          slotProps={{
                            input: { sx: { fontFamily: "var(--font-mono)", fontSize: "0.8rem" } },
                          }}
                        />
                        <Box
                          sx={{
                            display: "flex",
                            gap: 1,
                            mt: 1,
                            alignItems: "center",
                          }}
                        >
                          <HelpTextField
                            label="Weight"
                            value={b.weight}
                            onChange={(e) => {
                              const updated = [...proposalDelegationSplit];
                              updated[idx] = { ...b, weight: e.target.value };
                              setProposalDelegationSplit(updated);
                            }}
                            size="small"
                            sx={{ width: 110 }}
                            help="A whole number. Only the ratio matters: 1/1/1 is equal thirds, 80/20 is four to one. The exact percentage is derived."
                          />
                          <Typography
                            variant="caption"
                            sx={{ flex: 1, fontFamily: "var(--font-mono)" }}
                            color={shares ? "text.primary" : "text.disabled"}
                          >
                            {shares
                              ? `${formatSplitShare(shares[idx])}${idx === remainderRow && shares.length > 1 ? " ⟵ +rem" : ""}`
                              : "—"}
                          </Typography>
                          <Button
                            size="small"
                            color="error"
                            onClick={() =>
                              setProposalDelegationSplit(
                                proposalDelegationSplit.filter((_, i) => i !== idx),
                              )
                            }
                          >
                            Remove
                          </Button>
                        </Box>
                      </Box>
                    ))}
                    <Box sx={{ display: "flex", alignItems: "center", gap: 2 }}>
                      <Button
                        size="small"
                        onClick={() =>
                          setProposalDelegationSplit([
                            ...proposalDelegationSplit,
                            { beneficiary: "", weight: "1" },
                          ])
                        }
                      >
                        Add Beneficiary
                      </Button>
                      {proposalDelegationSplit.length > 0 &&
                        (() => {
                          const err = validateDelegationSplit(proposalDelegationSplit);
                          return (
                            <Typography
                              variant="caption"
                              color={err ? "error.main" : "success.main"}
                            >
                              {err ??
                                `Sums to exactly 1.0${
                                  shares && shares.length > 1
                                    ? ` — the rounding remainder goes to the largest weight (row ${remainderRow + 1})`
                                    : ""
                                }`}
                            </Typography>
                          );
                        })()}
                    </Box>
                  </>
                );
              })()}
            </>
          )}

          <RewardBeneficiariesForm state={state} />

          {proposalType === "setup_minting_delegation" && (
            <>
              <HelpTextField
                size="small"
                label="Delegate Party"
                value={proposalDelegate}
                onChange={(e) => setProposalDelegate(e.target.value)}
                fullWidth
                required
                help="Party id of the validator node operator authorized to mint the decentralized party's reward coupons. It accepts the resulting MintingDelegationProposal out-of-band via the wallet API."
              />
              <HelpTextField
                size="small"
                label="DSO Party"
                value={proposalExpectedDso}
                onChange={(e) => setProposalExpectedDso(e.target.value)}
                fullWidth
                required
                help="Party id of the Splice DSO; the delegation's mint transfers verify the AmuletRules contract belongs to this DSO."
              />
              <HelpTextField
                size="small"
                label="Expires At"
                type="datetime-local"
                value={proposalDelegationExpiresAt}
                onChange={(e) =>
                  setProposalDelegationExpiresAt(e.target.value)
                }
                fullWidth
                required
                slotProps={{
                  inputLabel: { shrink: true },
                }}
                help="When the delegation stops being valid. There is no auto-renewal — a new proposal must be voted in before this time to keep collecting rewards."
              />
              <HelpTextField
                size="small"
                label="Amulet Merge Limit"
                type="number"
                value={proposalAmuletMergeLimit}
                onChange={(e) => setProposalAmuletMergeLimit(e.target.value)}
                fullWidth
                required
                help="Number of amulet contracts to keep after auto-merging; the delegate merges contracts once there are strictly more than this number. Must be positive."
              />
              <HelpTextField
                size="small"
                label="Description"
                value={proposalDescription}
                onChange={(e) => setProposalDescription(e.target.value)}
                fullWidth
                required
                help="Free-form note recorded on the proposal, e.g. why this delegation is being set up."
              />
            </>
          )}

          {proposalType === "accept_external_party_setup" && (
            <HelpTextField
              size="small"
              label="External Party Setup Proposal Contract Id"
              value={proposalExternalPartySetupCid}
              onChange={(e) =>
                setProposalExternalPartySetupCid(e.target.value)
              }
              fullWidth
              required
              help="Contract id of the ExternalPartySetupProposal the validator operator created (via POST /api/validator/v0/admin/external-party/setup-proposal). Accepting it creates the decentralized party's ValidatorRight + TransferPreapproval, unblocking reward collection."
            />
          )}

          {(proposalType === "accept_mint_request" || proposalType === "accept_burn_request") && (() => {
            const isMint = proposalType === "accept_mint_request";
            const requestContracts = isMint ? mintRequestContracts : burnRequestContracts;
            const requestCid = isMint ? proposalMintRequestCid : proposalBurnRequestCid;
            const setRequestCid = isMint ? setProposalMintRequestCid : setProposalBurnRequestCid;
            const requestLabel = isMint ? "MintRequest" : "BurnRequest";
            const requestsLoading = isMint ? mintRequestsLoading : burnRequestsLoading;
            // Mint = tokens created (green), burn = tokens destroyed (red).
            const accentColor = isMint ? "success.main" : "error.main";
            // Selecting a request prefills the matching InstrumentConfiguration
            // so the user doesn't have to re-pick the token by hand.
            const prefillInstrument = (req: TokenRequestInfo) => {
              const inst = availableInstruments.find(
                (i) =>
                  i.instrument_admin === req.instrument_admin &&
                  i.instrument_id === req.instrument_id,
              );
              if (!inst) return;
              setProposalInstrumentConfigurationCid(inst.contract_id);
              // The registrar's active shards divide all party hashes
              // between them, so together they cover every holder.
              setProposalBlockListShardCids(
                inst.configuration?.check_block_list === true
                  ? registrarBlockListShards(inst.instrument_admin).map(
                      (shard) => shard.contract_id,
                    )
                  : [],
              );
              setBlockListShardsEdited(false);
            };
            const shardOptions = registrarBlockListShards(acceptInstrument?.instrument_admin);
            return (
              <>
                <Autocomplete
                  size="small"
                  freeSolo
                  options={requestContracts}
                  value={requestCid}
                  loading={requestsLoading}
                  onChange={(_event, value) => {
                    if (typeof value === "string" || value === null) {
                      setRequestCid(value ?? "");
                    } else {
                      setRequestCid(value.contract_id);
                      prefillInstrument(value);
                    }
                  }}
                  onInputChange={(_event, value, reason) => {
                    if (reason === "input") setRequestCid(value);
                  }}
                  getOptionLabel={(option) => {
                    if (typeof option === "string") return option;
                    const holderName = option.holder.split("::")[0];
                    const amount = option.amount.replace(/\.?0+$/, "");
                    const cidTail = option.contract_id.slice(-8);
                    return `${holderName} → ${amount} ${option.instrument_id} (…${cidTail})`;
                  }}
                  getOptionDisabled={(option) => {
                    if (typeof option === "string") return false;
                    const exp = option.expires_at ?? 0;
                    return exp > 0 && exp <= Math.floor(Date.now() / 1000);
                  }}
                  isOptionEqualToValue={(option, value) =>
                    typeof value === "string"
                      ? option.contract_id === value
                      : option.contract_id === value.contract_id
                  }
                  renderOption={(props, option) => {
                    if (typeof option === "string") {
                      return <li {...props}>{option}</li>;
                    }
                    const holderName = option.holder.split("::")[0];
                    const amount = option.amount.replace(/\.?0+$/, "");
                    const cidTail = option.contract_id.slice(-8);
                    const exp = option.expires_at ?? 0;
                    const isExpired =
                      exp > 0 && exp <= Math.floor(Date.now() / 1000);
                    return (
                      <li {...props} key={option.contract_id}>
                        <Box
                          sx={{
                            display: "flex",
                            flexDirection: "column",
                            gap: 0.25,
                            opacity: isExpired ? 0.6 : 1,
                          }}
                        >
                          <Typography variant="body2">
                            {holderName} →{" "}
                            <Box
                              component="span"
                              sx={{ color: accentColor, fontWeight: 600 }}
                            >
                              {amount} {option.instrument_id}
                            </Box>{" "}
                            (…{cidTail})
                          </Typography>
                          {isExpired && (
                            <Typography variant="caption" color="warning.main">
                              Expired {new Date(exp * 1000).toLocaleString()}
                            </Typography>
                          )}
                        </Box>
                      </li>
                    );
                  }}
                  renderInput={(params) => (
                    <TextField
                      {...params}
                      label={
                        <TextHelp
                          text={
                            isMint
                              ? "MintRequest contract created by the holder that this proposal will accept."
                              : "BurnRequest contract created by the holder that this proposal will accept."
                          }
                        >
                          {requestLabel}
                        </TextHelp>
                      }
                      required
                      helperText={
                        requestsLoading
                          ? `Loading open ${isMint ? "mint" : "burn"} requests…`
                          : requestContracts.length === 0
                            ? `No ${requestLabel} contracts found — holder must create one first`
                            : "Pick an open request, or paste a contract id"
                      }
                    />
                  )}
                />
                <ContractSelect
                  label="Instrument"
                  help="Instrument the request was made against. Derived automatically from the selected request."
                  value={proposalInstrumentConfigurationCid}
                  onChange={setProposalInstrumentConfigurationCid}
                  items={availableInstruments}
                  loading={instrumentsLoading}
                  loadingText="Loading instruments…"
                  emptyText="No instruments found — run SetupUtility first"
                  rowLabel={(inst) => <>{inst.instrument_id} ({inst.contract_id.slice(0, 8)}…)</>}
                  disabled
                />
                <Autocomplete
                  size="small"
                  multiple
                  freeSolo
                  options={availableCredentials}
                  value={proposalIssuerCredentialCids}
                  loading={credentialsLoading}
                  onChange={(_event, values) => {
                    setProposalIssuerCredentialCids(
                      values.map((value) =>
                        typeof value === "string" ? value : value.contract_id,
                      ),
                    );
                  }}
                  getOptionLabel={(option) => {
                    // Selected values are stored as bare cids; label them
                    // with the credential id when the contract is known.
                    const cid = typeof option === "string" ? option : option.contract_id;
                    const known =
                      typeof option === "string"
                        ? availableCredentials.find((c) => c.contract_id === option)
                        : option;
                    const cidTail = cid.slice(-8);
                    return known ? `${known.credential_id} (…${cidTail})` : cid;
                  }}
                  isOptionEqualToValue={(option, value) =>
                    typeof value === "string"
                      ? option.contract_id === value
                      : option.contract_id === value.contract_id
                  }
                  renderOption={(props, option) => {
                    const cidTail = option.contract_id.slice(-8);
                    // The claims' subjects name the parties the credential
                    // attests for — usually the request's holder.
                    const subjects = [
                      ...new Set(option.claims.map((claim) => claim.subject.split("::")[0])),
                    ].join(", ");
                    return (
                      <li {...props} key={option.contract_id}>
                        <Box sx={{ display: "flex", flexDirection: "column", gap: 0.25 }}>
                          <Typography variant="body2">
                            {option.credential_id} (…{cidTail})
                          </Typography>
                          {subjects && (
                            <Typography variant="caption" color="text.secondary">
                              attests for {subjects}
                            </Typography>
                          )}
                        </Box>
                      </li>
                    );
                  }}
                  renderInput={(params) => (
                    <TextField
                      {...params}
                      label={
                        <TextHelp
                          text={
                            isMint
                              ? "Credentials proving the mint holder meets the instrument's issuer requirements. Leave empty for instruments without issuer requirements."
                              : "Credentials proving the burn holder meets the instrument's issuer requirements. Leave empty for instruments without issuer requirements."
                          }
                        >
                          Issuer Credentials
                        </TextHelp>
                      }
                      helperText={
                        credentialsLoading
                          ? "Loading credentials…"
                          : "Pick the holder's credentials, or paste contract ids. Optional for instruments without issuer requirements."
                      }
                    />
                  )}
                />
                {acceptNeedsBlockListShards && (
                  <Autocomplete
                    size="small"
                    multiple
                    freeSolo
                    options={shardOptions}
                    value={proposalBlockListShardCids}
                    loading={blockListShardsLoading}
                    onChange={(_event, values) => {
                      setBlockListShardsEdited(true);
                      setProposalBlockListShardCids(
                        values.map((value) =>
                          typeof value === "string" ? value : value.contract_id,
                        ),
                      );
                    }}
                    getOptionLabel={(option) => {
                      // Selected values are stored as bare cids; label them
                      // with the shard's prefix when the contract is known.
                      const cid = typeof option === "string" ? option : option.contract_id;
                      const known =
                        typeof option === "string"
                          ? shardOptions.find((s) => s.contract_id === option)
                          : option;
                      return known ? `Prefix ${known.prefix} (…${cid.slice(-8)})` : cid;
                    }}
                    isOptionEqualToValue={(option, value) =>
                      typeof value === "string"
                        ? option.contract_id === value
                        : option.contract_id === value.contract_id
                    }
                    renderInput={(params) => (
                      <TextField
                        {...params}
                        label={
                          <TextHelp text="The registry checks the holder against these shards of the registrar's block list. Together, all of the registrar's shards cover every holder.">
                            Block List Shards
                          </TextHelp>
                        }
                        required
                        helperText={
                          blockListShardsLoading
                            ? "Loading block list shards…"
                            : shardOptions.length === 0
                              ? "No block list shards found for this registrar. Paste contract ids."
                              : "This instrument checks a block list. Keep all of the registrar's shards, or paste contract ids."
                        }
                      />
                    )}
                  />
                )}
                <HelpTextField
                  size="small"
                  label="Description"
                  value={proposalDescription}
                  onChange={(e) => setProposalDescription(e.target.value)}
                  fullWidth
                  required
                  help="Free-form human-readable note explaining why this request is being accepted."
                />
              </>
            );
          })()}

          {(proposalType === "offer_free_credential" ||
            proposalType === "offer_paid_credential") && (
            <>
              {renderUserServiceSelect(
                "Contract id of this governance party's UserService, used to issue the credential offer.",
              )}
              <HelpTextField
                size="small"
                label="Holder Party"
                value={proposalHolder}
                onChange={(e) => setProposalHolder(e.target.value)}
                fullWidth
                required
                help="Party id that will receive the credential offer."
              />
              <HelpTextField
                size="small"
                label="Credential ID"
                value={proposalCredentialId}
                onChange={(e) => setProposalCredentialId(e.target.value)}
                fullWidth
                required
                help="Unique identifier for this credential (free-form string)."
              />
              <HelpTextField
                size="small"
                label="Description"
                value={proposalDescription}
                onChange={(e) => setProposalDescription(e.target.value)}
                fullWidth
                required
                help="Free-form human-readable description of what this credential certifies."
              />
              <HelpTextField
                size="small"
                label="Claims (one per line: subject,property,value)"
                value={proposalCredentialClaimsText}
                onChange={(e) => setProposalCredentialClaimsText(e.target.value)}
                fullWidth
                multiline
                minRows={2}
                maxRows={6}
                helperText='Each line: "<subject>,<property>,<value>"'
                help="Claims baked into the credential. One per line, each formatted as subject,property,value."
                helpLabel="Claims"
              />
              {proposalType === "offer_paid_credential" && (
                <>
                  <HelpTextField
                    size="small"
                    label="Fee per Day (USD)"
                    value={proposalFeePerDayUsd}
                    onChange={(e) => setProposalFeePerDayUsd(e.target.value)}
                    fullWidth
                    required
                    help="The credential's daily fee in USD. Billing takes it from the holder's deposit."
                  />
                  <HelpTextField
                    size="small"
                    label="Billing Period (minutes)"
                    value={proposalBillingPeriodMinutes}
                    onChange={(e) => setProposalBillingPeriodMinutes(e.target.value)}
                    fullWidth
                    required
                    help="Minutes between fee charges. Each charge covers the time since the last one."
                  />
                  <HelpTextField
                    size="small"
                    label="Deposit Target (USD)"
                    value={proposalDepositTargetAmountUsd}
                    onChange={(e) => setProposalDepositTargetAmountUsd(e.target.value)}
                    fullWidth
                    required
                    help="The deposit in USD that the holder keeps locked to cover fees. Top-ups refill the deposit to this amount."
                  />
                  <HelpTextField
                    size="small"
                    label="Holder Activity Weight"
                    value={proposalHolderActivityWeight}
                    onChange={(e) => setProposalHolderActivityWeight(e.target.value)}
                    fullWidth
                    help="The holder's share of the activity reward, from 0 to 1. The issuer gets the rest. Leave empty for 0."
                  />
                  <HelpTextField
                    size="small"
                    label="Initial Deposit (USD)"
                    value={proposalDepositInitialAmountUsd}
                    onChange={(e) => setProposalDepositInitialAmountUsd(e.target.value)}
                    fullWidth
                    help="The USD amount the holder locks on accept. Leave empty to lock the Deposit Target."
                  />
                </>
              )}
            </>
          )}

          {proposalType === "accept_free_credential" && (
            <>
              {renderUserServiceSelect(
                "Contract id of this governance party's UserService, used to accept the credential offer.",
              )}
              <CredentialOfferPicker
                value={proposalCredentialOfferCid}
                setValue={setProposalCredentialOfferCid}
                offers={acceptableCredentialOffers}
                kind="free"
                loading={credentialOffersLoading}
              />
            </>
          )}

          {proposalType === "accept_paid_credential" && (
            <>
              {renderUserServiceSelect(
                "Contract id of this governance party's UserService, used to accept the credential offer.",
              )}
              <CredentialOfferPicker
                value={proposalCredentialOfferCid}
                setValue={setProposalCredentialOfferCid}
                offers={acceptablePaidCredentialOffers}
                kind="paid"
                loading={credentialOffersLoading}
              />
              {chosenPaidOffer?.billing_params && chosenDepositUsd ? (
                <Alert severity="info">
                  This accept locks a deposit of {formatDecimal(chosenDepositUsd)} USD, and
                  the vote shows that amount. The offer's deposit target is{" "}
                  {formatDecimal(chosenPaidOffer.billing_params.deposit_target_amount_usd)} USD.
                  Its fee is {formatDecimal(chosenPaidOffer.billing_params.fee_per_day_usd)} USD
                  a day, billed every {chosenPaidOffer.billing_params.billing_period_minutes}{" "}
                  minutes.
                </Alert>
              ) : (
                proposalCredentialOfferCid && (
                  <Alert severity="warning">
                    This offer is not in the list, so the vote will not show its deposit.
                  </Alert>
                )
              )}
              <Autocomplete
                size="small"
                multiple
                freeSolo
                options={ownAmulets}
                value={proposalDepositAmuletCids}
                loading={ownAmuletsLoading}
                onChange={(_event, values) => {
                  setProposalDepositAmuletCids(
                    values.map((value) =>
                      typeof value === "string" ? value : value.contract_id,
                    ),
                  );
                }}
                getOptionLabel={(option) => {
                  // Selected values are stored as bare cids; label them
                  // with the amount when the Amulet is known.
                  const cid = typeof option === "string" ? option : option.contract_id;
                  const known =
                    typeof option === "string"
                      ? ownAmulets.find((a) => a.contract_id === option)
                      : option;
                  return known ? `${known.amount} CC (…${cid.slice(-8)})` : cid;
                }}
                isOptionEqualToValue={(option, value) =>
                  typeof value === "string"
                    ? option.contract_id === value
                    : option.contract_id === value.contract_id
                }
                renderInput={(params) => (
                  <TextField
                    {...params}
                    label={
                      <TextHelp text="Unlocked Amulets of this governance party. Together they must cover the offer's deposit.">
                        Deposit Amulets
                      </TextHelp>
                    }
                    helperText={
                      ownAmuletsLoading
                        ? "Loading Amulets…"
                        : ownAmulets.length === 0
                          ? "No Amulets visible — paste a contract id directly if you have one"
                          : "Pick one or more Amulets"
                    }
                  />
                )}
              />
              <AmuletRulesCidField state={state} />
              <Box sx={{ display: "flex", gap: 1, alignItems: "center" }}>
                <HelpTextField
                  size="small"
                  label="Open Mining Round CID"
                  value={proposalOpenMiningRoundCid}
                  onChange={(e) => setProposalOpenMiningRoundCid(e.target.value)}
                  fullWidth
                  required
                  helperText={
                    openMiningRoundLoading
                      ? "Reading the open round from scan…"
                      : openMiningRound && openMiningRound.contract_id === proposalOpenMiningRoundCid
                        ? `Round ${openMiningRound.round}, open until ${new Date(openMiningRound.target_closes_at).toLocaleTimeString()}`
                        : undefined
                  }
                  help="Contract id of an OpenMiningRound that is open now. DecMan fills in the newest one from scan. You can paste another one."
                />
                <Tooltip title="Refresh">
                  <span>
                    <IconButton
                      size="small"
                      onClick={fetchOpenMiningRound}
                      disabled={openMiningRoundLoading}
                      aria-label="Refresh the open mining round"
                    >
                      {openMiningRoundLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                    </IconButton>
                  </span>
                </Tooltip>
              </Box>
              {openMiningRoundError && (
                <Alert severity="warning">{openMiningRoundError}</Alert>
              )}
              <HelpTextField
                size="small"
                label="Featured App Right CID"
                value={proposalFeaturedAppRightCid}
                onChange={(e) => setProposalFeaturedAppRightCid(e.target.value)}
                fullWidth
                help="Optional FeaturedAppRight of the credential issuer. The deposit transfer is unfeatured, so you can leave this empty."
              />
              <Alert severity="info">
                The proposal pins these Amulets and this open mining round.
                The round closes about 20 minutes after it opens. Confirm
                and execute before then, or propose again.
              </Alert>
            </>
          )}

          {proposalType === "create_provider_configuration" && (
            <>
              <ContractSelect
                label="ProviderService"
                help="ProviderService contract of this provider decparty. Its provider must be the governance party."
                value={proposalProviderServiceCid}
                onChange={setProposalProviderServiceCid}
                items={providerServices}
                loading={servicesLoading}
                loadingText="Loading services…"
                emptyText='No ProviderService found — run "Create Provider Service Request" first'
              />
              <RequirementRows
                label="Registrar Requirements"
                help="Credential requirements a registrar must meet before the provider onboards it. Rows whose issuer is this governance party are minted automatically during Onboard Registrar."
                rows={proposalRegistrarRequirements}
                setRows={setProposalRegistrarRequirements}
                defaultIssuer={partyId}
              />
              <RequirementRows
                label="Holder Requirements"
                help="Credential requirements a token holder must meet on this provider's utility."
                rows={proposalHolderRequirements}
                setRows={setProposalHolderRequirements}
                defaultIssuer={partyId}
              />
            </>
          )}

          {proposalType === "create_registrar_service_request" && (
            <>
              <HelpTextField
                size="small"
                label="Operator Party"
                value={proposalOperator}
                onChange={(e) => setProposalOperator(e.target.value)}
                fullWidth
                required
                help="Party id of the utility operator the registrar service runs under."
              />
              <HelpTextField
                size="small"
                label="Provider Party"
                value={proposalProvider}
                onChange={(e) => setProposalProvider(e.target.value)}
                fullWidth
                required
                help="Party id of the provider decparty this request asks for registrar service. The provider accepts via Onboard Registrar."
              />
              <FormControlLabel
                control={<Checkbox size="small" checked={proposalCreateTransferRule} onChange={(e) => setProposalCreateTransferRule(e.target.checked)} />}
                label={
                  <TextHelp text="Also create a TransferRule contract during the accept, so holders can transfer this registrar's tokens without per-transfer governance.">
                    Create TransferRule
                  </TextHelp>
                }
              />
              <FormControlLabel
                control={<Checkbox size="small" checked={proposalCreateAllocationFactory} onChange={(e) => setProposalCreateAllocationFactory(e.target.checked)} />}
                label={
                  <TextHelp text="Also create an AllocationFactory contract during the accept, so this registrar's tokens can be allocated by external apps.">
                    Create AllocationFactory
                  </TextHelp>
                }
              />
            </>
          )}

          {proposalType === "onboard_registrar" && (
            <>
              <ContractSelect
                label="ProviderService"
                help="ProviderService contract of this provider decparty, used to accept the request."
                value={proposalProviderServiceCid}
                onChange={setProposalProviderServiceCid}
                items={providerServices}
                loading={servicesLoading}
                loadingText="Loading services…"
                emptyText='No ProviderService found — run "Create Provider Service Request" first'
              />
              <ContractSelect
                label="RegistrarServiceRequest"
                help="Pending RegistrarServiceRequest to accept. The request names the registrar to onboard; its provider must be this governance party."
                value={proposalRegistrarServiceRequestCid}
                onChange={setProposalRegistrarServiceRequestCid}
                items={acceptableRegistrarServiceRequests}
                loading={registrarServiceRequestsLoading}
                loadingText="Loading requests…"
                emptyText='No pending requests for this provider — the registrar runs "Create Registrar Service Request" first'
                rowLabel={(req) => <>{req.registrar.split("::")[0]} (…{req.contract_id.slice(-8)})</>}
              />
              <ContractSelect
                label="ProviderConfiguration"
                help="ProviderConfiguration holding the registrar requirements to mint and validate against."
                value={proposalProviderConfigurationCid}
                onChange={setProposalProviderConfigurationCid}
                items={providerConfigurations}
                loading={providerConfigurationsLoading}
                loadingText="Loading configurations…"
                emptyText='No ProviderConfiguration found — run "Create Provider Configuration" first'
              />
            </>
          )}

          {proposalType === "provision_instrument" && (
            <>
              <ContractSelect
                label="RegistrarService"
                help="RegistrarService contract of this registrar decparty, used to create the InstrumentConfiguration."
                value={proposalRegistrarServiceCid}
                onChange={setProposalRegistrarServiceCid}
                items={ownRegistrarServices}
                loading={registrarServicesLoading}
                loadingText="Loading services…"
                emptyText='No RegistrarService for this party — run "Onboard Registrar" first'
              />
              <HelpTextField
                size="small"
                label="Instrument ID"
                value={proposalInstrumentIdText}
                onChange={(e) => setProposalInstrumentIdText(e.target.value)}
                fullWidth
                required
                help={"Token name for the instrument this registrar will manage (e.g. \"cTM\")."}
              />
              <RequirementRows
                label="Issuer Requirements"
                help="Credential requirements an instrument issuer must meet. Rows whose issuer is this governance party are minted automatically for the initial issuers below."
                rows={proposalIssuerRequirements}
                setRows={setProposalIssuerRequirements}
                defaultIssuer={partyId}
              />
              <RequirementRows
                label="Holder Requirements"
                help="Credential requirements a holder of this instrument must meet."
                rows={proposalHolderRequirements}
                setRows={setProposalHolderRequirements}
                defaultIssuer={partyId}
              />
              <HelpTextField
                size="small"
                label="Initial Instrument Issuers (one party id per line)"
                value={proposalInitialInstrumentIssuersText}
                onChange={(e) => setProposalInitialInstrumentIssuersText(e.target.value)}
                fullWidth
                multiline
                minRows={2}
                maxRows={6}
                help="Parties credentialed as instrument issuers at provisioning, one party id per line. Leave empty to onboard issuers later."
                helpLabel="Initial Instrument Issuers"
              />
            </>
          )}

          {proposalType === "onboard_instrument_issuers" && (
            <>
              <ContractSelect
                label="InstrumentConfiguration"
                help="InstrumentConfiguration whose issuer requirements the new issuers are credentialed against."
                value={proposalInstrumentConfigurationCid}
                onChange={setProposalInstrumentConfigurationCid}
                items={ownInstruments}
                loading={instrumentsLoading}
                loadingText="Loading instruments…"
                emptyText='No instruments administered by this party — run "Provision Instrument" first'
                rowLabel={(inst) => <>{inst.instrument_id} ({inst.contract_id.slice(0, 8)}…)</>}
              />
              <HelpTextField
                size="small"
                label="Instrument Issuers (one party id per line)"
                value={proposalInstrumentIssuersText}
                onChange={(e) => setProposalInstrumentIssuersText(e.target.value)}
                fullWidth
                required
                multiline
                minRows={2}
                maxRows={6}
                help="Parties to credential as instrument issuers, one party id per line. At least one is required."
                helpLabel="Instrument Issuers"
              />
            </>
          )}

          {proposalType === "create_transfer_rule" && (
            <ContractSelect
              label="RegistrarService"
              help="RegistrarService contract of this registrar decparty. The new TransferRule belongs to it. Setup creates the first rule when asked to, so use this to add one later."
              value={proposalRegistrarServiceCid}
              onChange={setProposalRegistrarServiceCid}
              items={ownRegistrarServices}
              loading={registrarServicesLoading}
              loadingText="Loading services…"
              emptyText='No RegistrarService for this party — run "Setup Utility" or "Onboard Registrar" first'
            />
          )}

          {proposalType === "update_instrument_configuration" && (
            <>
              <Alert severity="info">
                This replaces every field of the configuration. Picking a
                configuration fills the form with its current values. A
                field you clear is cleared on the ledger. The reward
                beneficiaries stay as they are.
              </Alert>
              <ContractSelect
                label="RegistrarService"
                help="RegistrarService contract of this registrar decparty. The update runs through it."
                value={proposalRegistrarServiceCid}
                onChange={setProposalRegistrarServiceCid}
                items={ownRegistrarServices}
                loading={registrarServicesLoading}
                loadingText="Loading services…"
                emptyText='No RegistrarService for this party — run "Onboard Registrar" first'
              />
              <ContractSelect
                label="InstrumentConfiguration"
                help="The InstrumentConfiguration to replace. Execution archives it and creates a new one."
                value={proposalInstrumentConfigurationCid}
                onChange={selectConfigurationToUpdate}
                items={ownInstruments}
                loading={instrumentsLoading}
                loadingText="Loading instruments…"
                emptyText='No instruments administered by this party — run "Provision Instrument" first'
                rowLabel={(inst) => <>{inst.instrument_id} ({inst.contract_id.slice(0, 8)}…)</>}
              />
              {configurationToUpdateUnreadable && (
                <Alert severity="warning">
                  DecMan could not read this configuration&apos;s current
                  values. Enter every field by hand.
                </Alert>
              )}
              {blockListProblem && <Alert severity="warning">{blockListProblem}</Alert>}
              <HelpTextField
                size="small"
                label="Instrument ID"
                value={proposalInstrumentIdText}
                fullWidth
                disabled
                help="The instrument's id. Holdings name the instrument by it, so the update cannot change it."
              />
              <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
                <TextHelp text="Other identifiers of the instrument, each with its source party, id and scheme.">
                  Additional Identifiers
                </TextHelp>
              </Typography>
              {proposalAdditionalIdentifiers.map((row, idx) => (
                <Box key={idx} sx={{ display: "flex", gap: 1, mb: 1, alignItems: "flex-start" }}>
                  {(["source", "id", "scheme"] as const).map((key) => (
                    <TextField
                      key={key}
                      label={`Identifier ${idx + 1} ${key === "source" ? "Source Party" : key === "id" ? "ID" : "Scheme"}`}
                      value={row[key]}
                      onChange={(e) => {
                        const updated = [...proposalAdditionalIdentifiers];
                        updated[idx] = { ...row, [key]: e.target.value };
                        setProposalAdditionalIdentifiers(updated);
                      }}
                      size="small"
                      sx={{ flex: key === "source" ? 2 : 1 }}
                    />
                  ))}
                  <Button
                    size="small"
                    color="error"
                    onClick={() =>
                      setProposalAdditionalIdentifiers(
                        proposalAdditionalIdentifiers.filter((_, i) => i !== idx),
                      )
                    }
                  >
                    Remove
                  </Button>
                </Box>
              ))}
              <Box>
                <Button
                  size="small"
                  onClick={() =>
                    setProposalAdditionalIdentifiers([
                      ...proposalAdditionalIdentifiers,
                      { source: partyId, id: "", scheme: "" },
                    ])
                  }
                >
                  Add Identifier
                </Button>
              </Box>
              <RequirementRows
                label="Issuer Requirements"
                help="Credential requirements an instrument issuer must meet."
                rows={proposalIssuerRequirements}
                setRows={setProposalIssuerRequirements}
                defaultIssuer={partyId}
              />
              <RequirementRows
                label="Holder Requirements"
                help="Credential requirements a holder of this instrument must meet."
                rows={proposalHolderRequirements}
                setRows={setProposalHolderRequirements}
                defaultIssuer={partyId}
              />
            </>
          )}

          {(proposalType === "provision_instrument" ||
            proposalType === "onboard_instrument_issuers") && (
            <>
              <HelpTextField
                size="small"
                label="Credentials Valid From (optional)"
                type="datetime-local"
                value={proposalCredentialValidFrom}
                onChange={(e) => setProposalCredentialValidFrom(e.target.value)}
                fullWidth
                slotProps={{
                  inputLabel: { shrink: true },
                }}
                help="When the minted credentials start to be valid. Leave empty for valid from the moment the action executes."
                helpLabel="Credentials Valid From"
              />
              <HelpTextField
                size="small"
                label="Credentials Valid Until (optional)"
                type="datetime-local"
                value={proposalCredentialValidUntil}
                onChange={(e) => setProposalCredentialValidUntil(e.target.value)}
                fullWidth
                slotProps={{
                  inputLabel: { shrink: true },
                }}
                help="When the minted credentials expire. Leave empty for credentials that never expire; ending those takes an Offboard Instrument Issuers vote. The proposal description shows this window to voters, and execution is refused once it has passed."
                helpLabel="Credentials Valid Until"
              />
            </>
          )}

          {proposalType === "offboard_instrument_issuers" && (
            <>
              <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
                <TextHelp text="One row per offboarded issuer. Each row lists the credentials this governance party issued for that issuer. An issuer keeps minting rights through any credential left out.">
                  Instrument Issuers to Offboard (add issuer + credentials)
                </TextHelp>
              </Typography>
              {proposalOffboardRows.map((row, idx) => (
                <Box key={idx} sx={{ display: "flex", gap: 1, mb: 1, alignItems: "flex-start" }}>
                  <HelpTextField
                    label="Instrument Issuer Party"
                    value={row.party}
                    onChange={(e) => {
                      const party = e.target.value;
                      // Reset cids when the party changes. Stale
                      // credentials name the wrong subject and fail at
                      // execution.
                      const updated = [...proposalOffboardRows];
                      updated[idx] =
                        party === row.party ? row : { ...row, party, cids: [] };
                      setProposalOffboardRows(updated);
                    }}
                    size="small"
                    sx={{ flex: 2 }}
                    help="Party id of the issuer being offboarded. The credential list below filters to credentials whose claims all name this party."
                  />
                  <Autocomplete
                    size="small"
                    multiple
                    freeSolo
                    sx={{ flex: 3 }}
                    options={offboardableCredentialsFor(row.party)}
                    value={row.cids}
                    loading={credentialsLoading}
                    onChange={(_event, values) => {
                      const updated = [...proposalOffboardRows];
                      updated[idx] = {
                        ...row,
                        cids: values.map((value) =>
                          typeof value === "string" ? value : value.contract_id,
                        ),
                      };
                      setProposalOffboardRows(updated);
                    }}
                    getOptionLabel={(option) => {
                      const cid =
                        typeof option === "string" ? option : option.contract_id;
                      const known =
                        typeof option === "string"
                          ? availableCredentials.find((c) => c.contract_id === option)
                          : option;
                      if (!known) {
                        return cid;
                      }
                      return `${known.credential_id.split("/")[0]} (…${cid.slice(-8)})`;
                    }}
                    isOptionEqualToValue={(option, value) =>
                      typeof value === "string"
                        ? option.contract_id === value
                        : option.contract_id === value.contract_id
                    }
                    renderInput={(params) => (
                      <TextField
                        {...params}
                        label="Credentials to Revoke"
                        helperText={
                          credentialsLoading
                            ? "Loading credentials…"
                            : "Pick every credential of this issuer, or paste contract ids."
                        }
                      />
                    )}
                  />
                  <Button
                    size="small"
                    color="error"
                    onClick={() =>
                      setProposalOffboardRows(
                        proposalOffboardRows.filter((_, i) => i !== idx),
                      )
                    }
                  >
                    Remove
                  </Button>
                </Box>
              ))}
              <Box sx={{ display: "flex", alignItems: "center", gap: 2 }}>
                <Button
                  size="small"
                  onClick={() =>
                    setProposalOffboardRows([
                      ...proposalOffboardRows,
                      { party: "", cids: [] },
                    ])
                  }
                >
                  Add Issuer
                </Button>
              </Box>
            </>
          )}

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
