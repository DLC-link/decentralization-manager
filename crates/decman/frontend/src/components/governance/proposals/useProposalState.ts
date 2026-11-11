import { useCallback, useEffect, useMemo, useState } from "react";
import {
  API_BASE,
  TEMPLATE_AMULET,
  TEMPLATE_REGISTRAR_SERVICE,
} from "../../../constants";
import { authenticatedFetch } from "../../../api";
import { usePartyList } from "../../../hooks/usePartyList";
import { beneficiariesProblem } from "../../../rewardBeneficiaries";
import { offerDepositUsd, ownedAmulets, type AmuletOption } from "../../../paidCredential";
import type { RequirementRow } from "../fields/RequirementRows";
import type { FormProposalType } from "./proposalMenu";
import type {
  ActiveCouponReassignmentDelegation,
  BlockListShardsResponse,
  BurnRequestsResponse,
  ContractQueryResponse,
  ContractWithBlob,
  CouponReassignmentDelegationSummary,
  CredentialOffersResponse,
  CredentialsResponse,
  Holding,
  HoldingsResponse,
  InstrumentAllowance,
  InstrumentIdentifier,
  InstrumentsResponse,
  MintRequestsResponse,
  NetworkInfo,
  OpenMiningRoundInfo,
  ProviderConfigurationsResponse,
  ProviderServiceInfo,
  ProviderServicesResponse,
  RegistrarServiceRequestsResponse,
  RegistrarServicesResponse,
  TransferFactoriesResponse,
  TransferFactoryInfo,
  TransferInstructionsResponse,
  TransferPreapprovalsResponse,
  UserServiceInfo,
  UserServicesResponse,
} from "../../../types";

/// Default validity window (hours) for a Transfer proposal / two-step offer.
/// Mirrors the backend default; overridable per-transfer in the form.
const DEFAULT_TRANSFER_EXPIRY_HOURS = 24;

interface ProposalStateArgs {
  partyId: string;
  defaultOperatorParty?: string;
}

/**
 * The fields, lookup lists and prefill effects of the new-proposal form.
 * The section calls it in both views, so its fetches start when the
 * section mounts, as they did before the form had its own component.
 */
const useProposalState = ({ partyId, defaultOperatorParty }: ProposalStateArgs) => {
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
  // The delegations this party already has, newest first, read from the ledger.
  // Normally zero or one — the singleton is not ledger-enforced, so several can
  // appear and both vote forms then offer a choice. Index 0 is the one the
  // automation acts on.
  const [activeDelegations, setActiveDelegations] = useState<
    CouponReassignmentDelegationSummary[]
  >([]);
  const [activeDelegationLoading, setActiveDelegationLoading] = useState(false);
  const [activeDelegationError, setActiveDelegationError] = useState<string | null>(null);
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
  // The newest open round as scan lists it, or why the node could not read it
  const [openMiningRound, setOpenMiningRound] = useState<OpenMiningRoundInfo | null>(null);
  const [openMiningRoundError, setOpenMiningRoundError] = useState("");
  const [openMiningRoundLoading, setOpenMiningRoundLoading] = useState(false);
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

  // Available services from ACS
  const [providerServices, setProviderServices] = useState<
    ProviderServiceInfo[]
  >([]);
  const [userServices, setUserServices] = useState<UserServiceInfo[]>([]);
  // Pending CredentialOffer contracts visible to this party. Powers the
  // CredentialOffer dropdowns on the Accept Free Credential forms.
  const {
    items: credentialOffers,
    loading: credentialOffersLoading,
    load: fetchCredentialOffers,
  } = usePartyList<CredentialOffersResponse>({
    path: `${API_BASE}/credential-offers`,
    partyId,
    field: "credential_offers",
    what: "credential offers",
  });
  // Amulets this party owns. Powers the deposit picker on the Accept Paid
  // Credential form.
  const [ownAmulets, setOwnAmulets] = useState<AmuletOption[]>([]);
  const [ownAmuletsLoading, setOwnAmuletsLoading] = useState(false);
  const [registrarServiceContracts, setRegistrarServiceContracts] = useState<ContractWithBlob[]>([]);
  // Typed `MintRequest`/`BurnRequest` rows so the Accept dropdowns can show
  // holder → amount instrument (…cid) — mirroring the Accept Transfer UX —
  // instead of just the contract id slug.
  const {
    items: mintRequestContracts,
    loading: mintRequestsLoading,
    load: fetchOpenMintRequests,
  } = usePartyList<MintRequestsResponse>({
    path: `${API_BASE}/governance/mint-requests`,
    partyId,
    field: "mint_requests",
    what: "mint requests",
  });
  const {
    items: burnRequestContracts,
    loading: burnRequestsLoading,
    load: fetchOpenBurnRequests,
  } = usePartyList<BurnRequestsResponse>({
    path: `${API_BASE}/governance/burn-requests`,
    partyId,
    field: "burn_requests",
    what: "burn requests",
  });
  // `Credential` contracts visible to this party. Powers the issuer
  // credential picker on the Accept Mint/Burn Request forms.
  const {
    items: availableCredentials,
    loading: credentialsLoading,
    load: fetchCredentials,
  } = usePartyList<CredentialsResponse>({
    path: `${API_BASE}/credentials`,
    partyId,
    field: "credentials",
    what: "credentials",
  });
  // Block list shards (`StoredBlockListPartyShas`) visible to this party.
  // Power the shard picker on the Accept Mint/Burn Request forms.
  const {
    items: availableBlockListShards,
    loading: blockListShardsLoading,
    load: fetchBlockListShards,
  } = usePartyList<BlockListShardsResponse>({
    path: `${API_BASE}/block-list-shards`,
    partyId,
    field: "block_list_shards",
    what: "block list shards",
  });
  // Pending `RegistrarServiceRequest` and `ProviderConfiguration` contracts.
  // Power the pickers on the Onboard Registrar form.
  const {
    items: registrarServiceRequests,
    loading: registrarServiceRequestsLoading,
    load: fetchRegistrarServiceRequests,
  } = usePartyList<RegistrarServiceRequestsResponse>({
    path: `${API_BASE}/registrar-service-requests`,
    partyId,
    field: "registrar_service_requests",
    what: "registrar service requests",
  });
  // Typed `RegistrarService` contracts. Power the Provision Instrument
  // picker, which needs the registrar field to filter out services this
  // decparty co-signed as the provider.
  const {
    items: availableRegistrarServices,
    loading: registrarServicesLoading,
    load: fetchRegistrarServices,
  } = usePartyList<RegistrarServicesResponse>({
    path: `${API_BASE}/services/registrar`,
    partyId,
    field: "services",
    what: "registrar services",
  });
  const {
    items: providerConfigurations,
    loading: providerConfigurationsLoading,
    load: fetchProviderConfigurations,
  } = usePartyList<ProviderConfigurationsResponse>({
    path: `${API_BASE}/provider-configurations`,
    partyId,
    field: "provider_configurations",
    what: "provider configurations",
  });
  // InstrumentConfiguration contracts fetched from /instruments. Each one
  // represents a token the governance party can mint/burn against and exposes
  // its parsed instrument_admin + instrument_id, so we can drive a real
  // dropdown without the frontend having to decode contract blobs.
  const {
    items: availableInstruments,
    loading: instrumentsLoading,
    load: fetchInstruments,
  } = usePartyList<InstrumentsResponse>({
    path: `${API_BASE}/instruments`,
    partyId,
    field: "instruments",
    what: "instruments",
  });
  // Open TransferInstruction contracts addressed to this dec-party. Powers the
  // Accept Transfer proposal dropdown.
  const {
    items: openTransferInstructions,
    loading: transferInstructionsLoading,
    load: fetchOpenTransferInstructions,
  } = usePartyList<TransferInstructionsResponse>({
    path: `${API_BASE}/governance/transfer-instructions`,
    partyId,
    field: "transfer_instructions",
    what: "transfer instructions",
  });
  // Holdings + TransferFactory contracts power the Transfer Proposal form's
  // instrument dropdown. Holdings define which instruments the user can pick
  // (and the available balance); factories prefill the factory contract id +
  // expected admin once an instrument is selected (joined by
  // factory.expected_admin == holding.instrument_admin).
  const [transferHoldings, setTransferHoldings] = useState<Holding[]>([]);
  const [transferFactories, setTransferFactories] = useState<TransferFactoryInfo[]>([]);
  const [transferPrefillLoading, setTransferPrefillLoading] = useState(false);
  // Key into `transferHoldings` for the currently-selected instrument:
  // `${instrument_admin}::${instrument_id}`. Empty string = none selected.
  const [selectedHoldingKey, setSelectedHoldingKey] = useState("");
  // Hides the explicit `Input Holding CIDs` field by default — Daml's
  // TransferFactory choice auto-selects matching holdings up to `amount`, so
  // typical users don't need it. Power users can reveal it to pin specific
  // UTXO holdings.
  const [showTransferAdvanced, setShowTransferAdvanced] = useState(false);
  // Counts of active TransferPreapproval contracts the gov party already has
  // (CC + Token). Used to warn before issuing a Setup*Preapproval proposal
  // that would be a no-op when executed.
  const [preapprovalCounts, setPreapprovalCounts] = useState<TransferPreapprovalsResponse>({
    cc: 0,
    token: 0,
  });
  const [servicesLoading, setServicesLoading] = useState(false);

  const [amuletRulesLoading, setAmuletRulesLoading] = useState(false);

  // Fetch available services from ACS
  const fetchServices = useCallback(async () => {
    setServicesLoading(true);
    try {
      const [providerRes, userRes] = await Promise.all([
        authenticatedFetch(
          `${API_BASE}/services/provider?party_id=${encodeURIComponent(partyId)}`,
        ),
        authenticatedFetch(
          `${API_BASE}/services/user?party_id=${encodeURIComponent(partyId)}`,
        ),
      ]);

      if (providerRes.ok) {
        const response: ProviderServicesResponse = await providerRes.json();
        setProviderServices(response.services);
      }

      if (userRes.ok) {
        const response: UserServicesResponse = await userRes.json();
        setUserServices(response.services);
      }
    } catch (e) {
      console.error("Failed to fetch services:", e);
    } finally {
      setServicesLoading(false);
    }
  }, [partyId]);

  // Fetch services when a proposal type that creates a service-request is
  // selected — used to detect when the corresponding service already exists,
  // so the form can warn that the proposal would be a no-op.
  // SetupUtility additionally needs the ProviderService list to populate its
  // dropdown of available services to wire the utility setup against.
  // The credential proposal forms need the UserService list to prefill their
  // UserService dropdown.
  useEffect(() => {
    if (
      proposalType === "create_user_service_request" ||
      proposalType === "create_provider_service_request" ||
      proposalType === "setup_utility" ||
      proposalType === "offer_free_credential" ||
      proposalType === "accept_free_credential" ||
      proposalType === "offer_paid_credential" ||
      proposalType === "accept_paid_credential" ||
      proposalType === "create_provider_configuration" ||
      proposalType === "onboard_registrar"
    ) {
      fetchServices();
    }
  }, [proposalType, fetchServices]);

  useEffect(() => {
    if (
      proposalType === "accept_free_credential" ||
      proposalType === "accept_paid_credential"
    ) {
      fetchCredentialOffers();
    }
  }, [proposalType, fetchCredentialOffers]);

  // Offers this party can take via the Free direction: it is the holder and
  // the offer carries no billing params (`CredentialOffer_AcceptFree` rejects
  // billed offers).
  const acceptableCredentialOffers = useMemo(
    () => credentialOffers.filter((o) => o.is_free && o.holder === partyId),
    [credentialOffers, partyId],
  );

  // Offers this party can take via the Paid direction: it is the holder and
  // the offer carries billing params.
  const acceptablePaidCredentialOffers = useMemo(
    () => credentialOffers.filter((o) => !o.is_free && o.holder === partyId),
    [credentialOffers, partyId],
  );

  // The listed paid offer the Accept Paid Credential form names. A pasted cid
  // of an unlisted offer leaves it undefined.
  const chosenPaidOffer = useMemo(
    () =>
      acceptablePaidCredentialOffers.find((o) => o.contract_id === proposalCredentialOfferCid),
    [acceptablePaidCredentialOffers, proposalCredentialOfferCid],
  );
  const chosenDepositUsd = chosenPaidOffer && offerDepositUsd(chosenPaidOffer);

  // Requests this provider decparty can accept via Onboard Registrar: only
  // those that name it as the provider. With the provider fixed, each row
  // needs only the registrar and the cid tail.
  const acceptableRegistrarServiceRequests = useMemo(
    () => registrarServiceRequests.filter((r) => r.provider === partyId),
    [registrarServiceRequests, partyId],
  );

  // Services this registrar decparty can provision instruments on. The
  // decparty also co-signs services as the provider side; those fail
  // ProvisionInstrument's registrar assertion, so hide them.
  const ownRegistrarServices = useMemo(
    () => availableRegistrarServices.filter((s) => s.registrar === partyId),
    [availableRegistrarServices, partyId],
  );

  // Instruments this registrar decparty administers. The decparty also
  // co-signs its registrars' instruments as the provider side; those fail
  // OnboardInstrumentIssuers' registrar assertion, so hide them. The
  // instrument admin is the configuration's registrar.
  const ownInstruments = useMemo(
    () => availableInstruments.filter((inst) => inst.instrument_admin === partyId),
    [availableInstruments, partyId],
  );

  const configurationToUpdate = ownInstruments.find(
    (inst) => inst.contract_id === proposalInstrumentConfigurationCid,
  )?.configuration;
  const configurationToUpdateUnreadable =
    proposalInstrumentConfigurationCid !== "" && configurationToUpdate == null;

  // Prefill the credential proposal form's UserService once the list arrives —
  // parties typically have exactly one.
  useEffect(() => {
    if (
      (proposalType === "offer_free_credential" ||
        proposalType === "accept_free_credential" ||
        proposalType === "offer_paid_credential" ||
        proposalType === "accept_paid_credential") &&
      !proposalUserServiceCid &&
      userServices.length > 0
    ) {
      setProposalUserServiceCid(userServices[0].contract_id);
    }
  }, [proposalType, userServices, proposalUserServiceCid]);

  // Prefill the CredentialOffer cid when there's exactly one candidate. With
  // several pending offers the operator has to pick deliberately.
  useEffect(() => {
    if (acceptableCredentialOffers.length !== 1) {
      return;
    }
    const offerCid = acceptableCredentialOffers[0].contract_id;
    if (proposalType === "accept_free_credential" && !proposalCredentialOfferCid) {
      setProposalCredentialOfferCid(offerCid);
    }
  }, [
    proposalType,
    acceptableCredentialOffers,
    proposalCredentialOfferCid,
  ]);

  // The same prefill for the Accept Paid Credential form.
  useEffect(() => {
    if (
      proposalType === "accept_paid_credential" &&
      acceptablePaidCredentialOffers.length === 1 &&
      !proposalCredentialOfferCid
    ) {
      setProposalCredentialOfferCid(acceptablePaidCredentialOffers[0].contract_id);
    }
  }, [proposalType, acceptablePaidCredentialOffers, proposalCredentialOfferCid]);

  // Fetch instruments when a proposal type needs them: Mint/Burn (for
  // instrument_id + instrument_configuration_cid) and
  // set_provider_app_reward_beneficiaries (for instrument_configuration_cid).
  useEffect(() => {
    if (
      proposalType === "accept_mint_request" ||
      proposalType === "accept_burn_request" ||
      proposalType === "set_provider_app_reward_beneficiaries" ||
      proposalType === "onboard_instrument_issuers" ||
      proposalType === "update_instrument_configuration"
    ) {
      fetchInstruments();
    }
  }, [proposalType, fetchInstruments]);

  useEffect(() => {
    if (proposalType === "accept_transfer") {
      fetchOpenTransferInstructions();
    }
  }, [proposalType, fetchOpenTransferInstructions]);

  // Fetch holdings + transfer factories for the Transfer Proposal dropdown.
  // Both endpoints are cheap (one ACS query each) and we need them together
  // to render the dropdown + prefill, so fetch them in parallel.
  const fetchTransferPrefillData = useCallback(async () => {
    setTransferPrefillLoading(true);
    try {
      const [hRes, fRes] = await Promise.all([
        authenticatedFetch(
          `${API_BASE}/holdings?party_id=${encodeURIComponent(partyId)}`,
        ),
        authenticatedFetch(
          `${API_BASE}/transfer-factories?party_id=${encodeURIComponent(partyId)}`,
        ),
      ]);
      if (hRes.ok) {
        const data: HoldingsResponse = await hRes.json();
        setTransferHoldings(data.holdings);
      }
      if (fRes.ok) {
        const data: TransferFactoriesResponse = await fRes.json();
        setTransferFactories(data.transfer_factories);
      }
    } catch (e) {
      console.error("Failed to fetch transfer prefill data:", e);
    } finally {
      setTransferPrefillLoading(false);
    }
  }, [partyId]);

  useEffect(() => {
    if (proposalType === "transfer") {
      fetchTransferPrefillData();
    }
  }, [proposalType, fetchTransferPrefillData]);

  // Whenever the user picks an instrument from the dropdown, push its
  // identifiers and the matching factory into the (still-required) submission
  // fields. We keep those state vars so the existing submit path is
  // untouched — the form is just driven by `selectedHoldingKey` now.
  useEffect(() => {
    if (!selectedHoldingKey) return;
    const holding = transferHoldings.find(
      (h) => `${h.instrument_admin}::${h.instrument_id}` === selectedHoldingKey,
    );
    if (!holding) return;
    setProposalInstrumentIdAdmin(holding.instrument_admin);
    setProposalInstrumentIdId(holding.instrument_id);
    const factory = transferFactories.find(
      (f) => f.expected_admin === holding.instrument_admin,
    );
    if (factory) {
      setProposalTransferFactoryCid(factory.contract_id);
      setProposalExpectedAdmin(factory.expected_admin);
    } else {
      setProposalTransferFactoryCid("");
      setProposalExpectedAdmin(holding.instrument_admin);
    }
  }, [selectedHoldingKey, transferHoldings, transferFactories]);

  // The CreateUserServiceRequest / CreateProviderServiceRequest proposals
  // always use the dec party itself as the user / provider — the field
  // exists because the Daml choice still asks for it, but every operator
  // ends up typing the same value. Seed it once when the form opens.
  useEffect(() => {
    if (proposalType === "create_user_service_request") {
      setProposalUser(partyId);
    } else if (proposalType === "create_provider_service_request") {
      setProposalProvider(partyId);
    }
  }, [proposalType, partyId]);

  // Fetch contracts by template (returns CID + blob)
  const fetchContractsByTemplate = useCallback(
    async (
      template: {
        package_ref: string;
        module: string;
        entity: string;
        interface?: boolean;
      },
      options?: { activeOnly?: boolean; includePayload?: boolean },
    ) => {
      const params = new URLSearchParams({
        party_id: partyId,
        package_id: template.package_ref,
        module_name: template.module,
        entity_name: template.entity,
      });
      if (template.interface) params.set("interface", "true");
      if (options?.activeOnly) params.set("active_only", "true");
      if (options?.includePayload) params.set("include_payload", "true");
      const res = await authenticatedFetch(`${API_BASE}/contracts/query?${params}`);
      if (res.ok) {
        const data: ContractQueryResponse = await res.json();
        return data.contracts;
      }
      return [];
    },
    [partyId],
  );

  // Accept Paid Credential locks Amulets this party owns as the deposit.
  const fetchOwnAmulets = useCallback(async () => {
    setOwnAmuletsLoading(true);
    try {
      const contracts = await fetchContractsByTemplate(TEMPLATE_AMULET, { includePayload: true });
      setOwnAmulets(ownedAmulets(contracts, partyId));
    } catch (e) {
      console.error("Failed to fetch Amulets:", e);
    } finally {
      setOwnAmuletsLoading(false);
    }
  }, [fetchContractsByTemplate, partyId]);

  useEffect(() => {
    if (proposalType === "accept_paid_credential") {
      fetchOwnAmulets();
    }
  }, [proposalType, fetchOwnAmulets]);

  // Fetch the contract lists the selected proposal type needs.
  useEffect(() => {
    if (proposalType === "set_enable_result_contracts") {
      fetchContractsByTemplate(TEMPLATE_REGISTRAR_SERVICE).then(setRegistrarServiceContracts);
    }
    // Provision Instrument needs the typed rows to filter on the registrar
    // field, so it uses /services/registrar instead of the blob query.
    if (
      proposalType === "provision_instrument" ||
      proposalType === "update_instrument_configuration" ||
      proposalType === "create_transfer_rule"
    ) {
      fetchRegistrarServices();
    }
    if (proposalType === "accept_mint_request") {
      fetchOpenMintRequests();
    }
    if (proposalType === "accept_burn_request") {
      fetchOpenBurnRequests();
    }
    // The accept forms need the party's credentials for the issuer
    // credential picker; the Offboard form for its per-issuer credential
    // pickers.
    if (
      proposalType === "accept_mint_request" ||
      proposalType === "accept_burn_request" ||
      proposalType === "offboard_instrument_issuers"
    ) {
      fetchCredentials();
    }
    if (proposalType === "accept_mint_request" || proposalType === "accept_burn_request") {
      fetchBlockListShards();
    }
    // The Onboard Registrar pickers list the pending requests and the
    // provider's configurations.
    if (proposalType === "onboard_registrar") {
      fetchRegistrarServiceRequests();
      fetchProviderConfigurations();
    }
  }, [
    proposalType,
    fetchContractsByTemplate,
    fetchOpenMintRequests,
    fetchOpenBurnRequests,
    fetchCredentials,
    fetchBlockListShards,
    fetchRegistrarServiceRequests,
    fetchProviderConfigurations,
    fetchRegistrarServices,
  ]);

  // Fetch network info (DSO party + amulet rules) from DSO API
  const fetchNetworkInfo = useCallback(async () => {
    setAmuletRulesLoading(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/network-info`);
      if (res.ok) {
        const data: NetworkInfo = await res.json();
        setAmuletRulesCid(data.amulet_rules_cid);
        setDsoPartyId(data.dso_party_id);
        setProposalExpectedDso(data.dso_party_id);
        // The coupon-reassignment form keeps its DSO in its own state, so it
        // needs prefilling here too. A wrong DSO there assigns nothing at all,
        // silently, which is indistinguishable from having nothing to do.
        setProposalDelegationDso(data.dso_party_id);
      }
    } catch (e) {
      console.error("Failed to fetch network info:", e);
    } finally {
      setAmuletRulesLoading(false);
    }
  }, []);

  // Read the newest open mining round from scan for the Accept Paid
  // Credential form. Without DECPM_SCAN_URL the node answers 422, and the
  // form shows its message.
  const fetchOpenMiningRound = useCallback(async () => {
    setOpenMiningRoundLoading(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/open-mining-round`);
      const data = await res.json();
      if (res.ok) {
        const round = data as OpenMiningRoundInfo;
        setOpenMiningRound(round);
        setOpenMiningRoundError("");
        setProposalOpenMiningRoundCid(round.contract_id);
      } else {
        setOpenMiningRound(null);
        setOpenMiningRoundError(data.error || "Failed to read the open mining round");
      }
    } catch (e) {
      setOpenMiningRound(null);
      setOpenMiningRoundError(e instanceof Error ? e.message : String(e));
    } finally {
      setOpenMiningRoundLoading(false);
    }
  }, []);

  useEffect(() => {
    if (proposalType === "accept_paid_credential") {
      fetchOpenMiningRound();
    }
  }, [proposalType, fetchOpenMiningRound]);

  // Setup CC Preapproval and Setup Minting Delegation need the DSO party id
  // from the network-info endpoint to prefill their DSO field. Mirror the
  // action-form trigger above for the proposal form.
  useEffect(() => {
    if (
      (proposalType === "setup_cc_preapproval" ||
        proposalType === "setup_minting_delegation" ||
        proposalType === "setup_coupon_reassignment_delegation") &&
      !dsoPartyId
    ) {
      fetchNetworkInfo();
    }
  }, [proposalType, dsoPartyId, fetchNetworkInfo]);

  // Request DevNet Featured App Right and Accept Paid Credential pin the
  // DSO's current AmuletRules, so prefill its cid from network-info too. The
  // field keeps a refresh button for when the DSO replaces the contract while
  // the form is open.
  useEffect(() => {
    if (
      (proposalType === "request_dev_net_featured_app_right" ||
        proposalType === "accept_paid_credential") &&
      !amuletRulesCid
    ) {
      fetchNetworkInfo();
    }
  }, [proposalType, amuletRulesCid, fetchNetworkInfo]);

  // Read the delegations this party already has, so neither vote form asks for a
  // pasted contract id. Setup needs it for "Replaces Delegation" — blank while
  // one is live is rejected with 409. Revoke needs it to name what to archive.
  const fetchActiveDelegations = useCallback(async () => {
    setActiveDelegationLoading(true);
    setActiveDelegationError(null);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/coupon-reassignment-delegation?party_id=${encodeURIComponent(partyId)}`,
      );
      if (!res.ok) {
        setActiveDelegationError("Could not read this party's current delegations.");
        return;
      }
      const data: ActiveCouponReassignmentDelegation = await res.json();
      const active = data.delegations ?? [];
      setActiveDelegations(active);
      // Preselect the one the automation acts on. With several active a human
      // still has to choose, but the automation's pick is the sane default.
      if (active.length > 0) {
        setProposalPriorDelegation((cur) => cur || active[0].cid);
        setProposalRevokeDelegationCid((cur) => cur || active[0].cid);
      }
    } catch (e) {
      console.error("Failed to fetch active coupon reassignment delegations:", e);
      setActiveDelegationError("Could not read this party's current delegations.");
    } finally {
      setActiveDelegationLoading(false);
    }
  }, [partyId]);

  useEffect(() => {
    if (
      proposalType === "setup_coupon_reassignment_delegation" ||
      proposalType === "revoke_coupon_reassignment_delegation"
    ) {
      fetchActiveDelegations();
    }
  }, [proposalType, fetchActiveDelegations]);

  // Setup*Preapproval forms warn when one already exists — fetch the counts
  // (cheap, two ACS template-filter queries).
  const fetchPreapprovalCounts = useCallback(async () => {
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/transfer-preapprovals?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        setPreapprovalCounts(await res.json());
      }
    } catch (e) {
      console.error("Failed to fetch transfer preapproval counts:", e);
    }
  }, [partyId]);

  useEffect(() => {
    if (
      proposalType === "setup_cc_preapproval" ||
      proposalType === "setup_token_preapproval"
    ) {
      fetchPreapprovalCounts();
    }
  }, [proposalType, fetchPreapprovalCounts]);

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

  // A beneficiary list the template would reject: submitting it would only
  // fail after the committee voted, so the form refuses it up front.
  const beneficiaryProblem =
    proposalType === "set_provider_app_reward_beneficiaries"
      ? beneficiariesProblem(proposalBeneficiaries, proposalClearBeneficiaries)
      : null;

  // The update can send only an unset or false block list flag (#566), so on
  // a configuration that checks a block list it would turn the check off.
  const blockListProblem =
    proposalType === "update_instrument_configuration" &&
    configurationToUpdate?.check_block_list === true
      ? "This configuration checks a block list. DecMan cannot update it until #566, because not every mint, burn and transfer sends block list shards yet."
      : null;

  // With the block list check on, the registry refuses an accept that
  // carries no shards. It fetches them against the instrument's registrar.
  const acceptInstrument = availableInstruments.find(
    (inst) => inst.contract_id === proposalInstrumentConfigurationCid,
  );
  const acceptNeedsBlockListShards =
    (proposalType === "accept_mint_request" || proposalType === "accept_burn_request") &&
    acceptInstrument?.configuration?.check_block_list === true;
  const registrarBlockListShards = (registrar: string | undefined) =>
    availableBlockListShards.filter((shard) => shard.registrar === registrar);
  const acceptBlockListShardCids = acceptNeedsBlockListShards ? proposalBlockListShardCids : [];
  // The shard list can arrive after the request is picked, when the prefill
  // found none. Fill in the registrar's shards once they arrive, unless the
  // operator has already chosen shards by hand.
  const acceptRegistrar = acceptInstrument?.instrument_admin;
  useEffect(() => {
    if (!acceptNeedsBlockListShards || blockListShardsEdited) return;
    if (proposalBlockListShardCids.length > 0) return;
    const shards = availableBlockListShards.filter((shard) => shard.registrar === acceptRegistrar);
    if (shards.length > 0) {
      setProposalBlockListShardCids(shards.map((shard) => shard.contract_id));
    }
  }, [
    acceptNeedsBlockListShards,
    acceptRegistrar,
    availableBlockListShards,
    blockListShardsEdited,
    proposalBlockListShardCids.length,
  ]);

  return {
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
    activeDelegations,
    setActiveDelegations,
    activeDelegationLoading,
    setActiveDelegationLoading,
    activeDelegationError,
    setActiveDelegationError,
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
    openMiningRound,
    setOpenMiningRound,
    openMiningRoundError,
    setOpenMiningRoundError,
    openMiningRoundLoading,
    setOpenMiningRoundLoading,
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
    providerServices,
    setProviderServices,
    userServices,
    setUserServices,
    credentialOffers,
    credentialOffersLoading,
    fetchCredentialOffers,
    ownAmulets,
    setOwnAmulets,
    ownAmuletsLoading,
    setOwnAmuletsLoading,
    registrarServiceContracts,
    setRegistrarServiceContracts,
    mintRequestContracts,
    mintRequestsLoading,
    fetchOpenMintRequests,
    burnRequestContracts,
    burnRequestsLoading,
    fetchOpenBurnRequests,
    availableCredentials,
    credentialsLoading,
    fetchCredentials,
    availableBlockListShards,
    blockListShardsLoading,
    fetchBlockListShards,
    registrarServiceRequests,
    registrarServiceRequestsLoading,
    fetchRegistrarServiceRequests,
    availableRegistrarServices,
    registrarServicesLoading,
    fetchRegistrarServices,
    providerConfigurations,
    providerConfigurationsLoading,
    fetchProviderConfigurations,
    availableInstruments,
    instrumentsLoading,
    fetchInstruments,
    openTransferInstructions,
    transferInstructionsLoading,
    fetchOpenTransferInstructions,
    transferHoldings,
    setTransferHoldings,
    transferFactories,
    setTransferFactories,
    transferPrefillLoading,
    setTransferPrefillLoading,
    selectedHoldingKey,
    setSelectedHoldingKey,
    showTransferAdvanced,
    setShowTransferAdvanced,
    preapprovalCounts,
    setPreapprovalCounts,
    servicesLoading,
    setServicesLoading,
    amuletRulesLoading,
    setAmuletRulesLoading,
    fetchServices,
    acceptableCredentialOffers,
    acceptablePaidCredentialOffers,
    chosenPaidOffer,
    chosenDepositUsd,
    acceptableRegistrarServiceRequests,
    ownRegistrarServices,
    ownInstruments,
    configurationToUpdate,
    configurationToUpdateUnreadable,
    fetchTransferPrefillData,
    fetchContractsByTemplate,
    fetchOwnAmulets,
    fetchNetworkInfo,
    fetchOpenMiningRound,
    fetchActiveDelegations,
    fetchPreapprovalCounts,
    resetProposalForm,
    beneficiaryProblem,
    blockListProblem,
    acceptInstrument,
    acceptNeedsBlockListShards,
    registrarBlockListShards,
    acceptBlockListShardCids,
    acceptRegistrar,
  };
};

type ProposalState = ReturnType<typeof useProposalState>;

export { DEFAULT_TRANSFER_EXPIRY_HOURS, useProposalState };
export type { ProposalState };
