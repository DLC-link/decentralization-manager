import { useEffect, useState } from "react";
import type { RequirementRow } from "../fields/RequirementRows";
import type { FormProposalType } from "./proposalMenu";
import { DEFAULT_TRANSFER_EXPIRY_HOURS } from "./transferPayloads";
import type { InstrumentAllowance, InstrumentIdentifier } from "../../../types";

interface ProposalFieldsArgs {
  partyId: string;
  defaultOperatorParty?: string;
}

/**
 * The fields of the new-proposal form, the values prefilled into them, and
 * `resetProposalForm`, which sets them all back for a new proposal type.
 */
export function useProposalFields({ partyId, defaultOperatorParty }: ProposalFieldsArgs) {
  // Domain proposal state
  const [proposalType, setProposalType] = useState<FormProposalType>("setup_cc_preapproval");
  const [proposalProvider, setProposalProvider] = useState("");
  const [proposalExpectedDso, setProposalExpectedDso] = useState("");
  const [proposalDelegate, setProposalDelegate] = useState("");
  // datetime-local string; converted to micros-since-epoch on submit.
  const [proposalDelegationExpiresAt, setProposalDelegationExpiresAt] =
    useState("");
  const [proposalAmuletMergeLimit, setProposalAmuletMergeLimit] =
    useState("10");
  // Optional validity window of the credentials a minting proposal mints.
  // datetime-local strings; empty means unbounded on that side.
  const [proposalCredentialValidFrom, setProposalCredentialValidFrom] =
    useState("");
  const [proposalCredentialValidUntil, setProposalCredentialValidUntil] =
    useState("");
  const [proposalOperator, setProposalOperator] = useState(
    defaultOperatorParty || "",
  );
  const [proposalInstrumentAdmin, setProposalInstrumentAdmin] = useState("");
  // Local row type carries a stable `uid` so React's reconciliation keeps
  // inputs / cursor position correct when rows are removed (using array
  // index as key reuses DOM nodes across rows and causes value/cursor
  // swaps). The `uid` is stripped before submit.
  const [proposalInstrumentAllowances, setProposalInstrumentAllowances] =
    useState<({ uid: string } & InstrumentAllowance)[]>([]);
  const [proposalTransferFactoryCid, setProposalTransferFactoryCid] = useState("");
  const [proposalExpectedAdmin, setProposalExpectedAdmin] = useState("");
  const [proposalReceiver, setProposalReceiver] = useState("");
  const [proposalAmount, setProposalAmount] = useState("");
  const [proposalInstrumentIdAdmin, setProposalInstrumentIdAdmin] = useState("");
  const [proposalInstrumentIdId, setProposalInstrumentIdId] = useState("");
  const [proposalInputHoldingCids, setProposalInputHoldingCids] = useState("");
  // Validity window (hours) for a Transfer proposal / two-step offer. Defaults
  // to the backend's default (24h) but is overridable; bounding it lets an
  // unaccepted offer expire and release escrow instead of locking funds.
  const [proposalTransferExpiryHours, setProposalTransferExpiryHours] =
    useState(String(DEFAULT_TRANSFER_EXPIRY_HOURS));
  const [proposalTransferInstructionCid, setProposalTransferInstructionCid] = useState("");
  const [proposalDescription, setProposalDescription] = useState("");
  // Utility-onboarding proposal state
  const [proposalProviderServiceCid, setProposalProviderServiceCid] = useState("");
  const [proposalInstrumentIdText, setProposalInstrumentIdText] = useState("");
  const [proposalCreateTransferRule, setProposalCreateTransferRule] = useState(true);
  const [proposalCreateAllocationFactory, setProposalCreateAllocationFactory] = useState(true);
  // Additional identifier rows of the Update Instrument Configuration form.
  const [proposalAdditionalIdentifiers, setProposalAdditionalIdentifiers] = useState<
    InstrumentIdentifier[]
  >([]);
  const [proposalUser, setProposalUser] = useState("");
  const [proposalInstrumentConfigurationCid, setProposalInstrumentConfigurationCid] = useState("");
  const [proposalClearBeneficiaries, setProposalClearBeneficiaries] = useState(false);
  // Row-based beneficiary entry. Each row is { beneficiary, weight } —
  // weights are decimals that
  // must sum to 1.0 (validated client-side + by Daml on submit).
  const [proposalBeneficiaries, setProposalBeneficiaries] = useState<
    { beneficiary: string; weight: string }[]
  >([]);
  // Coupon-reassignment delegation (CIP-104 Mode A). The split rows are kept
  // separate from proposalBeneficiaries: those carry a FAR *weight*, these a
  // *percentage*, and switching action types must not carry one into the other.
  const [proposalDelegationDso, setProposalDelegationDso] = useState("");
  const [proposalDelegationAssigners, setProposalDelegationAssigners] = useState<string[]>([]);
  // Integer weights, not percentages — the exact decimals are derived from them
  // (see splitFromWeights). An even 3-way split is not expressible as a repeated
  // decimal, so asking for percentages here means asking a human to hand-balance
  // the last entry.
  const [proposalDelegationSplit, setProposalDelegationSplit] = useState<
    { beneficiary: string; weight: string }[]
  >([]);
  const [proposalPriorDelegation, setProposalPriorDelegation] = useState("");
  const [proposalRevokeDelegationCid, setProposalRevokeDelegationCid] = useState("");
  const [proposalRegistrarServiceCid, setProposalRegistrarServiceCid] = useState("");
  const [proposalEnableResultContracts, setProposalEnableResultContracts] = useState<"true" | "false" | "clear">("true");
  const [proposalHolder, setProposalHolder] = useState("");
  // Credential proposal state (offer / accept, free and paid)
  const [proposalUserServiceCid, setProposalUserServiceCid] = useState("");
  const [proposalCredentialId, setProposalCredentialId] = useState("");
  const [proposalCredentialClaimsText, setProposalCredentialClaimsText] = useState("");
  const [proposalCredentialOfferCid, setProposalCredentialOfferCid] = useState("");
  // Offer Paid Credential billing fields, as typed
  const [proposalFeePerDayUsd, setProposalFeePerDayUsd] = useState("");
  const [proposalBillingPeriodMinutes, setProposalBillingPeriodMinutes] = useState("");
  const [proposalDepositTargetAmountUsd, setProposalDepositTargetAmountUsd] = useState("");
  const [proposalHolderActivityWeight, setProposalHolderActivityWeight] = useState("");
  const [proposalDepositInitialAmountUsd, setProposalDepositInitialAmountUsd] = useState("");
  // Accept Paid Credential deposit and transfer context
  const [proposalDepositAmuletCids, setProposalDepositAmuletCids] = useState<string[]>([]);
  const [proposalOpenMiningRoundCid, setProposalOpenMiningRoundCid] = useState("");
  const [proposalFeaturedAppRightCid, setProposalFeaturedAppRightCid] = useState("");
  // Accept holder-initiated mint/burn request state
  const [proposalMintRequestCid, setProposalMintRequestCid] = useState("");
  const [proposalBurnRequestCid, setProposalBurnRequestCid] = useState("");
  // Credential cids proving the request's holder meets the instrument's
  // issuer requirements. Passed into the accept's choice context.
  const [proposalIssuerCredentialCids, setProposalIssuerCredentialCids] = useState<string[]>([]);
  // `StoredBlockListPartyShas` cids for an instrument that checks a block
  // list. Passed into the accept's choice context.
  const [proposalBlockListShardCids, setProposalBlockListShardCids] = useState<string[]>([]);
  // Set once the operator edits the shard picker, so a late shard list never
  // replaces a hand-made choice.
  const [blockListShardsEdited, setBlockListShardsEdited] = useState(false);
  // Registrar-onboarding proposal state
  const [proposalRegistrarServiceRequestCid, setProposalRegistrarServiceRequestCid] = useState("");
  const [proposalProviderConfigurationCid, setProposalProviderConfigurationCid] = useState("");
  // Requirement-editor rows: an issuer party id plus its required claims as
  // "property,value" lines. Parsed into PartyCredentialRequirement on submit.
  const [proposalRegistrarRequirements, setProposalRegistrarRequirements] = useState<
    RequirementRow[]
  >([]);
  const [proposalHolderRequirements, setProposalHolderRequirements] = useState<
    RequirementRow[]
  >([]);
  const [proposalIssuerRequirements, setProposalIssuerRequirements] = useState<
    RequirementRow[]
  >([]);
  // Party-list textareas, one party id per line.
  const [proposalInitialInstrumentIssuersText, setProposalInitialInstrumentIssuersText] = useState("");
  const [proposalInstrumentIssuersText, setProposalInstrumentIssuersText] = useState("");
  // Offboard: one row per offboarded issuer. Each row's credential picker is
  // filtered to credentials whose claims all name that row's party.
  const [proposalOffboardRows, setProposalOffboardRows] = useState<
    { party: string; cids: string[] }[]
  >([]);
  // Accept External Party Setup: contract id of the validator-created
  // ExternalPartySetupProposal to accept.
  const [
    proposalExternalPartySetupCid,
    setProposalExternalPartySetupCid,
  ] = useState("");
  // Sync the autofetched operator party (from App.tsx) into the proposal
  // operator state once it arrives — without this, the fields stay empty
  // whenever the fetch completes after this component has already mounted
  // with an empty default.
  useEffect(() => {
    if (defaultOperatorParty) {
      setProposalOperator(defaultOperatorParty);
    }
  }, [defaultOperatorParty]);
  const [amuletRulesCid, setAmuletRulesCid] = useState("");
  const [dsoPartyId, setDsoPartyId] = useState("");
  // Key into `transferHoldings` for the currently-selected instrument:
  // `${instrument_admin}::${instrument_id}`. Empty string = none selected.
  const [selectedHoldingKey, setSelectedHoldingKey] = useState("");
  // Hides the explicit `Input Holding CIDs` field by default — Daml's
  // TransferFactory choice auto-selects matching holdings up to `amount`, so
  // typical users don't need it. Power users can reveal it to pin specific
  // UTXO holdings.
  const [showTransferAdvanced, setShowTransferAdvanced] = useState(false);

  // Clears every proposal field back to its default for `type`. Runs after a
  // successful submit and whenever the proposal type changes: the fields are
  // shared across types, so without it a value entered for one type lands in
  // the next one's payload. Fields prefilled from app or network state get
  // that value back rather than an empty string, because their fetches only
  // run once.
  const resetProposalForm = (type: FormProposalType = proposalType) => {
    setProposalProvider(type === "create_provider_service_request" ? partyId : "");
    setProposalOperator(defaultOperatorParty || "");
    setProposalExpectedDso(dsoPartyId);
    setProposalMintRequestCid("");
    setProposalBurnRequestCid("");
    setProposalInstrumentAdmin("");
    setProposalInstrumentAllowances([]);
    setProposalTransferFactoryCid("");
    setProposalExpectedAdmin("");
    setProposalReceiver("");
    setProposalAmount("");
    setSelectedHoldingKey("");
    setShowTransferAdvanced(false);
    setProposalInstrumentIdAdmin("");
    setProposalInstrumentIdId("");
    setProposalInputHoldingCids("");
    setProposalTransferExpiryHours(String(DEFAULT_TRANSFER_EXPIRY_HOURS));
    setProposalTransferInstructionCid("");
    setProposalDescription("");
    setProposalProviderServiceCid("");
    setProposalInstrumentIdText("");
    setProposalCreateTransferRule(true);
    setProposalCreateAllocationFactory(true);
    setProposalAdditionalIdentifiers([]);
    setProposalUser(type === "create_user_service_request" ? partyId : "");
    setProposalInstrumentConfigurationCid("");
    setProposalBeneficiaries([]);
    setProposalClearBeneficiaries(false);
    setProposalDelegationDso(dsoPartyId);
    setProposalDelegationAssigners([]);
    setProposalDelegationSplit([]);
    setProposalPriorDelegation("");
    setProposalRevokeDelegationCid("");
    setProposalRegistrarServiceCid("");
    setProposalEnableResultContracts("true");
    setProposalHolder("");
    setProposalUserServiceCid("");
    setProposalCredentialId("");
    setProposalCredentialClaimsText("");
    setProposalCredentialOfferCid("");
    setProposalFeePerDayUsd("");
    setProposalBillingPeriodMinutes("");
    setProposalDepositTargetAmountUsd("");
    setProposalHolderActivityWeight("");
    setProposalDepositInitialAmountUsd("");
    setProposalDepositAmuletCids([]);
    setProposalOpenMiningRoundCid("");
    setProposalFeaturedAppRightCid("");
    setProposalIssuerCredentialCids([]);
    setProposalBlockListShardCids([]);
    setBlockListShardsEdited(false);
    setProposalDelegate("");
    setProposalDelegationExpiresAt("");
    setProposalAmuletMergeLimit("10");
    setProposalRegistrarServiceRequestCid("");
    setProposalProviderConfigurationCid("");
    setProposalRegistrarRequirements([]);
    setProposalHolderRequirements([]);
    setProposalIssuerRequirements([]);
    setProposalInitialInstrumentIssuersText("");
    setProposalExternalPartySetupCid("");
    setProposalInstrumentIssuersText("");
    setProposalOffboardRows([]);
    setProposalCredentialValidFrom("");
    setProposalCredentialValidUntil("");
  };

  return {
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
    proposalInstrumentAdmin,
    setProposalInstrumentAdmin,
    proposalInstrumentAllowances,
    setProposalInstrumentAllowances,
    proposalTransferFactoryCid,
    setProposalTransferFactoryCid,
    proposalExpectedAdmin,
    setProposalExpectedAdmin,
    proposalReceiver,
    setProposalReceiver,
    proposalAmount,
    setProposalAmount,
    proposalInstrumentIdAdmin,
    setProposalInstrumentIdAdmin,
    proposalInstrumentIdId,
    setProposalInstrumentIdId,
    proposalInputHoldingCids,
    setProposalInputHoldingCids,
    proposalTransferExpiryHours,
    setProposalTransferExpiryHours,
    proposalTransferInstructionCid,
    setProposalTransferInstructionCid,
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
    proposalUser,
    setProposalUser,
    proposalInstrumentConfigurationCid,
    setProposalInstrumentConfigurationCid,
    proposalClearBeneficiaries,
    setProposalClearBeneficiaries,
    proposalBeneficiaries,
    setProposalBeneficiaries,
    proposalDelegationDso,
    setProposalDelegationDso,
    proposalDelegationAssigners,
    setProposalDelegationAssigners,
    proposalDelegationSplit,
    setProposalDelegationSplit,
    proposalPriorDelegation,
    setProposalPriorDelegation,
    proposalRevokeDelegationCid,
    setProposalRevokeDelegationCid,
    proposalRegistrarServiceCid,
    setProposalRegistrarServiceCid,
    proposalEnableResultContracts,
    setProposalEnableResultContracts,
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
    proposalMintRequestCid,
    setProposalMintRequestCid,
    proposalBurnRequestCid,
    setProposalBurnRequestCid,
    proposalIssuerCredentialCids,
    setProposalIssuerCredentialCids,
    proposalBlockListShardCids,
    setProposalBlockListShardCids,
    blockListShardsEdited,
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
    setAmuletRulesCid,
    dsoPartyId,
    setDsoPartyId,
    selectedHoldingKey,
    setSelectedHoldingKey,
    showTransferAdvanced,
    setShowTransferAdvanced,
    resetProposalForm,
  };
}

export type ProposalFields = ReturnType<typeof useProposalFields>;
